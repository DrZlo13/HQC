//! Apple push to the native iOS client (SPEC §16), next to Web Push: the same notices, sent
//! over APNs with a token-based key (`HQC_APNS_*`). The app registers its device token with
//! `POST /api/push/apns`; a notice goes to every device of the user that is not watching the
//! session, as a Web Push does.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::time::Instant;

use crate::{
    config::ApnsConfig,
    push::{BoxFuture, PushError},
};

/// APNs refuses a payload over this many bytes.
pub const MAX_PAYLOAD: usize = 4096;
/// A provider token lives an hour; renew it well before (Apple refuses one renewed more often
/// than every 20 minutes).
const TOKEN_LIFETIME: Duration = Duration::from_secs(45 * 60);

pub trait ApnsSender: Send + Sync {
    /// `sandbox`: the device runs a development build (APNs' sandbox host).
    fn send<'a>(
        &'a self,
        device_token: &'a str,
        sandbox: bool,
        collapse_id: &'a str,
        payload: &'a [u8],
    ) -> BoxFuture<'a, Result<(), PushError>>;
}

pub struct ApnsClient {
    key: EncodingKey,
    key_id: String,
    team_id: String,
    topic: String,
    http: reqwest::Client,
    token: Mutex<Option<(String, Instant)>>,
}

impl ApnsClient {
    pub fn new(c: &ApnsConfig) -> anyhow::Result<Self> {
        let key = EncodingKey::from_ec_pem(c.key_pem.trim().as_bytes())
            .map_err(|e| anyhow::anyhow!("HQC_APNS_KEY is not an EC P-256 key in PEM: {e}"))?;
        let client = Self {
            key,
            key_id: c.key_id.trim().to_string(),
            team_id: c.team_id.trim().to_string(),
            topic: c.topic.trim().to_string(),
            http: reqwest::Client::builder()
                .http2_prior_knowledge()
                .timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            token: Mutex::new(None),
        };
        // Fail at start, not on the first notice, when the key cannot sign.
        client.provider_token()?;
        Ok(client)
    }

    fn provider_token(&self) -> anyhow::Result<String> {
        let mut t = self.token.lock().unwrap();
        if let Some((tok, at)) = t.as_ref() {
            if at.elapsed() < TOKEN_LIFETIME {
                return Ok(tok.clone());
            }
        }
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(self.key_id.clone());
        let claims = json!({ "iss": self.team_id, "iat": chrono::Utc::now().timestamp() });
        let tok = jsonwebtoken::encode(&header, &claims, &self.key)?;
        *t = Some((tok.clone(), Instant::now()));
        Ok(tok)
    }
}

impl ApnsSender for ApnsClient {
    fn send<'a>(
        &'a self,
        device_token: &'a str,
        sandbox: bool,
        collapse_id: &'a str,
        payload: &'a [u8],
    ) -> BoxFuture<'a, Result<(), PushError>> {
        Box::pin(async move {
            let other = |e: &dyn std::fmt::Display| PushError::Other(e.to_string());
            let jwt = self.provider_token().map_err(|e| other(&e))?;
            let host = if sandbox {
                "api.sandbox.push.apple.com"
            } else {
                "api.push.apple.com"
            };
            let resp = self
                .http
                .post(format!("https://{host}/3/device/{device_token}"))
                .header("authorization", format!("bearer {jwt}"))
                .header("apns-topic", &self.topic)
                .header("apns-push-type", "alert")
                .header("apns-priority", "10")
                .header(
                    "apns-expiration",
                    (chrono::Utc::now().timestamp() + 3600).to_string(),
                )
                .header("apns-collapse-id", collapse_id)
                .body(payload.to_vec())
                .send()
                .await
                .map_err(|e| other(&e))?;
            let status = resp.status().as_u16();
            if (200..300).contains(&status) {
                return Ok(());
            }
            let reason = resp
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v["reason"].as_str().map(str::to_string))
                .unwrap_or_default();
            if gone(status, &reason) {
                Err(PushError::Gone)
            } else {
                Err(PushError::Other(format!("APNs answered {status} {reason}")))
            }
        })
    }
}

/// The device token is no longer valid for this app: delete it.
fn gone(status: u16, reason: &str) -> bool {
    status == 410
        || matches!(
            reason,
            "BadDeviceToken" | "Unregistered" | "DeviceTokenNotForTopic"
        )
}

/// A device token as APNs hands it to the app: hex, 32 bytes today, longer allowed.
pub fn valid_device_token(t: &str) -> bool {
    (64..=200).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `apns-collapse-id` takes at most 64 bytes; a notice's tag may be longer.
pub fn collapse_id(tag: &str) -> String {
    if tag.len() <= 64 {
        tag.to_string()
    } else {
        Sha256::digest(tag.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// The APNs payload of a notice built from the Web Push message ([`crate::push::message`]):
/// the alert, the session's thread, a category the app hangs its actions on, and what the app
/// needs to open the session or answer a request from the notification: `session_id`,
/// `open_session_id`, `type`, `request_id` and, for an approval, its options.
pub fn payload(web: &Value) -> Vec<u8> {
    let kind = web["type"].as_str().unwrap_or_default();
    let event = &web["payload"];
    let request_id = event["payload"]["request_id"].as_str();
    let category = match kind {
        "approval.request" => "APPROVAL",
        "clarify.request" => "QUESTION",
        _ => "MESSAGE",
    };
    let options: Vec<Value> = if kind == "approval.request" {
        event["payload"]["options"]
            .as_array()
            .map(|a| {
                a.iter()
                    .take(6)
                    .map(|o| {
                        json!({
                            "id": o["id"],
                            "canonical": o["canonical"],
                            "label": crate::push::clip(o["label"].as_str().unwrap_or_default(), 40),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let build = |body_len: usize| {
        let mut m = json!({
            "aps": {
                "alert": {
                    "title": crate::push::clip(web["title"].as_str().unwrap_or("hqc"), 80),
                    "body": crate::push::clip(web["body"].as_str().unwrap_or_default(), body_len),
                },
                "sound": "default",
                "thread-id": web["session_id"],
                "category": category,
            },
            "session_id": web["session_id"],
            "type": kind,
        });
        if !web["open_session_id"].is_null() {
            m["open_session_id"] = web["open_session_id"].clone();
        }
        if let Some(r) = request_id {
            m["request_id"] = json!(r);
        }
        if !options.is_empty() {
            m["options"] = json!(options);
        }
        serde_json::to_vec(&m).unwrap_or_default()
    };
    let mut body_len = 400;
    loop {
        let bytes = build(body_len);
        if bytes.len() <= MAX_PAYLOAD || body_len <= 20 {
            return bytes;
        }
        body_len /= 2;
    }
}

pub fn from_config(c: Option<&ApnsConfig>) -> anyhow::Result<Option<Arc<dyn ApnsSender>>> {
    Ok(match c {
        Some(c) => Some(Arc::new(ApnsClient::new(c)?)),
        None => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_collapse_ids() {
        assert!(valid_device_token(&"ab".repeat(32)));
        assert!(!valid_device_token("abc"));
        assert!(!valid_device_token(&"zz".repeat(32)));
        assert_eq!(collapse_id("short"), "short");
        let long = "x".repeat(100);
        assert_eq!(collapse_id(&long).len(), 64);
        assert!(gone(410, ""));
        assert!(gone(400, "BadDeviceToken"));
        assert!(!gone(500, "InternalServerError"));
    }

    #[test]
    fn payload_carries_the_request_and_fits() {
        let web = json!({
            "session_id": "0199f3a2-7c10-7000-8000-0000000000aa",
            "type": "approval.request",
            "title": "Session",
            "body": "x".repeat(10_000),
            "tag": "t",
            "payload": {"payload": {"request_id": "r1", "options": [
                {"id": "o1", "canonical": "once", "label": "Allow once"},
                {"id": "o2", "canonical": "deny", "label": "Deny"}
            ]}}
        });
        let p: Value = serde_json::from_slice(&payload(&web)).unwrap();
        assert!(payload(&web).len() <= MAX_PAYLOAD);
        assert_eq!(p["aps"]["category"], "APPROVAL");
        assert_eq!(p["request_id"], "r1");
        assert_eq!(p["options"][1]["canonical"], "deny");
        assert_eq!(
            p["aps"]["thread-id"],
            "0199f3a2-7c10-7000-8000-0000000000aa"
        );
    }
}
