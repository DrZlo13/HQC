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
    /// The event as JSON, sent as `event {event}`.
    pub event: serde_json::Value,
}

impl Outbox {
    /// Opens (creating if needed) `<dir>/outbox.sqlite3`.
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join("outbox.sqlite3");
        let db = Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             CREATE TABLE IF NOT EXISTS outbox (
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,
                 event_id TEXT NOT NULL UNIQUE,
                 event TEXT NOT NULL
             );",
        )
        .context("initializing the outbox")?;
        Ok(Self { db: Mutex::new(db) })
    }

    /// Appends an event; an `event_id` already waiting is kept as is.
    pub fn push(&self, event_id: Uuid, event: &serde_json::Value) -> anyhow::Result<()> {
        self.db()
            .execute(
                "INSERT OR IGNORE INTO outbox (event_id, event) VALUES (?1, ?2)",
                params![event_id.to_string(), event.to_string()],
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

    /// Entries with local `seq > after`, in order.
    pub fn after(&self, after: i64) -> anyhow::Result<Vec<Entry>> {
        let db = self.db();
        let mut st = db.prepare_cached(
            "SELECT seq, event_id, event FROM outbox WHERE seq > ?1 ORDER BY seq",
        )?;
        let rows = st.query_map(params![after], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|r| {
            let (seq, id, ev) = r?;
            Ok(Entry {
                seq,
                event_id: id.parse().context("outbox event_id")?,
                event: serde_json::from_str(&ev).context("outbox event")?,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    }
}
