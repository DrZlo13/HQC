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
//! - `send` to a session with `wake_on_send` (the default since T52) and every `ask` go to the input queue
//!   (`delivery: pending`). Other sends wait in the send buffer (`peer_buffer`, migration
//!   0015) without `delivery`, and go to the agent in `peer_prefix` of the session's next
//!   input ([`take_prefix`]); that input's `ack` clears them ([`delivered`]). The buffer
//!   keeps the newest [`MAX_BUFFER_MESSAGES`] / [`MAX_BUFFER_BYTES`]; the older ones are
//!   dropped with a `system peer_evicted` in the target. A buffered message with an `ask`
//!   cap caps the turn of the input that carries it (T40).
//! - An ask is a `PendingRequest` of the asking session (`kind: ask`, `worker_id` of its
//!   worker, `payload {target_session_id, event_id, to}`), answered later: the target's
//!   final `agent.message` with `in_reply_to` = the message ([`on_final`]), the timeout
//!   ([`expire`]), `mcp.cancel` ([`cancelled`]) or a restart of the asking worker
//!   ([`worker_restarted`]). The answer is an `mcp.result` frame kept in `result` until the
//!   worker acks it, so an asking worker that was offline gets it after `hello`.
//! - Files (T41): `artifact_ids` of a call are the owner's artifacts linked to the calling
//!   session (uploaded by its worker from the agent's `files`, or given to it); the message
//!   keeps them and links them to the target, whose worker downloads them into its inbox. A
//!   reply carries the artifacts the target attached during its answering turn
//!   ([`reply_files`]), linked to the asking session, whose worker downloads them before it
//!   answers the agent.
//! - A send that comes back as a reply (T42): a send of the target to the asker from the turn
//!   on the question, while the ask is open, is kept on the ask ([`join_reply`]) instead of
//!   the asker's send buffer; the reply carries its text after the final message and its
//!   files after the turn's attachments ([`on_final`]). An ask that closes without a reply
//!   puts such messages into the send buffer after all.

use chrono::{DateTime, Utc};
use hqc_proto::{
    event::{ArtifactKind, Chain},
    frame::{McpCall, McpCancel, McpError, McpResult, PeerPrefix, PermissionMode},
    mcp::{cancel_reason, error_code, ReplyArtifact, MAX_FILES},
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    app::{AppState, Notice},
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
    /// The worker's upload of the agent's `files` (T41).
    #[serde(default)]
    artifact_ids: Vec<Uuid>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskArgs {
    text: String,
    session: String,
    timeout_s: Option<u64>,
    #[serde(default)]
    artifact_ids: Vec<Uuid>,
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

/// The files of a message (T41): at most [`MAX_FILES`], each an artifact of the caller's owner
/// linked to the calling session (uploaded for it, or given to it in an input or a reply);
/// anything else is `not_found`. A repeated id counts once.
async fn checked_files(
    app: &AppState,
    caller: &SessionRow,
    ids: &[Uuid],
) -> Result<Vec<Uuid>, McpError> {
    let mut out: Vec<Uuid> = Vec::new();
    for id in ids {
        if !out.contains(id) {
            out.push(*id);
        }
    }
    if out.len() > MAX_FILES {
        return Err(err(
            error_code::INVALID_ARGS,
            format!("at most {MAX_FILES} files per message"),
        ));
    }
    if out.is_empty() {
        return Ok(out);
    }
    let found: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT a.id) FROM artifacts a
         JOIN artifact_sessions s ON s.artifact_id = a.id
         WHERE a.id = ANY($1) AND a.owner_id = $2 AND s.session_id = $3",
    )
    .bind(&out)
    .bind(caller.owner_id)
    .bind(caller.id)
    .fetch_one(&app.db)
    .await
    .map_err(internal)?;
    if found as usize != out.len() {
        return Err(err(
            error_code::NOT_FOUND,
            "files: no such artifact of this session",
        ));
    }
    Ok(out)
}

/// Links artifacts `ids` to session `session_id`, so that its worker may download them
/// (SPEC §12).
async fn link_files(app: &AppState, session_id: Uuid, ids: &[Uuid]) -> anyhow::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO artifact_sessions (artifact_id, session_id)
         SELECT unnest($1::uuid[]), $2 ON CONFLICT DO NOTHING",
    )
    .bind(ids)
    .bind(session_id)
    .execute(&app.db)
    .await?;
    sqlx::query("UPDATE artifacts SET updated_at = now() WHERE id = ANY($1)")
        .bind(ids)
        .execute(&app.db)
        .await?;
    Ok(())
}

/// An input with its chain and its cap from the send buffer (`input_caps`).
type TurnInput = (Uuid, Option<Value>, Option<String>);

/// The input that started the session's current turn (SPEC §7.1): the last input delivered
/// to it. `None`: no turn yet.
async fn last_delivered(app: &AppState, session_id: Uuid) -> anyhow::Result<Option<TurnInput>> {
    Ok(sqlx::query_as(
        "SELECT e.event_id, e.chain, c.capped_by
         FROM events e LEFT JOIN input_caps c ON c.event_id = e.event_id
         WHERE e.session_id = $1 AND e.delivery = 'delivered'
         ORDER BY e.seq DESC LIMIT 1",
    )
    .bind(session_id)
    .fetch_optional(&app.db)
    .await?)
}

/// The chain of input `input` of a session: its own, or for a human's input one derived
/// from it; capped to `ask` by `buffered` when the send buffer the input carried had an
/// `ask` cap and its own chain has none (T40, [`take_prefix`]).
fn input_chain(
    session_id: Uuid,
    input: Uuid,
    chain: Option<Value>,
    buffered: Option<String>,
) -> Chain {
    let mut c = chain
        .and_then(|c| serde_json::from_value(c).ok())
        .unwrap_or(Chain {
            chain_id: crate::mcp::once_id("chain", session_id, &input.to_string()),
            depth: 0,
            hops: 0,
            permission_cap: None,
            capped_by: None,
        });
    if let Some(by) = buffered.filter(|_| c.permission_cap != Some(PermissionMode::Ask)) {
        c.permission_cap = Some(PermissionMode::Ask);
        c.capped_by = Some(by);
    }
    c
}

/// The chain of call `c` of session `caller`: that of the input the worker says the turn
/// runs on (`turn_input`), if it is an input of the session given to the agent or on its
/// way, not older than the last delivered one; otherwise the last delivered input. A human's
/// turn is a chain of its own with nothing in it yet (`depth` and `hops` 0), its id derived
/// from the input, so every call of that turn shares it. The worker knows
/// the turn before the hub does: the input's `ack` bypasses its outbox and is lost with a
/// dropped connection, and until the resent input is acked again the hub's last delivered
/// input is the previous turn's (SPEC §7.1). It cannot pick an older turn's chain.
pub(crate) async fn call_chain(
    app: &AppState,
    caller: &SessionRow,
    c: &McpCall,
) -> anyhow::Result<Option<Chain>> {
    Ok(call_input(app, caller, c)
        .await?
        .map(|(input, chain, buffered)| input_chain(caller.id, input, chain, buffered)))
}

/// The input the turn of call `c` runs on, by the rules of [`call_chain`].
async fn call_input(
    app: &AppState,
    caller: &SessionRow,
    c: &McpCall,
) -> anyhow::Result<Option<TurnInput>> {
    if let Some(input) = c.turn_input {
        let row: Option<(Option<Value>, Option<String>)> = sqlx::query_as(
            "SELECT e.chain, c.capped_by
             FROM events e LEFT JOIN input_caps c ON c.event_id = e.event_id
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
        if let Some((chain, buffered)) = row {
            return Ok(Some((input, chain, buffered)));
        }
        tracing::debug!(session_id = %caller.id, %input, "turn_input is not the session's current input; the hub's last delivered one counts");
    }
    last_delivered(app, caller.id).await
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

/// The chain of a new message, spawn or schedule of session `caller` (SPEC §7.1): its
/// call's chain moved on ([`next_chain`]) with the caller's mode for the cap (T39).
pub(crate) async fn outgoing_chain(
    app: &AppState,
    caller: &SessionRow,
    c: &McpCall,
    ask: bool,
) -> Result<Chain, McpError> {
    let turn = call_chain(app, caller, c).await.map_err(internal)?;
    let from = address(app, caller).await.map_err(internal)?;
    next_chain(turn, ask, sender_mode(caller), &from)
}

/// The mode of session `s` for a cap: the lower of what the user asked for (T38) and what its
/// worker last reported, so a switch to `ask` counts at once, before the worker confirms it;
/// `ask` before any report: what the hub does not know cannot lift a cap (T39).
pub fn sender_mode(s: &SessionRow) -> PermissionMode {
    if s.permission_mode.as_deref().and_then(PermissionMode::parse) == Some(PermissionMode::Ask) {
        return PermissionMode::Ask;
    }
    s.effective_permission_mode
        .as_deref()
        .and_then(PermissionMode::parse)
        .unwrap_or(PermissionMode::Ask)
}

/// The cap of a message sent in `mode` by `from` from a turn with chain `turn` (T39, SPEC
/// §7.1): `auto` only if the sender and every session up the chain ran in `auto`, i.e. the
/// lower of `mode` and the cap of the sender's turn; a human's turn has no cap. With `ask`,
/// the session that set it: the first one up the chain in `ask`.
pub fn chain_cap(
    turn: Option<&Chain>,
    mode: PermissionMode,
    from: &str,
) -> (PermissionMode, Option<String>) {
    match turn.and_then(|t| Some((t.permission_cap?, t.capped_by.clone()))) {
        Some((PermissionMode::Ask, by)) => (
            PermissionMode::Ask,
            Some(by.unwrap_or_else(|| from.to_string())),
        ),
        _ if mode == PermissionMode::Ask => (PermissionMode::Ask, Some(from.to_string())),
        _ => (PermissionMode::Auto, None),
    }
}

/// The chain of a new message from a turn with chain `turn`, sent by `from` running in
/// `mode`: a human's turn starts one, `hops` grows on every message and `depth` on every
/// ask; over a limit is an error, which only a human's turn gets past (SPEC §7.1). The cap
/// is [`chain_cap`].
pub fn next_chain(
    turn: Option<Chain>,
    ask: bool,
    mode: PermissionMode,
    from: &str,
) -> Result<Chain, McpError> {
    let (permission_cap, capped_by) = chain_cap(turn.as_ref(), mode, from);
    let c = match turn {
        None => Chain {
            chain_id: Uuid::now_v7(),
            depth: u8::from(ask),
            hops: 1,
            permission_cap: Some(permission_cap),
            capped_by,
        },
        Some(c) => Chain {
            chain_id: c.chain_id,
            depth: c.depth.saturating_add(u8::from(ask)),
            hops: c.hops.saturating_add(1),
            permission_cap: Some(permission_cap),
            capped_by,
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

/// What a `peer.message` says.
#[derive(Clone, Copy)]
struct Message<'a> {
    text: &'a str,
    files: &'a [Uuid],
    ask: bool,
    /// A send that goes with the reply to an ask of the target (T42): never queued.
    with_reply: bool,
}

/// Stores the `peer.message` of a call, or finds the one stored by an earlier copy of it.
/// An ask, and a send to a session that wakes on them, go to the input queue (`pending`);
/// other sends have no `delivery` (the send buffer, or the reply they go with).
async fn store_message(
    app: &AppState,
    caller: &SessionRow,
    t: &SessionRow,
    request_id: &str,
    m: Message<'_>,
    chain: &Chain,
) -> Result<EventRow, McpError> {
    let Message {
        text,
        files,
        ask,
        with_reply,
    } = m;
    let delivery = (ask || (t.wake_on_send && !with_reply)).then_some("pending");
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
    if !files.is_empty() {
        payload["artifact_ids"] = json!(files);
    }
    // Before the message exists: once it does, its input may reach the target's worker,
    // which downloads the files at once.
    link_files(app, t.id, files).await.map_err(internal)?;
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

/// `hqc_send(text, session, artifact_ids?)`: `{to, delivery: queued | buffered |
/// with_reply}`. A send from the turn that answers an open ask of the target goes with the
/// reply ([`join_reply`]).
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
        let how = if row.delivery.is_some() {
            QUEUED
        } else if joined(app, t.id, id).await.map_err(internal)? {
            WITH_REPLY
        } else {
            BUFFERED
        };
        return Ok(Answer::Now(send_result(&to, how, row.seq)));
    }
    let (t, to) = checked_target(app, caller, &a.text, &a.session).await?;
    let files = checked_files(app, caller, &a.artifact_ids).await?;
    let chain = outgoing_chain(app, caller, c, false).await?;
    admit(app, caller, &chain).await?;
    let m = Message {
        text: &a.text,
        files: &files,
        ask: false,
        with_reply: true,
    };
    if let Some((input, _, _)) = call_input(app, caller, c).await.map_err(internal)? {
        if let Some(seq) = join_reply(app, caller, &t, input, &c.request_id, m, &chain).await? {
            tracing::info!(from = %caller.id, to = %t.id, "peer message goes with the reply");
            return Ok(Answer::Now(send_result(&to, WITH_REPLY, seq)));
        }
    }
    let wake = t.wake_on_send;
    let m = Message {
        with_reply: false,
        ..m
    };
    let row = store_message(app, caller, &t, &c.request_id, m, &chain).await?;
    if wake {
        delivery::kick(app, t.id, false).await;
    } else {
        buffer(app, &t, &row, a.text.len())
            .await
            .map_err(internal)?;
    }
    tracing::info!(from = %caller.id, to = %t.id, wake, "peer message sent");
    Ok(Answer::Now(send_result(
        &to,
        if wake { QUEUED } else { BUFFERED },
        row.seq,
    )))
}

/// `delivery` of an `hqc_send` result (SPEC §7).
const QUEUED: &str = "queued";
const BUFFERED: &str = "buffered";
const WITH_REPLY: &str = "with_reply";

/// `message_seq`: the `seq` of the `peer.message` in the session it went to (T51), so the
/// sender's client can show when it was delivered.
fn send_result(to: &str, delivery: &str, message_seq: i64) -> Value {
    json!({"to": to, "delivery": delivery, "message_seq": message_seq})
}

/// A send of `caller` to `t` from the turn on input `input` (T42, SPEC §7.1): if that input
/// is the question of an ask of `t` still open, the message is stored in `t` without
/// `delivery`, kept on the ask (`payload.joined`) instead of the send buffer, and goes with
/// the reply ([`on_final`]). The ask's row stays locked from the check until the message is
/// on it, so the reply cannot close the ask in between and leave the message behind. The
/// stored message's `seq`; `None`: no such ask, nothing stored.
async fn join_reply(
    app: &AppState,
    caller: &SessionRow,
    t: &SessionRow,
    input: Uuid,
    request_id: &str,
    m: Message<'_>,
    chain: &Chain,
) -> Result<Option<i64>, McpError> {
    let mut tx = app.db.begin().await.map_err(internal)?;
    let ask: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM pending_requests
         WHERE kind = 'ask' AND closed_at IS NULL AND session_id = $1
           AND payload ->> 'target_session_id' = $2 AND payload ->> 'event_id' = $3
         FOR UPDATE",
    )
    .bind(t.id)
    .bind(caller.id.to_string())
    .bind(input.to_string())
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?;
    let Some(ask) = ask else {
        return Ok(None);
    };
    let row = store_message(app, caller, t, request_id, m, chain).await?;
    sqlx::query(
        "UPDATE pending_requests
         SET payload = jsonb_set(payload, '{joined}',
                                 COALESCE(payload -> 'joined', '[]'::jsonb) || to_jsonb($2::text)),
             updated_at = now()
         WHERE id = $1",
    )
    .bind(ask)
    .bind(row.event_id.to_string())
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(Some(row.seq))
}

/// Whether message `event_id` went with the reply to an ask of session `asker`.
async fn joined(app: &AppState, asker: Uuid, event_id: Uuid) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pending_requests
                        WHERE kind = 'ask' AND session_id = $1 AND payload -> 'joined' ? $2)",
    )
    .bind(asker)
    .bind(event_id.to_string())
    .fetch_one(&app.db)
    .await?)
}

/// The messages joined to an ask (`payload.joined`).
fn joined_of(payload: &Value) -> Vec<Uuid> {
    payload
        .get("joined")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// `hqc_ask(text, session, timeout_s?, artifact_ids?)`: answered later with `{from, reply,
/// files?}`.
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
                result_acked(app, p.id).await.map_err(internal)?;
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
    let files = checked_files(app, caller, &a.artifact_ids).await?;
    let chain = outgoing_chain(app, caller, c, true).await?;
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
    let m = Message {
        text: &a.text,
        files: &files,
        ask: true,
        with_reply: false,
    };
    let row = store_message(app, caller, &t, &c.request_id, m, &chain).await?;
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
///
/// With them the input's turn takes the lowest cap (T40, SPEC §7.1): when one of them has
/// an `ask` cap, so does the input, named by the first such message's `capped_by`; it is
/// kept in `input_caps` for the header and for what the turn sends ([`input_chain`]).
pub async fn take_prefix(app: &AppState, session_id: Uuid, input: Uuid) -> anyhow::Result<Prefix> {
    let first =
        sqlx::query("INSERT INTO input_prefix (event_id) VALUES ($1) ON CONFLICT DO NOTHING")
            .bind(input)
            .execute(&app.db)
            .await?
            .rows_affected()
            == 1;
    let rows: Vec<(Uuid, Value, DateTime<Utc>, Option<Value>)> = if first {
        sqlx::query_as(
            "WITH c AS (
                 UPDATE peer_buffer b SET carried_by = $2
                 WHERE b.session_id = $1
                   AND NOT EXISTS (SELECT 1 FROM events e WHERE e.event_id = b.carried_by
                                     AND e.event_id <> $2 AND e.delivery = 'pending')
                 RETURNING b.event_id, b.seq)
             SELECT e.event_id, e.payload, e.ts, e.chain FROM c JOIN events e ON e.event_id = c.event_id
             ORDER BY c.seq",
        )
        .bind(session_id)
        .bind(input)
        .fetch_all(&app.db)
        .await?
    } else {
        sqlx::query_as(
            "SELECT e.event_id, e.payload, e.ts, e.chain FROM peer_buffer b JOIN events e ON e.event_id = b.event_id
             WHERE b.session_id = $1 AND b.carried_by = $2 ORDER BY b.seq",
        )
        .bind(session_id)
        .bind(input)
        .fetch_all(&app.db)
        .await?
    };
    let from = |p: &Value| p["from"].as_str().unwrap_or("?").to_string();
    let capped_by = rows.iter().find_map(|(_, p, _, chain)| {
        let c: Chain = serde_json::from_value(chain.clone()?).ok()?;
        (c.permission_cap? == PermissionMode::Ask).then(|| c.capped_by.unwrap_or_else(|| from(p)))
    });
    if let Some(by) = &capped_by {
        sqlx::query(
            "INSERT INTO input_caps (event_id, capped_by) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(input)
        .bind(by)
        .execute(&app.db)
        .await?;
    }
    Ok(Prefix {
        messages: rows
            .iter()
            .map(|(id, p, ts, _)| PeerPrefix {
                from: from(p),
                text: p["text"].as_str().unwrap_or_default().to_string(),
                ts: *ts,
                event_id: Some(*id),
                artifact_ids: p
                    .get("artifact_ids")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default(),
            })
            .collect(),
        capped_by,
    })
}

/// What [`take_prefix`] gives an input.
pub struct Prefix {
    /// Its `peer_prefix`.
    pub messages: Vec<PeerPrefix>,
    /// With an `ask` cap among them: the session that set the first one.
    pub capped_by: Option<String>,
}

/// Input `input` reached the agent: the messages it carried leave the buffer. They keep no
/// `delivery`, but get `delivered_at` (T51), which the sender's client shows.
pub async fn delivered(app: &AppState, input: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE events SET delivered_at = now()
         WHERE event_id IN (SELECT event_id FROM peer_buffer WHERE carried_by = $1)
           AND delivered_at IS NULL",
    )
    .bind(input)
    .execute(&app.db)
    .await?;
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

/// The worker acknowledged the result of request `id` (a cancellation or rejection is not an
/// acknowledgement: those call [`db::result_delivered`]). For the reply to an ask this is the
/// time the reply reached the asker: the first one is kept on the ask
/// (`payload.reply_delivered_at`), and the target's subscribers get the reply's event again
/// with `reply_delivered_at` (SPEC §6).
pub async fn result_acked(app: &AppState, id: Uuid) -> anyhow::Result<()> {
    let first: Option<(Uuid, DateTime<Utc>)> = sqlx::query_as(
        "UPDATE pending_requests
         SET result_delivered_at = now(),
             payload = payload || jsonb_build_object('reply_delivered_at', now())
         WHERE id = $1 AND kind = 'ask' AND payload ? 'reply_event_id'
           AND NOT payload ? 'reply_delivered_at'
         RETURNING (payload ->> 'reply_event_id')::uuid,
                   (payload ->> 'reply_delivered_at')::timestamptz",
    )
    .bind(id)
    .fetch_optional(&app.db)
    .await?;
    let Some((reply, at)) = first else {
        return Ok(db::result_delivered(&app.db, id).await?);
    };
    if let Some(row) = db::event_by_id(&app.db, reply).await? {
        let mut event = row.to_json();
        event["reply_delivered_at"] = json!(at);
        app.notify(Notice::Resent {
            session_id: row.session_id,
            seq: row.seq,
            event,
        });
    }
    Ok(())
}

/// Closes an open ask without a reply (`error`) and sends that to the asking worker; the
/// messages joined to it go to the asker's send buffer after all ([`rebuffer`]).
async fn fail(app: &AppState, p: PendingRow, error: McpError) -> anyhow::Result<()> {
    let frame = result_frame(McpResult::err(&p.request_id, &error.code, error.message));
    let Some(joined) = close_ask(app, p.id, Some(&frame)).await? else {
        return Ok(());
    };
    rebuffer(app, p.session_id, &joined).await?;
    sent(app, p, frame).await;
    Ok(())
}

/// The asking worker gets the closed ask's answer `frame`.
async fn sent(app: &AppState, p: PendingRow, frame: Value) {
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

/// Closes ask `id` with `result` unless it is closed already (`None`); the messages joined
/// to it. The update waits for a [`join_reply`] holding the row and then sees its message.
async fn close_ask(
    app: &AppState,
    id: Uuid,
    result: Option<&Value>,
) -> anyhow::Result<Option<Vec<Uuid>>> {
    let payload: Option<Value> = sqlx::query_scalar(
        "UPDATE pending_requests SET closed_at = now(), result = $2, updated_at = now()
         WHERE id = $1 AND closed_at IS NULL RETURNING payload",
    )
    .bind(id)
    .bind(result)
    .fetch_optional(&app.db)
    .await?;
    Ok(payload.as_ref().map(joined_of))
}

/// Messages joined to an ask that closed without a reply (T42): they wait in the asker's send
/// buffer like any send, so nothing the target sent is lost.
async fn rebuffer(app: &AppState, asker: Uuid, joined: &[Uuid]) -> anyhow::Result<()> {
    if joined.is_empty() {
        return Ok(());
    }
    let Some(s) = db::session(&app.db, asker).await? else {
        return Ok(());
    };
    for id in joined {
        if let Some(row) = db::event_by_id(&app.db, *id).await? {
            let bytes = row.payload["text"].as_str().map_or(0, str::len);
            buffer(app, &s, &row, bytes).await?;
        }
    }
    tracing::info!(session_id = %asker, messages = joined.len(), "an ask closed without a reply; its joined messages are buffered");
    Ok(())
}

const ASK_COLS: &str =
    "id, kind, session_id, request_id, payload, expires_at, worker_id, result, closed_at";

/// Separates the final message of a reply from a message the target sent with `hqc_send`
/// during the answering turn (T42), and two such messages.
pub const JOINED_SEPARATOR: &str = "\n\n--- sent with hqc_send while answering ---\n";

/// A final `agent.message` of session `s` (at `seq`) answering input `in_reply_to`: the
/// reply to the ask that delivered it, if one waits (SPEC §7.1), with the files of the
/// answering turn ([`reply_files`]) and the messages the turn sent to the asker (T42), their
/// texts after `text`. The ask's row is locked from reading its joined messages until it is
/// closed, so a [`join_reply`] either comes before and is in the reply, or finds it closed.
///
/// The ask keeps the final message's id (`payload.reply_event_id`) for the time its result is
/// acknowledged ([`result_acked`]), and the result has the question's `seq` in the target
/// (`message_seq`, T51).
pub async fn on_final(
    app: &AppState,
    s: &SessionRow,
    in_reply_to: Uuid,
    text: &str,
    reply: (Uuid, i64),
) -> anyhow::Result<()> {
    let (reply_id, seq) = reply;
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
        let mut tx = app.db.begin().await?;
        let payload: Option<Value> = sqlx::query_scalar(
            "SELECT payload FROM pending_requests WHERE id = $1 AND closed_at IS NULL FOR UPDATE",
        )
        .bind(p.id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(payload) = payload else { continue };
        let joined: Vec<(Value,)> =
            sqlx::query_as("SELECT payload FROM events WHERE event_id = ANY($1) ORDER BY seq")
                .bind(joined_of(&payload))
                .fetch_all(&app.db)
                .await?;
        let mut reply = text.to_string();
        let mut sent_files: Vec<Uuid> = Vec::new();
        for (m,) in &joined {
            reply.push_str(JOINED_SEPARATOR);
            reply.push_str(m["text"].as_str().unwrap_or_default());
            if let Some(ids) = m.get("artifact_ids") {
                sent_files
                    .extend(serde_json::from_value::<Vec<Uuid>>(ids.clone()).unwrap_or_default());
            }
        }
        let to = p.payload["to"].as_str().unwrap_or_default().to_string();
        let files = reply_files(app, s, p.session_id, in_reply_to, seq, &sent_files).await?;
        tracing::info!(session_id = %p.session_id, request_id = %p.request_id, files = files.len(), joined = joined.len(), "peer ask answered");
        let mut result = json!({"from": to, "reply": reply});
        if !files.is_empty() {
            result["files"] = serde_json::to_value(&files)?;
        }
        if let Some(q) = db::event_by_id(&app.db, in_reply_to).await? {
            result["message_seq"] = json!(q.seq);
        }
        let frame = result_frame(McpResult::ok(&p.request_id, result));
        sqlx::query(
            "UPDATE pending_requests SET closed_at = now(), result = $2, updated_at = now(),
                    payload = payload || jsonb_build_object('reply_event_id', $3::text)
             WHERE id = $1",
        )
        .bind(p.id)
        .bind(&frame)
        .bind(reply_id.to_string())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        sent(app, p, frame).await;
    }
    Ok(())
}

/// The files of a reply (T41, SPEC §7.1): the artifacts of the `attachment` events of
/// session `s` between the question `question` and the final message at `seq`, oldest first,
/// once each, at most [`MAX_FILES`]. Only artifacts of the session's owner count: a worker
/// may write `attachment` events of its own, and the id of another owner's artifact in one
/// must not reach the asking worker. Then `sent`, the files of the messages the turn sent to
/// the asker (T42), in order. They are linked to the asking session `asker`, so its worker may
/// download them.
pub async fn reply_files(
    app: &AppState,
    s: &SessionRow,
    asker: Uuid,
    question: Uuid,
    seq: i64,
    sent: &[Uuid],
) -> anyhow::Result<Vec<ReplyArtifact>> {
    let mut rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT a.id, a.name, a.kind FROM events e
         JOIN artifacts a ON a.id::text = e.payload ->> 'artifact_id'
         WHERE e.session_id = $1 AND e.type = 'attachment' AND a.owner_id = $2
           AND e.seq > (SELECT q.seq FROM events q WHERE q.event_id = $3 AND q.session_id = $1)
           AND e.seq < $4
         ORDER BY e.seq",
    )
    .bind(s.id)
    .bind(s.owner_id)
    .bind(question)
    .bind(seq)
    .fetch_all(&app.db)
    .await?;
    if !sent.is_empty() {
        // Checked by `hqc_send` already (the owner's, linked to the target); the owner again
        // for the same reason as above.
        let found: Vec<(Uuid, String, String)> = sqlx::query_as(
            "SELECT id, name, kind FROM artifacts WHERE id = ANY($1) AND owner_id = $2",
        )
        .bind(sent)
        .bind(s.owner_id)
        .fetch_all(&app.db)
        .await?;
        rows.extend(
            sent.iter()
                .filter_map(|id| found.iter().find(|f| f.0 == *id).cloned()),
        );
    }
    let mut files: Vec<ReplyArtifact> = Vec::new();
    for (id, name, kind) in rows {
        if files.iter().any(|f| f.artifact_id == id) {
            continue;
        }
        if files.len() == MAX_FILES {
            tracing::warn!(session_id = %s.id, %question, "the files of a reply beyond {MAX_FILES} are left out");
            break;
        }
        files.push(ReplyArtifact {
            artifact_id: id,
            name,
            kind: serde_json::from_value(Value::String(kind)).unwrap_or(ArtifactKind::File),
        });
    }
    let ids: Vec<Uuid> = files.iter().map(|f| f.artifact_id).collect();
    link_files(app, asker, &ids).await?;
    Ok(files)
}

/// The turn of session `s` on its last delivered input was lost (its process stopped, SPEC
/// §17.3): an ask that input carried gets no reply any more, so it fails now instead of at
/// its timeout. The input is not delivered again.
pub async fn turn_lost(app: &AppState, s: &SessionRow) -> anyhow::Result<()> {
    fail_turn(app, s, None, "lost", |to| {
        format!("{to} lost its turn on the question (its agent stopped); ask again")
    })
    .await
}

/// The turn of session `s` on input `input` ended with `status idle` (SPEC §7.1). A final
/// message of that turn answered its ask already ([`on_final`]); one still open got none (the
/// user's "No" to a permission request, `POST /stop`, a turn that ended without text) and
/// fails now instead of at its timeout. The input comes from the status (`in_reply_to`), not
/// from the session's last delivered input: the worker acks the next input outside its outbox,
/// so that ack may come before this status (SPEC §5.2).
pub async fn turn_stopped(app: &AppState, s: &SessionRow, input: Uuid) -> anyhow::Result<()> {
    fail_turn(app, s, Some(input), "stopped", |to| {
        format!("the turn of {to} on the question ended without an answer")
    })
    .await
}

/// Fails the open asks that input `input` of session `s` carried (`None`: its last delivered
/// input), with `unavailable` and `message(to)`.
async fn fail_turn(
    app: &AppState,
    s: &SessionRow,
    input: Option<Uuid>,
    why: &str,
    message: impl Fn(&str) -> String,
) -> anyhow::Result<()> {
    let open: Vec<PendingRow> = sqlx::query_as(&format!(
        "SELECT {ASK_COLS} FROM pending_requests
         WHERE kind = 'ask' AND closed_at IS NULL
           AND payload ->> 'target_session_id' = $1
           AND payload ->> 'event_id' = COALESCE($3::text,
                                                 (SELECT event_id::text FROM events
                                                  WHERE session_id = $2 AND delivery = 'delivered'
                                                  ORDER BY seq DESC LIMIT 1))"
    ))
    .bind(s.id.to_string())
    .bind(s.id)
    .bind(input.map(|i| i.to_string()))
    .fetch_all(&app.db)
    .await?;
    for p in open {
        let to = p.payload["to"]
            .as_str()
            .unwrap_or("the session")
            .to_string();
        tracing::info!(session_id = %p.session_id, request_id = %p.request_id, target = %s.id, why, "peer ask failed: the target's turn ended without a reply");
        fail(app, p, err(error_code::UNAVAILABLE, message(&to))).await?;
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
        fail(
            app,
            p,
            err(error_code::TIMEOUT, format!("no reply from {to} in time")),
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
    let Some(joined) = close_ask(app, p.id, None).await? else {
        return Ok(());
    };
    rebuffer(app, p.session_id, &joined).await?;
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
        if let Some(joined) = close_ask(app, p.id, Some(&frame)).await? {
            rebuffer(app, p.session_id, &joined).await?;
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
        let next = |turn, ask| next_chain(turn, ask, PermissionMode::Auto, "p/a");
        let first = next(None, false).unwrap();
        assert_eq!((first.depth, first.hops), (0, 1));
        let ask = next(None, true).unwrap();
        assert_eq!((ask.depth, ask.hops), (1, 1));
        let on = next(Some(ask.clone()), true).unwrap();
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
            next(Some(deep.clone()), false).is_ok(),
            "a send adds no depth"
        );
        assert_eq!(
            next(Some(deep), true).unwrap_err().code,
            error_code::LIMIT_EXCEEDED
        );
        let long = Chain {
            hops: MAX_HOPS,
            ..on
        };
        assert_eq!(
            next(Some(long), false).unwrap_err().code,
            error_code::LIMIT_EXCEEDED
        );
    }

    // T39: `auto` only if the sender and every session up the chain run in `auto`.
    #[test]
    fn the_cap_is_the_lowest_mode_up_the_chain() {
        use PermissionMode::{Ask, Auto};
        let cap = |c: &Chain| (c.permission_cap, c.capped_by.clone());
        // A human's turn has no cap: the sender's mode decides.
        let human = input_chain(Uuid::now_v7(), Uuid::now_v7(), None, None);
        assert_eq!(human.permission_cap, None);
        let a = next_chain(Some(human.clone()), true, Auto, "p/a").unwrap();
        assert_eq!(cap(&a), (Some(Auto), None));
        let a = next_chain(Some(human), true, Ask, "p/a").unwrap();
        assert_eq!(cap(&a), (Some(Ask), Some("p/a".into())));
        // Along the chain: Auto -> Ask -> Auto stays Ask, named by the first one in Ask.
        let ab = next_chain(None, true, Auto, "p/a").unwrap();
        let bc = next_chain(Some(ab), true, Ask, "p/b").unwrap();
        assert_eq!(cap(&bc), (Some(Ask), Some("p/b".into())));
        let cd = next_chain(Some(bc), true, Auto, "p/c").unwrap();
        assert_eq!(cap(&cd), (Some(Ask), Some("p/b".into())));
        let de = next_chain(Some(cd), false, Ask, "p/d").unwrap();
        assert_eq!(
            cap(&de),
            (Some(Ask), Some("p/b".into())),
            "the first Ask names it"
        );
        // A chain from before 0.43 carries no cap: the sender's mode decides.
        let old: Chain =
            serde_json::from_value(json!({"chain_id": Uuid::now_v7(), "depth": 1, "hops": 1}))
                .unwrap();
        assert_eq!(
            cap(&next_chain(Some(old), false, Auto, "p/a").unwrap()),
            (Some(Auto), None)
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
