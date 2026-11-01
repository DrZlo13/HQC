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
    /// A transcript record from the worker's mirror (SPEC §9.2): history, it opens no
    /// requests.
    Mirror,
}

/// Side effects of a stored mirrored event: clients are notified, a final answer closes
/// the turn (SPEC §9.2).
pub async fn after_mirror_store(
    app: &AppState,
    s: &SessionRow,
    row: &EventRow,
    kind: &EventKind,
) -> anyhow::Result<()> {
    after_store(app, s, row, kind, Source::Mirror).await
}

/// `system.code` of the worker's log lines in its service session (SPEC §10.2).
pub const WORKER_LOG: &str = "worker_log";

/// What became of an `event` frame from a worker.
#[derive(Debug)]
pub enum Ingested {
    /// `ack`: with `seq` when stored, without when dropped.
    Ack(Ack),
    /// An event the worker may not write (SPEC §5.2, §20): `error invalid_frame`.
    Refused(String),
}

/// Event types only the hub writes (SPEC §4): input of users, answers to requests and
/// messages between sessions. A worker sending one is refused.
fn hub_only(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::UserMessage { .. }
            | EventKind::ApprovalResponse { .. }
            | EventKind::ClarifyResponse { .. }
            | EventKind::PeerMessage { .. }
    )
}

/// The event closes the current turn (SPEC §17.1): the final `agent.message` or `status`
/// `idle` / `error`.
fn ends_turn(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::AgentMessage { r#final: true, .. }
            | EventKind::Status {
                state: SessionState::Idle | SessionState::Error,
                ..
            }
    )
}

/// An `event` frame from a worker: `ack {event_id, seq}`; a duplicate gets the stored
/// `seq`; an event for a session the hub does not know (or that is not this worker's)
/// gets `ack` without `seq` and is dropped (SPEC §5.2). So is an event for an archived
/// session, unless it closes the turn. Events only the hub writes, and in the worker's
/// service session anything but `system worker_log`, are refused (SPEC §10.2, §20). `chain`
/// and `delivery` belong to inputs, which only the hub writes: a worker's are dropped, so
/// that it can neither reset the chain of its session's turn nor queue input (SPEC §7.1).
pub async fn ingest_worker_event(
    app: &AppState,
    conn: &WorkerConn,
    raw: Value,
    ev: &Event,
) -> anyhow::Result<Ingested> {
    let dropped = Ingested::Ack(Ack {
        event_id: Some(ev.event_id),
        seq: None,
    });
    let Some(s) = db::session(&app.db, ev.session_id).await? else {
        tracing::debug!(session_id = %ev.session_id, "event for unknown session dropped");
        return Ok(dropped);
    };
    if s.worker_id != Some(conn.worker_id) {
        tracing::warn!(session_id = %ev.session_id, worker_id = %conn.worker_id, "event for another worker's session dropped");
        return Ok(dropped);
    }
    let kind_name = raw.get("type").and_then(Value::as_str).unwrap_or_default();
    if hub_only(&ev.kind) {
        tracing::warn!(session_id = %s.id, worker_id = %conn.worker_id, kind = kind_name, "event only the hub writes refused");
        return Ok(Ingested::Refused(format!(
            "`{kind_name}` is written by the hub only"
        )));
    }
    if s.system && !matches!(&ev.kind, EventKind::System { code, .. } if code == WORKER_LOG) {
        tracing::warn!(session_id = %s.id, worker_id = %conn.worker_id, kind = kind_name, "event in the service session refused");
        return Ok(Ingested::Refused(
            "the service session takes only `system worker_log` from the worker".into(),
        ));
    }
    if s.archived && !ends_turn(&ev.kind) {
        tracing::debug!(session_id = %s.id, kind = kind_name, "event for an archived session dropped");
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
        chain: None,
        client_op_id: str_field("client_op_id"),
        delivery: None,
        delivered_at: None,
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
            Ok(Ingested::Ack(Ack {
                event_id: Some(ev.event_id),
                seq: Some(seq as u64),
            }))
        }
        Inserted::Duplicate(seq) => Ok(Ingested::Ack(Ack {
            event_id: Some(ev.event_id),
            seq: Some(seq as u64),
        })),
        Inserted::UnknownSession => Ok(dropped),
    }
}

/// Store an event produced by the hub (input from a client, answers, cancellations, system).
pub async fn store_hub_event(
    app: &AppState,
    s: &SessionRow,
    author: Value,
    kind: &str,
    payload: Value,
    client_op_id: Option<String>,
    delivery: Option<&str>,
) -> anyhow::Result<EventRow> {
    let id = db::new_id();
    match store_hub_event_as(
        app,
        s,
        id,
        author,
        kind,
        payload,
        client_op_id,
        delivery,
        None,
    )
    .await?
    {
        Some(row) => Ok(row),
        None => anyhow::bail!("hub event {id} not stored: a duplicate"),
    }
}

/// [`store_hub_event`] under a given `event_id`; `None` if an event with it is stored
/// already (a repeated `mcp.call`, SPEC §5.2).
pub async fn store_hub_event_once(
    app: &AppState,
    s: &SessionRow,
    event_id: Uuid,
    author: Value,
    kind: &str,
    payload: Value,
) -> anyhow::Result<Option<EventRow>> {
    store_hub_event_as(app, s, event_id, author, kind, payload, None, None, None).await
}

/// [`store_hub_event_once`] with a `delivery` and a `chain` (an input of the hub cron, SPEC
/// §16: a schedule an agent created carries its chain, §7.1).
#[allow(clippy::too_many_arguments)]
pub async fn store_hub_event_with_id(
    app: &AppState,
    s: &SessionRow,
    event_id: Uuid,
    author: Value,
    kind: &str,
    payload: Value,
    delivery: Option<&str>,
    chain: Option<Value>,
) -> anyhow::Result<Option<EventRow>> {
    store_hub_event_as(
        app, s, event_id, author, kind, payload, None, delivery, chain,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn store_hub_event_as(
    app: &AppState,
    s: &SessionRow,
    event_id: Uuid,
    mut author: Value,
    kind: &str,
    mut payload: Value,
    client_op_id: Option<String>,
    delivery: Option<&str>,
    chain: Option<Value>,
) -> anyhow::Result<Option<EventRow>> {
    hqc_proto::replace_nul_json(&mut author);
    hqc_proto::replace_nul_json(&mut payload);
    let client_op_id = client_op_id.map(|c| hqc_proto::replace_nul(&c).into_owned());
    let typed: EventKind = serde_json::from_value(json!({"type": kind, "payload": payload}))?;
    let new = NewEvent {
        event_id,
        session_id: s.id,
        ts: Utc::now(),
        author,
        kind: kind.to_string(),
        payload,
        chain,
        client_op_id,
        delivery: delivery.map(str::to_string),
        delivered_at: None,
    };
    match db::insert_event(&app.db, &new).await? {
        Inserted::New(row) => {
            after_store(app, s, &row, &typed, Source::Hub).await?;
            Ok(Some(*row))
        }
        Inserted::Duplicate(_) => Ok(None),
        other => anyhow::bail!("hub event not stored: {other:?}"),
    }
}

/// A `peer.message` (SPEC §7.1) under `event_id`, with the sender's `chain`. A repeat of
/// the same `event_id` (a call sent again, SPEC §5.2) stores nothing and returns the row
/// stored the first time.
pub async fn store_peer_message(
    app: &AppState,
    s: &SessionRow,
    event_id: Uuid,
    author: Value,
    mut payload: Value,
    chain: Value,
    delivery: Option<&str>,
) -> anyhow::Result<EventRow> {
    hqc_proto::replace_nul_json(&mut payload);
    let typed: EventKind =
        serde_json::from_value(json!({"type": "peer.message", "payload": payload}))?;
    let new = NewEvent {
        event_id,
        session_id: s.id,
        ts: Utc::now(),
        author,
        kind: "peer.message".into(),
        payload,
        chain: Some(chain),
        client_op_id: None,
        delivery: delivery.map(str::to_string),
        delivered_at: None,
    };
    match db::insert_event(&app.db, &new).await? {
        Inserted::New(row) => {
            after_store(app, s, &row, &typed, Source::Hub).await?;
            Ok(*row)
        }
        Inserted::Duplicate(_) => db::event_by_id(&app.db, event_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("peer message {event_id} vanished")),
        Inserted::UnknownSession => anyhow::bail!("peer message not stored: no session {}", s.id),
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

/// A `system` event with more payload than `text` and `code` (`reason`, SPEC §4).
pub async fn system_with(
    app: &AppState,
    s: &SessionRow,
    payload: Value,
) -> anyhow::Result<EventRow> {
    store_hub_event(
        app,
        s,
        json!({"kind": "system"}),
        "system",
        payload,
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
        EventKind::AgentMessage {
            r#final: true,
            text,
            in_reply_to,
        } => {
            db::set_turn(&app.db, s.id, false, false).await?;
            if source != Source::Hub {
                // "Session N" gets a real title (T21).
                crate::titles::after_first_answer(app, s);
                // The reply to an `hqc_ask` (SPEC §7.1).
                if let Some(input) = in_reply_to {
                    crate::peers::on_final(app, s, *input, text, (row.event_id, row.seq)).await?;
                }
            }
        }
        EventKind::Status { state, .. } => match state {
            SessionState::Idle => db::set_turn(&app.db, s.id, false, false).await?,
            SessionState::Error => db::set_turn(&app.db, s.id, false, true).await?,
            _ => {}
        },
        _ => {}
    }
    // An import brings hundreds of old answers; what VS Code does live is the user's own
    // work at that machine. Neither is worth a notification.
    if source != Source::Mirror {
        push::on_event(app, s, row, kind);
    }
    app.session_changed(s.id);
    if matches!(
        kind,
        EventKind::ApprovalRequest { .. }
            | EventKind::ApprovalResponse { .. }
            | EventKind::ApprovalCancel { .. }
            | EventKind::ClarifyRequest { .. }
            | EventKind::ClarifyResponse { .. }
            | EventKind::ClarifyCancel { .. }
    ) {
        // The request row is opened or closed by now (above, or by the caller before it
        // stored the event).
        app.requests_changed(s.owner_id);
    }
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
    // An ask waiting for this turn's reply fails now (SPEC §7.1).
    crate::peers::turn_lost(app, &s).await?;
    Ok(())
}
