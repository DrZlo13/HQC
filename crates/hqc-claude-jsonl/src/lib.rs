//! Tolerant parser for Claude Code transcripts (SPEC §9.4): unknown record types are kept
//! as `Record::Other` and never fail the parse. Fixtures in `fixtures/` were captured from
//! Claude Code 2.1.300 with `api-request*` records removed and the file snapshot stripped.
//!
//! Record `uuid`s are shared between the jsonl file and stream-json output (verified on
//! 2.1.300, SPEC §8.1), which is what makes mirror/adapter event ids line up.

use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Record {
    User(Message),
    Assistant(Message),
    /// Everything else: attachment, file-history-snapshot, system, queue-operation,
    /// last-prompt, ai-title, cost-state, atis-latch, summary, ... (SPEC §9).
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

    const FIXTURE: &str = include_str!("../fixtures/tool-call-2.1.300.jsonl");

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
}
