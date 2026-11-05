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
    event::{Chain, PeerMode},
    frame::{
        error_code, HubFrame, PeerHeader, PermissionMode, Runtime, SessionInput, SessionStart,
        StartMode, WireError,
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
    // The service session has no agent to start (SPEC §10.2).
    if s.archived || s.system || s.locked_by == "vscode" {
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
                if sc.inflight.contains(&p.event_id) {
                    continue;
                }
                let mut frame = input_frame(session_id, p.event_id, &p.payload, p.chain.as_ref());
                let prefix = crate::peers::take_prefix(app, session_id, p.event_id).await?;
                frame.peer_prefix = prefix.messages;
                // Buffered text from a session in Ask never runs in Auto (T40).
                if prefix.capped_by.is_some() {
                    frame.permission_cap = Some(PermissionMode::Ask);
                }
                sc.inflight.insert(p.event_id);
                let id = conn.send(HubFrame::SessionInput(frame));
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

/// The input without `peer_prefix`; `peer` for a `peer.message` (SPEC §7.1); the cap of
/// its chain, if it has one (T39); `schedule_id` of a run of a schedule (§16, §17.2).
fn input_frame(
    session_id: Uuid,
    event_id: Uuid,
    payload: &Value,
    chain: Option<&Value>,
) -> SessionInput {
    let peer = match (
        payload.get("from").and_then(Value::as_str),
        payload.get("mode"),
    ) {
        (Some(from), Some(mode)) => Some(PeerHeader {
            from: from.to_string(),
            mode: serde_json::from_value(mode.clone()).unwrap_or(PeerMode::Unknown),
            request_id: payload
                .get("request_id")
                .and_then(Value::as_str)
                .map(str::to_string),
        }),
        _ => None,
    };
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
        peer_prefix: Vec::new(),
        peer,
        permission_cap: chain
            .and_then(|c| serde_json::from_value::<Chain>(c.clone()).ok())
            .and_then(|c| c.permission_cap),
        schedule_id: payload
            .get("schedule_id")
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
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
    let requested_by_owner = ws
        .as_ref()
        .is_some_and(|w| worker_owner == Some(w.owner_id));
    // The folder's hqc rules (T48); later changes go as `workspace.rules`.
    let allow_rules = match &ws {
        Some(w) => db::workspace_rule_texts(&app.db, w.id).await?,
        None => vec![],
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
        runtime_session_id: s.runtime_session_id.clone(),
        system_prompt: s.system_prompt.clone(),
        disallowed_tools: serde_json::from_value(s.disallowed_tools.clone()).unwrap_or_default(),
        restricted: s.restricted,
        auto_approve: s.auto_approve,
        session_token: token,
        owner_is_worker_owner: worker_owner == Some(s.owner_id),
        ask_timeout_s: app.config.timing.ask_timeout_s,
        // The worker owner added the folder, so they agreed to sessions in it (SPEC §10.2).
        requested_by_owner,
        permission_mode: s.permission_mode.as_deref().and_then(PermissionMode::parse),
        allow_rules,
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

/// `joined`: the runtime took the input into the turn already running (SPEC §17.2).
pub async fn on_input_ack(
    app: &AppState,
    conn: &Arc<WorkerConn>,
    session_id: Uuid,
    event_id: Uuid,
    joined: bool,
) -> anyhow::Result<()> {
    {
        let mut st = conn.st.lock().await;
        if let Some(sc) = st.sessions.get_mut(&session_id) {
            sc.inflight.remove(&event_id);
        }
    }
    if let Some(row) = db::mark_delivered(&app.db, event_id).await? {
        notify_delivery(app, &row);
        // The send buffer it carried reached the agent (SPEC §7.1).
        crate::peers::delivered(app, event_id).await?;
        // A delivered input starts its turn (SPEC §17.2), unless it went into the turn
        // already running (`joined`). The ack bypasses the worker's outbox and may come
        // after the turn's own events (SPEC §5.2): a turn that already has its final answer
        // stays closed, and a joined input's ack after that answer must not open it again.
        // Only a person's message joins (§17.2): a `joined` on another input (a
        // `peer.message`, a run of a schedule) is the worker's mistake, and its turn opens.
        let joinable = row.kind == "user.message" && row.payload.get("schedule_id").is_none();
        if joined && !joinable {
            tracing::warn!(%session_id, %event_id, kind = %row.kind, "a worker joined an input that cannot join a turn; its turn opens");
        }
        let joined = joined && joinable;
        if !joined && !db::turn_answered(&app.db, session_id, event_id).await? {
            db::set_turn(&app.db, session_id, true, false).await?;
        }
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
