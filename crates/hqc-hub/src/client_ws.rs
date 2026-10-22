//! `/api/ws` (SPEC §6): live events for clients.
//!
//! Client -> hub (JSON text frames):
//! - `{"type": "subscribe", "session_id", "after_seq"?}` — stored events with
//!   `seq > after_seq` (default 0), then `{"type": "subscribed", "session_id", "last_seq"}`,
//!   then the live stream: stored events, `agent.delta` (no `seq`), delivery changes;
//! - `{"type": "unsubscribe", "session_id"}`;
//! - `{"type": "subscribe_summary"}` — `{"type": "summary", "sessions": [...]}` with `state`,
//!   `queue_len` and unread of every session of the user, then `{"type": "session.summary",
//!   "session": {...}}` whenever one of them changes;
//! - `{"type": "ping"}` -> `{"type": "pong"}`.
//!
//! Hub -> client: `{"type": "event", "event": {...}}` (the event exactly as stored, see
//! SPEC §3 Event; `client_op_id` echoes the input that created it),
//! `{"type": "delivery", "session_id", "event_id", "seq", "delivery", "delivered_at"}`,
//! `{"type": "error", "code", "message", "session_id"?}`.

use std::collections::HashMap;

use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::Response,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use crate::{
    app::{AppState, Notice},
    auth::CurrentUser,
    db, delivery, state,
};

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ClientFrame {
    #[serde(rename = "subscribe")]
    Subscribe {
        session_id: Uuid,
        #[serde(default)]
        after_seq: Option<i64>,
    },
    #[serde(rename = "unsubscribe")]
    Unsubscribe { session_id: Uuid },
    #[serde(rename = "subscribe_summary")]
    SubscribeSummary {},
    #[serde(rename = "ping")]
    Ping {},
}

const BACKFILL_PAGE: i64 = 500;

pub async fn handler(
    State(app): State<AppState>,
    user: CurrentUser,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| run(app, user, socket))
}

struct Client {
    app: AppState,
    user: CurrentUser,
    socket: WebSocket,
    /// session -> last `seq` sent.
    subs: HashMap<Uuid, i64>,
    summary: bool,
}

async fn run(app: AppState, user: CurrentUser, socket: WebSocket) {
    let mut rx = app.bus.subscribe();
    let mut ping = tokio::time::interval(app.config.timing.heartbeat);
    ping.tick().await;
    let mut c = Client {
        app: app.clone(),
        user,
        socket,
        subs: HashMap::new(),
        summary: false,
    };
    loop {
        let ok = tokio::select! {
            _ = ping.tick() => c.socket.send(Message::Ping(Default::default())).await.is_ok(),
            m = c.socket.recv() => match m {
                Some(Ok(Message::Text(t))) => c.on_frame(t.as_str()).await,
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => false,
                Some(Ok(_)) => true,
            },
            n = rx.recv() => match n {
                Ok(n) => c.on_notice(&n).await,
                Err(RecvError::Lagged(_)) => c.resync().await,
                Err(RecvError::Closed) => false,
            },
        };
        if !ok {
            break;
        }
    }
    for sid in c.subs.keys() {
        app.watch(c.user.id, *sid, false);
    }
}

impl Client {
    async fn send(&mut self, v: Value) -> bool {
        self.socket
            .send(Message::Text(v.to_string().into()))
            .await
            .is_ok()
    }

    async fn error(&mut self, code: &str, message: &str, session_id: Option<Uuid>) -> bool {
        self.send(
            json!({"type": "error", "code": code, "message": message, "session_id": session_id}),
        )
        .await
    }

    async fn on_frame(&mut self, text: &str) -> bool {
        let f: ClientFrame = match serde_json::from_str(text) {
            Ok(f) => f,
            Err(e) => return self.error("invalid_frame", &e.to_string(), None).await,
        };
        match f {
            ClientFrame::Ping {} => self.send(json!({"type": "pong"})).await,
            ClientFrame::Subscribe {
                session_id,
                after_seq,
            } => {
                match db::session(&self.app.db, session_id).await {
                    Ok(Some(s)) if s.owner_id == self.user.id => {}
                    Ok(_) => {
                        return self
                            .error("not_found", "unknown session", Some(session_id))
                            .await
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "subscribe");
                        return self
                            .error("internal", "internal error", Some(session_id))
                            .await;
                    }
                }
                if self
                    .subs
                    .insert(session_id, after_seq.unwrap_or(0).max(0))
                    .is_none()
                {
                    self.app.watch(self.user.id, session_id, true);
                }
                if !self.backfill(session_id).await {
                    return false;
                }
                let last = self.subs[&session_id];
                self.send(json!({"type": "subscribed", "session_id": session_id, "last_seq": last}))
                    .await
            }
            ClientFrame::Unsubscribe { session_id } => {
                if self.subs.remove(&session_id).is_some() {
                    self.app.watch(self.user.id, session_id, false);
                }
                true
            }
            ClientFrame::SubscribeSummary {} => {
                self.summary = true;
                self.send_summary().await
            }
        }
    }

    /// Send stored events after the last sent `seq` of a subscription.
    async fn backfill(&mut self, session_id: Uuid) -> bool {
        loop {
            let Some(&after) = self.subs.get(&session_id) else {
                return true;
            };
            let rows = match db::events_after(&self.app.db, session_id, after, BACKFILL_PAGE).await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(error = %e, "backfill");
                    return self
                        .error("internal", "internal error", Some(session_id))
                        .await;
                }
            };
            let n = rows.len() as i64;
            for r in rows {
                if !self
                    .send(json!({"type": "event", "event": r.to_json()}))
                    .await
                {
                    return false;
                }
                self.subs.insert(session_id, r.seq);
            }
            if n < BACKFILL_PAGE {
                return true;
            }
        }
    }

    async fn on_notice(&mut self, n: &Notice) -> bool {
        match n {
            Notice::Event {
                session_id,
                seq,
                event,
            } => {
                let Some(&last) = self.subs.get(session_id) else {
                    return true;
                };
                match seq {
                    None => self.send(json!({"type": "event", "event": event})).await,
                    Some(seq) if *seq <= last => true,
                    Some(seq) if *seq == last + 1 => {
                        self.subs.insert(*session_id, *seq);
                        self.send(json!({"type": "event", "event": event})).await
                    }
                    // A gap: notices of concurrent writers can arrive out of order.
                    Some(_) => self.backfill(*session_id).await,
                }
            }
            Notice::Delivery { session_id, .. } => {
                if !self.subs.contains_key(session_id) {
                    return true;
                }
                match delivery::delivery_frame(n) {
                    Some(f) => self.send(f).await,
                    None => true,
                }
            }
            Notice::SessionChanged { session_id } => {
                if !self.summary {
                    return true;
                }
                let row = match db::session(&self.app.db, *session_id).await {
                    Ok(Some(s)) if s.owner_id == self.user.id => s,
                    _ => return true,
                };
                match state::view(&self.app, self.user.id, row).await {
                    Ok(Some(v)) => {
                        self.send(json!({"type": "session.summary", "session": v.summary()}))
                            .await
                    }
                    _ => true,
                }
            }
        }
    }

    async fn send_summary(&mut self) -> bool {
        let views = match db::sessions_of_owner(&self.app.db, self.user.id).await {
            Ok(rows) => state::views(&self.app, self.user.id, rows).await,
            Err(e) => Err(e),
        };
        match views {
            Ok(v) => {
                let sessions: Vec<Value> = v.iter().map(|v| v.summary()).collect();
                self.send(json!({"type": "summary", "sessions": sessions}))
                    .await
            }
            Err(e) => {
                tracing::error!(error = %e, "summary");
                self.error("internal", "internal error", None).await
            }
        }
    }

    /// The bus overflowed: catch up every subscription from the database.
    async fn resync(&mut self) -> bool {
        let ids: Vec<Uuid> = self.subs.keys().copied().collect();
        for id in ids {
            if !self.backfill(id).await {
                return false;
            }
        }
        !self.summary || self.send_summary().await
    }
}
