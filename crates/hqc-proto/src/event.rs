//! Event schema, SPEC §3 (Event) and §4.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Namespace for deterministic event ids derived from Claude Code jsonl record uuids
/// (SPEC §3 Event, §9.2). `Uuid::new_v5(&CLAUDE_RECORD_NS, record_uuid.as_bytes())`.
pub const CLAUDE_RECORD_NS: Uuid = Uuid::from_bytes([
    0x9e, 0x1b, 0x7a, 0x52, 0x3c, 0x4d, 0x4f, 0x6e, 0x8a, 0x1f, 0x2b, 0x3c, 0x4d, 0x5e, 0x6f, 0x70,
]);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Pending,
    Delivered,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivery {
    pub state: DeliveryState,
    pub delivered_at: Option<DateTime<Utc>>,
}

/// Session state as computed by the hub (SPEC §17.1), also used in `status` events.
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalCanonical {
    Once,
    Session,
    Always,
    Deny,
    DenyAlways,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalOption {
    pub id: String,
    pub label: String,
    pub canonical: ApprovalCanonical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerMode {
    Send,
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    File,
    Image,
    Diff,
    Html,
    Text,
}

/// Event payloads, SPEC §4. The `type` discriminator uses the spec's dotted names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum EventKind {
    #[serde(rename = "user.message")]
    UserMessage {
        text: String,
        #[serde(default)]
        artifact_ids: Vec<Uuid>,
    },
    #[serde(rename = "agent.delta")]
    AgentDelta { text: String },
    #[serde(rename = "agent.message")]
    AgentMessage {
        text: String,
        r#final: bool,
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
        duration_ms: Option<u64>,
        artifact_id: Option<Uuid>,
    },
    #[serde(rename = "approval.request")]
    ApprovalRequest {
        request_id: String,
        tool: String,
        description: String,
        options: Vec<ApprovalOption>,
        expires_at: Option<DateTime<Utc>>,
    },
    #[serde(rename = "approval.response")]
    ApprovalResponse {
        request_id: String,
        option_id: String,
        by: String,
    },
    #[serde(rename = "approval.cancel")]
    ApprovalCancel { request_id: String, reason: String },
    #[serde(rename = "clarify.request")]
    ClarifyRequest {
        request_id: String,
        questions: serde_json::Value,
    },
    #[serde(rename = "clarify.response")]
    ClarifyResponse {
        request_id: String,
        answers: serde_json::Value,
    },
    #[serde(rename = "clarify.cancel")]
    ClarifyCancel { request_id: String, reason: String },
    #[serde(rename = "attachment")]
    Attachment {
        artifact_id: Uuid,
        kind: ArtifactKind,
        name: String,
        caption: Option<String>,
    },
    #[serde(rename = "peer.message")]
    PeerMessage {
        from_session_id: Uuid,
        text: String,
        mode: PeerMode,
        request_id: Option<String>,
    },
    #[serde(rename = "usage")]
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cache_read: u64,
        cache_write: u64,
        cost_usd: Option<f64>,
        model: String,
    },
    #[serde(rename = "status")]
    Status {
        state: SessionState,
        detail: Option<String>,
    },
    #[serde(rename = "system")]
    System { text: String, code: String },
}

/// A stored event (SPEC §3 Event). `seq` is assigned by the hub and absent on the wire
/// from worker to hub.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
}

impl Event {
    /// Deterministic event id for an event derived from a Claude Code jsonl record (SPEC §3).
    pub fn id_from_claude_record(record_uuid: &Uuid) -> Uuid {
        Uuid::new_v5(&CLAUDE_RECORD_NS, record_uuid.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_wire_shape() {
        let e = Event {
            session_id: Uuid::nil(),
            event_id: Uuid::nil(),
            seq: None,
            ts: Utc::now(),
            author: Author::System,
            kind: EventKind::AgentMessage {
                text: "hi".into(),
                r#final: true,
                in_reply_to: None,
            },
            chain: None,
            client_op_id: None,
            delivery: None,
        };
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["type"], "agent.message");
        assert_eq!(json["payload"]["final"], true);
        assert!(json.get("seq").is_none());
        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back, e);
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
