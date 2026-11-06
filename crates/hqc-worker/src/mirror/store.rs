//! The mirror's state on disk (SPEC §5.2): per transcript file its working directory (the
//! "file → cwd" cache; the folder name cannot be decoded back, SPEC §9), how far it has been
//! read, the event id of its last prompt (`in_reply_to` of the answers that follow) and its
//! last `ai-title`.
//!
//! SQLite, like the outbox: a handful of keyed rows, crash-safe writes, no format to invent.

use std::path::Path;
use std::sync::Mutex;

use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

pub struct Store {
    db: Mutex<Connection>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileRow {
    /// `None`: no record with `cwd` read yet.
    pub cwd: Option<String>,
    /// Bytes handled: everything before is emitted, the adapter's own or not mirrored.
    pub offset: u64,
    pub last_prompt: Option<Uuid>,
    pub title: Option<String>,
}

impl Store {
    /// Opens (creating) `<dir>/mirror.sqlite3`; the directory is 0700, the file 0600 (it
    /// holds paths and titles of the user's sessions).
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        b.create(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join("mirror.sqlite3");
        let db = Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS files (
                 path TEXT PRIMARY KEY,
                 cwd TEXT,
                 offset INTEGER NOT NULL DEFAULT 0,
                 last_prompt TEXT,
                 title TEXT
             );",
        )
        .context("initializing the mirror state")?;
        Ok(Self { db: Mutex::new(db) })
    }

    /// An in-memory store, for tests and when no state directory is configured.
    pub fn memory() -> Self {
        let db = Connection::open_in_memory().expect("in-memory sqlite");
        db.execute_batch(
            "CREATE TABLE files (path TEXT PRIMARY KEY, cwd TEXT,
                 offset INTEGER NOT NULL DEFAULT 0, last_prompt TEXT, title TEXT);",
        )
        .expect("schema");
        Self { db: Mutex::new(db) }
    }

    pub fn get(&self, path: &Path) -> anyhow::Result<Option<FileRow>> {
        self.db()
            .query_row(
                "SELECT cwd, offset, last_prompt, title FROM files WHERE path = ?1",
                params![path.to_string_lossy()],
                |r| {
                    Ok(FileRow {
                        cwd: r.get(0)?,
                        offset: r.get::<_, i64>(1)? as u64,
                        last_prompt: r.get::<_, Option<String>>(2)?.and_then(|s| s.parse().ok()),
                        title: r.get(3)?,
                    })
                },
            )
            .optional()
            .context("reading the mirror state")
    }

    pub fn put(&self, path: &Path, row: &FileRow) -> anyhow::Result<()> {
        self.db()
            .execute(
                "INSERT INTO files (path, cwd, offset, last_prompt, title) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (path) DO UPDATE SET cwd = ?2, offset = ?3, last_prompt = ?4, title = ?5",
                params![
                    path.to_string_lossy(),
                    row.cwd,
                    row.offset as i64,
                    row.last_prompt.map(|u| u.to_string()),
                    row.title
                ],
            )
            .context("writing the mirror state")?;
        Ok(())
    }

    pub fn remove(&self, path: &Path) -> anyhow::Result<()> {
        self.db()
            .execute(
                "DELETE FROM files WHERE path = ?1",
                params![path.to_string_lossy()],
            )
            .context("writing the mirror state")?;
        Ok(())
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let p = Path::new("/c/projects/-a/x.jsonl");
        let row = FileRow {
            cwd: Some("/a".into()),
            offset: 42,
            last_prompt: Some(Uuid::nil()),
            title: Some("t".into()),
        };
        Store::open(dir.path()).unwrap().put(p, &row).unwrap();
        let s = Store::open(dir.path()).unwrap();
        assert_eq!(s.get(p).unwrap(), Some(row));
        assert_eq!(s.get(Path::new("/other")).unwrap(), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("mirror.sqlite3"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
