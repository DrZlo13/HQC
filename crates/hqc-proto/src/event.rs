//! Event schema, SPEC §3 (Event) and §4.
//!
//! Optional fields (`?` in the spec) are omitted from the wire when absent; `null` is
//! accepted too. Events are tolerant (SPEC §4): unknown payload fields are ignored, an
//! unknown `type` becomes [`EventKind::Unknown`] and keeps its payload, an unknown value of
//! an enum inside a payload becomes that enum's `Unknown`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

/// Namespace for deterministic event ids derived from Claude Code jsonl record uuids
/// (SPEC §3 Event, §9.2). `Uuid::new_v5(&CLAUDE_RECORD_NS, record_uuid.as_bytes())`.
pub const CLAUDE_RECORD_NS: Uuid = Uuid::from_bytes([
    0x9e, 0x1b, 0x7a, 0x52, 0x3c, 0x4d, 0x4f, 0x6e, 0x8a, 0x1f, 0x2b, 0x3c, 0x4d, 0x5e, 0x6f, 0x70,
]);

/// `{kind: user|agent|system, id}`; a system author has no `id` (SPEC §3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Author {
    User {
        id: Uuid,
    },
    /// `id` is the session id of the agent (SPEC §3).
    Agent {
        id: Uuid,
    },
    System,
}

/// Chain bookkeeping for agent->agent traffic (SPEC §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chain {
    pub chain_id: Uuid,
    pub depth: u8,
    pub hops: u8,
}

/// Delivery of `user.message` / `peer.message` to the runtime (SPEC §3 Event, §5.2).
/// The timestamp is the sibling field `Event::delivered_at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Pending,
    Delivered,
    Failed,
}

/// Session state as computed by the hub (SPEC §17.1), also used in `status` events.
/// Variants are listed in priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Archived,
    LockedVscode,
    WorkerOffline,
    Waiting,
    Error,
    Running,
    Queued,
    Idle,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalCanonical {
    Once,
    Session,
    Always,
    Deny,
    DenyAlways,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalOption {
    pub id: String,
    pub label: String,
    pub canonical: ApprovalCanonical,
}

/// `approval.request.policy_hint`: a rule of the worker's `[permissions]` matches the call
/// (SPEC §8.1, §14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyHint {
    Allow,
    Deny,
    #[serde(other)]
    Unknown,
}

/// Why an open approval or clarify request was closed without an answer (SPEC §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    Timeout,
    Stopped,
    WorkerRestart,
    Archived,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerMode {
    Send,
    Ask,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    File,
    Image,
    Diff,
    Html,
    Text,
    #[serde(other)]
    Unknown,
}

/// One question of `clarify.request` (SPEC §4). The Claude Code adapter maps `id` to the
/// question text that `AskUserQuestion` expects in `answers` (SPEC §8.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClarifyQuestion {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi: Option<bool>,
}

/// An answer in `clarify.response.answers`: one string, or several for `multi` questions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ClarifyAnswer {
    One(String),
    Many(Vec<String>),
}

/// Event payloads, SPEC §4. The `type` discriminator uses the spec's dotted names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum EventKind {
    #[serde(rename = "user.message")]
    UserMessage {
        text: String,
        #[serde(default)]
        artifact_ids: Vec<Uuid>,
    },
    /// Transport only, never stored (SPEC §4, §5.2).
    #[serde(rename = "agent.delta")]
    AgentDelta { text: String },
    #[serde(rename = "agent.message")]
    AgentMessage {
        text: String,
        r#final: bool,
        /// `event_id` of the input that started the turn.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        in_reply_to: Option<Uuid>,
    },
    #[serde(rename = "agent.thinking")]
    AgentThinking { text: String },
    #[serde(rename = "tool.call")]
    ToolCall {
        call_id: String,
        tool: String,
        args_preview: String,
    },
    #[serde(rename = "tool.result")]
    ToolResult {
        call_id: String,
        result_preview: String,
        error: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_id: Option<Uuid>,
    },
    #[serde(rename = "approval.request")]
    ApprovalRequest {
        request_id: String,
        tool: String,
        description: String,
        options: Vec<ApprovalOption>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<DateTime<Utc>>,
        /// What the worker's `[permissions]` say about this call (SPEC §8.1, §14); absent
        /// when no rule matches.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        policy_hint: Option<PolicyHint>,
    },
    #[serde(rename = "approval.response")]
    ApprovalResponse {
        request_id: String,
        option_id: String,
        /// A user id or `auto`.
        by: String,
        /// The user's text with a deny option: "Reply with text instead" (T26). It
        /// goes to the runtime with the denial; the client shows it as the user's message.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    #[serde(rename = "approval.cancel")]
    ApprovalCancel {
        request_id: String,
        reason: CancelReason,
    },
    #[serde(rename = "clarify.request")]
    ClarifyRequest {
        request_id: String,
        questions: Vec<ClarifyQuestion>,
    },
    /// `answers` is keyed by `ClarifyQuestion::id`.
    #[serde(rename = "clarify.response")]
    ClarifyResponse {
        request_id: String,
        answers: BTreeMap<String, ClarifyAnswer>,
    },
    #[serde(rename = "clarify.cancel")]
    ClarifyCancel {
        request_id: String,
        reason: CancelReason,
    },
    #[serde(rename = "attachment")]
    Attachment {
        artifact_id: Uuid,
        kind: ArtifactKind,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
    #[serde(rename = "peer.message")]
    PeerMessage {
        from_session_id: Uuid,
        /// The sender's address `<project-slug>/<session-slug>` when it sent (since 0.34).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        text: String,
        mode: PeerMode,
        /// Only for `mode: ask`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    #[serde(rename = "usage")]
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_read: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_write: Option<u64>,
        /// Cost of this turn (SPEC §4); Claude Code: the growth of `total_cost_usd`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        /// The runtime's cumulative cost of the whole runtime session after this turn
        /// (Claude Code: `result.total_cost_usd`, carried over `--resume`), if it reports one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_cost_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    #[serde(rename = "status")]
    Status {
        state: SessionState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    #[serde(rename = "system")]
    System {
        text: String,
        code: String,
        /// Only with [`SESSION_FORKED`]: its fields sit next to `text` and `code`.
        #[serde(flatten, default, skip_serializing_if = "Option::is_none")]
        fork: Option<SessionForked>,
    },
    /// An event type this build does not know; stored and passed on as is, not rendered.
    /// Never matches a known type: a known type with a bad payload is an error.
    #[serde(untagged)]
    Unknown {
        #[serde(deserialize_with = "unknown_type")]
        r#type: String,
        #[serde(default)]
        payload: serde_json::Value,
    },
}

impl EventKind {
    /// `type` values of the known variants, in SPEC §4 order.
    pub const TYPES: &'static [&'static str] = &[
        "user.message",
        "agent.delta",
        "agent.message",
        "agent.thinking",
        "tool.call",
        "tool.result",
        "approval.request",
        "approval.response",
        "approval.cancel",
        "clarify.request",
        "clarify.response",
        "clarify.cancel",
        "attachment",
        "peer.message",
        "usage",
        "status",
        "system",
    ];
}

/// Serde falls back to the untagged `Unknown` on any error of the tagged variants;
/// rejecting known names here keeps a malformed known event an error.
fn unknown_type<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let t = String::deserialize(d)?;
    if EventKind::TYPES.contains(&t.as_str()) {
        return Err(serde::de::Error::custom(format!(
            "invalid payload for event type `{t}`"
        )));
    }
    Ok(t)
}

/// A stored event (SPEC §3 Event). `seq` is assigned by the hub and absent on the wire
/// from worker to hub.
///
/// For the hub's logic only: the hub stores and serves the payload as received and never
/// writes it back from this type, since `Unknown` enum values do not round-trip (SPEC §5.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub session_id: Uuid,
    pub event_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    pub ts: DateTime<Utc>,
    pub author: Author,
    #[serde(flatten)]
    pub kind: EventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<Chain>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_op_id: Option<String>,
    /// Only on `user.message` and `peer.message`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<DateTime<Utc>>,
}

/// `system.code` of a fork in a Claude Code transcript (SPEC §4, §9.3). The payload carries,
/// besides `text` and `code`, the fields of [`SessionForked`].
pub const SESSION_FORKED: &str = "session_forked";

/// Extra payload of `system session_forked` (SPEC §4, §9.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionForked {
    /// The record that got a second line of turns.
    pub at_uuid: Uuid,
    /// Who started the later line: `vscode` (anyone outside hqc) or `hqc`.
    pub branch_from: String,
    /// Turns (prompts) of the other line, missing from the context of the new one.
    pub orphaned_turns: u32,
}

impl Event {
    /// Id of `system session_forked` at record `at_uuid`: one event per fork point, whoever
    /// reports it and however often (SPEC §9.3).
    pub fn id_of_fork(at_uuid: &Uuid) -> Uuid {
        Uuid::new_v5(&CLAUDE_RECORD_NS, format!("{at_uuid}/fork").as_bytes())
    }

    /// Deterministic event id for an event derived from a Claude Code jsonl record (SPEC §3).
    pub fn id_from_claude_record(record_uuid: &Uuid) -> Uuid {
        Uuid::new_v5(&CLAUDE_RECORD_NS, record_uuid.as_bytes())
    }

    /// Event id of content block `index` of a jsonl record (SPEC §3 Event, §9.2): block 0 is
    /// [`Event::id_from_claude_record`], block `i > 0` v5 of `"<uuid>/<i>"`. The adapter,
    /// the mirror and the import all use it, so one fact from two sources has one id.
    pub fn id_from_claude_block(record_uuid: &Uuid, index: usize) -> Uuid {
        if index == 0 {
            Self::id_from_claude_record(record_uuid)
        } else {
            Uuid::new_v5(
                &CLAUDE_RECORD_NS,
                format!("{record_uuid}/{index}").as_bytes(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// Every `type` from the SPEC §4 table, written out by hand.
    const SPEC_EVENT_TYPES: &[&str] = &[
        "user.message",
        "agent.delta",
        "agent.message",
        "agent.thinking",
        "tool.call",
        "tool.result",
        "approval.request",
        "approval.response",
        "approval.cancel",
        "clarify.request",
        "clarify.response",
        "clarify.cancel",
        "attachment",
        "peer.message",
        "usage",
        "status",
        "system",
    ];

    /// Wire examples shared with the TS client (`examples/wire/event.<type>.json`).
    const EXAMPLES: &[(&str, &str)] = &[
        (
            "user.message",
            include_str!("../examples/wire/event.user.message.json"),
        ),
        (
            "agent.delta",
            include_str!("../examples/wire/event.agent.delta.json"),
        ),
        (
            "agent.message",
            include_str!("../examples/wire/event.agent.message.json"),
        ),
        (
            "agent.thinking",
            include_str!("../examples/wire/event.agent.thinking.json"),
        ),
        (
            "tool.call",
            include_str!("../examples/wire/event.tool.call.json"),
        ),
        (
            "tool.result",
            include_str!("../examples/wire/event.tool.result.json"),
        ),
        (
            "approval.request",
            include_str!("../examples/wire/event.approval.request.json"),
        ),
        (
            "approval.response",
            include_str!("../examples/wire/event.approval.response.json"),
        ),
        (
            "approval.cancel",
            include_str!("../examples/wire/event.approval.cancel.json"),
        ),
        (
            "clarify.request",
            include_str!("../examples/wire/event.clarify.request.json"),
        ),
        (
            "clarify.response",
            include_str!("../examples/wire/event.clarify.response.json"),
        ),
        (
            "clarify.cancel",
            include_str!("../examples/wire/event.clarify.cancel.json"),
        ),
        (
            "attachment",
            include_str!("../examples/wire/event.attachment.json"),
        ),
        (
            "peer.message",
            include_str!("../examples/wire/event.peer.message.json"),
        ),
        ("usage", include_str!("../examples/wire/event.usage.json")),
        ("status", include_str!("../examples/wire/event.status.json")),
        ("system", include_str!("../examples/wire/event.system.json")),
    ];

    const UNKNOWN_EXAMPLE: &str = include_str!("../examples/wire/event.unknown.json");

    /// Exhaustive on purpose: a new variant does not compile until it gets a spec name here.
    fn spec_type(k: &EventKind) -> Option<&'static str> {
        Some(match k {
            EventKind::UserMessage { .. } => "user.message",
            EventKind::AgentDelta { .. } => "agent.delta",
            EventKind::AgentMessage { .. } => "agent.message",
            EventKind::AgentThinking { .. } => "agent.thinking",
            EventKind::ToolCall { .. } => "tool.call",
            EventKind::ToolResult { .. } => "tool.result",
            EventKind::ApprovalRequest { .. } => "approval.request",
            EventKind::ApprovalResponse { .. } => "approval.response",
            EventKind::ApprovalCancel { .. } => "approval.cancel",
            EventKind::ClarifyRequest { .. } => "clarify.request",
            EventKind::ClarifyResponse { .. } => "clarify.response",
            EventKind::ClarifyCancel { .. } => "clarify.cancel",
            EventKind::Attachment { .. } => "attachment",
            EventKind::PeerMessage { .. } => "peer.message",
            EventKind::Usage { .. } => "usage",
            EventKind::Status { .. } => "status",
            EventKind::System { .. } => "system",
            EventKind::Unknown { .. } => return None,
        })
    }

    /// T26: a denial with the user's text; without `message` the field is absent.
    #[test]
    fn approval_response_message_roundtrips() {
        let src = include_str!("../examples/wire/event.approval.response.message.json");
        let wire: Value = serde_json::from_str(src).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        let EventKind::ApprovalResponse { message, .. } = &e.kind else {
            panic!("not approval.response: {:?}", e.kind);
        };
        assert_eq!(
            message.as_deref(),
            Some("Leave the root alone, look only at ~/work/hqc")
        );
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
    }

    #[test]
    fn known_types_match_spec() {
        assert_eq!(EventKind::TYPES, SPEC_EVENT_TYPES);
        let names: Vec<&str> = EXAMPLES.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, SPEC_EVENT_TYPES);
    }

    #[test]
    fn examples_roundtrip_exactly() {
        for (name, src) in EXAMPLES {
            let wire: Value = serde_json::from_str(src).unwrap();
            let e: Event =
                serde_json::from_value(wire.clone()).unwrap_or_else(|err| panic!("{name}: {err}"));
            assert_eq!(spec_type(&e.kind), Some(*name));
            let back = serde_json::to_value(&e).unwrap();
            assert_eq!(back, wire, "{name}: wire form changed");
            assert_eq!(back["type"], *name);
            assert_eq!(serde_json::from_value::<Event>(back).unwrap(), e);
        }
    }

    #[test]
    fn unknown_type_is_kept_as_is() {
        let wire: Value = serde_json::from_str(UNKNOWN_EXAMPLE).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        let EventKind::Unknown { r#type, payload } = &e.kind else {
            panic!("expected Unknown, got {:?}", e.kind);
        };
        assert!(!EventKind::TYPES.contains(&r#type.as_str()));
        assert_eq!(payload, &wire["payload"]);
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
    }

    #[test]
    fn malformed_known_event_is_an_error() {
        for (name, src) in EXAMPLES {
            let mut wire: Value = serde_json::from_str(src).unwrap();
            wire["payload"] = json!({"bogus": true});
            assert!(
                serde_json::from_value::<Event>(wire).is_err(),
                "{name}: bad payload must not become Unknown"
            );
        }
    }

    #[test]
    fn unknown_enum_values_are_tolerated() {
        let v = |s: &str| Value::String(s.into());
        assert_eq!(
            serde_json::from_value::<SessionState>(v("sleeping")).unwrap(),
            SessionState::Unknown
        );
        assert_eq!(
            serde_json::from_value::<ApprovalCanonical>(v("ask_later")).unwrap(),
            ApprovalCanonical::Unknown
        );
        assert_eq!(
            serde_json::from_value::<CancelReason>(v("superseded")).unwrap(),
            CancelReason::Unknown
        );
        assert_eq!(
            serde_json::from_value::<PeerMode>(v("broadcast")).unwrap(),
            PeerMode::Unknown
        );
        assert_eq!(
            serde_json::from_value::<ArtifactKind>(v("audio")).unwrap(),
            ArtifactKind::Unknown
        );
    }

    #[test]
    fn enum_wire_values() {
        let states = [
            (SessionState::Archived, "archived"),
            (SessionState::LockedVscode, "locked_vscode"),
            (SessionState::WorkerOffline, "worker_offline"),
            (SessionState::Waiting, "waiting"),
            (SessionState::Error, "error"),
            (SessionState::Running, "running"),
            (SessionState::Queued, "queued"),
            (SessionState::Idle, "idle"),
        ];
        for (s, n) in states {
            assert_eq!(serde_json::to_value(s).unwrap(), n);
        }
        let reasons = [
            (CancelReason::Timeout, "timeout"),
            (CancelReason::Stopped, "stopped"),
            (CancelReason::WorkerRestart, "worker_restart"),
            (CancelReason::Archived, "archived"),
        ];
        for (r, n) in reasons {
            assert_eq!(serde_json::to_value(r).unwrap(), n);
        }
        let canon = [
            (ApprovalCanonical::Once, "once"),
            (ApprovalCanonical::Session, "session"),
            (ApprovalCanonical::Always, "always"),
            (ApprovalCanonical::Deny, "deny"),
            (ApprovalCanonical::DenyAlways, "deny_always"),
        ];
        for (c, n) in canon {
            assert_eq!(serde_json::to_value(c).unwrap(), n);
        }
        for (d, n) in [
            (Delivery::Pending, "pending"),
            (Delivery::Delivered, "delivered"),
            (Delivery::Failed, "failed"),
        ] {
            assert_eq!(serde_json::to_value(d).unwrap(), n);
        }
    }

    #[test]
    fn author_wire_shape() {
        let id = Uuid::nil();
        assert_eq!(
            serde_json::to_value(Author::User { id }).unwrap(),
            json!({"kind": "user", "id": id})
        );
        assert_eq!(
            serde_json::to_value(Author::Agent { id }).unwrap(),
            json!({"kind": "agent", "id": id})
        );
        assert_eq!(
            serde_json::to_value(Author::System).unwrap(),
            json!({"kind": "system"})
        );
    }

    #[test]
    fn unknown_payload_fields_are_ignored() {
        let e: Event = serde_json::from_value(json!({
            "session_id": Uuid::nil(), "event_id": Uuid::nil(),
            "ts": "2026-10-22T10:00:00Z", "author": {"kind": "system"},
            "type": "system", "payload": {"text": "t", "code": "c", "extra": 1}
        }))
        .unwrap();
        assert_eq!(
            e.kind,
            EventKind::System {
                text: "t".into(),
                code: "c".into(),
                fork: None
            }
        );
    }

    #[test]
    fn claude_record_id_is_stable() {
        let u = Uuid::parse_str("36c72c19-b4ad-4d95-a482-ce4e281952cb").unwrap();
        assert_eq!(
            Event::id_from_claude_record(&u),
            Event::id_from_claude_record(&u)
        );
        assert_ne!(Event::id_from_claude_record(&u), u);
    }
}
