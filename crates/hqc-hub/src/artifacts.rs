//! Artifacts (SPEC §3 Artifact, §6, §12): the on-disk store keyed by sha256, uploads, signed
//! links and the three ways content leaves the hub.
//!
//! - `POST /api/artifacts` (multipart `file`, `kind?`): streamed to a temporary file while
//!   hashing, at most [`ArtifactConfig::max_bytes`]; the same user uploading the same content
//!   under the same name gets the existing artifact back (200 instead of 201).
//! - `GET /api/artifacts/{id}` (metadata) and `GET /api/artifacts/{id}/url` (a link signed
//!   with HMAC-SHA256 over `id.exp`, valid for [`ArtifactConfig::link_ttl`]): owner only.
//! - `GET /artifacts/{id}?exp=&sig=[&dl=1]`: the content by a signed link, on the artifact
//!   domain (`HQC_ARTIFACT_PUBLIC_BASE`) or on the hub itself. Bad signature `403`, expired
//!   `410`.
//! - `GET /worker/artifacts/{id}`: the content for a worker token, if a session of that
//!   worker was given the artifact in its input (SPEC §5.3).
//! - `PUT /worker/artifacts`: a worker uploads the file of an agent's `hqc_attach` for one of
//!   its sessions (SPEC §5.4, §7); the owner is the session's owner.
//!
//! Cleanup (T21, [`cleanup`]): once a day artifacts no event refers to and not used for
//! `HQC_ARTIFACT_RETENTION_DAYS` are deleted; a file goes when no row has its sha256 any more.
//!
//! Every content response carries [`content_headers`]: a `sandbox` CSP, `nosniff` and a
//! `Content-Disposition` by kind. HTML is inline only on a separate artifact domain;
//! on the hub's own domain it is always an attachment.

use std::path::{Path, PathBuf};

use axum::{
    body::Body,
    extract::{Multipart, Path as UrlPath, Query, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use hqc_proto::{
    api::{Artifact, ArtifactUrl},
    event::ArtifactKind,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::{bearer, hash_secret, CurrentUser},
    config::ArtifactConfig,
    db,
    error::{ApiError, ApiResult},
    security::ClientIp,
    worker_ws::{is_worker_token, AUTH_FAILURES},
};

/// Artifacts one input may carry.
pub const MAX_INPUT_ARTIFACTS: usize = 32;
/// Longest stored name, in bytes.
const MAX_NAME: usize = 255;

// ---------------------------------------------------------------- database

#[derive(Debug, Clone, FromRow)]
pub struct ArtifactRow {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub session_id: Option<Uuid>,
    pub kind: String,
    pub mime: String,
    pub size: i64,
    pub sha256: String,
    pub name: String,
    pub storage_key: String,
    pub created_at: DateTime<Utc>,
}

const COLS: &str =
    "id, owner_id, session_id, kind, mime, size, sha256, name, storage_key, created_at";

impl ArtifactRow {
    pub fn kind(&self) -> ArtifactKind {
        serde_json::from_value(serde_json::Value::String(self.kind.clone()))
            .unwrap_or(ArtifactKind::File)
    }

    pub fn to_api(&self) -> Artifact {
        Artifact {
            id: self.id,
            kind: self.kind(),
            name: self.name.clone(),
            mime: self.mime.clone(),
            size: self.size.max(0) as u64,
            sha256: self.sha256.clone(),
            created_at: self.created_at,
        }
    }
}

pub async fn artifact(db: &PgPool, id: Uuid) -> sqlx::Result<Option<ArtifactRow>> {
    sqlx::query_as(&format!("SELECT {COLS} FROM artifacts WHERE id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// Insert, or the row the owner already has with this content and name (`false`).
async fn insert_artifact(db: &PgPool, a: &ArtifactRow) -> sqlx::Result<(ArtifactRow, bool)> {
    let new: Option<ArtifactRow> = sqlx::query_as(&format!(
        "INSERT INTO artifacts (id, owner_id, kind, mime, size, sha256, name, storage_key)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (owner_id, sha256, name) DO NOTHING
         RETURNING {COLS}"
    ))
    .bind(a.id)
    .bind(a.owner_id)
    .bind(&a.kind)
    .bind(&a.mime)
    .bind(a.size)
    .bind(&a.sha256)
    .bind(&a.name)
    .bind(&a.storage_key)
    .fetch_optional(db)
    .await?;
    if let Some(r) = new {
        return Ok((r, true));
    }
    // Used again: the cleanup counts its age from now.
    let old = sqlx::query_as(&format!(
        "UPDATE artifacts SET updated_at = now()
         WHERE owner_id = $1 AND sha256 = $2 AND name = $3 RETURNING {COLS}"
    ))
    .bind(a.owner_id)
    .bind(&a.sha256)
    .bind(&a.name)
    .fetch_one(db)
    .await?;
    Ok((old, false))
}

/// Checks that every id is an artifact of `owner` and links them to the session whose input
/// carries them, so that the session's worker may download them.
pub async fn attach_to_session(
    db: &PgPool,
    owner: Uuid,
    session_id: Uuid,
    ids: &[Uuid],
) -> ApiResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    if ids.len() > MAX_INPUT_ARTIFACTS {
        return Err(ApiError::BadRequest(format!(
            "at most {MAX_INPUT_ARTIFACTS} artifacts per input"
        )));
    }
    let found: i64 =
        sqlx::query_scalar("SELECT count(*) FROM artifacts WHERE id = ANY($1) AND owner_id = $2")
            .bind(ids)
            .bind(owner)
            .fetch_one(db)
            .await?;
    let distinct = ids.iter().collect::<std::collections::HashSet<_>>().len();
    if found as usize != distinct {
        return Err(ApiError::Invalid(
            "unknown_artifact",
            "artifact_ids: no such artifact".into(),
        ));
    }
    sqlx::query(
        "INSERT INTO artifact_sessions (artifact_id, session_id)
         SELECT unnest($1::uuid[]), $2 ON CONFLICT DO NOTHING",
    )
    .bind(ids)
    .bind(session_id)
    .execute(db)
    .await?;
    sqlx::query(
        "UPDATE artifacts SET session_id = coalesce(session_id, $2), updated_at = now()
         WHERE id = ANY($1)",
    )
    .bind(ids)
    .bind(session_id)
    .execute(db)
    .await?;
    Ok(())
}

async fn worker_may_read(db: &PgPool, worker_id: Uuid, artifact_id: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM artifact_sessions a JOIN sessions s ON s.id = a.session_id
                        WHERE a.artifact_id = $1 AND s.worker_id = $2)",
    )
    .bind(artifact_id)
    .bind(worker_id)
    .fetch_one(db)
    .await
}

// ---------------------------------------------------------------- kind, name, store

/// What is wrong with the store for a hub about to serve (T53, SPEC §12), logged as a warning
/// at start: a store the hub cannot write to fails every upload, and in a container (`root` is
/// `Some("/")` then) a store on the same device as the root is the container's own layer, so
/// its files go with the next deploy while the database keeps their rows.
pub fn storage_warning(dir: &Path, root: Option<&Path>) -> Option<String> {
    let probe = dir.join(".hqc-write-check");
    if let Err(e) = std::fs::write(&probe, b"").and_then(|()| std::fs::remove_file(&probe)) {
        return Some(format!(
            "HQC_ARTIFACT_DIR={} is not writable ({e}): uploads will fail",
            dir.display()
        ));
    }
    #[cfg(unix)]
    if let Some(root) = root {
        use std::os::unix::fs::MetadataExt;
        let dev = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
        if dev(dir).is_some() && dev(dir) == dev(root) {
            return Some(format!(
                "HQC_ARTIFACT_DIR={} is in the container's own layer, not a volume: attachments \
                 are lost on the next deploy (RUNBOOK §1.8, volume hqc-artifacts)",
                dir.display()
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = root;
    None
}

/// `kind` by mime and extension (SPEC §12): `image/*` image; `.diff`/`.patch` diff;
/// `text/html` html; other `text/*` text; the rest file.
pub fn kind_of(mime: &str, name: &str) -> ArtifactKind {
    let ext = extension(name);
    if mime.starts_with("image/") {
        ArtifactKind::Image
    } else if matches!(ext.as_str(), "diff" | "patch")
        || matches!(mime, "text/x-diff" | "text/x-patch")
    {
        ArtifactKind::Diff
    } else if mime == "text/html" {
        ArtifactKind::Html
    } else if mime.starts_with("text/") {
        ArtifactKind::Text
    } else {
        ArtifactKind::File
    }
}

fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// The declared type without parameters; empty or `application/octet-stream` is guessed from
/// the extension, since browsers send nothing better for most source files.
pub fn mime_of(declared: Option<&str>, name: &str) -> String {
    let declared = declared
        .and_then(|m| m.split(';').next())
        .map(|m| m.trim().to_ascii_lowercase())
        .filter(|m| !m.is_empty() && m != "application/octet-stream" && m.contains('/'));
    if let Some(m) = declared {
        return m;
    }
    let m = match extension(name).as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "html" | "htm" => "text/html",
        "diff" | "patch" => "text/x-diff",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "txt" | "log" | "rs" | "ts" | "tsx" | "js" | "py" | "go" | "toml" | "yaml" | "yml"
        | "sh" | "sql" | "c" | "h" | "cpp" | "java" | "kt" | "swift" => "text/plain",
        "json" => "application/json",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    };
    m.into()
}

/// The last path component, without control characters, at most [`MAX_NAME`] bytes; `file`
/// when nothing is left. The worker sanitizes again for its file system (SPEC §5.3).
pub fn clean_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = base.chars().filter(|c| !c.is_control()).collect();
    s = s.trim().to_string();
    if s.len() > MAX_NAME {
        let mut cut = MAX_NAME;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    if s.is_empty() || s == "." || s == ".." {
        "file".into()
    } else {
        s
    }
}

fn blob_path(cfg: &ArtifactConfig, sha: &str) -> PathBuf {
    cfg.dir.join(&sha[..2]).join(sha)
}

/// A file being written; removed on drop unless kept.
struct TempFile(Option<PathBuf>);

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

struct Stored {
    sha256: String,
    size: u64,
    /// The content, until [`place`] moves it into the store.
    tmp: TempFile,
}

/// Moves the uploaded content to `<dir>/<sha[0..2]>/<sha>` unless it is there. Called after
/// the row is stored: a cleanup that removed the file meanwhile ([`remove_blob`] checks rows
/// after taking the file away) is undone here.
async fn place(cfg: &ArtifactConfig, mut s: Stored) -> ApiResult<()> {
    let dest = blob_path(cfg, &s.sha256);
    if tokio::fs::metadata(&dest).await.is_ok() {
        return Ok(());
    }
    tokio::fs::create_dir_all(dest.parent().unwrap())
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    let tmp = s.tmp.0.take().expect("content of an upload");
    tokio::fs::rename(&tmp, &dest)
        .await
        .map_err(|e| ApiError::Internal(e.into()))
}

/// An upload being written to a temporary file while it is hashed.
struct Writer {
    f: tokio::fs::File,
    hash: Sha256,
    size: u64,
    max: u64,
    tmp: TempFile,
}

impl Writer {
    async fn new(cfg: &ArtifactConfig) -> ApiResult<Self> {
        let tmp_dir = cfg.dir.join("tmp");
        tokio::fs::create_dir_all(&tmp_dir)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
        let tmp = TempFile(Some(tmp_dir.join(Uuid::new_v4().to_string())));
        let f = tokio::fs::File::create(tmp.0.as_ref().unwrap())
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
        Ok(Self {
            f,
            hash: Sha256::new(),
            size: 0,
            max: cfg.max_bytes,
            tmp,
        })
    }

    /// Over the limit: `413 too_large`.
    async fn write(&mut self, chunk: &[u8]) -> ApiResult<()> {
        self.size += chunk.len() as u64;
        if self.size > self.max {
            return Err(too_large(self.max));
        }
        self.hash.update(chunk);
        self.f
            .write_all(chunk)
            .await
            .map_err(|e| ApiError::Internal(e.into()))
    }

    async fn finish(self) -> ApiResult<Stored> {
        self.f
            .sync_all()
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
        let sha256: String = self
            .hash
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(Stored {
            sha256,
            size: self.size,
            tmp: self.tmp,
        })
    }
}

fn too_large(max: u64) -> ApiError {
    ApiError::TooLarge(format!("an artifact is at most {max} bytes"))
}

/// Streams one multipart field into the store. Over the limit: `413 too_large`.
async fn store_field(
    cfg: &ArtifactConfig,
    field: &mut axum::extract::multipart::Field<'_>,
) -> ApiResult<Stored> {
    let mut w = Writer::new(cfg).await?;
    loop {
        match field.chunk().await {
            Ok(Some(c)) => w.write(&c).await?,
            Ok(None) => break,
            // The body limit cuts the stream: the upload was too large.
            Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => {
                return Err(too_large(cfg.max_bytes))
            }
            Err(e) => {
                return Err(ApiError::BadRequest(format!(
                    "multipart: {}",
                    e.body_text()
                )))
            }
        }
    }
    w.finish().await
}

/// Streams a request body into the store. Over the limit: `413 too_large`.
async fn store_body(cfg: &ArtifactConfig, body: Body) -> ApiResult<Stored> {
    use futures_util::StreamExt;
    let mut w = Writer::new(cfg).await?;
    let mut stream = body.into_data_stream();
    while let Some(c) = stream.next().await {
        let c = c.map_err(|e| ApiError::BadRequest(format!("body: {e}")))?;
        w.write(&c).await?;
    }
    w.finish().await
}

// ---------------------------------------------------------------- cleanup

/// Daily [`cleanup`], the first right after start.
pub async fn cleanup_loop(app: AppState, keep: std::time::Duration) {
    loop {
        match cleanup(&app, keep).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(removed = n, "artifact cleanup"),
            Err(e) => tracing::error!(error = %e, "artifact cleanup"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(86_400)).await;
    }
}

/// Deletes artifacts that no event refers to (`user.message.artifact_ids`, `attachment` and
/// `tool.result` `artifact_id`) and that were not uploaded or used for `keep`; their files go
/// when no row has the sha256 any more. Returns how many rows went.
pub async fn cleanup(app: &AppState, keep: std::time::Duration) -> anyhow::Result<usize> {
    let shas: Vec<String> = sqlx::query_scalar(
        "WITH refs AS (
            SELECT jsonb_array_elements_text(payload->'artifact_ids') AS id FROM events
             WHERE jsonb_typeof(payload->'artifact_ids') = 'array'
            UNION
            SELECT payload->>'artifact_id' FROM events
             WHERE jsonb_typeof(payload->'artifact_id') = 'string')
         DELETE FROM artifacts a
         WHERE a.updated_at < now() - $1 * interval '1 second'
           AND NOT EXISTS (SELECT 1 FROM refs r WHERE r.id = a.id::text)
         RETURNING a.storage_key",
    )
    .bind(keep.as_secs() as f64)
    .fetch_all(&app.db)
    .await?;
    let n = shas.len();
    let mut shas = shas;
    shas.sort();
    shas.dedup();
    for sha in shas {
        if let Err(e) = remove_blob(&app.config.artifacts, &app.db, &sha).await {
            tracing::warn!(%sha, "artifact cleanup: {e:#}");
        }
    }
    Ok(n)
}

async fn blob_used(db: &PgPool, sha: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM artifacts WHERE storage_key = $1)")
        .bind(sha)
        .fetch_one(db)
        .await
}

/// Removes the content of `sha` when no artifact row has it. The file is first moved aside,
/// then the rows are counted: an upload of the same content that stored its row meanwhile
/// gets it back (and [`place`] puts its own copy in if this one is already gone).
async fn remove_blob(cfg: &ArtifactConfig, db: &PgPool, sha: &str) -> anyhow::Result<()> {
    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        anyhow::bail!("not a sha256: {sha:?}");
    }
    if blob_used(db, sha).await? {
        return Ok(());
    }
    let dest = blob_path(cfg, sha);
    let tmp_dir = cfg.dir.join("tmp");
    tokio::fs::create_dir_all(&tmp_dir).await?;
    let aside = tmp_dir.join(format!("{sha}.removed-{}", Uuid::new_v4()));
    match tokio::fs::rename(&dest, &aside).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    }
    if blob_used(db, sha).await? && tokio::fs::metadata(&dest).await.is_err() {
        tokio::fs::rename(&aside, &dest).await?;
    } else {
        tokio::fs::remove_file(&aside).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------- signing

type HmacSha256 = Hmac<Sha256>;

fn mac(cfg: &ArtifactConfig, id: Uuid, exp: i64) -> HmacSha256 {
    let mut m = HmacSha256::new_from_slice(&cfg.key).expect("any key length");
    m.update(format!("{id}.{exp}").as_bytes());
    m
}

fn b64(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// `[<public base>]/artifacts/<id>?exp=<unix s>&sig=<base64url HMAC>`.
pub fn signed_url(cfg: &ArtifactConfig, id: Uuid, exp: i64) -> String {
    let sig = b64(&mac(cfg, id, exp).finalize().into_bytes());
    format!(
        "{}/artifacts/{id}?exp={exp}&sig={sig}",
        cfg.public_base.as_deref().unwrap_or("")
    )
}

fn signature_ok(cfg: &ArtifactConfig, id: Uuid, exp: i64, sig: &str) -> bool {
    use base64::Engine;
    let Ok(raw) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(sig) else {
        return false;
    };
    mac(cfg, id, exp).verify_slice(&raw).is_ok()
}

// ---------------------------------------------------------------- responses

/// Where the content is served, which decides how HTML is treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Served {
    /// The separate artifact domain: HTML inline in a sandbox.
    ArtifactDomain,
    /// The hub's own domain (no separate artifact domain, or the hub host): HTML as a download.
    HubDomain,
    /// To a worker.
    Worker,
}

/// Content-Security-Policy of artifact content: a sandbox without same-origin and without
/// network access; HTML on the artifact domain may run its inline scripts (SPEC §12).
pub const CSP: &str = "sandbox; default-src 'none'; img-src data:; style-src 'unsafe-inline'; \
                       frame-ancestors 'none'";
pub const CSP_HTML: &str = "sandbox allow-scripts; default-src 'none'; \
                            script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
                            img-src data:; font-src data:";

/// Headers of a content response: type and disposition by kind, the sandbox CSP, `nosniff`,
/// private caching.
pub fn content_headers(
    a: &ArtifactRow,
    served: Served,
    download: bool,
) -> Vec<(HeaderName, String)> {
    let kind = a.kind();
    let html_inline = kind == ArtifactKind::Html && served == Served::ArtifactDomain && !download;
    let ctype = match kind {
        ArtifactKind::Image if a.mime.starts_with("image/") => a.mime.clone(),
        ArtifactKind::Text | ArtifactKind::Diff => "text/plain; charset=utf-8".into(),
        ArtifactKind::Html => "text/html; charset=utf-8".into(),
        _ => "application/octet-stream".into(),
    };
    let inline = !download
        && match kind {
            ArtifactKind::Image | ArtifactKind::Text | ArtifactKind::Diff => true,
            ArtifactKind::Html => html_inline,
            _ => false,
        };
    let disposition = format!(
        "{}; filename=\"{}\"; filename*=UTF-8''{}",
        if inline { "inline" } else { "attachment" },
        ascii_name(&a.name),
        pct(&a.name)
    );
    let mut h = vec![
        (header::CONTENT_TYPE, ctype),
        (header::CONTENT_DISPOSITION, disposition),
        (
            header::CONTENT_SECURITY_POLICY,
            if html_inline { CSP_HTML } else { CSP }.into(),
        ),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
        (header::REFERRER_POLICY, "no-referrer".into()),
    ];
    match served {
        Served::Worker => h.push((header::CACHE_CONTROL, "private, no-store".into())),
        _ => {
            h.push((header::CACHE_CONTROL, "private, max-age=600".into()));
            // The URL is the capability; the client reads text and diffs with fetch().
            h.push((header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".into()));
            h.push((
                HeaderName::from_static("cross-origin-resource-policy"),
                "cross-origin".into(),
            ));
        }
    }
    if !html_inline {
        h.push((header::X_FRAME_OPTIONS, "DENY".into()));
    }
    h
}

fn ascii_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Percent-encoding of everything but RFC 3986 unreserved characters.
pub fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

async fn content(
    cfg: &ArtifactConfig,
    a: &ArtifactRow,
    served: Served,
    download: bool,
) -> ApiResult<Response> {
    let path = blob_path(cfg, &a.storage_key);
    let file = tokio::fs::File::open(&path).await.map_err(|e| {
        ApiError::Internal(anyhow::anyhow!(
            "artifact {} content {}: {e}",
            a.id,
            path.display()
        ))
    })?;
    let mut resp = Body::from_stream(tokio_util::io::ReaderStream::new(file)).into_response();
    let h = resp.headers_mut();
    for (k, v) in content_headers(a, served, download) {
        if let Ok(v) = HeaderValue::from_str(&v) {
            h.insert(k, v);
        }
    }
    h.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from(a.size.max(0) as u64),
    );
    if served == Served::Worker {
        h.insert(
            HeaderName::from_static("x-hqc-artifact-name"),
            HeaderValue::from_str(&pct(&a.name)).unwrap(),
        );
        h.insert(
            HeaderName::from_static("x-hqc-artifact-kind"),
            HeaderValue::from_str(&a.kind).unwrap_or(HeaderValue::from_static("file")),
        );
    }
    Ok(resp)
}

// ---------------------------------------------------------------- handlers

/// `POST /api/artifacts`.
pub async fn upload(
    State(app): State<AppState>,
    user: CurrentUser,
    mut mp: Multipart,
) -> ApiResult<(StatusCode, Json<Artifact>)> {
    let cfg = &app.config.artifacts;
    let mut file: Option<(Stored, String, String)> = None;
    let mut kind: Option<ArtifactKind> = None;
    loop {
        let field = match mp.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => {
                return Err(too_large(cfg.max_bytes))
            }
            Err(e) => {
                return Err(ApiError::BadRequest(format!(
                    "multipart: {}",
                    e.body_text()
                )))
            }
        };
        let mut field = field;
        match field.name().unwrap_or("") {
            "file" if file.is_none() => {
                let name = clean_name(field.file_name().unwrap_or(""));
                let mime = mime_of(field.content_type(), &name);
                let stored = store_field(cfg, &mut field).await?;
                file = Some((stored, name, mime));
            }
            "kind" => {
                let v = field
                    .text()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("multipart: {}", e.body_text())))?;
                kind = Some(parse_kind(&v)?);
            }
            // Unknown fields are ignored (SPEC §6).
            _ => {}
        }
    }
    let Some((stored, name, mime)) = file else {
        return Err(ApiError::BadRequest(
            "the multipart field `file` is required".into(),
        ));
    };
    let (row, created) = save(&app, user.id, stored, name, mime, kind).await?;
    Ok((created_status(created), Json(row.to_api())))
}

fn created_status(created: bool) -> StatusCode {
    if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    }
}

/// Stores the record of an upload of `owner` (the same content and name: the existing one,
/// `false`) and moves the content into place. `kind` absent: by type and extension (§12).
async fn save(
    app: &AppState,
    owner: Uuid,
    stored: Stored,
    name: String,
    mime: String,
    kind: Option<ArtifactKind>,
) -> ApiResult<(ArtifactRow, bool)> {
    let kind = kind.unwrap_or_else(|| kind_of(&mime, &name));
    let row = ArtifactRow {
        id: db::new_id(),
        owner_id: owner,
        session_id: None,
        kind: serde_json::to_value(kind)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "file".into()),
        mime,
        size: stored.size as i64,
        sha256: stored.sha256.clone(),
        name,
        storage_key: stored.sha256.clone(),
        created_at: Utc::now(),
    };
    let (row, created) = insert_artifact(&app.db, &row).await?;
    place(&app.config.artifacts, stored).await?;
    Ok((row, created))
}

/// `kind` as the client or the worker named it; `unknown` is refused.
fn parse_kind(v: &str) -> ApiResult<ArtifactKind> {
    let k: ArtifactKind = serde_json::from_value(v.trim().into())
        .map_err(|_| ApiError::BadRequest("kind: unknown".into()))?;
    if k == ArtifactKind::Unknown {
        return Err(ApiError::BadRequest(format!("kind: unknown {v:?}")));
    }
    Ok(k)
}

/// The user's artifact; another user's looks like a missing one.
async fn own(app: &AppState, user: &CurrentUser, id: Uuid) -> ApiResult<ArtifactRow> {
    match artifact(&app.db, id).await? {
        Some(a) if a.owner_id == user.id => Ok(a),
        _ => Err(ApiError::NotFound),
    }
}

/// `GET /api/artifacts/{id}`.
pub async fn meta(
    State(app): State<AppState>,
    user: CurrentUser,
    UrlPath(id): UrlPath<Uuid>,
) -> ApiResult<Json<Artifact>> {
    Ok(Json(own(&app, &user, id).await?.to_api()))
}

/// `GET /api/artifacts/{id}/url`.
pub async fn url(
    State(app): State<AppState>,
    user: CurrentUser,
    UrlPath(id): UrlPath<Uuid>,
) -> ApiResult<Json<ArtifactUrl>> {
    let a = own(&app, &user, id).await?;
    let cfg = &app.config.artifacts;
    let expires_at = Utc::now() + cfg.link_ttl;
    Ok(Json(ArtifactUrl {
        url: signed_url(cfg, a.id, expires_at.timestamp()),
        expires_at,
    }))
}

#[derive(Debug, Deserialize)]
pub struct SignedQuery {
    exp: Option<i64>,
    sig: Option<String>,
    /// `1`: always as an attachment (the client's "download" button).
    dl: Option<String>,
}

/// `GET /artifacts/{id}?exp=&sig=`: no session, the signature is the permission.
pub async fn signed(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<Uuid>,
    Query(q): Query<SignedQuery>,
    headers: HeaderMap,
) -> Response {
    let cfg = &app.config.artifacts;
    let (Some(exp), Some(sig)) = (q.exp, q.sig.as_deref()) else {
        return plain(StatusCode::FORBIDDEN, "unsigned link");
    };
    if !signature_ok(cfg, id, exp, sig) {
        return plain(StatusCode::FORBIDDEN, "bad signature");
    }
    if exp < Utc::now().timestamp() {
        return plain(StatusCode::GONE, "the link has expired");
    }
    let a = match artifact(&app.db, id).await {
        Ok(Some(a)) => a,
        Ok(None) => return plain(StatusCode::NOT_FOUND, "no such artifact"),
        Err(e) => return ApiError::from(e).into_response(),
    };
    let served = if is_artifact_host(cfg, &headers) {
        Served::ArtifactDomain
    } else {
        Served::HubDomain
    };
    let download = q.dl.as_deref() == Some("1");
    content(cfg, &a, served, download)
        .await
        .unwrap_or_else(IntoResponse::into_response)
}

/// The request came to the separate artifact domain (`HQC_ARTIFACT_PUBLIC_BASE`).
pub fn is_artifact_host(cfg: &ArtifactConfig, headers: &HeaderMap) -> bool {
    let Some(want) = cfg.public_host() else {
        return false;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| h.trim().to_ascii_lowercase());
    let strip = |h: String| h.strip_suffix(":443").map(String::from).unwrap_or(h);
    host.map(strip) == Some(strip(want))
}

/// A text error for the content routes: they are opened by browsers and `<img>`, not the API.
fn plain(status: StatusCode, msg: &'static str) -> Response {
    let mut r = (status, msg).into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    r
}

/// The worker of the bearer token; failures count against the address like a WebSocket
/// login's.
async fn worker_of(app: &AppState, ip: &str, headers: &HeaderMap) -> ApiResult<db::WorkerRow> {
    let max_failures = app.config.limits.ws_auth_failures_per_min;
    if app.limiter.exhausted(AUTH_FAILURES, ip, max_failures) {
        return Err(ApiError::TooManyRequests);
    }
    let refuse = || {
        app.limiter.allow(AUTH_FAILURES, ip, max_failures);
        ApiError::Unauthorized("unknown worker token".into())
    };
    let token = bearer(headers)
        .filter(|t| is_worker_token(t))
        .ok_or_else(refuse)?;
    let w = {
        let _permit = app.ws_auth.try_acquire().map_err(|_| ApiError::Busy)?;
        db::worker_by_token_hash(&app.db, &hash_secret(token)).await?
    };
    w.ok_or_else(refuse)
}

/// `GET /worker/artifacts/{id}` with the worker token (SPEC §5.3), no TTL. Only artifacts
/// given to a session of this worker; anything else is `404`.
pub async fn for_worker(
    State(app): State<AppState>,
    ClientIp(ip): ClientIp,
    UrlPath(id): UrlPath<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let w = worker_of(&app, &ip, &headers).await?;
    if !worker_may_read(&app.db, w.id, id).await? {
        return Err(ApiError::NotFound);
    }
    let a = artifact(&app.db, id).await?.ok_or(ApiError::NotFound)?;
    content(&app.config.artifacts, &a, Served::Worker, true).await
}

/// `PUT /worker/artifacts` with the worker token (SPEC §5.4, §12): the body is the file of
/// an agent's `hqc_attach`, streamed. `x-hqc-session-id` names the session it is for, which
/// must be one this worker runs (else `404`); `x-hqc-artifact-name` (percent-encoded),
/// `x-hqc-artifact-kind?` and `Content-Type` work as in the client's upload. The owner is the
/// session's owner; dedup and `kind` as for `POST /api/artifacts`. The artifact is linked to
/// the session as an input's artifacts are, which is what `hqc_attach` checks. A worker
/// uploads at most `Limits::worker_upload_bytes_per_hour` (SPEC §20): an agent attaching in
/// a loop must not fill the hub's disk; over it `429 upload_limit`, before the body is read
/// when `Content-Length` says so.
pub async fn from_worker(
    State(app): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<(StatusCode, Json<Artifact>)> {
    let w = worker_of(&app, &ip, &headers).await?;
    let text = |k: &str| headers.get(k).and_then(|v| v.to_str().ok());
    let session_id: Uuid = text("x-hqc-session-id")
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| ApiError::BadRequest("x-hqc-session-id: a session id is required".into()))?;
    let s = db::session(&app.db, session_id)
        .await?
        .filter(|s| s.worker_id == Some(w.id) && !s.system && !s.archived)
        .ok_or(ApiError::NotFound)?;
    let cfg = &app.config.artifacts;
    let announced = text(header::CONTENT_LENGTH.as_str()).and_then(|v| v.parse::<u64>().ok());
    if announced.is_some_and(|n| n > cfg.max_bytes) {
        return Err(too_large(cfg.max_bytes));
    }
    let (budget, hour, key) = (
        app.config.limits.worker_upload_bytes_per_hour,
        chrono::TimeDelta::hours(1),
        w.id.to_string(),
    );
    let left = app
        .budget
        .left(UPLOAD_BYTES, &key, budget, hour, chrono::Utc::now());
    if left == 0 || announced.is_some_and(|n| n > left) {
        tracing::warn!(worker_id = %w.id, left, ?announced, "worker upload over the hourly budget refused");
        return Err(ApiError::Limited(
            "upload_limit",
            format!("this worker uploaded its {budget} bytes of the last hour; try later"),
        ));
    }
    let name = clean_name(&pct_decode(text("x-hqc-artifact-name").unwrap_or("")));
    let mime = mime_of(text(header::CONTENT_TYPE.as_str()), &name);
    let kind = text("x-hqc-artifact-kind").map(parse_kind).transpose()?;
    let stored = store_body(cfg, body).await?;
    // Counted once read: a body without `Content-Length` is known only now. The next upload
    // finds the budget used up.
    app.budget.spend(
        UPLOAD_BYTES,
        &key,
        stored.size,
        u64::MAX,
        hour,
        chrono::Utc::now(),
    );
    let (row, created) = save(&app, s.owner_id, stored, name, mime, kind).await?;
    sqlx::query(
        "INSERT INTO artifact_sessions (artifact_id, session_id) VALUES ($1, $2)
         ON CONFLICT DO NOTHING",
    )
    .bind(row.id)
    .bind(s.id)
    .execute(&app.db)
    .await?;
    tracing::info!(worker_id = %w.id, session_id = %s.id, artifact_id = %row.id, size = row.size, created, "artifact uploaded by the worker");
    Ok((created_status(created), Json(row.to_api())))
}

/// [`crate::security::Budget`] bucket of the bytes a worker uploads.
const UPLOAD_BYTES: &str = "worker_upload_bytes";

/// The artifact was uploaded for `session_id` or given to it in an input.
pub async fn of_session(db: &PgPool, session_id: Uuid, artifact_id: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM artifact_sessions WHERE artifact_id = $1 AND session_id = $2)",
    )
    .bind(artifact_id)
    .bind(session_id)
    .fetch_one(db)
    .await
}

/// Inverse of [`pct`]; an invalid sequence stays as it is.
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Size of the request body `POST /api/artifacts` may have: the artifact plus the multipart
/// framing.
pub fn upload_body_limit(cfg: &ArtifactConfig) -> usize {
    (cfg.max_bytes as usize).saturating_add(64 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T53: a store on the root's device is reported in a container only; an unwritable
    /// store always.
    #[test]
    fn storage_warnings() {
        let base = std::env::temp_dir().join(format!("hqc-store-{}", Uuid::new_v4()));
        let dir = base.join("artifacts");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(storage_warning(&dir, None), None);
        let w = storage_warning(&dir, Some(&base)).unwrap();
        assert!(w.contains("container's own layer"), "{w}");
        assert!(!dir.join(".hqc-write-check").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
            // root writes anyway: nothing to check then.
            if std::fs::write(dir.join("x"), b"").is_err() {
                let w = storage_warning(&dir, None).unwrap();
                assert!(w.contains("not writable"), "{w}");
            }
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn kinds() {
        for (mime, name, kind) in [
            ("image/png", "a.png", ArtifactKind::Image),
            ("image/svg+xml", "a.svg", ArtifactKind::Image),
            ("text/x-diff", "x", ArtifactKind::Diff),
            ("text/plain", "fix.patch", ArtifactKind::Diff),
            ("application/octet-stream", "fix.diff", ArtifactKind::Diff),
            ("text/html", "r.html", ArtifactKind::Html),
            ("text/plain", "a.txt", ArtifactKind::Text),
            ("text/markdown", "a.md", ArtifactKind::Text),
            ("application/pdf", "a.pdf", ArtifactKind::File),
            ("application/zip", "a.zip", ArtifactKind::File),
        ] {
            assert_eq!(kind_of(mime, name), kind, "{mime} {name}");
        }
        assert_eq!(mime_of(None, "main.RS"), "text/plain");
        assert_eq!(
            mime_of(Some("application/octet-stream"), "a.png"),
            "image/png"
        );
        assert_eq!(mime_of(Some("Text/HTML; charset=utf-8"), "x"), "text/html");
        assert_eq!(mime_of(Some(""), "noext"), "application/octet-stream");
        assert_eq!(
            kind_of(&mime_of(None, "page.htm"), "page.htm"),
            ArtifactKind::Html
        );
    }

    #[test]
    fn names() {
        assert_eq!(clean_name("C:\\Users\\me\\shot.png"), "shot.png");
        assert_eq!(clean_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_name(" a\u{0}b\n.txt "), "ab.txt");
        assert_eq!(clean_name(".."), "file");
        assert_eq!(clean_name(""), "file");
        let long = "я".repeat(200);
        let c = clean_name(&long);
        assert!(c.len() <= MAX_NAME && c.chars().all(|ch| ch == 'я'));
    }

    #[test]
    fn signatures() {
        let cfg = ArtifactConfig::new("x".into());
        let id = Uuid::now_v7();
        let url = signed_url(&cfg, id, 100);
        assert!(
            url.starts_with(&format!("/artifacts/{id}?exp=100&sig=")),
            "{url}"
        );
        let sig = url.rsplit_once("sig=").unwrap().1;
        assert!(signature_ok(&cfg, id, 100, sig));
        assert!(!signature_ok(&cfg, id, 101, sig));
        assert!(!signature_ok(&cfg, Uuid::now_v7(), 100, sig));
        assert!(!signature_ok(
            &ArtifactConfig::new("x".into()),
            id,
            100,
            sig
        ));
        assert!(!signature_ok(&cfg, id, 100, "!!"));
    }

    fn row(kind: &str, mime: &str, name: &str) -> ArtifactRow {
        ArtifactRow {
            id: Uuid::nil(),
            owner_id: Uuid::nil(),
            session_id: None,
            kind: kind.into(),
            mime: mime.into(),
            size: 1,
            sha256: String::new(),
            name: name.into(),
            storage_key: String::new(),
            created_at: Utc::now(),
        }
    }

    fn get(h: &[(HeaderName, String)], k: HeaderName) -> String {
        h.iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }

    #[test]
    fn html_runs_only_on_the_artifact_domain() {
        let html = row("html", "text/html", "r.html");
        let h = content_headers(&html, Served::HubDomain, false);
        assert!(get(&h, header::CONTENT_DISPOSITION).starts_with("attachment;"));
        assert_eq!(get(&h, header::CONTENT_SECURITY_POLICY), CSP);
        assert_eq!(get(&h, header::X_FRAME_OPTIONS), "DENY");
        let h = content_headers(&html, Served::ArtifactDomain, false);
        assert!(get(&h, header::CONTENT_DISPOSITION).starts_with("inline;"));
        assert_eq!(get(&h, header::CONTENT_SECURITY_POLICY), CSP_HTML);
        assert_eq!(get(&h, header::X_FRAME_OPTIONS), "");
        let h = content_headers(&html, Served::ArtifactDomain, true);
        assert!(get(&h, header::CONTENT_DISPOSITION).starts_with("attachment;"));

        // A "text" file that is really HTML is still plain text.
        let t = content_headers(
            &row("text", "text/html", "x.txt"),
            Served::ArtifactDomain,
            false,
        );
        assert_eq!(get(&t, header::CONTENT_TYPE), "text/plain; charset=utf-8");
        let f = content_headers(
            &row("file", "application/pdf", "a.pdf"),
            Served::HubDomain,
            false,
        );
        assert_eq!(get(&f, header::CONTENT_TYPE), "application/octet-stream");
        assert!(get(&f, header::CONTENT_DISPOSITION).starts_with("attachment;"));
        let i = content_headers(
            &row("image", "text/html", "a.png"),
            Served::HubDomain,
            false,
        );
        assert_eq!(get(&i, header::CONTENT_TYPE), "application/octet-stream");
    }

    #[test]
    fn disposition_names() {
        let h = content_headers(&row("file", "x", "отчёт \"1\".pdf"), Served::Worker, true);
        assert_eq!(
            get(&h, header::CONTENT_DISPOSITION),
            "attachment; filename=\"_____ _1_.pdf\"; filename*=UTF-8''%D0%BE%D1%82%D1%87%D1%91%D1%82%20%221%22.pdf"
        );
    }
}
