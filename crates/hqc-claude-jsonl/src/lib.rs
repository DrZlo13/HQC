//! Tolerant parser for Claude Code transcripts (SPEC §9.4): unknown record types are kept
//! as `Record::Other` and never fail the parse. Fixtures in `fixtures/` were captured from
//! Claude Code 2.1.307 with `api-request*` records removed and the file snapshot stripped.
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
    pub message: serde_json::Value,
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
