//! Web Push (SPEC §16): VAPID-signed, aes128gcm-encrypted notifications on
//! `approval.request`, `clarify.request`, the final `agent.message` and undelivered input,
//! unless the user has a live WebSocket subscription to that session. The body is
//! [`PushMessage`]; `client/public/sw.js` shows it.

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use hqc_proto::{api::PushMessage, event::EventKind};
use serde_json::Value;
use uuid::Uuid;
use web_push::{
    ContentEncoding, PartialVapidSignatureBuilder, SubscriptionInfo, VapidSignatureBuilder,
    WebPushMessageBuilder,
};

use crate::{
    app::AppState,
    config::VapidConfig,
    db::{self, EventRow, PushSubRow, SessionRow},
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug)]
pub enum PushError {
    /// The subscription is gone (404 / 410): delete it.
    Gone,
    Other(String),
}

pub trait PushSender: Send + Sync {
    /// The VAPID public key clients subscribe with, if push is configured.
    fn public_key(&self) -> Option<String>;
    fn send<'a>(
        &'a self,
        sub: &'a PushSubRow,
        payload: &'a [u8],
    ) -> BoxFuture<'a, Result<(), PushError>>;
}

/// Push disabled (no VAPID keys).
pub struct NoPush;

impl PushSender for NoPush {
    fn public_key(&self) -> Option<String> {
        None
    }
    fn send<'a>(&'a self, _: &'a PushSubRow, _: &'a [u8]) -> BoxFuture<'a, Result<(), PushError>> {
        Box::pin(async { Ok(()) })
    }
}

pub struct WebPush {
    vapid: PartialVapidSignatureBuilder,
    public_key: String,
    subject: String,
    http: reqwest::Client,
}

impl WebPush {
    pub fn new(c: &VapidConfig) -> anyhow::Result<Self> {
        let vapid = VapidSignatureBuilder::from_base64_no_sub(c.private_key.trim())
            .map_err(|e| anyhow::anyhow!("HQC_VAPID_PRIVATE: {e}"))?;
        Ok(Self {
            vapid,
            public_key: c.public_key.trim().to_string(),
            subject: c.subject.clone(),
            // Endpoints are checked against the push services when subscribing
            // (`api::push_endpoint_allowed`); a redirect must not lead elsewhere (SPEC §20).
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }
}

impl PushSender for WebPush {
    fn public_key(&self) -> Option<String> {
        Some(self.public_key.clone())
    }

    fn send<'a>(
        &'a self,
        sub: &'a PushSubRow,
        payload: &'a [u8],
    ) -> BoxFuture<'a, Result<(), PushError>> {
        Box::pin(async move {
            let other = |e: &dyn std::fmt::Display| PushError::Other(e.to_string());
            let info = SubscriptionInfo::new(
                sub.endpoint.as_str(),
                sub.p256dh.as_str(),
                sub.auth.as_str(),
            );
            let mut sig = self.vapid.clone().add_sub_info(&info);
            sig.add_claim("sub", self.subject.clone());
            let sig = sig.build().map_err(|e| other(&e))?;
            let mut b = WebPushMessageBuilder::new(&info);
            b.set_payload(ContentEncoding::Aes128Gcm, payload);
            b.set_vapid_signature(sig);
            b.set_ttl(3600);
            let msg = b.build().map_err(|e| other(&e))?;
            let req = web_push::request_builder::build_request::<Vec<u8>>(msg);
            let mut rb = self.http.post(req.uri().to_string());
            for (k, v) in req.headers() {
                rb = rb.header(k.as_str(), v.as_bytes());
            }
            let resp = rb
                .body(req.into_body())
                .send()
                .await
                .map_err(|e| other(&e))?;
            match resp.status().as_u16() {
                200..=299 => Ok(()),
                404 | 410 => Err(PushError::Gone),
                s => Err(PushError::Other(format!("push service answered {s}"))),
            }
        })
    }
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n).collect();
        t.push('…');
        t
    }
}

/// Decide whether a stored event is worth a push and to whom; sending is spawned.
pub fn on_event(app: &AppState, s: &SessionRow, row: &EventRow, kind: &EventKind) {
    let body = match kind {
        EventKind::ApprovalRequest {
            tool, description, ..
        } => {
            format!("Approval needed: {tool} {}", clip(description, 120))
        }
        EventKind::ClarifyRequest { questions, .. } => questions
            .first()
            .map(|q| format!("Question: {}", clip(&q.text, 140)))
            .unwrap_or_else(|| "The agent has a question".into()),
        EventKind::AgentMessage {
            text,
            r#final: true,
            ..
        } => clip(text, 160),
        EventKind::System { code, text, .. }
            if code == "agent_start_failed" || code == "input_rejected" =>
        {
            clip(text, 160)
        }
        _ => return,
    };
    let request_id = match kind {
        EventKind::ApprovalRequest { request_id, .. }
        | EventKind::ClarifyRequest { request_id, .. } => Some(request_id),
        _ => None,
    };
    let approval_like = request_id.is_some();
    let payload = message(s, row, request_id.map(String::as_str), body);
    let app = app.clone();
    let session_owner = s.owner_id;
    let session_id = s.id;
    let worker_id = s.worker_id;
    tokio::spawn(async move {
        let mut users = vec![session_owner];
        // Approvals go to the worker owner (SPEC §10.1, §16).
        if approval_like {
            if let Some(w) = worker_id {
                if let Ok(Some(w)) = db::worker(&app.db, w).await {
                    if !users.contains(&w.owner_id) {
                        users.push(w.owner_id);
                    }
                }
            }
        }
        for u in users {
            if app.is_watching(u, session_id) {
                continue;
            }
            send_to_user(&app, u, &payload).await;
        }
    });
}

/// Web Push carries at most 4096 bytes after encryption; keep the plaintext under this.
const MAX_PUSH_BYTES: usize = 3800;

/// The push body (SPEC §16): `{session_id, type, title, body, tag, payload}` with the
/// stored event as `payload`, left out if the message would not fit a push.
pub fn message(s: &SessionRow, row: &EventRow, request_id: Option<&str>, body: String) -> Value {
    let mut m = PushMessage {
        session_id: s.id,
        kind: row.kind.clone(),
        title: Some(s.title.clone()),
        body: Some(body),
        // One notification per request; messages of a session replace each other.
        tag: Some(match request_id {
            Some(r) => format!("{}:{r}", s.id),
            None => format!("{}:{}", s.id, row.kind),
        }),
        payload: Some(row.to_json()),
    };
    let size = |m: &PushMessage| serde_json::to_vec(m).map(|v| v.len()).unwrap_or(usize::MAX);
    if size(&m) > MAX_PUSH_BYTES {
        m.payload = None;
    }
    serde_json::to_value(m).unwrap_or_default()
}

async fn send_to_user(app: &AppState, user: Uuid, payload: &Value) {
    if app.push.public_key().is_none() {
        return;
    }
    let subs = match db::push_subs_of(&app.db, user).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "push subscriptions query");
            return;
        }
    };
    let bytes = payload.to_string().into_bytes();
    for sub in subs {
        match app.push.send(&sub, &bytes).await {
            Ok(()) => tracing::debug!(%user, "push sent"),
            Err(PushError::Gone) => {
                tracing::info!(%user, "push subscription gone, deleting");
                let _ = db::delete_push_sub(&app.db, None, &sub.endpoint).await;
            }
            Err(PushError::Other(e)) => tracing::warn!(%user, error = %e, "push failed"),
        }
    }
}

pub fn from_config(c: Option<&VapidConfig>) -> anyhow::Result<Arc<dyn PushSender>> {
    Ok(match c {
        Some(c) => Arc::new(WebPush::new(c)?),
        None => {
            tracing::warn!("HQC_VAPID_* not set: Web Push disabled");
            Arc::new(NoPush)
        }
    })
}
