//! Transcript records to SPEC §4 events, for the mirror and the import (SPEC §9.1, §9.2).
//!
//! Only main-chain `user`/`assistant` records ([`Message::is_main`]) and messages typed during
//! a turn; every content block is one event with the id of SPEC §3 Event
//! ([`Event::id_from_claude_block`]), the same the adapter gives the same block in stream-json:
//! - `user` text (a typed prompt) → `user.message` with `delivery: delivered`;
//! - `attachment` `queued_command` a person typed while a turn ran
//!   ([`Attachment::queued_prompt`]) → `user.message` the same way, at the time it was typed;
//! - `user` `tool_result` → `tool.result`; of an answered `AskUserQuestion` (`toolUseResult`
//!   with `questions` and `answers`) also `clarify.request` and `clarify.response`, ids
//!   [`Event::id_from_claude_part`], which the client draws as the closed question card;
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

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use hqc_claude_jsonl::{Attachment, Message, Record};
use hqc_proto::event::{Author, ClarifyAnswer, Delivery, Event, EventKind};
use uuid::Uuid;

use crate::adapter::claude_code::engine::{
    parse_questions, preview, result_text, text, ERROR_TEXT_LIMIT, PREVIEW_LIMIT, SYNTHETIC_MODEL,
    TASK_NOTIFICATION,
};

/// `origin.kind` of the `user` record of a background task's notification (2.1.312).
const TASK_NOTIFICATION_ORIGIN: &str = "task-notification";

/// A main-chain record of a background task's notification.
pub fn is_task_notification(record: &Record) -> bool {
    matches!(record, Record::User(m) if m.is_main() && m.origin_kind() == Some(TASK_NOTIFICATION_ORIGIN))
}

/// A message a person typed while a turn ran ([`Attachment::queued_prompt`]).
pub fn is_queued_prompt(record: &Record) -> bool {
    matches!(record, Record::Attachment(a) if a.queued_prompt().is_some())
}

/// The events of one record; `in_reply_to`: event id of the file's last prompt.
pub fn record_events(record: &Record, in_reply_to: Option<Uuid>) -> Vec<Event> {
    match record {
        Record::User(m) if m.is_main() => user(m),
        Record::Assistant(m) if m.is_main() => assistant(m, in_reply_to),
        Record::Attachment(a) => queued(a),
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
    parse_ts(m.timestamp.as_deref())
}

/// A record's `timestamp`; now when it has none or a bad one.
fn parse_ts(t: Option<&str>) -> DateTime<Utc> {
    t.and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(Utc::now)
}

fn event(m: &Message, index: usize, author: Author, kind: EventKind) -> Event {
    event_at(m.uuid, ts(m), index, author, kind)
}

/// The event of content block `index` of record `uuid`, written at `ts`.
fn event_at(uuid: Uuid, ts: DateTime<Utc>, index: usize, author: Author, kind: EventKind) -> Event {
    let user = matches!(kind, EventKind::UserMessage { .. });
    Event {
        session_id: Uuid::nil(),
        event_id: Event::id_from_claude_block(&uuid, index),
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
                    user_message(t),
                ));
            }
            Some("tool_result") => {
                let t = result_text(&b["content"]);
                let error = b["is_error"] == true;
                let call_id = b["tool_use_id"].as_str().unwrap_or("");
                out.push(event(
                    m,
                    i,
                    agent(),
                    EventKind::ToolResult {
                        call_id: call_id.into(),
                        result_preview: preview(&t, PREVIEW_LIMIT),
                        error,
                        error_text: error.then(|| preview(&t, ERROR_TEXT_LIMIT)),
                        duration_ms: None,
                        artifact_id: None,
                    },
                ));
                out.extend(answered(m, i, call_id));
            }
            _ => {}
        }
    }
    out
}

fn user_message(t: &str) -> EventKind {
    EventKind::UserMessage {
        text: text(t),
        artifact_ids: vec![],
        schedule_id: None,
    }
}

/// A message typed while a turn ran: one `user.message` per non-empty text block, as for a
/// prompt.
fn queued(a: &Attachment) -> Vec<Event> {
    let (Some(uuid), Some(blocks)) = (a.uuid, a.queued_prompt()) else {
        return vec![];
    };
    let ts = parse_ts(a.timestamp.as_deref());
    blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| b["type"] == "text")
        .filter_map(|(i, b)| Some((i, b["text"].as_str()?)))
        .filter(|(_, t)| !t.trim().is_empty())
        .map(|(i, t)| {
            event_at(
                uuid,
                ts,
                i,
                Author::User { id: Uuid::nil() },
                user_message(t),
            )
        })
        .collect()
}

/// The question card of an answered `AskUserQuestion`, from block `index` of its
/// `tool_result` record: `toolUseResult` holds the questions and the answers keyed by
/// question text. A multi-select answer is the labels joined with ", ", typed text included
/// ([`split_labels`]). Request and response come from the one record, so the card is never
/// open.
fn answered(m: &Message, index: usize, request_id: &str) -> Vec<Event> {
    let Some(r) = &m.tool_use_result else {
        return vec![];
    };
    let (Some((questions, texts)), Some(given)) = (parse_questions(r), r["answers"].as_object())
    else {
        return vec![];
    };
    let mut answers = BTreeMap::new();
    for (q, raw) in questions.iter().zip(&texts) {
        let Some(a) = given.get(raw).and_then(|a| a.as_str()) else {
            continue;
        };
        let a = text(a);
        let answer = if q.multi == Some(true) {
            ClarifyAnswer::Many(split_labels(&a, q.options.as_deref().unwrap_or_default()))
        } else {
            ClarifyAnswer::One(a)
        };
        answers.insert(q.id.clone(), answer);
    }
    let part = |name: &str, author: Author, kind: EventKind| Event {
        event_id: Event::id_from_claude_part(&m.uuid, index, name),
        ..event(m, index, author, kind)
    };
    vec![
        part(
            "clarify.request",
            agent(),
            EventKind::ClarifyRequest {
                request_id: request_id.into(),
                questions,
            },
        ),
        part(
            "clarify.response",
            Author::User { id: Uuid::nil() },
            EventKind::ClarifyResponse {
                request_id: request_id.into(),
                answers,
            },
        ),
    ]
}

/// A multi-select answer back into its parts: labels may hold ", " themselves, so at each
/// point the longest label followed by the end or ", " is taken; where none fits, the rest is
/// one part, the text typed as Other (the client shows parts that are not options there).
fn split_labels(answer: &str, labels: &[String]) -> Vec<String> {
    // The split is quadratic in the answer's length; real answers are short (at most 332
    // characters and 4 options in 2147 answered cards), so a longer one stays whole.
    if answer.len() > 4096 || labels.len() > 32 {
        return vec![answer.to_string()];
    }
    let mut longest_first: Vec<&String> = labels.iter().collect();
    longest_first.sort_by_key(|l| std::cmp::Reverse(l.len()));
    let mut parts = Vec::new();
    let mut rest = answer;
    while !rest.is_empty() {
        let label = longest_first.iter().find(|l| {
            rest.strip_prefix(l.as_str())
                .is_some_and(|after| after.is_empty() || after.starts_with(", "))
        });
        let Some(label) = label else {
            parts.push(rest.to_string());
            break;
        };
        parts.push(label.to_string());
        rest = &rest[label.len()..];
        rest = rest.strip_prefix(", ").unwrap_or(rest);
    }
    parts
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
/// arguments; for a message typed while a turn ran, its first text block too. `None` for other
/// records.
pub fn typed_text(record: &Record) -> Option<String> {
    let blocks = match record {
        Record::User(m) if m.is_main() => m.blocks(),
        Record::Attachment(a) => a.queued_prompt()?,
        _ => return None,
    };
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
/// `user`/`assistant` record or a message typed while a turn ran that maps to at least one
/// event. A signature-only thinking record, which stream-json may not repeat, is not evidence
/// of another writer.
pub fn counted_uuid(record: &Record) -> Option<Uuid> {
    match record {
        Record::User(m) | Record::Assistant(m)
            if m.is_main() && !record_events(record, None).is_empty() =>
        {
            Some(m.uuid)
        }
        Record::Attachment(a) if !record_events(record, None).is_empty() => a.uuid,
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

    /// A message typed while a turn ran and an answered `AskUserQuestion`, in 2.1.318 shapes.
    /// The message is the user's, at the time it was typed, one event per text block with the
    /// block's id; a peer's message and a task's notification stay without events. The answer
    /// is the tool result, then the question card and its answers with ids of their own, a
    /// multi-select answer split by its labels; a declined question is the error result only.
    #[test]
    fn typed_during_a_turn_and_answered_questions() {
        use hqc_proto::event::ClarifyQuestion;
        use serde_json::json;
        let rec = |v: serde_json::Value| hqc_claude_jsonl::parse_line(&v.to_string()).unwrap();
        let queued_command = |a: serde_json::Value| {
            let uuid = Uuid::new_v4();
            let r = rec(json!({"parentUuid": Uuid::new_v4(), "isSidechain": false,
                "type": "attachment", "uuid": uuid, "timestamp": "2026-11-08T20:56:46.218Z",
                "attachment": a, "renderedRole": "system", "version": "2.1.318"}));
            (r, uuid)
        };
        let (typed, uuid) = queued_command(json!({"type": "queued_command", "prompt": [
                {"type": "text", "text": "Skip the tests folder"},
                {"type": "image",
                 "source": {"type": "base64", "media_type": "image/png", "data": ""}},
                {"type": "text", "text": "  "},
                {"type": "text", "text": "and the docs"}],
            "source_uuid": Uuid::new_v4(), "commandMode": "prompt", "origin": {"kind": "human"},
            "timestamp": "2026-11-08T20:56:46.218Z", "humanTurn": true}));
        let evs = record_events(&typed, None);
        assert_eq!(
            evs.iter().map(label).collect::<Vec<_>>(),
            ["user:Skip the tests folder", "user:and the docs"]
        );
        assert_eq!(
            evs.iter().map(|e| e.event_id).collect::<Vec<_>>(),
            [
                Event::id_from_claude_block(&uuid, 0),
                Event::id_from_claude_block(&uuid, 3)
            ]
        );
        assert_eq!(evs[0].ts.to_rfc3339(), "2026-11-08T20:56:46.218+00:00");
        assert_eq!(evs[0].author, Author::User { id: Uuid::nil() });
        assert_eq!(evs[0].delivery, Some(Delivery::Delivered));
        assert_eq!(typed_text(&typed).as_deref(), Some("Skip the tests folder"));
        assert_eq!(counted_uuid(&typed), Some(uuid));
        for other in [
            json!({"type": "queued_command",
                "prompt": "<cross-session-message>hi</cross-session-message>",
                "commandMode": "prompt", "origin": {"kind": "peer"}, "isMeta": true}),
            json!({"type": "queued_command", "prompt": "<task-notification></task-notification>",
                "commandMode": "task-notification", "origin": {"kind": "task-notification"}}),
        ] {
            let (r, _) = queued_command(other);
            assert!(record_events(&r, None).is_empty(), "{r:?}");
            assert_eq!((typed_text(&r), counted_uuid(&r)), (None, None));
        }

        let answer = |content: &str, error: bool, result: serde_json::Value| {
            let block = json!({"type": "tool_result", "content": content, "is_error": error,
                "tool_use_id": "toolu_01"});
            let v = json!({"parentUuid": Uuid::new_v4(), "isSidechain": false, "type": "user",
                "uuid": Uuid::new_v4(), "timestamp": "2026-11-08T23:55:39.726Z",
                "message": {"role": "user", "content": [block]},
                "toolUseResult": result, "version": "2.1.318"});
            rec(v)
        };
        let option =
            |label: &str, description: &str| json!({"label": label, "description": description});
        let answered = answer(
            "Your questions have been answered: \"Which parts?\"=\"Feed, Search\". …",
            false,
            json!({"questions": [
                {"question": "Which parts?", "header": "Parts", "multiSelect": true, "options": [
                    option("Feed", "The chat feed"), option("Search", ""), option("Settings", "")]},
                {"question": "Which order?", "header": "Order", "multiSelect": false, "options": [
                    option("Newest first", ""), option("Oldest first", "")]},
                {"question": "Which checks?", "header": "Checks", "multiSelect": true, "options": [
                    option("Always on", ""), option("Always on, with references", ""),
                    option("Service, twice a year", "")]},
                {"question": "Which else?", "header": "Else", "multiSelect": true, "options": [
                    option("Feed", ""), option("Search", "")]}],
                "answers": {"Which parts?": "Feed, Search", "Which order?": "By relevance",
                    "Which checks?": "Always on, with references, Service, twice a year",
                    "Which else?": "Feed, my own"}}),
        );
        let Record::User(m) = &answered else {
            unreachable!()
        };
        let evs = record_events(&answered, None);
        let [result, request, response] = &evs[..] else {
            panic!("{evs:?}")
        };
        assert_eq!(label(result), "result:false");
        assert_eq!(result.event_id, Event::id_from_claude_block(&m.uuid, 0));
        let EventKind::ClarifyRequest {
            request_id,
            questions,
        } = &request.kind
        else {
            panic!("{request:?}")
        };
        assert_eq!(request_id, "toolu_01");
        assert_eq!(
            questions[0],
            ClarifyQuestion {
                id: "1".into(),
                text: "Which parts?".into(),
                header: Some("Parts".into()),
                options: Some(vec!["Feed".into(), "Search".into(), "Settings".into()]),
                option_descriptions: Some(vec!["The chat feed".into(), "".into(), "".into()]),
                multi: Some(true),
            }
        );
        assert_eq!(
            questions.iter().map(|q| q.id.as_str()).collect::<Vec<_>>(),
            ["1", "2", "3", "4"]
        );
        assert_eq!(
            response.kind,
            EventKind::ClarifyResponse {
                request_id: "toolu_01".into(),
                answers: [
                    (
                        "1".to_string(),
                        ClarifyAnswer::Many(vec!["Feed".into(), "Search".into()])
                    ),
                    ("2".to_string(), ClarifyAnswer::One("By relevance".into())),
                    // Labels that hold ", " themselves, the longest one first.
                    (
                        "3".to_string(),
                        ClarifyAnswer::Many(vec![
                            "Always on, with references".into(),
                            "Service, twice a year".into()
                        ])
                    ),
                    // A label and the text typed as Other.
                    (
                        "4".to_string(),
                        ClarifyAnswer::Many(vec!["Feed".into(), "my own".into()])
                    ),
                ]
                .into(),
            }
        );
        assert_eq!(
            (request.event_id, response.event_id),
            (
                Event::id_from_claude_part(&m.uuid, 0, "clarify.request"),
                Event::id_from_claude_part(&m.uuid, 0, "clarify.response")
            )
        );
        assert_eq!(
            (&request.author, &response.author),
            (&agent(), &Author::User { id: Uuid::nil() })
        );
        assert_eq!(response.ts.to_rfc3339(), "2026-11-08T23:55:39.726+00:00");

        let declined = answer(
            "The user doesn't want to proceed with this tool use.",
            true,
            json!("User rejected tool use"),
        );
        assert_eq!(
            record_events(&declined, None)
                .iter()
                .map(label)
                .collect::<Vec<_>>(),
            ["result:true"]
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
