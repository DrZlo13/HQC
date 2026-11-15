//! Secrets and ssh keys given to a session from the chat (SPEC §14, since 0.147).
//!
//! - Only with `session_secrets = true` in `worker.toml`, for a session of the worker's owner
//!   that is not restricted ([`crate::config::Config::check_start`]); the hub cannot turn it on.
//! - A secret is a file `$HQC_SECRETS/<name>` (0600) of one agent process: the folder
//!   `$HQC_HOME/secrets/<id>/` (0700) is made when the process starts and removed when it
//!   ends ([`ProcessSecrets`]); a worker that died leaves folders behind, and its next start
//!   removes them ([`Stores::clean`]). The agent is told the path, never the value.
//! - Such a process always has an ssh-agent of its own (empty if nothing is to be loaded), so
//!   a key given in the middle of a conversation is added to it ([`crate::ssh::SshAgent::add`]).
//! - A key kept for a workspace is in `$HQC_HOME/workspace-keys/<hash of the canonical
//!   path>/<name>` ([`WorkspaceKeys`]), apart from the store of `ssh_keys`: through the hub
//!   one adds only a new key to one's own workspace, and never binds a key of that store to a
//!   folder by its name. Every agent process of a session in that workspace loads them.
//! - A key is read and decrypted here (the `ssh-key` crate): a passphrase never goes on a
//!   command line, and a kept key is stored without it (at a start there is no one to ask).
//! - Values, keys and passphrases are never in a log line, an error or a frame but
//!   `secret.put`.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use hqc_proto::frame::WorkspaceKey;
use sha2::{Digest, Sha256};
use ssh_key::{HashAlg, LineEnding, PrivateKey};

/// The variable of an agent process with secrets: its folder.
pub const SECRETS_ENV: &str = "HQC_SECRETS";
/// In the hqc MCP server's environment: it lists [`hqc_proto::mcp::SECRET_TOOLS`].
pub const SECRET_TOOLS_ENV: &str = "HQC_SECRET_TOOLS";
/// The file in a workspace's key folder that names the workspace.
const PATH_FILE: &str = ".path";

/// `$HQC_HOME/secrets` and `$HQC_HOME/workspace-keys`.
#[derive(Debug, Clone, PartialEq)]
pub struct Stores {
    pub secrets: PathBuf,
    pub workspace_keys: PathBuf,
}

/// Make `dir` (0700) and its parents if missing, and check it is the user's own and closed
/// to others.
fn private_dir(dir: &Path) -> anyhow::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    crate::ssh::private(dir, true)
}

/// Write `data` to `path` (0600) through a temporary file and a rename.
fn write_private(path: &Path, data: &[u8], replace: bool) -> anyhow::Result<()> {
    if !replace && path.exists() {
        bail!("{} exists already", path.display());
    }
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    let r = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r.with_context(|| format!("writing {}", path.display()))
}

impl Stores {
    pub fn in_home(home: &Path) -> Self {
        Self {
            secrets: home.join("secrets"),
            workspace_keys: home.join("workspace-keys"),
        }
    }

    /// At the worker's start, before any session: the folders of processes a worker that
    /// died left behind are removed, and both stores are the user's own and closed to others.
    /// Returns how many folders went.
    pub fn clean(&self) -> anyhow::Result<usize> {
        private_dir(&self.secrets)?;
        if self.workspace_keys.exists() {
            crate::ssh::private(&self.workspace_keys, true)?;
        }
        let mut n = 0;
        for e in std::fs::read_dir(&self.secrets)
            .with_context(|| format!("reading {}", self.secrets.display()))?
        {
            let path = e?.path();
            let m = std::fs::symlink_metadata(&path)?;
            if m.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            }
            .with_context(|| format!("removing {}", path.display()))?;
            n += 1;
        }
        Ok(n)
    }

    /// A folder for the secrets of one agent process, removed when it is dropped.
    pub fn process(&self, socket: PathBuf) -> anyhow::Result<ProcessSecrets> {
        private_dir(&self.secrets)?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let dir = self.secrets.join(&id[..16]);
        private_dir(&dir)?;
        Ok(ProcessSecrets { dir, socket })
    }

    /// Every workspace with kept keys, by the path it names, and its keys (`hqc-worker env`).
    pub fn all(&self) -> anyhow::Result<Vec<(String, Vec<WorkspaceKey>)>> {
        if !self.workspace_keys.exists() {
            return Ok(vec![]);
        }
        crate::ssh::private(&self.workspace_keys, true)?;
        let mut out = vec![];
        for e in std::fs::read_dir(&self.workspace_keys)
            .with_context(|| format!("reading {}", self.workspace_keys.display()))?
        {
            let dir = e?.path();
            let Ok(path) = std::fs::read_to_string(dir.join(PATH_FILE)) else {
                continue;
            };
            let keys = WorkspaceKeys {
                dir,
                path: PathBuf::from(&path),
            }
            .list()?;
            if !keys.is_empty() {
                out.push((path, keys));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// The keys kept for the workspace at canonical `path`.
    pub fn workspace(&self, path: &Path) -> WorkspaceKeys {
        let hash = Sha256::digest(path.as_os_str().as_encoded_bytes());
        let hex: String = hash[..16].iter().map(|b| format!("{b:02x}")).collect();
        WorkspaceKeys {
            dir: self.workspace_keys.join(hex),
            path: path.to_path_buf(),
        }
    }
}

/// The secrets folder and the ssh-agent socket of one agent process. Dropping it removes the
/// folder; the agent itself goes with its [`crate::ssh::SshAgent`].
#[derive(Debug)]
pub struct ProcessSecrets {
    dir: PathBuf,
    socket: PathBuf,
}

impl ProcessSecrets {
    /// What the process gets as `HQC_SECRETS`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The process's ssh-agent.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The secret `name` with `value`, replacing an earlier one of that name. Returns the
    /// path as the agent writes it.
    pub fn put(&self, name: &str, value: &str) -> anyhow::Result<String> {
        check_name(name)?;
        write_private(&self.dir.join(name), value.as_bytes(), true)?;
        Ok(format!("${SECRETS_ENV}/{name}"))
    }

    /// Add `key` to the process's ssh-agent, from a 0600 file in the process's folder that is
    /// removed at once.
    pub async fn add_key(&self, key: &PrivateKey) -> anyhow::Result<()> {
        let text = key
            .to_openssh(LineEnding::LF)
            .context("writing the key out")?;
        let file = self
            .dir
            .join(format!(".key-{}", uuid::Uuid::new_v4().simple()));
        write_private(&file, text.as_bytes(), false)?;
        let r = crate::ssh::ssh_add(&self.socket, std::slice::from_ref(&file)).await;
        let _ = std::fs::remove_file(&file);
        r
    }
}

impl Drop for ProcessSecrets {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn check_name(name: &str) -> anyhow::Result<()> {
    if !crate::ssh::valid_key_name(name) {
        bail!("{name:?} is not a name: 1 to 64 letters, digits, '_', '-' and '.', not starting with '.' or '-'");
    }
    Ok(())
}

/// A private key as the user gave it (OpenSSH format), decrypted with `passphrase` if it has
/// one. The error never carries the key or the passphrase.
pub fn read_key(text: &str, passphrase: Option<&str>) -> anyhow::Result<PrivateKey> {
    let Ok(key) = PrivateKey::from_openssh(text.trim()) else {
        bail!(
            "not a private key in OpenSSH format (-----BEGIN OPENSSH PRIVATE KEY-----); \
             an older PEM key converts with `ssh-keygen -p -f <file>`"
        );
    };
    if !key.is_encrypted() {
        return Ok(key);
    }
    let Some(pass) = passphrase.filter(|p| !p.is_empty()) else {
        bail!("the key has a passphrase: give it too");
    };
    key.decrypt(pass)
        .map_err(|_| anyhow::anyhow!("the passphrase does not decrypt the key"))
}

/// A new ed25519 key with `comment`.
pub fn generate(comment: &str) -> anyhow::Result<PrivateKey> {
    let mut key = PrivateKey::random(&mut ssh_key::rand_core::OsRng, ssh_key::Algorithm::Ed25519)
        .context("making an ed25519 key")?;
    key.set_comment(comment);
    Ok(key)
}

/// `SHA256:…` of a key.
pub fn fingerprint(key: &PrivateKey) -> String {
    key.public_key().fingerprint(HashAlg::Sha256).to_string()
}

/// The public half as one OpenSSH line.
pub fn public_line(key: &PrivateKey) -> anyhow::Result<String> {
    key.public_key()
        .to_openssh()
        .context("writing the public key out")
}

/// The keys kept for one workspace.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceKeys {
    dir: PathBuf,
    path: PathBuf,
}

impl WorkspaceKeys {
    /// The key files, by name; none when the workspace has no folder here.
    pub fn files(&self) -> anyhow::Result<Vec<(String, PathBuf)>> {
        if !self.dir.exists() {
            return Ok(vec![]);
        }
        crate::ssh::private(&self.dir, true)?;
        let mut out = vec![];
        for e in std::fs::read_dir(&self.dir)
            .with_context(|| format!("reading {}", self.dir.display()))?
        {
            let e = e?;
            let Some(name) = e.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !crate::ssh::valid_key_name(&name) {
                continue;
            }
            let path = e.path();
            crate::ssh::private(&path, false)?;
            out.push((name, path));
        }
        out.sort();
        Ok(out)
    }

    /// Names and fingerprints.
    pub fn list(&self) -> anyhow::Result<Vec<WorkspaceKey>> {
        let mut out = vec![];
        for (name, path) in self.files()? {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let fingerprint = match PrivateKey::from_openssh(&text) {
                Ok(k) => fingerprint(&k),
                Err(_) => "unreadable".into(),
            };
            out.push(WorkspaceKey { name, fingerprint });
        }
        Ok(out)
    }

    /// Keep `key` as `name`; a name the workspace has already is refused.
    pub fn add(&self, name: &str, key: &PrivateKey) -> anyhow::Result<()> {
        check_name(name)?;
        private_dir(&self.dir)?;
        let marker = self.dir.join(PATH_FILE);
        if !marker.exists() {
            write_private(&marker, self.path.as_os_str().as_encoded_bytes(), true)?;
        }
        let dest = self.dir.join(name);
        if dest.exists() {
            bail!("this workspace has an ssh key named {name:?} already");
        }
        let text = key
            .to_openssh(LineEnding::LF)
            .context("writing the key out")?;
        write_private(&dest, text.as_bytes(), false)
    }

    /// Forget `name`; `false` if the workspace has no such key.
    pub fn delete(&self, name: &str) -> anyhow::Result<bool> {
        check_name(name)?;
        let dest = self.dir.join(name);
        match std::fs::remove_file(&dest) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(anyhow::Error::new(e).context(format!("removing {}", dest.display()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stores() -> (tempfile::TempDir, Stores) {
        let dir = tempfile::Builder::new()
            .prefix("hqc-sec-")
            .tempdir_in("/tmp")
            .unwrap();
        let s = Stores::in_home(&dir.path().canonicalize().unwrap());
        (dir, s)
    }

    fn mode(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_secret_is_a_private_file_gone_with_the_process() {
        let (_d, s) = stores();
        let p = s.process(PathBuf::from("/nonexistent.sock")).unwrap();
        let dir = p.dir().to_path_buf();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(
            p.put("WIFI_PASS", "hunter2").unwrap(),
            "$HQC_SECRETS/WIFI_PASS"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("WIFI_PASS")).unwrap(),
            "hunter2"
        );
        assert_eq!(mode(&dir.join("WIFI_PASS")), 0o600);
        // Given again: replaced.
        p.put("WIFI_PASS", "other").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("WIFI_PASS")).unwrap(),
            "other"
        );
        for bad in ["", ".x", "-x", "a/b", "../x", "a b"] {
            assert!(p.put(bad, "v").is_err(), "{bad:?}");
        }
        drop(p);
        assert!(!dir.exists());
    }

    #[test]
    fn a_dead_workers_folders_are_removed_at_start() {
        let (_d, s) = stores();
        let p = s.process(PathBuf::from("/x.sock")).unwrap();
        p.put("A", "v").unwrap();
        let dir = p.dir().to_path_buf();
        std::mem::forget(p);
        assert_eq!(s.clean().unwrap(), 1);
        assert!(!dir.exists());
        assert_eq!(s.clean().unwrap(), 0);
    }

    #[test]
    fn keys_are_read_decrypted_and_kept_per_workspace() {
        let (_d, s) = stores();
        let key = generate("hqc pi").unwrap();
        let fp = fingerprint(&key);
        assert!(fp.starts_with("SHA256:"));
        assert!(public_line(&key).unwrap().starts_with("ssh-ed25519 "));
        // A key with a passphrase: refused without it or with a wrong one.
        let enc = key
            .encrypt(&mut ssh_key::rand_core::OsRng, "pass")
            .unwrap()
            .to_openssh(LineEnding::LF)
            .unwrap();
        let e = read_key(&enc, None).unwrap_err().to_string();
        assert!(e.contains("passphrase"), "{e}");
        let e = read_key(&enc, Some("nope")).unwrap_err().to_string();
        assert!(!e.contains("nope") && !e.contains("OPENSSH"), "{e}");
        let dec = read_key(&enc, Some("pass")).unwrap();
        assert_eq!(fingerprint(&dec), fp);
        assert!(read_key("not a key", None).is_err());

        let ws = s.workspace(Path::new("/Users/me/work/pi"));
        let other = s.workspace(Path::new("/Users/me/work/router"));
        assert!(ws.list().unwrap().is_empty());
        ws.add("pi-lab", &dec).unwrap();
        assert!(ws.add("pi-lab", &dec).is_err(), "a name is not replaced");
        let listed = ws.list().unwrap();
        assert_eq!(
            listed,
            vec![WorkspaceKey {
                name: "pi-lab".into(),
                fingerprint: fp.clone()
            }]
        );
        assert!(
            other.list().unwrap().is_empty(),
            "another workspace has none"
        );
        // Kept without the passphrase, 0600.
        let (_, file) = &ws.files().unwrap()[0];
        assert_eq!(mode(file), 0o600);
        let kept = std::fs::read_to_string(file).unwrap();
        assert!(!PrivateKey::from_openssh(&kept).unwrap().is_encrypted());
        assert!(ws.delete("pi-lab").unwrap());
        assert!(!ws.delete("pi-lab").unwrap());
        assert!(ws.list().unwrap().is_empty());
    }
}
