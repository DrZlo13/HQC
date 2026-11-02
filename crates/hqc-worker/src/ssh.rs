//! ssh keys of an environment (T44, SPEC §14).
//!
//! - The store `$HQC_HOME/keys/` (0700) holds one private key per file (0600), named as in
//!   `ssh_keys`. `hqc-worker env add-key <name> <file>` copies a key in ([`Store::add_key`]).
//!   A store or a file in it that others could read, and a name in `ssh_keys` that is not in
//!   the store, stop the worker at start ([`Store::check`]).
//! - Every agent process of a session whose environment has keys gets an `ssh-agent` of its
//!   own (never shared, not even within an environment) on a socket in `$HQC_HOME/agents/`
//!   (0700), with the keys loaded by `ssh-add`. Only that process gets its `SSH_AUTH_SOCK`.
//!   The adapter holds the [`SshAgent`] as long as the process runs; dropping it stops the
//!   agent and removes the socket. A worker that died leaves sockets behind: the next start
//!   empties those agents and removes them ([`Store::clean_agents`]).
//! - The worker reads key material only to copy it in `add-key`; logs, frames and `hqc-worker
//!   env` carry key names, never the keys.

use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context};
use tokio::io::{AsyncBufReadExt, BufReader};

/// The variable of an agent process with keys; never passed otherwise.
pub const SSH_AUTH_SOCK: &str = "SSH_AUTH_SOCK";
/// How long `ssh-agent` may take to listen and `ssh-add` to load the keys.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest socket path every unix takes (`sun_path` is 104 bytes on macOS).
const MAX_SOCKET_PATH: usize = 100;
/// `SSH_AGENTC_REMOVE_ALL_IDENTITIES` as a whole message (length 1, type 19).
const REMOVE_ALL_IDENTITIES: [u8; 5] = [0, 0, 0, 1, 19];

/// A key name: letters, digits, `_`, `-` and `.`, not starting with `.` or `-`.
pub fn valid_key_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && !name.starts_with(['.', '-'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// The worker's key store and the folder of its sessions' agent sockets.
#[derive(Debug, Clone, PartialEq)]
pub struct Store {
    /// `$HQC_HOME/keys`.
    pub keys: PathBuf,
    /// `$HQC_HOME/agents`.
    pub agents: PathBuf,
}

impl Store {
    pub fn in_home(home: &Path) -> Self {
        Self {
            keys: home.join("keys"),
            agents: home.join("agents"),
        }
    }

    /// At start: the store and the agents folder (where they exist) are the user's own and
    /// closed to others, every file in the store is a 0600 regular file, and every name in
    /// `names` is a key of the store.
    pub fn check<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> anyhow::Result<()> {
        if self.keys.exists() {
            private(&self.keys, true)?;
            for e in std::fs::read_dir(&self.keys)
                .with_context(|| format!("reading {}", self.keys.display()))?
            {
                private(&e?.path(), false)?;
            }
        }
        if self.agents.exists() {
            private(&self.agents, true)?;
        }
        for name in names {
            if !self.keys.join(name).exists() {
                bail!(
                    "ssh key {name:?} is not in the key store {}; add it with `hqc-worker env add-key {name} <file>`",
                    self.keys.display()
                );
            }
        }
        Ok(())
    }

    /// `hqc-worker env add-key`: copies `file` into the store as `name` (0600), creating the
    /// store (0700) if needed. An existing key is not replaced.
    pub fn add_key(&self, name: &str, file: &Path) -> anyhow::Result<PathBuf> {
        if !valid_key_name(name) {
            bail!("{name:?} is not a key name (letters, digits, _ - ., not starting with . or -)");
        }
        let key = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
        if self.keys.exists() {
            self.check(None)?;
        } else {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.keys)
                .with_context(|| format!("creating {}", self.keys.display()))?;
        }
        let dest = self.keys.join(name);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&dest)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::AlreadyExists => anyhow::anyhow!(
                    "{} is already in the key store; remove it first to replace it",
                    dest.display()
                ),
                _ => anyhow::Error::new(e).context(format!("creating {}", dest.display())),
            })?;
        f.write_all(&key)
            .and_then(|_| f.sync_all())
            .with_context(|| format!("writing {}", dest.display()))?;
        Ok(dest)
    }

    /// An `ssh-agent` of its own for one agent process, with the keys `names` loaded.
    pub async fn start_agent(&self, names: &[String]) -> anyhow::Result<SshAgent> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.agents)
            .with_context(|| format!("creating {}", self.agents.display()))?;
        private(&self.agents, true)?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let socket = self.agents.join(format!("{}.sock", &id[..16]));
        if socket.as_os_str().len() > MAX_SOCKET_PATH {
            bail!(
                "the ssh-agent socket {} is too long for a unix socket; use a shorter HQC_HOME",
                socket.display()
            );
        }
        let mut cmd = tokio::process::Command::new("ssh-agent");
        cmd.arg("-D")
            .arg("-a")
            .arg(&socket)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .process_group(0);
        if let Some(p) = std::env::var_os("PATH") {
            cmd.env("PATH", p);
        }
        let mut child = cmd.spawn().context("starting ssh-agent")?;
        let stdout = child.stdout.take().expect("piped stdout");
        // From here on, dropping it stops the agent.
        let agent = SshAgent { socket, child };
        // It prints `SSH_AUTH_SOCK=…` once the socket listens.
        let mut lines = BufReader::new(stdout).lines();
        if !matches!(
            tokio::time::timeout(START_TIMEOUT, lines.next_line()).await,
            Ok(Ok(Some(_)))
        ) {
            bail!("ssh-agent did not start");
        }

        let mut add = tokio::process::Command::new("ssh-add");
        add.arg("-q")
            .args(names.iter().map(|n| self.keys.join(n)))
            .env_clear()
            .env(SSH_AUTH_SOCK, &agent.socket)
            // A key with a passphrase fails instead of prompting on the worker's terminal.
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("SSH_ASKPASS", "/usr/bin/false")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        if let Some(p) = std::env::var_os("PATH") {
            add.env("PATH", p);
        }
        let out = tokio::time::timeout(START_TIMEOUT, add.output())
            .await
            .context("ssh-add did not finish")?
            .context("running ssh-add")?;
        if !out.status.success() {
            bail!(
                "ssh-add {}: {}",
                names.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(agent)
    }

    /// At the worker's start, before any session: the sockets left in the agents folder
    /// belong to no session (a worker that died could not stop its agents). An agent still
    /// listening is emptied of its keys; every socket is removed. Returns how many went.
    pub fn clean_agents(&self) -> anyhow::Result<usize> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.agents)
            .with_context(|| format!("creating {}", self.agents.display()))?;
        private(&self.agents, true)?;
        let mut n = 0;
        for e in std::fs::read_dir(&self.agents)
            .with_context(|| format!("reading {}", self.agents.display()))?
        {
            let e = e?;
            if !e.file_type()?.is_socket() {
                continue;
            }
            let path = e.path();
            if let Ok(mut s) = std::os::unix::net::UnixStream::connect(&path) {
                let _ = s.set_read_timeout(Some(Duration::from_secs(1)));
                let _ = s.set_write_timeout(Some(Duration::from_secs(1)));
                let mut reply = [0u8; 5];
                let _ = s
                    .write_all(&REMOVE_ALL_IDENTITIES)
                    .and_then(|_| s.read_exact(&mut reply));
            }
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
            n += 1;
        }
        Ok(n)
    }
}

/// The user's own folder (`dir`) or regular file, closed to group and others; a clear
/// refusal with the fix otherwise.
fn private(path: &Path, dir: bool) -> anyhow::Result<()> {
    let m = std::fs::symlink_metadata(path).with_context(|| format!("{}", path.display()))?;
    let (kind, fix) = if dir {
        ("folder", 0o700)
    } else {
        ("file", 0o600)
    };
    if (dir && !m.is_dir()) || (!dir && !m.is_file()) {
        bail!(
            "{} must be a {kind} (not a link or anything else): ssh keys live there",
            path.display()
        );
    }
    // SAFETY: geteuid(2) has no preconditions and touches no memory.
    if m.uid() != unsafe { libc::geteuid() } {
        bail!(
            "{} belongs to another user: ssh keys live there",
            path.display()
        );
    }
    let mode = m.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!(
            "{} has mode {mode:o}, so other users could read the ssh keys; run `chmod {fix:o} {}`",
            path.display(),
            path.display()
        );
    }
    Ok(())
}

/// The `ssh-agent` of one agent process. Dropping it stops the agent (its keys go with it)
/// and removes the socket.
pub struct SshAgent {
    socket: PathBuf,
    child: tokio::process::Child,
}

impl SshAgent {
    /// What the agent process gets as `SSH_AUTH_SOCK`.
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Drop for SshAgent {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_file(&self.socket);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A store under `/tmp`: socket paths must stay short.
    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::Builder::new()
            .prefix("hqc-t44-")
            .tempdir_in("/tmp")
            .unwrap();
        let s = Store::in_home(&dir.path().canonicalize().unwrap());
        (dir, s)
    }

    /// A fresh ed25519 key without passphrase at `<dir>/<name>`.
    pub(crate) fn keygen(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let ok = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", name, "-f"])
            .arg(&path)
            .status()
            .unwrap()
            .success();
        assert!(ok, "ssh-keygen");
        path
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    fn chmod(p: &Path, m: u32) {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
    }

    /// What `ssh-add -l` prints through `socket` (a line per key, with its comment, or "no
    /// identities"), or `None` if no agent listens there.
    pub(crate) fn listed(socket: &Path) -> Option<String> {
        let out = std::process::Command::new("ssh-add")
            .arg("-l")
            .env_clear()
            .env("SSH_AUTH_SOCK", socket)
            .output()
            .unwrap();
        // 0: keys, 1: no keys, 2: no agent.
        (out.status.code() != Some(2)).then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    #[test]
    fn add_key_makes_the_store_private_and_never_overwrites() {
        let (dir, s) = store();
        let src = keygen(dir.path(), "src");
        let dest = s.add_key("infra_ed25519", &src).unwrap();
        assert_eq!(dest, s.keys.join("infra_ed25519"));
        assert_eq!(mode(&s.keys), 0o700);
        assert_eq!(mode(&dest), 0o600);
        assert_eq!(std::fs::read(&dest).unwrap(), std::fs::read(&src).unwrap());
        s.check(["infra_ed25519"]).unwrap();

        let e = s.add_key("infra_ed25519", &src).unwrap_err();
        assert!(
            format!("{e:#}").contains("already in the key store"),
            "{e:#}"
        );
        for bad in ["", ".hidden", "-o", "a/b", "a b", "../x"] {
            assert!(s.add_key(bad, &src).is_err(), "{bad:?}");
        }
        assert!(s.add_key("missing", &dir.path().join("nope")).is_err());
        // A store others can read is refused, not repaired.
        chmod(&s.keys, 0o755);
        let e = s.add_key("other", &src).unwrap_err();
        assert!(format!("{e:#}").contains("chmod 700"), "{e:#}");
        assert!(!s.keys.join("other").exists());
    }

    /// Wrong modes refuse to start, with the fix in the message.
    #[test]
    fn wrong_modes_refuse_to_start() {
        let (dir, s) = store();
        s.check(None).unwrap();
        let e = s.check(["infra"]).unwrap_err();
        assert!(format!("{e:#}").contains("env add-key infra"), "{e:#}");

        s.add_key("infra", &keygen(dir.path(), "src")).unwrap();
        s.check(["infra"]).unwrap();
        chmod(&s.keys, 0o750);
        let e = s.check(["infra"]).unwrap_err();
        assert!(format!("{e:#}").contains("mode 750"), "{e:#}");
        chmod(&s.keys, 0o700);

        chmod(&s.keys.join("infra"), 0o644);
        let e = s.check(None).unwrap_err();
        assert!(format!("{e:#}").contains("chmod 600"), "{e:#}");
        chmod(&s.keys.join("infra"), 0o400);
        s.check(["infra"]).unwrap();

        // Every file of the store counts, and nothing in it may be a link.
        std::os::unix::fs::symlink(dir.path().join("src"), s.keys.join("link")).unwrap();
        assert!(format!("{:#}", s.check(None).unwrap_err()).contains("not a link"));
        std::fs::remove_file(s.keys.join("link")).unwrap();

        std::fs::create_dir(&s.agents).unwrap();
        chmod(&s.agents, 0o755);
        let e = s.check(None).unwrap_err();
        assert!(format!("{e:#}").contains("agents"), "{e:#}");
    }

    /// One agent per call, with the keys loaded; dropping it stops the agent and removes the
    /// socket.
    #[tokio::test]
    async fn an_agent_lives_as_long_as_its_guard() {
        let (dir, s) = store();
        s.add_key("infra", &keygen(dir.path(), "src")).unwrap();
        let a = s.start_agent(&["infra".into()]).await.unwrap();
        let b = s.start_agent(&["infra".into()]).await.unwrap();
        assert_ne!(a.socket(), b.socket());
        assert!(a.socket().starts_with(&s.agents));
        assert_eq!(mode(&s.agents), 0o700);
        assert!(listed(a.socket()).unwrap().contains("src"));
        let socket = a.socket().to_path_buf();
        drop(a);
        assert!(!socket.exists());
        assert_eq!(listed(&socket), None);
        assert!(listed(b.socket()).unwrap().contains("src"), "b is its own");

        // A key that does not load fails the start and leaves nothing behind.
        std::fs::write(s.keys.join("broken"), "not a key").unwrap();
        chmod(&s.keys.join("broken"), 0o600);
        let e = s.start_agent(&["broken".into()]).await.err().unwrap();
        assert!(format!("{e:#}").contains("ssh-add broken"), "{e:#}");
        drop(b);
        assert_eq!(std::fs::read_dir(&s.agents).unwrap().count(), 0);
    }

    /// A worker that died left an agent with keys: the next start empties and unlinks it.
    #[tokio::test]
    async fn stale_agents_are_cleaned_at_start() {
        let (dir, s) = store();
        s.add_key("infra", &keygen(dir.path(), "src")).unwrap();
        let a = s.start_agent(&["infra".into()]).await.unwrap();
        let socket = a.socket().to_path_buf();
        let pid = a.child.id().unwrap();
        // A second name for the same socket, outside the folder, to look at the agent after.
        let peek = dir.path().join("peek.sock");
        std::fs::hard_link(&socket, &peek).unwrap();
        assert!(listed(&peek).unwrap().contains("src"));
        // The worker died: nobody stops it.
        std::mem::forget(a);
        std::fs::write(s.agents.join("notes"), "").unwrap();

        assert_eq!(s.clean_agents().unwrap(), 1);
        assert!(!socket.exists());
        assert!(s.agents.join("notes").exists(), "only sockets are touched");
        let left = listed(&peek).unwrap();
        assert!(left.contains("no identities"), "{left}");
        // SAFETY: kill(2) on the agent this test started.
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        assert_eq!(s.clean_agents().unwrap(), 0);
    }
}
