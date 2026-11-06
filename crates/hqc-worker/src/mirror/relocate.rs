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
//! place. Otherwise the start fails and the transcript stays where it was. A transcript of
//! that folder itself is left alone: it was not moved.
//!
//! The mirror's row for the new path is written first (`cwd` the new folder, `offset` the
//! file's size, `last_prompt` and `title` of the old row): the mirror reads the row before the
//! file's records, so it neither imports the file again nor counts it to the old folder.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use uuid::Uuid;

use super::store::{FileRow, Store};
use crate::adapter::claude_code::paths::encode_project_dir;

/// Moves transcript `uuid` (and its `<uuid>/` folder) under `projects` to the place of the
/// canonical `workdir`; the new path, or `None` when it is there already or nowhere.
pub fn relocate(
    projects: &Path,
    store: &Store,
    roots: &[PathBuf],
    uuid: Uuid,
    workdir: &Path,
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
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    // A link fails where a file is already: the move never replaces one.
    std::fs::hard_link(&from, &to)
        .with_context(|| format!("moving {} to {}", from.display(), to.display()))?;
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
