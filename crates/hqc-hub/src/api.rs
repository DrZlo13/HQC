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
        self, CandidateList, CreateProject, CreateRoomRequest, CreateSessionRequest,
        CreateWorkspaceImport, DirList, EnrollConfirmRequest, EnrollConfirmed, EnrollInfo,
        EnrollPollRequest, EnrollRequest, EnrollStarted, EventPage, InputRequest, InputResponse,
        PatchProject, PatchWorkspace, ProjectList, PushSubscription, PutSessionOrder, SessionList,
        SessionOrder, SessionOrderMode, VapidKey, WorkspaceList, WorkspaceState,
    },
    event::CancelReason,
    frame::{
        error_code, FsListDirs, FsStat, FsStatResult, HubFrame, InputRef, ListCandidates,
        PermissionMode, ResultBody, SessionCleanup, SubagentHistory,
    },
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
    worker_ws::{RequestError, StartState},
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
pub(crate) fn not_system(s: &SessionRow) -> ApiResult<()> {
    if s.system {
        return Err(ApiError::Conflict(
            "system_session",
            "this is the worker's service session: it takes no input and cannot be archived".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn session_for(
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

pub async fn me(State(app): State<AppState>, user: CurrentUser) -> Json<api::Me> {
    Json(api::Me {
        id: user.id,
        email: user.email,
        display_name: user.display_name,
        timezone: user.timezone,
        personal_project_id: user.personal_project_id,
        theme: user.theme,
        look: user.look,
        project_sort: user.project_sort,
        new_session_project_id: user.new_session_project_id,
        limit_notice_percent: app.config.limits.limit_notice_percent,
    })
}

/// `PATCH /api/me` {timezone?, theme?, look?, project_sort?, new_session_project_id?}: an IANA
/// name, the default zone of new schedules (SPEC §16); the app's base palette (T49); the client's
/// look and the project bar's order; the user's own project a new session is quick-started in,
/// another user's or a missing one `404` (SPEC §3 User).
pub async fn patch_me(
    State(app): State<AppState>,
    mut user: CurrentUser,
    Json(b): Json<api::PatchMe>,
) -> ApiResult<Json<api::Me>> {
    // Checked before anything is stored: a refused project leaves the other fields as they were.
    let new_session_project = match b.new_session_project_id {
        Some(id) => Some(project_for(&app, &user, id).await?.id),
        None => None,
    };
    if let Some(tz) = b.timezone {
        let tz = crate::schedules::parse_tz(&tz).map_err(ApiError::BadRequest)?;
        sqlx::query("UPDATE users SET timezone = $2, updated_at = now() WHERE id = $1")
            .bind(user.id)
            .bind(tz.name())
            .execute(&app.db)
            .await?;
        user.timezone = tz.name().to_string();
    }
    if let Some(theme) = b.theme {
        sqlx::query("UPDATE users SET theme = $2, updated_at = now() WHERE id = $1")
            .bind(user.id)
            .bind(theme.as_str())
            .execute(&app.db)
            .await?;
        user.theme = theme;
    }
    if let Some(look) = b.look {
        sqlx::query("UPDATE users SET look = $2, updated_at = now() WHERE id = $1")
            .bind(user.id)
            .bind(look.as_str())
            .execute(&app.db)
            .await?;
        user.look = look;
    }
    if let Some(sort) = b.project_sort {
        sqlx::query("UPDATE users SET project_sort = $2, updated_at = now() WHERE id = $1")
            .bind(user.id)
            .bind(sort.as_str())
            .execute(&app.db)
            .await?;
        user.project_sort = sort;
    }
    if let Some(id) = new_session_project {
        sqlx::query(
            "UPDATE users SET new_session_project_id = $2, updated_at = now() WHERE id = $1",
        )
        .bind(user.id)
        .bind(id)
        .execute(&app.db)
        .await?;
        user.new_session_project_id = Some(id);
    }
    Ok(me(State(app), user).await)
}

/// The user's project; another user's looks like a missing one.
pub(crate) async fn project_for(
    app: &AppState,
    user: &CurrentUser,
    id: Uuid,
) -> ApiResult<db::ProjectRow> {
    match db::project(&app.db, id).await? {
        Some(p) if p.owner_id == user.id => Ok(p),
        _ => Err(ApiError::NotFound),
    }
}

/// The project as `user` gets it: `favorite_at` is the user's own.
pub(crate) async fn project_view(
    app: &AppState,
    user: &CurrentUser,
    p: &db::ProjectRow,
) -> ApiResult<api::Project> {
    let fav = db::project_favorites(&app.db, user.id, &[p.id]).await?;
    Ok(p.to_api(fav.first().map(|(_, at)| *at)))
}

#[derive(Debug, Deserialize)]
pub struct ProjectsQuery {
    /// `1`: the archived projects instead of the others (the client's project search).
    #[serde(default)]
    pub archived: Option<String>,
}

/// `GET /api/projects`: the projects that are not archived; `?archived=1` the archived ones.
pub async fn projects(
    State(app): State<AppState>,
    user: CurrentUser,
    Query(q): Query<ProjectsQuery>,
) -> ApiResult<Json<ProjectList>> {
    project_list(&app, &user, q.archived.as_deref() == Some("1")).await
}

/// The user's projects, archived or not, in the user's order, each with its `favorite_at`.
async fn project_list(
    app: &AppState,
    user: &CurrentUser,
    archived: bool,
) -> ApiResult<Json<ProjectList>> {
    let rows: Vec<db::ProjectRow> = db::projects_of(&app.db, user.id)
        .await?
        .into_iter()
        .filter(|p| p.archived_at.is_some() == archived)
        .collect();
    let ids: Vec<Uuid> = rows.iter().map(|p| p.id).collect();
    let fav = db::project_favorites(&app.db, user.id, &ids).await?;
    let at = |id: Uuid| fav.iter().find(|(p, _)| *p == id).map(|(_, at)| *at);
    Ok(Json(ProjectList {
        projects: rows.iter().map(|p| p.to_api(at(p.id))).collect(),
    }))
}

/// `PUT /api/me/project_order` {project_ids} (SPEC §6, since 0.64): the order of the project
/// bar; the reply is the list in it. The bar has no archived projects, so the archived ones
/// `project_ids` leaves out keep the place they had in the order the bar had (`keep_archived`;
/// the stored order, the projects missing from it after it): restored, a project comes back
/// where it was.
pub async fn put_project_order(
    State(app): State<AppState>,
    user: CurrentUser,
    Json(b): Json<api::PutProjectOrder>,
) -> ApiResult<Json<ProjectList>> {
    let all = db::projects_of(&app.db, user.id).await?;
    let old: Vec<Uuid> = all.iter().map(|p| p.id).collect();
    let archived: Vec<Uuid> = all
        .iter()
        .filter(|p| p.archived_at.is_some())
        .map(|p| p.id)
        .collect();
    let ids = keep_archived(&old, &b.project_ids, &archived);
    db::put_project_order(&app.db, user.id, &ids).await?;
    project_list(&app, &user, false).await
}

/// `new` with the `archived` projects of `old` it leaves out put back, in the order of `old`:
/// each right after the project before it in `old` when that one is in the list, else at its
/// index in `old` (at most the end).
fn keep_archived(old: &[Uuid], new: &[Uuid], archived: &[Uuid]) -> Vec<Uuid> {
    let mut out = new.to_vec();
    for (i, id) in old.iter().enumerate() {
        if !archived.contains(id) || out.contains(id) {
            continue;
        }
        let after = i
            .checked_sub(1)
            .and_then(|j| out.iter().position(|x| *x == old[j]));
        let at = after.map_or(i.min(out.len()), |k| k + 1);
        out.insert(at, *id);
    }
    out
}

pub async fn project(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<api::Project>> {
    let p = project_for(&app, &user, id).await?;
    Ok(Json(project_view(&app, &user, &p).await?))
}

pub(crate) const MAX_PROJECT_NAME: usize = 100;
const MAX_DESCRIPTION: usize = 2000;
const MAX_QUICK_MESSAGES: usize = 100;
const MAX_QUICK_MESSAGE: usize = 10_000;
const MAX_LABEL: usize = 100;

pub(crate) fn project_name(name: &str) -> ApiResult<String> {
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

fn quick_messages(list: &[String]) -> ApiResult<Vec<String>> {
    if list.len() > MAX_QUICK_MESSAGES {
        return Err(ApiError::BadRequest(format!(
            "more than {MAX_QUICK_MESSAGES} quick messages"
        )));
    }
    list.iter()
        .map(|m| {
            if m.trim().is_empty() {
                return Err(ApiError::BadRequest("a quick message is blank".into()));
            }
            if m.chars().count() > MAX_QUICK_MESSAGE {
                return Err(ApiError::BadRequest(format!(
                    "a quick message is longer than {MAX_QUICK_MESSAGE} characters"
                )));
            }
            Ok(hqc_proto::replace_nul(m).into_owned())
        })
        .collect()
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
    Ok((StatusCode::CREATED, Json(p.to_api(None))))
}

/// `PATCH /api/projects/{id}`: name, description, default workspace, color, icon (T49) and quick
/// messages.
/// The personal project keeps its name (SPEC §3 User); its appearance changes like any.
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
    let icon = b.icon.map(crate::icons::stored_icon).transpose()?;
    let quick = b
        .quick_messages
        .as_deref()
        .map(quick_messages)
        .transpose()?;
    let p = db::update_project(
        &app.db,
        id,
        db::ProjectUpdate {
            name: name.as_deref(),
            description: desc.as_deref(),
            default_workspace_id: b.default_workspace_id,
            color: b.color,
            icon,
            quick_messages: quick.as_deref(),
        },
    )
    .await?;
    Ok(Json(project_view(&app, &user, &p).await?))
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
        app.drop_live_agents(*session_id);
        app.token_rate_changed(*session_id, 0);
        app.drop_foreground_calls(*session_id);
        app.drop_activity(*session_id);
        // As for one session deleted: its text or thinking being written goes with it.
        app.end_live_text(*session_id);
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

/// `POST /api/projects/{id}/archive` (SPEC §3 Project): off the project list, its sessions as
/// they are; again keeps the first time. The personal project is not archived.
pub async fn archive_project(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<api::Project>> {
    let p = project_for(&app, &user, id).await?;
    if p.is_personal {
        return Err(ApiError::Conflict(
            "personal_project",
            "the personal project cannot be archived".into(),
        ));
    }
    let p = db::set_project_archived(&app.db, id, true).await?;
    Ok(Json(project_view(&app, &user, &p).await?))
}

/// `POST /api/projects/{id}/unarchive`: back in the project list, in its place of the order.
pub async fn unarchive_project(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<api::Project>> {
    project_for(&app, &user, id).await?;
    let p = db::set_project_archived(&app.db, id, false).await?;
    Ok(Json(project_view(&app, &user, &p).await?))
}

// ---------------------------------------------------------------- workspaces

/// How long the hub waits for the worker's `result` to `fs.stat` / `fs.list_dirs`.
const WORKER_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const MAX_PATH: usize = 4096;

pub(crate) async fn workspace_for(
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
pub(crate) async fn own_worker(
    app: &AppState,
    user: &CurrentUser,
    id: Uuid,
) -> ApiResult<db::WorkerRow> {
    match db::worker(&app.db, id).await? {
        Some(w) if w.owner_id == user.id && w.revoked_at.is_none() => Ok(w),
        _ => Err(ApiError::BadRequest("unknown worker".into())),
    }
}

/// Send a request frame to the online worker and wait for its result.
pub(crate) async fn ask_worker(
    app: &AppState,
    worker_id: Uuid,
    build: impl FnOnce(String) -> HubFrame,
) -> ApiResult<ResultBody> {
    ask_worker_for(app, worker_id, WORKER_REQUEST_TIMEOUT, build).await
}

/// [`ask_worker`] with its own timeout.
pub(crate) async fn ask_worker_for(
    app: &AppState,
    worker_id: Uuid,
    timeout: std::time::Duration,
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
    match conn.request(build, timeout).await {
        Ok(body) => Ok(body),
        Err(RequestError::Offline) => Err(offline()),
        Err(RequestError::Timeout) => Err(ApiError::WorkerTimeout),
        Err(RequestError::Worker(e)) => Err(ApiError::Invalid("invalid_path", e.message)),
    }
}

pub(crate) fn check_path(p: &str) -> ApiResult<&str> {
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
/// `409 workspace_conflict` (a folder belongs to one project, SPEC §3 Workspace). `kind:
/// clone` (T45): [`crate::workspaces::create_clone`].
pub async fn create_workspace(
    State(app): State<AppState>,
    user: CurrentUser,
    Json(body): Json<CreateWorkspaceImport>,
) -> ApiResult<(StatusCode, Json<api::Workspace>)> {
    let b = body.workspace;
    let project = project_for(&app, &user, b.project_id).await?;
    let worker = own_worker(&app, &user, b.worker_id).await?;
    let label = b.label.as_deref().unwrap_or("").trim();
    if label.chars().count() > MAX_LABEL {
        return Err(ApiError::BadRequest(format!(
            "label is longer than {MAX_LABEL} characters"
        )));
    }
    let label = hqc_proto::replace_nul(label);
    match b.kind.as_deref() {
        None | Some("dir") => {}
        Some("clone") => {
            return crate::workspaces::create_clone(
                &app,
                &project,
                &worker,
                user.id,
                b.clone(),
                &label,
                body.mirror_enabled,
            )
            .await
        }
        Some("worktree") => {
            return crate::workspaces::create_worktree(
                &app,
                &project,
                &worker,
                user.id,
                b.clone(),
                &label,
                body.mirror_enabled,
                None,
            )
            .await
        }
        Some(k) => {
            return Err(ApiError::BadRequest(format!(
                "kind {k:?}: a workspace is a dir, a clone or a worktree"
            )))
        }
    }
    let canonical = stat_folder(&app, worker.id, &b.path, b.create).await?;
    let ws = workspace_insert(
        &app,
        db::NewWorkspace {
            project_id: project.id,
            owner_id: user.id,
            worker_id: worker.id,
            path: &canonical,
            label: &label,
            mirror_enabled: body.mirror_enabled,
            source: db::Source::Dir,
            ephemeral: None,
        },
    )
    .await?;
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

/// The canonical path of folder `path` on the online worker, which checks it with `fs.stat`
/// (`create`: makes it when missing): the codes of `POST /api/workspaces` (SPEC §6).
pub(crate) async fn stat_folder(
    app: &AppState,
    worker_id: Uuid,
    path: &str,
    create: bool,
) -> ApiResult<String> {
    let st = fs_stat(app, worker_id, path, create).await?;
    folder(path, &st)
}

/// The online worker's `fs.stat` of `path` as it answered, for a path [`check_path`] takes.
pub(crate) async fn fs_stat(
    app: &AppState,
    worker_id: Uuid,
    path: &str,
    create: bool,
) -> ApiResult<FsStatResult> {
    let request = check_path(path)?.to_string();
    match ask_worker(app, worker_id, |request_id| {
        HubFrame::FsStat(FsStat {
            request_id,
            path: request,
            create,
        })
    })
    .await?
    {
        ResultBody::FsStat(st) => Ok(st),
        other => Err(anyhow::anyhow!("fs.stat answered with {other:?}").into()),
    }
}

/// The canonical path in the worker's answer `st` to `fs.stat` of `path`, with the codes of
/// [`stat_folder`].
pub(crate) fn folder(path: &str, st: &FsStatResult) -> ApiResult<String> {
    let path = check_path(path)?;
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
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("fs.stat without canonical for an existing path"))?;
    let canonical = check_path(canonical)?;
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
    Ok(canonical.to_string())
}

/// Stores a new workspace; one that overlaps a workspace of the same worker is `409
/// workspace_conflict` with that workspace in the body (SPEC §6).
pub(crate) async fn workspace_insert(
    app: &AppState,
    n: db::NewWorkspace<'_>,
) -> ApiResult<db::WorkspaceRow> {
    let path = n.path.to_string();
    match db::create_workspace(&app.db, n).await? {
        db::WorkspaceInsert::Created(ws) => Ok(ws),
        db::WorkspaceInsert::Conflict(x) => Err(workspace_conflict(app, &path, &x).await?),
    }
}

/// `409 workspace_conflict` for `path` that equals, lies inside or contains workspace `x`.
pub(crate) async fn workspace_conflict(
    app: &AppState,
    path: &str,
    x: &db::WorkspaceRow,
) -> ApiResult<ApiError> {
    let name = db::project(&app.db, x.project_id)
        .await?
        .map(|p| p.name)
        .unwrap_or_default();
    let how = if x.path == path {
        "is already"
    } else if std::path::Path::new(path).starts_with(&x.path) {
        "is inside"
    } else {
        "contains"
    };
    let mut extra = serde_json::Map::new();
    extra.insert("existing_workspace_id".into(), json!(x.id));
    extra.insert("existing_project_id".into(), json!(x.project_id));
    extra.insert("existing_path".into(), json!(x.path));
    Ok(ApiError::ConflictWith(
        "workspace_conflict",
        format!("{path} {how} workspace {} of project {name}", x.path),
        extra,
    ))
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

#[derive(Debug, Deserialize)]
pub struct DeleteWorkspaceQuery {
    #[serde(default)]
    pub remove_folder: bool,
    /// With `remove_folder`: the user confirmed the uncommitted files (T46).
    #[serde(default)]
    pub force: bool,
    /// The user confirmed archiving the sessions that work in it (SPEC §13.2, since 0.64).
    #[serde(default)]
    pub archive_sessions: bool,
}

/// `DELETE /api/workspaces/{id}?remove_folder=&force=`: the record (SPEC §13.2); the folder
/// stays unless `remove_folder` is set for a folder hqc created (T45), which the worker
/// deletes first (`workspace.delete`; offline: `409 worker_offline`, nothing deleted; `force`
/// for a worktree with uncommitted files, T46). `409 workspace_in_use` while a session that is
/// not archived runs there (unless `archive_sessions`, which archives them first) or worktrees
/// of it exist (T46), `409 workspace_creating` while the worker makes it.
pub async fn delete_workspace(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Query(q): Query<DeleteWorkspaceQuery>,
) -> ApiResult<StatusCode> {
    let ws = workspace_for(&app, &user, id).await?;
    let in_use = |n: i64| {
        ApiError::Conflict(
            "workspace_in_use",
            format!("{n} session(s) still work in this workspace; archive them first"),
        )
    };
    let has_worktrees = |n: i64| {
        ApiError::Conflict(
            "workspace_in_use",
            format!("{n} worktree(s) of this workspace exist; delete them first"),
        )
    };
    if ws.state() == WorkspaceState::Creating {
        return Err(ApiError::Conflict(
            "workspace_creating",
            "the worker is still making this workspace".into(),
        ));
    }
    let n = db::worktrees_of(&app.db, ws.id).await?;
    if n > 0 {
        return Err(has_worktrees(n));
    }
    if q.archive_sessions {
        // Before the folder goes: their processes are killed first.
        for sid in db::live_session_ids_in_workspace(&app.db, ws.id).await? {
            let s = session_for(&app, &user, sid, Action::Manage).await?;
            not_system(&s)?;
            archive_session(&app, &s).await?;
        }
    }
    match ws.state() {
        WorkspaceState::Ready if q.remove_folder && ws.created_by_hqc => {
            let n = db::live_sessions_in_workspace(&app.db, ws.id).await?;
            if n > 0 {
                return Err(in_use(n));
            }
            crate::workspaces::remove_folder(&app, &ws, q.force).await?;
        }
        _ if q.remove_folder => {
            return Err(ApiError::BadRequest(
                "remove_folder: only a ready folder hqc created is deleted".into(),
            ))
        }
        _ => {}
    }
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
        db::WorkspaceDelete::InUse(n) => Err(in_use(n)),
        db::WorkspaceDelete::HasWorktrees(n) => Err(has_worktrees(n)),
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

#[derive(Debug, Deserialize)]
pub struct MoveSession {
    pub path: String,
    /// A slug or id of the user's project; absent: a new project named after the folder.
    pub project: Option<String>,
    /// The worker makes a missing folder, as for `POST /api/workspaces`.
    #[serde(default)]
    pub create: bool,
}

/// `POST /api/sessions/{id}/move` {path, project?, create?} (SPEC §6): the session continues
/// in another folder of its worker, in another project ([`crate::moves`]); the response is
/// the session object. No input is queued: the session goes on with the next one.
pub async fn move_session(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<MoveSession>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Manage).await?;
    let project = b.project.as_deref();
    let m = crate::moves::move_session(&app, &s, &b.path, project, b.create, None).await?;
    Ok(Json(json!(state::view(&app, user.id, m.session).await?)))
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

/// `GET /api/sessions` (SPEC §6): the sessions of every project of the user in one list, the
/// objects of `GET /api/projects/{id}/sessions`, the newest `updated_at` first.
pub async fn all_sessions(
    State(app): State<AppState>,
    user: CurrentUser,
) -> ApiResult<Json<SessionList>> {
    let rows = db::sessions_of_owned_projects(&app.db, user.id).await?;
    Ok(Json(SessionList {
        sessions: state::views(&app, user.id, rows).await?,
    }))
}

/// `live` (the project's listed sessions, newest first) in the order of `placed`: ids of
/// `placed` that are not in `live` are dropped, sessions of `live` missing from `placed` go
/// first, newest first (T50: a new session is on top of a custom order).
pub(crate) fn arrange(live: &[Uuid], placed: &[Uuid]) -> Vec<Uuid> {
    let mut kept: Vec<Uuid> = Vec::with_capacity(live.len());
    for id in placed {
        if live.contains(id) && !kept.contains(id) {
            kept.push(*id);
        }
    }
    let mut out: Vec<Uuid> = live
        .iter()
        .filter(|id| !kept.contains(id))
        .copied()
        .collect();
    out.extend(kept);
    out
}

async fn session_order_of(app: &AppState, user: Uuid, project: Uuid) -> ApiResult<SessionOrder> {
    let live = db::live_session_ids(&app.db, project).await?;
    let (mode, placed) = db::session_order(&app.db, user, project)
        .await?
        .unwrap_or_default();
    Ok(SessionOrder {
        mode: SessionOrderMode::parse(&mode).unwrap_or_default(),
        session_ids: arrange(&live, &placed),
    })
}

/// `GET /api/projects/{id}/session_order` (T50, SPEC §6): the user's order of the list.
pub async fn get_session_order(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<SessionOrder>> {
    project_for(&app, &user, id).await?;
    Ok(Json(session_order_of(&app, user.id, id).await?))
}

/// `PUT /api/projects/{id}/session_order` {mode?, session_ids?}: only the project's listed
/// sessions are kept, so another project's, another user's, an archived or an unknown id is
/// ignored.
pub async fn put_session_order(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<PutSessionOrder>,
) -> ApiResult<Json<SessionOrder>> {
    project_for(&app, &user, id).await?;
    let ids = match &b.session_ids {
        Some(placed) => Some(arrange(&db::live_session_ids(&app.db, id).await?, placed)),
        None => None,
    };
    db::put_session_order(
        &app.db,
        user.id,
        id,
        b.mode.map(SessionOrderMode::as_str),
        ids.as_deref(),
    )
    .await?;
    Ok(Json(session_order_of(&app, user.id, id).await?))
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

/// The model a typed `/model <name>` switches the agent to (SPEC §6): `Some(None)` for
/// `default`, the runtime's default; `None` for any other text, and for a name hqc does not
/// take ([`valid_model`]), which the runtime still gets as the command.
fn typed_model(text: &str) -> Option<Option<String>> {
    let mut words = text.split_whitespace();
    if words.next()? != "/model" {
        return None;
    }
    let name = words.next()?;
    if words.next().is_some() {
        return None;
    }
    match name {
        "default" => Some(None),
        n if valid_model(n) => Some(Some(n.to_string())),
        _ => None,
    }
}

fn check_model(m: &str) -> ApiResult<()> {
    if !valid_model(m) {
        return Err(ApiError::BadRequest(
            "model must be 1..64 characters of A-Z a-z 0-9 . _ : -".into(),
        ));
    }
    Ok(())
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
    let w = picked_workspace(app, project, b).await?.ok_or_else(none)?;
    // A clone that runs or failed has no folder to start in (T45).
    match w.state() {
        WorkspaceState::Ready => Ok(w),
        WorkspaceState::Creating => Err(ApiError::Conflict(
            "workspace_not_ready",
            format!("the worker is still making {}", w.path),
        )),
        WorkspaceState::Failed => Err(ApiError::Conflict(
            "workspace_not_ready",
            format!("{} could not be cloned; remove it", w.path),
        )),
    }
}

async fn picked_workspace(
    app: &AppState,
    project: &db::ProjectRow,
    b: &CreateSessionRequest,
) -> ApiResult<Option<db::WorkspaceRow>> {
    if let Some(id) = b.workspace_id {
        return match db::workspace(&app.db, id).await? {
            Some(w)
                if w.project_id == project.id
                    && b.worker_id.is_none_or(|wid| wid == w.worker_id) =>
            {
                Ok(Some(w))
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
        return Ok(Some(on.swap_remove(i)));
    }
    if b.worker_id.is_some() {
        return Ok(on.into_iter().next());
    }
    Ok(None)
}

pub async fn create_session(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(b): Json<CreateSessionRequest>,
) -> ApiResult<impl IntoResponse> {
    let project = project_for(&app, &user, project_id).await?;
    let s = new_session(&app, user.id, &project, &b, None).await?;
    let v = state::view(&app, user.id, s).await?;
    Ok((StatusCode::CREATED, Json(json!(v))))
}

/// Creates a session of `owner` in their `project` and starts its agent (SPEC §6, §13.4):
/// `POST /api/projects/{id}/sessions` and `hqc_spawn` (SPEC §7, with `spawn`).
pub async fn new_session(
    app: &AppState,
    owner: Uuid,
    project: &db::ProjectRow,
    b: &CreateSessionRequest,
    spawn: Option<db::Spawn>,
) -> ApiResult<SessionRow> {
    let id = insert_session(app, owner, project, b, spawn, None).await?;
    delivery::kick(app, id, true).await;
    db::session(&app.db, id).await?.ok_or(ApiError::NotFound)
}

/// The workspace, worker and folder of a new session (SPEC §6, §13.4): a ready workspace of
/// the project on a worker of `owner`, `cwd` a relative path inside it (`.` when absent).
async fn session_place<'b>(
    app: &AppState,
    owner: Uuid,
    project: &db::ProjectRow,
    b: &'b CreateSessionRequest,
) -> ApiResult<(db::WorkspaceRow, db::WorkerRow, &'b str)> {
    let ws = session_workspace(app, project, b).await?;
    let worker = match db::worker(&app.db, ws.worker_id).await? {
        Some(w) if w.owner_id == owner && w.revoked_at.is_none() => w,
        _ => return Err(ApiError::BadRequest("unknown worker".into())),
    };
    if let Some(p) = b.provider_id {
        crate::providers::check(app, owner, &worker, p).await?;
    }
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
    Ok((ws, worker, cwd))
}

/// [`new_session`] without the start: the session (an agent of a room with `member`, SPEC
/// §7.1.1) and its `system session_created`; its id.
async fn insert_session(
    app: &AppState,
    owner: Uuid,
    project: &db::ProjectRow,
    b: &CreateSessionRequest,
    spawn: Option<db::Spawn>,
    member: Option<db::Member<'_>>,
) -> ApiResult<Uuid> {
    if let Some(m) = &b.model {
        check_model(m)?;
    }
    let (ws, worker, cwd) = session_place(app, owner, project, b).await?;
    // No title: "Session N" (SPEC §3 Session), which the runtime's title may replace.
    let title = b.title.as_deref().map(str::trim).filter(|t| !t.is_empty());
    if let Some(t) = title {
        check_title(t)?;
    }
    let title = title.map(hqc_proto::replace_nul);
    let id = db::create_spawned_session(
        &app.db,
        db::NewSession {
            project_id: project.id,
            owner_id: owner,
            workspace_id: Some(ws.id),
            worker_id: Some(worker.id),
            cwd,
            title: title.as_deref(),
            model: b.model.as_deref(),
            // Only the worker owner may enable it (SPEC §10.1); here that is the creator.
            auto_approve: b.auto_approve && worker.owner_id == owner,
            // The creator owns the worker (above), so `auto` and `allow_all` are theirs to
            // choose (T38).
            permission_mode: b.permission_mode,
        },
        spawn,
        member,
    )
    .await?;
    if b.provider_id.is_some() {
        // Before the first start, which carries it.
        db::set_session_provider(&app.db, id, b.provider_id).await?;
    }
    let s = db::session(&app.db, id).await?.ok_or(ApiError::NotFound)?;
    events::system(app, &s, "session_created", "Session created").await?;
    Ok(id)
}

/// The longest system prompt of a room's agent, in bytes (SPEC §6).
const MAX_ROOM_PROMPT: usize = 32 * 1024;

/// `POST /api/projects/{id}/rooms` (SPEC §6, §7.1.1): a room and a session for each of its
/// agents, checked as `POST .../sessions` checks one; `201` and the room's session object. An
/// agent's session is titled by its name, else by its model, or its provider's label, "default"
/// without one, a repeated one numbered ("opus 2"); its agent starts with the room's first
/// message.
pub async fn create_room(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(b): Json<CreateRoomRequest>,
) -> ApiResult<impl IntoResponse> {
    let project = project_for(&app, &user, project_id).await?;
    let room = new_room(&app, user.id, &project, &b, None).await?;
    let v = state::view(&app, user.id, room).await?;
    Ok((StatusCode::CREATED, Json(json!(v))))
}

/// Creates a room of `owner` in their `project` with a session for each agent of `b`
/// (`POST /api/projects/{id}/rooms` and `hqc_room`, SPEC §7.1.1): checked as the sessions are, the
/// agents' providers on the workspace's worker too. `spawn`, from `hqc_room`, is the room's id
/// and the session that made it, and its chain goes on the agents' sessions.
pub async fn new_room(
    app: &AppState,
    owner: Uuid,
    project: &db::ProjectRow,
    b: &CreateRoomRequest,
    spawn: Option<db::Spawn>,
) -> ApiResult<SessionRow> {
    if b.agents.is_empty() {
        return Err(ApiError::BadRequest(
            "a room needs at least one agent".into(),
        ));
    }
    let title = b.title.as_deref().map(str::trim).filter(|t| !t.is_empty());
    if let Some(t) = title {
        check_title(t)?;
    }
    let title = title.map(hqc_proto::replace_nul);
    let mut agents = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    for a in &b.agents {
        let model = a.model.as_deref().map(str::trim).filter(|m| !m.is_empty());
        if let Some(m) = model {
            check_model(m)?;
        }
        // A provider's agent is named by the provider's label, as the client names its model.
        let label = match a.provider_id {
            Some(p) => match db::provider(&app.db, p).await? {
                Some(p) if p.owner_id == owner => p.label,
                _ => return Err(ApiError::BadRequest("unknown provider".into())),
            },
            None => model.unwrap_or("default").to_string(),
        };
        // Its name, when given, titles its session; otherwise its label, numbered among the
        // agents titled by theirs.
        let name = a.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
        if let Some(n) = name {
            check_title(n)?;
        }
        let title = match name {
            Some(n) => hqc_proto::replace_nul(n).into_owned(),
            None => {
                let n = labels.iter().filter(|l| **l == label).count();
                let title = if n == 0 {
                    label.clone()
                } else {
                    format!("{label} {}", n + 1)
                };
                labels.push(label);
                title
            }
        };
        let req = CreateSessionRequest {
            title: Some(title),
            workspace_id: b.workspace_id,
            cwd: b.cwd.clone(),
            model: model.map(str::to_string),
            permission_mode: b.permission_mode,
            provider_id: a.provider_id,
            ..CreateSessionRequest::default()
        };
        // It goes to the worker as one argument of the CLI, which Linux refuses over 128 KiB; a
        // NUL, which Postgres refuses, is replaced as in a title.
        let system_prompt = a.system_prompt.as_deref().map(hqc_proto::replace_nul);
        if system_prompt
            .as_ref()
            .is_some_and(|p| p.len() > MAX_ROOM_PROMPT)
        {
            return Err(ApiError::BadRequest(format!(
                "an agent's system_prompt is at most {} KiB",
                MAX_ROOM_PROMPT / 1024
            )));
        }
        agents.push((req, system_prompt));
    }
    // The room's agents work in one workspace and folder: checked before the room exists, and
    // every agent's session is created in the workspace found here. So is each agent's provider
    // on that workspace's worker, the first one's in `session_place`.
    let (ws, worker, _) = session_place(app, owner, project, &agents[0].0).await?;
    for (req, _) in &agents[1..] {
        if let Some(p) = req.provider_id {
            crate::providers::check(app, owner, &worker, p).await?;
        }
    }
    let id = db::create_room(&app.db, project.id, owner, title.as_deref(), b.mode, spawn).await?;
    let room = db::session(&app.db, id).await?.ok_or(ApiError::NotFound)?;
    events::system(app, &room, "session_created", "Room created").await?;
    for (mut req, system_prompt) in agents {
        req.workspace_id = Some(ws.id);
        let member = db::Member {
            room_id: id,
            system_prompt: system_prompt.as_deref(),
            chain_id: spawn.map(|s| s.chain_id),
        };
        insert_session(app, owner, project, &req, None, Some(member)).await?;
    }
    db::session(&app.db, id).await?.ok_or(ApiError::NotFound)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkSession {
    event_id: Uuid,
}

/// `POST /api/sessions/{id}/fork {event_id}` (SPEC §6): a new session whose conversation is
/// this one's before the user's message `event_id` ([`crate::fork`]); `201` and the session
/// object.
pub async fn fork_session(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<ForkSession>,
) -> ApiResult<impl IntoResponse> {
    let s = session_for(&app, &user, id, Action::Manage).await?;
    not_system(&s)?;
    let new = crate::fork::fork(&app, &s, b.event_id).await?;
    let v = state::view(&app, user.id, new).await?;
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
    /// T38: `auto` and `allow_all` only by the worker owner; a running agent switches at once.
    pub permission_mode: Option<PermissionMode>,
    /// Absent: unchanged; `null`: the runtime's default. A running agent switches to a named
    /// model at once; `null` takes effect at its next start.
    #[serde(
        default,
        deserialize_with = "hqc_proto::frame::maybe_null::deserialize"
    )]
    pub model: Option<Option<String>>,
    /// Absent: unchanged; `null`: the runtime's own endpoint (SPEC 0.144). A provider of the
    /// owner the session's worker allows; a change restarts a running agent.
    #[serde(
        default,
        deserialize_with = "hqc_proto::frame::maybe_null::deserialize"
    )]
    pub provider_id: Option<Option<Uuid>>,
    /// The owner's Chrome for the agent (SPEC 0.148), on a worker with `browser`; a change
    /// restarts a running agent.
    #[serde(default)]
    pub browser: Option<bool>,
}

pub async fn patch_session(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<PatchSession>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Manage).await?;
    if s.room_mode.is_some()
        && (b.model.is_some() || b.permission_mode.is_some() || b.browser.is_some())
    {
        return Err(ApiError::Conflict(
            "room",
            "a room has no agent of its own: its agents' sessions have the model, the permission mode and the browser".into(),
        ));
    }
    if s.system && (b.model.is_some() || b.provider_id.is_some() || b.browser.is_some()) {
        return Err(ApiError::Conflict(
            "system_session",
            "the worker's service session has no agent".into(),
        ));
    }
    if let Some(Some(m)) = &b.model {
        check_model(m)?;
    }
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
    if let Some(m) = b.permission_mode {
        if s.system {
            return Err(ApiError::Conflict(
                "system_session",
                "the worker's service session has no agent".into(),
            ));
        }
        let owner = match s.worker_id {
            Some(w) => db::worker(&app.db, w).await?.map(|w| w.owner_id),
            None => None,
        };
        // Auto and Allow all run tool calls the worker owner would otherwise be asked about
        // (SPEC §10.1).
        if m != PermissionMode::Ask && owner != Some(user.id) {
            return Err(ApiError::Forbidden(format!(
                "only the worker owner switches a session to {}",
                m.as_str()
            )));
        }
        db::set_permission_mode(&app.db, id, m).await?;
        // A stopped agent starts in it; an offline worker gets it with `session.start`.
        send_to_worker(
            &app,
            &s,
            HubFrame::SessionPermissionMode {
                session_id: id,
                permission_mode: m,
            },
        );
    }
    if let Some(p) = b.provider_id {
        if let Some(p) = p {
            crate::providers::check_for_session(&app, &s, p).await?;
        }
        if p != s.provider_id {
            db::set_session_provider(&app.db, id, p).await?;
            // An environment cannot change in a running CLI: the agent starts again on the
            // new provider, its transcript resumed (SPEC 0.144).
            let s = db::session(&app.db, id).await?.ok_or(ApiError::NotFound)?;
            if s.process_state != "stopped" {
                restart_agent(&app, &s).await;
            }
        }
    }
    if let Some(on) = b.browser {
        if on {
            let worker = match s.worker_id {
                Some(w) => db::worker(&app.db, w).await?,
                None => None,
            };
            match worker {
                // The owner's own Chrome, on their own machine (SPEC §14).
                Some(w) if w.owner_id == user.id && s.owner_id == user.id && w.browser => {}
                Some(w) if w.owner_id == user.id && s.owner_id == user.id => {
                    return Err(ApiError::Unprocessable(
                        "browser_not_allowed",
                        format!(
                            "worker {} does not give a browser: add browser = true to its worker.toml",
                            w.name
                        ),
                    ))
                }
                _ => {
                    return Err(ApiError::Forbidden(
                        "only the worker owner gives a session their browser".into(),
                    ))
                }
            }
        }
        if on != s.browser {
            db::set_session_browser(&app.db, id, on).await?;
            // `--chrome` is a flag of the process: the agent starts again with it, its
            // transcript resumed.
            let s = db::session(&app.db, id).await?.ok_or(ApiError::NotFound)?;
            if s.process_state != "stopped" {
                restart_agent(&app, &s).await;
            }
        }
    }
    if let Some(m) = b.model {
        db::set_model(&app.db, id, m.as_deref()).await?;
        // A stopped agent starts with it; an offline worker gets it with `session.start`.
        send_to_worker(
            &app,
            &s,
            HubFrame::SessionModel {
                session_id: id,
                model: m,
            },
        );
    }
    app.session_changed(id);
    let s = db::session(&app.db, id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(json!(state::view(&app, user.id, s).await?)))
}

/// Deleting a session drops its events, cursors and requests; the worker's jsonl files are
/// not touched (SPEC §17.4). A room's agents' sessions are deleted with it (§7.1.1).
pub async fn delete_session(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let s = session_for(&app, &user, id, Action::Manage).await?;
    not_system(&s)?;
    // Before the room: deleting it would clear their `room_id`.
    if s.room_mode.is_some() {
        for a in db::room_agents(&app.db, id).await? {
            remove_session(&app, &a).await?;
        }
    }
    remove_session(&app, &s).await?;
    // Its requests and asks went with it.
    app.requests_changed(s.owner_id);
    Ok(StatusCode::NO_CONTENT)
}

/// Session `s` killed on its worker, its files there cleaned up, and its row deleted.
async fn remove_session(app: &AppState, s: &SessionRow) -> ApiResult<()> {
    let id = s.id;
    let cleanup = cleanup_frame(app, s).await?;
    send_to_worker(app, s, HubFrame::SessionKill { session_id: id });
    if let Some(f) = cleanup {
        send_to_worker(app, s, f);
    }
    sqlx::query("DELETE FROM sessions WHERE id = $1")
        .bind(id)
        .execute(&app.db)
        .await?;
    // The empty list and the 0 rate its killed agent sends are dropped with the session unknown
    // (SPEC §4).
    app.drop_live_agents(id);
    app.token_rate_changed(id, 0);
    app.drop_foreground_calls(id);
    app.drop_activity(id);
    // A text or thinking it was writing would otherwise stay in memory.
    app.end_live_text(id);
    app.session_changed(id);
    Ok(())
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
        Some(b) => db::events_before(&app.db, id, b, limit, db::Withdrawn::Included).await?,
        None => {
            db::events_after(
                &app.db,
                id,
                q.after_seq.unwrap_or(0),
                limit,
                db::Withdrawn::Included,
            )
            .await?
        }
    };
    Ok(Json(EventPage {
        events: db::events_for_clients(&app.db, &rows).await?,
        last_seq: s.last_seq.max(0) as u64,
    }))
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    /// A project id or slug of the user's.
    pub project: Option<String>,
    /// A session id of the user's: only its hits.
    pub session: Option<String>,
    /// `message`, `command`, `output` or `answer`: only hits of that kind.
    pub kind: Option<String>,
    /// An RFC 3339 time: only hits at or after it.
    pub since: Option<String>,
    /// `best` (the default) or `newest`.
    pub order: Option<String>,
    /// At most this many hits of each session.
    pub per_session: Option<usize>,
    /// `1` or `true`.
    pub fuzzy: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

/// `GET /api/search?q=&project=&session=&kind=&since=&order=&per_session=&fuzzy=&cursor=&limit=`
/// (SPEC §15): the user's sessions only; another user's project is `404`, as a missing one,
/// and another user's session has no hits.
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
    let bad = |m: &str| ApiError::BadRequest(m.into());
    let session_id = match q.session.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => Some(
            s.parse::<Uuid>()
                .map_err(|_| bad("session is not a session id"))?,
        ),
        None => None,
    };
    let kind = match q.kind.as_deref().filter(|k| !k.is_empty()) {
        Some(k) => Some(
            crate::search::Kind::parse(k)
                .ok_or_else(|| bad("kind is one of message, command, output, answer"))?,
        ),
        None => None,
    };
    let since = match q.since.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => Some(
            chrono::DateTime::parse_from_rfc3339(s)
                .map_err(|_| bad("since is not an RFC 3339 time"))?
                .to_utc(),
        ),
        None => None,
    };
    let newest = match q.order.as_deref().unwrap_or("") {
        "" | "best" => false,
        "newest" => true,
        _ => return Err(bad("order is best or newest")),
    };
    let cursor = q.cursor.filter(|c| !c.is_empty());
    let query = crate::search::Query {
        text: q.q,
        project_id,
        session_id,
        kind,
        since,
        newest,
        per_session: q.per_session.map(|n| n.clamp(1, crate::search::MAX_PAGE)),
        // The counts are of the whole query: its first page brings them, not a session's hits.
        counts: cursor.is_none() && session_id.is_none(),
        fuzzy: matches!(q.fuzzy.as_deref(), Some("1" | "true")),
        cursor,
        limit: q.limit.unwrap_or(crate::search::PAGE),
        // The owner's own search sees everything.
        room_agent: None,
    };
    match crate::search::capped(&app, user.id, &query).await {
        Ok(page) => Ok(Json(page)),
        Err(crate::search::SearchError::Invalid(m)) => Err(ApiError::BadRequest(m)),
        Err(crate::search::SearchError::Limited(m)) => Err(ApiError::Limited("search_limit", m)),
        Err(crate::search::SearchError::Busy) => Err(ApiError::Busy),
        Err(crate::search::SearchError::Db(e)) => Err(e.into()),
    }
}

/// Input, a restart, a pause and a resume go only to a session that is not archived (`409
/// archived`) and that VS Code does not drive (`409 locked_vscode`).
pub(crate) fn driven_from_hqc(s: &SessionRow) -> ApiResult<()> {
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
    Ok(())
}

/// `POST /api/sessions/{id}/input`: stored as `user.message` with `delivery: pending`
/// and delivered when the worker is there (SPEC §5.2); in a room without `delivery`, each of
/// its agents getting it (SPEC §7.1.1). A repeated `client_op_id` returns the event stored the
/// first time, with 200 instead of 201.
pub async fn input(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<InputRequest>,
) -> ApiResult<(StatusCode, Json<InputResponse>)> {
    let reply = |status, r: &db::EventRow| (status, Json(InputResponse { event: r.to_json() }));
    let s = session_for(&app, &user, id, Action::Write).await?;
    not_system(&s)?;
    driven_from_hqc(&s)?;
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
    // A room's message is stored and handed to its agents under the room's lock (SPEC §7.1.1).
    let _room = match s.room_mode {
        Some(_) => Some(crate::rooms::lock(&app, id).await),
        None => None,
    };
    if let Some(op) = &op {
        if let Some(r) = db::event_by_client_op(&app.db, id, op).await? {
            // A repeat completes a room's message to its agents cut short the first time; what
            // reached an agent is not stored twice (SPEC §7.1.1).
            if s.room_mode.is_some() {
                crate::rooms::owner_message(&app, &s, &r).await?;
            }
            return Ok(reply(StatusCode::OK, &r));
        }
    }
    // Only the user's own artifacts; the session's worker may then download them (SPEC §12).
    crate::artifacts::attach_to_session(&app.db, user.id, id, &b.artifact_ids).await?;
    // A failed turn shows `error` until the next input (SPEC §17.1).
    db::clear_turn_error(&app.db, id).await?;
    // A room has no agent to deliver to: its agents get the message (SPEC §7.1.1).
    let delivery = if s.room_mode.is_some() {
        None
    } else {
        Some("pending")
    };
    let stored = events::store_hub_event(
        &app,
        &s,
        user_author(user.id),
        "user.message",
        // `human`: typed by the owner in this route; the worker writes Claude Code's
        // `origin: human` for such an input only (SPEC 0.219). No other writer marks it.
        json!({"text": b.text, "artifact_ids": b.artifact_ids, "human": true}),
        op.clone(),
        delivery,
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
    if s.room_mode.is_some() {
        crate::rooms::owner_message(&app, &s, &row).await?;
        return Ok(reply(StatusCode::CREATED, &row));
    }
    // A typed `/model` switches the agent as `PATCH {model}` does; otherwise the next
    // `session.start` would bring the stored model back.
    if let Some(m) = typed_model(&b.text).filter(|m| *m != s.model) {
        db::set_model(&app.db, id, m.as_deref()).await?;
        send_to_worker(
            &app,
            &s,
            HubFrame::SessionModel {
                session_id: id,
                model: m,
            },
        );
        app.session_changed(id);
    }
    delivery::kick(&app, id, false).await;
    Ok(reply(StatusCode::CREATED, &row))
}

/// The owner's own message `event_id` of session `s` that waits for its delivery, for
/// [`withdraw`] and [`input_now`]: another event, one of another session, one the owner did not
/// type in the input route (`payload.human`: not a pause's resume note, a run of a schedule, a
/// message of another session, a note of the hub's own) is `404`; a room (its agents get the
/// message as posts of the room) `409 room`; one the agent took already `409 already_taken`;
/// one taken back already `409 withdrawn`.
async fn own_waiting_input(app: &AppState, s: &SessionRow, event_id: Uuid) -> ApiResult<()> {
    if s.room_mode.is_some() {
        return Err(ApiError::Conflict(
            "room",
            "a room has no agent of its own: its agents get its messages".into(),
        ));
    }
    let e = db::event_by_id(&app.db, event_id)
        .await?
        .filter(|e| {
            e.session_id == s.id
                && e.kind == "user.message"
                && e.payload.get("human") == Some(&Value::Bool(true))
        })
        .ok_or(ApiError::NotFound)?;
    match e.delivery.as_deref() {
        Some("pending") => Ok(()),
        Some("withdrawn") => Err(ApiError::Conflict(
            "withdrawn",
            "this message was taken back already".into(),
        )),
        _ => Err(already_taken()),
    }
}

fn already_taken() -> ApiError {
    ApiError::Conflict(
        "already_taken",
        "the agent already took this message".into(),
    )
}

/// The worker connection of `s`, when it is started for the session and holds the input
/// `event_id` (sent and not acknowledged): the one to ask about it. Asked on this very
/// connection: another one, which the worker may have opened meanwhile, holds nothing of it.
/// For [`input_now`]: a message of a session that is not started has no turn to join.
async fn holding_conn(
    app: &AppState,
    s: &SessionRow,
    event_id: Uuid,
) -> Option<std::sync::Arc<crate::worker_ws::WorkerConn>> {
    let conn = s.worker_id.and_then(|w| app.workers.get(w))?;
    let holds = conn.st.lock().await.sessions.get(&s.id).is_some_and(|sc| {
        sc.start == crate::worker_ws::StartState::Started && sc.inflight.contains(&event_id)
    });
    holds.then_some(conn)
}

/// A request about an input to the worker connection `conn` (SPEC 0.219): a worker's answer of
/// `error` comes back as `502` with the worker's own code and message, not as the `400
/// invalid_path` the shared [`ask_worker_for`] makes of it.
async fn ask_input(
    conn: &crate::worker_ws::WorkerConn,
    frame: impl FnOnce(String) -> HubFrame,
) -> ApiResult<ResultBody> {
    let offline = || ApiError::Conflict("worker_offline", "the worker is offline".into());
    let Ok(_slot) = conn.fs_slots.try_acquire() else {
        return Err(ApiError::Limited(
            "worker_busy",
            "too many requests to this worker in flight, retry later".into(),
        ));
    };
    match conn.request(frame, WORKER_REQUEST_TIMEOUT).await {
        Ok(body) => Ok(body),
        Err(RequestError::Offline) => Err(offline()),
        Err(RequestError::Timeout) => Err(ApiError::WorkerTimeout),
        Err(RequestError::Worker(e)) if e.code == error_code::NOT_NOW => {
            Err(ApiError::Conflict("not_now", e.message))
        }
        Err(RequestError::Worker(e)) => Err(ApiError::WorkerError(e.code, e.message)),
    }
}

/// `session.withdraw` for `event_id` asked of `conn` by a task of its own, which settles the
/// answer ([`withdraw_settle`]) and sends the outcome on the channel, so that a handler dropped
/// at any point (the owner's connection broke) leaves no message half done. The caller has put
/// the id in `withdrawing` already. An answer after [`WORKER_REQUEST_TIMEOUT`] is not waited
/// for: the owner gets `504` (SPEC 0.219, §6 limitations).
fn ask_withdraw(
    app: &AppState,
    conn: &std::sync::Arc<crate::worker_ws::WorkerConn>,
    session_id: Uuid,
    event_id: Uuid,
) -> tokio::sync::oneshot::Receiver<ApiResult<()>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (app, conn) = (app.clone(), conn.clone());
    tokio::spawn(async move {
        let answer = ask_input(&conn, |request_id| {
            HubFrame::SessionWithdraw(InputRef {
                request_id,
                session_id,
                event_id,
            })
        })
        .await;
        let _ = tx.send(withdraw_settle(&app, &conn, session_id, event_id, answer).await);
    });
    rx
}

/// The worker's answer to a `session.withdraw`, or its absence (a timeout, an error, the
/// connection closed): `withdrawn: true` withdraws the message (out of `inflight`, its `ack` slot
/// dropped, the clients told), `false` is `409 already_taken`; the message leaves `withdrawing`
/// either way.
async fn withdraw_settle(
    app: &AppState,
    conn: &crate::worker_ws::WorkerConn,
    id: Uuid,
    event_id: Uuid,
    answer: ApiResult<ResultBody>,
) -> ApiResult<()> {
    let withdrawn = matches!(&answer, Ok(ResultBody::InputWithdrawn(r)) if r.withdrawn);
    let row = if withdrawn {
        db::mark_input_withdrawn(&app.db, event_id, false).await
    } else {
        Ok(None)
    };
    {
        let mut st = conn.st.lock().await;
        if let Some(sc) = st.sessions.get_mut(&id) {
            sc.withdrawing.remove(&event_id);
            if withdrawn {
                sc.inflight.remove(&event_id);
            }
        }
        if withdrawn {
            // Its `ack` never comes.
            forget_input_reply(&mut st.replies, event_id);
        }
    }
    if withdrawn && row.is_err() {
        // The worker let go of it and the hub could not mark it: it waits again, to be handed.
        delivery::kick(app, id, false).await;
    }
    match answer? {
        ResultBody::InputWithdrawn(r) if r.withdrawn => match row? {
            Some(row) => {
                delivery::notify_delivery(app, &row);
                app.session_changed(id);
                Ok(())
            }
            // The worker let go of it, yet the hub has it not pending: it is no longer waiting.
            None => Err(already_taken()),
        },
        ResultBody::InputWithdrawn(_) => Err(already_taken()),
        _ => Err(anyhow::anyhow!("session.withdraw answered with another result").into()),
    }
}

/// An input's `ack` slot: its `ack` never comes once the worker dropped the input.
fn forget_input_reply(
    replies: &mut std::collections::HashMap<String, crate::worker_ws::Reply>,
    event_id: Uuid,
) {
    replies.retain(
        |_, r| !matches!(r, crate::worker_ws::Reply::Input { event_id: e, .. } if *e == event_id),
    );
}

/// `POST /api/sessions/{id}/inputs/{event_id}/withdraw` (SPEC 0.219): the owner takes back a
/// message of theirs the agent has not taken. The message becomes `delivery: withdrawn`: it is
/// delivered to nobody and shown by no client (a `delivery` frame tells the open ones). One the
/// hub never handed to a worker (`handed_at` null) is withdrawn here: no worker holds it. One
/// that was handed is asked about at the worker connection, in whatever start state (a kill
/// clears the hub's books and the worker's queue, and the worker then holds nothing and says so):
/// its `session.withdraw` says whether the agent had taken the message already, which is `409
/// already_taken` and leaves the message as it is. With no connection a handed message is `409
/// worker_offline`: the worker may keep it and give it to the agent. While the request is
/// outstanding the message is in the connection's `withdrawing`, which `kick` skips, and a
/// second take-back is `409 withdrawing`. The answer is settled by a task of its own
/// ([`ask_withdraw`]), also when this request is gone. Answers `{withdrawn: true}`.
pub async fn withdraw(
    State(app): State<AppState>,
    user: CurrentUser,
    Path((id, event_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Write).await?;
    not_system(&s)?;
    driven_from_hqc(&s)?;
    own_waiting_input(&app, &s, event_id).await?;
    match db::mark_input_withdrawn(&app.db, event_id, true).await? {
        Some(row) => {
            delivery::notify_delivery(&app, &row);
            app.session_changed(id);
        }
        None => {
            // Handed, or the agent took it, or it was taken back since the check above.
            own_waiting_input(&app, &s, event_id).await?;
            let Some(conn) = s.worker_id.and_then(|w| app.workers.get(w)) else {
                return Err(ApiError::Conflict(
                    "worker_offline",
                    "the worker holds this message; try again when it is connected".into(),
                ));
            };
            // The clients are told by the task that settles the answer.
            ask_and_withdraw(&app, &conn, id, event_id).await?;
        }
    }
    Ok(Json(json!({"withdrawn": true})))
}

/// Ask the worker of `conn` to let go of the handed input `event_id` and withdraw it when it
/// has. The wait is the usual one; a later answer is not acted on.
async fn ask_and_withdraw(
    app: &AppState,
    conn: &std::sync::Arc<crate::worker_ws::WorkerConn>,
    id: Uuid,
    event_id: Uuid,
) -> ApiResult<()> {
    let first = conn
        .st
        .lock()
        .await
        .sessions
        .entry(id)
        .or_default()
        .withdrawing
        .insert(event_id);
    if !first {
        return Err(ApiError::Conflict(
            "withdrawing",
            "this message is being taken back already".into(),
        ));
    }
    match ask_withdraw(app, conn, id, event_id).await {
        Ok(outcome) => outcome,
        // The task is gone with no word: nothing is outstanding.
        Err(_) => {
            clear_withdrawing(conn, id, event_id).await;
            Err(anyhow::anyhow!("session.withdraw was not asked").into())
        }
    }
}

async fn clear_withdrawing(conn: &crate::worker_ws::WorkerConn, id: Uuid, event_id: Uuid) {
    if let Some(sc) = conn.st.lock().await.sessions.get_mut(&id) {
        sc.withdrawing.remove(&event_id);
    }
}

/// `POST /api/sessions/{id}/inputs/{event_id}/now` (SPEC 0.219): the owner sends a message of
/// theirs that waits for the agent into the running turn at once (Claude Code moves the call
/// the agent is in, a Bash command, an MCP call or a subagent, to the background and takes the
/// message into the same turn). Answers `{sent_now}`: `false` when the agent had taken it
/// already. `409 not_now` when the runtime cannot take it now (no turn runs, the agent waits
/// for an answer to a request, the message waits on the hub and not at the worker, the worker
/// holds no such message).
pub async fn input_now(
    State(app): State<AppState>,
    user: CurrentUser,
    Path((id, event_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Write).await?;
    not_system(&s)?;
    driven_from_hqc(&s)?;
    own_waiting_input(&app, &s, event_id).await?;
    let conn = holding_conn(&app, &s, event_id).await.ok_or_else(|| {
        ApiError::Conflict(
            "not_now",
            "the agent cannot take this message into its turn now".into(),
        )
    })?;
    let ResultBody::InputNow(r) = ask_input(&conn, |request_id| {
        HubFrame::SessionInputNow(InputRef {
            request_id,
            session_id: id,
            event_id,
        })
    })
    .await?
    else {
        return Err(anyhow::anyhow!("session.input_now answered with another result").into());
    };
    Ok(Json(json!({"sent_now": r.sent_now})))
}

/// Abort the current turn. The hub's own open requests (a move, a secret) are cancelled with
/// `stopped`. The runtime's (approvals, questions) are the worker's to close: it cancels those
/// of the interrupted turn when the turn ends, and a background subagent's stay open, since the
/// subagent goes on (SPEC §8.1, since 0.161). With no worker connected the hub cancels them too.
/// Of a room, its agents' turns (SPEC §7.1.1).
pub async fn stop(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Write).await?;
    not_system(&s)?;
    let sent = if s.room_mode.is_some() {
        stop_room(&app, &s).await?
    } else {
        stop_session(&app, &s).await?
    };
    Ok(Json(json!({ "sent": sent })))
}

/// Stop of session `s`: its open requests other than the runtime's cancelled with `stopped`,
/// `session.stop` to its worker, and the runtime's approvals and questions cancelled too only
/// when the worker is not connected (a connected worker keeps those of background subagents
/// open, SPEC §10.1); `true` when the frame went out.
pub(crate) async fn stop_session(app: &AppState, s: &SessionRow) -> ApiResult<bool> {
    let open = db::open_requests_of_session(&app.db, s.id).await?;
    let runtimes = |kind: &str| matches!(kind, "approval" | "clarify");
    for p in open.iter().filter(|p| !runtimes(&p.kind)) {
        requests::cancel(app, p, CancelReason::Stopped).await?;
    }
    let sent = send_to_worker(app, s, HubFrame::SessionStop { session_id: s.id });
    if !sent {
        for p in open.iter().filter(|p| runtimes(&p.kind)) {
            requests::cancel(app, p, CancelReason::Stopped).await?;
        }
    }
    Ok(sent)
}

/// `POST /stop` of room `room` (SPEC §7.1.1), under the room's lock: `system room_stopped`
/// "Stopped" in the room, then each agent stopped as its own Stop does ([`stop_session`]), then
/// the senders' notice ([`crate::rooms::round_ended`]); `true` when a frame went out.
async fn stop_room(app: &AppState, room: &SessionRow) -> ApiResult<bool> {
    let _room = crate::rooms::lock(app, room.id).await;
    // The agents' inputs stay as they are: the worker keeps the ones it has after a stop. A final
    // message on an input whose message is from before this line, the part of the turn the stop
    // cut included, posts nothing and wakes nobody (`rooms::on_final`).
    events::system(app, room, crate::rooms::ROOM_STOPPED, crate::rooms::STOPPED).await?;
    let mut sent = false;
    for m in db::room_members(&app.db, room.id).await? {
        sent |= stop_session(app, &m).await?;
        app.session_changed(m.id);
    }
    // After the agents are stopped: the senders of the room's messages are told how it ended, and
    // a notice that fails leaves no agent running.
    crate::rooms::round_ended(app, room, "stopped by the owner").await?;
    Ok(sent)
}

/// Longest `call_id` of `POST .../background` and of a kept foreground call, in bytes.
pub(crate) const MAX_CALL_ID: usize = 128;

#[derive(Debug, Deserialize)]
pub struct BackgroundRequest {
    pub call_id: String,
}

/// Move the agent's running call `call_id` (the client sends one of the session's
/// `foreground_calls`; a call not in the foreground the runtime does not move) to the
/// background: `session.background` to the worker, whose runtime's answer is only logged.
/// An empty `call_id` is refused: Claude Code takes it for every call in the foreground.
pub async fn background(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<BackgroundRequest>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Write).await?;
    not_system(&s)?;
    if b.call_id.is_empty() || b.call_id.len() > MAX_CALL_ID || b.call_id.contains('\0') {
        return Err(ApiError::BadRequest(format!(
            "call_id must be 1 to {MAX_CALL_ID} bytes without NUL"
        )));
    }
    let sent = send_to_worker(
        &app,
        &s,
        HubFrame::SessionBackground {
            session_id: id,
            call_id: b.call_id,
        },
    );
    Ok(Json(json!({ "sent": sent })))
}

/// End the agent's process and start it again at once, resuming the transcript (SPEC §17.3).
pub async fn restart(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Write).await?;
    not_system(&s)?;
    driven_from_hqc(&s)?;
    let sent = restart_agent(&app, &s).await;
    Ok(Json(json!({ "sent": sent })))
}

/// Ends the session's agent process and starts it again at once with a new `session.start`
/// (SPEC §17.3): a restart, and a change of provider (SPEC 0.144). `false`: its worker is
/// offline, so the next start carries what changed anyway.
pub(crate) async fn restart_agent(app: &AppState, s: &SessionRow) -> bool {
    let Some(conn) = s.worker_id.and_then(|w| app.workers.get(w)) else {
        return false;
    };
    {
        // The kill goes out under the lock, so its `stopped` finds `Started` only if the process
        // it ends had its start still unanswered at the press; then that `stopped` brings one
        // more `session.start`, which the worker acks without touching the new process. Inputs
        // sent to the old process go again after the start's `ack`.
        let mut st = conn.st.lock().await;
        conn.send(HubFrame::SessionKill { session_id: s.id });
        if let Some(sc) = st.sessions.get_mut(&s.id) {
            sc.start = StartState::NotStarted;
            sc.inflight.clear();
        }
    }
    delivery::kick(app, s.id, true).await;
    true
}

/// Longest `task_id` of `POST .../tasks/{task_id}/stop`, in bytes.
const MAX_TASK_ID: usize = 128;

/// Stop the agent's task `task_id` alone (the client sends a `stoppable` row of the session's
/// `subagents.live`; the id is not checked against it, the session is the user's own):
/// `session.stop_task` to the worker, whose runtime's answer is only logged.
pub async fn stop_task(
    State(app): State<AppState>,
    user: CurrentUser,
    Path((id, task_id)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Write).await?;
    not_system(&s)?;
    if task_id.is_empty() || task_id.len() > MAX_TASK_ID || task_id.contains('\0') {
        return Err(ApiError::BadRequest(format!(
            "task_id must be 1 to {MAX_TASK_ID} bytes without NUL"
        )));
    }
    let sent = send_to_worker(
        &app,
        &s,
        HubFrame::SessionStopTask {
            session_id: id,
            task_id,
        },
    );
    Ok(Json(json!({ "sent": sent })))
}

#[derive(Debug, Deserialize)]
pub struct SubagentHistoryQuery {
    /// The `event_id` of the last event the client shows, passed on to the worker.
    pub after: Option<Uuid>,
}

/// What subagent `task_id` of the session has done (since SPEC 0.181): the worker's
/// `session.subagent_history`, read from the subagent's transcript when asked and stored
/// nowhere; `?after=` goes with it. A `task_id` that is not 1 to 64 bytes of `[A-Za-z0-9_-]`, or
/// an `after` that is not a UUID: `400`; the session's worker offline `409 worker_offline`, no
/// answer `504 worker_timeout`.
pub async fn subagent_history(
    State(app): State<AppState>,
    user: CurrentUser,
    Path((id, task_id)): Path<(Uuid, String)>,
    Query(q): Query<SubagentHistoryQuery>,
) -> ApiResult<Json<api::SubagentHistoryPage>> {
    let s = session_for(&app, &user, id, Action::Read).await?;
    if !hqc_proto::frame::valid_history_task_id(&task_id) {
        return Err(ApiError::BadRequest(format!(
            "task_id must be 1 to {} bytes of [A-Za-z0-9_-]",
            hqc_proto::frame::MAX_HISTORY_TASK_ID
        )));
    }
    let offline = || ApiError::Conflict("worker_offline", "the worker is offline".into());
    let worker = s.worker_id.ok_or_else(offline)?;
    match ask_worker(&app, worker, |request_id| {
        HubFrame::SessionSubagentHistory(SubagentHistory {
            request_id,
            session_id: id,
            task_id,
            after: q.after,
            before: None,
        })
    })
    .await?
    {
        ResultBody::SubagentHistory(r) => Ok(Json(r)),
        other => Err(anyhow::anyhow!("session.subagent_history answered with {other:?}").into()),
    }
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

pub async fn archive(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Manage).await?;
    not_system(&s)?;
    archive_session(&app, &s).await?;
    let s = db::session(&app.db, id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(json!(state::view(&app, user.id, s).await?)))
}

/// SPEC §17.4: kill the process, cancel open requests, fail pending input.
async fn archive_session(app: &AppState, s: &SessionRow) -> ApiResult<()> {
    let id = s.id;
    sqlx::query(
        "UPDATE sessions SET archived = true, archived_at = now(), turn_open = false,
            updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .execute(&app.db)
    .await?;
    send_to_worker(app, s, HubFrame::SessionKill { session_id: id });
    if let Some(f) = cleanup_frame(app, s).await? {
        send_to_worker(app, s, f);
    }
    requests::cancel_open_of_session(app, id, CancelReason::Archived).await?;
    for row in db::move_inputs(&app.db, id, "pending", "failed").await? {
        delivery::notify_delivery(app, &row);
    }
    app.session_changed(id);
    Ok(())
}

pub async fn unarchive(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Manage).await?;
    // A room has no workspace of its own (SPEC §7.1.1).
    if s.workspace_id.is_none() && s.room_mode.is_none() {
        // Its workspace record was removed while it was archived (SPEC §13.2).
        return Err(ApiError::Conflict(
            "no_workspace",
            "the session's workspace was removed".into(),
        ));
    }
    sqlx::query(
        "UPDATE sessions SET archived = false, archived_at = NULL, updated_at = now() WHERE id = $1",
    )
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

/// `GET /api/requests`: the user's open requests (SPEC §6, §10.1).
pub async fn open_requests(
    State(app): State<AppState>,
    user: CurrentUser,
) -> ApiResult<Json<api::RequestList>> {
    Ok(Json(api::RequestList {
        requests: requests::open_of(&app, user.id).await?,
    }))
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

/// `PUT /api/sessions/{id}/favorite`: the session is one of the user's favorites (SPEC §6).
/// Reading the session is enough, as for the read cursor; a repeated `PUT` keeps the first
/// time. The user's clients get a `session.summary` with it.
pub async fn favorite_session(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    session_for(&app, &user, id, Action::Read).await?;
    let at = db::add_favorite(&app.db, db::Favorite::Session, user.id, id).await?;
    app.session_changed(id);
    Ok(Json(json!({ "favorite_at": at })))
}

/// `DELETE /api/sessions/{id}/favorite`: no longer a favorite, also when it was not one.
pub async fn unfavorite_session(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    session_for(&app, &user, id, Action::Read).await?;
    db::remove_favorite(&app.db, db::Favorite::Session, user.id, id).await?;
    app.session_changed(id);
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /api/projects/{id}/favorite`, as for a session. No frame: another device of the user
/// sees it with the next `GET /api/projects`.
pub async fn favorite_project(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    project_for(&app, &user, id).await?;
    let at = db::add_favorite(&app.db, db::Favorite::Project, user.id, id).await?;
    Ok(Json(json!({ "favorite_at": at })))
}

/// `DELETE /api/projects/{id}/favorite`.
pub async fn unfavorite_project(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    project_for(&app, &user, id).await?;
    db::remove_favorite(&app.db, db::Favorite::Project, user.id, id).await?;
    Ok(StatusCode::NO_CONTENT)
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
    let limits = app.limits.lock().unwrap().clone();
    let ws: Vec<Value> = rows
        .into_iter()
        .map(|w| {
            // The account's usage limits as the worker last reported them (SPEC §6).
            let limits = limits.get(&w.id).map(|(l, at)| {
                json!({"five_hour": l.five_hour, "seven_day": l.seven_day,
                    "status": l.status, "rate_limit_type": l.rate_limit_type,
                    "resets_at": l.resets_at, "reported_at": at})
            });
            json!({
                "limits": limits,
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
                "session_secrets": w.session_secrets,
                "browser": w.browser,
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
    // Cloudflare's country, as `CF-Connecting-IP`: behind another reverse proxy
    // (`HQC_CLIENT_IP_HEADER` set) the client could send any (SPEC §5.1, §20).
    let country = headers
        .get("cf-ipcountry")
        .filter(|_| app.config.client_ip_header.is_none())
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
    fn typed_models() {
        use super::typed_model;
        assert_eq!(typed_model("/model fable"), Some(Some("fable".into())));
        assert_eq!(
            typed_model("  /model  claude-fable-5-1 \n"),
            Some(Some("claude-fable-5-1".into()))
        );
        assert_eq!(typed_model("/model default"), Some(None));
        for no in [
            "/model",
            "/model a b",
            "/models x",
            "model x",
            "/model opus[1m]",
            "hi /model x",
        ] {
            assert_eq!(typed_model(no), None, "{no}");
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
