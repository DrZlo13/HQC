//! The token rate of a Claude Code process ([`AdapterEvent::TokenRate`], `tokens.rate`, SPEC
//! §4). Every [`TICK`] the output tokens of the main agent's responses
//! ([`Core::take_output_tokens`]) and of the responses new in the transcript of each of its
//! subagents ([`Core::live_ids`]:
//! `<projects folder>/<CLI session id>/subagents/agent-<task_id>.jsonl`) go into a window of
//! [`WINDOW`]; the rate is their sum over it per second, reported when it changes. While no
//! turn and no subagent runs ([`Core::working`]) it is 0.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::SeekFrom;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::watch;
use tokio::time::Instant;

use super::engine::Core;
use super::process::ExitInfo;
use super::{lock, Io};
use crate::adapter::AdapterEvent;

/// How often the rate is counted.
const TICK: Duration = Duration::from_secs(2);
/// The rate is the output tokens of this last stretch, per second.
const WINDOW: Duration = Duration::from_secs(20);

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
        let offset = self.offset;
        let mut bytes = Vec::new();
        let read = async {
            let mut f = tokio::fs::File::open(path).await?;
            f.seek(SeekFrom::Start(offset)).await?;
            f.read_to_end(&mut bytes).await
        };
        if read.await.is_err() {
            return 0;
        }
        self.read(&bytes, since)
    }
}

/// Counts the rate of the process until it exits.
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
        let (mut tokens, working, session, live) = {
            let mut c = lock(&core);
            (
                c.take_output_tokens(),
                c.working(),
                c.cli_session().map(str::to_owned),
                c.live_ids(),
            )
        };
        // Beside the session's transcript: `<CLI session id>/subagents`.
        let dir = io
            .transcript
            .as_deref()
            .and_then(Path::parent)
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
}
