//! `hqc_attach` on the worker (SPEC §7, §12): the daemon reads the file itself and uploads it
//! with `PUT /worker/artifacts` before the `mcp.call` goes to the hub, which gets only the
//! artifact id.
//!
//! The hub is not trusted with the file system (SPEC §2.1), and neither is the agent's path:
//! - `path` is absolute or relative to the session's working directory; one that lies
//!   outside the workspace as written is refused before the file system is asked, and no
//!   refusal names a resolved path, so the tool tells nothing about files outside;
//! - its canonical form (symlinks resolved) must lie inside the session's workspace and inside
//!   `allowed_roots`;
//! - only a regular file, at most [`crate::inbox::MAX_ARTIFACT_BYTES`]; the file is opened
//!   without following a symlink in its last component and checked again on the open handle,
//!   so a file swapped after the check is refused, and exactly the size seen there is sent;
//! - `content` (for the bare model, iteration 4) is not accepted.
//!
//! The same rules hold for the `files` of `hqc_send` and `hqc_ask` (T41,
//! [`Attacher::prepare_files`]), and the files of a reply to `hqc_ask` are downloaded into the
//! session's own inbox before the agent gets the result ([`Attacher::reply_files`]).

use std::path::{Path, PathBuf};

use hqc_proto::event::ArtifactKind;
use hqc_proto::frame::McpError;
use hqc_proto::mcp::{error_code, AttachCall, ReplyArtifact, MAX_FILES};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::inbox::ArtifactClient;

/// What the daemon needs for `hqc_attach`: the worker's own roots and its artifact client.
#[derive(Clone)]
pub struct Attacher {
    pub allowed_roots: Vec<PathBuf>,
    pub client: ArtifactClient,
}

/// The arguments as the agent gives them (the `inputSchema` of [`hqc_proto::mcp::ATTACH`]).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    path: String,
    name: Option<String>,
    kind: Option<ArtifactKind>,
    caption: Option<String>,
}

fn invalid(m: impl Into<String>) -> McpError {
    McpError::new(error_code::INVALID_ARGS, m)
}

/// The session's folders, canonical (from its `session.start`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folders {
    pub workspace: PathBuf,
    pub workdir: PathBuf,
}

impl Attacher {
    /// Reads and uploads the file of `args` for `session_id`; the arguments of the
    /// `mcp.call` that follows.
    pub async fn prepare(
        &self,
        session_id: Uuid,
        folders: &Folders,
        args: Value,
    ) -> Result<Value, McpError> {
        let a: Args = serde_json::from_value(if args.is_null() {
            Value::Object(Default::default())
        } else {
            args
        })
        .map_err(|e| invalid(e.to_string()))?;
        if a.kind == Some(ArtifactKind::Unknown) {
            return Err(invalid("kind: one of file, image, diff, html, text"));
        }
        let artifact_id = self
            .upload_path(session_id, folders, &a.path, a.name, a.kind)
            .await?;
        Ok(serde_json::to_value(AttachCall {
            artifact_id,
            caption: a.caption,
        })
        .expect("AttachCall serializes"))
    }

    /// Resolves, reads and uploads the file `path` names for `session_id`; its artifact id.
    async fn upload_path(
        &self,
        session_id: Uuid,
        folders: &Folders,
        path: &str,
        name: Option<String>,
        kind: Option<ArtifactKind>,
    ) -> Result<Uuid, McpError> {
        let real = resolve(&self.allowed_roots, folders, path)?;
        let name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
            Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let (file, size) = open_regular(&real, self.client.max_artifact())?;
        let artifact_id = self
            .client
            .upload(session_id, file, size, &name, kind)
            .await
            .inspect_err(|e| {
                tracing::warn!(%session_id, path = %real.display(), code = %e.code, "upload: {}", e.message);
            })?;
        tracing::info!(%session_id, path = %real.display(), %artifact_id, "file uploaded for an hqc tool");
        Ok(artifact_id)
    }

    /// `files` of `hqc_send` / `hqc_ask` (T41): each entry an artifact id (a uuid, kept as it
    /// is: the hub checks it is the owner's and this session's) or a path, read and uploaded
    /// by the rules of `hqc_attach`; the arguments of the `mcp.call` carry `artifact_ids`
    /// instead. The first refusal fails the call, and nothing is sent.
    pub async fn prepare_files(
        &self,
        session_id: Uuid,
        folders: &Folders,
        args: Value,
    ) -> Result<Value, McpError> {
        let Value::Object(mut args) = args else {
            return Err(invalid("arguments must be an object"));
        };
        let files = match args.remove("files") {
            None | Some(Value::Null) => vec![],
            Some(Value::Array(v)) => v,
            Some(_) => return Err(invalid("files: a list of paths or artifact ids")),
        };
        if files.len() > MAX_FILES {
            return Err(invalid(format!("files: at most {MAX_FILES}")));
        }
        let mut ids: Vec<Uuid> = Vec::new();
        for f in files {
            let Value::String(f) = f else {
                return Err(invalid("files: a list of paths or artifact ids"));
            };
            let id = match f.trim().parse::<Uuid>() {
                Ok(id) => id,
                Err(_) => self
                    .upload_path(session_id, folders, &f, None, None)
                    .await
                    .map_err(|e| McpError::new(&e.code, format!("files: {}", e.message)))?,
            };
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if !ids.is_empty() {
            args.insert("artifact_ids".into(), serde_json::json!(ids));
        }
        Ok(Value::Object(args))
    }

    /// The result of `hqc_ask` with its `files` (artifacts of the reply, T41) downloaded into
    /// the session's inbox (`.hqc/inbox/<request_id>/`) and given as local paths; a file that
    /// could not be downloaded keeps its name with an `error`, the reply text comes back
    /// anyway. A result without files is returned as it is.
    pub async fn reply_files(
        &self,
        folders: &Folders,
        request_id: Uuid,
        mut result: Value,
    ) -> Value {
        let files: Vec<ReplyArtifact> = match result.get("files") {
            Some(v) => match serde_json::from_value(v.clone()) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(%request_id, "the files of a reply are not readable: {e}");
                    vec![]
                }
            },
            None => return result,
        };
        let got = crate::inbox::fetch_reply(
            &self.client,
            &folders.workspace,
            &folders.workdir,
            &self.allowed_roots,
            request_id,
            &files,
        )
        .await;
        result["files"] = serde_json::to_value(got).expect("ReplyFile serializes");
        result
    }
}

/// The canonical file `path` names, inside the workspace and `allowed_roots`.
pub fn resolve(roots: &[PathBuf], f: &Folders, path: &str) -> Result<PathBuf, McpError> {
    if path.is_empty() || path.contains('\0') {
        return Err(invalid("path is empty"));
    }
    let p = Path::new(path);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        f.workdir.join(p)
    };
    let outside = || {
        invalid(format!(
            "{path} is outside the session's workspace {}",
            f.workspace.display()
        ))
    };
    if !lexical(&joined).starts_with(&f.workspace) {
        return Err(outside());
    }
    let real = joined.canonicalize().map_err(|e| {
        let code = if e.kind() == std::io::ErrorKind::NotFound {
            error_code::NOT_FOUND
        } else {
            error_code::INVALID_ARGS
        };
        McpError::new(code, format!("{path}: {e}"))
    })?;
    if !real.starts_with(&f.workspace) {
        return Err(outside());
    }
    if !roots.iter().any(|r| real.starts_with(r)) {
        return Err(invalid(format!(
            "{path} is outside the worker's allowed_roots"
        )));
    }
    Ok(real)
}

/// `p` with `.` and `..` resolved as written, without asking the file system.
fn lexical(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The file opened for reading, with its size: a regular file of at most `max` bytes.
pub fn open_regular(real: &Path, max: u64) -> Result<(std::fs::File, u64), McpError> {
    use std::os::unix::fs::OpenOptionsExt;
    // No symlink in the last component (it was resolved a moment ago: one now is a swap);
    // non-blocking so that a FIFO put there cannot hang the open.
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(real)
        .map_err(|e| invalid(format!("{}: {e}", real.display())))?;
    let m = f
        .metadata()
        .map_err(|e| invalid(format!("{}: {e}", real.display())))?;
    if !m.is_file() {
        return Err(invalid(format!("{} is not a regular file", real.display())));
    }
    if m.len() > max {
        return Err(invalid(format!(
            "{} is {} bytes, more than {max}",
            real.display(),
            m.len()
        )));
    }
    Ok((f, m.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Vec<PathBuf>, Folders) {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap().join("root");
        let ws = root.join("ws");
        std::fs::create_dir_all(ws.join("sub")).unwrap();
        std::fs::write(ws.join("a.png"), b"png").unwrap();
        std::fs::write(ws.join("sub/b.txt"), b"bbb").unwrap();
        std::fs::write(root.join("outside.txt"), b"x").unwrap();
        std::fs::write(t.path().join("beyond.txt"), b"x").unwrap();
        let f = Folders {
            workspace: ws.clone(),
            workdir: ws.join("sub"),
        };
        (t, vec![root], f)
    }

    fn code(r: Result<PathBuf, McpError>) -> String {
        r.unwrap_err().code
    }

    #[test]
    fn paths_inside_the_workspace_only() {
        let (t, roots, f) = setup();
        let ws = f.workspace.clone();
        // Relative to the working directory, or absolute.
        assert_eq!(resolve(&roots, &f, "b.txt").unwrap(), ws.join("sub/b.txt"));
        assert_eq!(resolve(&roots, &f, "../a.png").unwrap(), ws.join("a.png"));
        let abs = ws.join("a.png").display().to_string();
        assert_eq!(resolve(&roots, &f, &abs).unwrap(), ws.join("a.png"));

        // Out of the workspace (still in the roots), out of the roots, absent, empty.
        assert_eq!(
            code(resolve(&roots, &f, "../../outside.txt")),
            "invalid_args"
        );
        let beyond = t.path().join("beyond.txt").display().to_string();
        assert_eq!(code(resolve(&roots, &f, &beyond)), "invalid_args");
        assert_eq!(code(resolve(&roots, &f, "/etc/hosts")), "invalid_args");
        assert_eq!(code(resolve(&roots, &f, "nope.txt")), "not_found");
        assert_eq!(code(resolve(&roots, &f, "")), "invalid_args");
        // Outside as written: the same refusal whether the file exists or not (T36).
        for p in ["/etc/hosts", "/etc/no-such-file", "../../../../nope"] {
            let e = resolve(&roots, &f, p).unwrap_err();
            assert_eq!(e.code, "invalid_args", "{p}");
            assert!(
                e.message.contains("outside the session's workspace"),
                "{p}: {}",
                e.message
            );
        }

        // A workspace outside allowed_roots (the hub said so; the worker does not trust it).
        let other = Folders {
            workspace: t.path().canonicalize().unwrap(),
            workdir: t.path().canonicalize().unwrap(),
        };
        assert_eq!(code(resolve(&roots, &other, "beyond.txt")), "invalid_args");
    }

    #[test]
    fn symlinks_are_resolved() {
        let (t, roots, f) = setup();
        let ws = &f.workspace;
        std::os::unix::fs::symlink(t.path().join("beyond.txt"), ws.join("out.txt")).unwrap();
        std::os::unix::fs::symlink("/etc", ws.join("etc")).unwrap();
        std::os::unix::fs::symlink(ws.join("a.png"), ws.join("in.png")).unwrap();
        assert_eq!(code(resolve(&roots, &f, "../out.txt")), "invalid_args");
        assert_eq!(code(resolve(&roots, &f, "../etc/hosts")), "invalid_args");
        // The target outside is not named.
        let e = resolve(&roots, &f, "../out.txt").unwrap_err();
        assert!(!e.message.contains("beyond"), "{}", e.message);
        // A link that stays inside is the file it points to.
        assert_eq!(resolve(&roots, &f, "../in.png").unwrap(), ws.join("a.png"));
        // A link put in place after the check is not followed.
        let swapped = ws.join("swapped.txt");
        std::os::unix::fs::symlink(t.path().join("beyond.txt"), &swapped).unwrap();
        assert!(open_regular(&swapped, 100).is_err());
    }

    #[test]
    fn regular_files_under_the_cap() {
        let (_t, _roots, f) = setup();
        let ws = &f.workspace;
        let (_, n) = open_regular(&ws.join("a.png"), 3).unwrap();
        assert_eq!(n, 3);
        let e = open_regular(&ws.join("a.png"), 2).unwrap_err();
        assert_eq!(e.code, "invalid_args");
        assert!(e.message.contains("more than 2"), "{}", e.message);
        let e = open_regular(&ws.join("sub"), 100).unwrap_err();
        assert!(e.message.contains("not a regular file"), "{}", e.message);
        let fifo = ws.join("pipe");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let e = open_regular(&fifo, 100).unwrap_err();
        assert!(e.message.contains("not a regular file"), "{}", e.message);
    }
}
