//! The hqc MCP tools behind `mcp.call` (SPEC §5.2, §7).
//!
//! - A call names its session by the token of a `session.start` (the latest or the one
//!   before, [`db::session_by_token_hash`]) and must come from the worker that runs that
//!   session; otherwise `unauthorized`, the same answer for an unknown token and for a
//!   session of another worker.
//! - The arguments are at most [`MAX_ARGS`] bytes and must fit the tool's argument type
//!   (unknown fields refused), otherwise `invalid_args`.
//! - Tools act with the rights of the calling session's owner (SPEC §7): until grants
//!   (iteration 6) only on the owner's own projects and sessions; anything else is
//!   `not_found`, as if it did not exist.
//! - Every error has the one shape `{code, message}` ([`McpError`]).
//!
//! [`REGISTRY`] is the hub's half of [`hqc_proto::mcp::TOOLS`]: a tool lands in both at once.

use std::collections::HashMap;

use futures_util::future::BoxFuture;
use hqc_proto::{
    frame::{McpCall, McpError, McpResult},
    mcp::{error_code, MAX_ARGS},
};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::hash_secret,
    db::{self, SessionRow},
};

type ToolResult = Result<Value, McpError>;

/// A tool's implementation: the calling session and the arguments.
pub type Handler = for<'a> fn(&'a AppState, &'a SessionRow, Value) -> BoxFuture<'a, ToolResult>;

pub struct HubTool {
    pub name: &'static str,
    pub run: Handler,
}

pub const REGISTRY: &[HubTool] = &[HubTool {
    name: hqc_proto::mcp::LIST_SESSIONS,
    run: |app, caller, args| Box::pin(list_sessions(app, caller, args)),
}];

/// The shape `new_secret("hqcs_")` gives session tokens; anything else never reaches the
/// database.
fn is_session_token(t: &str) -> bool {
    t.strip_prefix("hqcs_").is_some_and(|b| {
        b.len() == 43
            && b.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    })
}

fn err(code: &str, message: impl Into<String>) -> McpError {
    McpError::new(code, message)
}

fn internal(e: impl std::fmt::Display) -> McpError {
    tracing::error!(error = %e, "mcp tool failed");
    err(error_code::INTERNAL, "internal error")
}

/// `mcp.call` from `worker_id`; the answer is always an `mcp.result`.
pub async fn call(app: &AppState, worker_id: Uuid, c: &McpCall) -> McpResult {
    let outcome = run(app, worker_id, c).await;
    match &outcome {
        Ok(_) => {
            tracing::info!(%worker_id, request_id = %c.request_id, tool = %c.tool, "mcp call")
        }
        Err(e) => {
            tracing::info!(%worker_id, request_id = %c.request_id, tool = %c.tool, code = %e.code, "mcp call refused: {}", e.message)
        }
    }
    match outcome {
        Ok(v) => McpResult::ok(&c.request_id, v),
        Err(e) => McpResult {
            request_id: c.request_id.clone(),
            result: None,
            error: Some(e),
        },
    }
}

async fn run(app: &AppState, worker_id: Uuid, c: &McpCall) -> ToolResult {
    let size = serde_json::to_vec(&c.args).map_err(internal)?.len();
    if size > MAX_ARGS {
        return Err(err(
            error_code::INVALID_ARGS,
            format!("arguments are {size} bytes, more than {MAX_ARGS}"),
        ));
    }
    let unauthorized = || {
        err(
            error_code::UNAUTHORIZED,
            "the session token is not one of a session this worker runs",
        )
    };
    if !is_session_token(&c.session_token) {
        return Err(unauthorized());
    }
    let caller = db::session_by_token_hash(&app.db, worker_id, &hash_secret(&c.session_token))
        .await
        .map_err(internal)?
        .ok_or_else(unauthorized)?;
    let tool = REGISTRY.iter().find(|t| t.name == c.tool).ok_or_else(|| {
        err(
            error_code::UNKNOWN_TOOL,
            format!("the hub has no tool {}", c.tool),
        )
    })?;
    (tool.run)(app, &caller, c.args.clone()).await
}

/// The arguments as `T`; absent arguments read as `{}`.
fn args<T: DeserializeOwned>(v: Value) -> Result<T, McpError> {
    let v = if v.is_null() { json!({}) } else { v };
    serde_json::from_value(v).map_err(|e| err(error_code::INVALID_ARGS, e.to_string()))
}

/// The owner's project by slug or id (SPEC §7); another owner's is `not_found`.
async fn owner_project(app: &AppState, owner: Uuid, p: &str) -> Result<Uuid, McpError> {
    let projects = db::projects_of(&app.db, owner).await.map_err(internal)?;
    let id = p.parse::<Uuid>().ok();
    projects
        .into_iter()
        .find(|x| Some(x.id) == id || x.slug == p)
        .map(|x| x.id)
        .ok_or_else(|| err(error_code::NOT_FOUND, format!("no project {p}")))
}

/// Most sessions one `hqc_list_sessions` returns, the most recently active first.
pub const MAX_LISTED: usize = 200;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    project: Option<String>,
}

/// `hqc_list_sessions(project?)` (SPEC §7): the owner's sessions, archived ones and the
/// workers' service sessions left out, as `{address, title, status, current?}`.
async fn list_sessions(app: &AppState, caller: &SessionRow, v: Value) -> ToolResult {
    let a: ListArgs = args(v)?;
    let owner = caller.owner_id;
    let rows = match a.project {
        Some(p) => db::sessions_of_project(&app.db, owner_project(app, owner, &p).await?).await,
        None => db::sessions_of_owner(&app.db, owner).await,
    }
    .map_err(internal)?;
    let rows: Vec<SessionRow> = rows
        .into_iter()
        .filter(|s| s.owner_id == owner && !s.system && !s.archived)
        .collect();
    let truncated = rows.len() > MAX_LISTED;
    let rows: Vec<SessionRow> = rows.into_iter().take(MAX_LISTED).collect();
    let slugs: HashMap<Uuid, String> = db::projects_of(&app.db, owner)
        .await
        .map_err(internal)?
        .into_iter()
        .map(|p| (p.id, p.slug))
        .collect();
    let views = crate::state::views(app, owner, rows)
        .await
        .map_err(internal)?;
    let sessions: Vec<Value> = views
        .into_iter()
        .map(|s| {
            let mut v = json!({
                "address": format!("{}/{}", slugs.get(&s.project_id).map_or("?", String::as_str), s.slug),
                "title": s.title,
                "status": s.state,
            });
            if s.id == caller.id {
                v["current"] = json!(true);
            }
            v
        })
        .collect();
    Ok(json!({ "sessions": sessions, "truncated": truncated }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_matches_the_tool_list() {
        let hub: Vec<&str> = REGISTRY.iter().map(|t| t.name).collect();
        let listed: Vec<&str> = hqc_proto::mcp::TOOLS.iter().map(|t| t.name).collect();
        assert_eq!(hub, listed);
    }

    #[test]
    fn token_shape() {
        assert!(is_session_token(&crate::auth::new_secret("hqcs_")));
        assert!(!is_session_token(&crate::auth::new_secret("hqcw_")));
        assert!(!is_session_token("hqcs_short"));
        assert!(!is_session_token(""));
    }

    #[test]
    fn args_are_strict() {
        assert!(args::<ListArgs>(Value::Null).unwrap().project.is_none());
        assert_eq!(
            args::<ListArgs>(json!({"project": "hqc"}))
                .unwrap()
                .project
                .as_deref(),
            Some("hqc")
        );
        for bad in [json!({"projct": "x"}), json!({"project": 3}), json!([1])] {
            assert_eq!(
                args::<ListArgs>(bad).err().unwrap().code,
                error_code::INVALID_ARGS
            );
        }
    }
}
