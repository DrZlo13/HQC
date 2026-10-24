//! Folders this worker has agreed to run sessions in (SPEC §10.2): `$HQC_HOME/known_workspaces.json`,
//! a JSON array of canonical paths.
//!
//! A `session.start` in a folder that is not here, for a session the worker owner neither
//! owns (`owner_is_worker_owner`) nor put the folder in a project for (`requested_by_owner`),
//! waits for the owner's `once` through `worker.approval.request`. A folder gets here when
//! the owner allows it or starts a session in it. The file is local, like the rest of the
//! worker's policy: the hub cannot add to it (SPEC §2.1, §14).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Context;

pub struct KnownPaths {
    file: PathBuf,
    set: Mutex<BTreeSet<PathBuf>>,
}

impl KnownPaths {
    /// Reads the file; a missing one is an empty set, an unreadable one an error rather than
    /// a silently empty set.
    pub fn load(file: &Path) -> anyhow::Result<Self> {
        let set = match std::fs::read(file) {
            Ok(b) => serde_json::from_slice::<BTreeSet<PathBuf>>(&b)
                .with_context(|| format!("reading {}", file.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
        };
        Ok(Self {
            file: file.to_path_buf(),
            set: Mutex::new(set),
        })
    }

    pub fn paths(&self) -> Vec<PathBuf> {
        self.set.lock().unwrap().iter().cloned().collect()
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.set.lock().unwrap().contains(path)
    }

    /// Adds the folder and writes the file (temporary file, then rename; mode 0600).
    pub fn insert(&self, path: &Path) -> anyhow::Result<()> {
        let mut set = self.set.lock().unwrap();
        if !set.insert(path.to_path_buf()) {
            return Ok(());
        }
        let text = serde_json::to_vec_pretty(&*set)?;
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let tmp = self.file.with_extension("json.tmp");
        write_private(&tmp, &text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.file)
            .with_context(|| format!("writing {}", self.file.display()))?;
        Ok(())
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_across_loads() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("known_workspaces.json");
        let k = KnownPaths::load(&file).unwrap();
        assert!(!k.contains(Path::new("/w/a")));
        k.insert(Path::new("/w/a")).unwrap();
        k.insert(Path::new("/w/a")).unwrap();
        assert!(k.contains(Path::new("/w/a")));
        let k = KnownPaths::load(&file).unwrap();
        assert!(k.contains(Path::new("/w/a")));
        assert!(!k.contains(Path::new("/w")));
        std::fs::write(&file, "not json").unwrap();
        assert!(KnownPaths::load(&file).is_err());
    }
}
