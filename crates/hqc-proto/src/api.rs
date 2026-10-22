//! HTTP bodies shared by the hub, the worker and the client (SPEC §5.1, §6).
//!
//! Requests ignore unknown fields, so an older hub accepts a newer worker's body.
//! Wire examples: `examples/wire/api.*.json`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    fn enroll_request_ignores_unknown_fields() {
        let r: EnrollRequest =
            serde_json::from_str(r#"{"name":"w","os":"linux","arch":"arm64"}"#).unwrap();
        assert_eq!(r.protocol_version, None);
    }
}
