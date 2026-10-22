//! `/worker/ws` (SPEC §5): one WebSocket per worker, authenticated by its bearer token.
//!
//! Frames are strict (SPEC §5.2): the first frame must be `hello`; a frame that does not
//! parse gets `error {code: invalid_frame}`. Events inside `event` frames are tolerant and
//! stored as received.

use std::{
    collections::{HashMap, HashSet},
    ops::ControlFlow,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    http::HeaderMap,
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use hqc_proto::{
    event::CancelReason,
    frame::{error_code, Ack, Envelope, Hello, HubFrame, ProcessState, WireError, WorkerFrame},
    PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::{bearer, hash_secret},
    db, delivery,
    error::ApiError,
    events, requests,
};

/// What a hub -> worker request frame was, so its `ack` / `error` can be routed.
#[derive(Debug, Clone)]
pub enum Reply {
    Start {
        session_id: Uuid,
    },
    Input {
        session_id: Uuid,
        event_id: Uuid,
    },
    /// An answer or hub cancellation stored in `pending_requests.result`.
    Result {
        pending_id: Uuid,
    },
    Other,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum StartState {
    #[default]
    NotStarted,
    /// `session.start` sent, no reply yet; input waits.
    Starting,
    /// The worker acknowledged `session.start` on this connection.
    Started,
}

/// Per-connection view of a session: inputs are only sent after `session.start` is
/// acknowledged, and each pending input at most once per connection.
#[derive(Debug, Default)]
pub struct SessConn {
    pub start: StartState,
    pub inflight: HashSet<Uuid>,
}

#[derive(Debug, Default)]
pub struct ConnState {
    pub replies: HashMap<String, Reply>,
    pub sessions: HashMap<Uuid, SessConn>,
}

pub struct WorkerConn {
    pub worker_id: Uuid,
    pub owner_id: Uuid,
    pub conn_id: Uuid,
    tx: mpsc::UnboundedSender<Message>,
    next_id: AtomicU64,
    pub st: tokio::sync::Mutex<ConnState>,
}

impl WorkerConn {
    fn next_frame_id(&self) -> String {
        format!("h-{}", self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    fn send_text(&self, s: String) {
        // A closed channel means the connection is going away; the frame is re-sent from the
        // database state after the next `hello`.
        let _ = self.tx.send(Message::Text(s.into()));
    }

    /// Send a hub -> worker request frame, returning its `id`.
    pub fn send(&self, body: HubFrame) -> String {
        let id = self.next_frame_id();
        self.reply(&id, body);
        id
    }

    /// Send `{id, type, payload}` built from stored JSON (`pending_requests.result`).
    pub fn send_raw(&self, frame_type: &str, payload: &Value) -> String {
        let id = self.next_frame_id();
        self.send_text(json!({"id": id, "type": frame_type, "payload": payload}).to_string());
        id
    }

    fn reply(&self, id: &str, body: HubFrame) {
        let env = Envelope::new(id, body);
        match serde_json::to_string(&env) {
            Ok(s) => self.send_text(s),
            Err(e) => tracing::error!(error = %e, "serialize hub frame"),
        }
    }

    fn error(&self, id: &str, code: &str, message: impl Into<String>, retryable: bool) {
        self.reply(
            id,
            HubFrame::Error(WireError {
                code: code.into(),
                message: message.into(),
                retryable,
                details: None,
            }),
        );
    }

    pub fn close(&self) {
        let _ = self.tx.send(Message::Close(None));
    }
}

pub async fn handler(
    State(app): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let token =
        bearer(&headers).ok_or_else(|| ApiError::Unauthorized("missing worker token".into()))?;
    let w = db::worker_by_token_hash(&app.db, &hash_secret(token))
        .await?
        .ok_or_else(|| ApiError::Unauthorized("unknown worker token".into()))?;
    Ok(ws.on_upgrade(move |socket| run(app, w.id, w.owner_id, socket)))
}

async fn run(app: AppState, worker_id: Uuid, owner_id: Uuid, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            let close = matches!(m, Message::Close(_));
            if sink.send(m).await.is_err() || close {
                break;
            }
        }
    });
    let conn = Arc::new(WorkerConn {
        worker_id,
        owner_id,
        conn_id: Uuid::now_v7(),
        tx,
        next_id: AtomicU64::new(1),
        st: Default::default(),
    });
    tracing::info!(%worker_id, "worker connected");

    let mut registered = false;
    let mut last_rx = Instant::now();
    let mut ping = tokio::time::interval(app.config.timing.heartbeat);
    ping.tick().await;
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if last_rx.elapsed() > app.config.timing.worker_idle_timeout {
                    tracing::warn!(%worker_id, "worker connection idle, dropping");
                    break;
                }
                let _ = conn.tx.send(Message::Ping(Default::default()));
            }
            m = stream.next() => {
                let Some(Ok(m)) = m else { break };
                last_rx = Instant::now();
                match m {
                    Message::Text(t) => {
                        if handle_text(&app, &conn, &mut registered, t.as_str()).await.is_break() {
                            break;
                        }
                    }
                    Message::Binary(_) => conn.error("", error_code::INVALID_FRAME, "binary frames are not supported", false),
                    Message::Close(_) => break,
                    Message::Ping(_) | Message::Pong(_) => {}
                }
            }
        }
    }

    if registered && app.workers.remove(worker_id, conn.conn_id) {
        let _ = db::worker_seen(&app.db, worker_id).await;
        if let Ok(ids) = db::sessions_of_worker(&app.db, worker_id).await {
            for id in ids {
                app.session_changed(id);
            }
        }
    }
    conn.close();
    let _ = writer.await;
    tracing::info!(%worker_id, "worker disconnected");
}

async fn handle_text(
    app: &AppState,
    conn: &Arc<WorkerConn>,
    registered: &mut bool,
    text: &str,
) -> ControlFlow<()> {
    let raw: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            conn.error(
                "",
                error_code::INVALID_FRAME,
                format!("not JSON: {e}"),
                false,
            );
            return ControlFlow::Continue(());
        }
    };
    let id = raw
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let frame: Envelope<WorkerFrame> = match serde_json::from_value(raw.clone()) {
        Ok(f) => f,
        Err(e) => {
            conn.error(&id, error_code::INVALID_FRAME, e.to_string(), false);
            return ControlFlow::Continue(());
        }
    };

    if !*registered {
        return match frame.body {
            WorkerFrame::Hello(h) => match on_hello(app, conn, &id, h).await {
                Ok(true) => {
                    *registered = true;
                    after_hello(app, conn).await;
                    ControlFlow::Continue(())
                }
                Ok(false) => ControlFlow::Break(()),
                Err(e) => {
                    tracing::error!(error = %e, "hello failed");
                    conn.error(&id, error_code::INTERNAL, "internal error", true);
                    ControlFlow::Break(())
                }
            },
            _ => {
                conn.error(&id, error_code::INVALID_FRAME, "hello expected", false);
                ControlFlow::Continue(())
            }
        };
    }

    let result = match frame.body {
        WorkerFrame::Hello(_) => {
            conn.error(&id, error_code::INVALID_FRAME, "duplicate hello", false);
            Ok(())
        }
        WorkerFrame::Event { event } => {
            let raw_event = raw["payload"]["event"].clone();
            match events::ingest_worker_event(app, conn, raw_event, &event).await {
                Ok(ack) => {
                    conn.reply(&id, HubFrame::Ack(ack));
                    Ok(())
                }
                Err(e) => {
                    conn.error(&id, error_code::INTERNAL, "event not stored", true);
                    Err(e)
                }
            }
        }
        WorkerFrame::SessionState {
            session_id,
            process_state,
            runtime_session_id,
            locked_by,
        } => {
            on_session_state(
                app,
                conn,
                &id,
                session_id,
                process_state,
                runtime_session_id,
                locked_by,
            )
            .await
        }
        WorkerFrame::Ack(a) => on_ack(app, conn, &id, a).await,
        WorkerFrame::Error(e) => on_error(app, conn, &id, e).await,
    };
    if let Err(e) = result {
        tracing::error!(worker_id = %conn.worker_id, error = %e, "worker frame failed");
    }
    ControlFlow::Continue(())
}

/// `Ok(false)`: rejected, close the connection.
async fn on_hello(
    app: &AppState,
    conn: &Arc<WorkerConn>,
    id: &str,
    h: Hello,
) -> anyhow::Result<bool> {
    // Only the exact version is accepted until there is a second one to negotiate with.
    if h.protocol_version != PROTOCOL_VERSION {
        conn.reply(
            id,
            HubFrame::Error(WireError {
                code: error_code::PROTOCOL_VERSION_UNSUPPORTED.into(),
                message: format!(
                    "worker protocol {} is not supported, hub speaks {PROTOCOL_VERSION}",
                    h.protocol_version
                ),
                retryable: false,
                details: Some(json!({"min_version": PROTOCOL_VERSION})),
            }),
        );
        return Ok(false);
    }
    db::worker_hello(&app.db, conn.worker_id, &h).await?;

    // TODO(SPEC §19, iteration 2): workspaces become hub objects created through
    // `workspace.create`; until then the worker announces its single workspace here.
    if let Some(path) = h.capabilities.get("workspace_path").and_then(Value::as_str) {
        let owner = db::worker(&app.db, conn.worker_id)
            .await?
            .map(|w| w.owner_id);
        let project = match owner {
            Some(o) => sqlx::query_scalar::<_, Option<Uuid>>(
                "SELECT personal_project_id FROM users WHERE id = $1",
            )
            .bind(o)
            .fetch_one(&app.db)
            .await?
            .map(|p| (o, p)),
            None => None,
        };
        if let Some((owner, project)) = project {
            db::ensure_workspace(&app.db, project, owner, conn.worker_id, path).await?;
        }
    }

    if let Some(old) = app.workers.insert(conn.clone()) {
        tracing::info!(worker_id = %conn.worker_id, "replacing previous worker connection");
        old.close();
    }
    conn.reply(
        id,
        HubFrame::HelloAck {
            protocol_version: PROTOCOL_VERSION,
            worker_id: conn.worker_id,
            server_time: chrono::Utc::now(),
        },
    );
    tracing::info!(worker_id = %conn.worker_id, "worker hello");
    Ok(true)
}

/// After `hello`: deliver stored answers, close requests the restarted worker no longer
/// holds, re-send `pending` input in `seq` order (SPEC §5.2, §17.3).
async fn after_hello(app: &AppState, conn: &Arc<WorkerConn>) {
    let r: anyhow::Result<()> = async {
        for p in db::undelivered_results(&app.db, conn.worker_id).await? {
            requests::deliver_result(app, &p).await;
        }
        for p in db::open_requests_of_worker(&app.db, conn.worker_id).await? {
            requests::cancel(app, &p, CancelReason::WorkerRestart).await?;
        }
        for sid in db::sessions_of_worker(&app.db, conn.worker_id).await? {
            events::turn_lost(app, sid).await?;
            app.session_changed(sid);
        }
        for sid in db::sessions_with_pending_input(&app.db, conn.worker_id).await? {
            delivery::kick(app, sid, false).await;
        }
        Ok(())
    }
    .await;
    if let Err(e) = r {
        tracing::error!(worker_id = %conn.worker_id, error = %e, "after hello");
    }
}

async fn on_session_state(
    app: &AppState,
    conn: &Arc<WorkerConn>,
    id: &str,
    session_id: Uuid,
    process_state: ProcessState,
    runtime_session_id: Option<String>,
    locked_by: hqc_proto::frame::LockedBy,
) -> anyhow::Result<()> {
    let Some(s) = db::session(&app.db, session_id).await? else {
        conn.error(id, error_code::UNKNOWN_SESSION, "unknown session", false);
        return Ok(());
    };
    if s.worker_id != Some(conn.worker_id) {
        conn.error(id, error_code::UNKNOWN_SESSION, "unknown session", false);
        return Ok(());
    }
    let name = |v: Value| v.as_str().unwrap_or_default().to_string();
    let ps = name(serde_json::to_value(process_state)?);
    let lb = name(serde_json::to_value(locked_by)?);
    db::set_process_state(&app.db, session_id, &ps, runtime_session_id.as_deref(), &lb).await?;
    conn.reply(id, HubFrame::Ack(Ack::default()));

    if process_state == ProcessState::Stopped {
        {
            let mut st = conn.st.lock().await;
            if let Some(sc) = st.sessions.get_mut(&session_id) {
                sc.start = StartState::NotStarted;
                sc.inflight.clear();
            }
        }
        for p in db::open_requests_of_session(&app.db, session_id).await? {
            requests::cancel(app, &p, CancelReason::Stopped).await?;
        }
        events::turn_lost(app, session_id).await?;
        // Pending input brings the agent back up (idle_timeout, SPEC §17.3).
        delivery::kick(app, session_id, false).await;
    }
    app.session_changed(session_id);
    Ok(())
}

async fn on_ack(app: &AppState, conn: &Arc<WorkerConn>, id: &str, a: Ack) -> anyhow::Result<()> {
    let reply = conn.st.lock().await.replies.remove(id);
    match reply {
        Some(Reply::Start { session_id }) => delivery::on_start_ok(app, conn, session_id).await,
        Some(Reply::Input {
            session_id,
            event_id,
        }) => {
            if a.event_id.is_some_and(|e| e != event_id) {
                tracing::warn!(%session_id, "input ack carries a different event_id");
            }
            delivery::on_input_ack(app, conn, session_id, event_id).await
        }
        Some(Reply::Result { pending_id }) => Ok(db::result_delivered(&app.db, pending_id).await?),
        Some(Reply::Other) | None => Ok(()),
    }
}

async fn on_error(
    app: &AppState,
    conn: &Arc<WorkerConn>,
    id: &str,
    e: WireError,
) -> anyhow::Result<()> {
    let reply = conn.st.lock().await.replies.remove(id);
    match reply {
        Some(Reply::Start { session_id }) => {
            delivery::on_start_failed(app, conn, session_id, &e).await
        }
        Some(Reply::Input {
            session_id,
            event_id,
        }) => delivery::on_input_failed(app, conn, session_id, event_id, &e).await,
        Some(Reply::Result { pending_id }) => {
            // The worker no longer holds the request (restarted); stop re-sending.
            tracing::info!(%pending_id, code = %e.code, "worker rejected a request result");
            Ok(db::result_delivered(&app.db, pending_id).await?)
        }
        Some(Reply::Other) | None => {
            tracing::warn!(worker_id = %conn.worker_id, frame = id, code = %e.code, message = %e.message, "worker error");
            Ok(())
        }
    }
}
