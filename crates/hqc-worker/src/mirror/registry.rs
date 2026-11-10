//! Which transcript records the worker's own Claude Code processes wrote (SPEC §9.3).
//!
//! The adapter adds the `uuid` of every main-chain `user`/`assistant` message it reads from
//! stream-json, the `--replay-user-messages` echo of its own prompts included; stream-json
//! and the transcript share these uuids (checked on 2.1.307 and 2.1.308, after `--resume`
//! too). It also adds the text of every input it writes to stdin: a prompt record the stream
//! did not name (a slash command such as `/compact` is written as `<command-name>`, T25; an
//! echo with another uuid, seen in the field and not reproduced on 2.1.308) is the adapter's
//! when its typed text matches an input not yet matched ([`OwnRecords::claim`]), or several
//! inputs in a row joined by a newline: inputs the CLI took from its queue together are one
//! prompt record with a new uuid (2.1.312, a message typed while a fresh process started). The
//! mirror asks the registry whether a record is the adapter's and whether a process of the
//! session runs right now. Keyed by `runtime_session_id` (the transcript's uuid); in memory only:
//! after a worker restart the mirror's offsets on disk say what was already handled.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::sync::Mutex;

use uuid::Uuid;

/// Inputs kept per transcript until a record matches them: older ones are dropped.
const INPUTS: usize = 32;

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
    /// Their pids, when known: their entries in Claude Code's session registry are not
    /// another holder (SPEC §9.3).
    pids: Vec<u32>,
    /// Texts of the inputs written to stdin (trimmed) that no record has matched yet.
    inputs: VecDeque<String>,
}

impl OwnRecords {
    /// A process for transcript `session` was spawned.
    pub fn begin(&self, session: Uuid, pid: Option<u32>) {
        let mut m = self.lock();
        let e = m.entry(session).or_default();
        e.live += 1;
        e.pids.extend(pid);
    }

    /// That process is gone.
    pub fn end(&self, session: Uuid, pid: Option<u32>) {
        if let Some(e) = self.lock().get_mut(&session) {
            e.live = e.live.saturating_sub(1);
            if let Some(p) = pid {
                e.pids.retain(|x| *x != p);
            }
        }
    }

    /// Pids of the worker's live agent processes.
    pub fn pids(&self) -> std::collections::HashSet<u32> {
        self.lock()
            .values()
            .flat_map(|e| e.pids.iter().copied())
            .collect()
    }

    /// The adapter saw record `uuid` of transcript `session` in stream-json.
    pub fn add(&self, session: Uuid, uuid: Uuid) {
        self.lock().entry(session).or_default().uuids.insert(uuid);
    }

    /// The adapter wrote input `text` to the stdin of transcript `session`'s process.
    pub fn sent(&self, session: Uuid, text: &str) {
        let mut m = self.lock();
        let e = m.entry(session).or_default();
        e.inputs.push_back(text.trim().to_string());
        if e.inputs.len() > INPUTS {
            e.inputs.pop_front();
        }
    }

    /// Whether record `uuid` of transcript `session` is the adapter's: its uuid came in
    /// stream-json, or `typed` (what a prompt record stands for, `None` for other records)
    /// matches an input not yet matched, or several in a row joined by a newline; then the
    /// record is the adapter's from now on. Matching inputs are used up either way.
    pub fn claim(&self, session: Uuid, uuid: Uuid, typed: Option<&str>) -> bool {
        let mut m = self.lock();
        let Some(e) = m.get_mut(&session) else {
            return false;
        };
        if let Some(r) = typed.and_then(|t| matching(&e.inputs, t.trim())) {
            e.inputs.drain(r);
            e.uuids.insert(uuid);
            return true;
        }
        e.uuids.contains(&uuid)
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

/// The inputs `t` stands for: one input, else the first run of inputs in a row whose texts
/// joined by a newline are `t`.
fn matching(inputs: &VecDeque<String>, t: &str) -> Option<Range<usize>> {
    if let Some(i) = inputs.iter().position(|i| i == t) {
        return Some(i..i + 1);
    }
    (0..inputs.len()).find_map(|start| {
        let mut joined = String::new();
        for (j, input) in inputs.iter().enumerate().skip(start) {
            if j > start {
                joined.push('\n');
            }
            joined.push_str(input);
            if !t.starts_with(joined.as_str()) {
                return None;
            }
            if j > start && joined.len() == t.len() {
                return Some(start..j + 1);
            }
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_and_own() {
        let r = OwnRecords::default();
        let (s, u) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(!r.is_live(s) && !r.is_own(s, u));
        r.begin(s, Some(10));
        r.begin(s, Some(11));
        r.add(s, u);
        assert_eq!(r.pids(), [10, 11].into());
        r.end(s, Some(10));
        assert!(r.is_live(s), "the second process still runs");
        assert_eq!(r.pids(), [11].into());
        r.end(s, Some(11));
        r.end(s, None);
        assert!(!r.is_live(s));
        assert!(r.pids().is_empty());
        assert!(r.is_own(s, u), "own uuids outlive the process");
    }

    /// T25: a prompt record whose uuid the stream did not name is claimed by its text, once.
    #[test]
    fn claim_by_text() {
        let r = OwnRecords::default();
        let s = Uuid::new_v4();
        let (echoed, other, again) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        assert!(!r.claim(s, other, Some("x")), "unknown session");
        r.sent(s, " Reply with exactly: TWO\n");
        r.sent(s, "/compact");
        r.add(s, echoed);
        // The echoed record uses up its input.
        assert!(r.claim(s, echoed, Some("Reply with exactly: TWO")));
        assert!(!r.claim(s, other, Some("Reply with exactly: TWO")));
        assert!(r.claim(s, other, Some("/compact")));
        assert!(r.is_own(s, other));
        assert!(!r.claim(s, again, Some("/compact")), "used up");
        assert!(!r.claim(s, again, None));
        for i in 0..=INPUTS {
            r.sent(s, &i.to_string());
        }
        assert!(!r.claim(s, again, Some("0")), "dropped as the oldest");
        assert!(r.claim(s, again, Some("1")));
    }

    /// Inputs the CLI took from its queue together are one prompt record: "?" written to
    /// start a fresh process, "rename yourself" while it started, the record "?\nrename
    /// yourself" with a uuid the stream did not name (2.1.312).
    #[test]
    fn claim_inputs_taken_together() {
        let r = OwnRecords::default();
        let s = Uuid::new_v4();
        let (merged, other) = (Uuid::new_v4(), Uuid::new_v4());
        r.sent(s, "earlier");
        r.sent(s, "?");
        r.sent(s, "rename yourself\n");
        assert!(
            !r.claim(s, other, Some("?\nrename")),
            "not the whole of an input"
        );
        assert!(
            !r.claim(s, other, Some("rename yourself\n?")),
            "not in the order written"
        );
        assert!(r.claim(s, merged, Some("?\nrename yourself")));
        assert!(r.is_own(s, merged));
        assert!(!r.claim(s, other, Some("?")), "used up");
        assert!(!r.claim(s, other, Some("rename yourself")), "used up");
        assert!(
            r.claim(s, other, Some("earlier")),
            "the input before the run stays"
        );
    }
}
