//! Storing events and their side effects (SPEC §4, §5.2).
//!
//! The stored `author` and `payload` are the JSON exactly as received, except U+0000, which
//! Postgres refuses and which becomes U+FFFD (worker frames in `worker_ws`, hub events in
//! [`store_hub_event`]; SPEC §5.2), and `joined`, which the hub sets on an input
//! (a person's `user.message`, a `peer.message`) when it is delivered into the running turn
//! (SPEC §4, §17.2); the typed
//! [`EventKind`] drives only the hub's own logic (open requests, turn bookkeeping, push).

use chrono::{DateTime, Utc};
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

/// Most events one `retraction` hides: the runtime takes back the records of one answer.
const MAX_RETRACTED: usize = 1024;

/// Event types only the hub writes (SPEC §4): input of users, answers to requests and
/// messages between sessions, and `schedules.live`. A worker sending one is refused.
fn hub_only(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::UserMessage { .. }
            | EventKind::ApprovalResponse { .. }
            | EventKind::ClarifyResponse { .. }
            | EventKind::PeerMessage { .. }
            | EventKind::SchedulesLive { .. }
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

/// The event ends the text of `agent.delta` clients show, and the thinking (SPEC §6), as the
/// client does.
fn ends_live_text(kind: &EventKind) -> bool {
    match kind {
        EventKind::AgentMessage { .. } => true,
        // The runtime took the answer being written back (SPEC 0.179): no message ends it.
        EventKind::Retraction { .. } => true,
        EventKind::Status { state, .. } => *state != SessionState::Running,
        _ => false,
    }
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
        ..Ack::default()
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
    if let EventKind::SubagentsLive { agents } = &ev.kind {
        // Transport only, like `agent.delta` (SPEC §4). An archived session's too: the empty
        // list its stopped agent sends drops the one kept.
        app.worker_activity(s.id);
        app.live_agents_changed(s.id, raw, agents.is_empty());
        return Ok(dropped);
    }
    if let EventKind::TokensRate { output_per_s } = &ev.kind {
        // Transport only, like `subagents.live`: kept for the session object and its summary.
        app.worker_activity(s.id);
        if app.token_rate_changed(s.id, *output_per_s) {
            app.session_changed(s.id);
        }
        return Ok(dropped);
    }
    if s.archived && !ends_turn(&ev.kind) {
        tracing::debug!(session_id = %s.id, kind = kind_name, "event for an archived session dropped");
        return Ok(dropped);
    }
    app.worker_activity(s.id);
    if let EventKind::AgentDelta {
        text,
        reset,
        thinking,
    } = &ev.kind
    {
        // Transport only: to clients without seq, never stored (SPEC §4).
        app.delta(s.id, raw, text, *reset, *thinking);
        return Ok(dropped);
    }
    if ends_live_text(&ev.kind) {
        app.end_live_text(s.id);
    } else if matches!(ev.kind, EventKind::AgentThinking { .. }) {
        app.end_live_thinking(s.id);
    }
    let str_field = |k: &str| raw.get(k).and_then(Value::as_str).map(str::to_string);
    let mut payload = raw.get("payload").cloned().unwrap_or(Value::Null);
    // Only the hub's own card asks the owner for a secret (SPEC 0.147).
    if matches!(ev.kind, EventKind::ApprovalRequest { .. }) {
        if let Some(p) = payload.as_object_mut() {
            if p.remove("secret").is_some() {
                tracing::warn!(session_id = %s.id, "a worker's approval.request carried `secret`: dropped");
            }
        }
    }
    // A retraction hides events in every client and in forks (SPEC 0.179): it may name only the
    // agent's own output in this session, at most MAX_RETRACTED events; other ids are left out
    // of what is stored (the worker also names records that gave no event).
    if let EventKind::Retraction { event_ids } = &ev.kind {
        let named = &event_ids[..event_ids.len().min(MAX_RETRACTED)];
        let kept = db::retractable(&app.db, s.id, named).await?;
        if kept.len() != event_ids.len() {
            tracing::info!(session_id = %s.id, named = event_ids.len(), kept = kept.len(), "retraction: ids it may not hide left out");
        }
        payload = json!({ "event_ids": kept });
    }
    let new = NewEvent {
        event_id: ev.event_id,
        session_id: ev.session_id,
        ts: ev.ts,
        author: raw.get("author").cloned().unwrap_or(Value::Null),
        kind: str_field("type").unwrap_or_default(),
        payload,
        chain: None,
        client_op_id: str_field("client_op_id"),
        delivery: None,
        delivered_at: None,
    };
    // A worker's request is opened before its card is stored. One whose `request_id` an open
    // request of another kind in the session holds is not stored at all: an answer names its
    // request by the id alone, so the owner's answer to that card would answer the other one
    // (a publication, SPEC §10.1). A repeat of the same request is stored as before.
    let opens = match &ev.kind {
        EventKind::ApprovalRequest {
            request_id,
            expires_at,
            ..
        } => Some(("approval", request_id, *expires_at)),
        EventKind::ClarifyRequest { request_id, .. } => Some((
            "clarify",
            request_id,
            new.payload
                .get("expires_at")
                .and_then(|v| serde_json::from_value(v.clone()).ok()),
        )),
        _ => None,
    };
    if let Some((kind, request_id, expires_at)) = opens {
        let opened = db::open_request_as(
            &app.db,
            kind,
            s.id,
            request_id,
            &new.payload,
            expires_at,
            s.worker_id,
        )
        .await?;
        if opened == db::Opened::Taken {
            tracing::warn!(session_id = %s.id, worker_id = %conn.worker_id, %request_id, kind, "a worker's request with the id of an open request of another kind dropped");
            return Ok(dropped);
        }
    }
    // A cancel from the worker is stored in the transaction that closes its request, so one
    // that fails to be stored leaves the request open for its resend. One the hub closed
    // already (`POST /stop` cancels before it stops the turn, an answer that crossed the
    // cancel; SPEC §10.1) has its cancel or answer in the feed: a second one is not stored. A
    // cancel sent again after its `ack` was lost gets its `seq`.
    let cancelled = match &ev.kind {
        EventKind::ApprovalCancel { request_id, .. } => Some(("approval", request_id)),
        EventKind::ClarifyCancel { request_id, .. } => Some(("clarify", request_id)),
        _ => None,
    };
    let inserted = match cancelled {
        Some((kind, request_id)) => {
            let Some(i) = db::insert_worker_cancel(&app.db, &new, kind, request_id).await? else {
                tracing::info!(session_id = %s.id, %request_id, kind = kind_name, "cancel of a closed request dropped");
                return Ok(dropped);
            };
            i
        }
        None => db::insert_event(&app.db, &new).await?,
    };
    match inserted {
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
                ..Ack::default()
            }))
        }
        Inserted::Duplicate(seq) => Ok(Ingested::Ack(Ack {
            event_id: Some(ev.event_id),
            seq: Some(seq as u64),
            ..Ack::default()
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
    // Clients put an input that came while the agent's text or thinking was being written
    // under it (SPEC §6).
    if kind == "user.message" && app.text_open(s.id) {
        payload["during_text"] = json!(true);
    }
    let typed: EventKind = serde_json::from_value(json!({"type": kind, "payload": payload}))?;
    if ends_live_text(&typed) {
        app.end_live_text(s.id);
    }
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
    payload: Value,
    chain: Value,
    delivery: Option<&str>,
) -> anyhow::Result<EventRow> {
    store_peer_message_at(app, s, event_id, author, payload, chain, delivery, None).await
}

/// [`store_peer_message`] with `delivered_at` set: a message the session took on arrival (a
/// room, which has no delivery) shows as delivered to its sender.
#[allow(clippy::too_many_arguments)]
pub async fn store_peer_message_at(
    app: &AppState,
    s: &SessionRow,
    event_id: Uuid,
    author: Value,
    mut payload: Value,
    chain: Value,
    delivery: Option<&str>,
    delivered_at: Option<DateTime<Utc>>,
) -> anyhow::Result<EventRow> {
    hqc_proto::replace_nul_json(&mut payload);
    if app.text_open(s.id) {
        payload["during_text"] = json!(true);
    }
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
        delivered_at,
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

/// `45 s`, `12 min`, `1 h 20 min`, `3 d 2 h`: the two largest units, rounded down, a zero
/// second one left out; for the notes the hub gives an agent (SPEC §8.1, §17.3).
pub fn elapsed(secs: i64) -> String {
    let (d, h, m) = (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{secs} s"),
        (0, 0, m) => format!("{m} min"),
        (0, h, 0) => format!("{h} h"),
        (0, h, m) => format!("{h} h {m} min"),
        (d, 0, _) => format!("{d} d"),
        (d, h, _) => format!("{d} d {h} h"),
    }
}

async fn after_store(
    app: &AppState,
    s: &SessionRow,
    row: &EventRow,
    kind: &EventKind,
    source: Source,
) -> anyhow::Result<()> {
    // The runtime refused a model switch and kept this model: the session's again (SPEC §4,
    // §8.1), before the event goes out, since a client reads the session again on it. A name
    // hqc would not take from the user is not taken from it.
    if let EventKind::System {
        code,
        model: Some(m),
        ..
    } = kind
    {
        if source == Source::Worker
            && code == hqc_proto::event::MODEL_NOT_SWITCHED
            && crate::api::valid_model(m)
        {
            db::set_model(&app.db, s.id, Some(m)).await?;
        }
    }
    // Publish first so that clients see events in seq order even when side effects below
    // store further events.
    app.notify(Notice::Event {
        session_id: s.id,
        seq: row.seq,
        event: row.to_json(),
    });
    // A worker's `approval.request` and `clarify.request` opened their request before they were
    // stored (`ingest_worker_event`).
    match kind {
        EventKind::AgentMessage {
            r#final: true,
            text,
            in_reply_to,
            runtime,
            interrupted,
            ..
        } => {
            db::set_turn(&app.db, s.id, false, false).await?;
            if source != Source::Hub {
                // "Session N" gets a real title (T21).
                crate::titles::after_first_answer(app, s);
                // The reply to an `hqc_ask` (SPEC §7.1). A text an interrupt cut off (Stop, a
                // pause, "No" to a permission request) is no reply: the turn's `status idle`
                // fails the ask (`peers::turn_stopped`).
                if let Some(input) = in_reply_to.as_ref().filter(|_| !*interrupted) {
                    crate::peers::on_final(app, s, *input, text, (row.event_id, row.seq)).await?;
                }
            }
            // An agent's answer to its room is its post there, unless an interruption cut it
            // off (SPEC §7.1.1).
            if let (Source::Worker, Some(room), Some(input)) = (source, s.room_id, in_reply_to) {
                // Boxed: the post is a hub event, which comes back here.
                Box::pin(crate::rooms::on_final(
                    app,
                    s,
                    room,
                    *input,
                    text,
                    *runtime,
                    *interrupted,
                    row,
                ))
                .await?;
            }
        }
        EventKind::Status {
            state, in_reply_to, ..
        } => match state {
            SessionState::Idle => {
                db::set_turn(&app.db, s.id, false, false).await?;
                // An ask the turn's input carried and still open got no final message from that
                // turn (the user's "No" to a permission request, `POST /stop`): it fails now
                // (SPEC §7.1). Without `in_reply_to` (an older worker, or a turn that names no
                // input, §8.1) it waits for its timeout.
                // Boxed: failing it may store a hub event, which comes back here.
                if let Some(input) = in_reply_to {
                    Box::pin(crate::peers::turn_stopped(app, s, *input)).await?;
                }
                room_turn_ended(app, s, source).await?;
                // A session paused during the turn stops now (SPEC §17.3).
                crate::pause::stop_if_paused(app, s.id).await?;
            }
            SessionState::Error => {
                db::set_turn(&app.db, s.id, false, true).await?;
                room_turn_ended(app, s, source).await?;
                crate::pause::stop_if_paused(app, s.id).await?;
            }
            // A turn the runtime started by itself, with no input to open it (SPEC §17.1); only
            // while its process runs: the outbox of a restarted worker sends it again after the
            // `hello` that stopped the session and lost its turn (§17.3).
            SessionState::Running if source == Source::Worker && s.process_state == "running" => {
                db::set_turn(&app.db, s.id, true, false).await?
            }
            _ => {}
        },
        _ => {}
    }
    // An import brings hundreds of old answers; what VS Code does live is the user's own
    // work at that machine. Neither is worth a notification.
    if source != Source::Mirror {
        push::on_event(app, s, row, kind);
    }
    // What the agent waited on in the turn is over with it (SPEC §5.4).
    if ends_turn(kind) {
        app.drop_activity(s.id);
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

/// The worker's turn of a room's agent `s` ended: in a conversation the speaker's moves the
/// circle on (SPEC §7.1.1).
async fn room_turn_ended(app: &AppState, s: &SessionRow, source: Source) -> anyhow::Result<()> {
    if let (Source::Worker, Some(room)) = (source, s.room_id) {
        // Boxed: what it stores are hub events, which come back here.
        Box::pin(crate::rooms::turn_ended(app, s, room)).await?;
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
    app.drop_activity(session_id);
    let why = format!("no events for {} s", idle.as_secs());
    system(app, &s, "turn_lost", &why).await?;
    // The speaker of a conversation room: the room stops (SPEC §7.1.1).
    crate::rooms::turn_lost(app, &s, &why).await?;
    crate::pause::stop_if_paused(app, session_id).await?;
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
    app.drop_activity(session_id);
    system(
        app,
        &s,
        "turn_lost",
        "The agent process stopped during a turn; the turn was lost",
    )
    .await?;
    // An ask waiting for this turn's reply fails now (SPEC §7.1).
    crate::peers::turn_lost(app, &s).await?;
    // The speaker of a conversation room: the room stops (SPEC §7.1.1).
    crate::rooms::turn_lost(app, &s, "its agent process stopped").await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_names_the_two_largest_units() {
        assert_eq!(elapsed(0), "0 s");
        assert_eq!(elapsed(45), "45 s");
        assert_eq!(elapsed(60), "1 min");
        assert_eq!(elapsed(12 * 60 + 40), "12 min");
        assert_eq!(elapsed(3_600), "1 h");
        assert_eq!(elapsed(3_600 + 20 * 60 + 59), "1 h 20 min");
        assert_eq!(elapsed(86_400 + 59 * 60), "1 d");
        assert_eq!(elapsed(3 * 86_400 + 2 * 3_600 + 30 * 60), "3 d 2 h");
    }
}
