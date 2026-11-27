//! `hqc_spawn` (SPEC §7, §13.4): an agent starts a session of its owner and gives it its
//! first input; `hqc_room` ([`room`]) starts a room of agents the same way.
//!
//! - The session is created by the path of `POST /api/projects/{id}/sessions`
//!   ([`api::new_session`]): a project of the caller's owner (slug or id), a workspace of
//!   that project (id, label or path; the project's `default_workspace_id` when absent) on a
//!   worker of the owner; runtime `claude_code` only until iteration 5; `model` absent is the
//!   calling session's model.
//! - `prompt` is a `peer.message` (`mode: send`) from the calling session into the input
//!   queue of the new one, so it starts its first turn and the new agent knows whom to
//!   answer with `hqc_send`. The chain of the caller's turn goes on (`hops` + 1).
//! - The result is `{address}` at once; the turn runs on its own.
//! - Limits (SPEC §20, `Limits::spawns_per_chain`, `Limits::spawns_per_hour`): sessions
//!   spawned in one chain and per owner in the last hour; over them `limit_exceeded`. The
//!   prompt counts as a peer message ([`peers::admit`]). The count and the creation hold the
//!   owner's lock ([`AppState::owner_lock`]), so parallel calls cannot pass the check
//!   together.
//! - The new session runs in `ask` (T38): an agent never starts a session in `auto`, and
//!   `hqc_spawn` has no argument for the mode.
//! - Once per `request_id` (SPEC §5.2): the session's id and the message's `event_id` are
//!   derived from the call, so a repeat finds both and answers the same address.
//! - `isolation: "worktree"` (T47): the session works in an ephemeral worktree of the target
//!   workspace (T46's flow, `ephemeral: true`) on a new branch, `hqc/<session-slug>` unless
//!   `branch` is given. The worktree's id is derived from the call too. The call waits until
//!   the worker made it ([`Answer::Wait`]), then creates the session there and also returns
//!   the worktree's path and branch. A failed worktree fails the call with the worker's reason
//!   and leaves neither a session nor the record. A worktree being made counts as a spawn.
//!   `base` (SPEC 0.265): the new branch starts there instead of the origin's `HEAD`.
//! - `POST /api/projects/{id}/sessions` with `isolation: "worktree"` (SPEC 0.265,
//!   [`isolated_session`]): the owner's new session in such a worktree, by the same steps; the
//!   branch may be an existing one there.
//! - `hqc_room(project, workspace?, title?, mode?, agents, message)`: a room (SPEC §7.1.1) by
//!   [`api::new_room`], the path of `POST /api/projects/{id}/rooms`, and `message` as its first
//!   message from the caller, a `peer.message` that [`peers::into_room`] hands to the room's
//!   agents (the first in a conversation, each in a council). Its limits, its id derived from the
//!   call and its repeats are `hqc_spawn`'s; the room's agents count as spawns, one each, in the
//!   limits (their `spawn_chain_id` is the caller's chain), the room itself does not (its
//!   `spawned_by` is the caller); the agents' sessions are the room's and run in `ask`. An agent's model or provider
//!   is chosen as `hqc_spawn` chooses it.

use std::time::Duration;

use hqc_proto::{
    api::{
        CreateRoomRequest, CreateSessionRequest, CreateWorkspace, RoomAgent, RoomMode,
        WorkspaceState,
    },
    event::Chain,
    frame::{McpCall, McpError, PermissionMode},
    mcp::error_code,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    api,
    app::AppState,
    db::{self, SessionRow},
    delivery,
    error::ApiError,
    events,
    mcp::{args, internal, once_id, owner_project, Answer},
    peers,
};

/// How often a spawn looks at its worktree while the worker makes it (T47).
const WORKTREE_POLL: Duration = Duration::from_millis(200);
/// How long a spawn waits for its worktree: a little over the worker's default
/// `clone_timeout`, which also bounds `git worktree add`.
const WORKTREE_WAIT: Duration = Duration::from_secs(15 * 60);
/// What [`once_id`] derives a spawn's worktree id from.
const WORKTREE: &str = "hqc_spawn worktree";
/// The default branch of a spawn's worktree: `hqc/` and the session's slug `session-N`.
const BRANCH_PREFIX: &str = "hqc/session-";

fn err(code: &str, message: impl Into<String>) -> McpError {
    McpError::new(code, message)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnArgs {
    project: String,
    workspace: Option<String>,
    runtime: Option<String>,
    model: Option<String>,
    /// A provider of the owner by name (SPEC 0.144).
    provider: Option<String>,
    prompt: String,
    /// `worktree` (T47).
    isolation: Option<String>,
    /// The worktree's new branch (T47).
    branch: Option<String>,
    /// What the new branch starts from (SPEC 0.265).
    base: Option<String>,
}

/// `hqc_spawn(project, workspace?, runtime?, model?, provider?, prompt, isolation?, branch?)`:
/// `{address, worktree?: {path, branch}}`.
pub async fn spawn(app: &AppState, caller: &SessionRow, c: &McpCall) -> Result<Answer, McpError> {
    let a: SpawnArgs = args(c.args.clone())?;
    if a.prompt.trim().is_empty() {
        return Err(err(error_code::INVALID_ARGS, "prompt is empty"));
    }
    let isolated = match a.isolation.as_deref() {
        None => false,
        Some("worktree") => true,
        Some(i) => {
            return Err(err(
                error_code::INVALID_ARGS,
                format!("isolation {i} is not known; only worktree"),
            ))
        }
    };
    if (a.branch.is_some() || a.base.is_some()) && !isolated {
        return Err(err(
            error_code::INVALID_ARGS,
            "branch and base go with isolation: worktree",
        ));
    }
    let chain = peers::outgoing_chain(app, caller, c, false).await?;
    let id = once_id(hqc_proto::mcp::SPAWN, caller.id, &c.request_id);
    match db::session(&app.db, id).await.map_err(internal)? {
        // A repeat (SPEC §5.2): the session is there, and the message below is found if the
        // first call got that far.
        Some(s) if s.owner_id == caller.owner_id => {
            return started(app, caller, c, &a, &s, &chain)
                .await
                .map(Answer::Now)
        }
        Some(_) => return Err(internal(format!("session {id} is not the caller's"))),
        None => {}
    }
    if !isolated {
        let s = create(app, caller, id, &chain, &a, None).await?;
        return started(app, caller, c, &a, &s, &chain)
            .await
            .map(Answer::Now);
    }
    // Off the connection's loop from here on: the worker answers `fs.stat` and
    // `workspace.create` on the connection the call came by.
    let (app, caller, c) = (app.clone(), caller.clone(), c.clone());
    Ok(Answer::Wait(Box::pin(async move {
        let ws = worktree(&app, &caller, &c, &chain, &a).await?;
        let ws = ready(&app, ws).await.map_err(api_error)?;
        let s = create(&app, &caller, id, &chain, &a, Some(&ws)).await?;
        started(&app, &caller, &c, &a, &s, &chain).await.map(Some)
    })))
}

/// The prompt as the new session's first input (once per call), and the call's result.
async fn started(
    app: &AppState,
    caller: &SessionRow,
    c: &McpCall,
    a: &SpawnArgs,
    s: &SessionRow,
    chain: &Chain,
) -> Result<Value, McpError> {
    let from = peers::address(app, caller).await.map_err(internal)?;
    events::store_peer_message(
        app,
        s,
        peers::message_id(caller.id, &c.request_id),
        json!({"kind": "agent", "id": caller.id}),
        json!({"from_session_id": caller.id, "from": from, "text": a.prompt, "mode": "send"}),
        serde_json::to_value(chain).map_err(internal)?,
        Some("pending"),
    )
    .await
    .map_err(internal)?;
    delivery::kick(app, s.id, false).await;
    let address = peers::address(app, s).await.map_err(internal)?;
    tracing::info!(from = %caller.id, session_id = %s.id, %address, "session spawned");
    let mut result = json!({ "address": address });
    if a.isolation.is_some() {
        let ws = match s.workspace_id {
            Some(w) => db::workspace(&app.db, w).await.map_err(internal)?,
            None => None,
        }
        .ok_or_else(|| internal(format!("spawned session {} has no worktree", s.id)))?;
        result["worktree"] = json!({"path": ws.path, "branch": ws.branch});
    }
    Ok(result)
}

/// Checks the arguments and the limits, then creates the session `id` in `chain`; in the
/// worktree `ws` (T47) the limits were checked when it was made.
async fn create(
    app: &AppState,
    caller: &SessionRow,
    id: Uuid,
    chain: &Chain,
    a: &SpawnArgs,
    ws: Option<&db::WorkspaceRow>,
) -> Result<SessionRow, McpError> {
    let chain_id = chain.chain_id;
    let project = project(app, caller, a).await?;
    let workspace_id = match (ws, a.workspace.as_deref()) {
        (Some(ws), _) => Some(ws.id),
        (None, Some(w)) => Some(workspace(app, &project, w).await?),
        (None, None) => None,
    };
    // Held until the session is created: the next spawn of the owner counts it.
    let _lock = app.owner_lock("spawn", caller.owner_id).await;
    if let Some(s) = db::session(&app.db, id).await.map_err(internal)? {
        // The same call ran meanwhile (a repeat that came in parallel).
        return Ok(s);
    }
    if ws.is_none() {
        limits(app, caller.owner_id, chain_id, 1).await?;
        peers::admit(app, caller, chain).await?;
    }
    let (model, provider_id, inherited) =
        model_of(app, caller, a.model.as_deref(), a.provider.as_deref()).await?;
    let req = CreateSessionRequest {
        workspace_id,
        model,
        provider_id,
        // Never `auto`, whatever the caller runs in or the worker's default (T38): the owner
        // switches a spawned session to auto by hand.
        permission_mode: Some(PermissionMode::Ask),
        ..CreateSessionRequest::default()
    };
    let spawn = db::Spawn {
        id,
        by: caller.id,
        chain_id,
    };
    // The N the default branch was named after.
    let number = ws
        .filter(|_| a.branch.is_none())
        .and_then(|ws| branch_number(ws.branch.as_deref()?));
    match api::new_session(app, caller.owner_id, &project, &req, Some(spawn), number).await {
        Ok(s) => Ok(s),
        Err(ApiError::Unprocessable("provider_not_allowed", _)) if inherited => {
            let req = CreateSessionRequest {
                provider_id: None,
                ..req
            };
            api::new_session(app, caller.owner_id, &project, &req, Some(spawn), number)
                .await
                .map_err(api_error)
        }
        Err(e) => Err(api_error(e)),
    }
}

/// The model and provider of a new agent: the caller's model, or its provider, unless the call
/// names one (SPEC 0.144); and whether they are the caller's. Inherited, not asked for: a
/// workspace whose machine does not allow the provider runs the new agent on the runtime's own
/// endpoint instead of failing the call.
async fn model_of(
    app: &AppState,
    caller: &SessionRow,
    model: Option<&str>,
    provider: Option<&str>,
) -> Result<(Option<String>, Option<Uuid>, bool), McpError> {
    Ok(match (model, provider) {
        (Some(_), Some(_)) => {
            return Err(err(
                error_code::INVALID_ARGS,
                "give model or provider, not both",
            ))
        }
        (Some(m), None) => (Some(m.to_string()), None, false),
        (None, Some(name)) => {
            let p = db::provider_by_name(&app.db, caller.owner_id, name)
                .await
                .map_err(internal)?
                .ok_or_else(|| {
                    err(
                        error_code::INVALID_ARGS,
                        format!("no model provider named {name:?}"),
                    )
                })?;
            (None, Some(p.id), false)
        }
        (None, None) => (
            caller.model.clone(),
            caller.provider_id,
            caller.provider_id.is_some(),
        ),
    })
}

/// The owner's project of the call (slug or id), with `runtime` checked.
async fn project(
    app: &AppState,
    caller: &SessionRow,
    a: &SpawnArgs,
) -> Result<db::ProjectRow, McpError> {
    if let Some(r) = a.runtime.as_deref().filter(|r| *r != "claude_code") {
        return Err(err(
            error_code::INVALID_ARGS,
            format!("runtime {r} is not available; only claude_code"),
        ));
    }
    project_row(app, caller, &a.project).await
}

/// The owner's project `name` (slug or id).
async fn project_row(
    app: &AppState,
    caller: &SessionRow,
    name: &str,
) -> Result<db::ProjectRow, McpError> {
    let project = owner_project(app, caller.owner_id, name).await?;
    db::project(&app.db, project)
        .await
        .map_err(internal)?
        .ok_or_else(|| err(error_code::NOT_FOUND, format!("no project {name}")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoomArgs {
    project: String,
    workspace: Option<String>,
    title: Option<String>,
    mode: Option<String>,
    agents: Vec<AgentArgs>,
    message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentArgs {
    name: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    /// The agent's system prompt.
    prompt: Option<String>,
}

/// `hqc_room(project, workspace?, title?, mode?, agents, message)`: `{address}` of the room.
pub async fn room(app: &AppState, caller: &SessionRow, c: &McpCall) -> Result<Answer, McpError> {
    let a: RoomArgs = args(c.args.clone())?;
    if a.message.trim().is_empty() {
        return Err(err(error_code::INVALID_ARGS, "message is empty"));
    }
    let mode = match a.mode.as_deref() {
        None => RoomMode::Conversation,
        Some(m) => RoomMode::parse(m).ok_or_else(|| {
            err(
                error_code::INVALID_ARGS,
                format!("mode {m} is not known; conversation or council"),
            )
        })?,
    };
    let chain = peers::outgoing_chain(app, caller, c, false).await?;
    let id = once_id(hqc_proto::mcp::ROOM, caller.id, &c.request_id);
    let room = match db::session(&app.db, id).await.map_err(internal)? {
        // A repeat (SPEC §5.2): the room is there, and the message below is found if the first
        // call got that far.
        Some(s) if s.owner_id == caller.owner_id => s,
        Some(_) => return Err(internal(format!("session {id} is not the caller's"))),
        None => create_room(app, caller, id, &chain, &a, mode).await?,
    };
    posted(app, caller, c, &a.message, &room, &chain)
        .await
        .map(Answer::Now)
}

/// Checks the arguments and the limits, then creates the room `id` in `chain` and its agents.
async fn create_room(
    app: &AppState,
    caller: &SessionRow,
    id: Uuid,
    chain: &Chain,
    a: &RoomArgs,
    mode: RoomMode,
) -> Result<SessionRow, McpError> {
    let project = project_row(app, caller, &a.project).await?;
    let workspace_id = match a.workspace.as_deref() {
        Some(w) => Some(workspace(app, &project, w).await?),
        None => None,
    };
    // Held until the room is created: the next spawn of the owner counts it.
    let _lock = app.owner_lock("spawn", caller.owner_id).await;
    if let Some(s) = db::session(&app.db, id).await.map_err(internal)? {
        // The same call ran meanwhile (a repeat that came in parallel).
        return Ok(s);
    }
    limits(app, caller.owner_id, chain.chain_id, a.agents.len() as i64).await?;
    peers::admit(app, caller, chain).await?;
    let mut agents = Vec::new();
    let mut inherited = false;
    for x in &a.agents {
        let (model, provider_id, inherits) =
            model_of(app, caller, x.model.as_deref(), x.provider.as_deref()).await?;
        inherited |= inherits;
        agents.push(RoomAgent {
            name: x.name.clone(),
            model,
            provider_id,
            system_prompt: x.prompt.clone(),
        });
    }
    let req = CreateRoomRequest {
        title: a.title.clone(),
        mode,
        workspace_id,
        cwd: None,
        // Never `auto`, as for a spawned session (T38).
        permission_mode: Some(PermissionMode::Ask),
        agents,
    };
    let spawn = db::Spawn {
        id,
        by: caller.id,
        chain_id: chain.chain_id,
    };
    match api::new_room(app, caller.owner_id, &project, &req, Some(spawn)).await {
        Ok(s) => Ok(s),
        // The caller's provider is not allowed on the workspace's machine: its agents run on the
        // runtime's own endpoint, as a spawned session does.
        Err(ApiError::Unprocessable("provider_not_allowed", _)) if inherited => {
            let mut req = req;
            for (agent, x) in req.agents.iter_mut().zip(&a.agents) {
                if x.model.is_none() && x.provider.is_none() {
                    agent.provider_id = None;
                }
            }
            api::new_room(app, caller.owner_id, &project, &req, Some(spawn))
                .await
                .map_err(api_error)
        }
        Err(e) => Err(api_error(e)),
    }
}

/// `message` as the room's first message from the caller ([`peers::into_room`], once per call),
/// and the call's result.
async fn posted(
    app: &AppState,
    caller: &SessionRow,
    c: &McpCall,
    message: &str,
    room: &SessionRow,
    chain: &Chain,
) -> Result<Value, McpError> {
    peers::into_room(app, caller, room, &c.request_id, message, &[], chain).await?;
    let address = peers::address(app, room).await.map_err(internal)?;
    tracing::info!(from = %caller.id, session_id = %room.id, %address, "room created");
    Ok(json!({ "address": address }))
}

/// Starts the call's ephemeral worktree (T47), or finds it on a repeat; its id. The origin is
/// the workspace the spawn would use without isolation, by T46's rules (`invalid_args`: not
/// ready, itself a worktree), on a worker that is online. The branch is new: `branch`, or
/// `hqc/session-N` with the N the session will get.
async fn worktree(
    app: &AppState,
    caller: &SessionRow,
    c: &McpCall,
    chain: &Chain,
    a: &SpawnArgs,
) -> Result<Uuid, McpError> {
    let id = once_id(WORKTREE, caller.id, &c.request_id);
    if db::workspace(&app.db, id)
        .await
        .map_err(internal)?
        .is_some()
    {
        return Ok(id);
    }
    let project = project(app, caller, a).await?;
    let origin = match a.workspace.as_deref() {
        Some(w) => workspace(app, &project, w).await?,
        None => project.default_workspace_id.ok_or_else(|| {
            err(
                error_code::INVALID_ARGS,
                "the project has no default workspace; give `workspace`",
            )
        })?,
    };
    let origin = db::workspace(&app.db, origin)
        .await
        .map_err(internal)?
        .ok_or_else(|| err(error_code::NOT_FOUND, "the workspace is gone"))?;
    let worker = match db::worker(&app.db, origin.worker_id)
        .await
        .map_err(internal)?
    {
        Some(w) if w.owner_id == caller.owner_id && w.revoked_at.is_none() => w,
        _ => return Err(err(error_code::INVALID_ARGS, "unknown worker")),
    };
    // Held until the worktree is stored: the next spawn of the owner counts it.
    let _lock = app.owner_lock("spawn", caller.owner_id).await;
    if db::workspace(&app.db, id)
        .await
        .map_err(internal)?
        .is_some()
    {
        return Ok(id);
    }
    limits(app, caller.owner_id, chain.chain_id, 1).await?;
    peers::admit(app, caller, chain).await?;
    let branch = match a.branch.as_deref().map(str::trim) {
        Some(b) => b.to_string(),
        None => default_branch(app, project.id).await.map_err(internal)?,
    };
    make_worktree(
        app,
        &project,
        &worker,
        caller.owner_id,
        origin.id,
        id,
        branch,
        true,
        a.base.clone(),
    )
    .await
    .map_err(api_error)?;
    Ok(id)
}

/// The ephemeral worktree `id` of `origin` on `branch` (T46's checks), `creating` until the
/// worker made it: a new branch from `base` (the origin's `HEAD` when absent), or the existing
/// `branch`.
#[allow(clippy::too_many_arguments)]
async fn make_worktree(
    app: &AppState,
    project: &db::ProjectRow,
    worker: &db::WorkerRow,
    owner: Uuid,
    origin: Uuid,
    id: Uuid,
    branch: String,
    new_branch: bool,
    base: Option<String>,
) -> Result<(), ApiError> {
    let b = CreateWorkspace {
        kind: Some("worktree".into()),
        project_id: project.id,
        worker_id: worker.id,
        path: String::new(),
        origin_workspace_id: Some(origin),
        new_branch,
        label: None,
        create: false,
        url: None,
        branch: Some(branch),
        base,
    };
    crate::workspaces::create_worktree(app, project, worker, owner, b, "", false, Some(id))
        .await
        .map(drop)
}

/// `POST /api/projects/{id}/sessions` with `isolation: "worktree"` (SPEC 0.265): the session
/// starts in a new ephemeral worktree of the workspace `b` picks, as `hqc_spawn` makes one
/// (T47): checked first as the session would be in that workspace, then the worktree on
/// `branch` (`hqc/session-N` with the session's N when absent), new from `base`, or the
/// existing one with `existing_branch`. The request waits until the worker made it, then
/// creates the session there; a failed worktree leaves no record and is `409` with the worker's
/// reason (`branch_exists` for a new branch that exists). The wait runs on its own task: an
/// owner who goes away meanwhile still gets the session in the list.
pub async fn isolated_session(
    app: &AppState,
    owner: Uuid,
    project: db::ProjectRow,
    b: CreateSessionRequest,
) -> Result<SessionRow, ApiError> {
    let branch = b
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let base = b
        .base
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if b.existing_branch && (branch.is_none() || base.is_some()) {
        return Err(ApiError::BadRequest(
            "existing_branch needs branch and no base".into(),
        ));
    }
    let (origin, worker, _) = api::session_place(app, owner, &project, &b).await?;
    let id = db::new_id();
    let number = {
        // Held until the worktree is stored: a parallel default branch counts it.
        let _lock = app.owner_lock("spawn", owner).await;
        let (branch, number) = match branch {
            Some(b) => (b, None),
            None => {
                let b = default_branch(app, project.id).await?;
                let n = branch_number(&b);
                (b, n)
            }
        };
        make_worktree(
            app,
            &project,
            &worker,
            owner,
            origin.id,
            id,
            branch,
            !b.existing_branch,
            base,
        )
        .await?;
        number
    };
    let app = app.clone();
    tokio::spawn(async move {
        let ws = ready(&app, id).await?;
        let req = CreateSessionRequest {
            workspace_id: Some(ws.id),
            worker_id: None,
            isolation: None,
            branch: None,
            base: None,
            existing_branch: false,
            ..b
        };
        api::new_session(&app, owner, &project, &req, None, number).await
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("the new session's task: {e}")))?
}

/// The N of a default branch `hqc/session-N`.
fn branch_number(branch: &str) -> Option<i64> {
    branch.strip_prefix(BRANCH_PREFIX)?.parse().ok()
}

/// `hqc/session-N` with the N of the project's next default title, past the slugs and the
/// branches taken (a spawn whose worktree is being made has no session yet).
async fn default_branch(app: &AppState, project: Uuid) -> sqlx::Result<String> {
    let mut n = db::next_session_number(&app.db, project).await?;
    loop {
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM sessions WHERE project_id = $1 AND slug = $2)
                 OR EXISTS (SELECT 1 FROM workspaces WHERE project_id = $1 AND branch = $3)",
        )
        .bind(project)
        .bind(format!("session-{n}"))
        .bind(format!("{BRANCH_PREFIX}{n}"))
        .fetch_one(&app.db)
        .await?;
        if !taken {
            return Ok(format!("{BRANCH_PREFIX}{n}"));
        }
        n += 1;
    }
}

/// The worktree `id` once the worker made it. A failed one is removed (only its record was
/// left) and fails the call with the worker's reason: `409 branch_exists` for a new branch that
/// exists, else `409 worktree_failed`.
async fn ready(app: &AppState, id: Uuid) -> Result<db::WorkspaceRow, ApiError> {
    let deadline = tokio::time::Instant::now() + WORKTREE_WAIT;
    loop {
        let ws = db::workspace(&app.db, id).await?.ok_or_else(|| {
            ApiError::Conflict(
                "worktree_removed",
                "the worktree was removed before the session started".into(),
            )
        })?;
        match ws.state() {
            WorkspaceState::Ready => return Ok(ws),
            WorkspaceState::Failed => {
                db::delete_workspace(&app.db, &ws).await?;
                let why = ws.error.as_deref().unwrap_or("no reason");
                tracing::info!(workspace_id = %id, error = %why, "spawn worktree failed, record removed");
                let code = if hqc_proto::frame::is_branch_exists(why) {
                    "branch_exists"
                } else {
                    "worktree_failed"
                };
                return Err(ApiError::Conflict(
                    code,
                    format!("the worktree could not be made: {why}"),
                ));
            }
            WorkspaceState::Creating if tokio::time::Instant::now() >= deadline => {
                return Err(ApiError::Conflict(
                    "worktree_timeout",
                    format!(
                        "the worker has not made the worktree {} in {} minutes; no session was started",
                        ws.path,
                        WORKTREE_WAIT.as_secs() / 60
                    ),
                ))
            }
            WorkspaceState::Creating => tokio::time::sleep(WORKTREE_POLL).await,
        }
    }
}

/// A workspace of the project by id, label or path (SPEC §7).
async fn workspace(app: &AppState, project: &db::ProjectRow, w: &str) -> Result<Uuid, McpError> {
    let id = w.parse::<Uuid>().ok();
    db::workspaces_of_project(&app.db, project.id)
        .await
        .map_err(internal)?
        .into_iter()
        .find(|x| Some(x.id) == id || x.label == w || x.path == w)
        .map(|x| x.id)
        .ok_or_else(|| {
            err(
                error_code::NOT_FOUND,
                format!("no workspace {w} in project {}", project.slug),
            )
        })
}

/// The spawn limits of SPEC §20 for a call that creates `n` sessions. A worktree a spawn is
/// waiting for counts like its session (T47), in every chain of the owner for the seconds it
/// takes.
async fn limits(app: &AppState, owner: Uuid, chain_id: Uuid, n: i64) -> Result<(), McpError> {
    let (per_chain, per_hour) = (
        app.config.limits.spawns_per_chain,
        app.config.limits.spawns_per_hour,
    );
    let (in_chain, last_hour, making): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE spawn_chain_id = $2),
                count(*) FILTER (WHERE created_at > now() - interval '1 hour'),
                (SELECT count(*) FROM workspaces
                 WHERE owner_id = $1 AND ephemeral AND state = 'creating')
         FROM sessions WHERE owner_id = $1 AND spawn_chain_id IS NOT NULL",
    )
    .bind(owner)
    .bind(chain_id)
    .fetch_one(&app.db)
    .await
    .map_err(internal)?;
    if in_chain + making + n > per_chain {
        return Err(err(
            error_code::LIMIT_EXCEEDED,
            format!("one chain spawns at most {per_chain} sessions; a person has to continue it"),
        ));
    }
    if last_hour + making + n > per_hour {
        return Err(err(
            error_code::LIMIT_EXCEEDED,
            format!("at most {per_hour} sessions are spawned per hour; try again later"),
        ));
    }
    Ok(())
}

/// An error of the session creation path as the tool's error.
fn api_error(e: ApiError) -> McpError {
    match e {
        ApiError::NotFound => err(error_code::NOT_FOUND, "not found"),
        ApiError::BadRequest(m) | ApiError::Invalid(_, m) | ApiError::Unprocessable(_, m) => {
            err(error_code::INVALID_ARGS, m)
        }
        ApiError::Conflict("no_workspace", _) => err(
            error_code::INVALID_ARGS,
            "the project has no default workspace; give `workspace`",
        ),
        ApiError::Conflict(_, m) | ApiError::ConflictWith(_, m, _) => {
            err(error_code::UNAVAILABLE, m)
        }
        other => internal(other),
    }
}
