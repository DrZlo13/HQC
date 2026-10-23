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

use std::collections::BTreeMap;

use chrono::Utc;
use hqc_proto::{
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
            if !options(&p.payload).iter().any(|o| o.id == option_id) {
                return Err(ApiError::BadRequest(format!(
                    "unknown option `{option_id}`"
                )));
            }
            (
                json!({"type": "approval.response",
                       "payload": {"session_id": s.id, "request_id": request_id, "option_id": option_id}}),
                "approval.response",
                json!({"request_id": request_id, "option_id": option_id, "by": user.id.to_string()}),
            )
        }
        "worker_approval" => {
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
                worker_response(request_id, &option_id),
                "approval.response",
                json!({"request_id": request_id, "option_id": option_id, "by": user.id.to_string()}),
            )
        }
        "clarify" => {
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
                json!({"type": "clarify.response",
                       "payload": {"session_id": s.id, "request_id": request_id, "answers": answers}}),
                "clarify.response",
                json!({"request_id": request_id, "answers": answers}),
            )
        }
        _ => return Err(ApiError::NotFound),
    };
    if !db::close_request(&app.db, p.id, Some(&frame)).await? {
        return Err(ApiError::Conflict(
            "request_closed",
            "the request is already closed".into(),
        ));
    }
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
    deliver_result(
        app,
        &PendingRow {
            result: Some(frame),
            ..p
        },
    )
    .await;
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
        "approval" | "worker_approval" => "approval.cancel",
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

/// `auto_approve`: answer with the first `canonical: once` option, `by: auto` (SPEC §10.1).
pub async fn auto_approve(app: &AppState, s: &SessionRow, request_id: &str) -> anyhow::Result<()> {
    let Some(p) = db::find_open_request(&app.db, s.id, request_id).await? else {
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
/// A worker request lives at most this long, whatever `expires_at` it asks for.
const MAX_WORKER_REQUEST_TTL: chrono::TimeDelta = chrono::TimeDelta::days(1);

/// `worker.approval.request` (SPEC §5.4, §10.2): an `approval.request` in the worker's
/// service session with the options `once` / `deny` and a `worker_approval`
/// PendingRequest, so it is answered, expires and pushes like any approval. The worker
/// sends it again after a reconnect while it still waits: a known `request_id` is not
/// stored twice, and an answered one is returned, for the caller to send the answer again
/// after its `ack`.
pub async fn worker_approval(
    app: &AppState,
    worker_id: Uuid,
    r: &WorkerApprovalRequest,
) -> anyhow::Result<Option<PendingRow>> {
    let Some(sid) = db::ensure_system_session(&app.db, worker_id).await? else {
        anyhow::bail!("worker {worker_id} has no service session");
    };
    let Some(s) = db::session(&app.db, sid).await? else {
        anyhow::bail!("service session {sid} is gone");
    };
    if let Some(p) = db::request_by_id(&app.db, s.id, "worker_approval", &r.request_id).await? {
        return Ok(p.result.is_some().then_some(p));
    }
    let description: String = r.description.chars().take(MAX_WORKER_DESCRIPTION).collect();
    let expires_at = r.expires_at.min(Utc::now() + MAX_WORKER_REQUEST_TTL);
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
        return Ok(None);
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
    Ok(None)
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
        return;
    }
    let id = conn.send_raw(t, payload);
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
    }
}
