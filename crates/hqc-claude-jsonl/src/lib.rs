//! Tolerant parser for Claude Code transcripts (SPEC §9.4): unknown record types are kept
//! as `Record::Other` and never fail the parse. Fixtures in `fixtures/` were captured from
//! Claude Code 2.1.307 and 2.1.308 with `api-request*` records removed and the file
//! snapshot stripped.
//!
//! One parser serves the adapter (titles), the mirror and the import (SPEC §9.4): the
//! mirror reads [`Message::is_main`] records, takes the session's working directory from
//! [`first_cwd`] and the end of a turn from [`Message::stop_reason`].
//!
//! Record `uuid`s are shared between the jsonl file and stream-json output (verified on
//! 2.1.307, SPEC §8.1), which is what makes mirror/adapter event ids line up.

use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Record {
    User(Message),
    Assistant(Message),
    /// The session title Claude Code generates after the first prompt (SPEC §8.1, §9).
    AiTitle(AiTitle),
    /// Everything else: attachment, file-history-snapshot, system, queue-operation,
    /// last-prompt, cost-state, atis-latch, summary, ... (SPEC §9).
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub uuid: Uuid,
    #[serde(default)]
    pub parent_uuid: Option<Uuid>,
    #[serde(default)]
    pub is_sidechain: bool,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    /// Text the CLI injected as a user message (caveats, slash command output): not
    /// something a person typed.
    #[serde(default)]
    pub is_meta: bool,
    /// The summary that replaces the history after `/compact`.
    #[serde(default)]
    pub is_compact_summary: bool,
    pub message: serde_json::Value,
}

impl Message {
    /// A record of the session's main chain that shows in its history: not a subagent's
    /// (`isSidechain`), not injected by the CLI (`isMeta`, compact summary). These are the
    /// records the mirror maps to events and the foreign-writer check looks at (SPEC §9.3).
    pub fn is_main(&self) -> bool {
        !self.is_sidechain && !self.is_meta && !self.is_compact_summary
    }

    /// `message.stop_reason` of an assistant record: `end_turn` on the blocks of the turn's
    /// last API message, `tool_use` before a tool call (2.1.308). Every block of a message
    /// is a record of its own and carries the message's `stop_reason` in the transcript;
    /// stream-json leaves it `null`.
    pub fn stop_reason(&self) -> Option<&str> {
        self.message.get("stop_reason")?.as_str()
    }

    /// The content blocks; a plain string (a typed prompt) is one `text` block.
    pub fn blocks(&self) -> Vec<serde_json::Value> {
        match self.message.get("content") {
            Some(serde_json::Value::String(s)) => {
                vec![serde_json::json!({"type": "text", "text": s})]
            }
            Some(serde_json::Value::Array(a)) => a.clone(),
            _ => vec![],
        }
    }
}

/// The working directory of a transcript: `cwd` of its first record that has one. The
/// folder name under `projects/` cannot be decoded back (SPEC §9), so this is the source.
pub fn first_cwd(s: &str) -> Option<String> {
    s.lines().filter(|l| l.contains("\"cwd\"")).find_map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        v.get("cwd")?
            .as_str()
            .filter(|c| !c.is_empty())
            .map(str::to_owned)
    })
}

/// `{"type": "ai-title", "aiTitle": "...", "sessionId": "..."}` (2.1.307). The CLI may write
/// it more than once per session; the last one wins.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTitle {
    #[serde(default)]
    pub ai_title: String,
    /// Kept as text: a malformed id must not cost the title.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// The last non-empty `ai-title` of a transcript, trimmed. Lines that do not mention
/// `ai-title` are not parsed, so this is cheap on long transcripts.
pub fn last_ai_title(s: &str) -> Option<String> {
    s.lines()
        .rev()
        .filter(|l| l.contains("\"ai-title\""))
        .find_map(|l| match parse_line(l) {
            Ok(Record::AiTitle(t)) if !t.ai_title.trim().is_empty() => {
                Some(t.ai_title.trim().to_string())
            }
            _ => None,
        })
}

/// One line of a transcript. Lines that are not valid JSON objects are reported as
/// `Err` so callers can count and log them without stopping (SPEC §9.4).
pub fn parse_line(line: &str) -> Result<Record, serde_json::Error> {
    serde_json::from_str(line)
}

/// Parse a whole file, skipping blank lines; returns records and the number of
/// unparseable lines.
pub fn parse_str(s: &str) -> (Vec<Record>, usize) {
    let mut out = Vec::new();
    let mut bad = 0;
    for line in s.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match parse_line(line) {
            Ok(r) => out.push(r),
            Err(_) => bad += 1,
        }
    }
    (out, bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../fixtures/tool-call-2.1.307.jsonl");

    #[test]
    fn fixture_parses_and_keeps_main_chain() {
        let (records, bad) = parse_str(FIXTURE);
        assert_eq!(bad, 0);
        let main: Vec<&Message> = records
            .iter()
            .filter_map(|r| match r {
                Record::User(m) | Record::Assistant(m) if !m.is_sidechain => Some(m),
                _ => None,
            })
            .collect();
        // user prompt, assistant tool_use, user tool_result, assistant text
        assert_eq!(main.len(), 4);
        assert!(records.iter().any(|r| matches!(r, Record::Other)));
    }

    /// SPEC §8.1: the `ai-title` record, written twice in this run.
    #[test]
    fn ai_title_from_fixture() {
        let (records, _) = parse_str(FIXTURE);
        let titles: Vec<&AiTitle> = records
            .iter()
            .filter_map(|r| match r {
                Record::AiTitle(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(titles.len(), 2);
        assert_eq!(
            titles[0].session_id.as_deref(),
            Some("e47a9949-9b46-4e74-9701-525d009cf93e")
        );
        assert_eq!(
            last_ai_title(FIXTURE).as_deref(),
            Some("Hqc-uuid-test echo command")
        );
        let later = format!(
            "{FIXTURE}{}\n",
            r#"{"type":"ai-title","aiTitle":" Renamed "}"#
        );
        assert_eq!(last_ai_title(&later).as_deref(), Some("Renamed"));
        let blank = format!("{FIXTURE}{}\n", r#"{"type":"ai-title","aiTitle":""}"#);
        assert_eq!(
            last_ai_title(&blank).as_deref(),
            Some("Hqc-uuid-test echo command")
        );
        assert_eq!(
            last_ai_title(include_str!("../fixtures/permission-allow-2.1.307.jsonl")),
            None
        );
    }

    /// T0 fixtures: (transcript, stream-json stdout) of the same run.
    const T0_PAIRS: &[(&str, &str, &str)] = &[
        (
            "permission-allow",
            include_str!("../fixtures/permission-allow-2.1.307.jsonl"),
            include_str!("../fixtures/stream-permission-allow-2.1.307.jsonl"),
        ),
        (
            "permission-deny",
            include_str!("../fixtures/permission-deny-2.1.307.jsonl"),
            include_str!("../fixtures/stream-permission-deny-2.1.307.jsonl"),
        ),
        (
            "permission-always",
            include_str!("../fixtures/permission-always-2.1.307.jsonl"),
            include_str!("../fixtures/stream-permission-always-2.1.307.jsonl"),
        ),
        (
            "ask-user-question",
            include_str!("../fixtures/ask-user-question-2.1.307.jsonl"),
            include_str!("../fixtures/stream-ask-user-question-2.1.307.jsonl"),
        ),
        (
            "mcp-timeout",
            include_str!("../fixtures/mcp-timeout-2.1.307.jsonl"),
            include_str!("../fixtures/stream-mcp-timeout-2.1.307.jsonl"),
        ),
    ];

    fn main_chain_uuids(records: &[Record]) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = records
            .iter()
            .filter_map(|r| match r {
                Record::User(m) | Record::Assistant(m) if !m.is_sidechain => Some(m.uuid),
                _ => None,
            })
            .collect();
        ids.sort();
        ids
    }

    /// T15 sets (2.1.308): a session started with `claude -p "<prompt>"` and taken over
    /// through `--resume` with the adapter's flags (`vscode-takeover`), and an adapter session
    /// continued by `claude -p --resume` from outside (`adapter-then-outside`). The stream
    /// is the adapter's run; the transcript holds both writers.
    const T15_SETS: &[(&str, &str, &str)] = &[
        (
            "vscode-takeover",
            include_str!("../fixtures/vscode-takeover-2.1.308.jsonl"),
            include_str!("../fixtures/stream-vscode-takeover-2.1.308.jsonl"),
        ),
        (
            "adapter-then-outside",
            include_str!("../fixtures/adapter-then-outside-2.1.308.jsonl"),
            include_str!("../fixtures/stream-adapter-then-outside-2.1.308.jsonl"),
        ),
    ];

    fn main_records(records: &[Record]) -> Vec<&Message> {
        records
            .iter()
            .filter_map(|r| match r {
                Record::User(m) | Record::Assistant(m) if m.is_main() => Some(m),
                _ => None,
            })
            .collect()
    }

    /// SPEC §9: `--resume` appends to the same file; both writers' records carry the same
    /// `cwd` and `sessionId`; the adapter's uuids are all in the transcript, the other
    /// writer's records are exactly the rest; no `ai-title` in either run.
    #[test]
    fn t15_two_writers_share_one_transcript() {
        for (name, transcript, stream) in T15_SETS {
            let (tr, bad_tr) = parse_str(transcript);
            let (st, bad_st) = parse_str(stream);
            assert_eq!((bad_tr, bad_st), (0, 0), "{name}");
            assert_eq!(
                first_cwd(transcript).as_deref(),
                Some(format!("/tmp/hqc-fixture/{name}").as_str())
            );
            let main = main_records(&tr);
            let sessions: std::collections::BTreeSet<_> =
                main.iter().map(|m| m.session_id).collect();
            assert_eq!(sessions.len(), 1, "{name}: one session id");
            let own: std::collections::HashSet<Uuid> =
                main_records(&st).iter().map(|m| m.uuid).collect();
            let foreign: Vec<&Message> = main
                .iter()
                .copied()
                .filter(|m| !own.contains(&m.uuid))
                .collect();
            assert!(
                own.iter().all(|u| main.iter().any(|m| m.uuid == *u)),
                "{name}"
            );
            match *name {
                // `claude -p` before the adapter: prompt, tool_use, tool_result, thinking, text.
                "vscode-takeover" => {
                    assert_eq!(foreign.len(), 5, "{name}");
                    assert_eq!(own.len(), 2, "{name}: echo of the prompt and the answer");
                    assert!(main[..5].iter().all(|m| !own.contains(&m.uuid)));
                }
                // The outside turn after the adapter's: prompt, thinking, text.
                "adapter-then-outside" => {
                    assert_eq!(foreign.len(), 3, "{name}");
                    assert_eq!(own.len(), 4, "{name}");
                    assert!(main[4..].iter().all(|m| !own.contains(&m.uuid)));
                }
                _ => unreachable!(),
            }
            // The last record of each writer's turn ends it.
            assert_eq!(main.last().unwrap().stop_reason(), Some("end_turn"));
            assert_eq!(last_ai_title(transcript), None, "{name}: ai-title");
        }
    }

    #[test]
    fn meta_records_are_not_main() {
        let m = |extra: &str| match parse_line(&format!(
            r#"{{"type":"user","uuid":"{}","message":{{"content":"x"}}{extra}}}"#,
            Uuid::nil()
        ))
        .unwrap()
        {
            Record::User(m) => m,
            other => panic!("{other:?}"),
        };
        assert!(m("").is_main());
        assert!(!m(r#","isMeta":true"#).is_main());
        assert!(!m(r#","isSidechain":true"#).is_main());
        assert!(!m(r#","isCompactSummary":true"#).is_main());
        assert_eq!(m("").blocks()[0]["text"], "x");
    }

    /// SPEC §8.1, §9.3: stream-json and the transcript share message uuids, and the
    /// `--replay-user-messages` echo carries the uuid of the transcript's prompt record.
    #[test]
    fn t0_stream_and_transcript_share_uuids() {
        for (name, transcript, stream) in T0_PAIRS {
            let (tr, bad_tr) = parse_str(transcript);
            let (st, bad_st) = parse_str(stream);
            assert_eq!((bad_tr, bad_st), (0, 0), "{name}: unparseable lines");
            assert_eq!(main_chain_uuids(&tr), main_chain_uuids(&st), "{name}");

            let echo: Vec<String> = stream
                .lines()
                .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
                .filter(|v| v["isReplay"] == true)
                .map(|v| v["uuid"].as_str().unwrap().to_owned())
                .collect();
            let prompt = tr.iter().find_map(|r| match r {
                Record::User(m) if m.message["content"].is_string() => Some(m.uuid.to_string()),
                _ => None,
            });
            assert_eq!(echo, vec![prompt.unwrap()], "{name}: replay echo uuid");
        }
    }
}
