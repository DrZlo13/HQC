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
//! - `workspace.delete` removes only a folder of that list, by its canonical path: the hub
//!   cannot name another one (SPEC §2.1).
//! - `workspace.inspect` reports what deleting a folder would lose: uncommitted changes and
//!   commits that are on no remote branch.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use hqc_proto::frame::{
    check_branch, check_clone_url, CloneFrom, Unpushed, WorkerFrame, WorkspaceCreate,
    WorkspaceCreated, WorkspaceDeleteResult, WorkspaceInspectResult, WorkspaceSource, MAX_DIRTY,
    MAX_UNPUSHED,
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
    /// The clone into it runs (or ran when the worker stopped).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    creating: bool,
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
    /// removes the folders of clones a previous run did not finish.
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
                tracing::warn!(path = %p.display(), "unfinished clone outside allowed_roots left alone");
                created.forget(&p)?;
                continue;
            }
            tracing::warn!(path = %p.display(), "removing the folder of a clone the worker did not finish");
            created.remove(&p)?;
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

    fn begin(&self, path: &Path) -> anyhow::Result<()> {
        self.change(|l| {
            l.retain(|e| e.path != path);
            l.push(Entry {
                path: path.to_path_buf(),
                creating: true,
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

    /// Removes the folder, then the entry. A folder that cannot be removed keeps its entry, so
    /// the next start tries again (if it was a clone) or a later `workspace.delete` can.
    fn remove(&self, path: &Path) -> anyhow::Result<()> {
        match std::fs::remove_dir_all(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
        }
        self.forget(path)
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
                    tracing::info!(%request_id, path = %p.display(), "workspace cloned");
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
        let WorkspaceSource::Clone { url, branch } = &c.source;
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
        let target = self.new_folder(&c.path, known)?;
        let env = self.cfg.environment(&target);
        let r = git_clone(
            &env,
            url,
            branch.as_deref(),
            &target,
            protocols,
            self.cfg.clone_timeout,
        )
        .await;
        let r = r.and_then(|()| {
            self.created
                .finish(&target)
                .map_err(|e| format!("the list of created folders: {e:#}"))
        });
        if let Err(e) = r {
            if let Err(re) = self.created.remove(&target) {
                tracing::error!(path = %target.display(), "partial clone not removed: {re:#}");
            }
            return Err(e);
        }
        Ok(target)
    }

    /// Checks `path` and creates it, empty, as a folder of this worker's list.
    fn new_folder(&self, path: &str, known: &[PathBuf]) -> Result<PathBuf, String> {
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
        if let Err(e) = self.created.begin(&target) {
            let _ = std::fs::remove_dir(&target);
            return Err(format!("the list of created folders: {e:#}"));
        }
        Ok(target)
    }

    /// `workspace.delete`: only a folder of the list, by the canonical path it was created
    /// at; one that is gone already is `removed: false`.
    pub fn delete(&self, path: &str) -> Result<WorkspaceDeleteResult, String> {
        let p = Path::new(path);
        let not_ours = || format!("{path} is not a folder this worker created");
        let entry = self.created.get(p).ok_or_else(not_ours)?;
        if entry.creating {
            return Err(format!("a clone into {path} still runs"));
        }
        match p.canonicalize() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.created
                    .forget(p)
                    .map_err(|e| format!("the list of created folders: {e:#}"))?;
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
        self.created.remove(p).map_err(|e| format!("{e:#}"))?;
        tracing::info!(%path, "workspace folder deleted");
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
    let refs = git_output(
        &env,
        &dir,
        &["for-each-ref", "--format=%(refname)", "refs/heads"],
    )?;
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
/// ([`Environment::process_env`], where T44's keys come in) and never a prompt — no terminal,
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

/// `git clone [--branch <branch>] -- <url> <target>` into the empty `target`; only
/// `protocols` (`GIT_ALLOW_PROTOCOL`). Killed with whatever it started after `timeout`.
async fn git_clone(
    env: &Environment,
    url: &str,
    branch: Option<&str>,
    target: &Path,
    protocols: &str,
    timeout: Duration,
) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("clone").arg("--quiet");
    if let Some(b) = branch {
        cmd.arg("--branch").arg(b);
    }
    cmd.arg("--")
        .arg(url)
        .arg(target)
        .current_dir(target)
        .env_clear()
        .envs(git_env(env))
        .env("GIT_ALLOW_PROTOCOL", protocols)
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
                "git clone did not finish in {} s",
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
    Err(format!("git clone failed ({status}): {}", &text[start..]))
}

/// `git -C <dir> <args>`: its stdout, or why it failed.
fn git_output(env: &Environment, dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut child = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_clear()
        .envs(git_env(env))
        // Reading only: no lock on the index.
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
                creating: false
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
        let e = ws.delete(other.to_str().unwrap()).unwrap_err();
        assert!(e.contains("not a folder this worker created"), "{e}");
        assert!(other.exists());
        // A path that leads to its folder is not its path.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&mine, fx.root.join("alias")).unwrap();
            assert!(ws.delete(fx.root.join("alias").to_str().unwrap()).is_err());
        }
        assert_eq!(
            ws.delete(mine.to_str().unwrap()).unwrap(),
            WorkspaceDeleteResult { removed: true }
        );
        assert!(!mine.exists() && listed(&fx).is_empty());
        assert!(ws.delete(mine.to_str().unwrap()).is_err(), "forgotten");

        // Its folder swapped for a symlink to someone else's: nothing is deleted.
        #[cfg(unix)]
        {
            let again = fx.root.join("again");
            ws.make(&clone_req(&again, &fx.url()), &[]).await.unwrap();
            std::fs::remove_dir_all(&again).unwrap();
            std::os::unix::fs::symlink(&other, &again).unwrap();
            let e = ws.delete(again.to_str().unwrap()).unwrap_err();
            assert!(e.contains("no longer resolves"), "{e}");
            assert!(other.exists());
            std::fs::remove_file(&again).unwrap();
            // Gone already: nothing to remove, the entry goes.
            assert_eq!(
                ws.delete(again.to_str().unwrap()).unwrap(),
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
        ws.created.begin(&partial).unwrap();
        drop(ws);
        let created = Created::load(&fx.file(), &cfg).unwrap();
        assert_eq!(created.paths(), std::slice::from_ref(&done));
        assert!(done.join("README.md").exists());
        assert!(!partial.exists());
        assert_eq!(
            listed(&fx),
            [Entry {
                path: done,
                creating: false
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
}
