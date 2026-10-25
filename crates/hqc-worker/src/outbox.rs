//! On-disk outbox for worker -> hub events (SPEC §5.2).
//!
//! An event is written here before it goes to the socket and deleted when the hub acks its
//! `event_id`; after a reconnect or a restart everything left is sent again in insertion
//! order. `agent.delta` never comes here (SPEC §5.2).
//!
//! Storage: SQLite (`rusqlite`, bundled) rather than a hand-made append log with an index.
//! The operations are exactly "append, delete by key, scan in order", which SQLite gives
//! with crash-safe transactions, a unique index on `event_id` (a re-emitted event is not
//! stored twice) and no compaction to write; the volume is small (events of a few
//! sessions), so the cost of a synchronous commit per event does not matter. Access is
//! synchronous under a mutex: each call is one short local transaction.
//!
//! `attempts` counts retryable `error` replies of the hub to an event; the connection drops
//! an event after [`crate::connection::MAX_EVENT_ATTEMPTS`] of them (SPEC §5.2).

use std::path::Path;
use std::sync::Mutex;

use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

pub struct Outbox {
    db: Mutex<Connection>,
}

/// An event waiting for its ack. `seq` is the local insertion order, not the hub's `seq`.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub seq: i64,
    pub event_id: Uuid,
    /// The event as JSON, sent as `event {event}`; for a `mirror.event` its whole payload.
    pub event: serde_json::Value,
    /// [`FRAME_EVENT`], [`FRAME_MIRROR_EVENT`], [`FRAME_SESSION_STATE`] or [`FRAME_MCP_CALL`].
    pub frame: String,
}

pub const FRAME_EVENT: &str = "event";
/// `mirror.event` (SPEC §5.4): through the outbox like `event`, keyed by the event's id.
pub const FRAME_MIRROR_EVENT: &str = "mirror.event";
/// A holder report, `session.state` with the nil `session_id` (SPEC §5.4, §9.3): through the
/// outbox so that it reaches the hub in order with the `mirror.event`s, keyed by a fresh id.
pub const FRAME_SESSION_STATE: &str = "session.state";
/// `mcp.call` (SPEC §5.2, §7.2), keyed by its `request_id`. Stored without `session_token`:
/// the token is never written to disk; the connection adds it when sending, and drops a
/// call that is no longer open.
pub const FRAME_MCP_CALL: &str = "mcp.call";

impl Outbox {
    /// Opens (creating if needed) `<dir>/outbox.sqlite3`. The directory is 0700 and the
    /// database 0600: it holds agent messages and tool previews. SQLite gives its `-wal` and
    /// `-shm` files the database's mode.
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        private_dir(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join("outbox.sqlite3");
        private_file(&path).with_context(|| format!("creating {}", path.display()))?;
        let db = Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             CREATE TABLE IF NOT EXISTS outbox (
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,
                 event_id TEXT NOT NULL UNIQUE,
                 event TEXT NOT NULL,
                 attempts INTEGER NOT NULL DEFAULT 0
             );",
        )
        .context("initializing the outbox")?;
        // Outboxes of workers before T10 have no `attempts`.
        let has_attempts: bool = db
            .query_row(
                "SELECT count(*) > 0 FROM pragma_table_info('outbox') WHERE name = 'attempts'",
                [],
                |r| r.get(0),
            )
            .context("reading the outbox schema")?;
        if !has_attempts {
            db.execute_batch("ALTER TABLE outbox ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0")
                .context("adding outbox.attempts")?;
        }
        // Outboxes before T15 hold `event` frames only.
        let has_frame: bool = db
            .query_row(
                "SELECT count(*) > 0 FROM pragma_table_info('outbox') WHERE name = 'frame'",
                [],
                |r| r.get(0),
            )
            .context("reading the outbox schema")?;
        if !has_frame {
            db.execute_batch("ALTER TABLE outbox ADD COLUMN frame TEXT NOT NULL DEFAULT 'event'")
                .context("adding outbox.frame")?;
        }
        Ok(Self { db: Mutex::new(db) })
    }

    /// Appends an event; an `event_id` already waiting is kept as is.
    pub fn push(&self, event_id: Uuid, event: &serde_json::Value) -> anyhow::Result<()> {
        self.push_frame(event_id, FRAME_EVENT, event)
    }

    /// Appends a frame carrying event `event_id`: `payload` is the event for
    /// [`FRAME_EVENT`], the frame's payload otherwise. The same `event_id` waiting already
    /// (the adapter's copy of a mirrored record) is kept as is.
    pub fn push_frame(
        &self,
        event_id: Uuid,
        frame: &str,
        payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        self.db()
            .execute(
                "INSERT OR IGNORE INTO outbox (event_id, event, frame) VALUES (?1, ?2, ?3)",
                params![event_id.to_string(), payload.to_string(), frame],
            )
            .context("writing to the outbox")?;
        Ok(())
    }

    /// Deletes the event acked by the hub; returns whether it was there.
    pub fn remove(&self, event_id: Uuid) -> anyhow::Result<bool> {
        let n = self
            .db()
            .execute(
                "DELETE FROM outbox WHERE event_id = ?1",
                params![event_id.to_string()],
            )
            .context("deleting from the outbox")?;
        Ok(n > 0)
    }

    /// Deletes `event_id` if it is a [`FRAME_MIRROR_EVENT`]; returns its transcript id.
    pub fn take_mirror(&self, event_id: Uuid) -> anyhow::Result<Option<String>> {
        self.db()
            .query_row(
                "DELETE FROM outbox WHERE event_id = ?1 AND frame = ?2
                 RETURNING json_extract(event, '$.runtime_session_id')",
                params![event_id.to_string(), FRAME_MIRROR_EVENT],
                |r| r.get(0),
            )
            .optional()
            .context("deleting a mirrored event")
    }

    /// Counts one more failed send of `event_id`; returns the count, `None` if the event is
    /// not there.
    pub fn failed(&self, event_id: Uuid) -> anyhow::Result<Option<u32>> {
        self.db()
            .query_row(
                "UPDATE outbox SET attempts = attempts + 1 WHERE event_id = ?1 RETURNING attempts",
                params![event_id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .context("counting a failed send")
    }

    /// Entries with local `seq > after`, in order.
    pub fn after(&self, after: i64) -> anyhow::Result<Vec<Entry>> {
        let db = self.db();
        let mut st = db.prepare_cached(
            "SELECT seq, event_id, event, frame FROM outbox WHERE seq > ?1 ORDER BY seq",
        )?;
        let rows = st.query_map(params![after], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|r| {
            let (seq, id, ev, frame) = r?;
            Ok(Entry {
                seq,
                event_id: id.parse().context("outbox event_id")?,
                event: serde_json::from_str(&ev).context("outbox event")?,
                frame,
            })
        })
        .collect()
    }

    pub fn len(&self) -> anyhow::Result<usize> {
        let n: i64 = self
            .db()
            .query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    pub fn is_empty(&self) -> anyhow::Result<bool> {
        Ok(self.len()? == 0)
    }

    pub fn contains(&self, event_id: Uuid) -> anyhow::Result<bool> {
        Ok(self
            .db()
            .query_row(
                "SELECT 1 FROM outbox WHERE event_id = ?1",
                params![event_id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn private_dir(dir: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Creates `path` 0600 if missing; tightens an existing one (older workers made it 0644).
fn private_file(path: &Path) -> std::io::Result<()> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in [
            path.to_path_buf(),
            path.with_extension("sqlite3-wal"),
            path.with_extension("sqlite3-shm"),
        ] {
            if p.exists() {
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(unix)]
    #[test]
    fn private_modes() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("outbox");
        let ob = Outbox::open(&dir).unwrap();
        ob.push(Uuid::now_v7(), &json!({})).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        for f in std::fs::read_dir(&dir).unwrap() {
            let p = f.unwrap().path();
            assert_eq!(mode(&p), 0o600, "{}", p.display());
        }
        // An outbox of an older worker is tightened on open.
        drop(ob);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let db = dir.join("outbox.sqlite3");
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).unwrap();
        Outbox::open(&dir).unwrap();
        assert_eq!((mode(&dir), mode(&db)), (0o700, 0o600));
    }

    #[test]
    fn order_ack_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let ids: Vec<Uuid> = (0..4).map(|_| Uuid::now_v7()).collect();
        {
            let ob = Outbox::open(dir.path()).unwrap();
            for (i, id) in ids.iter().enumerate() {
                ob.push(*id, &json!({ "n": i })).unwrap();
            }
            ob.push(ids[0], &json!({ "n": 99 })).unwrap();
            assert_eq!(ob.len().unwrap(), 4);
            assert!(ob.remove(ids[1]).unwrap());
            assert!(!ob.remove(ids[1]).unwrap());
        }
        let ob = Outbox::open(dir.path()).unwrap();
        let all = ob.after(0).unwrap();
        let got: Vec<Uuid> = all.iter().map(|e| e.event_id).collect();
        assert_eq!(got, [ids[0], ids[2], ids[3]]);
        assert_eq!(all[0].event, json!({ "n": 0 }));
        assert_eq!(ob.after(all[1].seq).unwrap().len(), 1);
        assert!(ob.contains(ids[3]).unwrap());
        assert_eq!(ob.failed(ids[3]).unwrap(), Some(1));
        assert_eq!(ob.failed(ids[3]).unwrap(), Some(2));
        assert_eq!(ob.failed(ids[1]).unwrap(), None);
    }

    #[test]
    fn old_outbox_gets_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::now_v7();
        {
            let db = Connection::open(dir.path().join("outbox.sqlite3")).unwrap();
            db.execute_batch(
                "CREATE TABLE outbox (seq INTEGER PRIMARY KEY AUTOINCREMENT,
                     event_id TEXT NOT NULL UNIQUE, event TEXT NOT NULL);",
            )
            .unwrap();
            db.execute(
                "INSERT INTO outbox (event_id, event) VALUES (?1, '{}')",
                params![id.to_string()],
            )
            .unwrap();
        }
        let ob = Outbox::open(dir.path()).unwrap();
        assert_eq!(ob.failed(id).unwrap(), Some(1));
        drop(ob);
        Outbox::open(dir.path()).unwrap();
    }
}
