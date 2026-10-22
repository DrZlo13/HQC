//! Worker <-> hub frames, SPEC §5. Every frame is `{id, type, payload}`; a reply
//! (`ack`, `hello.ack`, `error`) reuses the `id` of the frame it answers.
//!
//! Frames are strict (SPEC §5.2): the protocol version is agreed in `hello`, and a frame
//! that does not parse is answered with `error` [`error_code::INVALID_FRAME`].
//!
//! Only the frames of iteration 1 are typed here (docs/ITERATION-1.md, T1). Later iterations:
//! hub -> worker `session.steer`, `workspace.*`, `mirror.set`, `model.proxy`,
//! `worker.approval.response`, `mcp.result`; worker -> hub `mirror.event`,
//! `worker.approval.request`, `mcp.call`, `mcp.cancel`, `result`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event::{ClarifyAnswer, Event};

/// `{id, type, payload}`; `body` supplies `type` and `payload`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub id: String,
    #[serde(flatten)]
    pub body: T,
}

impl<T> Envelope<T> {
    pub fn new(id: impl Into<String>, body: T) -> Self {
        Self {
            id: id.into(),
            body,
        }
    }

    /// A reply to this frame: same `id`, new body.
    pub fn reply<U>(&self, body: U) -> Envelope<U> {
        Envelope {
            id: self.id.clone(),
            body,
        }
    }
}

/// `ack {event_id?, seq?}` (SPEC §5, §5.2).
/// - to `event`: `{event_id, seq}`; `seq` of the stored event for a duplicate, absent when
///   the event was dropped for an unknown session;
/// - to `session.input`: `{event_id}` of the delivered input;
/// - otherwise `{}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

/// `error {code, message, retryable, details?}` (SPEC §5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireError {
    /// One of [`error_code`]; the list is open, an unknown code is treated as `internal`.
    pub code: String,
    pub message: String,
    pub retryable: bool,
    /// `{min_version}` for `protocol_version_unsupported`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Initial error codes (SPEC §5).
pub mod error_code {
    pub const PROTOCOL_VERSION_UNSUPPORTED: &str = "protocol_version_unsupported";
    pub const UNAUTHORIZED: &str = "unauthorized";
    pub const UNKNOWN_SESSION: &str = "unknown_session";
    pub const INVALID_FRAME: &str = "invalid_frame";
    pub const AGENT_START_FAILED: &str = "agent_start_failed";
    pub const INTERNAL: &str = "internal";
}

/// Frames sent by the worker (SPEC §5.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum WorkerFrame {
    #[serde(rename = "hello")]
    Hello(Hello),
    /// `event.session_id` identifies the session; it is not repeated in the frame (SPEC §5.4).
    #[serde(rename = "event")]
    Event { event: Event },
    #[serde(rename = "session.state")]
    SessionState {
        session_id: Uuid,
        process_state: ProcessState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        runtime_session_id: Option<String>,
        locked_by: LockedBy,
    },
    #[serde(rename = "ack")]
    Ack(Ack),
    #[serde(rename = "error")]
    Error(WireError),
}

/// Frames sent by the hub (SPEC §5.3).
///
/// There is no `approval.cancel` frame: when the hub closes a request itself (timeout,
/// answer from another device, archiving) it writes the `approval.cancel` event and sends
/// `approval.response` with the option whose `canonical` is `deny` (SPEC §5.3, §10.1).
/// A clarify request is closed the same way: `clarify.cancel` event plus `clarify.response`
/// with empty `answers`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum HubFrame {
    /// Reply to `hello`; a rejected `hello` gets `error` `protocol_version_unsupported`.
    #[serde(rename = "hello.ack")]
    HelloAck {
        protocol_version: u32,
        worker_id: Uuid,
        server_time: DateTime<Utc>,
    },
    #[serde(rename = "session.start")]
    SessionStart(SessionStart),
    #[serde(rename = "session.input")]
    SessionInput(SessionInput),
    /// Abort the current turn.
    #[serde(rename = "session.stop")]
    SessionStop { session_id: Uuid },
    /// Terminate the agent process.
    #[serde(rename = "session.kill")]
    SessionKill { session_id: Uuid },
    #[serde(rename = "approval.response")]
    ApprovalResponse {
        session_id: Uuid,
        request_id: String,
        option_id: String,
    },
    /// Empty `answers` means the hub cancelled the request; the adapter answers the
    /// runtime with `deny` and the turn goes on without answers.
    #[serde(rename = "clarify.response")]
    ClarifyResponse {
        session_id: Uuid,
        request_id: String,
        answers: BTreeMap<String, ClarifyAnswer>,
    },
    #[serde(rename = "ack")]
    Ack(Ack),
    #[serde(rename = "error")]
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

/// `session.start` (SPEC §5.3). Optional fields are omitted when absent;
/// `workspace_path` is explicitly `null` for a session without workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStart {
    pub session_id: Uuid,
    pub runtime: Runtime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_agent: Option<String>,
    /// Absent: the runtime's default model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub workspace_path: Option<String>,
    pub cwd: String,
    pub environment: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    /// `event_id` of the `user.message` / `peer.message` being delivered.
    pub event_id: Uuid,
    pub text: String,
    #[serde(default)]
    pub artifact_ids: Vec<Uuid>,
    /// Buffered `send` messages (SPEC §7.1); always empty in iteration 1.
    #[serde(default)]
    pub peer_prefix: Vec<PeerPrefix>,
}

/// One buffered `send` message in `session.input.peer_prefix` (SPEC §5.3, §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerPrefix {
    /// `<project-slug>/<session-slug>` of the sender.
    pub from: String,
    pub text: String,
    pub ts: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use serde_json::{json, Value};

    /// Frame `type` values of iteration 1, as written in SPEC §5.3 / §5.4.
    const SPEC_HUB_TYPES: &[&str] = &[
        "hello.ack",
        "session.start",
        "session.input",
        "session.stop",
        "session.kill",
        "approval.response",
        "clarify.response",
        "ack",
        "error",
    ];
    const SPEC_WORKER_TYPES: &[&str] = &["hello", "event", "session.state", "ack", "error"];

    /// Wire examples shared with the TS client (`examples/wire/{hub,worker}.<type>[.<case>].json`).
    const HUB_EXAMPLES: &[(&str, &str)] = &[
        (
            "hello.ack",
            include_str!("../examples/wire/hub.hello.ack.json"),
        ),
        (
            "session.start",
            include_str!("../examples/wire/hub.session.start.json"),
        ),
        (
            "session.input",
            include_str!("../examples/wire/hub.session.input.json"),
        ),
        (
            "session.stop",
            include_str!("../examples/wire/hub.session.stop.json"),
        ),
        (
            "session.kill",
            include_str!("../examples/wire/hub.session.kill.json"),
        ),
        (
            "approval.response",
            include_str!("../examples/wire/hub.approval.response.json"),
        ),
        (
            "clarify.response",
            include_str!("../examples/wire/hub.clarify.response.json"),
        ),
        (
            "clarify.response",
            include_str!("../examples/wire/hub.clarify.response.cancel.json"),
        ),
        ("ack", include_str!("../examples/wire/hub.ack.json")),
        (
            "ack",
            include_str!("../examples/wire/hub.ack.unknown-session.json"),
        ),
        ("error", include_str!("../examples/wire/hub.error.json")),
    ];
    const WORKER_EXAMPLES: &[(&str, &str)] = &[
        ("hello", include_str!("../examples/wire/worker.hello.json")),
        ("event", include_str!("../examples/wire/worker.event.json")),
        (
            "session.state",
            include_str!("../examples/wire/worker.session.state.json"),
        ),
        ("ack", include_str!("../examples/wire/worker.ack.json")),
        ("error", include_str!("../examples/wire/worker.error.json")),
    ];

    /// Exhaustive on purpose: a new variant does not compile until it gets a spec name here.
    fn hub_type(f: &HubFrame) -> &'static str {
        match f {
            HubFrame::HelloAck { .. } => "hello.ack",
            HubFrame::SessionStart(_) => "session.start",
            HubFrame::SessionInput(_) => "session.input",
            HubFrame::SessionStop { .. } => "session.stop",
            HubFrame::SessionKill { .. } => "session.kill",
            HubFrame::ApprovalResponse { .. } => "approval.response",
            HubFrame::ClarifyResponse { .. } => "clarify.response",
            HubFrame::Ack(_) => "ack",
            HubFrame::Error(_) => "error",
        }
    }

    fn worker_type(f: &WorkerFrame) -> &'static str {
        match f {
            WorkerFrame::Hello(_) => "hello",
            WorkerFrame::Event { .. } => "event",
            WorkerFrame::SessionState { .. } => "session.state",
            WorkerFrame::Ack(_) => "ack",
            WorkerFrame::Error(_) => "error",
        }
    }

    fn check_examples<T>(examples: &[(&str, &str)], spec: &[&str], type_of: fn(&T) -> &'static str)
    where
        T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let mut names: Vec<&str> = examples.iter().map(|(n, _)| *n).collect();
        names.dedup();
        assert_eq!(names, spec);
        for (name, src) in examples {
            let wire: Value = serde_json::from_str(src).unwrap();
            let keys: Vec<&String> = wire.as_object().unwrap().keys().collect();
            assert_eq!(keys, ["id", "payload", "type"], "{name}: envelope keys");
            let f: Envelope<T> =
                serde_json::from_value(wire.clone()).unwrap_or_else(|err| panic!("{name}: {err}"));
            assert_eq!(type_of(&f.body), *name);
            let back = serde_json::to_value(&f).unwrap();
            assert_eq!(back, wire, "{name}: wire form changed");
            assert_eq!(serde_json::from_value::<Envelope<T>>(back).unwrap(), f);
        }
    }

    #[test]
    fn hub_examples_roundtrip_exactly() {
        check_examples::<HubFrame>(HUB_EXAMPLES, SPEC_HUB_TYPES, hub_type);
    }

    #[test]
    fn worker_examples_roundtrip_exactly() {
        check_examples::<WorkerFrame>(WORKER_EXAMPLES, SPEC_WORKER_TYPES, worker_type);
    }

    #[test]
    fn frames_are_strict() {
        let unknown = json!({"id": "x", "type": "approval.cancel", "payload": {}});
        assert!(serde_json::from_value::<Envelope<HubFrame>>(unknown.clone()).is_err());
        assert!(serde_json::from_value::<Envelope<WorkerFrame>>(unknown).is_err());
    }

    #[test]
    fn reply_reuses_id() {
        let req = Envelope::new(
            "f-17",
            HubFrame::SessionStop {
                session_id: Uuid::nil(),
            },
        );
        let ack = req.reply(WorkerFrame::Ack(Ack::default()));
        assert_eq!(
            serde_json::to_value(&ack).unwrap(),
            json!({"id": "f-17", "type": "ack", "payload": {}})
        );
    }

    #[test]
    fn acks_answer_their_requests() {
        let input: Envelope<HubFrame> =
            serde_json::from_str(include_str!("../examples/wire/hub.session.input.json")).unwrap();
        let ack: Envelope<WorkerFrame> =
            serde_json::from_str(include_str!("../examples/wire/worker.ack.json")).unwrap();
        let HubFrame::SessionInput(si) = &input.body else {
            panic!("not session.input");
        };
        assert_eq!(ack.id, input.id);
        assert_eq!(
            ack.body,
            WorkerFrame::Ack(Ack {
                event_id: Some(si.event_id),
                seq: None
            })
        );

        let ev: Envelope<WorkerFrame> =
            serde_json::from_str(include_str!("../examples/wire/worker.event.json")).unwrap();
        let ack: Envelope<HubFrame> =
            serde_json::from_str(include_str!("../examples/wire/hub.ack.json")).unwrap();
        let WorkerFrame::Event { event } = &ev.body else {
            panic!("not an event frame");
        };
        assert_eq!(event.seq, None, "seq is assigned by the hub (SPEC §5.2)");
        assert_eq!(ack.id, ev.id);
        let HubFrame::Ack(a) = &ack.body else {
            panic!("not an ack");
        };
        assert_eq!(a.event_id, Some(event.event_id));
        assert!(a.seq.is_some());
    }

    #[test]
    fn null_seq_reads_as_absent() {
        let a: Ack = serde_json::from_value(json!({"event_id": Uuid::nil(), "seq": null})).unwrap();
        assert_eq!(a.seq, None);
    }
}
