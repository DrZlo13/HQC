//! Worker <-> hub frames, SPEC §5. Every frame is `{id, type, payload}`; replies reuse `id`.
//! Only the envelope and the frames needed for iteration 1 are typed here; add the rest
//! as the corresponding tasks land (docs/ITERATION-1.md).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event::Event;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub id: String,
    #[serde(flatten)]
    pub body: T,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

/// Frames sent by the worker (SPEC §5.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum WorkerFrame {
    Hello(Hello),
    /// `event.session_id` identifies the session; it is not repeated in the frame (SPEC §5.4).
    Event {
        event: Event,
    },
    #[serde(rename = "session.state")]
    SessionState {
        session_id: Uuid,
        process_state: ProcessState,
        runtime_session_id: Option<String>,
        locked_by: LockedBy,
    },
    Ack(serde_json::Value),
    Result(serde_json::Value),
    Error(WireError),
}

/// Frames sent by the hub (SPEC §5.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum HubFrame {
    #[serde(rename = "hello.ack")]
    HelloAck {
        protocol_version: u32,
    },
    #[serde(rename = "session.start")]
    SessionStart(SessionStart),
    #[serde(rename = "session.input")]
    SessionInput(SessionInput),
    #[serde(rename = "session.stop")]
    SessionStop {
        session_id: Uuid,
    },
    #[serde(rename = "session.kill")]
    SessionKill {
        session_id: Uuid,
    },
    #[serde(rename = "approval.response")]
    ApprovalResponse {
        session_id: Uuid,
        request_id: String,
        option_id: String,
    },
    Ack {
        event_id: Uuid,
        seq: u64,
    },
    Error(WireError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    pub capabilities: serde_json::Value,
    #[serde(default)]
    pub environments: Vec<String>,
    #[serde(default)]
    pub env_bindings: Vec<serde_json::Value>,
    #[serde(default)]
    pub allowed_roots: Vec<String>,
    #[serde(default)]
    pub allow_foreign_sessions: bool,
    #[serde(default)]
    pub share_local_models: bool,
    #[serde(default)]
    pub local_models: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    ClaudeCode,
    Acp,
    Bare,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Stopped,
    Starting,
    Running,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockedBy {
    None,
    Hqc,
    Vscode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStart {
    pub session_id: Uuid,
    pub runtime: Runtime,
    pub acp_agent: Option<String>,
    pub model: Option<String>,
    pub workspace_path: Option<String>,
    pub cwd: String,
    pub environment: String,
    pub runtime_session_id: Option<String>,
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
    pub restricted: bool,
    pub auto_approve: bool,
    pub session_token: String,
    pub owner_is_worker_owner: bool,
    pub ask_timeout_s: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInput {
    pub session_id: Uuid,
    pub event_id: Uuid,
    pub text: String,
    #[serde(default)]
    pub artifact_ids: Vec<Uuid>,
    #[serde(default)]
    pub peer_prefix: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_frame_wire_names() {
        let f = Envelope {
            id: "1".into(),
            body: HubFrame::SessionStop {
                session_id: Uuid::nil(),
            },
        };
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["type"], "session.stop");
        assert_eq!(v["id"], "1");
    }
}
