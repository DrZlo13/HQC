//! A session moves to another folder of its worker, in another project (SPEC §3 Session,
//! §6, §7): `POST /api/sessions/{id}/move` and the agent's `hqc_move` ([`tool`]) go through
//! [`move_session`].
//!
//! - Only a Claude Code session with a workspace, not archived, neither driven from VS Code
//!   (`locked_by: vscode`) nor held open by a live process outside hqc (`held_by`, SPEC
//!   §9.3), with no subagent running (`subagents`, a foreground one too), whose worker is
//!   its owner's, online and reports `claude_code.moves` (SPEC §5.4); otherwise `409`.
//!   Checked again on the row read anew under the owner's lock: it may change during the
//!   worker's round-trip.
//! - The worker checks the folder with `fs.stat` (`create`: it makes a missing one), with the
//!   codes of `POST /api/workspaces`; the canonical path it reports is the target. With
//!   `create` the overlap and readiness checks below run on the requested path first, so a
//!   refused move makes no folder (unless the path reaches a workspace through a symlink,
//!   which only the canonical path shows).
//! - A workspace of the owner on that worker at exactly that path is taken with its project
//!   (`project`, when given, must be that one), unless it is not `ready` (`409
//!   workspace_not_ready`, as for a new session); a path that equals another owner's
//!   workspace, lies inside one or contains one is `409 workspace_conflict`, and no record is
//!   created.
//! - Otherwise the folder becomes a new `dir` workspace, `ready` and mirrored, of `project`
//!   (a slug or id of the owner's) or of a new project named after the folder's last
//!   component, whose id is derived from the session and the path ([`crate::mcp::once_id`]),
//!   so a repeat finds it. The project's first workspace becomes its default.
//! - A session already in that workspace is a repeat: the same result, nothing written; the
//!   `mirror.set` and the start below go out again, as the first call may have ended before
//!   them.
//! - One UPDATE moves the session ([`db::move_session`]: `project_id`, `workspace_id`, `cwd =
//!   '.'`, the slug kept when free there); its old project's saved orders drop it; `system
//!   session_moved` goes into its feed; then the workspace's `mirror.set`, so its `since`
//!   counts the session.
//! - The connection's start state goes back to "not started" and a running agent gets a new
//!   `session.start` with the new folder at once, which the worker applies once no turn runs
//!   (SPEC §5.3); a stopped agent starts there with its next input. Nothing is queued; only
//!   the tool queues the input that wakes the agent there.

use std::path::Path;

use hqc_proto::{
    api::WorkspaceState,
    frame::{McpCall, McpError},
    mcp::error_code,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::{
    api,
    app::AppState,
    db::{self, SessionRow},
    delivery,
    error::{ApiError, ApiResult},
    events,
    mcp::{args, internal, once_id, Answer},
    worker_ws::StartState,
};

/// `system.code` written into the feed of a session that moved (SPEC §4).
pub const MOVED: &str = "session_moved";
/// What [`crate::mcp::once_id`] derives the id of a move's new project from.
const PROJECT: &str = "hqc_move project";
/// What [`crate::mcp::once_id`] derives the `event_id` of the input after a tool move from.
const WAKE: &str = "hqc_move wake";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveArgs {
    path: String,
    project: Option<String>,
}

/// `hqc_move(path, project?)` (SPEC §7): `{address, project, path}`. The calling session,
/// run by hqc (`locked_by: hqc`), moves by [`move_session`], a missing folder made; the call
/// waits for the worker's `fs.stat` on the connection it came by ([`Answer::Wait`]). Then an
/// input of the owner, "you moved to …", goes into its queue as a run of `hqc_schedule` does
/// (SPEC §16): a `user.message` with the call's chain moved on, so the turn it starts runs in
/// the session's own mode (or the `ask` cap of the chain) and, after the move, in the new
/// folder. Once per `request_id`: a repeat finds the session moved and the input stored, and
/// answers the same.
pub async fn tool(app: &AppState, caller: &SessionRow, c: &McpCall) -> Result<Answer, McpError> {
    let a: MoveArgs = args(c.args.clone())?;
    if caller.locked_by != "hqc" {
        return Err(McpError::new(
            error_code::UNAVAILABLE,
            "the session's agent is not run by hqc now",
        ));
    }
    // The chain moves on as for a schedule; one at its limit moves nothing (SPEC §7.1).
    let chain = crate::peers::outgoing_chain(app, caller, c, false).await?;
    let chain = serde_json::to_value(chain).map_err(internal)?;
    let wake = once_id(WAKE, caller.id, &c.request_id);
    // Off the connection's loop: the worker answers `fs.stat` on the connection the call came by.
    let (app, caller) = (app.clone(), caller.clone());
    Ok(Answer::Wait(Box::pin(async move {
        let m = move_session(&app, &caller, &a.path, a.project.as_deref(), true)
            .await
            .map_err(tool_error)?;
        let text = format!(
            "You moved to {}, project {}; continue the task.",
            m.workspace.path, m.project.name
        );
        events::store_hub_event_with_id(
            &app,
            &m.session,
            wake,
            events::user_author(m.session.owner_id),
            "user.message",
            json!({"text": text, "artifact_ids": []}),
            Some("pending"),
            Some(chain),
        )
        .await
        .map_err(internal)?;
        delivery::kick(&app, m.session.id, false).await;
        let address = crate::peers::address(&app, &m.session)
            .await
            .map_err(internal)?;
        Ok(json!({"address": address, "project": m.project.slug, "path": m.workspace.path}))
    })))
}

/// An error of [`move_session`] as the tool's (SPEC §7), after `spawn`'s: a folder the worker
/// refuses or cannot make and a project that does not fit the folder are `invalid_args` with
/// the reason, an overlap `invalid_args` naming `workspace_conflict`, an unknown project
/// `not_found`, a session or worker that cannot move now `unavailable`.
fn tool_error(e: ApiError) -> McpError {
    match e {
        ApiError::NotFound => McpError::new(error_code::NOT_FOUND, "no such project"),
        ApiError::ConflictWith(code @ "workspace_conflict", m, _) => {
            McpError::new(error_code::INVALID_ARGS, format!("{code}: {m}"))
        }
        ApiError::BadRequest(m) | ApiError::Invalid(_, m) => {
            McpError::new(error_code::INVALID_ARGS, m)
        }
        ApiError::Conflict(_, m) | ApiError::ConflictWith(_, m, _) => {
            McpError::new(error_code::UNAVAILABLE, m)
        }
        other => internal(other),
    }
}

/// Where the session is now.
pub struct Moved {
    pub session: SessionRow,
    pub project: db::ProjectRow,
    pub workspace: db::WorkspaceRow,
}

fn unavailable(message: &str) -> ApiError {
    ApiError::Conflict("unavailable", message.into())
}

/// Moves session `s` into folder `path` of its worker, in `project` (a slug or id of the
/// owner's) or a new project named after the folder.
pub async fn move_session(
    app: &AppState,
    s: &SessionRow,
    path: &str,
    project: Option<&str>,
    create: bool,
) -> ApiResult<Moved> {
    let worker = movable(app, s).await?;
    if create {
        // Before the worker makes the folder: a refused move leaves none.
        let requested = api::check_path(path)?;
        if let Some(x) = db::overlapping_workspace(&app.db, worker.id, requested).await? {
            if x.owner_id != s.owner_id || Path::new(&x.path) != Path::new(requested) {
                return Err(api::workspace_conflict(app, requested, &x).await?);
            }
            ready(&x)?;
        }
    }
    let path = api::stat_folder(app, worker.id, path, create).await?;
    // Held until the session is moved: a repeat that comes in parallel finds what this made.
    let _lock = app.owner_lock("move", s.owner_id).await;
    let s = db::session(&app.db, s.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // The row may have changed during the worker's round-trip.
    movable(app, &s).await?;
    let wanted = match project {
        Some(p) => Some(owner_project(app, s.owner_id, p).await?),
        None => None,
    };
    let ws = match db::overlapping_workspace(&app.db, worker.id, &path).await? {
        Some(w) if w.path == path && w.owner_id == s.owner_id => {
            if let Some(p) = wanted.as_ref().filter(|p| p.id != w.project_id) {
                return Err(ApiError::BadRequest(format!(
                    "{path} is a workspace of another project, not of {}",
                    p.slug
                )));
            }
            ready(&w)?;
            w
        }
        Some(x) => return Err(api::workspace_conflict(app, &path, &x).await?),
        None => {
            let project = match wanted {
                Some(p) => p,
                None => new_project(app, &s, &path).await?,
            };
            let ws = api::workspace_insert(
                app,
                db::NewWorkspace {
                    project_id: project.id,
                    owner_id: s.owner_id,
                    worker_id: worker.id,
                    path: &path,
                    label: "",
                    mirror_enabled: true,
                    source: db::Source::Dir,
                    ephemeral: None,
                },
            )
            .await?;
            tracing::info!(workspace_id = %ws.id, project_id = %ws.project_id, path = %ws.path, "workspace added for a move");
            ws
        }
    };
    let project = db::project(&app.db, ws.project_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // A session there already is a repeat: it is not moved again. What follows goes out
    // again, as the first call may have ended before it (the client went away, a database
    // error); none of it changes anything twice.
    if s.workspace_id != Some(ws.id) {
        let from = db::project(&app.db, s.project_id)
            .await?
            .map_or_else(|| "?".to_string(), |p| p.name);
        db::move_session(&app.db, &s, project.id, ws.id).await?;
        // The row is moved: the next input starts the agent in the new folder, also when a
        // step below fails and the input goes out before a repeat of the move.
        if let Some(conn) = app.workers.get(worker.id) {
            conn.st.lock().await.sessions.entry(s.id).or_default().start = StartState::NotStarted;
        }
        let moved = db::session(&app.db, s.id)
            .await?
            .ok_or(ApiError::NotFound)?;
        events::system(
            app,
            &moved,
            MOVED,
            &format!(
                "The session moved from {from} to {} ({})",
                project.name, ws.path
            ),
        )
        .await?;
        tracing::info!(session_id = %s.id, project_id = %project.id, workspace_id = %ws.id, path = %ws.path, slug = %moved.slug, "session moved");
    }
    if let Some(conn) = app.workers.get(worker.id) {
        // After the move: `since` of the workspace's sessions counts this one (SPEC §9.1).
        if ws.mirror_enabled {
            crate::mirror::enable(app, &conn, &ws).await?;
        }
        conn.st.lock().await.sessions.entry(s.id).or_default().start = StartState::NotStarted;
    }
    delivery::kick(app, s.id, s.process_state != "stopped").await;
    app.session_changed(s.id);
    let session = db::session(&app.db, s.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Moved {
        session,
        project,
        workspace: ws,
    })
}

/// Workspace `w` takes the session, as for a new session (T45): a clone that runs or failed
/// has no folder to run in, `409 workspace_not_ready`.
fn ready(w: &db::WorkspaceRow) -> ApiResult<()> {
    match w.state() {
        WorkspaceState::Ready => Ok(()),
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

/// The worker of session `s` when nothing refuses its move; otherwise `409` (`archived`,
/// `unavailable`, `worker_offline`).
async fn movable(app: &AppState, s: &SessionRow) -> ApiResult<db::WorkerRow> {
    if s.archived {
        return Err(ApiError::Conflict(
            "archived",
            "the session is archived".into(),
        ));
    }
    if s.system || s.runtime != "claude_code" {
        return Err(unavailable("only a Claude Code session can move"));
    }
    if s.workspace_id.is_none() {
        return Err(unavailable("the session has no folder to move from"));
    }
    if s.locked_by == "vscode" || s.held_by.is_some() {
        return Err(unavailable(
            "the session is driven or open outside hqc (VS Code): close it there first",
        ));
    }
    // A background subagent outlives the turn: the agent, and its input with it, would wait in
    // its old folder for it. The hub sees only the count, so a foreground one refuses too.
    if s.subagents > 0 {
        return Err(unavailable(
            "a subagent of the session is still running: move once it has ended",
        ));
    }
    let worker = match s.worker_id {
        Some(w) => db::worker(&app.db, w).await?,
        None => None,
    }
    // As for `POST /api/workspaces`: the folder becomes a workspace of the owner there.
    .filter(|w| w.owner_id == s.owner_id && w.revoked_at.is_none())
    .ok_or_else(|| unavailable("the session has no worker of its owner"))?;
    if !app.workers.is_online(worker.id) {
        return Err(ApiError::Conflict(
            "worker_offline",
            "the worker is offline".into(),
        ));
    }
    if worker.capabilities["claude_code"]["moves"] != true {
        return Err(unavailable(
            "the session's worker cannot move a session; update it",
        ));
    }
    Ok(worker)
}

/// The owner's project by slug or id; another owner's is `404`, as a missing one.
async fn owner_project(app: &AppState, owner: Uuid, p: &str) -> ApiResult<db::ProjectRow> {
    let id = p.parse::<Uuid>().ok();
    db::projects_of(&app.db, owner)
        .await?
        .into_iter()
        .find(|x| Some(x.id) == id || x.slug == p)
        .ok_or(ApiError::NotFound)
}

/// The project a move of `s` to `path` makes, named after the folder; found again by its
/// derived id on a repeat.
async fn new_project(app: &AppState, s: &SessionRow, path: &str) -> ApiResult<db::ProjectRow> {
    let id = crate::mcp::once_id(PROJECT, s.id, path);
    if let Some(p) = db::project(&app.db, id).await? {
        if p.owner_id != s.owner_id {
            return Err(anyhow::anyhow!("project {id} is not the session owner's").into());
        }
        return Ok(p);
    }
    let name: String = Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
        .chars()
        .take(api::MAX_PROJECT_NAME)
        .collect();
    let p = db::create_project_as(&app.db, id, s.owner_id, &api::project_name(&name)?, "").await?;
    tracing::info!(project_id = %p.id, session_id = %s.id, "project created for a move");
    Ok(p)
}
