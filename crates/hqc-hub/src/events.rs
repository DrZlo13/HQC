//! Storing events and their side effects (SPEC §4, §5.2).
//!
//! The stored `author` and `payload` are the JSON exactly as received, except U+0000, which
//! Postgres refuses and which becomes U+FFFD (worker frames in `worker_ws`, hub events in
//! [`store_hub_event`]; SPEC §5.2); the typed
//! [`EventKind`] drives only the hub's own logic (open requests, turn bookkeeping, push).

use chrono::Utc;
use hqc_proto::{
    event::{Event, EventKind, SessionState},
    frame::Ack,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    app::{AppState, Notice},
    db::{self, EventRow, Inserted, NewEvent, SessionRow},
    push, requests,
    worker_ws::WorkerConn,
};

/// Where an event came from: the hub writes its own events after it has already acted on
/// them (closed the request, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Worker,
    Hub,
}

/// An `event` frame from a worker: `ack {event_id, seq}`; a duplicate gets the stored
/// `seq`; an event for a session the hub does not know (or that is not this worker's)
/// gets `ack` without `seq` and is dropped (SPEC §5.2).
pub async fn ingest_worker_event(
    app: &AppState,
    conn: &WorkerConn,
    raw: Value,
    ev: &Event,
) -> anyhow::Result<Ack> {
    let dropped = Ack {
        event_id: Some(ev.event_id),
        seq: None,
    };
    let Some(s) = db::session(&app.db, ev.session_id).await? else {
        tracing::debug!(session_id = %ev.session_id, "event for unknown session dropped");
        return Ok(dropped);
    };
    if s.worker_id != Some(conn.worker_id) {
        tracing::warn!(session_id = %ev.session_id, worker_id = %conn.worker_id, "event for another worker's session dropped");
        return Ok(dropped);
    }
    app.worker_activity(s.id);
    if matches!(ev.kind, EventKind::AgentDelta { .. }) {
        // Transport only: to clients without seq, never stored (SPEC §4).
        app.notify(Notice::Event {
            session_id: s.id,
            seq: None,
            event: raw,
        });
        return Ok(dropped);
    }

    let str_field = |k: &str| raw.get(k).and_then(Value::as_str).map(str::to_string);
    let new = NewEvent {
        event_id: ev.event_id,
        session_id: ev.session_id,
        ts: ev.ts,
        author: raw.get("author").cloned().unwrap_or(Value::Null),
        kind: str_field("type").unwrap_or_default(),
        payload: raw.get("payload").cloned().unwrap_or(Value::Null),
        chain: raw.get("chain").filter(|c| !c.is_null()).cloned(),
        client_op_id: str_field("client_op_id"),
        delivery: str_field("delivery"),
        delivered_at: ev.delivered_at,
    };
    match db::insert_event(&app.db, &new).await? {
        Inserted::New(row) => {
            let seq = row.seq;
            after_store(app, &s, &row, &ev.kind, Source::Worker).await?;
            // Here rather than in after_store: auto_approve stores an event itself.
            if let EventKind::ApprovalRequest { request_id, .. } = &ev.kind {
                if s.auto_approve {
                    requests::auto_approve(app, &s, request_id).await?;
                }
            }
            Ok(Ack {
                event_id: Some(ev.event_id),
                seq: Some(seq as u64),
            })
        }
        Inserted::Duplicate(seq) => Ok(Ack {
            event_id: Some(ev.event_id),
            seq: Some(seq as u64),
        }),
        Inserted::UnknownSession => Ok(dropped),
    }
}

/// Store an event produced by the hub (input from a client, answers, cancellations, system).
pub async fn store_hub_event(
    app: &AppState,
    s: &SessionRow,
    mut author: Value,
    kind: &str,
    mut payload: Value,
    client_op_id: Option<String>,
    delivery: Option<&str>,
) -> anyhow::Result<EventRow> {
    hqc_proto::replace_nul_json(&mut author);
    hqc_proto::replace_nul_json(&mut payload);
    let client_op_id = client_op_id.map(|c| hqc_proto::replace_nul(&c).into_owned());
    let typed: EventKind = serde_json::from_value(json!({"type": kind, "payload": payload}))?;
    let new = NewEvent {
        event_id: db::new_id(),
        session_id: s.id,
        ts: Utc::now(),
        author,
        kind: kind.to_string(),
        payload,
        chain: None,
        client_op_id,
        delivery: delivery.map(str::to_string),
        delivered_at: None,
    };
    match db::insert_event(&app.db, &new).await? {
        Inserted::New(row) => {
            after_store(app, s, &row, &typed, Source::Hub).await?;
            Ok(*row)
        }
        other => anyhow::bail!("hub event not stored: {other:?}"),
    }
}

/// `system {text, code}` authored by the hub (SPEC §4).
pub async fn system(
    app: &AppState,
    s: &SessionRow,
    code: &str,
    text: &str,
) -> anyhow::Result<EventRow> {
    store_hub_event(
        app,
        s,
        json!({"kind": "system"}),
        "system",
        json!({"text": text, "code": code}),
        None,
        None,
    )
    .await
}

pub fn user_author(id: Uuid) -> Value {
    json!({"kind": "user", "id": id})
}

async fn after_store(
    app: &AppState,
    s: &SessionRow,
    row: &EventRow,
    kind: &EventKind,
    source: Source,
) -> anyhow::Result<()> {
    // Publish first so that clients see events in seq order even when side effects below
    // store further events.
    app.notify(Notice::Event {
        session_id: s.id,
        seq: Some(row.seq),
        event: row.to_json(),
    });
    match kind {
        EventKind::ApprovalRequest {
            request_id,
            expires_at,
            ..
        } if source == Source::Worker => {
            db::open_request(
                &app.db,
                "approval",
                s.id,
                request_id,
                &row.payload,
                *expires_at,
                s.worker_id,
            )
            .await?;
        }
        EventKind::ClarifyRequest { request_id, .. } if source == Source::Worker => {
            let expires_at = row
                .payload
                .get("expires_at")
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            db::open_request(
                &app.db,
                "clarify",
                s.id,
                request_id,
                &row.payload,
                expires_at,
                s.worker_id,
            )
            .await?;
        }
        EventKind::ApprovalResponse { request_id, .. }
        | EventKind::ApprovalCancel { request_id, .. }
            if source == Source::Worker =>
        {
            db::close_request_by_worker(&app.db, s.id, "approval", request_id).await?;
        }
        EventKind::ClarifyResponse { request_id, .. }
        | EventKind::ClarifyCancel { request_id, .. }
            if source == Source::Worker =>
        {
            db::close_request_by_worker(&app.db, s.id, "clarify", request_id).await?;
        }
        EventKind::AgentMessage { r#final: true, .. } => {
            db::set_turn(&app.db, s.id, false, false).await?;
        }
        EventKind::Status { state, .. } => match state {
            SessionState::Idle => db::set_turn(&app.db, s.id, false, false).await?,
            SessionState::Error => db::set_turn(&app.db, s.id, false, true).await?,
            _ => {}
        },
        _ => {}
    }
    push::on_event(app, s, row, kind);
    app.session_changed(s.id);
    Ok(())
}

/// The worker sent nothing for a `running` session for `idle` (SPEC §17.3): open
/// approval/clarify requests are cancelled with `timeout`, the turn is closed without a
/// final answer and the session shows `error` until the next input.
pub async fn turn_timed_out(
    app: &AppState,
    session_id: Uuid,
    idle: std::time::Duration,
) -> anyhow::Result<()> {
    let Some(s) = db::session(&app.db, session_id).await? else {
        return Ok(());
    };
    if !s.turn_open {
        return Ok(());
    }
    tracing::warn!(%session_id, idle_s = idle.as_secs(), "turn watchdog: no events from the worker, turn lost");
    for p in db::open_requests_of_session(&app.db, session_id).await? {
        requests::cancel(app, &p, hqc_proto::event::CancelReason::Timeout).await?;
    }
    db::set_turn(&app.db, session_id, false, true).await?;
    system(
        app,
        &s,
        "turn_lost",
        &format!("no events for {} s", idle.as_secs()),
    )
    .await?;
    Ok(())
}

/// The agent process went away with a turn in progress: the turn is lost (SPEC §17.3).
pub async fn turn_lost(app: &AppState, session_id: Uuid) -> anyhow::Result<()> {
    let Some(s) = db::session(&app.db, session_id).await? else {
        return Ok(());
    };
    if !s.turn_open {
        return Ok(());
    }
    db::set_turn(&app.db, session_id, false, false).await?;
    system(
        app,
        &s,
        "turn_lost",
        "The agent process stopped during a turn; the turn was lost",
    )
    .await?;
    Ok(())
}
