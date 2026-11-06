//! `/api/ws` (SPEC §6): live events for clients. Frames are flat JSON objects, typed in
//! [`hqc_proto::client_frame`].
//!
//! - `subscribe {session_id, after_seq?}` — stored events with `seq > after_seq` (default 0),
//!   the answer being written as one `agent.delta` ([`AppState::live_text`]), then
//!   `subscribed {session_id, last_seq}`, then the live stream: stored events,
//!   `agent.delta` (no `seq`), `delivery` changes of inputs;
//! - `unsubscribe {session_id}`;
//! - `subscribe_summary` — `summary {sessions}` for every session of the user, then
//!   `session.summary {session}` whenever one of them changes; with it `requests {requests}`,
//!   the user's open requests, sent whole again whenever one opens or closes or an ask
//!   waiting on one does (T37);
//! - `ping` -> `pong`.
//!
//! An `event` frame carries the event exactly as stored (SPEC §3 Event); `client_op_id`
//! echoes the input that created it. Errors: `error {code, message, session_id?}`.

use std::collections::HashMap;

use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::Response,
};
use hqc_proto::client_frame::{ClientFrame, ServerFrame};
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use crate::{
    app::{AppState, Notice},
    auth::CurrentUser,
    db, delivery, requests,
    security::MAX_WS_MESSAGE,
    state,
};

const BACKFILL_PAGE: i64 = 500;

pub async fn handler(
    State(app): State<AppState>,
    user: CurrentUser,
    ws: WebSocketUpgrade,
) -> Response {
    // Origin was checked by `security::guard` (SPEC §20).
    ws.max_message_size(MAX_WS_MESSAGE)
        .max_frame_size(MAX_WS_MESSAGE)
        .on_upgrade(move |socket| run(app, user, socket))
}

struct Client {
    app: AppState,
    user: CurrentUser,
    socket: WebSocket,
    /// session -> last `seq` sent.
    subs: HashMap<Uuid, i64>,
    /// session -> the number of the first `agent.delta` not in the text sent on subscribe
    /// ([`AppState::live_text`]).
    live_from: HashMap<Uuid, u64>,
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
        live_from: HashMap::new(),
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
    async fn send(&mut self, f: ServerFrame) -> bool {
        let text = match serde_json::to_string(&f) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(error = %e, "serialize client frame");
                return true;
            }
        };
        self.socket.send(Message::Text(text.into())).await.is_ok()
    }

    async fn error(&mut self, code: &str, message: &str, session_id: Option<Uuid>) -> bool {
        self.send(ServerFrame::Error {
            code: code.into(),
            message: message.into(),
            session_id,
        })
        .await
    }

    async fn on_frame(&mut self, text: &str) -> bool {
        let f: ClientFrame = match serde_json::from_str(text) {
            Ok(f) => f,
            Err(e) => return self.error("invalid_frame", &e.to_string(), None).await,
        };
        match f {
            ClientFrame::Ping {} => self.send(ServerFrame::Pong {}).await,
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
                    .insert(
                        session_id,
                        i64::try_from(after_seq.unwrap_or(0)).unwrap_or(i64::MAX),
                    )
                    .is_none()
                {
                    self.app.watch(self.user.id, session_id, true);
                }
                if !self.backfill(session_id).await {
                    return false;
                }
                // After the backfill: the text is ended before its `agent.message` is stored.
                let (text, next) = self.app.live_text(session_id);
                self.live_from.insert(session_id, next);
                if let Some(event) = text {
                    if !self.send(ServerFrame::Event { event }).await {
                        return false;
                    }
                }
                let last_seq = self.subs[&session_id].max(0) as u64;
                self.send(ServerFrame::Subscribed {
                    session_id,
                    last_seq,
                })
                .await
            }
            ClientFrame::Unsubscribe { session_id } => {
                self.live_from.remove(&session_id);
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
            let page = match db::events_after(&self.app.db, session_id, after, BACKFILL_PAGE).await
            {
                Ok(rows) => db::events_for_clients(&self.app.db, &rows)
                    .await
                    .map(|events| (rows, events)),
                Err(e) => Err(e),
            };
            let (rows, events) = match page {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(error = %e, "backfill");
                    return self
                        .error("internal", "internal error", Some(session_id))
                        .await;
                }
            };
            let n = rows.len() as i64;
            for (r, event) in rows.iter().zip(events) {
                if !self.send(ServerFrame::Event { event }).await {
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
                match *seq {
                    seq if seq <= last => true,
                    seq if seq == last + 1 => {
                        self.subs.insert(*session_id, seq);
                        self.send(ServerFrame::Event {
                            event: event.clone(),
                        })
                        .await
                    }
                    // A gap: notices of concurrent writers can arrive out of order.
                    _ => self.backfill(*session_id).await,
                }
            }
            Notice::Delta {
                session_id,
                at,
                event,
            } => match self.live_from.get(session_id) {
                // Before it: in the text sent on subscribe.
                Some(from) if at >= from => {
                    self.send(ServerFrame::Event {
                        event: event.clone(),
                    })
                    .await
                }
                _ => true,
            },
            Notice::Resent {
                session_id,
                seq,
                event,
            } => match self.subs.get(session_id) {
                // A later backfill sends the event with the field anyway.
                Some(&last) if *seq <= last => {
                    self.send(ServerFrame::Event {
                        event: event.clone(),
                    })
                    .await
                }
                _ => true,
            },
            Notice::Delivery { session_id, .. } => {
                if !self.subs.contains_key(session_id) {
                    return true;
                }
                match delivery::delivery_frame(n) {
                    Some(f) => self.send(f).await,
                    None => true,
                }
            }
            Notice::RequestsChanged { owner_id } => {
                if !self.summary || *owner_id != self.user.id {
                    return true;
                }
                self.send_requests().await
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
                        self.send(ServerFrame::SessionSummary {
                            session: v.summary(),
                        })
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
                let sessions = v.iter().map(|v| v.summary()).collect();
                self.send(ServerFrame::Summary { sessions }).await && self.send_requests().await
            }
            Err(e) => {
                tracing::error!(error = %e, "summary");
                self.error("internal", "internal error", None).await
            }
        }
    }

    /// Every open request of the user (SPEC §6 `requests`).
    async fn send_requests(&mut self) -> bool {
        match requests::open_of(&self.app, self.user.id).await {
            Ok(requests) => self.send(ServerFrame::Requests { requests }).await,
            Err(e) => {
                tracing::error!(error = %e, "requests");
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
