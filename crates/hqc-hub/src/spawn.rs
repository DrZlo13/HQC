//! `hqc_spawn` (SPEC §7, §13.4): an agent starts a session of its owner and gives it its
//! first input.
//!
//! - The session is created by the path of `POST /api/projects/{id}/sessions`
//!   ([`api::new_session`]): a project of the caller's owner (slug or id), a workspace of
//!   that project (id, label or path; the project's `default_workspace_id` when absent) on a
//!   worker of the owner; runtime `claude_code` only until iteration 4; `model` absent is the
//!   calling session's model.
//! - `prompt` is a `peer.message` (`mode: send`) from the calling session into the input
//!   queue of the new one, so it starts its first turn and the new agent knows whom to
//!   answer with `hqc_send`. The chain of the caller's turn goes on (`hops` + 1).
//! - The result is `{address}` at once; the turn runs on its own.
//! - Limits (SPEC §20): [`MAX_PER_CHAIN`] sessions spawned in one chain, [`MAX_PER_HOUR`]
//!   per owner in the last hour; over them `limit_exceeded`.
//! - Once per `request_id` (SPEC §5.2): the session's id and the message's `event_id` are
//!   derived from the call, so a repeat finds both and answers the same address.

use hqc_proto::{
    api::CreateSessionRequest,
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
    error::ApiError,
    events,
    mcp::{args, internal, once_id, owner_project, Answer},
    peers,
};

/// Sessions spawned in one chain (SPEC §7.1, §20) ...
pub const MAX_PER_CHAIN: i64 = 5;
/// ... and per owner in the last hour.
pub const MAX_PER_HOUR: i64 = 20;

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
    prompt: String,
}

/// `hqc_spawn(project, workspace?, runtime?, model?, prompt)`: `{address}`.
pub async fn spawn(app: &AppState, caller: &SessionRow, c: &McpCall) -> Result<Answer, McpError> {
    let a: SpawnArgs = args(c.args.clone())?;
    if a.prompt.trim().is_empty() {
        return Err(err(error_code::INVALID_ARGS, "prompt is empty"));
    }
    let turn = peers::turn_chain(app, caller.id).await.map_err(internal)?;
    let chain = peers::next_chain(turn, false)?;
    let id = once_id(hqc_proto::mcp::SPAWN, caller.id, &c.request_id);
    let s = match db::session(&app.db, id).await.map_err(internal)? {
        // A repeat (SPEC §5.2): the session is there, and the message below is found if the
        // first call got that far.
        Some(s) if s.owner_id == caller.owner_id => s,
        Some(_) => return Err(internal(format!("session {id} is not the caller's"))),
        None => create(app, caller, id, chain.chain_id, &a).await?,
    };
    let from = peers::address(app, caller).await.map_err(internal)?;
    events::store_peer_message(
        app,
        &s,
        peers::message_id(caller.id, &c.request_id),
        json!({"kind": "agent", "id": caller.id}),
        json!({"from_session_id": caller.id, "from": from, "text": a.prompt, "mode": "send"}),
        serde_json::to_value(&chain).map_err(internal)?,
        Some("pending"),
    )
    .await
    .map_err(internal)?;
    delivery::kick(app, s.id, false).await;
    let address = peers::address(app, &s).await.map_err(internal)?;
    tracing::info!(from = %caller.id, session_id = %s.id, %address, "session spawned");
    Ok(Answer::Now(json!({ "address": address })))
}

/// Checks the arguments and the limits, then creates the session `id` in chain `chain_id`.
async fn create(
    app: &AppState,
    caller: &SessionRow,
    id: Uuid,
    chain_id: Uuid,
    a: &SpawnArgs,
) -> Result<SessionRow, McpError> {
    if let Some(r) = a.runtime.as_deref().filter(|r| *r != "claude_code") {
        return Err(err(
            error_code::INVALID_ARGS,
            format!("runtime {r} is not available; only claude_code"),
        ));
    }
    let project = owner_project(app, caller.owner_id, &a.project).await?;
    let project = db::project(&app.db, project)
        .await
        .map_err(internal)?
        .ok_or_else(|| err(error_code::NOT_FOUND, format!("no project {}", a.project)))?;
    let workspace_id = match a.workspace.as_deref() {
        Some(w) => Some(workspace(app, &project, w).await?),
        None => None,
    };
    limits(app, caller.owner_id, chain_id).await?;
    let req = CreateSessionRequest {
        workspace_id,
        model: a.model.clone().or_else(|| caller.model.clone()),
        ..CreateSessionRequest::default()
    };
    let spawn = db::Spawn {
        id,
        by: caller.id,
        chain_id,
    };
    match api::new_session(app, caller.owner_id, &project, &req, Some(spawn)).await {
        Ok(s) => Ok(s),
        Err(e) => {
            // The same call running twice at once: the other one created it.
            if let Some(s) = db::session(&app.db, id).await.map_err(internal)? {
                return Ok(s);
            }
            Err(api_error(e))
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

/// The spawn limits of SPEC §20.
async fn limits(app: &AppState, owner: Uuid, chain_id: Uuid) -> Result<(), McpError> {
    let (in_chain, last_hour): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE spawn_chain_id = $2),
                count(*) FILTER (WHERE created_at > now() - interval '1 hour')
         FROM sessions WHERE owner_id = $1 AND spawn_chain_id IS NOT NULL",
    )
    .bind(owner)
    .bind(chain_id)
    .fetch_one(&app.db)
    .await
    .map_err(internal)?;
    if in_chain >= MAX_PER_CHAIN {
        return Err(err(
            error_code::LIMIT_EXCEEDED,
            format!(
                "one chain spawns at most {MAX_PER_CHAIN} sessions; a person has to continue it"
            ),
        ));
    }
    if last_hour >= MAX_PER_HOUR {
        return Err(err(
            error_code::LIMIT_EXCEEDED,
            format!("at most {MAX_PER_HOUR} sessions are spawned per hour; try again later"),
        ));
    }
    Ok(())
}

/// An error of the session creation path as the tool's error.
fn api_error(e: ApiError) -> McpError {
    match e {
        ApiError::NotFound => err(error_code::NOT_FOUND, "not found"),
        ApiError::BadRequest(m) | ApiError::Invalid(_, m) => err(error_code::INVALID_ARGS, m),
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
