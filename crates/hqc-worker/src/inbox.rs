//! `.hqc/inbox` (SPEC §5.3 `session.input`, §8.1, §12): the artifacts of an input are
//! downloaded from the hub with the worker token into
//! `<workspace>/<cwd>/.hqc/inbox/<event_id>/<name>` before the input goes to the runtime;
//! their paths are appended to the text as a block "Вложения:", and `.hqc/` is added to the
//! workspace's `.git/info/exclude`.
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
pub const BLOCK_HEADING: &str = "Вложения:";

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
        }
    }

    /// Streams the artifact into a new temporary file in `dir`; returns it with the name,
    /// kind and type the hub gave.
    async fn download(&self, id: Uuid, dir: &Path) -> Result<Downloaded, InboxError> {
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
        let tmp = dir.join(format!(".part-{}", Uuid::new_v4()));
        let mut f = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .await
            .with_context(|| format!("create {}", tmp.display()))?;
        let mut size = 0u64;
        let r: anyhow::Result<()> = async {
            while let Some(chunk) = resp.chunk().await? {
                size += chunk.len() as u64;
                f.write_all(&chunk).await?;
            }
            f.flush().await?;
            Ok(())
        }
        .await;
        if let Err(e) = r {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e.context(format!("download artifact {id}")).into());
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

/// The input as the runtime gets it: the text with the block of paths, and the files.
/// Without artifacts the input is unchanged and nothing is touched on disk.
pub async fn prepare(
    client: &ArtifactClient,
    spec: &StartSpec,
    input: &SessionInput,
) -> Result<(SessionInput, Vec<InboxFile>), InboxError> {
    let mut ids: Vec<Uuid> = Vec::new();
    for id in &input.artifact_ids {
        if !ids.contains(id) {
            ids.push(*id);
        }
    }
    if ids.is_empty() {
        return Ok((input.clone(), vec![]));
    }
    let dir = inbox_dir(spec, input.event_id)?;
    if let Err(e) = exclude_hqc(&spec.workspace) {
        tracing::warn!(workspace = %spec.workspace.display(), "cannot add .hqc/ to .git/info/exclude: {e:#}");
    }
    match fetch_all(client, &dir, input.event_id, ids).await {
        Ok(files) => {
            let mut out = input.clone();
            out.text = with_block(&input.text, &files);
            Ok((out, files))
        }
        Err(e) => {
            // The hub sends the input again (retryable) or it has failed: start clean.
            let _ = std::fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

async fn fetch_all(
    client: &ArtifactClient,
    dir: &Path,
    event_id: Uuid,
    ids: Vec<Uuid>,
) -> Result<Vec<InboxFile>, InboxError> {
    let mut files = Vec::new();
    let mut taken: Vec<String> = Vec::new();
    for id in ids {
        let d = client.download(id, dir).await?;
        let name = unique(&sanitize_name(&d.name), &taken);
        taken.push(name.clone());
        let path = dir.join(&name);
        // rename replaces whatever is at `path` (a symlink included) without following it.
        std::fs::rename(&d.tmp, &path)
            .with_context(|| format!("move into {}", path.display()))
            .map_err(InboxError::Other)?;
        let rel = format!("{HQC_DIR}/inbox/{event_id}/{name}");
        files.push(InboxFile {
            artifact_id: id,
            path,
            rel,
            kind: d.kind,
            mime: d.mime,
            size: d.size,
        });
    }
    Ok(files)
}

/// `<workdir>/.hqc/inbox/<event_id>`, created one level at a time: the working directory
/// must still resolve inside the workspace, and none of the three levels may be a symlink,
/// so nothing is created outside the workspace.
fn inbox_dir(spec: &StartSpec, event_id: Uuid) -> Result<PathBuf, InboxError> {
    spec.recheck_workdir()?;
    let mut dir = spec
        .workdir
        .canonicalize()
        .with_context(|| format!("resolve {}", spec.workdir.display()))?;
    for part in [HQC_DIR, "inbox", &event_id.to_string()] {
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

/// The text followed by "Вложения:" and one path per line.
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
pub fn exclude_hqc(workspace: &Path) -> anyhow::Result<()> {
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
    let info = git_dir.join("info");
    if !git_dir.is_dir() {
        bail!("{}: not a directory", git_dir.display());
    }
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
            "look\n\nВложения:\n- .hqc/inbox/e/a.png\n- .hqc/inbox/e/b.txt"
        );
        assert_eq!(with_block("  ", &[f("x")]), "Вложения:\n- x");
    }

    #[test]
    fn exclude() {
        let d = tempfile::tempdir().unwrap();
        // Not a git checkout: nothing is created.
        exclude_hqc(d.path()).unwrap();
        assert!(!d.path().join(".git").exists());

        std::fs::create_dir_all(d.path().join(".git")).unwrap();
        exclude_hqc(d.path()).unwrap();
        exclude_hqc(d.path()).unwrap();
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
        exclude_hqc(&wt).unwrap();
        let e = std::fs::read_to_string(&common).unwrap();
        assert!(e.starts_with("target/\n") && e.ends_with(".hqc/\n"), "{e}");
    }
}
