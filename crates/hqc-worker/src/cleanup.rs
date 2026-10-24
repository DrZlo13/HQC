//! Removing what the worker wrote for sessions (T21, SPEC §5.3, §8.4, §12, §17.4).
//!
//! - `session.cleanup` from the hub (archived or deleted session): the inbox folders
//!   `<workspace>/<cwd>/.hqc/inbox/<event_id>` of the listed inputs, or, for a session without
//!   workspace, its scratch folder `<scratch_root>/<session_id>` ([`session`]).
//! - At start: inbox folders older than [`INBOX_MAX_AGE`] in the working directories the worker
//!   knows — its known workspaces and the folders it wrote inboxes into ([`sweep`]).
//!
//! The hub is not trusted with paths (SPEC §2.1): the workspace must resolve inside
//! `allowed_roots`, the working directory inside the workspace, and `.hqc` and `inbox` must be
//! real folders, not symlinks. Only direct children of `.hqc/inbox` named by an event uuid, and
//! only `<scratch_root>/<uuid>`, are ever removed; a symlink there is removed as a link, never
//! followed.

use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context};
use hqc_proto::frame::SessionCleanup;
use uuid::Uuid;

use crate::config::Config;
use crate::inbox::HQC_DIR;

/// Inbox folders older than this go at the worker's start.
pub const INBOX_MAX_AGE: Duration = Duration::from_secs(14 * 86_400);

/// `session.cleanup`: returns how many folders went.
pub fn session(cfg: &Config, c: &SessionCleanup) -> anyhow::Result<usize> {
    match &c.workspace_path {
        Some(ws) => {
            let Some(inbox) = inbox_of(cfg, ws, &c.cwd)? else {
                return Ok(0);
            };
            let mut n = 0;
            for id in &c.event_ids {
                if remove_entry(&inbox.join(id.to_string()))? {
                    n += 1;
                }
            }
            Ok(n)
        }
        None => {
            let root = match cfg.scratch_root.canonicalize() {
                Ok(r) => r,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
                Err(e) => return Err(e).context("scratch root"),
            };
            Ok(usize::from(remove_entry(
                &root.join(c.session_id.to_string()),
            )?))
        }
    }
}

/// `<workspace>/<cwd>/.hqc/inbox`, resolved and checked; `None` if it does not exist.
fn inbox_of(cfg: &Config, ws: &str, cwd: &str) -> anyhow::Result<Option<PathBuf>> {
    let ws = Path::new(ws);
    if !ws.is_absolute() {
        bail!("workspace_path {} is not absolute", ws.display());
    }
    let ws = match ws.canonicalize() {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("workspace {}", ws.display())),
    };
    if !cfg.inside_roots(&ws) {
        bail!("workspace {} is outside allowed_roots", ws.display());
    }
    let rel = Path::new(cwd);
    if !rel
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        bail!("cwd {cwd:?} must be relative to the workspace, without `..`");
    }
    let workdir = match ws.join(rel).canonicalize() {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("cwd {cwd:?}")),
    };
    if !workdir.starts_with(&ws) {
        bail!("cwd {cwd:?} resolves outside the workspace");
    }
    inbox_in(&workdir)
}

/// `<workdir>/.hqc/inbox` if both levels are real folders; `None` if missing.
fn inbox_in(workdir: &Path) -> anyhow::Result<Option<PathBuf>> {
    let mut dir = workdir.to_path_buf();
    for part in [HQC_DIR, "inbox"] {
        dir.push(part);
        match std::fs::symlink_metadata(&dir) {
            Ok(m) if m.file_type().is_symlink() => {
                bail!("{} is a symlink: not cleaned", dir.display())
            }
            Ok(m) if m.is_dir() => {}
            Ok(_) => bail!("{} is not a directory", dir.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("{}", dir.display())),
        }
    }
    Ok(Some(dir))
}

/// Removes `path` (a folder with its content, or a symlink as a link); `false` if absent.
fn remove_entry(path: &Path) -> anyhow::Result<bool> {
    let m = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).with_context(|| format!("{}", path.display())),
    };
    if m.is_dir() {
        // Does not follow symlinks inside.
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
    .with_context(|| format!("remove {}", path.display()))?;
    tracing::info!(path = %path.display(), "removed");
    Ok(true)
}

/// At start: inbox folders older than `max_age` (by modification time) under each of
/// `workdirs` that resolves inside `allowed_roots` and inside one of `workspaces`. Returns
/// how many went; problems are logged and skipped.
pub fn sweep(
    cfg: &Config,
    workspaces: &[PathBuf],
    workdirs: &[PathBuf],
    max_age: Duration,
    now: SystemTime,
) -> usize {
    let mut n = 0;
    let mut seen = std::collections::BTreeSet::new();
    for d in workspaces.iter().chain(workdirs) {
        let Ok(real) = d.canonicalize() else {
            continue;
        };
        let known = workspaces
            .iter()
            .filter_map(|w| w.canonicalize().ok())
            .any(|w| real.starts_with(&w));
        if !known || !cfg.inside_roots(&real) || !seen.insert(real.clone()) {
            continue;
        }
        let inbox = match inbox_in(&real) {
            Ok(Some(i)) => i,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!("inbox sweep: {e:#}");
                continue;
            }
        };
        let Ok(entries) = std::fs::read_dir(&inbox) else {
            continue;
        };
        for e in entries.flatten() {
            let is_event = e
                .file_name()
                .to_str()
                .is_some_and(|s| s.parse::<Uuid>().is_ok());
            let old = std::fs::symlink_metadata(e.path())
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age > max_age);
            if !is_event || !old {
                continue;
            }
            match remove_entry(&e.path()) {
                Ok(true) => n += 1,
                Ok(false) => {}
                Err(err) => tracing::warn!("inbox sweep: {err:#}"),
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(root: &Path) -> Config {
        let mut c = Config::parse(&format!(
            "hub_url = \"http://127.0.0.1:1\"\nallowed_roots = [\"{}\"]\n",
            root.display()
        ))
        .unwrap();
        c.scratch_root = root.join("home/scratch");
        c
    }

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }

    fn cleanup(ws: &Path, cwd: &str, ids: &[Uuid]) -> SessionCleanup {
        SessionCleanup {
            session_id: Uuid::new_v4(),
            workspace_path: Some(ws.display().to_string()),
            cwd: cwd.into(),
            event_ids: ids.to_vec(),
        }
    }

    #[test]
    fn removes_only_the_listed_inboxes() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        let cfg = config(&root);
        let ws = root.join("ws");
        let (mine, other) = (Uuid::new_v4(), Uuid::new_v4());
        let inbox = ws.join("sub/.hqc/inbox");
        touch(&inbox.join(mine.to_string()).join("a.png"));
        touch(&inbox.join(other.to_string()).join("b.png"));
        touch(&ws.join("sub/.hqc/keep.txt"));
        touch(&ws.join("sub/src/main.rs"));
        let n = session(&cfg, &cleanup(&ws, "sub", &[mine, Uuid::new_v4()])).unwrap();
        assert_eq!(n, 1);
        assert!(!inbox.join(mine.to_string()).exists());
        assert!(inbox.join(other.to_string()).join("b.png").exists());
        assert!(ws.join("sub/.hqc/keep.txt").exists());
        assert!(ws.join("sub/src/main.rs").exists());
        // Again: nothing left to do.
        assert_eq!(session(&cfg, &cleanup(&ws, "sub", &[mine])).unwrap(), 0);
        // No inbox at all: nothing, no error.
        assert_eq!(session(&cfg, &cleanup(&ws, ".", &[other])).unwrap(), 0);
    }

    #[test]
    fn refuses_paths_outside() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap().join("root");
        let outside = d.path().canonicalize().unwrap().join("outside");
        let cfg = config(&root);
        let id = Uuid::new_v4();
        let victim = outside.join(format!(".hqc/inbox/{id}/secret"));
        touch(&victim);
        touch(&root.join("ws/x"));
        // A workspace outside allowed_roots.
        assert!(session(&cfg, &cleanup(&outside, ".", &[id])).is_err());
        // `..` in cwd, an absolute cwd.
        assert!(session(&cfg, &cleanup(&root.join("ws"), "../../outside", &[id])).is_err());
        assert!(session(
            &cfg,
            &cleanup(&root.join("ws"), &outside.display().to_string(), &[id])
        )
        .is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            // A cwd symlinked out of the workspace.
            symlink(&outside, root.join("ws/out")).unwrap();
            assert!(session(&cfg, &cleanup(&root.join("ws"), "out", &[id])).is_err());
            // A symlinked `.hqc` is not followed.
            symlink(outside.join(".hqc"), root.join("ws/.hqc")).unwrap();
            assert!(session(&cfg, &cleanup(&root.join("ws"), ".", &[id])).is_err());
            std::fs::remove_file(root.join("ws/.hqc")).unwrap();
            // A symlinked event folder goes as a link; its target stays.
            let inbox = root.join("ws/.hqc/inbox");
            std::fs::create_dir_all(&inbox).unwrap();
            symlink(
                outside.join(format!(".hqc/inbox/{id}")),
                inbox.join(id.to_string()),
            )
            .unwrap();
            assert_eq!(
                session(&cfg, &cleanup(&root.join("ws"), ".", &[id])).unwrap(),
                1
            );
            assert!(!inbox.join(id.to_string()).exists());
        }
        assert!(victim.exists(), "nothing outside the workspace is removed");
    }

    #[test]
    fn scratch_folder_of_a_session_without_workspace() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        let cfg = config(&root);
        let (sid, other) = (Uuid::new_v4(), Uuid::new_v4());
        touch(&cfg.scratch_root.join(sid.to_string()).join("notes.md"));
        touch(&cfg.scratch_root.join(other.to_string()).join("notes.md"));
        let c = SessionCleanup {
            session_id: sid,
            workspace_path: None,
            cwd: ".".into(),
            event_ids: vec![],
        };
        assert_eq!(session(&cfg, &c).unwrap(), 1);
        assert!(!cfg.scratch_root.join(sid.to_string()).exists());
        assert!(cfg.scratch_root.join(other.to_string()).exists());
        assert!(cfg.scratch_root.exists());
        assert_eq!(session(&cfg, &c).unwrap(), 0);
    }

    #[test]
    fn sweep_removes_only_old_event_folders_in_known_places() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap().join("root");
        let cfg = config(&root);
        let ws = root.join("ws");
        let (old, fresh) = (Uuid::new_v4(), Uuid::new_v4());
        let inbox = ws.join(".hqc/inbox");
        let sub_inbox = ws.join("sub/.hqc/inbox");
        touch(&inbox.join(old.to_string()).join("a"));
        touch(&inbox.join(fresh.to_string()).join("a"));
        touch(&inbox.join("not-an-event/a"));
        touch(&sub_inbox.join(old.to_string()).join("a"));
        // An inbox of a folder that is not a known workspace.
        let stranger = root.join("other/.hqc/inbox").join(old.to_string());
        touch(&stranger.join("a"));
        // An inbox outside allowed_roots, even if listed.
        let outside = d.path().canonicalize().unwrap().join("outside");
        touch(&outside.join(".hqc/inbox").join(old.to_string()).join("a"));
        // `fresh` was written just now; pretend the others are 15 days old by moving `now`.
        let later = SystemTime::now() + Duration::from_secs(15 * 86_400);
        std::fs::File::open(inbox.join(fresh.to_string()))
            .unwrap()
            .set_modified(later)
            .unwrap();
        let n = sweep(
            &cfg,
            &[ws.clone(), outside.clone()],
            &[ws.join("sub"), root.join("other")],
            INBOX_MAX_AGE,
            later,
        );
        assert_eq!(n, 2);
        assert!(!inbox.join(old.to_string()).exists());
        assert!(!sub_inbox.join(old.to_string()).exists());
        assert!(inbox.join(fresh.to_string()).join("a").exists());
        assert!(inbox.join("not-an-event/a").exists());
        assert!(stranger.join("a").exists());
        assert!(outside.join(".hqc/inbox").join(old.to_string()).exists());
        // Nothing is old yet by the real clock.
        touch(&inbox.join(Uuid::new_v4().to_string()).join("a"));
        assert_eq!(sweep(&cfg, &[ws], &[], INBOX_MAX_AGE, SystemTime::now()), 0);
    }
}
