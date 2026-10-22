//! REST API (SPEC §6), iteration 1 subset, plus the worker enrollment endpoints (SPEC §5.1).
//!
//! Access control: `can` stands on every request. Iteration 1 has no grants (SPEC §11,
//! iteration 6), so a person can act only on their own projects, sessions and workers.

use std::time::{Duration, Instant};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use hqc_proto::{
    api::{self, EnrollPollRequest, EnrollRequest, EnrollStarted},
    event::CancelReason,
    frame::HubFrame,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::{hash_secret, new_enroll_code, new_secret, normalize_enroll_code, CurrentUser},
    db::{self, EnrollPoll, SessionRow},
    delivery,
    error::{ApiError, ApiResult},
    events::{self, user_author},
    requests, state,
};

/// SPEC §11 `can(user, action, resource)`. Iteration 1: owner only.
#[derive(Debug, Clone, Copy)]
pub enum Action {
    Read,
    Write,
    Manage,
}

fn can(user: &CurrentUser, _action: Action, s: &SessionRow) -> bool {
    s.owner_id == user.id
}

async fn session_for(
    app: &AppState,
    user: &CurrentUser,
    id: Uuid,
    action: Action,
) -> ApiResult<SessionRow> {
    match db::session(&app.db, id).await? {
        // Not found and not allowed look the same: no existence leak.
        Some(s) if can(user, action, &s) => Ok(s),
        _ => Err(ApiError::NotFound),
    }
}

// ---------------------------------------------------------------- me, projects

pub async fn me(user: CurrentUser) -> Json<Value> {
    Json(json!({
        "id": user.id,
        "email": user.email,
        "display_name": user.display_name,
        "timezone": user.timezone,
        "personal_project_id": user.personal_project_id,
    }))
}

pub async fn projects(State(app): State<AppState>, user: CurrentUser) -> ApiResult<Json<Value>> {
    Ok(Json(
        json!({ "projects": db::projects_of(&app.db, user.id).await? }),
    ))
}

pub async fn project(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    match db::project(&app.db, id).await? {
        Some(p) if p.owner_id == user.id => {
            Ok(Json(serde_json::to_value(p).map_err(anyhow::Error::from)?))
        }
        _ => Err(ApiError::NotFound),
    }
}

// ---------------------------------------------------------------- sessions

pub async fn project_sessions(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    match db::project(&app.db, id).await? {
        Some(p) if p.owner_id == user.id => {}
        _ => return Err(ApiError::NotFound),
    }
    let rows = db::sessions_of_project(&app.db, id).await?;
    Ok(Json(
        json!({ "sessions": state::views(&app, user.id, rows).await? }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct CreateSession {
    pub title: Option<String>,
    pub worker_id: Option<Uuid>,
    pub model: Option<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub auto_approve: bool,
}

/// `cwd` is relative to the workspace (SPEC §3 Session); the worker checks it again
/// against its own config (SPEC §2.1).
fn valid_cwd(cwd: &str) -> bool {
    !cwd.starts_with('/') && !cwd.contains('\\') && !cwd.split('/').any(|c| c == "..")
}

pub async fn create_session(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(b): Json<CreateSession>,
) -> ApiResult<impl IntoResponse> {
    match db::project(&app.db, project_id).await? {
        Some(p) if p.owner_id == user.id => {}
        _ => return Err(ApiError::NotFound),
    }
    let worker = match b.worker_id {
        Some(id) => match db::worker(&app.db, id).await? {
            Some(w) if w.owner_id == user.id && w.revoked_at.is_none() => w,
            _ => return Err(ApiError::BadRequest("unknown worker".into())),
        },
        None => {
            // Default: an online worker of the user, else the most recently seen one.
            let mut ws = db::workers_of(&app.db, user.id).await?;
            ws.sort_by_key(|w| (app.workers.is_online(w.id), w.last_seen));
            ws.pop()
                .ok_or_else(|| ApiError::Conflict("no_worker", "no worker is enrolled".into()))?
        }
    };
    // Iteration 1: the session runs in the workspace the worker announced in `hello`.
    let ws = db::latest_workspace(&app.db, project_id, worker.id)
        .await?
        .ok_or_else(|| {
            ApiError::Conflict(
                "no_workspace",
                "the worker has not announced a workspace".into(),
            )
        })?;
    let cwd = b
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .unwrap_or(".");
    if !valid_cwd(cwd) {
        return Err(ApiError::BadRequest(
            "cwd must be a relative path inside the workspace".into(),
        ));
    }
    let title = b
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("New session");
    let id = db::create_session(
        &app.db,
        db::NewSession {
            project_id,
            owner_id: user.id,
            workspace_id: Some(ws.id),
            worker_id: Some(worker.id),
            cwd,
            title,
            model: b.model.as_deref(),
            // Only the worker owner may enable it (SPEC §10.1); here that is the creator.
            auto_approve: b.auto_approve && worker.owner_id == user.id,
        },
    )
    .await?;
    let s = db::session(&app.db, id).await?.ok_or(ApiError::NotFound)?;
    events::system(&app, &s, "session_created", "Session created").await?;
    delivery::kick(&app, id, true).await;
    let s = db::session(&app.db, id).await?.ok_or(ApiError::NotFound)?;
    let v = state::view(&app, user.id, s).await?;
    Ok((StatusCode::CREATED, Json(json!(v))))
}

pub async fn get_session(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Read).await?;
    Ok(Json(json!(state::view(&app, user.id, s).await?)))
}

#[derive(Debug, Deserialize)]
pub struct PatchSession {
    pub title: Option<String>,
    pub auto_approve: Option<bool>,
}

pub async fn patch_session(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<PatchSession>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Manage).await?;
    if let Some(t) = b.title.as_deref().map(str::trim) {
        if t.is_empty() {
            return Err(ApiError::BadRequest("title must not be empty".into()));
        }
        // The slug stays: it is the session's address (SPEC §7).
        sqlx::query("UPDATE sessions SET title = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(t)
            .execute(&app.db)
            .await?;
    }
    if let Some(a) = b.auto_approve {
        let owner = match s.worker_id {
            Some(w) => db::worker(&app.db, w).await?.map(|w| w.owner_id),
            None => None,
        };
        if a && owner != Some(user.id) {
            return Err(ApiError::Forbidden(
                "only the worker owner enables auto_approve".into(),
            ));
        }
        sqlx::query("UPDATE sessions SET auto_approve = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(a)
            .execute(&app.db)
            .await?;
    }
    app.session_changed(id);
    let s = db::session(&app.db, id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(json!(state::view(&app, user.id, s).await?)))
}

/// Deleting a session drops its events, cursors and requests; the worker's jsonl files are
/// not touched (SPEC §17.4).
pub async fn delete_session(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let s = session_for(&app, &user, id, Action::Manage).await?;
    send_to_worker(&app, &s, HubFrame::SessionKill { session_id: id });
    sqlx::query("DELETE FROM sessions WHERE id = $1")
        .bind(id)
        .execute(&app.db)
        .await?;
    app.session_changed(id);
    Ok(StatusCode::NO_CONTENT)
}

/// Fire-and-forget control frame; `false` when the worker is offline.
fn send_to_worker(app: &AppState, s: &SessionRow, frame: HubFrame) -> bool {
    match s.worker_id.and_then(|w| app.workers.get(w)) {
        Some(conn) => {
            conn.send(frame);
            true
        }
        None => false,
    }
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    pub after_seq: Option<i64>,
    pub before_seq: Option<i64>,
    pub limit: Option<i64>,
}

/// `GET /api/sessions/{id}/events?after_seq=&limit=` — oldest first. `before_seq` (an
/// extension) returns the last `limit` events before it, for scrolling back.
pub async fn session_events(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Query(q): Query<EventsQuery>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Read).await?;
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let rows = match q.before_seq {
        Some(b) => db::events_before(&app.db, id, b, limit).await?,
        None => db::events_after(&app.db, id, q.after_seq.unwrap_or(0), limit).await?,
    };
    let events: Vec<Value> = rows.iter().map(|r| r.to_json()).collect();
    Ok(Json(json!({ "events": events, "last_seq": s.last_seq })))
}

#[derive(Debug, Deserialize)]
pub struct InputBody {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub artifact_ids: Vec<Uuid>,
    pub client_op_id: Option<String>,
}

/// `POST /api/sessions/{id}/input`: stored as `user.message` with `delivery: pending`
/// and delivered when the worker is there (SPEC §5.2). A repeated `client_op_id` returns
/// the event stored the first time.
pub async fn input(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<InputBody>,
) -> ApiResult<impl IntoResponse> {
    let s = session_for(&app, &user, id, Action::Write).await?;
    if s.archived {
        return Err(ApiError::Conflict(
            "archived",
            "the session is archived".into(),
        ));
    }
    if s.locked_by == "vscode" {
        return Err(ApiError::Conflict(
            "locked_vscode",
            "the session is being driven from VS Code; take it over first".into(),
        ));
    }
    if b.text.trim().is_empty() && b.artifact_ids.is_empty() {
        return Err(ApiError::BadRequest(
            "text or artifact_ids is required".into(),
        ));
    }
    let op = b.client_op_id.filter(|c| !c.is_empty());
    if let Some(op) = &op {
        if let Some(r) = db::event_by_client_op(&app.db, id, op).await? {
            return Ok((StatusCode::OK, Json(json!({ "event": r.to_json() }))));
        }
    }
    let stored = events::store_hub_event(
        &app,
        &s,
        user_author(user.id),
        "user.message",
        json!({"text": b.text, "artifact_ids": b.artifact_ids}),
        op.clone(),
        Some("pending"),
    )
    .await;
    let row = match stored {
        Ok(r) => r,
        Err(e) => {
            // Lost a race on the same client_op_id.
            if let Some(op) = &op {
                if let Some(r) = db::event_by_client_op(&app.db, id, op).await? {
                    return Ok((StatusCode::OK, Json(json!({ "event": r.to_json() }))));
                }
            }
            return Err(e.into());
        }
    };
    delivery::kick(&app, id, false).await;
    Ok((StatusCode::CREATED, Json(json!({ "event": row.to_json() }))))
}

/// Abort the current turn; open requests are cancelled with `stopped`.
pub async fn stop(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Write).await?;
    requests::cancel_open_of_session(&app, id, CancelReason::Stopped).await?;
    let sent = send_to_worker(&app, &s, HubFrame::SessionStop { session_id: id });
    Ok(Json(json!({ "sent": sent })))
}

pub async fn retry(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    session_for(&app, &user, id, Action::Write).await?;
    let n = delivery::retry(&app, id).await?;
    Ok(Json(json!({ "requeued": n })))
}

/// SPEC §17.4: kill the process, cancel open requests, fail pending input.
pub async fn archive(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Manage).await?;
    sqlx::query(
        "UPDATE sessions SET archived = true, turn_open = false, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&app.db)
    .await?;
    send_to_worker(&app, &s, HubFrame::SessionKill { session_id: id });
    requests::cancel_open_of_session(&app, id, CancelReason::Archived).await?;
    for row in db::move_inputs(&app.db, id, "pending", "failed").await? {
        delivery::notify_delivery(&app, &row);
    }
    app.session_changed(id);
    let s = db::session(&app.db, id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(json!(state::view(&app, user.id, s).await?)))
}

pub async fn unarchive(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    session_for(&app, &user, id, Action::Manage).await?;
    sqlx::query("UPDATE sessions SET archived = false, updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(&app.db)
        .await?;
    app.session_changed(id);
    let s = db::session(&app.db, id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(json!(state::view(&app, user.id, s).await?)))
}

pub async fn answer_request(
    State(app): State<AppState>,
    user: CurrentUser,
    Path((id, request_id)): Path<(Uuid, String)>,
    Json(b): Json<requests::AnswerBody>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Write).await?;
    let r = requests::answer(&app, &user, &s, &request_id, b).await?;
    Ok(Json(json!(r)))
}

#[derive(Debug, Deserialize)]
pub struct ReadCursor {
    pub last_read_seq: i64,
}

pub async fn read_cursor(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<ReadCursor>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Read).await?;
    let seq =
        db::set_read_cursor(&app.db, user.id, id, b.last_read_seq.clamp(0, s.last_seq)).await?;
    app.session_changed(id);
    Ok(Json(json!({ "last_read_seq": seq })))
}

// ---------------------------------------------------------------- workers

pub async fn workers(State(app): State<AppState>, user: CurrentUser) -> ApiResult<Json<Value>> {
    let ws: Vec<Value> = db::workers_of(&app.db, user.id)
        .await?
        .into_iter()
        .map(|w| {
            json!({
                "id": w.id,
                "name": w.name,
                "os": w.os,
                "status": if app.workers.is_online(w.id) { "online" } else { "offline" },
                "last_seen": w.last_seen,
                "protocol_version": w.protocol_version,
                "capabilities": w.capabilities,
                "environments": w.environments,
                "env_bindings": w.env_bindings,
                "allowed_roots": w.allowed_roots,
                "allow_foreign_sessions": w.allow_foreign_sessions,
                "share_local_models": w.share_local_models,
                "local_models": w.local_models,
                "created_at": w.created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "workers": ws })))
}

#[derive(Debug, Deserialize)]
pub struct EnrollConfirm {
    pub code: String,
    pub name: Option<String>,
}

pub async fn enroll_confirm(
    State(app): State<AppState>,
    user: CurrentUser,
    Json(b): Json<EnrollConfirm>,
) -> ApiResult<Json<Value>> {
    let code = normalize_enroll_code(&b.code);
    let name = b.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    let (worker_id, name) = db::enroll_confirm(&app.db, &code, user.id, name)
        .await?
        .ok_or(ApiError::NotFound)?;
    tracing::info!(%worker_id, user = %user.id, "worker enrollment confirmed");
    Ok(Json(json!({ "worker_id": worker_id, "name": name })))
}

/// Revoke the worker's token and drop its connection (SPEC §5.1).
pub async fn delete_worker(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    if !db::revoke_worker(&app.db, id, user.id).await? {
        return Err(ApiError::NotFound);
    }
    if let Some(c) = app.workers.get(id) {
        c.close();
    }
    tracing::info!(worker_id = %id, "worker revoked");
    Ok(StatusCode::NO_CONTENT)
}

const ENROLL_PER_MINUTE: usize = 10;
const ENROLL_MAX_ACTIVE: i64 = 100;

/// `POST /worker/enroll` (no auth): a code for the user to confirm and a secret to poll
/// with. Rate-limited (SPEC §20). `protocol_version` in the body is informational; unknown
/// fields are ignored (SPEC §5.1).
pub async fn worker_enroll(
    State(app): State<AppState>,
    Json(b): Json<EnrollRequest>,
) -> ApiResult<Json<EnrollStarted>> {
    {
        let mut calls = app.enroll_calls.lock().unwrap();
        calls.retain(|t| t.elapsed() < Duration::from_secs(60));
        if calls.len() >= ENROLL_PER_MINUTE {
            return Err(ApiError::TooManyRequests);
        }
        calls.push(Instant::now());
    }
    if db::enroll_active_count(&app.db).await? >= ENROLL_MAX_ACTIVE {
        return Err(ApiError::TooManyRequests);
    }
    let name: String = b.name.trim().chars().take(64).collect();
    if name.is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    let os: String = b.os.trim().chars().take(32).collect();
    if b.protocol_version
        .is_some_and(|v| v != hqc_proto::PROTOCOL_VERSION)
    {
        tracing::warn!(worker_protocol = ?b.protocol_version, "enrolling a worker with another protocol version; its hello will be refused");
    }
    let code = new_enroll_code();
    let poll_token = new_secret("hqce_");
    let expires_at = Utc::now() + app.config.timing.enroll_ttl;
    db::enroll_create(
        &app.db,
        &code,
        &hash_secret(&poll_token),
        &name,
        &os,
        expires_at,
    )
    .await?;
    Ok(Json(EnrollStarted {
        code,
        poll_token,
        expires_at,
        poll_interval_s: 3,
    }))
}

/// `POST /worker/enroll/poll`: `pending`, `confirmed` with the worker token (exactly once),
/// or `expired`.
pub async fn worker_enroll_poll(
    State(app): State<AppState>,
    Json(b): Json<EnrollPollRequest>,
) -> ApiResult<Json<api::EnrollPoll>> {
    let token = new_secret("hqcw_");
    match db::enroll_poll(&app.db, &hash_secret(&b.poll_token), &hash_secret(&token)).await? {
        EnrollPoll::Pending => Ok(Json(api::EnrollPoll::Pending)),
        EnrollPoll::Confirmed { worker_id } => {
            tracing::info!(%worker_id, "worker token issued");
            Ok(Json(api::EnrollPoll::Confirmed { worker_id, token }))
        }
        EnrollPoll::Gone => Ok(Json(api::EnrollPoll::Expired)),
    }
}

// ---------------------------------------------------------------- push

pub async fn vapid(State(app): State<AppState>, _user: CurrentUser) -> Json<Value> {
    Json(json!({ "public_key": app.push.public_key() }))
}

#[derive(Debug, Deserialize)]
pub struct PushKeys {
    pub p256dh: String,
    pub auth: String,
}

/// The browser's `PushSubscription.toJSON()`.
#[derive(Debug, Deserialize)]
pub struct PushSubscribe {
    pub endpoint: String,
    pub keys: PushKeys,
}

pub async fn push_subscribe(
    State(app): State<AppState>,
    user: CurrentUser,
    Json(b): Json<PushSubscribe>,
) -> ApiResult<impl IntoResponse> {
    if !b.endpoint.starts_with("https://") {
        return Err(ApiError::BadRequest("endpoint must be https".into()));
    }
    let id =
        db::upsert_push_sub(&app.db, user.id, &b.endpoint, &b.keys.p256dh, &b.keys.auth).await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

#[derive(Debug, Deserialize)]
pub struct PushUnsubscribe {
    pub endpoint: String,
}

pub async fn push_unsubscribe(
    State(app): State<AppState>,
    user: CurrentUser,
    Json(b): Json<PushUnsubscribe>,
) -> ApiResult<StatusCode> {
    db::delete_push_sub(&app.db, Some(user.id), &b.endpoint).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::valid_cwd;

    #[test]
    fn cwd_must_stay_inside() {
        assert!(valid_cwd("."));
        assert!(valid_cwd("crates/hqc-hub"));
        assert!(!valid_cwd("/etc"));
        assert!(!valid_cwd("../x"));
        assert!(!valid_cwd("a/../../b"));
    }
}
