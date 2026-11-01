//! `.hqc/inbox` (SPEC §5.3 `session.input`, §8.1, §12): the artifacts of an input are
//! downloaded from the hub with the worker token into
//! `<workspace>/<cwd>/.hqc/inbox/<event_id>/<name>` before the input goes to the runtime;
//! their paths are appended to the text as a block "Attachments:", and `.hqc/` is added to the
//! workspace's `.git/info/exclude`.
//!
//! Since T41 also the files of buffered messages in `peer_prefix` (each in the folder of its
//! message) and of a reply to `hqc_ask` (in the folder of the call, [`fetch_reply`]).
//!
//! The hub is not trusted with the file system (SPEC §2.1): names are sanitized here, the
//! inbox folder must resolve inside the workspace (a symlinked `.hqc` is refused), and files
//! are written to a fresh temporary file and renamed into place, so an existing symlink is
//! replaced, never followed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use hqc_proto::event::ArtifactKind;
use hqc_proto::frame::SessionInput;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::config::StartSpec;
use crate::token::Secret;

/// Folder of hqc inside the working directory; excluded from git.
pub const HQC_DIR: &str = ".hqc";
/// Longest file name written, in bytes.
const MAX_NAME: usize = 120;
/// Heading of the block of paths appended to the input text.
pub const BLOCK_HEADING: &str = "Attachments:";
/// Largest artifact downloaded, as the hub's own limit (SPEC §12, §20).
pub const MAX_ARTIFACT_BYTES: u64 = 100 * 1000 * 1000;
/// Largest sum of the artifacts of one input (SPEC §20).
pub const MAX_INPUT_BYTES: u64 = 500 * 1000 * 1000;

/// One downloaded artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxFile {
    pub artifact_id: Uuid,
    /// Absolute path of the file.
    pub path: PathBuf,
    /// The path as given to the agent: relative to the working directory.
    pub rel: String,
    pub kind: ArtifactKind,
    /// `Content-Type` the hub served it with, without parameters.
    pub mime: String,
    pub size: u64,
}

/// Why an input could not be prepared; decides whether the hub should retry.
#[derive(Debug, thiserror::Error)]
pub enum InboxError {
    /// The hub will not give the artifact (`404`, `401`): retrying does not help.
    #[error("artifact {0} is not available: {1}")]
    Unavailable(Uuid, String),
    /// Over [`MAX_ARTIFACT_BYTES`] or, with the artifacts before it, [`MAX_INPUT_BYTES`]:
    /// it would be too large again.
    #[error("artifact {0} is too large: {1} bytes are left for it (SPEC §20)")]
    TooLarge(Uuid, u64),
    /// Network or local I/O: worth another try.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl InboxError {
    pub fn retryable(&self) -> bool {
        matches!(self, InboxError::Other(_))
    }
}

/// Downloads artifacts from `GET /worker/artifacts/{id}` with the worker token.
#[derive(Clone)]
pub struct ArtifactClient {
    http: reqwest::Client,
    hub_url: String,
    token: Secret,
    max_artifact: u64,
    max_input: u64,
    /// Where the working directories with an inbox are remembered, for the sweep at start
    /// ([`crate::cleanup::sweep`], T21).
    inboxes: Option<std::sync::Arc<crate::known_paths::KnownPaths>>,
}

impl ArtifactClient {
    pub fn new(hub_url: &str, token: Secret) -> Self {
        let http = reqwest::Client::builder()
            // The token must not follow a redirect anywhere else.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest client");
        Self {
            http,
            hub_url: hub_url.trim_end_matches('/').to_string(),
            token,
            max_artifact: MAX_ARTIFACT_BYTES,
            max_input: MAX_INPUT_BYTES,
            inboxes: None,
        }
    }

    /// Other limits per artifact and per input than [`MAX_ARTIFACT_BYTES`] and
    /// [`MAX_INPUT_BYTES`]; tests.
    pub fn with_limits(mut self, max_artifact: u64, max_input: u64) -> Self {
        self.max_artifact = max_artifact;
        self.max_input = max_input;
        self
    }

    pub fn with_inboxes(mut self, k: std::sync::Arc<crate::known_paths::KnownPaths>) -> Self {
        self.inboxes = Some(k);
        self
    }

    /// Largest artifact downloaded or uploaded.
    pub fn max_artifact(&self) -> u64 {
        self.max_artifact
    }

    /// Streams the first `size` bytes of `file` to `PUT /worker/artifacts` for `session_id`
    /// (an agent's `hqc_attach`, SPEC §7, §12); the artifact's id.
    pub async fn upload(
        &self,
        session_id: Uuid,
        file: std::fs::File,
        size: u64,
        name: &str,
        kind: Option<ArtifactKind>,
    ) -> Result<Uuid, hqc_proto::frame::McpError> {
        use hqc_proto::frame::McpError;
        use hqc_proto::mcp::error_code;
        use tokio::io::AsyncReadExt;
        let body = tokio::fs::File::from_std(file).take(size);
        let mut req = self
            .http
            .put(format!("{}/worker/artifacts", self.hub_url))
            .bearer_auth(self.token.expose())
            .header("x-hqc-session-id", session_id.to_string())
            .header("x-hqc-artifact-name", pct_encode(name))
            .header(reqwest::header::CONTENT_LENGTH, size)
            .body(reqwest::Body::wrap_stream(
                tokio_util::io::ReaderStream::new(body),
            ));
        if let Some(k) = kind {
            if let Some(k) = serde_json::to_value(k)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
            {
                req = req.header("x-hqc-artifact-kind", k);
            }
        }
        let unavailable = |m: String| McpError::new(error_code::UNAVAILABLE, m);
        let resp = req
            .send()
            .await
            .map_err(|e| unavailable(format!("upload to the hub: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let code = match status.as_u16() {
                413 | 400 => error_code::INVALID_ARGS,
                401 | 404 => error_code::UNAUTHORIZED,
                // The worker's uploads per hour (SPEC §20).
                429 => error_code::LIMIT_EXCEEDED,
                _ => error_code::UNAVAILABLE,
            };
            return Err(McpError::new(
                code,
                format!("the hub refused the upload: {status} {}", text.trim()),
            ));
        }
        #[derive(serde::Deserialize)]
        struct Uploaded {
            id: Uuid,
        }
        resp.json::<Uploaded>()
            .await
            .map(|u| u.id)
            .map_err(|e| unavailable(format!("the hub's answer to the upload: {e}")))
    }

    /// Streams the artifact into a new temporary file in `dir`; returns it with the name,
    /// kind and type the hub gave. At most `budget` bytes (and [`Self::max_artifact`]) are
    /// written: beyond that the download stops and the partial file is removed.
    async fn download(&self, id: Uuid, dir: &Path, budget: u64) -> Result<Downloaded, InboxError> {
        let limit = self.max_artifact.min(budget);
        let url = format!("{}/worker/artifacts/{id}", self.hub_url);
        let mut resp = self
            .http
            .get(&url)
            .bearer_auth(self.token.expose())
            .send()
            .await
            .with_context(|| format!("GET /worker/artifacts/{id}"))?;
        let status = resp.status();
        if matches!(status.as_u16(), 401 | 403 | 404 | 410) {
            return Err(InboxError::Unavailable(id, status.to_string()));
        }
        if !status.is_success() {
            return Err(anyhow!("GET /worker/artifacts/{id}: {status}").into());
        }
        let h = resp.headers();
        let text = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).unwrap_or("");
        let name = pct_decode(text("x-hqc-artifact-name"));
        let kind = serde_json::from_value(text("x-hqc-artifact-kind").into())
            .unwrap_or(ArtifactKind::File);
        let mime = text("content-type")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        // A hub that announces too much is not read at all.
        if resp.content_length().is_some_and(|n| n > limit) {
            return Err(InboxError::TooLarge(id, limit));
        }
        let tmp = dir.join(format!(".part-{}", Uuid::new_v4()));
        let mut f = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .await
            .with_context(|| format!("create {}", tmp.display()))?;
        let mut size = 0u64;
        let r: Result<(), InboxError> = async {
            while let Some(chunk) = resp.chunk().await.map_err(anyhow::Error::from)? {
                size += chunk.len() as u64;
                if size > limit {
                    return Err(InboxError::TooLarge(id, limit));
                }
                f.write_all(&chunk).await.map_err(anyhow::Error::from)?;
            }
            f.flush().await.map_err(anyhow::Error::from)?;
            Ok(())
        }
        .await;
        if let Err(e) = r {
            drop(f);
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(match e {
                InboxError::Other(e) => e.context(format!("download artifact {id}")).into(),
                e => e,
            });
        }
        Ok(Downloaded {
            tmp,
            name,
            kind,
            mime,
            size,
        })
    }
}

struct Downloaded {
    tmp: PathBuf,
    name: String,
    kind: ArtifactKind,
    mime: String,
    size: u64,
}

fn distinct(ids: &[Uuid]) -> Vec<Uuid> {
    let mut out: Vec<Uuid> = Vec::new();
    for id in ids {
        if !out.contains(id) {
            out.push(*id);
        }
    }
    out
}

/// Before the first file goes into an inbox of `workdir`: the folder is remembered for the
/// sweep at start, and `.hqc/` excluded from git.
fn touch(client: &ArtifactClient, workspace: &Path, workdir: &Path, allowed_roots: &[PathBuf]) {
    if let Some(k) = &client.inboxes {
        if let Err(e) = k.insert(workdir) {
            tracing::warn!(workdir = %workdir.display(), "inbox folder not remembered: {e:#}");
        }
    }
    if let Err(e) = exclude_hqc(workspace, allowed_roots) {
        tracing::warn!(workspace = %workspace.display(), "cannot add .hqc/ to .git/info/exclude: {e:#}");
    }
}

/// The input as the runtime gets it: the text with the block of paths, and the files.
/// Without artifacts the input is unchanged and nothing is touched on disk.
///
/// The files of a buffered message in `peer_prefix` (T41) go to the folder of that message
/// (`.hqc/inbox/<its event_id>/`) and their block follows its text; the limit per input
/// counts them together with the input's own.
pub async fn prepare(
    client: &ArtifactClient,
    spec: &StartSpec,
    input: &SessionInput,
    allowed_roots: &[PathBuf],
) -> Result<(SessionInput, Vec<InboxFile>), InboxError> {
    let own = distinct(&input.artifact_ids);
    if own.is_empty() && input.peer_prefix.iter().all(|p| p.artifact_ids.is_empty()) {
        return Ok((input.clone(), vec![]));
    }
    touch(client, &spec.workspace, &spec.workdir, allowed_roots);
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut total = 0u64;
    let r: Result<(SessionInput, Vec<InboxFile>), InboxError> = async {
        let mut out = input.clone();
        let mut all = Vec::new();
        for p in out.peer_prefix.iter_mut() {
            let ids = distinct(&p.artifact_ids);
            if ids.is_empty() {
                continue;
            }
            let folder = p.event_id.unwrap_or_else(Uuid::now_v7);
            let dir = inbox_dir(&spec.workspace, &spec.workdir, folder)?;
            dirs.push(dir.clone());
            let files = fetch_all(client, &dir, folder, ids, &mut total).await?;
            p.text = with_block(&p.text, &files);
            all.extend(files);
        }
        if !own.is_empty() {
            let dir = inbox_dir(&spec.workspace, &spec.workdir, input.event_id)?;
            dirs.push(dir.clone());
            let files = fetch_all(client, &dir, input.event_id, own, &mut total).await?;
            out.text = with_block(&input.text, &files);
            all.extend(files);
        }
        Ok((out, all))
    }
    .await;
    if r.is_err() {
        // The hub sends the input again (retryable) or it has failed: start clean.
        for d in dirs {
            let _ = std::fs::remove_dir_all(&d);
        }
    }
    r
}

/// The files of an `hqc_ask` reply (T41, SPEC §7.1) for the session working in `workdir`:
/// downloaded into `.hqc/inbox/<folder>/` (`folder` is the call's id) with the limits of an
/// input, each one given back with its absolute path, or with the error that stopped it.
pub async fn fetch_reply(
    client: &ArtifactClient,
    workspace: &Path,
    workdir: &Path,
    allowed_roots: &[PathBuf],
    folder: Uuid,
    files: &[hqc_proto::mcp::ReplyArtifact],
) -> Vec<hqc_proto::mcp::ReplyFile> {
    use hqc_proto::mcp::ReplyFile;
    let failed = |f: &hqc_proto::mcp::ReplyArtifact, e: String| ReplyFile {
        artifact_id: f.artifact_id,
        name: f.name.clone(),
        kind: f.kind,
        path: None,
        size: None,
        error: Some(e),
    };
    if files.is_empty() {
        return vec![];
    }
    touch(client, workspace, workdir, allowed_roots);
    let dir = match inbox_dir(workspace, workdir, folder) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(workdir = %workdir.display(), "reply files: {e:#}");
            return files.iter().map(|f| failed(f, format!("{e:#}"))).collect();
        }
    };
    let mut out = Vec::new();
    let mut taken: Vec<String> = Vec::new();
    let mut total = 0u64;
    for f in files {
        match fetch_one(client, &dir, folder, f.artifact_id, &mut total, &mut taken).await {
            Ok(got) => out.push(ReplyFile {
                artifact_id: f.artifact_id,
                name: f.name.clone(),
                kind: f.kind,
                path: Some(got.path.display().to_string()),
                size: Some(got.size),
                error: None,
            }),
            Err(e) => {
                tracing::warn!(artifact_id = %f.artifact_id, "reply file not downloaded: {e:#}");
                out.push(failed(f, format!("not downloaded: {e}")));
            }
        }
    }
    out
}

async fn fetch_all(
    client: &ArtifactClient,
    dir: &Path,
    event_id: Uuid,
    ids: Vec<Uuid>,
    total: &mut u64,
) -> Result<Vec<InboxFile>, InboxError> {
    let mut files = Vec::new();
    let mut taken: Vec<String> = Vec::new();
    for id in ids {
        files.push(fetch_one(client, dir, event_id, id, total, &mut taken).await?);
    }
    Ok(files)
}

/// Downloads artifact `id` into `dir` (`.hqc/inbox/<folder>`) under a name not in `taken`,
/// within what is left of [`ArtifactClient::max_input`] after `total`.
async fn fetch_one(
    client: &ArtifactClient,
    dir: &Path,
    folder: Uuid,
    id: Uuid,
    total: &mut u64,
    taken: &mut Vec<String>,
) -> Result<InboxFile, InboxError> {
    let budget = client.max_input.saturating_sub(*total);
    let d = client.download(id, dir, budget).await?;
    *total += d.size;
    let name = unique(&sanitize_name(&d.name), taken);
    taken.push(name.clone());
    let path = dir.join(&name);
    // rename replaces whatever is at `path` (a symlink included) without following it.
    std::fs::rename(&d.tmp, &path)
        .with_context(|| format!("move into {}", path.display()))
        .map_err(InboxError::Other)?;
    let rel = format!("{HQC_DIR}/inbox/{folder}/{name}");
    Ok(InboxFile {
        artifact_id: id,
        path,
        rel,
        kind: d.kind,
        mime: d.mime,
        size: d.size,
    })
}

/// `<workdir>/.hqc/inbox/<folder>`, created one level at a time: the working directory
/// must still resolve inside the workspace, and none of the three levels may be a symlink,
/// so nothing is created outside the workspace.
fn inbox_dir(workspace: &Path, workdir: &Path, folder: Uuid) -> Result<PathBuf, InboxError> {
    crate::config::recheck_workdir(workspace, workdir)?;
    let mut dir = workdir
        .canonicalize()
        .with_context(|| format!("resolve {}", workdir.display()))?;
    for part in [HQC_DIR, "inbox", &folder.to_string()] {
        dir.push(part);
        match std::fs::symlink_metadata(&dir) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(anyhow!(
                    "{} is a symlink: refusing to write outside the workspace",
                    dir.display()
                )
                .into())
            }
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Err(anyhow!("{} is not a directory", dir.display()).into()),
            Err(_) => std::fs::create_dir(&dir)
                .or_else(|e| match e.kind() {
                    std::io::ErrorKind::AlreadyExists if dir.is_dir() => Ok(()),
                    _ => Err(e),
                })
                .with_context(|| format!("create {}", dir.display()))?,
        }
    }
    Ok(dir)
}

/// The text followed by "Attachments:" and one path per line.
pub fn with_block(text: &str, files: &[InboxFile]) -> String {
    if files.is_empty() {
        return text.to_string();
    }
    let mut s = String::new();
    let t = text.trim_end();
    if !t.is_empty() {
        s.push_str(t);
        s.push_str("\n\n");
    }
    s.push_str(BLOCK_HEADING);
    for f in files {
        s.push_str("\n- ");
        s.push_str(&f.rel);
    }
    s
}

/// A file name safe on every file system: the last path component, no control or reserved
/// characters, no leading dots (no hidden files, no `..`), no trailing dots or spaces, at most
/// [`MAX_NAME`] bytes keeping the extension; `file` when nothing is left.
pub fn sanitize_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mapped: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = mapped
        .trim_start_matches(['.', ' '])
        .trim_end_matches(['.', ' ']);
    let mut name = trimmed.to_string();
    if name.len() > MAX_NAME {
        let (stem, ext) = match name.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() && e.len() <= 16 => (s.to_string(), format!(".{e}")),
            _ => (name.clone(), String::new()),
        };
        let mut cut = MAX_NAME.saturating_sub(ext.len());
        while !stem.is_char_boundary(cut) {
            cut -= 1;
        }
        name = format!("{}{ext}", &stem[..cut]);
    }
    if name.is_empty() {
        "file".into()
    } else {
        name
    }
}

/// `name`, or `name-2.ext`, `name-3.ext`, ... if taken (case-insensitive: macOS).
fn unique(name: &str, taken: &[String]) -> String {
    let free = |n: &str| !taken.iter().any(|t| t.eq_ignore_ascii_case(n));
    if free(name) {
        return name.to_string();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    (2..)
        .map(|i| format!("{stem}-{i}{ext}"))
        .find(|n| free(n))
        .unwrap()
}

/// Adds `.hqc/` to the git exclude file of the workspace, if it is a git checkout (a `.git`
/// folder, or a `.git` file of a worktree pointing at its git dir). Nothing else is created.
/// The git dir a `.git` file points at is followed only inside `allowed_roots`: the worker
/// never writes outside its roots (SPEC §2.1, §14), whatever a checkout's `gitdir:` says.
pub fn exclude_hqc(workspace: &Path, allowed_roots: &[PathBuf]) -> anyhow::Result<()> {
    let dot_git = workspace.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else if dot_git.is_file() {
        let s = std::fs::read_to_string(&dot_git)?;
        let Some(p) = s.trim().strip_prefix("gitdir:") else {
            bail!("{}: not a gitdir file", dot_git.display());
        };
        let gd = workspace.join(p.trim());
        // A linked worktree reads info/exclude of the common git dir.
        match std::fs::read_to_string(gd.join("commondir")) {
            Ok(c) => gd.join(c.trim()),
            Err(_) => gd,
        }
    } else {
        return Ok(());
    };
    if !git_dir.is_dir() {
        bail!("{}: not a directory", git_dir.display());
    }
    let canonical = std::fs::canonicalize(&git_dir)?;
    let inside = allowed_roots.iter().any(|r| {
        std::fs::canonicalize(r)
            .map(|r| canonical.starts_with(r))
            .unwrap_or(false)
    });
    if !inside {
        bail!(
            "{}: git dir outside allowed_roots, .hqc/ not added to info/exclude",
            canonical.display()
        );
    }
    let git_dir = canonical;
    let info = git_dir.join("info");
    std::fs::create_dir_all(&info)?;
    let exclude = info.join("exclude");
    let current = std::fs::read_to_string(&exclude).unwrap_or_default();
    let line = format!("{HQC_DIR}/");
    if current
        .lines()
        .any(|l| matches!(l.trim(), ".hqc/" | ".hqc" | "/.hqc/" | "/.hqc"))
    {
        return Ok(());
    }
    let mut out = current;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("# hqc: inbox of attachments (SPEC §12)\n");
    out.push_str(&line);
    out.push('\n');
    std::fs::write(&exclude, out)?;
    Ok(())
}

/// Percent-encoding of `x-hqc-artifact-name`: everything but RFC 3986 unreserved characters.
pub fn pct_encode(s: &str) -> String {
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

/// Percent-decoding of `x-hqc-artifact-name`; invalid UTF-8 is replaced.
pub fn pct_decode(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for (raw, want) in [
            ("shot.png", "shot.png"),
            ("../../etc/passwd", "passwd"),
            ("C:\\x\\a.txt", "a.txt"),
            (".bashrc", "bashrc"),
            ("..", "file"),
            ("", "file"),
            ("a:b*c?.txt", "a_b_c_.txt"),
            ("bad\u{0}\n.txt", "bad__.txt"),
            ("trailing. ", "trailing"),
            ("отчёт 1.pdf", "отчёт 1.pdf"),
        ] {
            assert_eq!(sanitize_name(raw), want, "{raw:?}");
        }
        let long = format!("{}.png", "я".repeat(100));
        let s = sanitize_name(&long);
        assert!(s.len() <= MAX_NAME && s.ends_with(".png"), "{s}");
    }

    /// A hub on loopback serving `/worker/artifacts/{id}`: `(id, size, chunked)` — a body of
    /// `size` bytes, without `Content-Length` when `chunked`.
    async fn fake_hub(arts: Vec<(Uuid, usize, bool)>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut c, _)) = l.accept().await {
                let arts = arts.clone();
                tokio::spawn(async move {
                    let mut req = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = c.read(&mut buf).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        req.extend_from_slice(&buf[..n]);
                    }
                    let line = String::from_utf8_lossy(&req)
                        .lines()
                        .next()
                        .unwrap()
                        .to_string();
                    let id: Uuid = line
                        .split(' ')
                        .nth(1)
                        .unwrap()
                        .rsplit('/')
                        .next()
                        .unwrap()
                        .parse()
                        .unwrap();
                    let (_, size, chunked) = *arts.iter().find(|a| a.0 == id).unwrap();
                    let head = "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\nx-hqc-artifact-name: a.txt\r\nx-hqc-artifact-kind: text\r\nconnection: close\r\n";
                    let body = vec![b'x'; size];
                    let r = if chunked {
                        let mut out =
                            format!("{head}transfer-encoding: chunked\r\n\r\n").into_bytes();
                        for part in body.chunks(1000) {
                            out.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
                            out.extend_from_slice(part);
                            out.extend_from_slice(b"\r\n");
                        }
                        out.extend_from_slice(b"0\r\n\r\n");
                        out
                    } else {
                        let mut out = format!("{head}content-length: {size}\r\n\r\n").into_bytes();
                        out.extend_from_slice(&body);
                        out
                    };
                    let _ = c.write_all(&r).await;
                });
            }
        });
        url
    }

    fn entries(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    /// An artifact over the per-artifact limit, announced or streamed, and artifacts over the
    /// per-input sum are refused without retry; nothing partial stays on disk.
    #[tokio::test]
    async fn downloads_are_limited() {
        let (small, big, chunked, a, b) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let url = fake_hub(vec![
            (small, 900, false),
            (big, 5000, false),
            (chunked, 5000, true),
            (a, 1500, true),
            (b, 1500, true),
        ])
        .await;
        let client = ArtifactClient::new(&url, Secret::new("hqcw_test")).with_limits(2000, 2500);
        let d = tempfile::tempdir().unwrap();

        let files = fetch_all(&client, d.path(), Uuid::nil(), vec![small], &mut 0)
            .await
            .unwrap();
        assert_eq!(files[0].size, 900);
        std::fs::remove_file(&files[0].path).unwrap();

        for id in [big, chunked] {
            let e = fetch_all(&client, d.path(), Uuid::nil(), vec![id], &mut 0)
                .await
                .unwrap_err();
            assert!(matches!(e, InboxError::TooLarge(i, 2000) if i == id), "{e}");
            assert!(!e.retryable());
            assert!(entries(d.path()).is_empty(), "{:?}", entries(d.path()));
        }

        // 1500 + 1500 is over the 2500 of one input: the second stops at the 1000 left.
        let e = fetch_all(&client, d.path(), Uuid::nil(), vec![a, b], &mut 0)
            .await
            .unwrap_err();
        assert!(matches!(e, InboxError::TooLarge(i, 1000) if i == b), "{e}");
        assert!(!e.retryable());
        assert!(
            entries(d.path()).iter().all(|n| !n.starts_with(".part-")),
            "{:?}",
            entries(d.path())
        );
    }

    #[test]
    fn unique_names() {
        let taken = vec!["a.txt".to_string(), "A-2.txt".to_string()];
        assert_eq!(unique("b.txt", &taken), "b.txt");
        assert_eq!(unique("a.txt", &taken), "a-3.txt");
        assert_eq!(unique("A.TXT", &["a.txt".into()]), "A-2.TXT");
        assert_eq!(unique("noext", &["noext".into()]), "noext-2");
    }

    #[test]
    fn decode() {
        assert_eq!(pct_decode("%D0%BE%D1%82%20a.txt"), "от a.txt");
        assert_eq!(pct_decode("plain"), "plain");
        assert_eq!(pct_decode("bad%zz%"), "bad%zz%");
    }

    #[test]
    fn block() {
        let f = |rel: &str| InboxFile {
            artifact_id: Uuid::nil(),
            path: PathBuf::new(),
            rel: rel.into(),
            kind: ArtifactKind::File,
            mime: String::new(),
            size: 0,
        };
        assert_eq!(with_block("hi", &[]), "hi");
        assert_eq!(
            with_block(
                "look\n",
                &[f(".hqc/inbox/e/a.png"), f(".hqc/inbox/e/b.txt")]
            ),
            "look\n\nAttachments:\n- .hqc/inbox/e/a.png\n- .hqc/inbox/e/b.txt"
        );
        assert_eq!(with_block("  ", &[f("x")]), "Attachments:\n- x");
    }

    #[test]
    fn exclude() {
        let d = tempfile::tempdir().unwrap();
        // Not a git checkout: nothing is created.
        let roots = vec![d.path().to_path_buf()];
        exclude_hqc(d.path(), &roots).unwrap();
        assert!(!d.path().join(".git").exists());

        std::fs::create_dir_all(d.path().join(".git")).unwrap();
        exclude_hqc(d.path(), &roots).unwrap();
        exclude_hqc(d.path(), &roots).unwrap();
        let e = std::fs::read_to_string(d.path().join(".git/info/exclude")).unwrap();
        assert_eq!(e.matches(".hqc/").count(), 1, "{e}");

        // A linked worktree: the common git dir's exclude.
        let wt = d.path().join("wt");
        let gd = d.path().join(".git/worktrees/wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::create_dir_all(&gd).unwrap();
        std::fs::write(gd.join("commondir"), "../..\n").unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", gd.display())).unwrap();
        let common = d.path().join(".git/info/exclude");
        std::fs::write(&common, "target/").unwrap();
        exclude_hqc(&wt, &roots).unwrap();
        let e = std::fs::read_to_string(&common).unwrap();
        assert!(e.starts_with("target/\n") && e.ends_with(".hqc/\n"), "{e}");
    }

    /// A `.git` file whose `gitdir:` points outside `allowed_roots` is not followed: the
    /// worker must not write anywhere outside its roots (SPEC §2.1, §14).
    #[test]
    fn exclude_does_not_follow_gitdir_outside_roots() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let gd = outside.path().join("repo.git");
        std::fs::create_dir_all(gd.join("info")).unwrap();
        let wt = root.path().join("ws");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", gd.display())).unwrap();
        let err = exclude_hqc(&wt, &[root.path().to_path_buf()]).unwrap_err();
        assert!(err.to_string().contains("outside allowed_roots"), "{err}");
        assert!(!gd.join("info/exclude").exists());
        // Inside the roots it is followed as before.
        exclude_hqc(
            &wt,
            &[root.path().to_path_buf(), outside.path().to_path_buf()],
        )
        .unwrap();
        assert!(std::fs::read_to_string(gd.join("info/exclude"))
            .unwrap()
            .contains(".hqc/"));
    }
}
