//! Workspaces hqc makes and deletes on a worker (T45, T46, SPEC §3 Workspace, §5.3, §6, §13).
//!
//! A clone or a worktree is stored as `creating` and sent as `workspace.create` (`request_id`
//! = the workspace's id). The worker acks it at once and answers `workspace.created` when git
//! is done; the workspace becomes `ready` at the canonical path it reports, or `failed` with
//! its error. While it is `creating` the hub sends the request again after every `hello` of
//! the same worker process, and the worker answers a request it knows with its outcome; a new
//! `boot_id` fails it (the worker removed the partial folder when it started).
//!
//! A worktree (T46) is made from an origin: a ready workspace of the same project and worker
//! that is not a worktree itself. The hub sends the origin's `workspace.rules` before the
//! request, so the worker knows the origin as a workspace even if no session ran there yet.
//!
//! Deleting a folder hqc created asks the worker first (`workspace.inspect`, shown in the
//! client's confirmation) and then sends `workspace.delete`; the worker removes only a folder
//! of its own list.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use hqc_proto::{
    api::{self, CreateWorkspace, WorkspaceInspection, WorkspaceState},
    frame::{
        check_branch, check_clone_url, FsStat, HubFrame, ResultBody, WorkspaceCreate,
        WorkspaceCreated, WorkspaceDelete, WorkspacePath, WorkspaceSource,
    },
};
use uuid::Uuid;

use crate::{
    api::{ask_worker, ask_worker_for, check_path, workspace_for, workspace_insert},
    app::AppState,
    auth::CurrentUser,
    db,
    error::{ApiError, ApiResult},
    worker_ws::{Reply, WorkerConn},
};

/// How long the hub waits for `workspace.delete`: removing a big checkout takes a while.
const DELETE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// What the client shows when the worker restarted while it made the folder.
pub const RESTARTED: &str = "the worker restarted while it made this folder; it removed it";

/// `POST /api/workspaces {kind: clone, ...}`: `path` is the new folder ([`new_folder`]).
/// Answers `201` with the workspace `creating`.
pub async fn create_clone(
    app: &AppState,
    project: &db::ProjectRow,
    worker: &db::WorkerRow,
    owner_id: Uuid,
    b: CreateWorkspace,
    label: &str,
    mirror_enabled: bool,
) -> ApiResult<(StatusCode, Json<api::Workspace>)> {
    let url = b.url.as_deref().map(str::trim).unwrap_or("");
    check_clone_url(url).map_err(|e| ApiError::Invalid("invalid_url", e))?;
    let branch = b.branch.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if let Some(br) = branch {
        check_branch(br).map_err(|e| ApiError::Invalid("invalid_branch", e))?;
    }
    let canonical = new_folder(app, worker.id, &b.path).await?;
    make(
        app,
        db::NewWorkspace {
            project_id: project.id,
            owner_id,
            worker_id: worker.id,
            path: &canonical,
            label,
            mirror_enabled,
            source: db::Source::Clone { url, branch },
            ephemeral: None,
        },
    )
    .await
}

/// `POST /api/workspaces {kind: worktree, origin_workspace_id, branch, new_branch, path?}`
/// (T46): the origin is a ready workspace of the same project and worker and not a worktree
/// itself (`400 invalid_origin`); whether it is a git repository only the worker knows (the
/// worktree fails then). `path` defaults to `<origin-parent>/<origin-name>-<branch>` with the
/// branch's slashes as `-`, and is checked as a clone's ([`new_folder`]). `ephemeral`: the id
/// of `hqc_spawn`'s worktree for the session it starts (T47).
#[allow(clippy::too_many_arguments)]
pub async fn create_worktree(
    app: &AppState,
    project: &db::ProjectRow,
    worker: &db::WorkerRow,
    owner_id: Uuid,
    b: CreateWorkspace,
    label: &str,
    mirror_enabled: bool,
    ephemeral: Option<Uuid>,
) -> ApiResult<(StatusCode, Json<api::Workspace>)> {
    let invalid = |m: &str| ApiError::Invalid("invalid_origin", m.into());
    let id = b
        .origin_workspace_id
        .ok_or_else(|| invalid("a worktree needs origin_workspace_id"))?;
    let origin = match db::workspace(&app.db, id).await? {
        Some(o) if o.owner_id == owner_id => o,
        _ => return Err(invalid("origin_workspace_id is not a workspace of yours")),
    };
    if origin.project_id != project.id || origin.worker_id != worker.id {
        return Err(invalid(
            "the origin is a workspace of another project or worker",
        ));
    }
    if origin.kind == "worktree" {
        return Err(invalid(
            "the origin is a worktree itself: make the worktree from its origin",
        ));
    }
    if origin.state() != WorkspaceState::Ready {
        return Err(invalid("the origin's folder is not ready"));
    }
    let branch = b.branch.as_deref().map(str::trim).unwrap_or("");
    check_branch(branch).map_err(|e| ApiError::Invalid("invalid_branch", e))?;
    let path = match b.path.trim() {
        "" => default_worktree_path(&origin.path, branch),
        p => p.to_string(),
    };
    let canonical = new_folder(app, worker.id, &path).await?;
    make(
        app,
        db::NewWorkspace {
            project_id: project.id,
            owner_id,
            worker_id: worker.id,
            path: &canonical,
            label,
            mirror_enabled,
            source: db::Source::Worktree {
                origin: origin.id,
                branch,
                new_branch: b.new_branch,
            },
            ephemeral,
        },
    )
    .await
}

/// `<origin-parent>/<origin-name>-<branch>`, the branch's slashes as `-` (T46).
fn default_worktree_path(origin: &str, branch: &str) -> String {
    format!(
        "{}-{}",
        origin.trim_end_matches('/'),
        branch.replace('/', "-")
    )
}

/// The new folder of a clone or a worktree: its parent must be a folder inside
/// `allowed_roots` (`fs.stat`), and the folder itself must not exist (`400 path_exists`).
/// Returns the canonical parent plus the name; the worker checks it all again.
async fn new_folder(app: &AppState, worker_id: Uuid, path: &str) -> ApiResult<String> {
    let path = std::path::Path::new(check_path(path)?);
    let bad = || {
        ApiError::BadRequest(
            "path must be an absolute path that ends in the new folder's name".into(),
        )
    };
    let (Some(parent), Some(std::path::Component::Normal(name))) =
        (path.parent(), path.components().next_back())
    else {
        return Err(bad());
    };
    let Some(name) = name.to_str().filter(|_| path.is_absolute()) else {
        return Err(bad());
    };
    let parent = stat(app, worker_id, &parent.to_string_lossy()).await?;
    if !parent.inside_allowed_roots {
        return Err(ApiError::Invalid(
            "outside_allowed_roots",
            format!("{} is outside the worker's allowed_roots", path.display()),
        ));
    }
    let Some(parent_path) = parent.canonical.filter(|_| parent.exists) else {
        return Err(ApiError::Invalid(
            "path_not_found",
            "the parent folder does not exist on the worker".into(),
        ));
    };
    if !parent.is_dir {
        return Err(ApiError::Invalid(
            "not_a_directory",
            format!("{parent_path} is not a directory"),
        ));
    }
    let canonical = format!("{}/{name}", parent_path.trim_end_matches('/'));
    if stat(app, worker_id, &canonical).await?.exists {
        return Err(ApiError::Invalid(
            "path_exists",
            format!("{canonical} already exists on the worker"),
        ));
    }
    Ok(canonical)
}

/// Stores the new workspace `creating` (`409 workspace_conflict` if it overlaps another) and
/// sends `workspace.create`; `201`.
async fn make(
    app: &AppState,
    n: db::NewWorkspace<'_>,
) -> ApiResult<(StatusCode, Json<api::Workspace>)> {
    let ws = workspace_insert(app, n).await?;
    match app.workers.get(ws.worker_id) {
        Some(conn) => send_create(app, &conn, &ws).await?,
        // Gone since the `fs.stat`: sent again after its `hello`.
        None => {
            tracing::info!(workspace_id = %ws.id, "worker went offline before workspace.create")
        }
    }
    tracing::info!(workspace_id = %ws.id, project_id = %ws.project_id, worker_id = %ws.worker_id, path = %ws.path, kind = %ws.kind, "workspace requested");
    Ok((StatusCode::CREATED, Json(ws.to_api())))
}

async fn stat(
    app: &AppState,
    worker_id: Uuid,
    path: &str,
) -> ApiResult<hqc_proto::frame::FsStatResult> {
    let path = path.to_string();
    match ask_worker(app, worker_id, |request_id| {
        HubFrame::FsStat(FsStat { request_id, path })
    })
    .await?
    {
        ResultBody::FsStat(st) => Ok(st),
        other => Err(anyhow::anyhow!("fs.stat answered with {other:?}").into()),
    }
}

/// `workspace.create` for a `creating` workspace; an `error` to it fails the workspace. A
/// worktree's origin goes first as `workspace.rules`, which makes it a workspace the worker
/// knows.
pub async fn send_create(
    app: &AppState,
    conn: &WorkerConn,
    ws: &db::WorkspaceRow,
) -> anyhow::Result<()> {
    let source = match (&ws.clone_url, ws.origin_workspace_id) {
        (Some(url), _) => WorkspaceSource::Clone {
            url: url.clone(),
            branch: ws.branch.clone(),
        },
        (None, Some(origin)) => {
            // The key and the delete check keep the origin while its worktree exists.
            let Some(origin) = db::workspace(&app.db, origin).await? else {
                db::workspace_failed(&app.db, ws.id, "its origin workspace is gone").await?;
                return Ok(());
            };
            crate::rules::send_to(app, conn, &origin).await?;
            WorkspaceSource::Worktree {
                origin: origin.path,
                branch: ws.branch.clone().unwrap_or_default(),
                new_branch: ws.new_branch,
            }
        }
        (None, None) => return Ok(()),
    };
    let frame = HubFrame::WorkspaceCreate(WorkspaceCreate {
        request_id: ws.id.to_string(),
        path: ws.path.clone(),
        source,
    });
    let mut st = conn.st.lock().await;
    let id = conn.send(frame);
    st.replies.insert(
        id,
        Reply::WorkspaceCreate {
            workspace_id: ws.id,
        },
    );
    Ok(())
}

/// `workspace.created` (acked by the caller): the clone or worktree is ready or failed. An outcome for a
/// workspace that is not `creating` any more (answered before, deleted) changes nothing.
pub async fn created(
    app: &AppState,
    conn: &WorkerConn,
    c: &WorkspaceCreated,
) -> anyhow::Result<()> {
    let Ok(id) = c.request_id.parse::<Uuid>() else {
        tracing::warn!(worker_id = %conn.worker_id, request_id = %c.request_id, "workspace.created for no workspace");
        return Ok(());
    };
    let ws = match db::workspace(&app.db, id).await? {
        Some(ws) if ws.worker_id == conn.worker_id => ws,
        _ => {
            tracing::info!(worker_id = %conn.worker_id, workspace_id = %id, "workspace.created for no workspace of this worker");
            return Ok(());
        }
    };
    match (&c.canonical, &c.error) {
        (Some(canonical), None) if valid_canonical(canonical) => {
            if let Some(ws) = db::workspace_created(&app.db, ws.id, canonical).await? {
                tracing::info!(workspace_id = %ws.id, path = %ws.path, "workspace ready");
                if ws.mirror_enabled {
                    crate::mirror::enable(app, conn, &ws).await?;
                }
            }
        }
        _ => {
            let error = c
                .error
                .as_deref()
                .map(|e| {
                    hqc_proto::replace_nul(e)
                        .chars()
                        .take(2000)
                        .collect::<String>()
                })
                .unwrap_or_else(|| "the worker reported no folder".into());
            if db::workspace_failed(&app.db, ws.id, &error)
                .await?
                .is_some()
            {
                tracing::info!(workspace_id = %ws.id, %error, "workspace failed");
            }
        }
    }
    Ok(())
}

/// An absolute path without `.`/`..` and within the path limit: the overlap checks compare
/// components.
fn valid_canonical(p: &str) -> bool {
    let path = std::path::Path::new(p);
    path.is_absolute()
        && check_path(p).is_ok()
        && path.components().all(|c| {
            matches!(
                c,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
}

/// The worker refused the `workspace.create` frame itself (a worker before T45, or before T46
/// for a worktree).
pub async fn refused(app: &AppState, workspace_id: Uuid, message: &str) -> anyhow::Result<()> {
    let error = format!("the worker refused the request: {message}");
    db::workspace_failed(&app.db, workspace_id, &error).await?;
    Ok(())
}

/// After `hello`: a new worker process lost what it was making; otherwise each `creating`
/// workspace is asked for again (the worker answers a known one with its outcome).
pub async fn after_hello(app: &AppState, conn: &WorkerConn, restarted: bool) -> anyhow::Result<()> {
    for ws in db::creating_workspaces(&app.db, conn.worker_id).await? {
        if restarted {
            db::workspace_failed(&app.db, ws.id, RESTARTED).await?;
            tracing::info!(workspace_id = %ws.id, "workspace failed: the worker restarted");
        } else {
            send_create(app, conn, &ws).await?;
        }
    }
    Ok(())
}

/// `GET /api/workspaces/{id}/inspect`: the worker's `workspace.inspect` of a folder hqc
/// created (`400 bad_request` for another workspace), for the deletion dialog; the worker must
/// be online.
pub async fn inspect(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<WorkspaceInspection>> {
    let ws = workspace_for(&app, &user, id).await?;
    if !ws.created_by_hqc || ws.state() != WorkspaceState::Ready {
        return Err(ApiError::BadRequest(
            "only a ready folder hqc created is inspected".into(),
        ));
    }
    let path = ws.path.clone();
    match ask_worker(&app, ws.worker_id, |request_id| {
        HubFrame::WorkspaceInspect(WorkspacePath { request_id, path })
    })
    .await?
    {
        ResultBody::WorkspaceInspect(r) => Ok(Json(r)),
        other => Err(anyhow::anyhow!("workspace.inspect answered with {other:?}").into()),
    }
}

/// `workspace.delete` of a folder hqc created, before its record goes; the worker must be
/// online and removes only a folder of its own list. `force`: the user confirmed the
/// uncommitted files the dialog showed, which `git worktree remove` keeps otherwise (T46).
pub async fn remove_folder(app: &AppState, ws: &db::WorkspaceRow, force: bool) -> ApiResult<()> {
    let path = ws.path.clone();
    match ask_worker_for(app, ws.worker_id, DELETE_TIMEOUT, |request_id| {
        HubFrame::WorkspaceDelete(WorkspaceDelete {
            request_id,
            path,
            force,
        })
    })
    .await?
    {
        ResultBody::WorkspaceDelete(r) => {
            tracing::info!(workspace_id = %ws.id, path = %ws.path, removed = r.removed, force, "workspace folder deleted");
            Ok(())
        }
        other => Err(anyhow::anyhow!("workspace.delete answered with {other:?}").into()),
    }
}
