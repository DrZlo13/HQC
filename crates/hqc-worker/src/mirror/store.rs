//! The mirror's state on disk (SPEC §5.2): per transcript file its working directory (the
//! "file → cwd" cache; the folder name cannot be decoded back, SPEC §9), how far it has been
//! read, the event id of its last prompt (`in_reply_to` of the answers that follow), its
//! last `ai-title` and its last `custom-title`.
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
    /// The last `custom-title`, a person's: shown instead of `title` ([`FileRow::shown_title`]).
    pub custom_title: Option<String>,
}

impl FileRow {
    /// The session's title: `custom_title` wins over `title` (SPEC §9.2).
    pub fn shown_title(&self) -> Option<String> {
        self.custom_title.clone().or_else(|| self.title.clone())
    }
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
                 title TEXT,
                 custom_title TEXT
             );",
        )
        .context("initializing the mirror state")?;
        // A state file from before SPEC 0.162 has no `custom_title`.
        let has: bool = db
            .query_row(
                "SELECT count(*) FROM pragma_table_info('files') WHERE name = 'custom_title'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .context("reading the mirror state")?;
        if !has {
            db.execute_batch("ALTER TABLE files ADD COLUMN custom_title TEXT;")
                .context("initializing the mirror state")?;
        }
        Ok(Self { db: Mutex::new(db) })
    }

    /// An in-memory store, for tests and when no state directory is configured.
    pub fn memory() -> Self {
        let db = Connection::open_in_memory().expect("in-memory sqlite");
        db.execute_batch(
            "CREATE TABLE files (path TEXT PRIMARY KEY, cwd TEXT,
                 offset INTEGER NOT NULL DEFAULT 0, last_prompt TEXT, title TEXT, custom_title TEXT);",
        )
        .expect("schema");
        Self { db: Mutex::new(db) }
    }

    pub fn get(&self, path: &Path) -> anyhow::Result<Option<FileRow>> {
        self.db()
            .query_row(
                "SELECT cwd, offset, last_prompt, title, custom_title FROM files WHERE path = ?1",
                params![path.to_string_lossy()],
                |r| {
                    Ok(FileRow {
                        cwd: r.get(0)?,
                        offset: r.get::<_, i64>(1)? as u64,
                        last_prompt: r.get::<_, Option<String>>(2)?.and_then(|s| s.parse().ok()),
                        title: r.get(3)?,
                        custom_title: r.get(4)?,
                    })
                },
            )
            .optional()
            .context("reading the mirror state")
    }

    pub fn put(&self, path: &Path, row: &FileRow) -> anyhow::Result<()> {
        self.db()
            .execute(
                "INSERT INTO files (path, cwd, offset, last_prompt, title, custom_title)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (path) DO UPDATE SET cwd = ?2, offset = ?3, last_prompt = ?4,
                 title = ?5, custom_title = ?6",
                params![
                    path.to_string_lossy(),
                    row.cwd,
                    row.offset as i64,
                    row.last_prompt.map(|u| u.to_string()),
                    row.title,
                    row.custom_title
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
            custom_title: Some("c".into()),
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

    /// A state file written before SPEC 0.162 has no `custom_title`: it opens with its rows and
    /// stores one from then on.
    #[test]
    fn a_state_file_without_custom_titles_opens() {
        let dir = tempfile::tempdir().unwrap();
        let p = Path::new("/c/projects/-a/x.jsonl");
        Connection::open(dir.path().join("mirror.sqlite3"))
            .unwrap()
            .execute_batch(
                "CREATE TABLE files (path TEXT PRIMARY KEY, cwd TEXT,
                     offset INTEGER NOT NULL DEFAULT 0, last_prompt TEXT, title TEXT);
                 INSERT INTO files (path, cwd, offset, title)
                     VALUES ('/c/projects/-a/x.jsonl', '/a', 7, 't');",
            )
            .unwrap();
        let s = Store::open(dir.path()).unwrap();
        let mut row = s.get(p).unwrap().unwrap();
        assert_eq!(
            (
                row.offset,
                row.title.as_deref(),
                row.custom_title.as_deref()
            ),
            (7, Some("t"), None)
        );
        row.custom_title = Some("c".into());
        s.put(p, &row).unwrap();
        assert_eq!(Store::open(dir.path()).unwrap().get(p).unwrap(), Some(row));
    }
}
