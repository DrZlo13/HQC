//! Which transcript records the worker's own Claude Code processes wrote (SPEC §9.3).
//!
//! The adapter adds the `uuid` of every main-chain `user`/`assistant` message it reads from
//! stream-json, the `--replay-user-messages` echo of its own prompts included; stream-json
//! and the transcript share these uuids (checked on 2.1.307 and 2.1.308). The mirror asks
//! the registry whether a record is the adapter's and whether a process of the session runs
//! right now. Keyed by `runtime_session_id` (the transcript's uuid); in memory only: after a
//! worker restart the mirror's offsets on disk say what was already handled.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use uuid::Uuid;

#[derive(Debug, Default)]
pub struct OwnRecords {
    inner: Mutex<HashMap<Uuid, Entry>>,
}

#[derive(Debug, Default)]
struct Entry {
    uuids: HashSet<Uuid>,
    /// Processes of the session alive now (a new one may start before the old one's
    /// monitor has finished).
    live: u32,
}

impl OwnRecords {
    /// A process for transcript `session` was spawned.
    pub fn begin(&self, session: Uuid) {
        self.lock().entry(session).or_default().live += 1;
    }

    /// That process is gone.
    pub fn end(&self, session: Uuid) {
        if let Some(e) = self.lock().get_mut(&session) {
            e.live = e.live.saturating_sub(1);
        }
    }

    /// The adapter saw record `uuid` of transcript `session` in stream-json.
    pub fn add(&self, session: Uuid, uuid: Uuid) {
        self.lock().entry(session).or_default().uuids.insert(uuid);
    }

    pub fn is_own(&self, session: Uuid, uuid: Uuid) -> bool {
        self.lock()
            .get(&session)
            .is_some_and(|e| e.uuids.contains(&uuid))
    }

    /// A process of this worker writes the transcript now (`locked_by: hqc`).
    pub fn is_live(&self, session: Uuid) -> bool {
        self.lock().get(&session).is_some_and(|e| e.live > 0)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Entry>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_and_own() {
        let r = OwnRecords::default();
        let (s, u) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(!r.is_live(s) && !r.is_own(s, u));
        r.begin(s);
        r.begin(s);
        r.add(s, u);
        r.end(s);
        assert!(r.is_live(s), "the second process still runs");
        r.end(s);
        r.end(s);
        assert!(!r.is_live(s));
        assert!(r.is_own(s, u), "own uuids outlive the process");
    }
}
