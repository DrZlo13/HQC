//! Folders hqc makes and deletes on this worker (T45, SPEC §3 Workspace, §5.3, §13).
//!
//! - `workspace.create {kind: clone}`: the worker checks the new path itself — its parent,
//!   with symlinks resolved, is a folder inside `allowed_roots`, the path does not exist and
//!   overlaps no workspace the worker knows — creates the folder, records it as creating in
//!   `$HQC_HOME/created_workspaces.json` and runs `git clone` into it in the environment of the
//!   path, without prompts and with `clone_timeout`. The outcome is `workspace.created`; a
//!   failed or timed-out clone leaves nothing behind. The hub repeats the request after every
//!   `hello` while it waits, and a request the worker knows is answered with its outcome, never
//!   started twice. A worker that starts and finds a clone recorded as creating removes its
//!   folder: the hub fails such a clone on the new `boot_id`.
//! - `workspace.create {kind: worktree}` (T46): the origin must be a workspace the worker knows,
//!   inside `allowed_roots` and the top folder of a git repository; the new path is checked and
//!   created as for a clone, recorded with its origin, and `git worktree add` runs in the origin
//!   in the environment of the new path. A branch checked out in another worktree is refused.
//! - `workspace.delete` removes only a folder of that list, by its canonical path: the hub
//!   cannot name another one (SPEC §2.1). A worktree goes with `git worktree remove` in its
//!   origin (`--force` only when the hub says the user confirmed the uncommitted files), then
//!   `git worktree prune`; its branch stays.
//! - `workspace.inspect` reports what deleting a folder would lose: uncommitted changes and
//!   commits that are on no remote branch.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use hqc_proto::frame::{
    check_branch, check_clone_url, CloneFrom, Unpushed, WorkerFrame, WorkspaceCreate,
    WorkspaceCreated, WorkspaceDelete, WorkspaceDeleteResult, WorkspaceInspectResult,
    WorkspaceSource, MAX_DIRTY, MAX_UNPUSHED,
};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt as _;

use crate::config::{Config, Environment};
use crate::link::HubLink;

/// The list of folders the worker created, next to `known_workspaces.json`.
pub const CREATED_FILE: &str = "created_workspaces.json";

/// How much of git's stderr is kept for the error message.
const STDERR_KEEP: usize = 64 * 1024;
/// How much of it the message shows.
const STDERR_SHOWN: usize = 400;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Entry {
    /// Canonical.
    path: PathBuf,
    /// The clone or `git worktree add` into it runs (or ran when the worker stopped).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    creating: bool,
    /// A worktree (T46): the canonical path of the repository it was added in, where `git
    /// worktree remove` runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<PathBuf>,
}

/// `$HQC_HOME/created_workspaces.json`: the folders this worker created for workspaces, the
/// only ones `workspace.delete` removes. Written atomically (temporary file, then rename;
/// 0600) on every change.
pub struct Created {
    file: PathBuf,
    list: Mutex<Vec<Entry>>,
}

impl Created {
    /// Reads the list (a missing file is an empty list, an unreadable one an error) and
    /// removes the folders of clones and worktrees a previous run did not finish.
    pub fn load(file: &Path, cfg: &Config) -> anyhow::Result<Self> {
        let list: Vec<Entry> = match std::fs::read(file) {
            Ok(b) => {
                serde_json::from_slice(&b).with_context(|| format!("reading {}", file.display()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
        };
        let partial: Vec<PathBuf> = list
            .iter()
            .filter(|e| e.creating)
            .map(|e| e.path.clone())
            .collect();
        let created = Self {
            file: file.to_path_buf(),
            list: Mutex::new(list),
        };
        for p in partial {
            if !cfg.inside_roots(&p) {
                tracing::warn!(path = %p.display(), "unfinished workspace outside allowed_roots left alone");
                created.forget(&p)?;
                continue;
            }
            tracing::warn!(path = %p.display(), "removing the folder of a workspace the worker did not finish");
            created.remove(cfg, &p)?;
        }
        Ok(created)
    }

    pub fn paths(&self) -> Vec<PathBuf> {
        self.list
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.path.clone())
            .collect()
    }

    fn get(&self, path: &Path) -> Option<Entry> {
        self.list
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.path == path)
            .cloned()
    }

    fn change(&self, f: impl FnOnce(&mut Vec<Entry>)) -> anyhow::Result<()> {
        let mut list = self.list.lock().unwrap();
        f(&mut list);
        let text = serde_json::to_vec_pretty(&*list)?;
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let tmp = self.file.with_extension("json.tmp");
        crate::known_paths::write_private(&tmp, &text)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.file)
            .with_context(|| format!("writing {}", self.file.display()))
    }

    fn begin(&self, path: &Path, origin: Option<&Path>) -> anyhow::Result<()> {
        self.change(|l| {
            l.retain(|e| e.path != path);
            l.push(Entry {
                path: path.to_path_buf(),
                creating: true,
                origin: origin.map(Path::to_path_buf),
            });
        })
    }

    fn finish(&self, path: &Path) -> anyhow::Result<()> {
        self.change(|l| {
            l.iter_mut()
                .filter(|e| e.path == path)
                .for_each(|e| e.creating = false)
        })
    }

    fn forget(&self, path: &Path) -> anyhow::Result<()> {
        self.change(|l| l.retain(|e| e.path != path))
    }

    /// Removes the folder of an unfinished clone or worktree, then the entry; for a worktree
    /// `git worktree prune` in its origin drops what git had recorded of it. A folder that
    /// cannot be removed keeps its entry, so the next start tries again.
    fn remove(&self, cfg: &Config, path: &Path) -> anyhow::Result<()> {
        match std::fs::remove_dir_all(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
        }
        if let Some(origin) = self.get(path).and_then(|e| e.origin) {
            if let Err(e) = prune(cfg, &origin) {
                tracing::warn!(origin = %origin.display(), "git worktree prune: {e}");
            }
        }
        self.forget(path)
    }
}

/// `git worktree prune` in a worktree's origin, if it is still a folder inside the roots.
fn prune(cfg: &Config, origin: &Path) -> Result<(), String> {
    match origin.canonicalize() {
        Ok(c) if c == origin && c.is_dir() && cfg.inside_roots(&c) => {
            git_output(&cfg.environment(&c), &c, &["worktree", "prune"]).map(drop)
        }
        _ => Err(format!(
            "{} is no longer a folder inside allowed_roots",
            origin.display()
        )),
    }
}

/// `workspace.*` of the hub.
pub struct Workspaces {
    cfg: Arc<Config>,
    created: Created,
    link: HubLink,
    /// `workspace.create` by `request_id`: `None` while it runs, then its outcome, which a
    /// repeated request gets again. A few hundred bytes per clone for the life of the process.
    requests: Mutex<HashMap<String, Option<WorkspaceCreated>>>,
}

impl Workspaces {
    pub fn new(cfg: Arc<Config>, created: Created, link: HubLink) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            created,
            link,
            requests: Mutex::default(),
        })
    }

    /// The folders this worker created.
    pub fn created(&self) -> Vec<PathBuf> {
        self.created.paths()
    }

    /// `workspace.create` (acked by the caller): runs it in the background and sends
    /// `workspace.created`. `known`: the workspaces the worker knows, which the new folder must
    /// not overlap.
    pub fn create(self: &Arc<Self>, c: WorkspaceCreate, known: Vec<PathBuf>) {
        {
            let mut requests = self.requests.lock().unwrap();
            match requests.get(&c.request_id) {
                Some(Some(done)) => {
                    self.link.send(WorkerFrame::WorkspaceCreated(done.clone()));
                    return;
                }
                Some(None) => return,
                None => {
                    requests.insert(c.request_id.clone(), None);
                }
            }
        }
        let this = self.clone();
        tokio::spawn(async move {
            let request_id = c.request_id.clone();
            let outcome = match this.make(&c, &known).await {
                Ok(p) => {
                    tracing::info!(%request_id, path = %p.display(), "workspace created");
                    WorkspaceCreated {
                        request_id,
                        canonical: Some(p.to_string_lossy().into_owned()),
                        error: None,
                    }
                }
                Err(e) => {
                    tracing::warn!(%request_id, path = %c.path, "workspace not created: {e}");
                    WorkspaceCreated {
                        request_id,
                        canonical: None,
                        error: Some(e),
                    }
                }
            };
            this.requests
                .lock()
                .unwrap()
                .insert(c.request_id, Some(outcome.clone()));
            this.link.send(WorkerFrame::WorkspaceCreated(outcome));
        });
    }

    /// The new folder, made: the canonical path, or the reason for the user with nothing left
    /// on disk.
    pub async fn make(&self, c: &WorkspaceCreate, known: &[PathBuf]) -> Result<PathBuf, String> {
        // Everything is checked before the folder is made.
        let run = match &c.source {
            WorkspaceSource::Clone { url, branch } => {
                let protocols = match check_clone_url(url)? {
                    CloneFrom::Remote => "https:ssh",
                    CloneFrom::File(p) => match Path::new(&p).canonicalize() {
                        Ok(c) if self.cfg.inside_roots(&c) => "file",
                        _ => return Err(format!("{url} is not a repository inside allowed_roots")),
                    },
                };
                if let Some(b) = branch {
                    check_branch(b)?;
                }
                Run::Clone {
                    url,
                    branch: branch.as_deref(),
                    protocols,
                }
            }
            WorkspaceSource::Worktree {
                origin,
                branch,
                new_branch,
            } => {
                let origin = self.origin(origin, known)?;
                let env = self.cfg.environment(&origin);
                let (o, b, new) = (origin.clone(), branch.clone(), *new_branch);
                let (options, commit) =
                    tokio::task::spawn_blocking(move || worktree_args(&env, &o, &b, new))
                        .await
                        .map_err(|e| format!("checking the branch: {e}"))??;
                Run::Worktree {
                    origin,
                    options,
                    commit,
                }
            }
        };
        let origin = match &run {
            Run::Worktree { origin, .. } => Some(origin.as_path()),
            Run::Clone { .. } => None,
        };
        let target = self.new_folder(&c.path, known, origin)?;
        // `git clone ... -- <url> <target>` with only the URL's transports, or `git worktree
        // add ... -- <target> <commit>` in the origin.
        let (dir, args, protocols) = match run {
            Run::Clone {
                url,
                branch,
                protocols,
            } => {
                let mut args: Vec<OsString> = vec!["clone".into(), "--quiet".into()];
                if let Some(b) = branch {
                    args.extend(["--branch".into(), b.into()]);
                }
                args.extend(["--".into(), url.into(), target.clone().into()]);
                (target.clone(), args, Some(protocols))
            }
            Run::Worktree {
                origin,
                options,
                commit,
            } => {
                let mut args: Vec<OsString> =
                    vec!["worktree".into(), "add".into(), "--quiet".into()];
                args.extend(options);
                args.extend(["--".into(), target.clone().into(), commit.into()]);
                (origin, args, None)
            }
        };
        let env = self.cfg.environment(&target);
        // T44: the environment's keys in an ssh-agent of this run's own, stopped with it.
        let r = match env.ssh_keys.as_slice() {
            [] => Ok(None),
            keys => self
                .cfg
                .ssh
                .start_agent(keys)
                .await
                .map(Some)
                .map_err(|e| format!("the environment's ssh keys: {e:#}")),
        };
        let r = match r {
            Ok(agent) => {
                git_run(
                    &env,
                    agent.as_ref().map(|a| a.socket()),
                    &dir,
                    &args,
                    protocols,
                    self.cfg.clone_timeout,
                )
                .await
            }
            Err(e) => Err(e),
        };
        let r = r.and_then(|()| {
            self.created
                .finish(&target)
                .map_err(|e| format!("the list of created folders: {e:#}"))
        });
        if let Err(e) = r {
            if let Err(re) = self.created.remove(&self.cfg, &target) {
                tracing::error!(path = %target.display(), "partial workspace not removed: {re:#}");
            }
            return Err(e);
        }
        Ok(target)
    }

    /// The origin of a worktree: a workspace this worker knows (from the hub's frames or its
    /// own list), still resolving to itself inside `allowed_roots`, and the top folder of a
    /// git repository. The hub is not trusted with it (SPEC §2.1).
    fn origin(&self, origin: &str, known: &[PathBuf]) -> Result<PathBuf, String> {
        let p = Path::new(origin);
        let created = self.created.get(p);
        let listed = known.iter().any(|k| k == p) || created.is_some();
        match p.canonicalize() {
            Ok(c) if listed && c == p && c.is_dir() && self.cfg.inside_roots(&c) => {}
            _ => return Err(format!("{origin} is not a workspace of this worker")),
        }
        if created.is_some_and(|e| e.creating) {
            return Err(format!("{origin} is still being created"));
        }
        let top = git_output(
            &self.cfg.environment(p),
            p,
            &["rev-parse", "--show-toplevel"],
        )
        .ok()
        .and_then(|t| Path::new(t.trim()).canonicalize().ok());
        if top.as_deref() != Some(p) {
            return Err(format!(
                "{origin} is not the top folder of a git repository"
            ));
        }
        Ok(p.to_path_buf())
    }

    /// Checks `path` and creates it, empty, as a folder of this worker's list (a worktree's
    /// with its `origin`).
    fn new_folder(
        &self,
        path: &str,
        known: &[PathBuf],
        origin: Option<&Path>,
    ) -> Result<PathBuf, String> {
        let p = Path::new(path);
        if !p.is_absolute()
            || !p
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
        {
            return Err(format!("{path} is not an absolute path without . or .."));
        }
        let (Some(parent), Some(name)) = (p.parent(), p.file_name()) else {
            return Err(format!("{path} names no new folder"));
        };
        // A parent outside the roots and a missing one read the same: nothing is told about
        // paths outside (SPEC §2.1).
        let parent = match parent.canonicalize() {
            Ok(c) if self.cfg.inside_roots(&c) && c.is_dir() => c,
            _ => {
                return Err(format!(
                    "{} is not a folder inside allowed_roots",
                    parent.display()
                ))
            }
        };
        let target = parent.join(name);
        if target.to_str().is_none() {
            return Err(format!("{} is not valid UTF-8", target.display()));
        }
        match std::fs::symlink_metadata(&target) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => return Err(format!("{} already exists", target.display())),
            Err(e) => return Err(format!("{}: {e}", target.display())),
        }
        let created = self.created.paths();
        if let Some(k) = known
            .iter()
            .chain(&created)
            .find(|k| target.starts_with(k) || k.starts_with(&target))
        {
            return Err(format!(
                "{} overlaps the workspace {}",
                target.display(),
                k.display()
            ));
        }
        // Fails if someone made it meanwhile: the folder is the worker's own from here.
        std::fs::create_dir(&target).map_err(|e| format!("{}: {e}", target.display()))?;
        if let Err(e) = self.created.begin(&target, origin) {
            let _ = std::fs::remove_dir(&target);
            return Err(format!("the list of created folders: {e:#}"));
        }
        Ok(target)
    }

    /// `workspace.delete`: only a folder of the list, by the canonical path it was created
    /// at; one that is gone already is `removed: false`. A worktree is removed by git in its
    /// origin, refused with uncommitted files unless `force`.
    pub fn delete(&self, d: &WorkspaceDelete) -> Result<WorkspaceDeleteResult, String> {
        let path = d.path.as_str();
        let p = Path::new(path);
        let not_ours = || format!("{path} is not a folder this worker created");
        let entry = self.created.get(p).ok_or_else(not_ours)?;
        if entry.creating {
            return Err(format!("{path} is still being created"));
        }
        let forget = || {
            self.created
                .forget(p)
                .map_err(|e| format!("the list of created folders: {e:#}"))
        };
        match p.canonicalize() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(origin) = &entry.origin {
                    if let Err(e) = prune(&self.cfg, origin) {
                        tracing::warn!(origin = %origin.display(), "git worktree prune: {e}");
                    }
                }
                forget()?;
                return Ok(WorkspaceDeleteResult { removed: false });
            }
            Err(e) => return Err(format!("{path}: {e}")),
            // Replaced by a symlink (or under one) since: what it leads to is not ours.
            Ok(c) if c != p || !self.cfg.inside_roots(&c) => {
                return Err(format!(
                    "{path} no longer resolves to the folder this worker created"
                ))
            }
            Ok(_) => {}
        }
        match &entry.origin {
            // git removes only a worktree of that repository; the branch stays.
            Some(origin) => {
                let origin = match origin.canonicalize() {
                    Ok(c) if &c == origin && self.cfg.inside_roots(&c) => c,
                    _ => {
                        return Err(format!(
                        "the repository of {path}, {}, is no longer a folder inside allowed_roots",
                        origin.display()
                    ))
                    }
                };
                let mut args = vec!["worktree", "remove"];
                if d.force {
                    args.push("--force");
                }
                args.extend(["--", path]);
                git_output(&self.cfg.environment(&origin), &origin, &args)?;
                prune(&self.cfg, &origin)?;
                forget()?;
                tracing::info!(%path, origin = %origin.display(), force = d.force, "worktree removed");
            }
            None => {
                std::fs::remove_dir_all(p).map_err(|e| format!("removing {path}: {e}"))?;
                forget()?;
                tracing::info!(%path, "workspace folder deleted");
            }
        }
        Ok(WorkspaceDeleteResult { removed: true })
    }
}

/// `workspace.inspect`: a folder inside `allowed_roots`; one without `.git` is `git: false`.
pub fn inspect(cfg: &Config, path: &str) -> Result<WorkspaceInspectResult, String> {
    let p = Path::new(path);
    let dir = match p.canonicalize() {
        Ok(d) if p.is_absolute() && d.is_dir() && cfg.inside_roots(&d) => d,
        _ => return Err(format!("{path} is not a folder inside allowed_roots")),
    };
    if std::fs::symlink_metadata(dir.join(".git")).is_err() {
        return Ok(WorkspaceInspectResult::default());
    }
    let env = cfg.environment(&dir);
    let status = git_output(&env, &dir, &["status", "--porcelain=v1", "-z"])?;
    let mut dirty = vec![];
    let mut dirty_count = 0u32;
    let mut fields = status.split('\0').filter(|f| !f.is_empty());
    while let Some(f) = fields.next() {
        let (Some(code), Some(file)) = (f.get(..2), f.get(3..)) else {
            continue;
        };
        // A rename or copy is followed by its old path.
        if code.starts_with(['R', 'C']) {
            fields.next();
        }
        dirty_count += 1;
        if dirty.len() < MAX_DIRTY {
            dirty.push(file.to_string());
        }
    }
    let mut unpushed = vec![];
    // A linked worktree (`.git` is a file, T46) shares the branches of its repository; only
    // the one it has checked out is its own (none when detached).
    let refs = if dir.join(".git").is_file() {
        git_output(&env, &dir, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_default()
    } else {
        git_output(
            &env,
            &dir,
            &["for-each-ref", "--format=%(refname)", "refs/heads"],
        )?
    };
    for r in refs.lines().filter(|r| r.starts_with("refs/heads/")) {
        let n = git_output(
            &env,
            &dir,
            &["rev-list", "--count", r, "--not", "--remotes"],
        )?;
        let commits: u32 = n.trim().parse().unwrap_or(0);
        if commits > 0 {
            unpushed.push(Unpushed {
                branch: r.trim_start_matches("refs/heads/").to_string(),
                commits,
            });
            if unpushed.len() == MAX_UNPUSHED {
                break;
            }
        }
    }
    Ok(WorkspaceInspectResult {
        git: true,
        dirty,
        dirty_count,
        unpushed,
    })
}

/// The environment of a `git` the worker runs for a workspace: the folder's environment
/// ([`Environment::process_env`]; the keys of T44 come as `SSH_AUTH_SOCK`, [`git_run`]) and never a prompt — no terminal,
/// no credential dialog (`GIT_ASKPASS` answers empty), ssh in batch mode unless the
/// environment brings its own ssh command.
fn git_env(env: &Environment) -> Vec<(String, String)> {
    let mut vars = env.process_env();
    let own_ssh = env
        .env
        .names()
        .any(|n| n == "GIT_SSH_COMMAND" || n == "GIT_SSH");
    let mut set = vec![
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_ASKPASS", "true"),
        ("GCM_INTERACTIVE", "never"),
    ];
    if !own_ssh {
        set.push(("GIT_SSH_COMMAND", "ssh -o BatchMode=yes"));
    }
    for (k, v) in set {
        vars.retain(|(x, _)| x != k);
        vars.push((k.into(), v.into()));
    }
    vars
}

/// What [`Workspaces::make`] runs once the new folder exists.
enum Run<'a> {
    Clone {
        url: &'a str,
        branch: Option<&'a str>,
        /// `GIT_ALLOW_PROTOCOL`.
        protocols: &'static str,
    },
    Worktree {
        origin: PathBuf,
        /// The options of `git worktree add` before `--`.
        options: Vec<OsString>,
        /// What it checks out after the path.
        commit: String,
    },
}

/// The checks of a worktree's branch in its `origin` and what `git worktree add` gets for it:
/// a new branch from `HEAD` (refused if it exists), a local branch that no worktree has
/// checked out, or a branch of exactly one remote, which becomes a local branch tracking it.
/// The name passes [`check_branch`] and `git check-ref-format --branch`.
fn worktree_args(
    env: &Environment,
    origin: &Path,
    branch: &str,
    new_branch: bool,
) -> Result<(Vec<OsString>, String), String> {
    check_branch(branch)?;
    git_output(env, origin, &["check-ref-format", "--branch", branch])
        .map_err(|_| format!("{branch:?} is not a branch name"))?;
    let local = format!("refs/heads/{branch}");
    let exists = git_output(env, origin, &["rev-parse", "--verify", "--quiet", &local]).is_ok();
    if new_branch {
        if exists {
            return Err(format!(
                "the branch {branch} exists already: pick it as an existing branch"
            ));
        }
        return Ok((vec!["-b".into(), branch.into()], "HEAD".into()));
    }
    if exists {
        let list = git_output(env, origin, &["worktree", "list", "--porcelain"])?;
        let mut at = "";
        for line in list.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                at = p;
            } else if line.strip_prefix("branch ") == Some(local.as_str()) {
                return Err(format!(
                    "the branch {branch} is already checked out at {at}"
                ));
            }
        }
        return Ok((vec![], branch.into()));
    }
    let remotes = git_output(
        env,
        origin,
        &["for-each-ref", "--format=%(refname)", "refs/remotes"],
    )?;
    let found: Vec<&str> = remotes
        .lines()
        .filter(|r| {
            r.strip_prefix("refs/remotes/")
                .and_then(|r| r.split_once('/'))
                .is_some_and(|(_, b)| b == branch)
        })
        .collect();
    match found.as_slice() {
        [remote] => Ok((
            vec!["--track".into(), "-b".into(), branch.into()],
            remote.to_string(),
        )),
        [] => Err(format!(
            "there is no branch {branch} in {}: make it a new branch",
            origin.display()
        )),
        _ => Err(format!(
            "the branch {branch} is on several remotes: fetch the one you want as a local branch first"
        )),
    }
}

/// `git <args>` for a new workspace in `dir` (the new folder for a clone, the origin for a
/// worktree), in the environment of the new folder ([`git_env`]) with `SSH_AUTH_SOCK` of
/// `ssh_agent` if that environment has keys, and for a clone only `protocols`
/// (`GIT_ALLOW_PROTOCOL`). No shell, stdin closed, its own process group, killed with whatever
/// it started after `timeout`.
async fn git_run(
    env: &Environment,
    ssh_agent: Option<&Path>,
    dir: &Path,
    args: &[OsString],
    protocols: Option<&str>,
    timeout: Duration,
) -> Result<(), String> {
    let what = args
        .iter()
        .take_while(|a| !a.to_string_lossy().starts_with('-'))
        .map(|a| a.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .env_clear()
        .envs(git_env(env))
        .envs(protocols.map(|p| ("GIT_ALLOW_PROTOCOL", p)))
        .envs(ssh_agent.map(|s| (crate::ssh::SSH_AUTH_SOCK, s)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().map_err(|e| format!("starting git: {e}"))?;
    let pid = child.id();
    let mut stderr = child.stderr.take().expect("piped stderr");
    let read = tokio::spawn(async move {
        let mut kept = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 {
                break;
            }
            kept.extend_from_slice(&buf[..n]);
            if kept.len() > STDERR_KEEP {
                kept.drain(..kept.len() - STDERR_KEEP);
            }
        }
        kept
    });
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(r) => r.map_err(|e| format!("git: {e}"))?,
        Err(_) => {
            #[cfg(unix)]
            if let Some(pid) = pid {
                // SAFETY: a plain syscall on the child's own group (`process_group(0)`).
                unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
            }
            let _ = child.kill().await;
            read.abort();
            return Err(format!(
                "git {what} did not finish in {} s",
                timeout.as_secs()
            ));
        }
    };
    // A grandchild (ssh) may hold stderr open a little longer.
    let err = match tokio::time::timeout(Duration::from_secs(2), read).await {
        Ok(Ok(b)) => b,
        _ => vec![],
    };
    if status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&err);
    let text = text.trim();
    let start = text
        .char_indices()
        .rev()
        .nth(STDERR_SHOWN)
        .map_or(0, |(i, _)| i);
    Err(format!("git {what} failed ({status}): {}", &text[start..]))
}

/// `git -C <dir> <args>`: its stdout, or why it failed. Quick commands only (no timeout).
fn git_output(env: &Environment, dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut child = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_clear()
        .envs(git_env(env))
        // No optional lock on the index (`status` would take one to refresh it).
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("starting git: {e}"))?;
    let mut out = String::new();
    let mut err = String::new();
    let stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let reader = std::thread::spawn(move || {
        let _ = stderr.read_to_string(&mut err);
        err
    });
    let r = { stdout }.read_to_string(&mut out);
    let status = child.wait().map_err(|e| format!("git: {e}"))?;
    let err = reader.join().unwrap_or_default();
    r.map_err(|e| format!("git {}: {e}", args[0]))?;
    if !status.success() {
        let tail: String = err.trim().chars().take(STDERR_SHOWN).collect();
        return Err(format!("git {} failed: {tail}", args[0]));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outbox::Outbox;

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=hqc",
                "-c",
                "user.email=hqc@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    struct Fx {
        dir: tempfile::TempDir,
        root: PathBuf,
        /// `root/repos/origin.git`, a bare repository with one commit on `main`.
        origin: PathBuf,
        hqc_home: PathBuf,
    }

    impl Fx {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("root");
            std::fs::create_dir_all(root.join("repos/src")).unwrap();
            let root = root.canonicalize().unwrap();
            let src = root.join("repos/src");
            git(&src, &["init", "-q"]);
            std::fs::write(src.join("README.md"), "hello\n").unwrap();
            git(&src, &["add", "README.md"]);
            git(&src, &["commit", "-q", "-m", "first"]);
            git(
                &root.join("repos"),
                &["clone", "-q", "--bare", "src", "origin.git"],
            );
            let hqc_home = dir.path().join("hqc");
            Self {
                origin: root.join("repos/origin.git"),
                root,
                hqc_home,
                dir,
            }
        }

        fn config(&self, extra: &str) -> Config {
            Config::parse(&format!(
                "hub_url = \"https://h\"\nallowed_roots = [{:?}]\n{extra}",
                self.root.display().to_string()
            ))
            .unwrap()
        }

        fn file(&self) -> PathBuf {
            self.hqc_home.join(CREATED_FILE)
        }

        fn workspaces(&self, cfg: Config) -> Arc<Workspaces> {
            let created = Created::load(&self.file(), &cfg).unwrap();
            let outbox = Arc::new(Outbox::open(&self.hqc_home.join("outbox")).unwrap());
            Workspaces::new(Arc::new(cfg), created, HubLink::new(outbox).0)
        }

        fn url(&self) -> String {
            format!("file://{}", self.origin.display())
        }
    }

    fn clone_req(path: &Path, url: &str) -> WorkspaceCreate {
        WorkspaceCreate {
            request_id: "r-1".into(),
            path: path.to_string_lossy().into_owned(),
            source: WorkspaceSource::Clone {
                url: url.into(),
                branch: None,
            },
        }
    }

    fn del(path: &Path) -> WorkspaceDelete {
        WorkspaceDelete {
            request_id: "d-1".into(),
            path: path.to_string_lossy().into_owned(),
            force: false,
        }
    }

    fn worktree_req(path: &Path, origin: &Path, branch: &str, new_branch: bool) -> WorkspaceCreate {
        WorkspaceCreate {
            request_id: "r-2".into(),
            path: path.to_string_lossy().into_owned(),
            source: WorkspaceSource::Worktree {
                origin: origin.to_string_lossy().into_owned(),
                branch: branch.into(),
                new_branch,
            },
        }
    }

    fn listed(fx: &Fx) -> Vec<Entry> {
        serde_json::from_slice(&std::fs::read(fx.file()).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn clone_into_a_new_folder() {
        let fx = Fx::new();
        let ws = fx.workspaces(fx.config(""));
        let target = fx.root.join("work/c6");
        std::fs::create_dir(fx.root.join("work")).unwrap();
        let got = ws.make(&clone_req(&target, &fx.url()), &[]).await.unwrap();
        assert_eq!(got, target);
        assert_eq!(
            std::fs::read_to_string(target.join("README.md")).unwrap(),
            "hello\n"
        );
        assert_eq!(
            listed(&fx),
            [Entry {
                path: target.clone(),
                creating: false,
                origin: None
            }]
        );
        // A branch that does not exist fails and leaves nothing.
        let mut r = clone_req(&fx.root.join("work/c7"), &fx.url());
        r.source = WorkspaceSource::Clone {
            url: fx.url(),
            branch: Some("nope".into()),
        };
        let e = ws.make(&r, &[]).await.unwrap_err();
        assert!(e.contains("git clone failed"), "{e}");
        assert!(!fx.root.join("work/c7").exists());
        assert_eq!(ws.created(), [target]);
    }

    #[tokio::test]
    async fn refusals_create_nothing() {
        let fx = Fx::new();
        let ws = fx.workspaces(fx.config(""));
        let outside = fx.dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::create_dir(fx.root.join("taken")).unwrap();
        std::fs::create_dir_all(fx.root.join("known/inner")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, fx.root.join("escape")).unwrap();
        let known = [fx.root.join("known")];
        let url = fx.url();
        let cases: Vec<(PathBuf, String, &str)> = vec![
            (
                outside.join("x"),
                url.clone(),
                "not a folder inside allowed_roots",
            ),
            (
                fx.root.join("escape/x"),
                url.clone(),
                "not a folder inside allowed_roots",
            ),
            (
                fx.root.join("missing/x"),
                url.clone(),
                "not a folder inside allowed_roots",
            ),
            (fx.root.join("taken"), url.clone(), "already exists"),
            (
                fx.root.join("known/inner/x"),
                url.clone(),
                "overlaps the workspace",
            ),
            (fx.root.join("known"), url.clone(), "already exists"),
            (fx.root.join("a/../b"), url.clone(), "without . or .."),
            (PathBuf::from("relative/x"), url.clone(), "absolute"),
            (fx.root.join("x"), "ext::sh".into(), "not allowed"),
            (
                fx.root.join("x"),
                "/abs/local/path".into(),
                "not a clone URL",
            ),
            (
                fx.root.join("x"),
                format!("file://{}", outside.display()),
                "not a repository inside allowed_roots",
            ),
        ];
        for (path, url, why) in cases {
            let e = ws.make(&clone_req(&path, &url), &known).await.unwrap_err();
            assert!(e.contains(why), "{}: {e}", path.display());
        }
        assert!(!fx.root.join("x").exists() && !outside.join("x").exists());
        assert!(ws.created().is_empty());
        // A folder of its own list overlaps too.
        let mine = fx.root.join("mine");
        ws.make(&clone_req(&mine, &fx.url()), &[]).await.unwrap();
        let e = ws
            .make(&clone_req(&mine.join("sub"), &fx.url()), &[])
            .await
            .unwrap_err();
        assert!(
            e.contains("already exists") || e.contains("overlaps"),
            "{e}"
        );
    }

    /// The clone runs in the environment of the new path (its `env`), and a clone that hangs
    /// is killed after `clone_timeout` with its folder removed.
    #[tokio::test]
    async fn timeout_kills_and_removes() {
        let fx = Fx::new();
        std::fs::create_dir(fx.root.join("slow")).unwrap();
        let cfg = fx.config(&format!(
            "clone_timeout = 1\n[environments.slow]\nenv = {{ GIT_SSH_COMMAND = \"sleep 30 #\" }}\n\
             [[env_bindings]]\npath = {:?}\nenvironment = \"slow\"\n",
            fx.root.join("slow").display().to_string()
        ));
        let ws = fx.workspaces(cfg);
        let t = std::time::Instant::now();
        let target = fx.root.join("slow/repo");
        let e = ws
            .make(&clone_req(&target, "ssh://git@example.invalid/x.git"), &[])
            .await
            .unwrap_err();
        assert!(e.contains("did not finish in 1 s"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(10));
        assert!(!target.exists());
        assert!(listed(&fx).is_empty());
    }

    #[tokio::test]
    async fn delete_only_its_own_folders() {
        let fx = Fx::new();
        let ws = fx.workspaces(fx.config(""));
        let mine = fx.root.join("mine");
        ws.make(&clone_req(&mine, &fx.url()), &[]).await.unwrap();
        let other = fx.root.join("other");
        std::fs::create_dir(&other).unwrap();
        let e = ws.delete(&del(&other)).unwrap_err();
        assert!(e.contains("not a folder this worker created"), "{e}");
        assert!(other.exists());
        // A path that leads to its folder is not its path.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&mine, fx.root.join("alias")).unwrap();
            assert!(ws.delete(&del(&fx.root.join("alias"))).is_err());
        }
        assert_eq!(
            ws.delete(&del(&mine)).unwrap(),
            WorkspaceDeleteResult { removed: true }
        );
        assert!(!mine.exists() && listed(&fx).is_empty());
        assert!(ws.delete(&del(&mine)).is_err(), "forgotten");

        // Its folder swapped for a symlink to someone else's: nothing is deleted.
        #[cfg(unix)]
        {
            let again = fx.root.join("again");
            ws.make(&clone_req(&again, &fx.url()), &[]).await.unwrap();
            std::fs::remove_dir_all(&again).unwrap();
            std::os::unix::fs::symlink(&other, &again).unwrap();
            let e = ws.delete(&del(&again)).unwrap_err();
            assert!(e.contains("no longer resolves"), "{e}");
            assert!(other.exists());
            std::fs::remove_file(&again).unwrap();
            // Gone already: nothing to remove, the entry goes.
            assert_eq!(
                ws.delete(&del(&again)).unwrap(),
                WorkspaceDeleteResult { removed: false }
            );
            assert!(ws.created().is_empty());
        }
    }

    /// The list survives a restart; a clone the previous run did not finish is removed.
    #[tokio::test]
    async fn list_survives_restart() {
        let fx = Fx::new();
        let cfg = fx.config("");
        let ws = fx.workspaces(cfg.clone());
        let done = fx.root.join("done");
        ws.make(&clone_req(&done, &fx.url()), &[]).await.unwrap();
        let partial = fx.root.join("partial");
        std::fs::create_dir(&partial).unwrap();
        std::fs::write(partial.join("half"), "x").unwrap();
        ws.created.begin(&partial, None).unwrap();
        drop(ws);
        let created = Created::load(&fx.file(), &cfg).unwrap();
        assert_eq!(created.paths(), std::slice::from_ref(&done));
        assert!(done.join("README.md").exists());
        assert!(!partial.exists());
        assert_eq!(
            listed(&fx),
            [Entry {
                path: done,
                creating: false,
                origin: None
            }]
        );
        std::fs::write(fx.file(), "not json").unwrap();
        assert!(Created::load(&fx.file(), &cfg).is_err());
    }

    #[tokio::test]
    async fn dirty_and_unpushed() {
        let fx = Fx::new();
        let cfg = fx.config("");
        let ws = fx.workspaces(cfg.clone());
        let c = fx.root.join("c");
        ws.make(&clone_req(&c, &fx.url()), &[]).await.unwrap();
        let clean = inspect(&cfg, c.to_str().unwrap()).unwrap();
        assert_eq!(
            clean,
            WorkspaceInspectResult {
                git: true,
                ..Default::default()
            }
        );
        std::fs::write(c.join("README.md"), "changed\n").unwrap();
        std::fs::write(c.join("new file.txt"), "x").unwrap();
        git(&c, &["switch", "-q", "-c", "hqc/fix"]);
        std::fs::write(c.join("a.txt"), "a").unwrap();
        git(&c, &["add", "a.txt"]);
        git(&c, &["commit", "-q", "-m", "a"]);
        std::fs::write(c.join("b.txt"), "b").unwrap();
        git(&c, &["add", "b.txt"]);
        git(&c, &["commit", "-q", "-m", "b"]);
        git(&c, &["mv", "b.txt", "c.txt"]);
        let r = inspect(&cfg, c.to_str().unwrap()).unwrap();
        assert!(r.git);
        let mut dirty = r.dirty.clone();
        dirty.sort();
        assert_eq!(dirty, ["README.md", "c.txt", "new file.txt"]);
        assert_eq!(r.dirty_count, 3);
        assert_eq!(
            r.unpushed,
            [Unpushed {
                branch: "hqc/fix".into(),
                commits: 2
            }]
        );
        // Not a git folder.
        let plain = fx.root.join("plain");
        std::fs::create_dir(&plain).unwrap();
        assert_eq!(
            inspect(&cfg, plain.to_str().unwrap()).unwrap(),
            WorkspaceInspectResult::default()
        );
        assert!(inspect(&cfg, fx.dir.path().to_str().unwrap()).is_err());
    }

    /// `git <args>` in `dir`: its trimmed stdout, or `None` when it fails.
    fn git_read(dir: &Path, args: &[&str]) -> Option<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// A clone `root/c6` of the fixture's repository, the origin of the worktree tests.
    async fn origin(fx: &Fx, ws: &Workspaces) -> PathBuf {
        let c6 = fx.root.join("c6");
        ws.make(&clone_req(&c6, &fx.url()), &[]).await.unwrap();
        c6
    }

    #[tokio::test]
    async fn worktree_on_a_new_and_an_existing_branch() {
        let fx = Fx::new();
        let ws = fx.workspaces(fx.config(""));
        let c6 = origin(&fx, &ws).await;

        // A new branch from the origin's HEAD.
        let fix = fx.root.join("c6-hqc-fix");
        let got = ws
            .make(&worktree_req(&fix, &c6, "hqc/fix", true), &[])
            .await
            .unwrap();
        assert_eq!(got, fix);
        assert_eq!(
            git_read(&fix, &["symbolic-ref", "HEAD"]).as_deref(),
            Some("refs/heads/hqc/fix")
        );
        assert!(fix.join("README.md").exists() && fix.join(".git").is_file());
        assert_eq!(
            listed(&fx).last(),
            Some(&Entry {
                path: fix.clone(),
                creating: false,
                origin: Some(c6.clone())
            })
        );

        // An existing local branch that no worktree has.
        git(&c6, &["branch", "side"]);
        let side = fx.root.join("c6-side");
        ws.make(&worktree_req(&side, &c6, "side", false), &[])
            .await
            .unwrap();
        assert_eq!(
            git_read(&side, &["symbolic-ref", "HEAD"]).as_deref(),
            Some("refs/heads/side")
        );

        // A branch only a remote has becomes a local one tracking it.
        git(&c6, &["update-ref", "refs/remotes/origin/feature", "HEAD"]);
        let feature = fx.root.join("c6-feature");
        ws.make(&worktree_req(&feature, &c6, "feature", false), &[])
            .await
            .unwrap();
        assert_eq!(
            git_read(
                &feature,
                &["rev-parse", "--abbrev-ref", "feature@{upstream}"]
            )
            .as_deref(),
            Some("origin/feature")
        );

        // Refusals leave nothing behind.
        let x = fx.root.join("c6-x");
        let plain = fx.root.join("plain");
        std::fs::create_dir(&plain).unwrap();
        let cases: Vec<(WorkspaceCreate, &[PathBuf], &str)> = vec![
            (
                worktree_req(&x, &c6, "hqc/fix", false),
                &[],
                "already checked out at",
            ),
            (
                worktree_req(&x, &c6, "main", false),
                &[],
                "already checked out at",
            ),
            (worktree_req(&x, &c6, "side", true), &[], "exists already"),
            (
                worktree_req(&x, &c6, "nope", false),
                &[],
                "there is no branch",
            ),
            (
                worktree_req(&x, &c6, "a..b", true),
                &[],
                "not a branch name",
            ),
            (worktree_req(&x, &c6, "-b", true), &[], "not a branch name"),
            (
                worktree_req(&x, &c6, "a@{-1}", true),
                &[],
                "not a branch name",
            ),
            (
                worktree_req(&c6.join("inner"), &c6, "inner", true),
                &[],
                "overlaps the workspace",
            ),
            (
                worktree_req(&x, &fx.root.join("repos/src"), "y", true),
                &[],
                "not a workspace of this worker",
            ),
            (
                worktree_req(&x, &plain, "y", true),
                std::slice::from_ref(&plain),
                "not the top folder of a git repository",
            ),
            (
                worktree_req(&x, &fx.dir.path().join("outside"), "y", true),
                &[],
                "not a workspace of this worker",
            ),
        ];
        for (req, known, why) in cases {
            let e = ws.make(&req, known).await.unwrap_err();
            assert!(e.contains(why), "{req:?}: {e}");
        }
        assert!(!x.exists() && !c6.join("inner").exists());
        // A repository the hub told about (`known`) is an origin too.
        let src = fx.root.join("repos/src");
        let y = fx.root.join("src-y");
        ws.make(
            &worktree_req(&y, &src, "y", true),
            std::slice::from_ref(&src),
        )
        .await
        .unwrap();
        assert_eq!(ws.created(), [c6, fix, side, feature, y]);
    }

    /// The branch `hqc_spawn` names a worktree after (T47): `hqc/` and the session's slug, new
    /// from the origin's `HEAD`, in the hub's default folder; the same name again (a slug used
    /// once more after its session was deleted) is refused, since the branch outlives its
    /// worktree.
    #[tokio::test]
    async fn worktree_on_a_spawn_branch() {
        let fx = Fx::new();
        let ws = fx.workspaces(fx.config(""));
        let c6 = origin(&fx, &ws).await;
        let head = git_read(&c6, &["rev-parse", "HEAD"]);
        let wt = fx.root.join("c6-hqc-session-12");
        ws.make(&worktree_req(&wt, &c6, "hqc/session-12", true), &[])
            .await
            .unwrap();
        assert_eq!(
            git_read(&wt, &["symbolic-ref", "HEAD"]).as_deref(),
            Some("refs/heads/hqc/session-12")
        );
        assert_eq!(git_read(&wt, &["rev-parse", "HEAD"]), head);
        ws.delete(&del(&wt)).unwrap();
        assert!(!wt.exists());
        assert!(git_read(&c6, &["rev-parse", "--verify", "refs/heads/hqc/session-12"]).is_some());
        let e = ws
            .make(&worktree_req(&wt, &c6, "hqc/session-12", true), &[])
            .await
            .unwrap_err();
        assert!(e.contains("exists already"), "{e}");
        assert!(!wt.exists());
    }

    /// `git worktree add` runs in the environment of the new path (its `env` reaches the
    /// origin's `post-checkout` hook), not of the origin.
    #[cfg(unix)]
    #[tokio::test]
    async fn worktree_runs_in_the_environment_of_its_path() {
        use std::os::unix::fs::PermissionsExt as _;
        let fx = Fx::new();
        std::fs::create_dir(fx.root.join("bound")).unwrap();
        let cfg = fx.config(&format!(
            "[environments.infra]\nenv = {{ T46_MARK = \"infra\" }}\n\
             [[env_bindings]]\npath = {:?}\nenvironment = \"infra\"\n",
            fx.root.join("bound").display().to_string()
        ));
        let ws = fx.workspaces(cfg);
        let c6 = origin(&fx, &ws).await;
        let hook = c6.join(".git/hooks/post-checkout");
        std::fs::write(&hook, "#!/bin/sh\necho \"${T46_MARK:-none}\" > mark.txt\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let wt = fx.root.join("bound/c6-infra");
        ws.make(&worktree_req(&wt, &c6, "infra", true), &[])
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(wt.join("mark.txt")).unwrap(),
            "infra\n"
        );
    }

    #[tokio::test]
    async fn remove_worktree_with_and_without_changes() {
        let fx = Fx::new();
        let cfg = fx.config("");
        let ws = fx.workspaces(cfg.clone());
        let c6 = origin(&fx, &ws).await;
        // A branch of the origin with commits of its own, not the worktree's.
        git(&c6, &["switch", "-q", "-c", "elsewhere"]);
        git(&c6, &["commit", "-q", "--allow-empty", "-m", "elsewhere"]);
        git(&c6, &["switch", "-q", "main"]);
        let fix = fx.root.join("c6-fix");
        ws.make(&worktree_req(&fix, &c6, "hqc/fix", true), &[])
            .await
            .unwrap();
        std::fs::write(fix.join("a.txt"), "a").unwrap();
        git(&fix, &["add", "a.txt"]);
        git(&fix, &["commit", "-q", "-m", "a"]);
        std::fs::write(fix.join("b.txt"), "b").unwrap();
        // Only the worktree's own branch counts.
        let r = inspect(&cfg, fix.to_str().unwrap()).unwrap();
        assert_eq!(r.dirty, ["b.txt"]);
        assert_eq!(
            r.unpushed,
            [Unpushed {
                branch: "hqc/fix".into(),
                commits: 1
            }]
        );

        // Uncommitted files: refused unless the user confirmed them.
        let e = ws.delete(&del(&fix)).unwrap_err();
        assert!(e.contains("modified or untracked"), "{e}");
        assert!(fix.join("b.txt").exists() && ws.created().contains(&fix));
        let mut force = del(&fix);
        force.force = true;
        assert_eq!(
            ws.delete(&force).unwrap(),
            WorkspaceDeleteResult { removed: true }
        );
        assert!(!fix.exists() && !ws.created().contains(&fix));
        let list = git_read(&c6, &["worktree", "list", "--porcelain"]).unwrap();
        assert!(!list.contains("c6-fix"), "{list}");
        // The branch and its commit stay.
        assert!(git_read(
            &c6,
            &["rev-parse", "--verify", "--quiet", "refs/heads/hqc/fix"]
        )
        .is_some());

        // A clean one goes without force; one removed by hand is pruned.
        let clean = fx.root.join("c6-clean");
        ws.make(&worktree_req(&clean, &c6, "clean", true), &[])
            .await
            .unwrap();
        assert_eq!(
            ws.delete(&del(&clean)).unwrap(),
            WorkspaceDeleteResult { removed: true }
        );
        let gone = fx.root.join("c6-gone");
        ws.make(&worktree_req(&gone, &c6, "gone", true), &[])
            .await
            .unwrap();
        std::fs::remove_dir_all(&gone).unwrap();
        assert_eq!(
            ws.delete(&del(&gone)).unwrap(),
            WorkspaceDeleteResult { removed: false }
        );
        let list = git_read(&c6, &["worktree", "list", "--porcelain"]).unwrap();
        assert!(
            !list.contains("c6-clean") && !list.contains("c6-gone"),
            "{list}"
        );
        // The branch can be checked out again: git forgot both worktrees.
        let again = fx.root.join("c6-again");
        ws.make(&worktree_req(&again, &c6, "gone", false), &[])
            .await
            .unwrap();
        assert_eq!(ws.created(), [c6, again]);
    }

    /// A worktree the previous run did not finish is removed at start, and git forgets it.
    #[tokio::test]
    async fn unfinished_worktree_removed_at_start() {
        let fx = Fx::new();
        let cfg = fx.config("");
        let ws = fx.workspaces(cfg.clone());
        let c6 = origin(&fx, &ws).await;
        let half = fx.root.join("c6-half");
        git(
            &c6,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "half",
                half.to_str().unwrap(),
            ],
        );
        ws.created.begin(&half, Some(&c6)).unwrap();
        drop(ws);
        let created = Created::load(&fx.file(), &cfg).unwrap();
        assert_eq!(created.paths(), std::slice::from_ref(&c6));
        assert!(!half.exists());
        let list = git_read(&c6, &["worktree", "list", "--porcelain"]).unwrap();
        assert!(!list.contains("c6-half"), "{list}");
    }
}
