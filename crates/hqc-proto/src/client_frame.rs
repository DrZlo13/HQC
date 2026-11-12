//! Client <-> hub frames on `/api/ws` (SPEC §6): flat JSON objects `{type, ...}` without
//! `id` and without a `payload` wrapper.
//!
//! The hub parses [`ClientFrame`] and sends [`ServerFrame`]; the TypeScript client reads the
//! same wire examples (`examples/wire/client.<type>.json`) as its contract. Unknown fields
//! are ignored in both directions; the client ignores server frames of unknown `type`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event::{Delivery, SessionState};

/// Client -> hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientFrame {
    /// Stored events with `seq > after_seq` (default 0), then `subscribed`, then the live
    /// stream of the session.
    #[serde(rename = "subscribe")]
    Subscribe {
        session_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_seq: Option<u64>,
    },
    #[serde(rename = "unsubscribe")]
    Unsubscribe { session_id: Uuid },
    /// `summary` with every session of the user, then `session.summary` on each change.
    #[serde(rename = "subscribe_summary")]
    SubscribeSummary {},
    #[serde(rename = "ping")]
    Ping {},
}

/// Hub -> client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerFrame {
    /// The event exactly as stored (SPEC §3 Event, raw JSON, SPEC §5.2); `agent.delta`
    /// comes without `seq` and is not stored.
    #[serde(rename = "event")]
    Event { event: serde_json::Value },
    /// The backlog after `after_seq` has been sent; the live stream follows.
    #[serde(rename = "subscribed")]
    Subscribed { session_id: Uuid, last_seq: u64 },
    /// `delivery` of a stored `user.message` / `peer.message` changed (SPEC §3 Event, §5.2).
    /// The event itself is not sent again.
    #[serde(rename = "delivery")]
    Delivery {
        session_id: Uuid,
        event_id: Uuid,
        seq: u64,
        delivery: Delivery,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivered_at: Option<DateTime<Utc>>,
        /// The input went into the turn already running (SPEC §17.2): the stored message got
        /// `payload.joined` with this change. Sent only when true.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        joined: bool,
    },
    /// Reply to `subscribe_summary`: every session of the user.
    #[serde(rename = "summary")]
    Summary { sessions: Vec<SessionSummary> },
    /// One session changed.
    #[serde(rename = "session.summary")]
    SessionSummary { session: SessionSummary },
    /// Every open request of the user (SPEC §6, §10.1): after `summary`, and again, whole,
    /// whenever a request opens or closes or an `hqc_ask` that waits on one does.
    #[serde(rename = "requests")]
    Requests {
        requests: Vec<crate::api::OpenRequest>,
    },
    #[serde(rename = "pong")]
    Pong {},
    #[serde(rename = "error")]
    Error {
        code: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<Uuid>,
    },
}

/// The compact session line of `summary` / `session.summary` (SPEC §6, §17.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: Uuid,
    pub project_id: Uuid,
    pub title: String,
    pub state: SessionState,
    /// Inputs with `delivery: pending`.
    pub queue_len: u64,
    /// Events after `last_read_seq` that count as unread (SPEC §6): `agent.message` with
    /// `final: true`, `approval.request`, `clarify.request`, `peer.message`, `system`.
    pub unread: u64,
    pub last_seq: u64,
    pub last_read_seq: u64,
    /// As in the session object: who outside hqc holds the transcript open (SPEC §9.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_by: Option<crate::frame::HeldBy>,
    /// As in the session object (T38): the mode hqc asks for and the mode the agent runs
    /// in, so the chat header follows a switch the worker confirms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<crate::frame::PermissionMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_permission_mode: Option<crate::frame::PermissionMode>,
    /// As in the session object (T39).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_capped_by: Option<String>,
    /// As in the session object (SPEC 0.68): the subagents the agent runs now; absent — 0.
    #[serde(default, skip_serializing_if = "crate::frame::is_zero")]
    pub subagents: u32,
    /// As in the session object (SPEC 0.104): the background tasks the runtime lists for the
    /// agent now; absent — 0.
    #[serde(default, skip_serializing_if = "crate::frame::is_zero")]
    pub background_tasks: u32,
    /// As in the session object: the output tokens per second of the agent and its
    /// subagents; absent — 0.
    #[serde(default, skip_serializing_if = "crate::frame::is_zero")]
    pub output_per_s: u32,
    /// As in the session object (SPEC 0.125): the calls the agent runs in the foreground now
    /// and can move to the background; absent — none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreground_calls: Vec<String>,
    /// As in the session object: the user's, so the list of favorites follows other devices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favorite_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use serde_json::{json, Value};

    /// `examples/wire/client.<type>[.<case>].json`, by direction.
    const CLIENT_TO_HUB: &[(&str, &str)] = &[
        (
            "subscribe",
            include_str!("../examples/wire/client.subscribe.json"),
        ),
        (
            "unsubscribe",
            include_str!("../examples/wire/client.unsubscribe.json"),
        ),
        (
            "subscribe_summary",
            include_str!("../examples/wire/client.subscribe_summary.json"),
        ),
        ("ping", include_str!("../examples/wire/client.ping.json")),
    ];
    const HUB_TO_CLIENT: &[(&str, &str)] = &[
        ("event", include_str!("../examples/wire/client.event.json")),
        (
            "event",
            include_str!("../examples/wire/client.event.delta.json"),
        ),
        (
            "subscribed",
            include_str!("../examples/wire/client.subscribed.json"),
        ),
        (
            "delivery",
            include_str!("../examples/wire/client.delivery.json"),
        ),
        (
            "delivery",
            include_str!("../examples/wire/client.delivery.joined.json"),
        ),
        (
            "summary",
            include_str!("../examples/wire/client.summary.json"),
        ),
        (
            "session.summary",
            include_str!("../examples/wire/client.session.summary.json"),
        ),
        (
            "requests",
            include_str!("../examples/wire/client.requests.json"),
        ),
        ("pong", include_str!("../examples/wire/client.pong.json")),
        ("error", include_str!("../examples/wire/client.error.json")),
    ];

    fn client_type(f: &ClientFrame) -> &'static str {
        match f {
            ClientFrame::Subscribe { .. } => "subscribe",
            ClientFrame::Unsubscribe { .. } => "unsubscribe",
            ClientFrame::SubscribeSummary {} => "subscribe_summary",
            ClientFrame::Ping {} => "ping",
        }
    }

    fn server_type(f: &ServerFrame) -> &'static str {
        match f {
            ServerFrame::Event { .. } => "event",
            ServerFrame::Subscribed { .. } => "subscribed",
            ServerFrame::Delivery { .. } => "delivery",
            ServerFrame::Summary { .. } => "summary",
            ServerFrame::SessionSummary { .. } => "session.summary",
            ServerFrame::Requests { .. } => "requests",
            ServerFrame::Pong {} => "pong",
            ServerFrame::Error { .. } => "error",
        }
    }

    fn check<T>(examples: &[(&str, &str)], type_of: fn(&T) -> &'static str)
    where
        T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        for (name, src) in examples {
            let wire: Value = serde_json::from_str(src).unwrap();
            assert!(wire.get("payload").is_none(), "{name}: flat frames");
            assert!(wire.get("id").is_none(), "{name}: no id");
            let f: T =
                serde_json::from_value(wire.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(type_of(&f), *name);
            assert_eq!(serde_json::to_value(&f).unwrap(), wire, "{name}: wire form");
        }
    }

    #[test]
    fn client_examples_roundtrip_exactly() {
        check::<ClientFrame>(CLIENT_TO_HUB, client_type);
        check::<ServerFrame>(HUB_TO_CLIENT, server_type);
    }

    #[test]
    fn event_frames_carry_stored_events() {
        for src in [
            include_str!("../examples/wire/client.event.json"),
            include_str!("../examples/wire/client.event.delta.json"),
        ] {
            let ServerFrame::Event { event } = serde_json::from_str(src).unwrap() else {
                panic!("not an event frame");
            };
            let ev: crate::event::Event = serde_json::from_value(event).unwrap();
            let is_delta = matches!(ev.kind, crate::event::EventKind::AgentDelta { .. });
            assert_eq!(ev.seq.is_none(), is_delta, "only agent.delta has no seq");
        }
    }

    #[test]
    fn bare_frames_parse() {
        let f: ClientFrame = serde_json::from_value(json!({"type": "subscribe_summary"})).unwrap();
        assert_eq!(f, ClientFrame::SubscribeSummary {});
        let f: ClientFrame =
            serde_json::from_value(json!({"type": "subscribe", "session_id": Uuid::nil()}))
                .unwrap();
        assert_eq!(
            f,
            ClientFrame::Subscribe {
                session_id: Uuid::nil(),
                after_seq: None
            }
        );
        // The old enveloped form is not accepted.
        assert!(serde_json::from_value::<ClientFrame>(
            json!({"type": "subscribe", "payload": {"session_id": Uuid::nil()}})
        )
        .is_err());
    }
}
