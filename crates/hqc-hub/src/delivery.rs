//! Hub -> worker delivery of input (SPEC §5.2).
//!
//! `user.message` / `peer.message` are stored with `delivery: pending`. While the session's
//! worker is connected, the hub sends `session.start` (once per connection; `mode: new` the
//! very first time for the session, `resume` after that), and after its `ack` the answers the
//! worker has not acknowledged, then every pending input as `session.input` in `seq` order.
//! The worker's `ack {event_id}`
//! makes the input `delivered`. A retryable `error` on start is retried; after
//! `max_start_failures` in a row the pending input becomes `failed`, a `system` event is
//! written and the session shows `error` until `POST /retry`.

use std::sync::Arc;

use hqc_proto::{
    client_frame::ServerFrame,
    frame::{
        error_code, HubFrame, PeerPrefix, Runtime, SessionInput, SessionStart, StartMode, WireError,
    },
};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    app::{AppState, Notice},
    auth::{hash_secret, new_secret},
    db::{self, EventRow, SessionRow},
    events, requests,
    worker_ws::{ConnState, Reply, StartState, WorkerConn},
};

/// Move the session's delivery forward on its worker's live connection, if any.
/// `force_start` sends `session.start` even without pending input (new session).
pub async fn kick(app: &AppState, session_id: Uuid, force_start: bool) {
    if let Err(e) = kick_inner(app, session_id, force_start).await {
        tracing::error!(%session_id, error = %e, "delivery failed");
    }
}

async fn kick_inner(app: &AppState, session_id: Uuid, force_start: bool) -> anyhow::Result<()> {
    let Some(s) = db::session(&app.db, session_id).await? else {
        return Ok(());
    };
    if s.archived || s.locked_by == "vscode" {
        return Ok(());
    }
    let Some(conn) = s.worker_id.and_then(|w| app.workers.get(w)) else {
        return Ok(());
    };
    let mut guard = conn.st.lock().await;
    let ConnState { replies, sessions } = &mut *guard;
    let sc = sessions.entry(session_id).or_default();
    let pending = db::pending_inputs(&app.db, session_id).await?;
    match sc.start {
        StartState::NotStarted => {
            if force_start || !pending.is_empty() {
                let frame = build_start(app, &s).await?;
                let id = conn.send(HubFrame::SessionStart(frame));
                sc.start = StartState::Starting;
                replies.insert(id, Reply::Start { session_id });
            }
        }
        StartState::Starting => {}
        StartState::Started => {
            for p in pending {
                if !sc.inflight.insert(p.event_id) {
                    continue;
                }
                let id = conn.send(HubFrame::SessionInput(input_frame(
                    session_id, p.event_id, &p.payload,
                )));
                replies.insert(
                    id,
                    Reply::Input {
                        session_id,
                        event_id: p.event_id,
                    },
                );
            }
        }
    }
    Ok(())
}

fn input_frame(session_id: Uuid, event_id: Uuid, payload: &Value) -> SessionInput {
    SessionInput {
        session_id,
        event_id,
        text: payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        artifact_ids: payload
            .get("artifact_ids")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default(),
        // The send buffer arrives with MCP hqc (SPEC §7.1, iteration 3).
        peer_prefix: Vec::<PeerPrefix>::new(),
    }
}

async fn build_start(app: &AppState, s: &SessionRow) -> anyhow::Result<SessionStart> {
    let ws = match s.workspace_id {
        Some(id) => db::workspace(&app.db, id).await?,
        None => None,
    };
    let worker_owner = match s.worker_id {
        Some(w) => db::worker(&app.db, w).await?.map(|w| w.owner_id),
        None => None,
    };
    // A fresh token per start: only the current agent process holds a valid one.
    let token = new_secret("hqcs_");
    db::set_session_token_hash(&app.db, s.id, &hash_secret(&token)).await?;
    let mode = if db::mark_start_sent(&app.db, s.id).await? {
        StartMode::Resume
    } else {
        StartMode::New
    };
    Ok(SessionStart {
        session_id: s.id,
        mode,
        runtime: serde_json::from_value(Value::String(s.runtime.clone()))
            .unwrap_or(Runtime::ClaudeCode),
        acp_agent: s.acp_agent.clone(),
        model: s.model.clone(),
        workspace_path: ws.as_ref().map(|w| w.path.clone()),
        cwd: s.cwd.clone(),
        environment: ws
            .map(|w| w.environment)
            .unwrap_or_else(|| "default".into()),
        runtime_session_id: s.runtime_session_id.clone(),
        system_prompt: s.system_prompt.clone(),
        disallowed_tools: serde_json::from_value(s.disallowed_tools.clone()).unwrap_or_default(),
        restricted: s.restricted,
        auto_approve: s.auto_approve,
        session_token: token,
        owner_is_worker_owner: worker_owner == Some(s.owner_id),
        ask_timeout_s: app.config.timing.ask_timeout_s,
    })
}

pub async fn on_start_ok(
    app: &AppState,
    conn: &Arc<WorkerConn>,
    session_id: Uuid,
) -> anyhow::Result<()> {
    {
        let mut st = conn.st.lock().await;
        st.sessions.entry(session_id).or_default().start = StartState::Started;
    }
    db::reset_start_failures(&app.db, session_id).await?;
    // Answers the worker has not acknowledged go first, then pending input (SPEC §5.2).
    for p in db::undelivered_results(&app.db, conn.worker_id).await? {
        if p.session_id == session_id {
            requests::deliver_result(app, &p).await;
        }
    }
    kick(app, session_id, false).await;
    app.session_changed(session_id);
    Ok(())
}

pub async fn on_start_failed(
    app: &AppState,
    conn: &Arc<WorkerConn>,
    session_id: Uuid,
    err: &WireError,
) -> anyhow::Result<()> {
    {
        let mut st = conn.st.lock().await;
        let sc = st.sessions.entry(session_id).or_default();
        sc.start = StartState::NotStarted;
        sc.inflight.clear();
    }
    let n = db::bump_start_failures(&app.db, session_id).await?;
    tracing::warn!(%session_id, failures = n, code = %err.code, retryable = err.retryable, "agent start failed");
    if !err.retryable || n >= app.config.timing.max_start_failures {
        fail_pending(app, session_id, &err.message).await?;
    } else {
        let app = app.clone();
        let delay = app.config.timing.start_retry_delay;
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            kick(&app, session_id, false).await;
        });
    }
    app.session_changed(session_id);
    Ok(())
}

pub async fn on_input_ack(
    app: &AppState,
    conn: &Arc<WorkerConn>,
    session_id: Uuid,
    event_id: Uuid,
) -> anyhow::Result<()> {
    {
        let mut st = conn.st.lock().await;
        if let Some(sc) = st.sessions.get_mut(&session_id) {
            sc.inflight.remove(&event_id);
        }
    }
    if let Some(row) = db::mark_delivered(&app.db, event_id).await? {
        notify_delivery(app, &row);
        // The worker hands input to the runtime one turn at a time (SPEC §17.2), so a
        // delivered input means its turn has started.
        db::set_turn(&app.db, session_id, true, false).await?;
    }
    db::reset_start_failures(&app.db, session_id).await?;
    app.session_changed(session_id);
    Ok(())
}

pub async fn on_input_failed(
    app: &AppState,
    conn: &Arc<WorkerConn>,
    session_id: Uuid,
    event_id: Uuid,
    err: &WireError,
) -> anyhow::Result<()> {
    if err.retryable || err.code == error_code::AGENT_START_FAILED {
        return on_start_failed(app, conn, session_id, err).await;
    }
    {
        let mut st = conn.st.lock().await;
        if let Some(sc) = st.sessions.get_mut(&session_id) {
            sc.inflight.remove(&event_id);
        }
    }
    if let Some(row) = db::mark_input_failed(&app.db, event_id).await? {
        notify_delivery(app, &row);
        if let Some(s) = db::session(&app.db, session_id).await? {
            events::system(
                app,
                &s,
                "input_rejected",
                &format!("Input was not delivered: {}", err.message),
            )
            .await?;
        }
    }
    app.session_changed(session_id);
    Ok(())
}

/// All pending input of the session becomes `failed` (SPEC §5.2).
async fn fail_pending(app: &AppState, session_id: Uuid, reason: &str) -> anyhow::Result<()> {
    for row in db::move_inputs(&app.db, session_id, "pending", "failed").await? {
        notify_delivery(app, &row);
    }
    if let Some(s) = db::session(&app.db, session_id).await? {
        events::system(
            app,
            &s,
            "agent_start_failed",
            &format!("The agent did not start, input was not delivered: {reason}"),
        )
        .await?;
    }
    Ok(())
}

/// `POST /retry`: reset the counter, failed input back to pending, try again (SPEC §5.2).
pub async fn retry(app: &AppState, session_id: Uuid) -> anyhow::Result<usize> {
    sqlx::query(
        "UPDATE sessions SET start_failures = 0, last_turn_error = false, updated_at = now() WHERE id = $1",
    )
    .bind(session_id)
    .execute(&app.db)
    .await?;
    let rows = db::move_inputs(&app.db, session_id, "failed", "pending").await?;
    for row in &rows {
        notify_delivery(app, row);
    }
    kick(app, session_id, false).await;
    app.session_changed(session_id);
    Ok(rows.len())
}

pub fn notify_delivery(app: &AppState, row: &EventRow) {
    app.notify(Notice::Delivery {
        session_id: row.session_id,
        event_id: row.event_id,
        seq: row.seq,
        delivery: row.delivery.clone().unwrap_or_default(),
        delivered_at: row.delivered_at,
    });
}

/// Client frame for a delivery change (SPEC §6).
pub fn delivery_frame(n: &Notice) -> Option<ServerFrame> {
    match n {
        Notice::Delivery {
            session_id,
            event_id,
            seq,
            delivery,
            delivered_at,
        } => Some(ServerFrame::Delivery {
            session_id: *session_id,
            event_id: *event_id,
            seq: (*seq).max(0) as u64,
            delivery: serde_json::from_value(Value::String(delivery.clone())).ok()?,
            delivered_at: *delivered_at,
        }),
        _ => None,
    }
}
