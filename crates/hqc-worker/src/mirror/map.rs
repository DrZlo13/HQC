//! Transcript records to SPEC §4 events, for the mirror and the import (SPEC §9.1, §9.2).
//!
//! Only main-chain `user`/`assistant` records ([`Message::is_main`]); every content block
//! is one event with the id of SPEC §3 Event ([`Event::id_from_claude_block`]), the same the
//! adapter gives the same block in stream-json:
//! - `user` text (a typed prompt) → `user.message` with `delivery: delivered`;
//! - `user` `tool_result` → `tool.result`;
//! - `assistant` text → `agent.message`, `final` when the record's `stop_reason` is
//!   `end_turn` (2.1.308 writes it on every block of the turn's last message), `in_reply_to`
//!   the last prompt of the file;
//! - `assistant` `tool_use` → `tool.call`, non-empty `thinking` → `agent.thinking`.
//!
//! Empty thinking (2.1.308 writes only a signature), images and unknown blocks give no event.
//! `session_id` and the author ids are nil: the worker does not know the hqc session, the
//! hub fills them in (`mirror.event`, SPEC §5.4). Timestamps are the records' own.

use chrono::{DateTime, Utc};
use hqc_claude_jsonl::{Message, Record};
use hqc_proto::event::{Author, Delivery, Event, EventKind};
use uuid::Uuid;

use crate::adapter::claude_code::engine::{
    preview, result_text, text, ERROR_TEXT_LIMIT, PREVIEW_LIMIT,
};

/// The events of one record; `in_reply_to`: event id of the file's last prompt.
pub fn record_events(record: &Record, in_reply_to: Option<Uuid>) -> Vec<Event> {
    match record {
        Record::User(m) if m.is_main() => user(m),
        Record::Assistant(m) if m.is_main() => assistant(m, in_reply_to),
        _ => vec![],
    }
}

/// The `user.message` among `events`: a prompt typed by a person, the `in_reply_to` of the
/// answers that follow.
pub fn prompt_id(events: &[Event]) -> Option<Uuid> {
    events
        .iter()
        .find(|e| matches!(e.kind, EventKind::UserMessage { .. }))
        .map(|e| e.event_id)
}

pub(crate) fn ts(m: &Message) -> DateTime<Utc> {
    m.timestamp
        .as_deref()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(Utc::now)
}

fn event(m: &Message, index: usize, author: Author, kind: EventKind) -> Event {
    let user = matches!(kind, EventKind::UserMessage { .. });
    let ts = ts(m);
    Event {
        session_id: Uuid::nil(),
        event_id: Event::id_from_claude_block(&m.uuid, index),
        seq: None,
        ts,
        author,
        kind,
        chain: None,
        client_op_id: None,
        delivery: user.then_some(Delivery::Delivered),
        delivered_at: user.then_some(ts),
    }
}

fn agent() -> Author {
    Author::Agent { id: Uuid::nil() }
}

fn user(m: &Message) -> Vec<Event> {
    let mut out = Vec::new();
    for (i, b) in m.blocks().iter().enumerate() {
        match b["type"].as_str() {
            Some("text") => {
                let t = b["text"].as_str().unwrap_or("");
                if t.trim().is_empty() {
                    continue;
                }
                out.push(event(
                    m,
                    i,
                    Author::User { id: Uuid::nil() },
                    EventKind::UserMessage {
                        text: text(t),
                        artifact_ids: vec![],
                    },
                ));
            }
            Some("tool_result") => {
                let t = result_text(&b["content"]);
                let error = b["is_error"] == true;
                out.push(event(
                    m,
                    i,
                    agent(),
                    EventKind::ToolResult {
                        call_id: b["tool_use_id"].as_str().unwrap_or("").into(),
                        result_preview: preview(&t, PREVIEW_LIMIT),
                        error,
                        error_text: error.then(|| preview(&t, ERROR_TEXT_LIMIT)),
                        duration_ms: None,
                        artifact_id: None,
                    },
                ));
            }
            _ => {}
        }
    }
    out
}

fn assistant(m: &Message, in_reply_to: Option<Uuid>) -> Vec<Event> {
    let end_turn = m.stop_reason() == Some("end_turn");
    let mut out = Vec::new();
    for (i, b) in m.blocks().iter().enumerate() {
        let kind = match b["type"].as_str() {
            Some("text") => {
                let t = b["text"].as_str().unwrap_or("");
                if t.is_empty() {
                    continue;
                }
                EventKind::AgentMessage {
                    text: text(t),
                    r#final: end_turn,
                    in_reply_to,
                }
            }
            Some("tool_use") => EventKind::ToolCall {
                call_id: b["id"].as_str().unwrap_or("").into(),
                tool: b["name"].as_str().unwrap_or("").into(),
                args_preview: preview(
                    &serde_json::to_string(&b["input"]).unwrap_or_default(),
                    PREVIEW_LIMIT,
                ),
            },
            Some("thinking") => {
                let t = b["thinking"].as_str().unwrap_or("");
                if t.is_empty() {
                    continue;
                }
                EventKind::AgentThinking { text: text(t) }
            }
            _ => continue,
        };
        out.push(event(m, i, agent(), kind));
    }
    out
}

/// What a person typed to get prompt record `record`, to match it with the adapter's inputs
/// ([`crate::mirror::OwnRecords::claim`]): the text of its first text block, and for a slash
/// command, which 2.1.308 writes as `<command-name>/compact</command-name>
/// <command-message>…</command-message> <command-args>…</command-args>`, the command and its
/// arguments. `None` for records that are not a main-chain `user` prompt.
pub fn typed_text(record: &Record) -> Option<String> {
    let Record::User(m) = record else {
        return None;
    };
    if !m.is_main() {
        return None;
    }
    let blocks = m.blocks();
    let t = blocks
        .iter()
        .filter(|b| b["type"] == "text")
        .find_map(|b| b["text"].as_str())?
        .trim();
    let tag = |name: &str| {
        let (open, close) = (format!("<{name}>"), format!("</{name}>"));
        let rest = &t[t.find(&open)? + open.len()..];
        Some(rest[..rest.find(&close)?].trim())
    };
    Some(
        match tag("command-name").filter(|_| t.starts_with("<command-name>")) {
            Some(name) => match tag("command-args").filter(|a| !a.is_empty()) {
                Some(args) => format!("{name} {args}"),
                None => name.to_string(),
            },
            None => t.to_string(),
        },
    )
}

/// `uuid` of a record that counts for the foreign-writer check (SPEC §9.3): a main-chain
/// `user`/`assistant` record that maps to at least one event. A signature-only thinking
/// record, which stream-json may not repeat, is not evidence of another writer.
pub fn counted_uuid(record: &Record) -> Option<Uuid> {
    match record {
        Record::User(m) | Record::Assistant(m)
            if m.is_main() && !record_events(record, None).is_empty() =>
        {
            Some(m.uuid)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hqc_claude_jsonl::parse_str;

    const VSCODE: &str =
        include_str!("../../../hqc-claude-jsonl/fixtures/vscode-takeover-2.1.308.jsonl");

    fn label(e: &Event) -> String {
        match &e.kind {
            EventKind::UserMessage { text, .. } => format!("user:{text}"),
            EventKind::AgentMessage { text, r#final, .. } => format!("agent:{text}:{final}"),
            EventKind::ToolCall { tool, .. } => format!("call:{tool}"),
            EventKind::ToolResult { error, .. } => format!("result:{error}"),
            EventKind::AgentThinking { .. } => "thinking".into(),
            other => format!("{other:?}"),
        }
    }

    /// The whole fixture as the import sends it: both turns, the empty thinking skipped,
    /// answers pointing at their prompts, ids as the adapter derives them.
    #[test]
    fn fixture_maps_to_the_history() {
        let (records, bad) = parse_str(VSCODE);
        assert_eq!(bad, 0);
        let mut prompt = None;
        let mut all = Vec::new();
        for r in &records {
            let evs = record_events(r, prompt);
            prompt = prompt_id(&evs).or(prompt);
            all.extend(evs);
        }
        let labels: Vec<String> = all.iter().map(label).collect();
        assert_eq!(
            labels,
            [
                "user:Run the shell command: echo hqc-vscode. Then reply with exactly: VSCODE-DONE",
                "call:Bash",
                "result:false",
                "agent:VSCODE-DONE:true",
                "user:Reply with exactly: HQC-TURN",
                "agent:HQC-TURN:true",
            ]
        );
        let EventKind::AgentMessage { in_reply_to, .. } = &all[3].kind else {
            unreachable!()
        };
        assert_eq!(*in_reply_to, Some(all[0].event_id));
        assert_eq!(all[0].delivery, Some(Delivery::Delivered));
        assert_eq!(all[0].ts.to_rfc3339(), "2026-10-23T00:10:15.931+00:00");
        // The adapter's id for the same block.
        let Record::Assistant(m) = records
            .iter()
            .find(|r| matches!(r, Record::Assistant(m) if m.stop_reason() == Some("tool_use")))
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            all[1].event_id,
            crate::adapter::claude_code::engine::record_event_id(&m.uuid, 0)
        );
    }

    /// T25, fixture `resume-compact`: prompts stand for their text, the `/compact` command
    /// record for `/compact`; answers and tool results for nothing.
    #[test]
    fn typed_text_of_prompts_and_commands() {
        let (records, _) = parse_str(include_str!(
            "../../../hqc-claude-jsonl/fixtures/resume-compact-2.1.308.jsonl"
        ));
        let typed: Vec<String> = records.iter().filter_map(typed_text).collect();
        assert_eq!(
            typed,
            [
                "Reply with exactly: ONE",
                "Reply with exactly: TWO",
                "/compact",
                "<local-command-stdout>Compacted </local-command-stdout>"
            ]
        );
        let line = r#"{"type":"user","uuid":"5fb160d6-592d-4780-a2b6-ebdcb33e8070","message":{"role":"user","content":"<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args>haiku</command-args>"}}"#;
        let rec = hqc_claude_jsonl::parse_line(line).unwrap();
        assert_eq!(typed_text(&rec).as_deref(), Some("/model haiku"));
    }

    #[test]
    fn signature_only_thinking_is_not_counted() {
        let (records, _) = parse_str(VSCODE);
        let thinking = records
            .iter()
            .find(|r| {
                matches!(r, Record::Assistant(m)
                    if m.blocks().first().is_some_and(|b| b["type"] == "thinking"))
            })
            .unwrap();
        assert_eq!(counted_uuid(thinking), None);
        assert!(records.iter().filter_map(counted_uuid).count() == 6);
    }
}
