//! Open `approval` / `clarify` requests (SPEC §3 PendingRequest, §10.1).
//!
//! An answer closes the request (first answer wins), is stored as an event, and its frame
//! is kept in `pending_requests.result` until the worker acknowledges it, so an answer given
//! while the worker is offline reaches it after `hello`.
//!
//! A cancellation by the hub (timeout, stop, archive, worker restart) writes
//! `approval.cancel` and sends `approval.response` with the `canonical: deny` option; for
//! clarify it writes `clarify.cancel` and sends `clarify.response` with empty `answers`
//! (SPEC §5.3).
//!
//! A worker's own request (`worker.approval.request`, kind `worker_approval`, SPEC §10.2)
//! lives in its service session: `approval.request` with the options `once` / `deny`, the
//! answer or the hub's cancellation goes back as `worker.approval.response`.
//!
//! An agent's `hqc_move` that needs the owner (kind `move`, SPEC §7, §10.1) lives in the
//! calling session: `approval.request` with the options `once` / `deny`; the session owner
//! answers it, the call gets the outcome as its `mcp.result` ([`crate::moves`]).
//!
//! A move after the transcript into another environment (kind `follow`, SPEC §9.2) is asked
//! the same way and answered by the session owner; no call waits for it, so no frame goes
//! anywhere, and an allowed one moves the session at once ([`crate::mirror::allowed`]).

use std::collections::{BTreeMap, HashMap};

use chrono::Utc;
use hqc_proto::{
    api::{OpenRequest, WaitingSession},
    event::{ApprovalOption, CancelReason, ClarifyAnswer},
    frame::{worker_approval, WorkerApprovalRequest},
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::CurrentUser,
    db::{self, PendingRow, SessionRow},
    error::{ApiError, ApiResult},
    events::{self, user_author},
    worker_ws::{Reply, StartState},
};

#[derive(Debug, Deserialize)]
pub struct AnswerBody {
    pub option_id: Option<String>,
    pub answers: Option<BTreeMap<String, ClarifyAnswer>>,
    /// Text instead of the call (T26): only with a deny option of an in-session approval.
    pub message: Option<String>,
}

/// Longest `message` of an approval answer, in bytes.
pub const MAX_ANSWER_MESSAGE: usize = 16 * 1024;

/// The `message` of an approval answer, checked: only with a `deny`/`deny_always` option,
/// not blank, not over [`MAX_ANSWER_MESSAGE`]; NUL replaced (SPEC §4).
fn answer_message(
    message: Option<String>,
    option: &ApprovalOption,
) -> Result<Option<String>, ApiError> {
    use hqc_proto::event::ApprovalCanonical::{Deny, DenyAlways};
    let Some(m) = message else { return Ok(None) };
    if !matches!(option.canonical, Deny | DenyAlways) {
        return Err(ApiError::BadRequest(
            "message goes only with a deny option".into(),
        ));
    }
    if m.trim().is_empty() {
        return Err(ApiError::BadRequest("message must not be blank".into()));
    }
    if m.len() > MAX_ANSWER_MESSAGE {
        return Err(ApiError::BadRequest(format!(
            "message is over {MAX_ANSWER_MESSAGE} bytes"
        )));
    }
    Ok(Some(hqc_proto::replace_nul(&m).into_owned()))
}

fn options(payload: &Value) -> Vec<ApprovalOption> {
    payload
        .get("options")
        .and_then(|o| serde_json::from_value(o.clone()).ok())
        .unwrap_or_default()
}

/// The option that closes the request in the runtime when the hub cancels it.
fn deny_option(payload: &Value) -> Option<String> {
    use hqc_proto::event::ApprovalCanonical::{Deny, DenyAlways};
    let opts = options(payload);
    opts.iter()
        .find(|o| o.canonical == Deny)
        .or_else(|| opts.iter().find(|o| o.canonical == DenyAlways))
        .map(|o| o.id.clone())
}

/// `POST /api/sessions/{id}/requests/{request_id}`.
pub async fn answer(
    app: &AppState,
    user: &CurrentUser,
    s: &SessionRow,
    request_id: &str,
    body: AnswerBody,
) -> ApiResult<EventRowOut> {
    let Some(p) = db::find_open_request(&app.db, s.id, request_id).await? else {
        return Err(if db::request_exists(&app.db, s.id, request_id).await? {
            ApiError::Conflict("request_closed", "the request is already closed".into())
        } else {
            ApiError::NotFound
        });
    };
    let (frame, ev_type, ev_payload) = match p.kind.as_str() {
        "approval" => {
            // Approvals go to the worker owner only (SPEC §10.1, §11).
            let owner = match s.worker_id {
                Some(w) => db::worker(&app.db, w).await?.map(|w| w.owner_id),
                None => None,
            };
            if owner != Some(user.id) {
                return Err(ApiError::Forbidden(
                    "only the worker owner answers approvals".into(),
                ));
            }
            let option_id = body
                .option_id
                .ok_or_else(|| ApiError::BadRequest("option_id is required".into()))?;
            let Some(option) = options(&p.payload).into_iter().find(|o| o.id == option_id) else {
                return Err(ApiError::BadRequest(format!(
                    "unknown option `{option_id}`"
                )));
            };
            let mut frame =
                json!({"session_id": s.id, "request_id": request_id, "option_id": option_id});
            let mut event = json!({"request_id": request_id, "option_id": option_id, "by": user.id.to_string()});
            if let Some(m) = answer_message(body.message, &option)? {
                frame["message"] = json!(m);
                event["message"] = json!(m);
            }
            (
                Some(json!({"type": "approval.response", "payload": frame})),
                "approval.response",
                event,
            )
        }
        "move" | crate::mirror::FOLLOW => {
            if body.message.is_some() {
                return Err(ApiError::BadRequest(
                    "a move request takes no message".into(),
                ));
            }
            // Only the session owner, whose session moves (SPEC §7, §9.2, §10.1).
            if s.owner_id != user.id {
                return Err(ApiError::Forbidden(
                    "only the session owner answers a move of it".into(),
                ));
            }
            let option_id = body
                .option_id
                .ok_or_else(|| ApiError::BadRequest("option_id is required".into()))?;
            if !options(&p.payload).iter().any(|o| o.id == option_id) {
                return Err(ApiError::BadRequest(format!(
                    "unknown option `{option_id}`"
                )));
            }
            // Allowed, the result is known once the session moved: none here. A follow has no
            // call to answer.
            (
                (p.kind == "move" && option_id != worker_approval::ONCE)
                    .then(|| crate::moves::denied(request_id)),
                "approval.response",
                json!({"request_id": request_id, "option_id": option_id, "by": user.id.to_string()}),
            )
        }
        "worker_approval" => {
            if body.message.is_some() {
                return Err(ApiError::BadRequest(
                    "a worker request takes no message".into(),
                ));
            }
            // Only the owner of the worker that asks (SPEC §10.2).
            let owner = match p.worker_id {
                Some(w) => db::worker(&app.db, w).await?.map(|w| w.owner_id),
                None => None,
            };
            if owner != Some(user.id) {
                return Err(ApiError::Forbidden(
                    "only the worker owner answers its requests".into(),
                ));
            }
            let option_id = body
                .option_id
                .ok_or_else(|| ApiError::BadRequest("option_id is required".into()))?;
            if !options(&p.payload).iter().any(|o| o.id == option_id) {
                return Err(ApiError::BadRequest(format!(
                    "unknown option `{option_id}`"
                )));
            }
            (
                Some(worker_response(request_id, &option_id)),
                "approval.response",
                json!({"request_id": request_id, "option_id": option_id, "by": user.id.to_string()}),
            )
        }
        "clarify" => {
            if body.message.is_some() {
                return Err(ApiError::BadRequest(
                    "a clarify request takes answers, not a message".into(),
                ));
            }
            let answers = body
                .answers
                .ok_or_else(|| ApiError::BadRequest("answers is required".into()))?;
            // Empty answers mean "cancelled by the hub" on the wire (SPEC §5.3).
            if answers.is_empty() {
                return Err(ApiError::BadRequest("answers must not be empty".into()));
            }
            let mut answers = serde_json::to_value(answers).map_err(anyhow::Error::from)?;
            hqc_proto::replace_nul_json(&mut answers);
            (
                Some(json!({"type": "clarify.response",
                       "payload": {"session_id": s.id, "request_id": request_id, "answers": answers}})),
                "clarify.response",
                json!({"request_id": request_id, "answers": answers}),
            )
        }
        _ => return Err(ApiError::NotFound),
    };
    if !db::close_request(&app.db, p.id, frame.as_ref()).await? {
        return Err(ApiError::Conflict(
            "request_closed",
            "the request is already closed".into(),
        ));
    }
    let once = ev_payload["option_id"] == worker_approval::ONCE;
    let row = events::store_hub_event(
        app,
        s,
        user_author(user.id),
        ev_type,
        ev_payload,
        None,
        None,
    )
    .await?;
    match frame {
        Some(frame) => {
            deliver_result(
                app,
                &PendingRow {
                    result: Some(frame),
                    ..p
                },
            )
            .await
        }
        // A follow of the transcript: nobody's call waits; allowed, the session moves now.
        None if p.kind == crate::mirror::FOLLOW => {
            if once {
                crate::mirror::allowed(app, p);
            }
        }
        // An allowed move: the hub moves the session now and answers the call then.
        None => crate::moves::allowed(app, p),
    }
    Ok(EventRowOut {
        event_id: row.event_id,
        seq: row.seq,
    })
}

#[derive(Debug, serde::Serialize)]
pub struct EventRowOut {
    pub event_id: Uuid,
    pub seq: i64,
}

/// Close an open request on the hub's initiative.
pub async fn cancel(app: &AppState, p: &PendingRow, reason: CancelReason) -> anyhow::Result<()> {
    cancel_inner(app, p, reason, true).await
}

/// Close an open request that no runtime holds any more (the worker restarted): the
/// cancel event is written, no frame goes to the worker.
pub async fn cancel_without_frame(
    app: &AppState,
    p: &PendingRow,
    reason: CancelReason,
) -> anyhow::Result<()> {
    cancel_inner(app, p, reason, false).await
}

async fn cancel_inner(
    app: &AppState,
    p: &PendingRow,
    reason: CancelReason,
    with_frame: bool,
) -> anyhow::Result<()> {
    let Some(s) = db::session(&app.db, p.session_id).await? else {
        return Ok(());
    };
    let ev_type = match p.kind.as_str() {
        "approval" | "worker_approval" | "move" => "approval.cancel",
        "clarify" => "clarify.cancel",
        _ => return Ok(()),
    };
    if !with_frame {
        if !db::close_request(&app.db, p.id, None).await? {
            return Ok(());
        }
        events::store_hub_event(
            app,
            &s,
            json!({"kind": "system"}),
            ev_type,
            json!({"request_id": p.request_id, "reason": reason}),
            None,
            None,
        )
        .await?;
        return Ok(());
    }
    let frame = match p.kind.as_str() {
        "approval" => deny_option(&p.payload).map(|option_id| {
            json!({"type": "approval.response",
                   "payload": {"session_id": s.id, "request_id": p.request_id, "option_id": option_id}})
        }),
        "worker_approval" => Some(worker_response(&p.request_id, worker_approval::DENY)),
        "move" => Some(crate::moves::cancelled(&p.request_id, reason)),
        _ => Some(json!({"type": "clarify.response",
                         "payload": {"session_id": s.id, "request_id": p.request_id, "answers": {}}})),
    };
    if frame.is_none() {
        tracing::warn!(session_id = %s.id, request_id = %p.request_id, "approval without a deny option: cancelled without a frame");
    }
    if !db::close_request(&app.db, p.id, frame.as_ref()).await? {
        return Ok(());
    }
    events::store_hub_event(
        app,
        &s,
        json!({"kind": "system"}),
        ev_type,
        json!({"request_id": p.request_id, "reason": reason}),
        None,
        None,
    )
    .await?;
    if frame.is_some() {
        deliver_result(
            app,
            &PendingRow {
                result: frame,
                ..p.clone()
            },
        )
        .await;
    }
    Ok(())
}

pub async fn cancel_open_of_session(
    app: &AppState,
    session_id: Uuid,
    reason: CancelReason,
) -> anyhow::Result<()> {
    for p in db::open_requests_of_session(&app.db, session_id).await? {
        cancel(app, &p, reason).await?;
    }
    Ok(())
}

/// `auto_approve`: answer with the first `canonical: once` option, `by: auto` (SPEC §10.1);
/// only the runtime's own approvals, never a move of the session (SPEC §7).
pub async fn auto_approve(app: &AppState, s: &SessionRow, request_id: &str) -> anyhow::Result<()> {
    let Some(p) = db::find_open_request(&app.db, s.id, request_id)
        .await?
        .filter(|p| p.kind == "approval")
    else {
        return Ok(());
    };
    let Some(opt) = options(&p.payload)
        .into_iter()
        .find(|o| o.canonical == hqc_proto::event::ApprovalCanonical::Once)
    else {
        return Ok(());
    };
    let frame = json!({"type": "approval.response",
                       "payload": {"session_id": s.id, "request_id": request_id, "option_id": opt.id}});
    if !db::close_request(&app.db, p.id, Some(&frame)).await? {
        return Ok(());
    }
    events::store_hub_event(
        app,
        s,
        json!({"kind": "system"}),
        "approval.response",
        json!({"request_id": request_id, "option_id": opt.id, "by": "auto"}),
        None,
        None,
    )
    .await?;
    deliver_result(
        app,
        &PendingRow {
            result: Some(frame),
            ..p
        },
    )
    .await;
    Ok(())
}

fn worker_response(request_id: &str, option_id: &str) -> Value {
    json!({"type": "worker.approval.response",
           "payload": {"request_id": request_id, "option_id": option_id}})
}

/// Longest `description` of a worker request kept, in characters.
const MAX_WORKER_DESCRIPTION: usize = 2000;
/// Longest `description` accepted, in bytes (SPEC §20); longer is refused, not cut.
pub const MAX_WORKER_DESCRIPTION_BYTES: usize = 4096;
/// Longest `request_id` of a worker request, in bytes (SPEC §20).
pub const MAX_WORKER_REQUEST_ID: usize = 128;
/// Longest `kind` of a worker request, in bytes (SPEC §20).
pub const MAX_WORKER_KIND: usize = 64;
/// A worker request lives at most this long (SPEC §10.2, §20).
const MAX_WORKER_REQUEST_TTL: chrono::TimeDelta = chrono::TimeDelta::days(1);

/// What became of a `worker.approval.request`.
#[derive(Debug)]
pub enum WorkerApproval {
    /// Stored (or a resend of a request still open): `ack`.
    Stored,
    /// A resend of an answered request: `ack`, then the answer again.
    Answered(PendingRow),
    /// Over a limit of SPEC §20: `error invalid_frame`, nothing stored.
    Refused(String),
}

/// The limits of SPEC §20 on a new `worker.approval.request`, except the number of open
/// ones; `Err` says why it is refused.
fn check_worker_request(
    r: &WorkerApprovalRequest,
    now: chrono::DateTime<Utc>,
) -> Result<(), String> {
    if r.request_id.is_empty() || r.request_id.len() > MAX_WORKER_REQUEST_ID {
        return Err(format!(
            "request_id must be 1..{MAX_WORKER_REQUEST_ID} bytes"
        ));
    }
    if r.kind.is_empty() || r.kind.len() > MAX_WORKER_KIND {
        return Err(format!("kind must be 1..{MAX_WORKER_KIND} bytes"));
    }
    if r.description.len() > MAX_WORKER_DESCRIPTION_BYTES {
        return Err(format!(
            "description is over {MAX_WORKER_DESCRIPTION_BYTES} bytes"
        ));
    }
    if r.expires_at <= now {
        return Err("expires_at is in the past".into());
    }
    if r.expires_at > now + MAX_WORKER_REQUEST_TTL {
        return Err("expires_at is more than 24 h ahead".into());
    }
    Ok(())
}

/// `worker.approval.request` (SPEC §5.4, §10.2): an `approval.request` in the worker's
/// service session with the options `once` / `deny` and a `worker_approval`
/// PendingRequest, so it is answered, expires and pushes like any approval. The worker
/// sends it again after a reconnect while it still waits: a known `request_id` is not
/// stored twice, and an answered one is returned, for the caller to send the answer again
/// after its `ack`. A new request over the limits of SPEC §20 (lengths, `expires_at`,
/// open requests of the worker) is refused.
pub async fn worker_approval(
    app: &AppState,
    worker_id: Uuid,
    r: &WorkerApprovalRequest,
) -> anyhow::Result<WorkerApproval> {
    if r.request_id.len() > MAX_WORKER_REQUEST_ID {
        return Ok(WorkerApproval::Refused(format!(
            "request_id must be 1..{MAX_WORKER_REQUEST_ID} bytes"
        )));
    }
    let Some(sid) = db::ensure_system_session(&app.db, worker_id).await? else {
        anyhow::bail!("worker {worker_id} has no service session");
    };
    let Some(s) = db::session(&app.db, sid).await? else {
        anyhow::bail!("service session {sid} is gone");
    };
    if let Some(p) = db::request_by_id(&app.db, s.id, "worker_approval", &r.request_id).await? {
        return Ok(match p.result {
            Some(_) => WorkerApproval::Answered(p),
            None => WorkerApproval::Stored,
        });
    }
    if let Err(why) = check_worker_request(r, Utc::now()) {
        tracing::warn!(%worker_id, request_id = %r.request_id, "worker request refused: {why}");
        return Ok(WorkerApproval::Refused(why));
    }
    let max_open = app.config.limits.worker_approvals_open;
    if db::open_worker_requests(&app.db, worker_id).await? >= max_open {
        tracing::warn!(%worker_id, request_id = %r.request_id, "worker request refused: {max_open} open already");
        return Ok(WorkerApproval::Refused(format!(
            "{max_open} requests of this worker are open already"
        )));
    }
    let description: String = r.description.chars().take(MAX_WORKER_DESCRIPTION).collect();
    let expires_at = r.expires_at;
    let payload = json!({
        "request_id": r.request_id,
        "tool": r.kind,
        "description": description,
        "options": [
            {"id": worker_approval::ONCE, "label": "Allow", "canonical": "once"},
            {"id": worker_approval::DENY, "label": "Deny", "canonical": "deny"},
        ],
        "expires_at": expires_at,
    });
    let mut stored = payload.clone();
    hqc_proto::replace_nul_json(&mut stored);
    if !db::open_request(
        &app.db,
        "worker_approval",
        s.id,
        &r.request_id,
        &stored,
        Some(expires_at),
        Some(worker_id),
    )
    .await?
    {
        return Ok(WorkerApproval::Stored);
    }
    tracing::info!(%worker_id, request_id = %r.request_id, kind = %r.kind, "worker asks its owner");
    events::store_hub_event(
        app,
        &s,
        json!({"kind": "system"}),
        "approval.request",
        payload,
        None,
        None,
    )
    .await?;
    Ok(WorkerApproval::Stored)
}

/// Send the stored result frame if the worker is connected; it stays undelivered until
/// the worker's `ack`. While `session.start` of the session awaits its `ack` on this
/// connection, the frame waits too: [`crate::delivery::on_start_ok`] sends it.
pub async fn deliver_result(app: &AppState, p: &PendingRow) {
    let Some(frame) = &p.result else { return };
    let worker = match p.worker_id {
        Some(w) => Some(w),
        None => db::session(&app.db, p.session_id)
            .await
            .ok()
            .flatten()
            .and_then(|s| s.worker_id),
    };
    let Some(conn) = worker.and_then(|w| app.workers.get(w)) else {
        return;
    };
    let (Some(t), Some(payload)) = (
        frame.get("type").and_then(Value::as_str),
        frame.get("payload"),
    ) else {
        return;
    };
    let mut st = conn.st.lock().await;
    if st
        .sessions
        .get(&p.session_id)
        .is_some_and(|sc| sc.start == StartState::Starting)
    {
        tracing::debug!(session_id = %p.session_id, request_id = %p.request_id, "result waits for the ack of session.start");
        return;
    }
    let id = conn.send_raw(t, payload);
    tracing::debug!(session_id = %p.session_id, request_id = %p.request_id, frame = %id, "result sent");
    st.replies.insert(id, Reply::Result { pending_id: p.id });
}

/// Cancel requests whose `expires_at` has passed (SPEC §10.1), every `expiry_interval`.
pub async fn expiry_loop(app: AppState) {
    let mut tick = tokio::time::interval(app.config.timing.expiry_interval);
    loop {
        tick.tick().await;
        match db::expired_requests(&app.db).await {
            Ok(rows) => {
                for p in rows {
                    if let Err(e) = cancel(&app, &p, CancelReason::Timeout).await {
                        tracing::error!(request_id = %p.request_id, error = %e, "cancel expired request");
                    }
                }
            }
            Err(e) => tracing::error!(error = %e, "expired requests query"),
        }
        if let Err(e) = crate::peers::expire(&app).await {
            tracing::error!(error = %e, "expired asks");
        }
    }
}

// ---------------------------------------------------------------- the user's open requests

/// How far along a chain of asks the waiting sessions are looked for. A chain nests at most
/// [`crate::peers::MAX_DEPTH`] asks; asks of separate chains can join into a longer line or
/// a cycle, which this bounds.
const MAX_WAIT_DEPTH: i32 = 8;

#[derive(Debug, sqlx::FromRow)]
struct WaitRow {
    session_id: Uuid,
    waiter: Uuid,
    address: String,
    title: String,
}

/// Session -> the sessions of `owner` that wait on it through open `hqc_ask`s, directly or
/// along a chain, the nearest first (SPEC §7.1, §10.1).
async fn waits(app: &AppState, owner: Uuid) -> anyhow::Result<HashMap<Uuid, Vec<WaitingSession>>> {
    let rows: Vec<WaitRow> = sqlx::query_as(
        "WITH RECURSIVE asks AS (
             SELECT p.session_id AS asker, (p.payload ->> 'target_session_id')::uuid AS target
             FROM pending_requests p JOIN sessions s ON s.id = p.session_id
             WHERE p.kind = 'ask' AND p.closed_at IS NULL AND s.owner_id = $1),
         waits (session_id, waiter, depth) AS (
             SELECT target, asker, 1 FROM asks
             UNION
             SELECT w.session_id, a.asker, w.depth + 1
             FROM waits w JOIN asks a ON a.target = w.waiter
             WHERE w.depth < $2)
         SELECT w.session_id, w.waiter, pr.slug || '/' || s.slug AS address, s.title
         FROM waits w JOIN sessions s ON s.id = w.waiter JOIN projects pr ON pr.id = s.project_id
         WHERE w.waiter <> w.session_id AND s.owner_id = $1
         GROUP BY w.session_id, w.waiter, pr.slug, s.slug, s.title
         ORDER BY w.session_id, min(w.depth), w.waiter",
    )
    .bind(owner)
    .bind(MAX_WAIT_DEPTH)
    .fetch_all(&app.db)
    .await?;
    let mut m: HashMap<Uuid, Vec<WaitingSession>> = HashMap::new();
    for r in rows {
        m.entry(r.session_id).or_default().push(WaitingSession {
            session_id: r.waiter,
            address: r.address,
            title: r.title,
        });
    }
    Ok(m)
}

/// The sessions of `owner` waiting on `session_id`, the nearest first; the last is the top
/// of the chain.
pub async fn waiting_on(
    app: &AppState,
    owner: Uuid,
    session_id: Uuid,
) -> anyhow::Result<Vec<WaitingSession>> {
    Ok(waits(app, owner)
        .await?
        .remove(&session_id)
        .unwrap_or_default())
}

#[derive(Debug, sqlx::FromRow)]
struct OpenRow {
    session_id: Uuid,
    request_id: String,
    kind: String,
    address: String,
    title: String,
    worker_id: Option<Uuid>,
    worker_name: Option<String>,
    system: bool,
    event_id: Option<Uuid>,
}

/// Every open `approval`, `clarify`, `worker_approval`, `move` and `follow` in the sessions of
/// `owner`, oldest first, each with its event and the sessions waiting on it (SPEC §6 `GET
/// /api/requests`). Only the owner's sessions: another user's requests are never listed.
pub async fn open_of(app: &AppState, owner: Uuid) -> anyhow::Result<Vec<OpenRequest>> {
    let rows: Vec<OpenRow> = sqlx::query_as(
        "SELECT p.session_id, p.request_id, p.kind, pr.slug || '/' || s.slug AS address,
                s.title, s.worker_id, w.name AS worker_name, s.system,
                (SELECT e.event_id FROM events e
                 WHERE e.session_id = p.session_id
                   AND e.type IN ('approval.request', 'clarify.request')
                   AND e.payload ->> 'request_id' = p.request_id
                 ORDER BY e.seq DESC LIMIT 1) AS event_id
         FROM pending_requests p
         JOIN sessions s ON s.id = p.session_id
         JOIN projects pr ON pr.id = s.project_id
         LEFT JOIN workers w ON w.id = s.worker_id
         WHERE p.closed_at IS NULL
           AND p.kind IN ('approval', 'clarify', 'worker_approval', 'move', 'follow')
           AND s.owner_id = $1 AND NOT s.archived
         ORDER BY p.created_at, p.id",
    )
    .bind(owner)
    .fetch_all(&app.db)
    .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = rows.iter().filter_map(|r| r.event_id).collect();
    let events: HashMap<Uuid, Value> = db::events_by_ids(&app.db, &ids)
        .await?
        .into_iter()
        .map(|e| (e.event_id, e.to_json()))
        .collect();
    let waits = waits(app, owner).await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let event = events.get(&r.event_id?)?.clone();
            Some(OpenRequest {
                waiting: waits.get(&r.session_id).cloned().unwrap_or_default(),
                session_id: r.session_id,
                request_id: r.request_id,
                kind: r.kind,
                address: r.address,
                title: r.title,
                worker_id: r.worker_id,
                worker_name: r.worker_name,
                system: r.system,
                event,
            })
        })
        .collect())
}

/// The open requests of the owner of `session_id`, or who waits on them, may have changed:
/// clients of that owner get the list again (SPEC §6 `requests`).
pub async fn changed_in(app: &AppState, session_id: Uuid) {
    match db::session(&app.db, session_id).await {
        Ok(Some(s)) => app.requests_changed(s.owner_id),
        Ok(None) => {}
        Err(e) => tracing::error!(%session_id, error = %e, "requests changed: session lookup"),
    }
}
