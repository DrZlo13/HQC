//! A session's transcript follows the session into its new folder (SPEC §8.1, §9.2).
//!
//! Claude Code keeps a transcript under the encoded working directory
//! (`<config>/projects/<encoded cwd>/<uuid>.jsonl`, its subagents in `<uuid>/` next to it), and
//! the adapter resumes only a transcript at the place its working directory gives. A session
//! that moved (`session.start` with another folder) finds its transcript there missing:
//! [`relocate`] looks for it by uuid in every project folder, not by memory of the old folder,
//! so a move survives a worker restart between the hub's start and the agent's next one.
//!
//! The worker does not trust the hub with paths (SPEC §2.1): it moves exactly one transcript
//! of that uuid, only one whose folder (the mirror row's `cwd`, which a move sets, else the
//! first recorded `cwd`; symlinks resolved, through its deepest existing ancestor when the
//! folder is gone) lies inside `allowed_roots`, and never over a file or folder at the new
//! place. Nor does it move one that a live Claude Code process outside the worker holds open
//! (Claude Code's registry of live sessions, read at that moment, [`Registry`]; SPEC §9.3): that
//! process would go on appending to the moved file while the agent appends too. The hub's
//! `held_by` may lag, so the worker does not rely on it. Otherwise the start fails and the
//! transcript stays where it was. A transcript of that folder itself is left alone: it was not
//! moved.
//!
//! The mirror's row for the new path is written first (`cwd` the new folder, `offset` the
//! file's size, `last_prompt` and `title` of the old row): the mirror reads the row before the
//! file's records, so it neither imports the file again nor counts it to the old folder. When
//! the file does not get there (its folder or the link fails), that row is removed again.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use uuid::Uuid;

use super::holders::{self, Processes};
use super::registry::OwnRecords;
use super::store::{FileRow, Store};
use crate::adapter::claude_code::paths::encode_project_dir;

/// Claude Code's registry of live sessions (`<config>/sessions`), read before a transcript
/// moves; the worker's own agent processes are left out, as the mirror does.
pub struct Registry<'a> {
    pub dir: &'a Path,
    pub own: &'a OwnRecords,
    pub procs: &'a dyn Processes,
}

/// Moves transcript `uuid` (and its `<uuid>/` folder) under `projects` to the place of the
/// canonical `workdir`; the new path, or `None` when it is there already or nowhere.
/// `registry`: `None` when Claude Code's registry is not looked at.
pub fn relocate(
    projects: &Path,
    store: &Store,
    roots: &[PathBuf],
    uuid: Uuid,
    workdir: &Path,
    registry: Option<Registry<'_>>,
) -> anyhow::Result<Option<PathBuf>> {
    let to = projects
        .join(encode_project_dir(&workdir.to_string_lossy()))
        .join(format!("{uuid}.jsonl"));
    if to.exists() {
        return Ok(None);
    }
    let found: Vec<PathBuf> = super::transcripts(projects)
        .into_iter()
        .filter(|(_, r)| *r == uuid)
        .map(|(p, _)| p)
        .collect();
    let from = match found.as_slice() {
        [] => return Ok(None),
        [one] => one.clone(),
        many => bail!(
            "{} transcripts of {uuid} under {}; none is moved",
            many.len(),
            projects.display()
        ),
    };
    // The folder a move took it to (the mirror's row), else the one Claude Code wrote it in.
    let old = store.get(&from)?.unwrap_or_default();
    let cwd = old
        .cwd
        .clone()
        .or_else(|| super::read_cwd(&from))
        .with_context(|| format!("transcript {} records no cwd", from.display()))?;
    let real = resolve(Path::new(&cwd))
        .with_context(|| format!("the folder {cwd} of transcript {}", from.display()))?;
    if !roots.iter().any(|r| real.starts_with(r)) {
        bail!(
            "transcript {} was written in {}, outside allowed_roots; it is not moved",
            from.display(),
            real.display()
        );
    }
    if real == workdir {
        // Not moved: Claude Code keeps it where it chose (a path through a symlink).
        return Ok(None);
    }
    let (dir_from, dir_to) = (
        from.with_file_name(uuid.to_string()),
        to.with_file_name(uuid.to_string()),
    );
    if dir_to.exists() {
        bail!(
            "{} exists already; transcript {uuid} is not moved",
            dir_to.display()
        );
    }
    if let Some(r) = registry {
        let held = holders::read(r.dir, &r.own.pids(), |s| r.own.is_live(s), r.procs);
        if let Some(h) = held.get(&uuid) {
            bail!(
                "transcript {uuid} is open in a Claude Code process outside hqc ({}, pid {}); it is not moved",
                h.entrypoint,
                h.pid
            );
        }
    }
    let size = std::fs::metadata(&from)
        .with_context(|| format!("transcript {}", from.display()))?
        .len();
    store.put(
        &to,
        &FileRow {
            cwd: Some(workdir.to_string_lossy().into_owned()),
            offset: size,
            last_prompt: old.last_prompt,
            title: old.title,
        },
    )?;
    let place = || -> anyhow::Result<()> {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        // A link fails where a file is already: the move never replaces one.
        std::fs::hard_link(&from, &to)
            .with_context(|| format!("moving {} to {}", from.display(), to.display()))
    };
    if let Err(e) = place() {
        if let Err(r) = store.remove(&to) {
            tracing::warn!(path = %to.display(), "mirror state: {r:#}");
        }
        return Err(e);
    }
    std::fs::remove_file(&from).with_context(|| format!("removing {}", from.display()))?;
    if dir_from.is_dir() {
        std::fs::rename(&dir_from, &dir_to)
            .with_context(|| format!("moving {} to {}", dir_from.display(), dir_to.display()))?;
    }
    tracing::info!(runtime_session_id = %uuid, from = %from.display(), to = %to.display(), "transcript moved to the session's new folder");
    Ok(Some(to))
}

/// `p` with symlinks resolved; for a folder that is gone (a worktree deleted after the move),
/// its deepest existing ancestor resolved and the rest appended (plain components only: `..`
/// has no file name).
fn resolve(p: &Path) -> anyhow::Result<PathBuf> {
    let mut rest = Vec::new();
    let mut cur = p;
    loop {
        if let Ok(c) = cur.canonicalize() {
            return Ok(rest.iter().rev().fold(c, |acc, n| acc.join(n)));
        }
        rest.push(cur.file_name().context("no existing ancestor")?.to_owned());
        cur = cur.parent().context("no existing ancestor")?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Processes 100 and 200 are alive, each its own group.
    struct Fake;

    impl Processes for Fake {
        fn alive(&self, pid: u32) -> bool {
            [100, 200].contains(&pid)
        }
        fn group(&self, pid: u32) -> Option<u32> {
            self.alive(pid).then_some(pid)
        }
    }

    /// `root/a` and `root/b` inside `allowed_roots`, transcript `uuid` written in `a`.
    struct Setup {
        _dir: tempfile::TempDir,
        root: PathBuf,
        projects: PathBuf,
        sessions: PathBuf,
        store: Store,
        uuid: Uuid,
        from: PathBuf,
        b: PathBuf,
    }

    fn setup() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (a, b) = (root.join("a"), root.join("b"));
        for d in [&a, &b] {
            std::fs::create_dir_all(d).unwrap();
        }
        let (projects, sessions) = (root.join("projects"), root.join("sessions"));
        std::fs::create_dir_all(&sessions).unwrap();
        let uuid = Uuid::new_v4();
        let from = projects
            .join(encode_project_dir(&a.to_string_lossy()))
            .join(format!("{uuid}.jsonl"));
        std::fs::create_dir_all(from.parent().unwrap()).unwrap();
        std::fs::write(&from, "{}\n").unwrap();
        let store = Store::memory();
        let row = FileRow {
            cwd: Some(a.to_string_lossy().into_owned()),
            ..FileRow::default()
        };
        store.put(&from, &row).unwrap();
        Setup {
            _dir: dir,
            root,
            projects,
            sessions,
            store,
            uuid,
            from,
            b,
        }
    }

    impl Setup {
        fn to(&self) -> PathBuf {
            self.projects
                .join(encode_project_dir(&self.b.to_string_lossy()))
                .join(format!("{}.jsonl", self.uuid))
        }

        /// A registry entry of process `pid` for the transcript.
        fn hold(&self, pid: u32) {
            let e = serde_json::json!({"pid": pid, "sessionId": self.uuid, "cwd": "/x",
                "entrypoint": "claude-vscode", "status": "idle"});
            std::fs::write(self.sessions.join(format!("{pid}.json")), e.to_string()).unwrap();
        }

        fn relocate(&self, own: &OwnRecords) -> anyhow::Result<Option<PathBuf>> {
            let registry = Registry {
                dir: &self.sessions,
                own,
                procs: &Fake,
            };
            relocate(
                &self.projects,
                &self.store,
                std::slice::from_ref(&self.root),
                self.uuid,
                &self.b,
                Some(registry),
            )
        }
    }

    /// A live process outside the worker that holds the transcript keeps it where it is; the
    /// worker's own agent process does not; a dead one's entry does not.
    #[test]
    fn a_held_transcript_stays() {
        let s = setup();
        s.hold(100);
        let e = s.relocate(&OwnRecords::default()).unwrap_err();
        let msg = format!("{e:#}");
        assert!(
            msg.contains("claude-vscode") && msg.contains("pid 100"),
            "{msg}"
        );
        assert!(s.from.is_file() && !s.to().exists());
        assert_eq!(s.store.get(&s.to()).unwrap(), None);

        let own = OwnRecords::default();
        own.begin(s.uuid, Some(100));
        s.hold(300);
        assert_eq!(s.relocate(&own).unwrap(), Some(s.to()));
        assert!(s.to().is_file() && !s.from.exists());
    }

    /// The new path's folder cannot be made (a file is there): the row written for the new
    /// path is removed again and the transcript stays.
    #[test]
    fn a_failed_move_leaves_no_row() {
        let s = setup();
        std::fs::write(s.to().parent().unwrap(), "").unwrap();
        let e = s.relocate(&OwnRecords::default()).unwrap_err();
        assert!(format!("{e:#}").contains("creating"), "{e:#}");
        assert_eq!(s.store.get(&s.to()).unwrap(), None);
        assert!(s.from.is_file());
        assert_eq!(
            s.store.get(&s.from).unwrap().unwrap().cwd,
            Some(s.root.join("a").to_string_lossy().into_owned())
        );
    }
}
