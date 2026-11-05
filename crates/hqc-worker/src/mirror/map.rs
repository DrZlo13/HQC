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
//! - `assistant` `tool_use` → `tool.call`, non-empty `thinking` → `agent.thinking`;
//! - `assistant` text with `model` `<synthetic>` (the CLI's own reply) → `agent.message`
//!   with `runtime: true`;
//! - `user` with `origin.kind` `task-notification` (a background task ended; the CLI writes
//!   `<task-notification>…<summary>…</summary>` as its text) → `system task_notification`
//!   with the summary. While the worker's own process runs the session the mirror does not
//!   emit it: the adapter reported the stream-json line, whose uuid differs.
//!
//! Empty thinking (2.1.308 writes only a signature), images and unknown blocks give no event.
//! `session_id` and the author ids are nil: the worker does not know the hqc session, the
//! hub fills them in (`mirror.event`, SPEC §5.4). Timestamps are the records' own.

use chrono::{DateTime, Utc};
use hqc_claude_jsonl::{Message, Record};
use hqc_proto::event::{Author, Delivery, Event, EventKind};
use uuid::Uuid;

use crate::adapter::claude_code::engine::{
    preview, result_text, text, ERROR_TEXT_LIMIT, PREVIEW_LIMIT, SYNTHETIC_MODEL, TASK_NOTIFICATION,
};

/// `origin.kind` of the `user` record of a background task's notification (2.1.312).
const TASK_NOTIFICATION_ORIGIN: &str = "task-notification";

/// A main-chain record of a background task's notification.
pub fn is_task_notification(record: &Record) -> bool {
    matches!(record, Record::User(m) if m.is_main() && m.origin_kind() == Some(TASK_NOTIFICATION_ORIGIN))
}

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
        reply_delivered_at: None,
    }
}

fn agent() -> Author {
    Author::Agent { id: Uuid::nil() }
}

fn system(m: &Message, index: usize, code: &str, text: String) -> Event {
    event(
        m,
        index,
        Author::System,
        EventKind::System {
            text,
            code: code.into(),
            fork: None,
        },
    )
}

/// The text of `<tag>…</tag>` in a notification, trimmed; `None` when absent or empty.
fn tag<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let (open, close) = (format!("<{name}>"), format!("</{name}>"));
    let rest = &s[s.find(&open)? + open.len()..];
    Some(rest[..rest.find(&close)?].trim()).filter(|t| !t.is_empty())
}

fn user(m: &Message) -> Vec<Event> {
    if m.origin_kind() == Some(TASK_NOTIFICATION_ORIGIN) {
        let blocks = m.blocks();
        let (i, body) = blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b["type"] == "text")
            .find_map(|(i, b)| Some((i, b["text"].as_str()?)))
            .unwrap_or((0, ""));
        let t = tag(body, "summary").unwrap_or("Background task");
        return vec![system(m, i, TASK_NOTIFICATION, text(t))];
    }
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
                        schedule_id: None,
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
    let synthetic = m.model() == Some(SYNTHETIC_MODEL);
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
                    runtime: synthetic,
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

    /// A background task's notification is `system task_notification` with its summary (else
    /// "Background task"), not a prompt; the CLI's own reply (`<synthetic>`, the record of a live run
    /// on 2.1.312 with a model the API does not know) is `agent.message` with `runtime: true`
    /// and the adapter's id for the block.
    #[test]
    fn notifications_and_runtime_replies() {
        let rec = hqc_claude_jsonl::parse_line(crate::mirror::tests::NOTIFICATION).unwrap();
        assert!(is_task_notification(&rec));
        let evs = record_events(&rec, None);
        assert_eq!(
            evs.iter().map(|e| e.kind.clone()).collect::<Vec<_>>(),
            [EventKind::System {
                text: "Background command \"Run background sleep and echo marker\" completed \
                       (exit code 0)"
                    .into(),
                code: "task_notification".into(),
                fork: None,
            }]
        );
        assert_eq!((&evs[0].author, evs[0].delivery), (&Author::System, None));
        assert_eq!(prompt_id(&evs), None, "not a prompt");
        let bare = crate::mirror::tests::NOTIFICATION.replace(
            r#"<summary>Background command \"Run background sleep and echo marker\" completed (exit code 0)</summary>\n"#,
            "",
        );
        let evs = record_events(&hqc_claude_jsonl::parse_line(&bare).unwrap(), None);
        assert!(
            matches!(&evs[..], [Event { kind: EventKind::System { text, .. }, .. }] if text == "Background task"),
            "{evs:?}"
        );
        let typed = crate::mirror::tests::NOTIFICATION.replace(r#""origin":{"kind":"task-notification","producer":"session-task","runId":"0mw2x8jvm-af387e8f"},"#, "");
        let rec = hqc_claude_jsonl::parse_line(&typed).unwrap();
        assert!(!is_task_notification(&rec));
        assert!(prompt_id(&record_events(&rec, None)).is_some());

        let line = r#"{"parentUuid":"ce9e21ed-6734-479d-b65e-8794ac737a0b","isSidechain":false,"type":"assistant","uuid":"c294c442-b0d3-4724-94a4-17d06e405b0a","timestamp":"2026-11-05T02:26:14.254Z","message":{"id":"75495e5d-148c-41a6-8b21-25d0efd759a8","container":null,"model":"<synthetic>","role":"assistant","stop_reason":"stop_sequence","stop_sequence":"","type":"message","content":[{"type":"text","text":"There's an issue with the selected model (claude-bogus-model-9-9). It may not exist or you may not have access to it."}]},"error":"model_not_found","isApiErrorMessage":true,"apiErrorStatus":404,"cwd":"/tmp/hqc-fixture/bg","sessionId":"382723fd-060a-4dad-a3a6-e43899c86dac"}"#;
        let rec = hqc_claude_jsonl::parse_line(line).unwrap();
        let prompt = Uuid::now_v7();
        let evs = record_events(&rec, Some(prompt));
        assert_eq!(
            evs.iter().map(|e| e.kind.clone()).collect::<Vec<_>>(),
            [EventKind::AgentMessage {
                text: "There's an issue with the selected model (claude-bogus-model-9-9). It \
                       may not exist or you may not have access to it."
                    .into(),
                r#final: false,
                in_reply_to: Some(prompt),
                runtime: true,
            }]
        );
        assert_eq!(evs[0].author, agent());
        assert_eq!(
            evs[0].event_id,
            crate::adapter::claude_code::engine::record_event_id(
                &"c294c442-b0d3-4724-94a4-17d06e405b0a".parse().unwrap(),
                0
            )
        );
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
