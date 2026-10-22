//! HTTP bodies shared by the hub, the worker and the client (SPEC §5.1, §6).
//!
//! Requests ignore unknown fields, so an older hub accepts a newer worker's body.
//! Wire examples: `examples/wire/api.*.json`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::client_frame::SessionSummary;
use crate::event::SessionState;
use crate::frame::{LockedBy, ProcessState, Runtime};

/// A session as `GET /api/sessions/{id}`, the session list and `POST .../sessions` return
/// it (SPEC §3 Session, §6). `state`, `queue_len`, `last_read_seq` and `unread` are computed
/// by the hub for the requesting user (SPEC §17.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub project_id: Uuid,
    pub owner_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<Uuid>,
    pub cwd: String,
    pub title: String,
    pub slug: String,
    pub agent: Agent,
    pub auto_approve: bool,
    pub wake_on_send: bool,
    pub locked_by: LockedBy,
    pub last_seq: u64,
    pub archived: bool,
    pub state: SessionState,
    pub queue_len: u64,
    pub last_read_seq: u64,
    /// Same rule as [`SessionSummary::unread`].
    pub unread: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Session {
    /// The line of `summary` / `session.summary` (SPEC §6).
    pub fn summary(&self) -> SessionSummary {
        SessionSummary {
            session_id: self.id,
            project_id: self.project_id,
            title: self.title.clone(),
            state: self.state,
            queue_len: self.queue_len,
            unread: self.unread,
            last_seq: self.last_seq,
            last_read_seq: self.last_read_seq,
        }
    }
}

/// The agent embedded in a session (SPEC §3 Agent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub runtime: Runtime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_session_id: Option<String>,
    pub process_state: ProcessState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
    pub restricted: bool,
    pub start_failures: u32,
}

/// `GET /api/projects/{id}/sessions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionList {
    pub sessions: Vec<Session>,
}

/// `GET /api/sessions/{id}/events`: stored events as raw JSON, oldest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventPage {
    pub events: Vec<serde_json::Value>,
    pub last_seq: u64,
}

/// `POST /api/sessions/{id}/input`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputRequest {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub artifact_ids: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_op_id: Option<String>,
}

/// Reply to `POST /api/sessions/{id}/input`: the stored `user.message` (201), or for a
/// repeated `client_op_id` the event stored the first time (200).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputResponse {
    pub event: serde_json::Value,
}

/// Error body of every REST endpoint, with the matching HTTP status (SPEC §6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
    pub message: String,
}

/// `GET /api/push/vapid`; `null` when the hub has no VAPID keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VapidKey {
    pub public_key: Option<String>,
}

/// `POST /api/push/subscribe`: the browser's `PushSubscription.toJSON()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushSubscription {
    pub endpoint: String,
    #[serde(
        default,
        rename = "expirationTime",
        skip_serializing_if = "Option::is_none"
    )]
    pub expiration_time: Option<f64>,
    pub keys: PushKeys,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushKeys {
    pub p256dh: String,
    pub auth: String,
}

/// The Web Push message body (SPEC §16). `payload` is the event of SPEC §4 as stored; the
/// hub leaves it out when the message would not fit a push (about 4 KB). The service worker
/// derives `title` / `body` from `type` and `payload` when they are absent and shows
/// `approval.request` / `clarify.request` with `requireInteraction`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushMessage {
    pub session_id: Uuid,
    /// The event type.
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Notifications with the same tag replace each other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

/// `POST /api/workers/enroll/confirm`; case and the dash of `code` do not matter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollConfirmRequest {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `GET /api/workers/enroll/{code}`: what the worker that printed an unconfirmed code said
/// about itself, shown before the user confirms (SPEC §5.1, §6). `country` is Cloudflare's
/// `CF-IPCountry` of the enrolling request, when there was one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollInfo {
    pub name: String,
    pub os: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
}

/// Reply to `POST /api/workers/enroll/confirm`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollConfirmed {
    pub worker_id: Uuid,
    pub name: String,
}

/// `POST /worker/enroll` (SPEC §5.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollRequest {
    pub name: String,
    #[serde(default)]
    pub os: String,
    /// The worker's [`crate::PROTOCOL_VERSION`]; informational, `hello` decides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u32>,
}

/// Reply to `POST /worker/enroll`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollStarted {
    /// `XXXX-XXXX`, shown to the user; confirmed with `POST /api/workers/enroll/confirm`.
    pub code: String,
    /// The worker's secret for polling; the hub keeps only its hash.
    pub poll_token: String,
    pub expires_at: DateTime<Utc>,
    pub poll_interval_s: u32,
}

/// `POST /worker/enroll/poll`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollPollRequest {
    pub poll_token: String,
}

/// Reply to `POST /worker/enroll/poll`; the token is handed out exactly once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EnrollPoll {
    Pending,
    Confirmed { worker_id: Uuid, token: String },
    Expired,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use serde_json::Value;

    fn roundtrip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(src: &str) -> T {
        let wire: Value = serde_json::from_str(src).unwrap();
        let v: T = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&v).unwrap(), wire, "wire form changed");
        v
    }

    #[test]
    fn enroll_examples_roundtrip() {
        let r: EnrollRequest = roundtrip(include_str!("../examples/wire/api.enroll.request.json"));
        assert_eq!(r.protocol_version, Some(crate::PROTOCOL_VERSION));
        roundtrip::<EnrollStarted>(include_str!("../examples/wire/api.enroll.json"));
        roundtrip::<EnrollPollRequest>(include_str!(
            "../examples/wire/api.enroll.poll.request.json"
        ));
        let p: EnrollPoll = roundtrip(include_str!("../examples/wire/api.enroll.poll.json"));
        assert!(matches!(p, EnrollPoll::Confirmed { .. }));
        assert_eq!(
            serde_json::from_str::<EnrollPoll>(r#"{"status":"pending"}"#).unwrap(),
            EnrollPoll::Pending
        );
    }

    #[test]
    fn rest_examples_roundtrip() {
        let s: Session = roundtrip(include_str!("../examples/wire/api.session.json"));
        let summary: SessionSummary = serde_json::from_value(
            serde_json::from_str::<Value>(include_str!(
                "../examples/wire/client.session.summary.json"
            ))
            .unwrap()["session"]
                .clone(),
        )
        .unwrap();
        assert_eq!(
            s.summary(),
            summary,
            "summary is the session's compact form"
        );
        roundtrip::<EventPage>(include_str!("../examples/wire/api.events.json"));
        roundtrip::<InputRequest>(include_str!("../examples/wire/api.input.request.json"));
        let r: InputResponse = roundtrip(include_str!("../examples/wire/api.input.json"));
        let ev: crate::event::Event = serde_json::from_value(r.event).unwrap();
        assert!(ev.client_op_id.is_some() && ev.seq.is_some());
        roundtrip::<ApiError>(include_str!("../examples/wire/api.error.json"));
        roundtrip::<VapidKey>(include_str!("../examples/wire/api.vapid.json"));
        roundtrip::<PushSubscription>(include_str!("../examples/wire/api.push.subscribe.json"));
        let p: PushMessage = roundtrip(include_str!("../examples/wire/api.push.json"));
        let ev: crate::event::Event = serde_json::from_value(p.payload.unwrap()).unwrap();
        assert_eq!(ev.session_id, p.session_id);
        roundtrip::<EnrollConfirmRequest>(include_str!(
            "../examples/wire/api.enroll.confirm.request.json"
        ));
        roundtrip::<EnrollConfirmed>(include_str!("../examples/wire/api.enroll.confirm.json"));
        roundtrip::<EnrollInfo>(include_str!("../examples/wire/api.enroll.info.json"));
    }

    #[test]
    fn push_subscription_accepts_browser_null() {
        let s: PushSubscription = serde_json::from_str(
            r#"{"endpoint":"https://p/x","expirationTime":null,"keys":{"p256dh":"a","auth":"b"}}"#,
        )
        .unwrap();
        assert_eq!(s.expiration_time, None);
    }

    #[test]
    fn vapid_key_is_null_without_push() {
        assert_eq!(
            serde_json::to_string(&VapidKey { public_key: None }).unwrap(),
            r#"{"public_key":null}"#
        );
    }

    #[test]
    fn enroll_request_ignores_unknown_fields() {
        let r: EnrollRequest =
            serde_json::from_str(r#"{"name":"w","os":"linux","arch":"arm64"}"#).unwrap();
        assert_eq!(r.protocol_version, None);
    }
}
