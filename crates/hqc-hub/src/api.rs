//! REST API (SPEC §6): iteration 1 plus projects and workspaces (T14), and the worker
//! enrollment endpoints (SPEC §5.1).
//!
//! Access control: `can` stands on every request. Iteration 1 has no grants (SPEC §11,
//! iteration 6), so a person can act only on their own projects, sessions and workers.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use hqc_proto::{
    api::{
        self, CandidateList, CreateProject, CreateSessionRequest, CreateWorkspaceImport, DirList,
        EnrollConfirmRequest, EnrollConfirmed, EnrollInfo, EnrollPollRequest, EnrollRequest,
        EnrollStarted, EventPage, InputRequest, InputResponse, PatchProject, PatchWorkspace,
        ProjectList, PushSubscription, SessionList, VapidKey, WorkspaceList,
    },
    event::CancelReason,
    frame::{FsListDirs, FsStat, HubFrame, ListCandidates, ResultBody, SessionCleanup},
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
    requests,
    security::ClientIp,
    state,
    worker_ws::RequestError,
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

/// The worker's service session takes no input and is neither stopped, archived nor
/// deleted (SPEC §10.2): `409 system_session`.
fn not_system(s: &SessionRow) -> ApiResult<()> {
    if s.system {
        return Err(ApiError::Conflict(
            "system_session",
            "this is the worker's service session: it takes no input and cannot be archived".into(),
        ));
    }
    Ok(())
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

/// The user's project; another user's looks like a missing one.
async fn project_for(app: &AppState, user: &CurrentUser, id: Uuid) -> ApiResult<db::ProjectRow> {
    match db::project(&app.db, id).await? {
        Some(p) if p.owner_id == user.id => Ok(p),
        _ => Err(ApiError::NotFound),
    }
}

pub async fn projects(
    State(app): State<AppState>,
    user: CurrentUser,
) -> ApiResult<Json<ProjectList>> {
    let rows = db::projects_of(&app.db, user.id).await?;
    Ok(Json(ProjectList {
        projects: rows.iter().map(db::ProjectRow::to_api).collect(),
    }))
}

pub async fn project(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<api::Project>> {
    Ok(Json(project_for(&app, &user, id).await?.to_api()))
}

const MAX_PROJECT_NAME: usize = 100;
const MAX_DESCRIPTION: usize = 2000;
const MAX_LABEL: usize = 100;

fn project_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("name must not be empty".into()));
    }
    if name.chars().count() > MAX_PROJECT_NAME {
        return Err(ApiError::BadRequest(format!(
            "name is longer than {MAX_PROJECT_NAME} characters"
        )));
    }
    Ok(hqc_proto::replace_nul(name).into_owned())
}

fn description(d: &str) -> ApiResult<String> {
    if d.chars().count() > MAX_DESCRIPTION {
        return Err(ApiError::BadRequest(format!(
            "description is longer than {MAX_DESCRIPTION} characters"
        )));
    }
    Ok(hqc_proto::replace_nul(d.trim()).into_owned())
}

/// `POST /api/projects` (SPEC §6): `201` and the project.
pub async fn create_project(
    State(app): State<AppState>,
    user: CurrentUser,
    Json(b): Json<CreateProject>,
) -> ApiResult<impl IntoResponse> {
    let name = project_name(&b.name)?;
    let desc = description(b.description.as_deref().unwrap_or(""))?;
    let p = db::create_project(&app.db, user.id, &name, &desc).await?;
    tracing::info!(project_id = %p.id, user = %user.id, "project created");
    Ok((StatusCode::CREATED, Json(p.to_api())))
}

/// `PATCH /api/projects/{id}`: name, description, default workspace. The personal project
/// keeps its name (SPEC §3 User).
pub async fn patch_project(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<PatchProject>,
) -> ApiResult<Json<api::Project>> {
    let p = project_for(&app, &user, id).await?;
    let name = b.name.as_deref().map(project_name).transpose()?;
    if p.is_personal && name.as_ref().is_some_and(|n| *n != p.name) {
        return Err(ApiError::Conflict(
            "personal_project",
            "the personal project cannot be renamed".into(),
        ));
    }
    let desc = b.description.as_deref().map(description).transpose()?;
    if let Some(ws) = b.default_workspace_id {
        if !db::workspace(&app.db, ws)
            .await?
            .is_some_and(|w| w.project_id == id)
        {
            return Err(ApiError::BadRequest(
                "default_workspace_id is not a workspace of this project".into(),
            ));
        }
    }
    let p = db::update_project(
        &app.db,
        id,
        name.as_deref(),
        desc.as_deref(),
        b.default_workspace_id,
    )
    .await?;
    Ok(Json(p.to_api()))
}

/// `DELETE /api/projects/{id}`: the project, its workspace records and its sessions; agents
/// of those sessions are killed, folders and transcripts on the workers stay (SPEC §13.2,
/// §17.4). The personal project is not deleted.
pub async fn delete_project(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let p = project_for(&app, &user, id).await?;
    if p.is_personal {
        return Err(ApiError::Conflict(
            "personal_project",
            "the personal project cannot be deleted".into(),
        ));
    }
    // The inbox folders of its sessions go too (T21); computed before the events are gone.
    let mut cleanups = vec![];
    for s in db::sessions_of_project(&app.db, id).await? {
        if let Some(f) = cleanup_frame(&app, &s).await? {
            cleanups.push((s.worker_id, f));
        }
    }
    let killed = db::delete_project(&app.db, id).await?;
    for (session_id, worker_id) in &killed {
        if let Some(conn) = app.workers.get(*worker_id) {
            conn.send(HubFrame::SessionKill {
                session_id: *session_id,
            });
        }
        app.session_changed(*session_id);
    }
    for (worker_id, f) in cleanups {
        if let Some(conn) = worker_id.and_then(|w| app.workers.get(w)) {
            conn.send(f);
        }
    }
    tracing::info!(project_id = %id, sessions = killed.len(), "project deleted");
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- workspaces

/// How long the hub waits for the worker's `result` to `fs.stat` / `fs.list_dirs`.
const WORKER_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const MAX_PATH: usize = 4096;

async fn workspace_for(
    app: &AppState,
    user: &CurrentUser,
    id: Uuid,
) -> ApiResult<db::WorkspaceRow> {
    match db::workspace(&app.db, id).await? {
        Some(w) if w.owner_id == user.id => Ok(w),
        _ => Err(ApiError::NotFound),
    }
}

/// The user's worker, not revoked.
async fn own_worker(app: &AppState, user: &CurrentUser, id: Uuid) -> ApiResult<db::WorkerRow> {
    match db::worker(&app.db, id).await? {
        Some(w) if w.owner_id == user.id && w.revoked_at.is_none() => Ok(w),
        _ => Err(ApiError::BadRequest("unknown worker".into())),
    }
}

/// Send a request frame to the online worker and wait for its result.
async fn ask_worker(
    app: &AppState,
    worker_id: Uuid,
    build: impl FnOnce(String) -> HubFrame,
) -> ApiResult<ResultBody> {
    let offline = || ApiError::Conflict("worker_offline", "the worker is offline".into());
    let conn = app.workers.get(worker_id).ok_or_else(offline)?;
    // A slow or hostile worker does not tie up the hub with waiting requests (SPEC §20).
    let Ok(_slot) = conn.fs_slots.try_acquire() else {
        return Err(ApiError::Limited(
            "worker_busy",
            "too many requests to this worker in flight, retry later".into(),
        ));
    };
    match conn.request(build, WORKER_REQUEST_TIMEOUT).await {
        Ok(body) => Ok(body),
        Err(RequestError::Offline) => Err(offline()),
        Err(RequestError::Timeout) => Err(ApiError::WorkerTimeout),
        Err(RequestError::Worker(e)) => Err(ApiError::Invalid("invalid_path", e.message)),
    }
}

fn check_path(p: &str) -> ApiResult<&str> {
    let p = p.trim();
    if p.is_empty() || p.len() > MAX_PATH || p.contains('\0') {
        return Err(ApiError::BadRequest(format!(
            "path must be 1..{MAX_PATH} bytes without NUL"
        )));
    }
    Ok(p)
}

#[derive(Debug, Deserialize)]
pub struct WorkspacesQuery {
    pub project_id: Option<Uuid>,
}

/// `GET /api/workspaces?project_id=`: the project's workspaces, or all of the user's.
pub async fn workspaces(
    State(app): State<AppState>,
    user: CurrentUser,
    Query(q): Query<WorkspacesQuery>,
) -> ApiResult<Json<WorkspaceList>> {
    let rows = match q.project_id {
        Some(p) => {
            project_for(&app, &user, p).await?;
            db::workspaces_of_project(&app.db, p).await?
        }
        None => db::workspaces_of_owner(&app.db, user.id).await?,
    };
    Ok(Json(WorkspaceList {
        workspaces: rows.iter().map(db::WorkspaceRow::to_api).collect(),
    }))
}

/// `POST /api/workspaces` (SPEC §6, §13.1, `kind: dir`): the worker checks the folder with
/// `fs.stat` and the hub stores the canonical path it reports. The worker must be online;
/// a path that equals, lies inside or contains a workspace of the same worker is
/// `409 workspace_conflict` (a folder belongs to one project, SPEC §3 Workspace).
pub async fn create_workspace(
    State(app): State<AppState>,
    user: CurrentUser,
    Json(body): Json<CreateWorkspaceImport>,
) -> ApiResult<impl IntoResponse> {
    let b = body.workspace;
    let project = project_for(&app, &user, b.project_id).await?;
    let worker = own_worker(&app, &user, b.worker_id).await?;
    let path = check_path(&b.path)?;
    let label = b.label.as_deref().unwrap_or("").trim();
    if label.chars().count() > MAX_LABEL {
        return Err(ApiError::BadRequest(format!(
            "label is longer than {MAX_LABEL} characters"
        )));
    }
    let request = path.to_string();
    let st = match ask_worker(&app, worker.id, |request_id| {
        HubFrame::FsStat(FsStat {
            request_id,
            path: request,
        })
    })
    .await?
    {
        ResultBody::FsStat(st) => st,
        other => {
            return Err(anyhow::anyhow!("fs.stat answered with {other:?}").into());
        }
    };
    if !st.inside_allowed_roots {
        return Err(ApiError::Invalid(
            "outside_allowed_roots",
            format!("{path} is outside the worker's allowed_roots"),
        ));
    }
    if !st.exists {
        return Err(ApiError::Invalid(
            "path_not_found",
            format!("{path} does not exist on the worker"),
        ));
    }
    if !st.is_dir {
        return Err(ApiError::Invalid(
            "not_a_directory",
            format!("{path} is not a directory"),
        ));
    }
    let canonical = st
        .canonical
        .ok_or_else(|| anyhow::anyhow!("fs.stat without canonical for an existing path"))?;
    let canonical = check_path(&canonical)?;
    // The overlap check compares components: they must be resolved already.
    if std::path::Path::new(canonical).components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        return Err(ApiError::Invalid(
            "invalid_path",
            format!("the worker reported {canonical}, which is not a canonical path"),
        ));
    }
    let r = db::create_workspace(
        &app.db,
        db::NewWorkspace {
            project_id: project.id,
            owner_id: user.id,
            worker_id: worker.id,
            path: canonical,
            label: &hqc_proto::replace_nul(label),
            mirror_enabled: body.mirror_enabled,
        },
    )
    .await?;
    let ws = match r {
        db::WorkspaceInsert::Created(ws) => ws,
        db::WorkspaceInsert::Conflict(x) => {
            let name = db::project(&app.db, x.project_id)
                .await?
                .map(|p| p.name)
                .unwrap_or_default();
            let how = if x.path == canonical {
                "is already"
            } else if std::path::Path::new(canonical).starts_with(&x.path) {
                "is inside"
            } else {
                "contains"
            };
            let mut extra = serde_json::Map::new();
            extra.insert("existing_workspace_id".into(), json!(x.id));
            extra.insert("existing_project_id".into(), json!(x.project_id));
            extra.insert("existing_path".into(), json!(x.path));
            return Err(ApiError::ConflictWith(
                "workspace_conflict",
                format!("{canonical} {how} workspace {} of project {name}", x.path),
                extra,
            ));
        }
    };
    // Mirrored by default (T21; the import of SPEC §9.1): the worker follows the folder's
    // transcripts and sends their history.
    if ws.mirror_enabled {
        if let Some(conn) = app.workers.get(ws.worker_id) {
            crate::mirror::enable(&app, &conn, &ws).await?;
        }
    }
    tracing::info!(workspace_id = %ws.id, project_id = %ws.project_id, worker_id = %ws.worker_id, path = %ws.path, mirror = ws.mirror_enabled, "workspace added");
    Ok((StatusCode::CREATED, Json(ws.to_api())))
}

pub async fn patch_workspace(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<PatchWorkspace>,
) -> ApiResult<Json<api::Workspace>> {
    let mut ws = workspace_for(&app, &user, id).await?;
    if let Some(label) = b.label.as_deref().map(str::trim) {
        if label.chars().count() > MAX_LABEL {
            return Err(ApiError::BadRequest(format!(
                "label is longer than {MAX_LABEL} characters"
            )));
        }
        ws = db::set_workspace_label(&app.db, id, &hqc_proto::replace_nul(label)).await?;
    }
    // The switch "VS Code sessions" (T21, SPEC §9.2). An offline worker learns it after `hello`.
    if let Some(on) = b.mirror_enabled.filter(|on| *on != ws.mirror_enabled) {
        ws = db::set_workspace_mirror(&app.db, id, on).await?;
        if let Some(conn) = app.workers.get(ws.worker_id) {
            if on {
                crate::mirror::enable(&app, &conn, &ws).await?;
            } else {
                crate::mirror::disable(&conn, &ws.path);
            }
        }
        tracing::info!(workspace_id = %id, path = %ws.path, mirror = on, "workspace mirror switched");
    }
    Ok(Json(ws.to_api()))
}

/// `DELETE /api/workspaces/{id}`: only the record (`kind: dir`, SPEC §13.2), the folder
/// stays. `409 workspace_in_use` while a session that is not archived runs there.
pub async fn delete_workspace(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let ws = workspace_for(&app, &user, id).await?;
    match db::delete_workspace(&app.db, &ws).await? {
        db::WorkspaceDelete::Deleted => {
            if ws.mirror_enabled {
                if let Some(conn) = app.workers.get(ws.worker_id) {
                    crate::mirror::disable(&conn, &ws.path);
                }
            }
            tracing::info!(workspace_id = %id, path = %ws.path, "workspace removed");
            Ok(StatusCode::NO_CONTENT)
        }
        db::WorkspaceDelete::InUse(n) => Err(ApiError::Conflict(
            "workspace_in_use",
            format!("{n} session(s) still work in this workspace; archive them first"),
        )),
    }
}

#[derive(Debug, Deserialize)]
pub struct DirsQuery {
    pub path: Option<String>,
}

/// `GET /api/workers/{id}/dirs?path=`: the worker's `fs.list_dirs` (SPEC §5.3, §6); no
/// `path` lists its `allowed_roots`.
pub async fn worker_dirs(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Query(q): Query<DirsQuery>,
) -> ApiResult<Json<DirList>> {
    let worker = match db::worker(&app.db, id).await? {
        Some(w) if w.owner_id == user.id && w.revoked_at.is_none() => w,
        _ => return Err(ApiError::NotFound),
    };
    let path = match q.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => Some(check_path(p)?.to_string()),
        None => None,
    };
    match ask_worker(&app, worker.id, |request_id| {
        HubFrame::FsListDirs(FsListDirs { request_id, path })
    })
    .await?
    {
        ResultBody::FsListDirs(l) => Ok(Json(l)),
        other => Err(anyhow::anyhow!("fs.list_dirs answered with {other:?}").into()),
    }
}

/// `GET /api/workers/{id}/candidates`: folders with Claude Code sessions on the worker
/// (`workspace.list_candidates`, SPEC §9.1), for the import.
pub async fn worker_candidates(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<CandidateList>> {
    let worker = match db::worker(&app.db, id).await? {
        Some(w) if w.owner_id == user.id && w.revoked_at.is_none() => w,
        _ => return Err(ApiError::NotFound),
    };
    match ask_worker(&app, worker.id, |request_id| {
        HubFrame::WorkspaceListCandidates(ListCandidates { request_id })
    })
    .await?
    {
        ResultBody::Candidates(c) => Ok(Json(c)),
        other => Err(anyhow::anyhow!("workspace.list_candidates answered with {other:?}").into()),
    }
}

/// `POST /api/sessions/{id}/takeover` (SPEC §9.3): continue a session VS Code drove.
/// `409 vscode_open` while a process outside hqc holds it open, `409 vscode_active` while
/// VS Code wrote within 30 s (only without registry data), `409 worker_offline`,
/// `409 system_session` for the worker's service session.
pub async fn takeover(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Write).await?;
    not_system(&s)?;
    crate::mirror::takeover(&app, &s).await?;
    let s = db::session(&app.db, id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(json!(state::view(&app, user.id, s).await?)))
}

// ---------------------------------------------------------------- sessions

pub async fn project_sessions(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<SessionList>> {
    match db::project(&app.db, id).await? {
        Some(p) if p.owner_id == user.id => {}
        _ => return Err(ApiError::NotFound),
    }
    let rows = db::sessions_of_project(&app.db, id).await?;
    Ok(Json(SessionList {
        sessions: state::views(&app, user.id, rows).await?,
    }))
}

/// `cwd` is relative to the workspace (SPEC §3 Session); the worker checks it again
/// against its own config (SPEC §2.1).
fn valid_cwd(cwd: &str) -> bool {
    !cwd.starts_with('/') && !cwd.contains('\\') && !cwd.split('/').any(|c| c == "..")
}

pub const MAX_TITLE: usize = 200;
const MAX_CLIENT_OP_ID: usize = 128;

/// `^[A-Za-z0-9._:-]{1,64}$`, not starting with `-`: the model goes to the agent's command
/// line (SPEC §20).
pub fn valid_model(m: &str) -> bool {
    (1..=64).contains(&m.len())
        && !m.starts_with('-')
        && m.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b':' | b'-'))
}

fn check_title(t: &str) -> ApiResult<()> {
    if t.chars().count() > MAX_TITLE {
        return Err(ApiError::BadRequest(format!(
            "title is longer than {MAX_TITLE} characters"
        )));
    }
    Ok(())
}

/// The workspace of a new session (SPEC §6, §13.4): the one asked for; with only
/// `worker_id`, the project's workspace on that worker (the default one if it is there,
/// else the oldest); otherwise the project's `default_workspace_id`.
async fn session_workspace(
    app: &AppState,
    project: &db::ProjectRow,
    b: &CreateSessionRequest,
) -> ApiResult<db::WorkspaceRow> {
    let none = || {
        ApiError::Conflict(
            "no_workspace",
            "the project has no workspace; add one first".into(),
        )
    };
    if let Some(id) = b.workspace_id {
        return match db::workspace(&app.db, id).await? {
            Some(w)
                if w.project_id == project.id
                    && b.worker_id.is_none_or(|wid| wid == w.worker_id) =>
            {
                Ok(w)
            }
            _ => Err(ApiError::BadRequest(
                "workspace_id is not a workspace of this project on this worker".into(),
            )),
        };
    }
    let all = db::workspaces_of_project(&app.db, project.id).await?;
    let mut on: Vec<_> = all
        .into_iter()
        .filter(|w| b.worker_id.is_none_or(|wid| wid == w.worker_id))
        .collect();
    if let Some(i) = on
        .iter()
        .position(|w| Some(w.id) == project.default_workspace_id)
    {
        return Ok(on.swap_remove(i));
    }
    if b.worker_id.is_some() {
        return on.into_iter().next().ok_or_else(none);
    }
    Err(none())
}

pub async fn create_session(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(b): Json<CreateSessionRequest>,
) -> ApiResult<impl IntoResponse> {
    let project = project_for(&app, &user, project_id).await?;
    if let Some(m) = &b.model {
        if !valid_model(m) {
            return Err(ApiError::BadRequest(
                "model must be 1..64 characters of A-Z a-z 0-9 . _ : -".into(),
            ));
        }
    }
    let ws = session_workspace(&app, &project, &b).await?;
    let worker = own_worker(&app, &user, ws.worker_id).await?;
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
    // No title: "Session N" (SPEC §3 Session), which the runtime's title may replace.
    let title = b.title.as_deref().map(str::trim).filter(|t| !t.is_empty());
    if let Some(t) = title {
        check_title(t)?;
    }
    let title = title.map(hqc_proto::replace_nul);
    let id = db::create_session(
        &app.db,
        db::NewSession {
            project_id,
            owner_id: user.id,
            workspace_id: Some(ws.id),
            worker_id: Some(worker.id),
            cwd,
            title: title.as_deref(),
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
        check_title(t)?;
        // The slug stays: it is the session's address (SPEC §7).
        db::set_user_title(&app.db, id, &hqc_proto::replace_nul(t)).await?;
    }
    if let Some(a) = b.auto_approve {
        let owner = match s.worker_id {
            Some(w) => db::worker(&app.db, w).await?.map(|w| w.owner_id),
            None => None,
        };
        if a && (owner != Some(user.id) || s.system) {
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
    not_system(&s)?;
    let cleanup = cleanup_frame(&app, &s).await?;
    send_to_worker(&app, &s, HubFrame::SessionKill { session_id: id });
    if let Some(f) = cleanup {
        send_to_worker(&app, &s, f);
    }
    sqlx::query("DELETE FROM sessions WHERE id = $1")
        .bind(id)
        .execute(&app.db)
        .await?;
    app.session_changed(id);
    Ok(StatusCode::NO_CONTENT)
}

/// `session.cleanup` for an archived or deleted session (T21, SPEC §5.3, §17.4): its inbox
/// folders, or the scratch folder of a session without workspace. `None` when there is
/// nothing on a worker to clean. A worker that is offline misses it; its own sweep of old
/// inbox folders at start covers that.
async fn cleanup_frame(app: &AppState, s: &SessionRow) -> anyhow::Result<Option<HubFrame>> {
    if s.system || s.worker_id.is_none() {
        return Ok(None);
    }
    let workspace_path = match s.workspace_id {
        Some(w) => match db::workspace(&app.db, w).await? {
            Some(ws) => Some(ws.path),
            None => return Ok(None),
        },
        None => None,
    };
    let event_ids = db::inbox_events(&app.db, s.id).await?;
    if workspace_path.is_some() && event_ids.is_empty() {
        return Ok(None);
    }
    Ok(Some(HubFrame::SessionCleanup(SessionCleanup {
        session_id: s.id,
        workspace_path,
        cwd: s.cwd.clone(),
        event_ids,
    })))
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
) -> ApiResult<Json<EventPage>> {
    let s = session_for(&app, &user, id, Action::Read).await?;
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let rows = match q.before_seq {
        Some(b) => db::events_before(&app.db, id, b, limit).await?,
        None => db::events_after(&app.db, id, q.after_seq.unwrap_or(0), limit).await?,
    };
    Ok(Json(EventPage {
        events: rows.iter().map(|r| r.to_json()).collect(),
        last_seq: s.last_seq.max(0) as u64,
    }))
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    /// A project id or slug of the user's.
    pub project: Option<String>,
    /// `1` or `true`.
    pub fuzzy: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

/// `GET /api/search?q=&project=&fuzzy=&cursor=&limit=` (SPEC §15): the user's sessions
/// only; another user's project is `404`, as a missing one.
pub async fn search(
    State(app): State<AppState>,
    user: CurrentUser,
    Query(q): Query<SearchQuery>,
) -> ApiResult<Json<api::SearchPage>> {
    let project_id = match q.project.as_deref().filter(|p| !p.is_empty()) {
        Some(p) => Some(
            crate::search::project_of(&app.db, user.id, p)
                .await?
                .ok_or(ApiError::NotFound)?,
        ),
        None => None,
    };
    let query = crate::search::Query {
        text: q.q,
        project_id,
        fuzzy: matches!(q.fuzzy.as_deref(), Some("1" | "true")),
        cursor: q.cursor.filter(|c| !c.is_empty()),
        limit: q.limit.unwrap_or(crate::search::PAGE),
    };
    match crate::search::search(&app.db, user.id, &query).await {
        Ok(page) => Ok(Json(page)),
        Err(crate::search::SearchError::Invalid(m)) => Err(ApiError::BadRequest(m)),
        Err(crate::search::SearchError::Db(e)) => Err(e.into()),
    }
}

/// `POST /api/sessions/{id}/input`: stored as `user.message` with `delivery: pending`
/// and delivered when the worker is there (SPEC §5.2). A repeated `client_op_id` returns
/// the event stored the first time, with 200 instead of 201.
pub async fn input(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<InputRequest>,
) -> ApiResult<(StatusCode, Json<InputResponse>)> {
    let reply = |status, r: &db::EventRow| (status, Json(InputResponse { event: r.to_json() }));
    let s = session_for(&app, &user, id, Action::Write).await?;
    not_system(&s)?;
    if s.archived {
        return Err(ApiError::Conflict(
            "archived",
            "the session is archived".into(),
        ));
    }
    if s.locked_by == "vscode" {
        // No silent queue (SPEC §9.3): the client offers "Take over", or asks to close the
        // session in VS Code first while it holds it.
        let mut extra = serde_json::Map::new();
        extra.insert("hint".into(), json!("takeover"));
        if let Some(h) = &s.held_by {
            extra.insert("held_by".into(), h.clone());
        }
        return Err(ApiError::ConflictWith(
            "locked_vscode",
            "the session is being driven from VS Code; take it over first (POST /takeover)".into(),
            extra,
        ));
    }
    if b.text.trim().is_empty() && b.artifact_ids.is_empty() {
        return Err(ApiError::BadRequest(
            "text or artifact_ids is required".into(),
        ));
    }
    let op = b.client_op_id.filter(|c| !c.is_empty());
    if op.as_ref().is_some_and(|o| o.len() > MAX_CLIENT_OP_ID) {
        return Err(ApiError::BadRequest(format!(
            "client_op_id is longer than {MAX_CLIENT_OP_ID} bytes"
        )));
    }
    if let Some(op) = &op {
        if let Some(r) = db::event_by_client_op(&app.db, id, op).await? {
            return Ok(reply(StatusCode::OK, &r));
        }
    }
    // Only the user's own artifacts; the session's worker may then download them (SPEC §12).
    crate::artifacts::attach_to_session(&app.db, user.id, id, &b.artifact_ids).await?;
    // A failed turn shows `error` until the next input (SPEC §17.1).
    db::clear_turn_error(&app.db, id).await?;
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
                    return Ok(reply(StatusCode::OK, &r));
                }
            }
            return Err(e.into());
        }
    };
    delivery::kick(&app, id, false).await;
    Ok(reply(StatusCode::CREATED, &row))
}

/// Abort the current turn; open requests are cancelled with `stopped`.
pub async fn stop(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Write).await?;
    not_system(&s)?;
    requests::cancel_open_of_session(&app, id, CancelReason::Stopped).await?;
    let sent = send_to_worker(&app, &s, HubFrame::SessionStop { session_id: id });
    Ok(Json(json!({ "sent": sent })))
}

pub async fn retry(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    not_system(&session_for(&app, &user, id, Action::Write).await?)?;
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
    not_system(&s)?;
    sqlx::query(
        "UPDATE sessions SET archived = true, turn_open = false, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&app.db)
    .await?;
    send_to_worker(&app, &s, HubFrame::SessionKill { session_id: id });
    if let Some(f) = cleanup_frame(&app, &s).await? {
        send_to_worker(&app, &s, f);
    }
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
    let s = session_for(&app, &user, id, Action::Manage).await?;
    if s.workspace_id.is_none() {
        // Its workspace record was removed while it was archived (SPEC §13.2).
        return Err(ApiError::Conflict(
            "no_workspace",
            "the session's workspace was removed".into(),
        ));
    }
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

#[derive(Debug, Deserialize)]
pub struct WorkersQuery {
    /// `1`: revoked workers too, with `status: revoked` (the settings screen, SPEC §6).
    #[serde(default)]
    pub include_revoked: Option<String>,
}

pub async fn workers(
    State(app): State<AppState>,
    user: CurrentUser,
    Query(q): Query<WorkersQuery>,
) -> ApiResult<Json<Value>> {
    let rows = if q.include_revoked.as_deref() == Some("1") {
        db::all_workers_of(&app.db, user.id).await?
    } else {
        db::workers_of(&app.db, user.id).await?
    };
    let ws: Vec<Value> = rows
        .into_iter()
        .map(|w| {
            json!({
                "id": w.id,
                "name": w.name,
                "os": w.os,
                "status": worker_status(&app, &w),
                "revoked_at": w.revoked_at,
                "last_seen": w.last_seen,
                "protocol_version": w.protocol_version,
                "capabilities": w.capabilities,
                "environments": w.environments,
                "env_bindings": w.env_bindings,
                "allowed_roots": w.allowed_roots,
                "allow_foreign_sessions": w.allow_foreign_sessions,
                "share_local_models": w.share_local_models,
                "local_models": w.local_models,
                "system_session_id": w.system_session_id,
                "created_at": w.created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "workers": ws })))
}

/// `online` | `offline` | `revoked`.
fn worker_status(app: &AppState, w: &db::WorkerRow) -> &'static str {
    if w.revoked_at.is_some() {
        "revoked"
    } else if app.workers.is_online(w.id) {
        "online"
    } else {
        "offline"
    }
}

/// Lookups of enrollment codes per user and minute (info and confirm together): codes are
/// short, guessing them must stay slow.
const ENROLL_LOOKUPS_PER_MIN: usize = 30;

fn limit_enroll_lookups(app: &AppState, user: &CurrentUser) -> ApiResult<()> {
    if app.limiter.allow(
        "enroll_lookup",
        &user.id.to_string(),
        ENROLL_LOOKUPS_PER_MIN,
    ) {
        Ok(())
    } else {
        Err(ApiError::TooManyRequests)
    }
}

/// `GET /api/workers/enroll/{code}`: who is asking to be enrolled, shown before the user
/// confirms (SPEC §5.1, §6).
pub async fn enroll_info(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(code): Path<String>,
) -> ApiResult<Json<EnrollInfo>> {
    limit_enroll_lookups(&app, &user)?;
    let (mut info, machine) = db::enroll_info(&app.db, &normalize_enroll_code(&code))
        .await?
        .ok_or(ApiError::NotFound)?;
    info.replaceable = db::replace_candidates(&app.db, user.id)
        .await?
        .into_iter()
        .filter(|(w, _)| worker_status(&app, w) != "online")
        .map(|(w, sessions_count)| api::ReplaceableWorker {
            status: worker_status(&app, &w).into(),
            same_machine: machine.is_some() && w.machine_hash == machine,
            id: w.id,
            name: w.name,
            os: w.os,
            last_seen: w.last_seen,
            sessions_count,
        })
        .collect();
    // Workers of the same machine first (T21, SPEC §5.1); the sort is stable.
    info.replaceable.sort_by_key(|w| !w.same_machine);
    Ok(Json(info))
}

/// `POST /api/workers/enroll/confirm`. With `replace_worker_id` the enrolling worker takes
/// over the user's offline or revoked worker: same id, sessions and workspaces; its old
/// token stops working (SPEC §5.1).
pub async fn enroll_confirm(
    State(app): State<AppState>,
    user: CurrentUser,
    Json(b): Json<EnrollConfirmRequest>,
) -> ApiResult<Json<EnrollConfirmed>> {
    limit_enroll_lookups(&app, &user)?;
    let code = normalize_enroll_code(&b.code);
    let name = b.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    // A revoked worker's connection is already being closed.
    let target = match b.replace_worker_id {
        Some(id) => db::worker(&app.db, id).await?,
        None => None,
    };
    if target.is_some_and(|w| worker_status(&app, &w) == "online") {
        return Err(ApiError::Conflict(
            "worker_online",
            "the worker is online: only an offline or revoked worker can be replaced".into(),
        ));
    }
    let (worker_id, name, replaced) =
        match db::enroll_confirm(&app.db, &code, user.id, name, b.replace_worker_id).await? {
            db::EnrollConfirm::NoCode => return Err(ApiError::NotFound),
            db::EnrollConfirm::UnknownWorker => {
                return Err(ApiError::BadRequest("unknown worker".into()))
            }
            db::EnrollConfirm::ReplaceBusy => {
                return Err(ApiError::Conflict(
                    "conflict",
                    "another confirmed enrollment is already replacing this worker".into(),
                ))
            }
            db::EnrollConfirm::Confirmed {
                worker_id,
                name,
                replaced,
            } => (worker_id, name, replaced),
        };
    let Some((old_name, system)) = replaced else {
        tracing::info!(%worker_id, user = %user.id, "worker enrollment confirmed");
        return Ok(Json(EnrollConfirmed {
            worker_id,
            name,
            replaced: None,
        }));
    };
    // A connection that got in before the token was cleared belongs to the old process.
    if let Some(c) = app.workers.get(worker_id) {
        c.close();
    }
    let system = match system {
        Some(id) => db::session(&app.db, id).await?,
        None => None,
    };
    if let Some(s) = system {
        events::store_hub_event(
            &app,
            &s,
            json!({"kind": "system"}),
            "system",
            json!({
                "text": format!("Worker re-enrolled: “{old_name}” → “{name}”"),
                "code": "worker_replaced",
                "old_name": old_name,
                "new_name": name,
            }),
            None,
            None,
        )
        .await?;
        app.session_changed(s.id);
    }
    tracing::info!(%worker_id, user = %user.id, %old_name, "worker enrollment confirmed as a replacement");
    Ok(Json(EnrollConfirmed {
        worker_id,
        name,
        replaced: Some(old_name),
    }))
}

#[derive(Debug, Deserialize)]
pub struct DeleteWorkerQuery {
    /// `1`: delete a revoked worker instead of revoking.
    #[serde(default)]
    pub purge: Option<String>,
}

/// `DELETE /api/workers/{id}`: revoke the worker's token, drop its connection and archive
/// its service session (SPEC §5.1, §10.2). `?purge=1`: delete a revoked worker (SPEC §6).
pub async fn delete_worker(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Query(q): Query<DeleteWorkerQuery>,
) -> ApiResult<axum::response::Response> {
    if q.purge.as_deref() == Some("1") {
        return purge_worker(&app, &user, id).await;
    }
    if !db::revoke_worker(&app.db, id, user.id).await? {
        return Err(ApiError::NotFound);
    }
    if let Some(c) = app.workers.get(id) {
        c.close();
    }
    let system = match db::worker(&app.db, id)
        .await?
        .and_then(|w| w.system_session_id)
    {
        Some(sid) => db::session(&app.db, sid).await?,
        None => None,
    };
    if let Some(s) = system {
        events::system(
            &app,
            &s,
            "worker_revoked",
            &format!("Token of worker “{}” revoked", s.title),
        )
        .await?;
        db::archive_system_session(&app.db, s.id).await?;
        // The process that asked is cut off: nobody is left to deliver an answer to.
        for p in db::open_requests_of_session(&app.db, s.id).await? {
            requests::cancel_without_frame(&app, &p, CancelReason::Archived).await?;
        }
        app.session_changed(s.id);
    }
    tracing::info!(worker_id = %id, "worker revoked");
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Delete a revoked worker with its service session and workspace records; `409
/// worker_has_sessions` with their number while sessions that are not archived work there.
async fn purge_worker(
    app: &AppState,
    user: &CurrentUser,
    id: Uuid,
) -> ApiResult<axum::response::Response> {
    let w = match db::worker(&app.db, id).await? {
        Some(w) if w.owner_id == user.id => w,
        _ => return Err(ApiError::NotFound),
    };
    if w.revoked_at.is_none() {
        return Err(ApiError::Conflict(
            "worker_not_revoked",
            "revoke the worker before deleting it".into(),
        ));
    }
    match db::purge_worker(&app.db, id).await? {
        db::WorkerPurge::HasSessions(n) => Ok((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "worker_has_sessions",
                "message": format!(
                    "{n} session(s) still work on this worker: move them by enrolling the machine \
                     again as its replacement, or archive them"
                ),
                "sessions_count": n,
            })),
        )
            .into_response()),
        db::WorkerPurge::Deleted => {
            if let Some(sid) = w.system_session_id {
                app.session_changed(sid);
            }
            tracing::info!(worker_id = %id, "revoked worker deleted");
            Ok(StatusCode::NO_CONTENT.into_response())
        }
    }
}

/// `POST /worker/enroll` (no auth): a code for the user to confirm and a secret to poll
/// with. Limited per client address and in total (SPEC §20). `protocol_version` in the body
/// is informational; unknown fields are ignored (SPEC §5.1). `CF-IPCountry` is kept for the
/// confirmation screen.
pub async fn worker_enroll(
    State(app): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Json(b): Json<EnrollRequest>,
) -> ApiResult<Json<EnrollStarted>> {
    let limits = &app.config.limits;
    if !app.limiter.allow("enroll", &ip, limits.enroll_per_min) {
        return Err(ApiError::TooManyRequests);
    }
    let (total, from_ip) = db::enroll_active_count(&app.db, &ip).await?;
    if total >= limits.enroll_active_total || from_ip >= limits.enroll_active_per_ip {
        return Err(ApiError::TooManyRequests);
    }
    let country = headers
        .get("cf-ipcountry")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_alphanumeric()))
        .map(str::to_ascii_uppercase);
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
        db::NewEnroll {
            code: &code,
            poll_token_hash: &hash_secret(&poll_token),
            name: &name,
            os: &os,
            expires_at,
            country: country.as_deref(),
            client_ip: &ip,
            machine_hash: b
                .machine_id
                .as_deref()
                .and_then(db::machine_hash)
                .as_deref(),
        },
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
    ClientIp(ip): ClientIp,
    Json(b): Json<EnrollPollRequest>,
) -> ApiResult<Json<api::EnrollPoll>> {
    if !app
        .limiter
        .allow("enroll_poll", &ip, app.config.limits.poll_per_min)
    {
        return Err(ApiError::TooManyRequests);
    }
    let token = new_secret("hqcw_");
    match db::enroll_poll(&app.db, &hash_secret(&b.poll_token), &hash_secret(&token)).await? {
        EnrollPoll::Pending => Ok(Json(api::EnrollPoll::Pending)),
        EnrollPoll::Confirmed {
            worker_id,
            replaced,
        } => {
            tracing::info!(%worker_id, replaced = replaced.is_some(), "worker token issued");
            Ok(Json(api::EnrollPoll::Confirmed {
                worker_id,
                token,
                replaced,
            }))
        }
        EnrollPoll::Gone => Ok(Json(api::EnrollPoll::Expired)),
    }
}

// ---------------------------------------------------------------- push

pub async fn vapid(State(app): State<AppState>, _user: CurrentUser) -> Json<VapidKey> {
    Json(VapidKey {
        public_key: app.push.public_key(),
    })
}

/// The browser's `PushSubscription.toJSON()`.
pub async fn push_subscribe(
    State(app): State<AppState>,
    user: CurrentUser,
    Json(b): Json<PushSubscription>,
) -> ApiResult<impl IntoResponse> {
    if !push_endpoint_allowed(&b.endpoint) {
        return Err(ApiError::BadRequest(
            "endpoint is not a known push service".into(),
        ));
    }
    let r = db::upsert_push_sub(
        &app.db,
        user.id,
        &b.endpoint,
        &b.keys.p256dh,
        &b.keys.auth,
        app.config.limits.push_subs_per_user,
    )
    .await?;
    match r {
        db::PushUpsert::Stored(id) => Ok((StatusCode::CREATED, Json(json!({ "id": id })))),
        db::PushUpsert::Foreign => Err(ApiError::Conflict(
            "conflict",
            "this push endpoint belongs to another user".into(),
        )),
        db::PushUpsert::Limit => Err(ApiError::Conflict(
            "push_limit",
            format!(
                "at most {} push subscriptions per user; remove one first",
                app.config.limits.push_subs_per_user
            ),
        )),
    }
}

/// Push services of the browsers (SPEC §20): the hub POSTs to the endpoint, so anything else
/// would let a user point it into the hub's network.
const PUSH_HOSTS: &[&str] = &["fcm.googleapis.com", "updates.push.services.mozilla.com"];
const PUSH_HOST_SUFFIXES: &[&str] = &[
    ".push.apple.com",
    ".notify.windows.com",
    ".push.services.mozilla.com",
];

pub fn push_endpoint_allowed(endpoint: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(endpoint) else {
        return false;
    };
    let Some(host) = u.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    u.scheme() == "https"
        && u.username().is_empty()
        && u.password().is_none()
        && u.port().is_none()
        && (PUSH_HOSTS.contains(&host.as_str())
            || PUSH_HOST_SUFFIXES.iter().any(|s| host.ends_with(s)))
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
    use super::{push_endpoint_allowed, valid_cwd, valid_model};

    #[test]
    fn push_endpoints() {
        for ok in [
            "https://fcm.googleapis.com/fcm/send/abc",
            "https://web.push.apple.com/QGx",
            "https://updates.push.services.mozilla.com/wpush/v2/x",
            "https://wns2-db5p.notify.windows.com/w/?token=x",
        ] {
            assert!(push_endpoint_allowed(ok), "{ok}");
        }
        for bad in [
            "http://fcm.googleapis.com/fcm/send/abc",
            "https://evil.example/fcm.googleapis.com",
            "https://fcm.googleapis.com.evil.example/x",
            "https://push.apple.com.evil/x",
            "https://fcm.googleapis.com:8443/x",
            "https://user@fcm.googleapis.com/x",
            "https://169.254.169.254/latest",
            "https://hqc-db:5432/",
            "not a url",
        ] {
            assert!(!push_endpoint_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn models() {
        for ok in ["claude-haiku-5-5", "opus", "qwen3.8-27b:q4_K", "a.b_c"] {
            assert!(valid_model(ok), "{ok}");
        }
        for bad in ["", "-x y", "a b", "a;b", "--resume", &"x".repeat(65), "мод"] {
            assert!(!valid_model(bad), "{bad}");
        }
    }

    #[test]
    fn overlap_is_by_components() {
        use crate::db::paths_overlap;
        assert!(paths_overlap("/w/hqc", "/w/hqc"));
        assert!(paths_overlap("/w/hqc", "/w/hqc/crates"));
        assert!(paths_overlap("/w", "/w/hqc"));
        assert!(paths_overlap("/", "/w"));
        assert!(!paths_overlap("/w/hqc", "/w/hqc2"));
        assert!(!paths_overlap("/w/a", "/w/b"));
    }

    #[test]
    fn cwd_must_stay_inside() {
        assert!(valid_cwd("."));
        assert!(valid_cwd("crates/hqc-hub"));
        assert!(!valid_cwd("/etc"));
        assert!(!valid_cwd("../x"));
        assert!(!valid_cwd("a/../../b"));
    }
}
