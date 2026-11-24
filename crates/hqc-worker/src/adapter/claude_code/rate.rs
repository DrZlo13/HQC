//! The token rate of a Claude Code process ([`AdapterEvent::TokenRate`], `tokens.rate`, SPEC
//! §4). Every [`TICK`] the output tokens of the main agent's responses
//! ([`Core::take_output_tokens`]) and of the responses new in the transcript of each of its
//! subagents ([`Core::live_ids`]:
//! `<projects folder>/<CLI session id>/subagents/agent-<task_id>.jsonl`) go into a window of
//! [`WINDOW`]; the rate is their sum over it per second, reported when it changes. While no
//! turn and no subagent runs ([`Core::working`]) it is 0. The same tick tells of the commands a
//! subagent left running ([`Core::leftovers`]) and of background work that runs long
//! ([`Core::long_runs`]), and reads what the CLI wrote into the main
//! transcript about the session's goal ([`GoalTail`], SPEC 0.218).

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use hqc_claude_jsonl::GoalRecord;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::watch;
use tokio::time::Instant;

use super::engine::{Action, Core};
use super::process::ExitInfo;
use super::{lock, Io};
use crate::adapter::AdapterEvent;

/// How often the rate is counted.
const TICK: Duration = Duration::from_secs(2);
/// The rate is the output tokens of this last stretch, per second.
const WINDOW: Duration = Duration::from_secs(20);
/// A transcript larger than this is not read for its title on a tick, only at a turn's end.
const TITLE_MAX: u64 = 4 * 1024 * 1024;

/// What has been read of one subagent's transcript.
#[derive(Debug, Default)]
struct Tail {
    /// Bytes read: up to the end of the last complete line.
    offset: u64,
    /// `message.id`s counted: each content block of a response is a line of its own, and every
    /// line of the response carries its `usage`.
    seen: HashSet<String>,
}

impl Tail {
    /// The output tokens of the responses new in the complete lines of `bytes`, what follows
    /// [`Tail::offset`] in the file, written at `since` or later; the offset moves past the
    /// last of those lines.
    fn read(&mut self, bytes: &[u8], since: DateTime<Utc>) -> u64 {
        let Some(end) = bytes.iter().rposition(|b| *b == b'\n') else {
            return 0;
        };
        self.offset += end as u64 + 1;
        let mut tokens = 0;
        for line in bytes[..end].split(|b| *b == b'\n') {
            let Ok(v) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            let m = &v["message"];
            match m["id"].as_str() {
                Some(id) if v["type"] == "assistant" && self.seen.insert(id.to_owned()) => {
                    // A subagent this process runs on from an earlier process (`--resume`, a
                    // subagent continued): what it wrote before was not written now.
                    let before = v["timestamp"]
                        .as_str()
                        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                        .is_some_and(|t| t < since);
                    if !before {
                        tokens += m["usage"]["output_tokens"].as_u64().unwrap_or(0);
                    }
                }
                _ => {}
            }
        }
        tokens
    }

    /// [`Tail::read`] of what `path` holds after the offset; a file not there yet holds
    /// nothing, and is read again on the next tick.
    async fn read_file(&mut self, path: &Path, since: DateTime<Utc>) -> u64 {
        match read_after(path, self.offset).await {
            Some(bytes) => self.read(&bytes, since),
            None => 0,
        }
    }
}

/// What `path` holds from byte `offset` on; `None` when it cannot be read.
async fn read_after(path: &Path, offset: u64) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut f = tokio::fs::File::open(path).await.ok()?;
    f.seek(SeekFrom::Start(offset)).await.ok()?;
    f.read_to_end(&mut bytes).await.ok()?;
    Some(bytes)
}

/// What has been read of the main transcript for the goal records the CLI writes into it
/// (`goal_status` attachments, SPEC 0.218); stdout carries none. Read on the meter's ticks and
/// before a `result` line ([`read_goals`]).
#[derive(Debug)]
pub(super) struct GoalTail {
    /// The transcript read; it moves to another file after `/clear`.
    path: Option<PathBuf>,
    /// Bytes read: up to the end of the last complete line.
    offset: u64,
    /// When the process started: a record written earlier is a fork's copied history.
    since: DateTime<Utc>,
}

impl GoalTail {
    /// Starts at the end of `path`, which holds what was written before this process: the goal
    /// it holds is told once at start (`hqc_claude_jsonl::last_goal_status`), not on every
    /// tick. A file not there yet is read from its start.
    pub(super) async fn start(path: Option<PathBuf>) -> Self {
        let offset = match &path {
            Some(p) => tokio::fs::metadata(p).await.map_or(0, |m| m.len()),
            None => 0,
        };
        GoalTail {
            path,
            offset,
            since: Utc::now(),
        }
    }

    /// The goal records in the complete lines of `bytes`, what follows [`GoalTail::offset`],
    /// written at [`GoalTail::since`] or later (a fork's transcript starts with copied
    /// history); the offset moves past the last of those lines.
    fn read(&mut self, bytes: &[u8]) -> Vec<GoalRecord> {
        let since = self.since;
        let Some(end) = bytes.iter().rposition(|b| *b == b'\n') else {
            return vec![];
        };
        self.offset += end as u64 + 1;
        bytes[..end]
            .split(|b| *b == b'\n')
            .filter_map(|l| hqc_claude_jsonl::goal_record(std::str::from_utf8(l).ok()?))
            .filter(|g| {
                !g.timestamp
                    .as_deref()
                    .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                    .is_some_and(|t| t < since)
            })
            .collect()
    }

    /// [`GoalTail::read`] of what the transcript `path` holds after the offset. A transcript
    /// other than the last one read (after `/clear`) is read from its start.
    async fn read_file(&mut self, path: Option<PathBuf>) -> Vec<GoalRecord> {
        if path != self.path {
            self.path = path;
            self.offset = 0;
        }
        let Some(path) = &self.path else {
            return vec![];
        };
        match read_after(path, self.offset).await {
            Some(bytes) => self.read(&bytes),
            None => vec![],
        }
    }
}

/// The goal records the CLI wrote since the last read, through the core, which sets them
/// against the text it holds ([`Core::on_goal`]). Also before a `result` line: the check of the
/// turn's last text is in the transcript by then, and what follows the turn at once (a slash
/// command typed during it) would otherwise come before it.
pub(super) async fn read_goals(core: &Mutex<Core>, io: &Io) -> Vec<Action> {
    let records = io.goals.lock().await.read_file(io.transcript()).await;
    if records.is_empty() {
        return Vec::new();
    }
    let mut c = lock(core);
    records.iter().flat_map(|g| c.on_goal(g)).collect()
}

/// The transcript's title is read on this tick: during the first model turn of a new
/// conversation ([`Core::fresh`]) while the process has sent no title.
fn reads_title(working: bool, fresh: bool, titled: bool) -> bool {
    working && fresh && !titled
}

/// Counts the rate of the process until it exits, and on each tick runs what
/// [`Core::leftovers`] and [`Core::long_runs`] give.
pub(super) async fn meter(
    core: Arc<Mutex<Core>>,
    io: Arc<Io>,
    mut exited: watch::Receiver<Option<ExitInfo>>,
) {
    let started = Utc::now();
    let mut tick = tokio::time::interval(TICK);
    let mut window: VecDeque<(Instant, u64)> = VecDeque::new();
    let mut tails: HashMap<String, Tail> = HashMap::new();
    // The subagents live on the last tick.
    let mut was_live: Vec<String> = Vec::new();
    let mut sent = 0;
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = exited.wait_for(Option::is_some) => return,
        }
        let (mut tokens, working, fresh, session, live, told) = {
            let mut c = lock(&core);
            let now = std::time::Instant::now();
            let mut told = c.leftovers(now);
            told.extend(c.long_runs(now));
            (
                c.take_output_tokens(),
                c.working(),
                c.fresh(),
                c.cli_session().map(str::to_owned),
                c.live_ids(),
                told,
            )
        };
        if let Err(e) = io.run(told).await {
            tracing::warn!(session_id = %io.session_id, "{e:#}");
        }
        // Beside the session's transcript: `<CLI session id>/subagents`.
        let dir = io
            .transcript()
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .zip(session)
            .map(|(p, s)| p.join(s).join("subagents"));
        if let Some(dir) = dir {
            // A subagent that ended is read to the end once more. Its tail is kept: one the CLI
            // runs again under the same id writes on into the same file.
            let ended = was_live.iter().filter(|id| !live.contains(id));
            for id in live.iter().chain(ended) {
                let tail = tails.entry(id.clone()).or_default();
                tokens += tail
                    .read_file(&dir.join(format!("agent-{id}.jsonl")), started)
                    .await;
            }
        }
        was_live = live;
        let goals = read_goals(&core, &io).await;
        if let Err(e) = io.run(goals).await {
            tracing::warn!(session_id = %io.session_id, "{e:#}");
        }
        // The CLI writes its own title into the transcript during the first turn of a new
        // conversation and does not stream it (`session_title_changed` carries only a
        // person's): it is read on each tick of that turn until the process has one, not only
        // at the turn's end (SPEC §8.1). A resumed conversation without one gets none, and its
        // transcript, tens of MB, is not read every tick; nor is one over [`TITLE_MAX`], which a
        // first turn the CLI writes no title in (a first message under 10 characters) grows.
        if reads_title(working, fresh, lock(&io.title).is_some()) {
            io.report_title(Some(TITLE_MAX)).await;
        }
        let now = Instant::now();
        if working {
            window.push_back((now, tokens));
        } else {
            window.clear();
        }
        while window.front().is_some_and(|(at, _)| now - *at >= WINDOW) {
            window.pop_front();
        }
        let total: u64 = window.iter().map(|(_, n)| n).sum();
        let rate = (total as f64 / WINDOW.as_secs_f64()).round() as u32;
        if rate != sent {
            sent = rate;
            let _ = io.events.send(AdapterEvent::TokenRate(rate)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// SPEC §8.1: a tick reads the title only in a new conversation's first turn, until the
    /// process has one; a resumed transcript without one, tens of MB, is not read every tick.
    #[test]
    fn a_tick_reads_the_title_only_in_a_new_conversation_first_turn() {
        assert!(reads_title(true, true, false));
        assert!(!reads_title(true, false, false), "a resumed conversation");
        assert!(!reads_title(true, true, true), "the process has a title");
        assert!(!reads_title(false, true, false), "no turn");
    }

    fn line(id: &str, block: &str, output_tokens: u64) -> String {
        json!({
            "type": "assistant",
            "message": {
                "id": id,
                "content": [{"type": block}],
                "usage": {"output_tokens": output_tokens},
            },
        })
        .to_string()
    }

    /// A subagent's transcript (2.1.312): each content block of a response is its own line, the
    /// lines of one response share `message.id` and its `usage`. A response counts once, also
    /// when its lines come in two reads; a line not ended yet waits for the next read.
    #[test]
    fn a_response_counts_once() {
        let mut t = Tail::default();
        let done = format!(
            "{}\n{}\n{}\n{}\n",
            line("msg_1", "thinking", 234),
            line("msg_1", "tool_use", 234),
            json!({"type": "user", "message": {"content": []}}),
            line("msg_2", "tool_use", 306),
        );
        let next = line("msg_3", "text", 50);
        let (head, rest) = next.split_at(10);
        let since = DateTime::<Utc>::MIN_UTC;
        assert_eq!(t.read(format!("{done}{head}").as_bytes(), since), 540);
        assert_eq!(t.offset, done.len() as u64);
        let more = format!("{head}{rest}\n{}\n", line("msg_2", "text", 306));
        assert_eq!(t.read(more.as_bytes(), since), 50);
        assert_eq!(t.offset, (done.len() + more.len()) as u64);
        assert_eq!(t.read(b"", since), 0);
    }

    /// A subagent continued in a later process writes on into its transcript: the responses
    /// written before the process started do not count, a later line of such a response
    /// neither.
    #[test]
    fn what_was_written_before_does_not_count() {
        let at = |id: &str, ts: &str, n: u64| {
            let mut v: Value = serde_json::from_str(&line(id, "text", n)).unwrap();
            v["timestamp"] = json!(ts);
            v.to_string()
        };
        let since = DateTime::parse_from_rfc3339("2026-11-11T02:00:00Z")
            .unwrap()
            .into();
        let bytes = format!(
            "{}\n{}\n{}\n",
            at("msg_1", "2026-11-09T21:14:31.214Z", 25_000),
            at("msg_1", "2026-11-11T02:00:01.000Z", 25_000),
            at("msg_2", "2026-11-11T02:00:05.000Z", 120),
        );
        assert_eq!(Tail::default().read(bytes.as_bytes(), since), 120);
    }

    fn goal_line(uuid: &str, ts: &str, attachment: Value) -> String {
        json!({"type": "attachment", "uuid": uuid, "timestamp": ts, "attachment": attachment})
            .to_string()
    }

    fn goal_tail(offset: u64, since: DateTime<Utc>) -> GoalTail {
        GoalTail {
            path: None,
            offset,
            since,
        }
    }

    /// SPEC 0.218: a tick reads the goal records past the offset only; a record written before
    /// the process started (a fork's copied history) is skipped; a last line not ended yet
    /// waits for the next read; other lines are not goal records.
    #[test]
    fn a_tick_reads_the_goal_records_past_the_offset() {
        let since = DateTime::parse_from_rfc3339("2026-11-22T19:00:00Z")
            .unwrap()
            .into();
        let g = |u: &str, ts: &str, met: bool| {
            goal_line(
                u,
                ts,
                json!({"type": "goal_status", "met": met, "condition": "C", "reason": "R"}),
            )
        };
        let old = g(
            "00000000-0000-4000-8000-000000000001",
            "2026-11-20T10:00:00.000Z",
            false,
        );
        let new1 = g(
            "00000000-0000-4000-8000-000000000002",
            "2026-11-22T19:33:57.895Z",
            false,
        );
        let new2 = g(
            "00000000-0000-4000-8000-000000000003",
            "2026-11-22T19:34:57.895Z",
            true,
        );
        let other = json!({"type": "user", "message": {"content": "goal_status"}}).to_string();
        let done = format!("{old}\n{other}\n{new1}\n");
        let (head, rest) = new2.split_at(20);
        let mut t = goal_tail(0, since);
        let got = t.read(format!("{done}{head}").as_bytes());
        assert_eq!(
            got.iter().map(|g| g.uuid.to_string()).collect::<Vec<_>>(),
            ["00000000-0000-4000-8000-000000000002"]
        );
        assert_eq!(t.offset, done.len() as u64);
        let more = format!("{head}{rest}\n");
        let got = t.read(more.as_bytes());
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].uuid.to_string(),
            "00000000-0000-4000-8000-000000000003"
        );
        assert_eq!(t.offset, (done.len() + more.len()) as u64);
        assert!(t.read(b"").is_empty());
    }

    /// SPEC 0.218: the tail starts at the end of the transcript the process starts on, a
    /// transcript it moves to (`/clear`) is read from its start, and one not there yet holds
    /// nothing.
    #[tokio::test]
    async fn the_goal_tail_starts_at_the_end_and_follows_a_new_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jsonl");
        let b = dir.path().join("b.jsonl");
        let at = |u: &str| {
            goal_line(
                u,
                "2026-11-22T19:33:57.895Z",
                json!({"type": "goal_status", "met": false, "sentinel": true, "condition": "C"}),
            ) + "\n"
        };
        let before = at("00000000-0000-4000-8000-000000000001");
        std::fs::write(&a, &before).unwrap();
        let mut t = GoalTail::start(Some(a.clone())).await;
        // The records carry a fixed time, before the tail's start.
        t.since = DateTime::<Utc>::MIN_UTC;
        assert_eq!(t.offset, before.len() as u64);
        assert!(t.read_file(Some(a.clone())).await.is_empty());
        let mut f = std::fs::OpenOptions::new().append(true).open(&a).unwrap();
        std::io::Write::write_all(
            &mut f,
            at("00000000-0000-4000-8000-000000000002").as_bytes(),
        )
        .unwrap();
        let got = t.read_file(Some(a.clone())).await;
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].uuid.to_string(),
            "00000000-0000-4000-8000-000000000002"
        );
        // `/clear`: the new transcript, from its first byte.
        std::fs::write(&b, at("00000000-0000-4000-8000-000000000003")).unwrap();
        let got = t.read_file(Some(b.clone())).await;
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].uuid.to_string(),
            "00000000-0000-4000-8000-000000000003"
        );
        // A transcript not written yet, and none at all.
        let c = dir.path().join("c.jsonl");
        assert!(t.read_file(Some(c.clone())).await.is_empty());
        assert_eq!(GoalTail::start(Some(c)).await.offset, 0);
        assert!(t.read_file(None).await.is_empty());
    }
}
