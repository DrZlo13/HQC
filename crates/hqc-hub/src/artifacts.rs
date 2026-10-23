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
//!
//! Every content response carries [`content_headers`]: a `sandbox` CSP, `nosniff` and a
//! `Content-Disposition` by kind. HTML is inline only on a separate artifact domain;
//! on the hub's own domain it is always an attachment.

use std::path::PathBuf;

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
    let old = sqlx::query_as(&format!(
        "SELECT {COLS} FROM artifacts WHERE owner_id = $1 AND sha256 = $2 AND name = $3"
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
    sqlx::query("UPDATE artifacts SET session_id = $2 WHERE id = ANY($1) AND session_id IS NULL")
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
}

/// Streams one multipart field into the store. Over the limit: `413 too_large`.
async fn store_field(
    cfg: &ArtifactConfig,
    field: &mut axum::extract::multipart::Field<'_>,
) -> ApiResult<Stored> {
    let tmp_dir = cfg.dir.join("tmp");
    tokio::fs::create_dir_all(&tmp_dir)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    let tmp = TempFile(Some(tmp_dir.join(Uuid::new_v4().to_string())));
    let path = tmp.0.clone().unwrap();
    let mut f = tokio::fs::File::create(&path)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    let mut hash = Sha256::new();
    let mut size: u64 = 0;
    let too_large =
        || ApiError::TooLarge(format!("an artifact is at most {} bytes", cfg.max_bytes));
    loop {
        let chunk = match field.chunk().await {
            Ok(Some(c)) => c,
            Ok(None) => break,
            // The body limit cuts the stream: the upload was too large.
            Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => return Err(too_large()),
            Err(e) => return Err(ApiError::BadRequest(format!("multipart: {e}"))),
        };
        size += chunk.len() as u64;
        if size > cfg.max_bytes {
            return Err(too_large());
        }
        hash.update(&chunk);
        f.write_all(&chunk)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    }
    f.sync_all()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    drop(f);
    let sha256: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let dest = blob_path(cfg, &sha256);
    if tokio::fs::metadata(&dest).await.is_err() {
        tokio::fs::create_dir_all(dest.parent().unwrap())
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
        tokio::fs::rename(&path, &dest)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    }
    Ok(Stored { sha256, size })
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
                return Err(ApiError::TooLarge(format!(
                    "an artifact is at most {} bytes",
                    cfg.max_bytes
                )))
            }
            Err(e) => return Err(ApiError::BadRequest(format!("multipart: {e}"))),
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
                    .map_err(|e| ApiError::BadRequest(format!("multipart: {e}")))?;
                let k: ArtifactKind = serde_json::from_value(v.trim().into())
                    .map_err(|_| ApiError::BadRequest("kind: unknown".into()))?;
                if k == ArtifactKind::Unknown {
                    return Err(ApiError::BadRequest(format!("kind: unknown {v:?}")));
                }
                kind = Some(k);
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
    let kind = kind.unwrap_or_else(|| kind_of(&mime, &name));
    let row = ArtifactRow {
        id: db::new_id(),
        owner_id: user.id,
        session_id: None,
        kind: serde_json::to_value(kind)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "file".into()),
        mime,
        size: stored.size as i64,
        sha256: stored.sha256.clone(),
        name,
        storage_key: stored.sha256,
        created_at: Utc::now(),
    };
    let (row, created) = insert_artifact(&app.db, &row).await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(row.to_api())))
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

/// `GET /worker/artifacts/{id}` with the worker token (SPEC §5.3), no TTL. Only artifacts
/// given to a session of this worker; anything else is `404`.
pub async fn for_worker(
    State(app): State<AppState>,
    ClientIp(ip): ClientIp,
    UrlPath(id): UrlPath<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let max_failures = app.config.limits.ws_auth_failures_per_min;
    if app.limiter.exhausted(AUTH_FAILURES, &ip, max_failures) {
        return Err(ApiError::TooManyRequests);
    }
    let refuse = || {
        app.limiter.allow(AUTH_FAILURES, &ip, max_failures);
        ApiError::Unauthorized("unknown worker token".into())
    };
    let token = bearer(&headers)
        .filter(|t| is_worker_token(t))
        .ok_or_else(refuse)?;
    let w = {
        let _permit = app.ws_auth.try_acquire().map_err(|_| ApiError::Busy)?;
        db::worker_by_token_hash(&app.db, &hash_secret(token)).await?
    };
    let w = w.ok_or_else(refuse)?;
    if !worker_may_read(&app.db, w.id, id).await? {
        return Err(ApiError::NotFound);
    }
    let a = artifact(&app.db, id).await?.ok_or(ApiError::NotFound)?;
    content(&app.config.artifacts, &a, Served::Worker, true).await
}

/// Size of the request body `POST /api/artifacts` may have: the artifact plus the multipart
/// framing.
pub fn upload_body_limit(cfg: &ArtifactConfig) -> usize {
    (cfg.max_bytes as usize).saturating_add(64 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

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
