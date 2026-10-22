//! `/worker/ws` (SPEC §5): one WebSocket per worker, authenticated by its bearer token.
//!
//! Frames are strict (SPEC §5.2): the first frame must be `hello`; a frame that does not
//! parse gets `error {code: invalid_frame}`. Events inside `event` frames are tolerant and
//! stored as received, except U+0000, which becomes U+FFFD in every incoming frame.

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
    frame::{
        error_code, Ack, Envelope, Hello, HubFrame, ProcessState, ResultBody, WireError,
        WorkerFrame,
    },
    PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Notify};
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::{bearer, hash_secret},
    db, delivery,
    error::ApiError,
    events, requests,
    security::{ClientIp, MAX_WS_MESSAGE},
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

/// Frames queued for one worker's socket. A worker that does not read them in time is
/// disconnected ([`WorkerConn::overflow`]).
const SEND_QUEUE: usize = 4096;

pub struct WorkerConn {
    pub worker_id: Uuid,
    pub owner_id: Uuid,
    pub conn_id: Uuid,
    tx: mpsc::Sender<Message>,
    /// The send queue overflowed: the connection loop drops the worker.
    overflow: Notify,
    next_id: AtomicU64,
    pub st: tokio::sync::Mutex<ConnState>,
    /// Requests waiting for their `result` / `error` ([`WorkerConn::request`]), by frame id.
    /// Dropped with the connection, so a waiting request learns that the worker is gone.
    requests: std::sync::Mutex<HashMap<String, oneshot::Sender<Result<ResultBody, WireError>>>>,
}

/// Why [`WorkerConn::request`] has no result.
#[derive(Debug)]
pub enum RequestError {
    /// The connection closed before the answer.
    Offline,
    Timeout,
    /// The worker answered `error`.
    Worker(WireError),
}

impl WorkerConn {
    /// Send a request frame whose `request_id` is its frame id (`fs.stat`, `fs.list_dirs`,
    /// SPEC §5.3) and wait for the worker's `result`.
    pub async fn request(
        &self,
        build: impl FnOnce(String) -> HubFrame,
        timeout: std::time::Duration,
    ) -> Result<ResultBody, RequestError> {
        let id = self.next_frame_id();
        let (tx, rx) = oneshot::channel();
        self.requests.lock().unwrap().insert(id.clone(), tx);
        self.reply(&id, build(id.clone()));
        let r = tokio::time::timeout(timeout, rx).await;
        self.requests.lock().unwrap().remove(&id);
        match r {
            Ok(Ok(Ok(body))) => Ok(body),
            Ok(Ok(Err(e))) => Err(RequestError::Worker(e)),
            Ok(Err(_)) => Err(RequestError::Offline),
            Err(_) => Err(RequestError::Timeout),
        }
    }

    /// Hand a `result` / `error` to the waiting request; `false` if none waits for `id`.
    fn answer(&self, id: &str, r: Result<ResultBody, WireError>) -> bool {
        match self.requests.lock().unwrap().remove(id) {
            Some(tx) => {
                let _ = tx.send(r);
                true
            }
            None => false,
        }
    }

    fn next_frame_id(&self) -> String {
        format!("h-{}", self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// A closed channel means the connection is going away; a full one drops the
    /// connection. Either way the frame is re-sent from the database state after the next
    /// `hello`.
    fn push(&self, m: Message) {
        if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(m) {
            self.overflow.notify_one();
        }
    }

    fn send_text(&self, s: String) {
        self.push(Message::Text(s.into()));
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
        self.push(Message::Close(None));
    }
}

const AUTH_FAILURES: &str = "worker_ws_auth_failures";

/// Before the upgrade: the bearer token (SPEC §5.1). Unauthenticated attempts are limited
/// (SPEC §20): an address with too many bad tokens in the last minute gets 429 without a
/// database lookup, a malformed token never reaches the database, and at most
/// `ws_auth_concurrency` lookups run at once (503 above that).
pub async fn handler(
    State(app): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let max_failures = app.config.limits.ws_auth_failures_per_min;
    if app.limiter.exhausted(AUTH_FAILURES, &ip, max_failures) {
        return Err(ApiError::TooManyRequests);
    }
    let refuse = |why: &str| {
        app.limiter.allow(AUTH_FAILURES, &ip, max_failures);
        ApiError::Unauthorized(why.into())
    };
    let token = bearer(&headers).ok_or_else(|| refuse("missing worker token"))?;
    if !is_worker_token(token) {
        return Err(refuse("unknown worker token"));
    }
    let w = {
        let _permit = app.ws_auth.try_acquire().map_err(|_| ApiError::Busy)?;
        db::worker_by_token_hash(&app.db, &hash_secret(token)).await?
    };
    let w = w.ok_or_else(|| refuse("unknown worker token"))?;
    Ok(ws
        .max_message_size(MAX_WS_MESSAGE)
        .max_frame_size(MAX_WS_MESSAGE)
        .on_upgrade(move |socket| run(app, w.id, w.owner_id, socket)))
}

/// The shape [`crate::auth::new_secret`] gives worker tokens: `hqcw_` + 43 base64url chars.
fn is_worker_token(t: &str) -> bool {
    t.strip_prefix("hqcw_").is_some_and(|b| {
        b.len() == 43
            && b.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    })
}

async fn run(app: AppState, worker_id: Uuid, owner_id: Uuid, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<Message>(SEND_QUEUE);
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
        overflow: Notify::new(),
        next_id: AtomicU64::new(1),
        st: Default::default(),
        requests: Default::default(),
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
                conn.push(Message::Ping(Default::default()));
            }
            _ = conn.overflow.notified() => {
                tracing::warn!(%worker_id, "worker does not read its frames, dropping");
                break;
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

    // Waiting requests learn the worker is gone instead of running into their timeout.
    conn.requests.lock().unwrap().clear();
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
    let mut raw: Value = match serde_json::from_str(text) {
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
    // Before anything is parsed or stored: Postgres takes no U+0000 (SPEC §5.2).
    hqc_proto::replace_nul_json(&mut raw);
    let id = raw
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    // The version is checked before the strict parse: a `hello` of another protocol version
    // may have another shape, and must still learn which version the hub needs (SPEC §5.1).
    if !*registered && raw.get("type").and_then(Value::as_str) == Some("hello") {
        if let Some(v) = raw["payload"]["protocol_version"].as_u64() {
            if v != u64::from(PROTOCOL_VERSION) {
                conn.reply(
                    &id,
                    HubFrame::Error(WireError {
                        code: error_code::PROTOCOL_VERSION_UNSUPPORTED.into(),
                        message: format!(
                            "worker protocol {v} is not supported, hub speaks {PROTOCOL_VERSION}"
                        ),
                        retryable: false,
                        details: Some(json!({"min_version": PROTOCOL_VERSION})),
                    }),
                );
                return ControlFlow::Break(());
            }
        }
    }
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
                Ok(Some(restarted)) => {
                    *registered = true;
                    after_hello(app, conn, restarted).await;
                    ControlFlow::Continue(())
                }
                Ok(None) => ControlFlow::Break(()),
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
                // Data the database refuses would be refused on every resend: the worker
                // drops it. A database that is down is worth a retry (SPEC §5.2).
                Err(e) if db::is_data_error(&e) => {
                    tracing::error!(worker_id = %conn.worker_id, event_id = %event.event_id, error = %e, "event refused by the database, rejected");
                    conn.error(
                        &id,
                        error_code::INVALID_FRAME,
                        "event refused by the database",
                        false,
                    );
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
            title,
        } => {
            let st = StateFrame {
                session_id,
                process_state,
                runtime_session_id,
                locked_by,
                title,
            };
            on_session_state(app, conn, &id, st).await
        }
        WorkerFrame::Result(r) => {
            if !conn.answer(&id, Ok(r.body.clone())) && !conn.answer(&r.request_id, Ok(r.body)) {
                tracing::debug!(worker_id = %conn.worker_id, request_id = %r.request_id, "result for no waiting request");
            }
            Ok(())
        }
        WorkerFrame::Ack(a) => on_ack(app, conn, &id, a).await,
        WorkerFrame::Error(e) => on_error(app, conn, &id, e).await,
    };
    if let Err(e) = result {
        tracing::error!(worker_id = %conn.worker_id, error = %e, "worker frame failed");
    }
    ControlFlow::Continue(())
}

/// `Ok(None)`: rejected, close the connection. `Ok(Some(restarted))`: accepted;
/// `restarted` when `boot_id` differs from the previous `hello` (SPEC §5.2, §17.3).
async fn on_hello(
    app: &AppState,
    conn: &Arc<WorkerConn>,
    id: &str,
    h: Hello,
) -> anyhow::Result<Option<bool>> {
    // Only the exact version is accepted until there is a second one to negotiate with;
    // other versions are refused in `handle_text` before parsing.
    debug_assert_eq!(h.protocol_version, PROTOCOL_VERSION);
    let Some(previous_boot) = db::worker_hello(&app.db, conn.worker_id, &h).await? else {
        // Revoked between the token check and `hello` (SPEC §5.1).
        tracing::info!(worker_id = %conn.worker_id, "hello from a revoked worker refused");
        conn.error(id, error_code::UNAUTHORIZED, "worker token revoked", false);
        return Ok(None);
    };
    let restarted = previous_boot != Some(h.boot_id);

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
    tracing::info!(worker_id = %conn.worker_id, boot_id = %h.boot_id, restarted, "worker hello");
    Ok(Some(restarted))
}

/// After `hello` (SPEC §5.2, §17.3).
///
/// A new `boot_id` means the worker restarted: its agents are gone, so open approval /
/// clarify requests are cancelled with `worker_restart` (event only: no process holds them),
/// unacknowledged answers are dropped and an open turn gets `system turn_lost`.
/// The same `boot_id` is a reconnect: nothing is cancelled.
///
/// Then every session with pending input, an open request or an unacknowledged answer gets
/// `session.start` (`mode: resume`); its answers and inputs follow the `ack` on it
/// ([`delivery::on_start_ok`]).
async fn after_hello(app: &AppState, conn: &Arc<WorkerConn>, restarted: bool) {
    let r: anyhow::Result<()> = async {
        if restarted {
            let dropped = db::drop_undelivered_results(&app.db, conn.worker_id).await?;
            if dropped > 0 {
                tracing::info!(worker_id = %conn.worker_id, dropped, "worker restarted: unacknowledged answers dropped");
            }
            for p in db::open_requests_of_worker(&app.db, conn.worker_id).await? {
                requests::cancel_without_frame(app, &p, CancelReason::WorkerRestart).await?;
            }
            db::worker_restarted(&app.db, conn.worker_id).await?;
            for sid in db::sessions_of_worker(&app.db, conn.worker_id).await? {
                events::turn_lost(app, sid).await?;
                app.session_changed(sid);
            }
        }
        for sid in db::sessions_to_restart(&app.db, conn.worker_id).await? {
            delivery::kick(app, sid, true).await;
        }
        Ok(())
    }
    .await;
    if let Err(e) = r {
        tracing::error!(worker_id = %conn.worker_id, error = %e, "after hello");
    }
}

/// The fields of `session.state` (SPEC §5.4).
struct StateFrame {
    session_id: Uuid,
    process_state: ProcessState,
    runtime_session_id: Option<String>,
    locked_by: hqc_proto::frame::LockedBy,
    title: Option<String>,
}

async fn on_session_state(
    app: &AppState,
    conn: &Arc<WorkerConn>,
    id: &str,
    st: StateFrame,
) -> anyhow::Result<()> {
    let StateFrame {
        session_id,
        process_state,
        runtime_session_id,
        locked_by,
        title,
    } = st;
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
    // Unless the user renamed the session (SPEC §3 Session).
    if let Some(t) = title.as_deref().and_then(runtime_title) {
        db::set_runtime_title(&app.db, session_id, &t).await?;
    }
    conn.reply(id, HubFrame::Ack(Ack::default()));

    if process_state == ProcessState::Stopped {
        {
            let mut st = conn.st.lock().await;
            if let Some(sc) = st.sessions.get_mut(&session_id) {
                // A failed start reports `stopped` too; the reply to the outstanding
                // `session.start` decides what happens next, not this frame.
                if sc.start != StartState::Starting {
                    sc.start = StartState::NotStarted;
                    sc.inflight.clear();
                }
            }
        }
        for p in db::open_requests_of_session(&app.db, session_id).await? {
            requests::cancel(app, &p, CancelReason::Stopped).await?;
        }
        events::turn_lost(app, session_id).await?;
        // Pending input brings the agent back up (idle_timeout, SPEC §17.3). After a failed
        // start the retry belongs to `on_start_failed`, after its pause (SPEC §5.2).
        if s.start_failures == 0 {
            delivery::kick(app, session_id, false).await;
        }
    }
    app.session_changed(session_id);
    Ok(())
}

/// A runtime title as stored: one line, at most `MAX_TITLE` characters (SPEC §6), U+0000
/// replaced (SPEC §5.2); `None` if nothing is left.
fn runtime_title(t: &str) -> Option<String> {
    let t: String = t
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(crate::api::MAX_TITLE)
        .collect();
    (!t.is_empty()).then(|| hqc_proto::replace_nul(&t).into_owned())
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
    let e = match conn.requests.lock().unwrap().remove(id) {
        Some(tx) => {
            let _ = tx.send(Err(e));
            return Ok(());
        }
        None => e,
    };
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
