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
//!   which only the canonical path shows). The tool's move passes the canonical path it was
//!   decided on; a canonical path other than that one refuses the move before anything is
//!   recorded (the folder may have been swapped for a symlink since).
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
//!
//! The tool moves at once only into a workspace of the owner in the environment of the
//! session's folder now; anything else waits for the owner's answer, a PendingRequest of kind
//! [`KIND`] in the calling session (SPEC §3, §7, §10.1), unless the move would be refused
//! anyway ([`refused`]).

use std::path::Path;

use hqc_proto::{
    api::WorkspaceState,
    event::CancelReason,
    frame::{environment_for, worker_approval, McpCall, McpError, McpResult},
    mcp::error_code,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    api,
    app::AppState,
    db::{self, PendingRow, SessionRow},
    delivery,
    error::{ApiError, ApiResult},
    events,
    mcp::{args, internal, once_id, Answer},
    requests,
    worker_ws::StartState,
};

/// `system.code` written into the feed of a session that moved (SPEC §4).
pub const MOVED: &str = "session_moved";
/// `kind` of the PendingRequest of an `hqc_move` that waits for the owner (SPEC §3).
pub const KIND: &str = "move";
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
/// run by hqc (`locked_by: hqc`), in a turn without an `ask` cap (SPEC §7.1), looks at the
/// folder with `fs.stat` on the connection the call came by ([`Answer::Wait`]), creating
/// nothing ([`target`]). Into a workspace of the owner in the environment of its folder now
/// it moves at once by [`move_session`]; anything else that would not be refused anyway
/// ([`refused`]) waits for the owner's answer ([`ask_owner`]), and the call is answered later,
/// as `hqc_ask` is. Either way the move lands only in the canonical path it was decided on.
/// After the move an input of the owner, "you moved to …", goes into its queue as a run of
/// `hqc_schedule` does (SPEC §16): a `user.message` with the call's chain moved on, so the
/// turn it starts runs in the session's own mode and, after the move, in the new folder.
/// Once per `request_id`: a repeat finds the session moved and the input stored, or the
/// request it opened, and answers the same.
pub async fn tool(app: &AppState, caller: &SessionRow, c: &McpCall) -> Result<Answer, McpError> {
    let a: MoveArgs = args(c.args.clone())?;
    if let Some(p) = db::request_by_id(&app.db, caller.id, KIND, &c.request_id)
        .await
        .map_err(internal)?
    {
        // A repeat of a call that asked the owner (SPEC §5.2): waiting, or its outcome.
        return match p.result {
            None => Ok(Answer::Later),
            Some(frame) => {
                crate::peers::result_acked(app, p.id)
                    .await
                    .map_err(internal)?;
                let r: McpResult =
                    serde_json::from_value(frame["payload"].clone()).map_err(internal)?;
                r.outcome().map(Answer::Now)
            }
        };
    }
    if caller.locked_by != "hqc" {
        return Err(McpError::new(
            error_code::UNAVAILABLE,
            "the session's agent is not run by hqc now",
        ));
    }
    if let Some(by) = &caller.permission_capped_by {
        return Err(McpError::new(
            error_code::UNAVAILABLE,
            format!("this turn runs under the ask cap of {by}; the session does not move from it"),
        ));
    }
    // The chain moves on as for a schedule; one at its limit moves nothing (SPEC §7.1).
    let chain = crate::peers::outgoing_chain(app, caller, c, false).await?;
    let chain = serde_json::to_value(chain).map_err(internal)?;
    let wake = once_id(WAKE, caller.id, &c.request_id);
    // Off the connection's loop: the worker answers `fs.stat` on the connection the call came by.
    let (app, caller, request_id) = (app.clone(), caller.clone(), c.request_id.clone());
    Ok(Answer::Wait(Box::pin(async move {
        match target(&app, &caller, &a.path).await? {
            Target::Workspace(path) => {
                let project = a.project.as_deref();
                let m = move_session(&app, &caller, &path, project, false, Some(path.as_str()))
                    .await
                    .map_err(tool_error)?;
                woken(&app, &m, wake, chain).await.map(Some)
            }
            Target::Ask { path, why } => {
                let project = a.project.as_deref();
                refused(&app, &caller, &path, project)
                    .await
                    .map_err(tool_error)?;
                ask_owner(&app, &caller, &request_id, &path, project, &why, chain).await?;
                Ok(None)
            }
        }
    })))
}

/// After a move of the calling session: the owner's input "you moved to …" in its queue, in
/// `chain`, once per call (`wake`), and the call's result.
async fn woken(app: &AppState, m: &Moved, wake: Uuid, chain: Value) -> Result<Value, McpError> {
    let text = format!(
        "You moved to {}, project {}; continue the task.",
        m.workspace.path, m.project.name
    );
    events::store_hub_event_with_id(
        app,
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
    delivery::kick(app, m.session.id, false).await;
    let address = crate::peers::address(app, &m.session)
        .await
        .map_err(internal)?;
    Ok(json!({"address": address, "project": m.project.slug, "path": m.workspace.path}))
}

/// Where `hqc_move` goes: the path is the one the move is decided on, and the one it must land
/// in ([`move_session`]'s `decided`).
enum Target {
    /// Exactly a workspace of the owner on the session's worker, in the environment of the
    /// session's folder now: the session moves at once.
    Workspace(String),
    /// Anything else waits for the owner: the canonical path (as given, when missing), shown
    /// to the owner, and what the owner is asked about.
    Ask { path: String, why: String },
}

/// Where a move of `s` into `path` goes (SPEC §7): the worker's `fs.stat` without `create`,
/// the owner's workspaces on that worker and the environments of the worker's bindings (as
/// [`db::set_workspace_environments`] has them) for the target and for the session's folder.
/// A folder the worker refuses is the tool's error at once; a missing one asks.
async fn target(app: &AppState, s: &SessionRow, path: &str) -> Result<Target, McpError> {
    let worker = match s.worker_id {
        Some(w) => db::worker(&app.db, w).await.map_err(internal)?,
        None => None,
    }
    .ok_or_else(|| McpError::new(error_code::UNAVAILABLE, "the session has no worker"))?;
    let here = match s.workspace_id {
        Some(w) => db::workspace(&app.db, w).await.map_err(internal)?,
        None => None,
    }
    .ok_or_else(|| {
        McpError::new(
            error_code::UNAVAILABLE,
            "the session has no folder to move from",
        )
    })?;
    let (path, exists) = match api::stat_folder(app, worker.id, path, false).await {
        Ok(p) => (p, true),
        Err(ApiError::Invalid("path_not_found", _)) => (
            api::check_path(path).map_err(tool_error)?.to_string(),
            false,
        ),
        Err(e) => return Err(tool_error(e)),
    };
    let workspace = exists
        && db::overlapping_workspace(&app.db, worker.id, &path)
            .await
            .map_err(internal)?
            .is_some_and(|w| w.path == path && w.owner_id == s.owner_id);
    let bindings = db::env_bindings(worker.env_bindings);
    let (to, from) = (
        environment_for(&bindings, &path),
        environment_for(&bindings, &here.path),
    );
    if workspace && to == from {
        return Ok(Target::Workspace(path));
    }
    let folder = if !exists {
        "a new folder (made if you allow it)"
    } else if !workspace {
        "a folder that is not one of your workspaces yet"
    } else {
        "your workspace"
    };
    let env = if to == from {
        String::new()
    } else {
        format!(" in environment {to} (its folder now is in {from})")
    };
    Ok(Target::Ask {
        why: format!("{folder}{env}"),
        path,
    })
}

/// The owner is asked about the move of `s` into `path` (SPEC §7, §10.1): an `approval.request`
/// of the hub in the session's feed with the options `once` / `deny`, and an open request of
/// kind [`KIND`] keyed by the call's `request_id`, which keeps what the move needs (`move`:
/// the path, the project, the call's chain moved on). No expiry; a repeat of the call opens
/// none and stores no second event.
async fn ask_owner(
    app: &AppState,
    s: &SessionRow,
    request_id: &str,
    path: &str,
    project: Option<&str>,
    why: &str,
    chain: Value,
) -> Result<(), McpError> {
    let payload = json!({
        "request_id": request_id,
        "tool": hqc_proto::mcp::MOVE,
        "description": format!("The agent asks to move this session into {why}:\n{path}"),
        "options": [
            {"id": worker_approval::ONCE, "label": "Allow", "canonical": "once"},
            {"id": worker_approval::DENY, "label": "Deny", "canonical": "deny"},
        ],
    });
    let mut stored = payload.clone();
    stored["move"] = json!({"path": path, "project": project, "chain": chain});
    hqc_proto::replace_nul_json(&mut stored);
    if !db::open_request(&app.db, KIND, s.id, request_id, &stored, None, s.worker_id)
        .await
        .map_err(internal)?
    {
        return Ok(());
    }
    tracing::info!(session_id = %s.id, %request_id, %path, "hqc_move asks the owner");
    events::store_hub_event(
        app,
        s,
        json!({"kind": "system"}),
        "approval.request",
        payload,
        None,
        None,
    )
    .await
    .map_err(internal)?;
    Ok(())
}

/// The owner allowed the move of request `p` (closed already, without a result): the session
/// moves as the call asked, into the path the owner was shown, a missing folder made, and then
/// as [`tool`] goes on after a move; the call gets that result, or the move's error, as an
/// `mcp.result` of its own, kept in the request until the worker acknowledges it. Off the
/// answer's HTTP request, so that a client that goes away does not cut the move short.
pub fn allowed(app: &AppState, p: PendingRow) {
    let app = app.clone();
    tokio::spawn(async move {
        let r = match carry_out(&app, &p).await {
            Ok(v) => McpResult::ok(&p.request_id, v),
            Err(e) => McpResult::err(&p.request_id, &e.code, e.message),
        };
        tracing::info!(session_id = %p.session_id, request_id = %p.request_id, ok = r.error.is_none(), "allowed hqc_move carried out");
        let frame = json!({"type": "mcp.result", "payload": r});
        if let Err(e) = db::set_request_result(&app.db, p.id, &frame).await {
            tracing::error!(request_id = %p.request_id, error = %e, "the result of an allowed move not stored");
            return;
        }
        requests::deliver_result(
            &app,
            &PendingRow {
                result: Some(frame),
                ..p
            },
        )
        .await;
    });
}

async fn carry_out(app: &AppState, p: &PendingRow) -> Result<Value, McpError> {
    let m = &p.payload["move"];
    let path = m["path"]
        .as_str()
        .ok_or_else(|| internal("a move request without its path"))?;
    let s = db::session(&app.db, p.session_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| McpError::new(error_code::UNAVAILABLE, "the session is gone"))?;
    let moved = move_session(app, &s, path, m["project"].as_str(), true, Some(path))
        .await
        .map_err(tool_error)?;
    let wake = once_id(WAKE, s.id, &p.request_id);
    woken(app, &moved, wake, m["chain"].clone()).await
}

/// The `mcp.result` of a move the owner denied: nothing moved, nothing was made.
pub fn denied(request_id: &str) -> Value {
    json!({"type": "mcp.result", "payload": McpResult::err(
        request_id,
        error_code::UNAVAILABLE,
        "the user did not allow the move; the session stays where it is",
    )})
}

/// The `mcp.result` of a move whose request the hub cancelled (SPEC §10.1).
pub fn cancelled(request_id: &str, reason: CancelReason) -> Value {
    let why = serde_json::to_value(reason).unwrap_or_default();
    json!({"type": "mcp.result", "payload": McpResult::err(
        request_id,
        error_code::UNAVAILABLE,
        format!(
            "the request to the user to allow the move was cancelled ({}); the session stays where it is",
            why.as_str().unwrap_or("cancelled")
        ),
    )})
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
/// owner's) or a new project named after the folder. `decided`: the canonical path the move
/// was decided on (`hqc_move`; `None` from `POST /api/sessions/{id}/move`), which the worker's
/// `fs.stat` here must report, or nothing is recorded.
pub async fn move_session(
    app: &AppState,
    s: &SessionRow,
    path: &str,
    project: Option<&str>,
    create: bool,
    decided: Option<&str>,
) -> ApiResult<Moved> {
    let worker = movable(app, s).await?;
    if create {
        // Before the worker makes the folder: a refused move leaves none.
        overlap(app, worker.id, s, api::check_path(path)?).await?;
    }
    let path = api::stat_folder(app, worker.id, path, create).await?;
    // The folder may have been swapped for a symlink since the move was decided on.
    if let Some(d) = decided.filter(|d| Path::new(d) != Path::new(&path)) {
        return Err(ApiError::BadRequest(format!(
            "{d} is {path} now, not the folder the move was decided on; nothing moved: \
             call again with the canonical path"
        )));
    }
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

/// What refuses a move of `s` into `path` in `project` anyway, checked before the owner is
/// asked (SPEC §7): [`movable`], an unknown `project` (`404`) and an [`overlap`] of `path`.
async fn refused(
    app: &AppState,
    s: &SessionRow,
    path: &str,
    project: Option<&str>,
) -> ApiResult<()> {
    let worker = movable(app, s).await?;
    if let Some(p) = project {
        owner_project(app, s.owner_id, p).await?;
    }
    overlap(app, worker.id, s, path).await
}

/// `409 workspace_conflict` when `path` equals another owner's workspace on `worker`, lies
/// inside a workspace there or contains one; the owner's own workspace at exactly `path` is
/// none, but `409 workspace_not_ready` while it is not ready.
async fn overlap(app: &AppState, worker: Uuid, s: &SessionRow, path: &str) -> ApiResult<()> {
    if let Some(x) = db::overlapping_workspace(&app.db, worker, path).await? {
        if x.owner_id != s.owner_id || Path::new(&x.path) != Path::new(path) {
            return Err(api::workspace_conflict(app, path, &x).await?);
        }
        ready(&x)?;
    }
    Ok(())
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
