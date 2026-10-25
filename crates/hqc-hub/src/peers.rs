//! `hqc_send` and `hqc_ask` (SPEC §7.1): messages between the sessions of one owner.
//!
//! - The target is an address `<project-slug>/<session-slug>` or an id of a session of the
//!   caller's owner, not archived, not a worker's service session; anything else is
//!   `not_found`. The calling session itself, a blank text and a target driven from VS Code
//!   are refused at once.
//! - The message is a `peer.message` in the target, authored by the calling agent, with the
//!   chain of the caller's current turn moved on ([`next_chain`]). Its `event_id` is derived
//!   from the caller and the call's `request_id` ([`message_id`]): a call the worker sends
//!   again after its answer was lost (SPEC §5.2) finds the message it stored the first time
//!   and gets the same answer, nothing is stored or delivered twice.
//! - `send` to a session with `wake_on_send` and every `ask` go to the input queue
//!   (`delivery: pending`). Other sends wait in the send buffer (`peer_buffer`, migration
//!   0015) without `delivery`, and go to the agent in `peer_prefix` of the session's next
//!   input ([`take_prefix`]); that input's `ack` clears them ([`delivered`]). The buffer
//!   keeps the newest [`MAX_BUFFER_MESSAGES`] / [`MAX_BUFFER_BYTES`]; the older ones are
//!   dropped with a `system peer_evicted` in the target.
//! - An ask is a `PendingRequest` of the asking session (`kind: ask`, `worker_id` of its
//!   worker, `payload {target_session_id, event_id, to}`), answered later: the target's
//!   final `agent.message` with `in_reply_to` = the message ([`on_final`]), the timeout
//!   ([`expire`]), `mcp.cancel` ([`cancelled`]) or a restart of the asking worker
//!   ([`worker_restarted`]). The answer is an `mcp.result` frame kept in `result` until the
//!   worker acks it, so an asking worker that was offline gets it after `hello`.

use chrono::{DateTime, Utc};
use hqc_proto::{
    event::Chain,
    frame::{McpCall, McpCancel, McpError, McpResult, PeerPrefix},
    mcp::{cancel_reason, error_code},
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    app::AppState,
    db::{self, EventRow, PendingRow, SessionRow},
    delivery, events,
    mcp::{args, internal, Answer},
    requests,
};

/// The send buffer of one session keeps at most this many messages (SPEC §7.1) ...
pub const MAX_BUFFER_MESSAGES: i64 = 50;
/// ... and at most this many bytes of their texts.
pub const MAX_BUFFER_BYTES: i64 = 64 * 1024;
/// Chain limits (SPEC §7.1): nested asks, and peer messages in one chain.
pub const MAX_DEPTH: u8 = 3;
pub const MAX_HOPS: u8 = 20;

/// `system.code` of messages dropped from a full send buffer (SPEC §4).
pub const PEER_EVICTED: &str = "peer_evicted";
/// `system.code` of an ask closed by `mcp.cancel` (SPEC §4).
pub const ASK_CANCELLED: &str = "ask_cancelled";

/// The `event_id` of the `peer.message` of call `request_id` of session `caller`.
pub fn message_id(caller: Uuid, request_id: &str) -> Uuid {
    crate::mcp::once_id("peer.message", caller, request_id)
}

fn err(code: &str, message: impl Into<String>) -> McpError {
    McpError::new(code, message)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendArgs {
    text: String,
    session: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskArgs {
    text: String,
    session: String,
    timeout_s: Option<u64>,
}

/// `<project-slug>/<session-slug>` of a session.
pub async fn address(app: &AppState, s: &SessionRow) -> anyhow::Result<String> {
    let project = db::project(&app.db, s.project_id).await?;
    Ok(format!(
        "{}/{}",
        project.map_or_else(|| "?".into(), |p| p.slug),
        s.slug
    ))
}

/// The owner's session `addr` points at (an address or an id), with its address.
pub(crate) async fn target(
    app: &AppState,
    caller: &SessionRow,
    addr: &str,
) -> Result<(SessionRow, String), McpError> {
    let not_found = || err(error_code::NOT_FOUND, format!("no session {addr}"));
    let addr = addr.trim();
    let found = if let Ok(id) = addr.parse::<Uuid>() {
        db::session(&app.db, id).await.map_err(internal)?
    } else {
        let (p, s) = addr.split_once('/').ok_or_else(not_found)?;
        let project = db::projects_of(&app.db, caller.owner_id)
            .await
            .map_err(internal)?
            .into_iter()
            .find(|x| x.slug == p)
            .ok_or_else(not_found)?;
        db::sessions_of_project(&app.db, project.id)
            .await
            .map_err(internal)?
            .into_iter()
            .find(|x| x.slug == s)
    };
    let t = found
        .filter(|t| t.owner_id == caller.owner_id && !t.archived && !t.system)
        .ok_or_else(not_found)?;
    let a = address(app, &t).await.map_err(internal)?;
    Ok((t, a))
}

/// The checks both tools make before anything is stored.
async fn checked_target(
    app: &AppState,
    caller: &SessionRow,
    text: &str,
    session: &str,
) -> Result<(SessionRow, String), McpError> {
    if text.trim().is_empty() {
        return Err(err(error_code::INVALID_ARGS, "text is empty"));
    }
    let (t, to) = target(app, caller, session).await?;
    if t.id == caller.id {
        return Err(err(
            error_code::INVALID_ARGS,
            "that is this session itself; send to another one",
        ));
    }
    if t.locked_by == "vscode" {
        // No silent queue for a session driven from VS Code (SPEC §9.3).
        return Err(err(
            error_code::UNAVAILABLE,
            format!("{to} is being driven from VS Code; it takes no messages until it is taken over in hqc"),
        ));
    }
    Ok((t, to))
}

/// The chain of the input that started the session's current turn (SPEC §7.1): the last
/// input delivered to it. A human's turn is a chain of its own with nothing in it yet
/// (`depth` and `hops` 0), its id derived from the input, so every call of that turn shares
/// it. `None`: no turn yet.
pub(crate) async fn turn_chain(app: &AppState, session_id: Uuid) -> anyhow::Result<Option<Chain>> {
    let row: Option<(Uuid, Option<Value>)> = sqlx::query_as(
        "SELECT event_id, chain FROM events WHERE session_id = $1 AND delivery = 'delivered'
         ORDER BY seq DESC LIMIT 1",
    )
    .bind(session_id)
    .fetch_optional(&app.db)
    .await?;
    Ok(row.map(|(input, chain)| input_chain(session_id, input, chain)))
}

/// The chain of input `input` of a session: its own, or for a human's input one derived
/// from it.
fn input_chain(session_id: Uuid, input: Uuid, chain: Option<Value>) -> Chain {
    chain
        .and_then(|c| serde_json::from_value(c).ok())
        .unwrap_or(Chain {
            chain_id: crate::mcp::once_id("chain", session_id, &input.to_string()),
            depth: 0,
            hops: 0,
        })
}

/// The chain of call `c` of session `caller`: that of the input the worker says the turn
/// runs on (`turn_input`), if it is an input of the session given to the agent or on its
/// way, not older than the last delivered one; otherwise [`turn_chain`]. The worker knows
/// the turn before the hub does: the input's `ack` bypasses its outbox and is lost with a
/// dropped connection, and until the resent input is acked again the hub's last delivered
/// input is the previous turn's (SPEC §7.1). It cannot pick an older turn's chain.
pub(crate) async fn call_chain(
    app: &AppState,
    caller: &SessionRow,
    c: &McpCall,
) -> anyhow::Result<Option<Chain>> {
    if let Some(input) = c.turn_input {
        let row: Option<(Option<Value>,)> = sqlx::query_as(
            "SELECT e.chain FROM events e
             WHERE e.event_id = $1 AND e.session_id = $2
               AND e.type IN ('user.message', 'peer.message')
               AND e.delivery IN ('pending', 'delivered')
               AND e.seq >= COALESCE((SELECT max(d.seq) FROM events d
                                      WHERE d.session_id = $2 AND d.delivery = 'delivered'), 0)",
        )
        .bind(input)
        .bind(caller.id)
        .fetch_optional(&app.db)
        .await?;
        if let Some((chain,)) = row {
            return Ok(Some(input_chain(caller.id, input, chain)));
        }
        tracing::debug!(session_id = %caller.id, %input, "turn_input is not the session's current input; the hub's last delivered one counts");
    }
    turn_chain(app, caller.id).await
}

/// The caps a new peer message of `caller` in `chain` must fit (SPEC §20): messages per
/// session and minute, messages per chain.
pub(crate) async fn admit(
    app: &AppState,
    caller: &SessionRow,
    chain: &Chain,
) -> Result<(), McpError> {
    let limits = &app.config.limits;
    let in_chain: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE type = 'peer.message' AND chain ->> 'chain_id' = $1",
    )
    .bind(chain.chain_id.to_string())
    .fetch_one(&app.db)
    .await
    .map_err(internal)?;
    if in_chain >= limits.peer_messages_per_chain {
        return Err(err(
            error_code::LIMIT_EXCEEDED,
            format!(
                "one chain carries at most {} messages between sessions; a person has to continue it",
                limits.peer_messages_per_chain
            ),
        ));
    }
    if !crate::mcp::per_minute(
        app,
        "peer_messages",
        caller.id,
        limits.peer_messages_per_session_min,
    ) {
        return Err(crate::mcp::too_often("messages to other sessions"));
    }
    Ok(())
}

/// The chain of a new message from a turn with chain `turn`: a human's turn starts one,
/// `hops` grows on every message and `depth` on every ask; over a limit is an error, which
/// only a human's turn gets past (SPEC §7.1).
pub fn next_chain(turn: Option<Chain>, ask: bool) -> Result<Chain, McpError> {
    let c = match turn {
        None => Chain {
            chain_id: Uuid::now_v7(),
            depth: u8::from(ask),
            hops: 1,
        },
        Some(c) => Chain {
            chain_id: c.chain_id,
            depth: c.depth.saturating_add(u8::from(ask)),
            hops: c.hops.saturating_add(1),
        },
    };
    if c.depth > MAX_DEPTH {
        return Err(err(
            error_code::LIMIT_EXCEEDED,
            format!("asks nest at most {MAX_DEPTH} deep in one chain; a person has to continue it"),
        ));
    }
    if c.hops > MAX_HOPS {
        return Err(err(
            error_code::LIMIT_EXCEEDED,
            format!("one chain carries at most {MAX_HOPS} messages between sessions; a person has to continue it"),
        ));
    }
    Ok(c)
}

/// Stores the `peer.message` of a call, or finds the one stored by an earlier copy of it.
/// An ask, and a send to a session that wakes on them, go to the input queue (`pending`);
/// other sends have no `delivery` (the send buffer).
async fn store_message(
    app: &AppState,
    caller: &SessionRow,
    t: &SessionRow,
    request_id: &str,
    text: &str,
    ask: bool,
    chain: &Chain,
) -> Result<EventRow, McpError> {
    let delivery = (ask || t.wake_on_send).then_some("pending");
    let from = address(app, caller).await.map_err(internal)?;
    let mut payload = json!({
        "from_session_id": caller.id,
        "from": from,
        "text": text,
        "mode": if ask { "ask" } else { "send" },
    });
    if ask {
        payload["request_id"] = json!(request_id);
    }
    events::store_peer_message(
        app,
        t,
        message_id(caller.id, request_id),
        json!({"kind": "agent", "id": caller.id}),
        payload,
        serde_json::to_value(chain).map_err(internal)?,
        delivery,
    )
    .await
    .map_err(internal)
}

/// `hqc_send(text, session)`: `{to, delivery: queued | buffered}`.
pub async fn send(app: &AppState, caller: &SessionRow, c: &McpCall) -> Result<Answer, McpError> {
    let a: SendArgs = args(c.args.clone())?;
    let id = message_id(caller.id, &c.request_id);
    if let Some(row) = db::event_by_id(&app.db, id).await.map_err(internal)? {
        // A repeat of a call that ran: the same answer (SPEC §5.2).
        let t = db::session(&app.db, row.session_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| err(error_code::NOT_FOUND, "the target session is gone"))?;
        let to = address(app, &t).await.map_err(internal)?;
        return Ok(Answer::Now(send_result(&to, row.delivery.is_some())));
    }
    let (t, to) = checked_target(app, caller, &a.text, &a.session).await?;
    let turn = call_chain(app, caller, c).await.map_err(internal)?;
    let chain = next_chain(turn, false)?;
    admit(app, caller, &chain).await?;
    let wake = t.wake_on_send;
    let row = store_message(app, caller, &t, &c.request_id, &a.text, false, &chain).await?;
    if wake {
        delivery::kick(app, t.id, false).await;
    } else {
        buffer(app, &t, &row, a.text.len())
            .await
            .map_err(internal)?;
    }
    tracing::info!(from = %caller.id, to = %t.id, wake, "peer message sent");
    Ok(Answer::Now(send_result(&to, wake)))
}

fn send_result(to: &str, queued: bool) -> Value {
    json!({"to": to, "delivery": if queued { "queued" } else { "buffered" }})
}

/// `hqc_ask(text, session, timeout_s?)`: answered later with `{from, reply}`.
pub async fn ask(app: &AppState, caller: &SessionRow, c: &McpCall) -> Result<Answer, McpError> {
    let a: AskArgs = args(c.args.clone())?;
    if let Some(p) = db::request_by_id(&app.db, caller.id, "ask", &c.request_id)
        .await
        .map_err(internal)?
    {
        // A repeat (SPEC §5.2): still open, or its stored answer.
        return match (p.closed_at, p.result) {
            (None, _) => Ok(Answer::Later),
            (Some(_), Some(frame)) => {
                db::result_delivered(&app.db, p.id)
                    .await
                    .map_err(internal)?;
                let r: McpResult =
                    serde_json::from_value(frame["payload"].clone()).map_err(internal)?;
                r.outcome().map(Answer::Now)
            }
            (Some(_), None) => Err(err(error_code::UNAVAILABLE, "the call was cancelled")),
        };
    }
    let max = u64::from(app.config.timing.ask_timeout_s);
    let timeout = match a.timeout_s {
        Some(0) => {
            return Err(err(
                error_code::INVALID_ARGS,
                "timeout_s must be at least 1",
            ))
        }
        Some(t) => t.min(max),
        None => max,
    };
    let (t, to) = checked_target(app, caller, &a.text, &a.session).await?;
    let turn = call_chain(app, caller, c).await.map_err(internal)?;
    let chain = next_chain(turn, true)?;
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pending_requests
         WHERE kind = 'ask' AND session_id = $1 AND closed_at IS NULL",
    )
    .bind(caller.id)
    .fetch_one(&app.db)
    .await
    .map_err(internal)?;
    let max_open = app.config.limits.open_asks_per_session;
    if open >= max_open {
        return Err(err(
            error_code::LIMIT_EXCEEDED,
            format!("this session already waits for {open} replies; at most {max_open} at once"),
        ));
    }
    admit(app, caller, &chain).await?;
    let row = store_message(app, caller, &t, &c.request_id, &a.text, true, &chain).await?;
    let expires_at = Utc::now() + chrono::TimeDelta::seconds(timeout as i64);
    db::open_request(
        &app.db,
        "ask",
        caller.id,
        &c.request_id,
        &json!({"target_session_id": t.id, "event_id": row.event_id, "to": to}),
        Some(expires_at),
        caller.worker_id,
    )
    .await
    .map_err(internal)?;
    tracing::info!(from = %caller.id, to = %t.id, request_id = %c.request_id, timeout, "peer ask");
    // A request the target has open now shows in the caller too (SPEC §10.1).
    app.requests_changed(caller.owner_id);
    delivery::kick(app, t.id, false).await;
    Ok(Answer::Later)
}

// ---------------------------------------------------------------- the send buffer

/// Adds a message to the session's send buffer and drops the oldest beyond the limits.
async fn buffer(
    app: &AppState,
    t: &SessionRow,
    row: &EventRow,
    bytes: usize,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO peer_buffer (event_id, session_id, seq, bytes) VALUES ($1, $2, $3, $4)
         ON CONFLICT (event_id) DO NOTHING",
    )
    .bind(row.event_id)
    .bind(t.id)
    .bind(row.seq)
    .bind(i32::try_from(bytes).unwrap_or(i32::MAX))
    .execute(&app.db)
    .await?;
    let evicted = sqlx::query(
        "WITH b AS (
             SELECT event_id,
                    row_number() OVER (ORDER BY seq DESC) AS n,
                    sum(bytes) OVER (ORDER BY seq DESC) AS total
             FROM peer_buffer WHERE session_id = $1)
         DELETE FROM peer_buffer WHERE event_id IN
             (SELECT event_id FROM b WHERE n > $2 OR total > $3)",
    )
    .bind(t.id)
    .bind(MAX_BUFFER_MESSAGES)
    .bind(MAX_BUFFER_BYTES)
    .execute(&app.db)
    .await?
    .rows_affected();
    if evicted > 0 {
        tracing::info!(session_id = %t.id, evicted, "send buffer full, oldest messages dropped");
        events::system(
            app,
            t,
            PEER_EVICTED,
            &format!(
                "{evicted} message(s) from other sessions were dropped unread: the session keeps at most \
                 {MAX_BUFFER_MESSAGES} messages / {} KB until its next input",
                MAX_BUFFER_BYTES / 1024
            ),
        )
        .await?;
    }
    Ok(())
}

/// The buffered messages input `input` carries in `peer_prefix`: when it is sent for the
/// first time, those not carried by another input still pending, in `seq` order; when it is
/// sent again (a reconnect), exactly those it carried then and that are still buffered. They
/// stay in the buffer until its `ack`. Taking newer ones into a resent input would let its
/// `ack` clear them although the agent may have got the first frame, without them (T32).
pub async fn take_prefix(
    app: &AppState,
    session_id: Uuid,
    input: Uuid,
) -> anyhow::Result<Vec<PeerPrefix>> {
    let first =
        sqlx::query("INSERT INTO input_prefix (event_id) VALUES ($1) ON CONFLICT DO NOTHING")
            .bind(input)
            .execute(&app.db)
            .await?
            .rows_affected()
            == 1;
    let rows: Vec<(Value, DateTime<Utc>)> = if first {
        sqlx::query_as(
            "WITH c AS (
                 UPDATE peer_buffer b SET carried_by = $2
                 WHERE b.session_id = $1
                   AND NOT EXISTS (SELECT 1 FROM events e WHERE e.event_id = b.carried_by
                                     AND e.event_id <> $2 AND e.delivery = 'pending')
                 RETURNING b.event_id, b.seq)
             SELECT e.payload, e.ts FROM c JOIN events e ON e.event_id = c.event_id ORDER BY c.seq",
        )
        .bind(session_id)
        .bind(input)
        .fetch_all(&app.db)
        .await?
    } else {
        sqlx::query_as(
            "SELECT e.payload, e.ts FROM peer_buffer b JOIN events e ON e.event_id = b.event_id
             WHERE b.session_id = $1 AND b.carried_by = $2 ORDER BY b.seq",
        )
        .bind(session_id)
        .bind(input)
        .fetch_all(&app.db)
        .await?
    };
    Ok(rows
        .into_iter()
        .map(|(p, ts)| PeerPrefix {
            from: p["from"].as_str().unwrap_or("?").to_string(),
            text: p["text"].as_str().unwrap_or_default().to_string(),
            ts,
        })
        .collect())
}

/// Input `input` reached the agent: the messages it carried leave the buffer.
pub async fn delivered(app: &AppState, input: Uuid) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM peer_buffer WHERE carried_by = $1")
        .bind(input)
        .execute(&app.db)
        .await?;
    sqlx::query("DELETE FROM input_prefix WHERE event_id = $1")
        .bind(input)
        .execute(&app.db)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------- answers to asks

fn result_frame(r: McpResult) -> Value {
    json!({"type": "mcp.result", "payload": r})
}

/// Closes an open ask with `outcome` and sends it to the asking worker.
async fn answer(
    app: &AppState,
    p: PendingRow,
    outcome: Result<Value, McpError>,
) -> anyhow::Result<()> {
    let r = match outcome {
        Ok(v) => McpResult::ok(&p.request_id, v),
        Err(e) => McpResult::err(&p.request_id, &e.code, e.message),
    };
    let frame = result_frame(r);
    if db::close_request(&app.db, p.id, Some(&frame)).await? {
        requests::changed_in(app, p.session_id).await;
        requests::deliver_result(
            app,
            &PendingRow {
                result: Some(frame),
                ..p
            },
        )
        .await;
    }
    Ok(())
}

const ASK_COLS: &str =
    "id, kind, session_id, request_id, payload, expires_at, worker_id, result, closed_at";

/// A final `agent.message` of session `s` answering input `in_reply_to`: the reply to the
/// ask that delivered it, if one waits (SPEC §7.1).
pub async fn on_final(
    app: &AppState,
    s: &SessionRow,
    in_reply_to: Uuid,
    text: &str,
) -> anyhow::Result<()> {
    let open: Vec<PendingRow> = sqlx::query_as(&format!(
        "SELECT {ASK_COLS} FROM pending_requests
         WHERE kind = 'ask' AND closed_at IS NULL AND payload ->> 'event_id' = $1"
    ))
    .bind(in_reply_to.to_string())
    .fetch_all(&app.db)
    .await?;
    for p in open {
        if p.payload["target_session_id"].as_str() != Some(&s.id.to_string()) {
            continue;
        }
        let to = p.payload["to"].as_str().unwrap_or_default().to_string();
        tracing::info!(session_id = %p.session_id, request_id = %p.request_id, "peer ask answered");
        answer(app, p, Ok(json!({"from": to, "reply": text}))).await?;
    }
    Ok(())
}

/// The turn of session `s` on its last delivered input was lost (its process stopped, SPEC
/// §17.3): an ask that input carried gets no reply any more, so it fails now instead of at
/// its timeout. The input is not delivered again.
pub async fn turn_lost(app: &AppState, s: &SessionRow) -> anyhow::Result<()> {
    let open: Vec<PendingRow> = sqlx::query_as(&format!(
        "SELECT {ASK_COLS} FROM pending_requests
         WHERE kind = 'ask' AND closed_at IS NULL
           AND payload ->> 'target_session_id' = $1
           AND payload ->> 'event_id' = (SELECT event_id::text FROM events
                                         WHERE session_id = $2 AND delivery = 'delivered'
                                         ORDER BY seq DESC LIMIT 1)"
    ))
    .bind(s.id.to_string())
    .bind(s.id)
    .fetch_all(&app.db)
    .await?;
    for p in open {
        let to = p.payload["to"]
            .as_str()
            .unwrap_or("the session")
            .to_string();
        tracing::info!(session_id = %p.session_id, request_id = %p.request_id, target = %s.id, "peer ask failed: the target's turn was lost");
        answer(
            app,
            p,
            Err(err(
                error_code::UNAVAILABLE,
                format!("{to} lost its turn on the question (its agent stopped); ask again"),
            )),
        )
        .await?;
    }
    Ok(())
}

/// Asks past their `expires_at` get `timeout`; called every `expiry_interval`.
pub async fn expire(app: &AppState) -> anyhow::Result<()> {
    let rows: Vec<PendingRow> = sqlx::query_as(&format!(
        "SELECT {ASK_COLS} FROM pending_requests
         WHERE kind = 'ask' AND closed_at IS NULL AND expires_at <= now()"
    ))
    .fetch_all(&app.db)
    .await?;
    for p in rows {
        let to = p.payload["to"]
            .as_str()
            .unwrap_or("the session")
            .to_string();
        tracing::info!(session_id = %p.session_id, request_id = %p.request_id, "peer ask timed out");
        answer(
            app,
            p,
            Err(err(
                error_code::TIMEOUT,
                format!("no reply from {to} in time"),
            )),
        )
        .await?;
    }
    Ok(())
}

/// `mcp.cancel` from `worker_id`: an open ask of that call is closed and the asking
/// session gets `system ask_cancelled`; the target's turn goes on (SPEC §5.2). A fast call
/// has nothing open here.
pub async fn cancelled(app: &AppState, worker_id: Uuid, c: &McpCancel) -> anyhow::Result<()> {
    let open: Option<PendingRow> = sqlx::query_as(&format!(
        "SELECT {ASK_COLS} FROM pending_requests
         WHERE kind = 'ask' AND request_id = $1 AND worker_id = $2 AND closed_at IS NULL"
    ))
    .bind(&c.request_id)
    .bind(worker_id)
    .fetch_optional(&app.db)
    .await?;
    let Some(p) = open else { return Ok(()) };
    if !db::close_request(&app.db, p.id, None).await? {
        return Ok(());
    }
    let Some(s) = db::session(&app.db, p.session_id).await? else {
        return Ok(());
    };
    app.requests_changed(s.owner_id);
    let to = p.payload["to"].as_str().unwrap_or("another session");
    let why = if c.reason == cancel_reason::EOF {
        "the agent's hqc tools stopped"
    } else {
        "the agent stopped waiting"
    };
    events::system(
        app,
        &s,
        ASK_CANCELLED,
        &format!("The question to {to} was cancelled: {why}"),
    )
    .await?;
    Ok(())
}

/// The asking worker restarted: its blocking calls died with it, so its open asks are
/// closed with an error that is never sent (SPEC §7.1).
pub async fn worker_restarted(app: &AppState, worker_id: Uuid) -> anyhow::Result<()> {
    let rows: Vec<PendingRow> = sqlx::query_as(&format!(
        "SELECT {ASK_COLS} FROM pending_requests
         WHERE kind = 'ask' AND worker_id = $1 AND closed_at IS NULL"
    ))
    .bind(worker_id)
    .fetch_all(&app.db)
    .await?;
    for p in rows {
        let frame = result_frame(McpResult::err(
            &p.request_id,
            error_code::WORKER_RESTART,
            "the worker restarted while the call waited",
        ));
        if db::close_request(&app.db, p.id, Some(&frame)).await? {
            db::result_delivered(&app.db, p.id).await?;
            requests::changed_in(app, p.session_id).await;
        }
    }
    Ok(())
}

/// Answers to asks of `worker_id` it has not acknowledged: sent after `hello`, no
/// `session.start` needed (the worker routes `mcp.result` itself).
pub async fn after_hello(app: &AppState, worker_id: Uuid) -> anyhow::Result<()> {
    let rows: Vec<PendingRow> = sqlx::query_as(&format!(
        "SELECT {ASK_COLS} FROM pending_requests
         WHERE kind = 'ask' AND worker_id = $1 AND result IS NOT NULL
           AND result_delivered_at IS NULL
         ORDER BY closed_at"
    ))
    .bind(worker_id)
    .fetch_all(&app.db)
    .await?;
    for p in rows {
        requests::deliver_result(app, &p).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chains() {
        let first = next_chain(None, false).unwrap();
        assert_eq!((first.depth, first.hops), (0, 1));
        let ask = next_chain(None, true).unwrap();
        assert_eq!((ask.depth, ask.hops), (1, 1));
        let on = next_chain(Some(ask.clone()), true).unwrap();
        assert_eq!(
            (on.chain_id, on.depth, on.hops),
            (ask.chain_id, 2, 2),
            "the chain goes on"
        );
        let deep = Chain {
            depth: MAX_DEPTH,
            ..on.clone()
        };
        assert!(
            next_chain(Some(deep.clone()), false).is_ok(),
            "a send adds no depth"
        );
        assert_eq!(
            next_chain(Some(deep), true).unwrap_err().code,
            error_code::LIMIT_EXCEEDED
        );
        let long = Chain {
            hops: MAX_HOPS,
            ..on
        };
        assert_eq!(
            next_chain(Some(long), false).unwrap_err().code,
            error_code::LIMIT_EXCEEDED
        );
    }

    #[test]
    fn message_ids_are_per_caller_and_call() {
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        assert_eq!(message_id(a, "r1"), message_id(a, "r1"));
        assert_ne!(message_id(a, "r1"), message_id(a, "r2"));
        assert_ne!(message_id(a, "r1"), message_id(b, "r1"));
    }
}
