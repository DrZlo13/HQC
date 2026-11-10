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
//! The tool moves at once only into a ready workspace of the owner in the environment of the
//! session's folder now, both as the worker reports them; anything else waits for the owner's
//! answer, a PendingRequest of kind [`KIND`] in the calling session (SPEC §3, §7, §10.1),
//! unless the move would be refused anyway ([`refused`]).

use std::{path::Path, time::Duration};

use hqc_proto::{
    api::WorkspaceState,
    event::{CancelReason, Chain},
    frame::{worker_approval, McpCall, McpError, McpResult, PermissionMode},
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
/// The longest pause before the result of an allowed move is stored again ([`allowed`]).
const MAX_STORE_PAUSE: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveArgs {
    path: String,
    project: Option<String>,
}

/// `hqc_move(path, project?)` (SPEC §7): `{address, project, path}`. The calling session,
/// run by hqc (`locked_by: hqc`), in a turn whose chain is not capped to `ask` (SPEC §7.1;
/// a person's turn has no cap of its own, but one that carries buffered messages of a session
/// in `ask` has theirs), whatever the session's own mode, looks at the folder with
/// `fs.stat` on the connection the call came by ([`Answer::Wait`]), creating nothing
/// ([`target`]). Into a ready workspace of the owner in the environment of its folder now it
/// moves at once by [`move_session`]; anything else that would not be refused anyway
/// ([`refused`]) waits for the owner's answer ([`ask_owner`]), and the call is answered later,
/// as `hqc_ask` is. Either way the move lands only in the canonical path it was decided on.
/// After the move an input of the owner, "you moved to …", goes into its queue as a run of
/// `hqc_schedule` does (SPEC §16): a `user.message` with the call's chain moved on ([`woken`]),
/// so the turn it starts runs in the session's own mode and, after the move, in the new folder.
/// Once per `request_id`: a repeat finds the request it opened, or the input stored after a
/// move at once, and answers the same.
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
    let wake = once_id(WAKE, caller.id, &c.request_id);
    // A repeat of a move made at once (SPEC §5.2): its input is stored, and may have started
    // the turn the call is now said to run in. Answered from where the session is, before
    // anything that turn's chain decides.
    if db::event(&app.db, wake)
        .await
        .map_err(internal)?
        .is_some_and(|e| e.session_id == caller.id)
    {
        return here(app, caller).await.map(Answer::Now);
    }
    if caller.locked_by != "hqc" {
        return Err(McpError::new(
            error_code::UNAVAILABLE,
            "the session's agent is not run by hqc now",
        ));
    }
    // The chain the turn runs on, not the worker's report, which names a cap only while the
    // session runs in `auto`. Any `ask` cap refuses, one the session set itself too (the input
    // after its own move, an answer to its own message, a run of its own schedule).
    let turn = crate::peers::call_chain(app, caller, c)
        .await
        .map_err(internal)?;
    if let Some(t) = turn
        .as_ref()
        .filter(|t| t.permission_cap == Some(PermissionMode::Ask))
    {
        let by = t.capped_by.as_deref().unwrap_or("a session up the chain");
        return Err(McpError::new(
            error_code::UNAVAILABLE,
            format!("this turn runs under the ask cap of {by}; the session does not move from it"),
        ));
    }
    let me = crate::peers::address(app, caller).await.map_err(internal)?;
    // The chain moves on as for a schedule; one at its limit moves nothing (SPEC §7.1). The
    // input's chain is built after the move ([`woken`]), from the same turn.
    crate::peers::next_chain(turn.clone(), false, crate::peers::sender_mode(caller), &me)?;
    // Off the connection's loop: the worker answers `fs.stat` on the connection the call came by.
    let (app, caller, request_id) = (app.clone(), caller.clone(), c.request_id.clone());
    Ok(Answer::Wait(Box::pin(async move {
        match target(&app, &caller, &a.path).await? {
            Target::Workspace(path) => {
                let project = a.project.as_deref();
                let m = move_session(&app, &caller, &path, project, false, Some(path.as_str()))
                    .await
                    .map_err(tool_error)?;
                woken(&app, &m, wake, turn).await.map(Some)
            }
            Target::Ask { path, why } => {
                let project = a.project.as_deref();
                let (lands, mirrored) = refused(&app, &caller, &path, project)
                    .await
                    .map_err(tool_error)?;
                let mut description =
                    format!("The agent asks to move this session into {why}, {lands}:\n{path}");
                if mirrored {
                    description.push_str(
                        "\nIt becomes a mirrored workspace: Claude Code sessions in it are \
                         imported into hqc.",
                    );
                }
                let turn = serde_json::to_value(turn).map_err(internal)?;
                ask_owner(
                    &app,
                    &caller,
                    &request_id,
                    &path,
                    project,
                    &description,
                    turn,
                )
                .await?;
                Ok(None)
            }
        }
    })))
}

/// After a move of the calling session: the owner's input "you moved to …" in its queue, once
/// per call (`wake`), and the call's result. Its chain is that of the call's turn (`turn`, not
/// capped to `ask`: such a call is refused) moved on, built now, so a cap of the session's own
/// mode is named by its address in the new project (SPEC §7, §7.1).
async fn woken(
    app: &AppState,
    m: &Moved,
    wake: Uuid,
    turn: Option<Chain>,
) -> Result<Value, McpError> {
    let address = crate::peers::address(app, &m.session)
        .await
        .map_err(internal)?;
    let mode = crate::peers::sender_mode(&m.session);
    let chain = crate::peers::next_chain(turn, false, mode, &address)?;
    let chain = serde_json::to_value(chain).map_err(internal)?;
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
    Ok(json!({"address": address, "project": m.project.slug, "path": m.workspace.path}))
}

/// The result of `hqc_move` for session `s` where it is now: `{address, project, path}`.
async fn here(app: &AppState, s: &SessionRow) -> Result<Value, McpError> {
    let project = db::project(&app.db, s.project_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| internal("the session's project is gone"))?;
    let workspace = match s.workspace_id {
        Some(w) => db::workspace(&app.db, w).await.map_err(internal)?,
        None => None,
    }
    .ok_or_else(|| internal("the session has no workspace"))?;
    let address = crate::peers::address(app, s).await.map_err(internal)?;
    Ok(json!({"address": address, "project": project.slug, "path": workspace.path}))
}

/// Where `hqc_move` goes: the path is the one the move is decided on, and the one it must land
/// in ([`move_session`]'s `decided`).
enum Target {
    /// Exactly a ready workspace of the owner on the session's worker, in the environment of
    /// the session's folder now: the session moves at once.
    Workspace(String),
    /// Anything else waits for the owner: the canonical path (as given, when missing), shown
    /// to the owner, and what the owner is asked about.
    Ask { path: String, why: String },
}

/// What the card may not show of a path: control characters, the line and paragraph separators
/// (U+2028, U+2029), which break the line shown, and the bidirectional controls (U+061C,
/// U+200E, U+200F, U+202A–U+202E, U+2066–U+2069), which reorder the text around them.
pub(crate) fn unshowable(path: &str) -> bool {
    path.chars().any(|c| {
        c.is_control()
            || matches!(c, '\u{2028}' | '\u{2029}')
            || matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}')
            || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
    })
}

/// Where a move of `s` into `path` goes (SPEC §7): the worker's `fs.stat` without `create`
/// of the target, the owner's workspaces on that worker, and the environments the worker's
/// `fs.stat` names for the target and for the session's folder; one it does not name (an
/// older worker) asks. A folder the worker refuses is the tool's error at once; a missing one
/// asks. The path is the agent's text the owner may be shown: one with a control character or
/// a bidirectional control ([`unshowable`]) is refused, and so is a missing folder's with a
/// `.` or `..` component, and an existing folder whose canonical path (shown instead) has
/// such a character.
async fn target(app: &AppState, s: &SessionRow, path: &str) -> Result<Target, McpError> {
    if unshowable(path) {
        return Err(McpError::new(
            error_code::INVALID_ARGS,
            "path must not contain control characters or bidirectional controls",
        ));
    }
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
    let st = api::fs_stat(app, worker.id, path, false)
        .await
        .map_err(tool_error)?;
    let (path, exists) = match api::folder(path, &st) {
        Ok(p) if unshowable(&p) => {
            return Err(McpError::new(
                error_code::INVALID_ARGS,
                "the folder's canonical path contains control characters or bidirectional controls",
            ));
        }
        Ok(p) => (p, true),
        Err(ApiError::Invalid("path_not_found", _)) => {
            let p = api::check_path(path).map_err(tool_error)?;
            // Split by hand: `Path::components` drops a `.` inside the path.
            if p.split('/').any(|c| c == "." || c == "..") {
                return Err(McpError::new(
                    error_code::INVALID_ARGS,
                    format!("{p} does not exist; a new folder takes a path without `.` or `..`"),
                ));
            }
            (p.to_string(), false)
        }
        Err(e) => return Err(tool_error(e)),
    };
    let workspace = exists
        && db::overlapping_workspace(&app.db, worker.id, &path)
            .await
            .map_err(internal)?
            .is_some_and(|w| {
                w.path == path && w.owner_id == s.owner_id && w.state() == WorkspaceState::Ready
            });
    // As the worker resolves its bindings now, not as its `hello` reported them: the agent
    // starts in what the worker resolves.
    let to = st.environment;
    let from = api::fs_stat(app, worker.id, &here.path, false)
        .await
        .map_err(tool_error)?
        .environment;
    if workspace && to.is_some() && to == from {
        return Ok(Target::Workspace(path));
    }
    let folder = if !exists {
        "a new folder (made if you allow it)"
    } else if !workspace {
        "a folder that is not one of your workspaces yet"
    } else {
        "your workspace"
    };
    let env = match (to.as_deref(), from.as_deref()) {
        (Some(t), Some(f)) if t == f => String::new(),
        (Some(t), Some(f)) => format!(" in environment {t} (its folder now is in {f})"),
        (Some(t), None) => format!(
            " in environment {t} (the worker does not report the environment of its folder now)"
        ),
        (None, Some(f)) => {
            format!(" in an environment the worker does not report (its folder now is in {f})")
        }
        (None, None) => " in an environment the worker does not report".to_string(),
    };
    Ok(Target::Ask {
        why: format!("{folder}{env}"),
        path,
    })
}

/// The owner is asked about the move of `s` into `path` (SPEC §7, §10.1): an `approval.request`
/// of the hub in the session's feed with `description` and the options `once` / `deny`, and an
/// open request of kind [`KIND`] keyed by the call's `request_id`, which keeps what the move
/// needs (`move`: the path, the project, the chain of the call's turn). No expiry; a repeat of
/// the call opens none and stores no second event. One card per session: while a request of
/// another call waits, the call is `unavailable`.
async fn ask_owner(
    app: &AppState,
    s: &SessionRow,
    request_id: &str,
    path: &str,
    project: Option<&str>,
    description: &str,
    turn: Value,
) -> Result<(), McpError> {
    let payload = json!({
        "request_id": request_id,
        "tool": hqc_proto::mcp::MOVE,
        "description": description,
        "options": [
            {"id": worker_approval::ONCE, "label": "Allow", "canonical": "once"},
            {"id": worker_approval::DENY, "label": "Deny", "canonical": "deny"},
        ],
    });
    let mut stored = payload.clone();
    stored["move"] = json!({"path": path, "project": project, "turn": turn});
    hqc_proto::replace_nul_json(&mut stored);
    {
        // Held until the request is stored: parallel calls of one session open one card.
        let _lock = app.owner_lock("move request", s.owner_id).await;
        let open = db::open_requests_of_session(&app.db, s.id)
            .await
            .map_err(internal)?;
        if open
            .iter()
            .any(|p| p.kind == KIND && p.request_id != request_id)
        {
            return Err(McpError::new(
                error_code::UNAVAILABLE,
                "a move already waits for the user's answer",
            ));
        }
        if !db::open_request(&app.db, KIND, s.id, request_id, &stored, None, s.worker_id)
            .await
            .map_err(internal)?
        {
            return Ok(());
        }
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
/// answer's HTTP request, so that a client that goes away does not cut the move short. A result
/// that is not stored is stored again after a pause, a second at first, doubled each time up to
/// [`MAX_STORE_PAUSE`]; the move is not carried out again from here (only a hub started anew
/// does that, [`after_hello`]), and the result is delivered once stored.
pub fn allowed(app: &AppState, p: PendingRow) {
    let app = app.clone();
    tokio::spawn(async move {
        let r = match carry_out(&app, &p).await {
            Ok(v) => McpResult::ok(&p.request_id, v),
            Err(e) => McpResult::err(&p.request_id, &e.code, e.message),
        };
        tracing::info!(session_id = %p.session_id, request_id = %p.request_id, ok = r.error.is_none(), "allowed hqc_move carried out");
        let frame = json!({"type": "mcp.result", "payload": r});
        let mut pause = Duration::from_secs(1);
        while let Err(e) = db::set_request_result(&app.db, p.id, &frame).await {
            tracing::error!(request_id = %p.request_id, error = %e, retry_in_s = pause.as_secs(), "the result of an allowed move not stored; storing it again");
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(MAX_STORE_PAUSE);
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

/// On hub start, before it serves anything (SPEC §3 PendingRequest, §7): the moves the owner
/// allowed whose result the previous process did not store — it stopped while it carried them
/// out — are kept by their session's worker until that worker says `hello` ([`after_hello`]).
/// Only these: no move this process carries out is among them.
pub async fn left_on_start(app: &AppState) {
    let rows = match db::allowed_moves_without_result(&app.db).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(error = %e, "allowed moves without a result");
            return;
        }
    };
    let mut left = app.left_moves.lock().unwrap();
    for p in rows {
        // The worker's record is gone: nobody waits for the call.
        let Some(worker) = p.worker_id else { continue };
        tracing::info!(session_id = %p.session_id, request_id = %p.request_id, %worker, "an allowed hqc_move left without a result");
        left.entry(worker).or_default().push(p);
    }
}

/// After `hello` of `worker_id`: the moves the previous hub process left for it
/// ([`left_on_start`]) are carried out, once, so their calls get their results ([`allowed`]); a
/// move done before the stop answers as a repeat.
pub fn after_hello(app: &AppState, worker_id: Uuid) {
    let rows = app
        .left_moves
        .lock()
        .unwrap()
        .remove(&worker_id)
        .unwrap_or_default();
    for p in rows {
        tracing::info!(session_id = %p.session_id, request_id = %p.request_id, "an allowed hqc_move carried out after the hub restarted");
        allowed(app, p);
    }
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
    let turn: Option<Chain> = serde_json::from_value(m["turn"].clone()).map_err(internal)?;
    woken(app, &moved, wake, turn).await
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
        db::move_session(&app.db, &s, project.id, ws.id, ".").await?;
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

/// What refuses any move of session `s`, a move of this module and one after its transcript
/// alike (SPEC §7, §9.2): archived, not a Claude Code session, a subagent running; `409`
/// (`archived`, `unavailable`).
pub fn may_move(s: &SessionRow) -> ApiResult<()> {
    if s.archived {
        return Err(ApiError::Conflict(
            "archived",
            "the session is archived".into(),
        ));
    }
    if s.system || s.runtime != "claude_code" {
        return Err(unavailable("only a Claude Code session can move"));
    }
    // A background subagent outlives the turn: the agent, and its input with it, would wait in
    // its old folder for it. The hub sees only the count, so a foreground one refuses too.
    if s.subagents > 0 {
        return Err(unavailable(
            "a subagent of the session is still running: move once it has ended",
        ));
    }
    Ok(())
}

/// The worker of session `s` when nothing refuses its move; otherwise `409` (`archived`,
/// `unavailable`, `worker_offline`).
async fn movable(app: &AppState, s: &SessionRow) -> ApiResult<db::WorkerRow> {
    may_move(s)?;
    if s.workspace_id.is_none() {
        return Err(unavailable("the session has no folder to move from"));
    }
    if s.locked_by == "vscode" || s.held_by.is_some() {
        return Err(unavailable(
            "the session is driven or open outside hqc (VS Code): close it there first",
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
/// asked (SPEC §7): [`movable`], an unknown `project` (`404`), an [`overlap`] of `path` (with a
/// workspace of the owner there that is not ready), and the owner's workspace at `path` of
/// another project when `project` names one. Otherwise what the card says the move lands in:
/// `in project <name>` or `in a new project <folder name>`, and whether the folder becomes a
/// new workspace, mirrored.
async fn refused(
    app: &AppState,
    s: &SessionRow,
    path: &str,
    project: Option<&str>,
) -> ApiResult<(String, bool)> {
    let worker = movable(app, s).await?;
    let wanted = match project {
        Some(p) => Some(owner_project(app, s.owner_id, p).await?),
        None => None,
    };
    overlap(app, worker.id, s, path).await?;
    let Some(w) = db::overlapping_workspace(&app.db, worker.id, path)
        .await?
        .filter(|w| w.owner_id == s.owner_id && Path::new(&w.path) == Path::new(path))
    else {
        let lands = match wanted {
            Some(p) => format!("in project {}", p.name),
            None => format!("in a new project {}", folder_name(path)),
        };
        return Ok((lands, true));
    };
    if let Some(p) = wanted.filter(|p| p.id != w.project_id) {
        return Err(ApiError::BadRequest(format!(
            "{path} is a workspace of another project, not of {}",
            p.slug
        )));
    }
    let p = db::project(&app.db, w.project_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok((format!("in project {}", p.name), false))
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
    let name = folder_name(path);
    let p = db::create_project_as(&app.db, id, s.owner_id, &api::project_name(&name)?, "").await?;
    tracing::info!(project_id = %p.id, session_id = %s.id, "project created for a move");
    Ok(p)
}

/// The name of the project a move to `path` makes: the folder's last component, cut to
/// [`api::MAX_PROJECT_NAME`].
fn folder_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
        .chars()
        .take(api::MAX_PROJECT_NAME)
        .collect()
}
