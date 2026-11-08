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
//! - A call whose answer was lost is sent again with the same `request_id` (SPEC §5.2). A
//!   tool with side effects acts once per `request_id`: what it writes is keyed by it
//!   ([`once_id`]) and a repeat answers from what the first call wrote.
//! - A deferred tool (`hqc_ask`) answers [`Answer::Later`]: the hub acks the call frame and
//!   sends `mcp.result` on its own later.
//! - A tool that waits for the worker (`hqc_spawn` with a worktree, T47; `hqc_move`) answers
//!   [`Answer::Wait`]: the connection goes on reading frames, and the result goes under the
//!   call's frame id once the wait is over, or the call is deferred then (an `hqc_move` that
//!   asks the owner): `ack {}` and `mcp.result` later; if the connection closes first, nothing
//!   is sent and the worker sends the call again after its next `hello`.
//!
//! [`REGISTRY`] is the hub's half of [`hqc_proto::mcp::TOOLS`]: a tool lands in both at once.

use std::collections::HashMap;

use futures_util::future::BoxFuture;
use hqc_proto::{
    frame::{McpCall, McpError, McpResult},
    mcp::{error_code, AttachCall, MAX_ARGS},
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

/// What a tool answers.
pub enum Answer {
    /// `mcp.result` under the call's frame id.
    Now(Value),
    /// Deferred: `ack {}` now, `mcp.result` as a frame of its own later (SPEC §5.2).
    Later,
    /// `mcp.result` under the call's frame id once this is done, off the connection's loop;
    /// `Ok(None)`: deferred after all, as [`Answer::Later`].
    Wait(BoxFuture<'static, Result<Option<Value>, McpError>>),
}

/// What the connection does with an `mcp.call` ([`call`]).
pub enum Called {
    /// Send this `mcp.result` under the call's frame id.
    Now(McpResult),
    /// Ack the frame; the result comes later.
    Later,
    /// Send its `mcp.result` under the call's frame id when it is done; `None`: ack the frame
    /// then, the result comes later.
    Wait(BoxFuture<'static, Option<McpResult>>),
}

/// A tool's implementation: the calling session and the call (its `request_id`, `args`).
pub type Handler = for<'a> fn(
    &'a AppState,
    &'a SessionRow,
    &'a McpCall,
) -> BoxFuture<'a, Result<Answer, McpError>>;

pub struct HubTool {
    pub name: &'static str,
    pub run: Handler,
}

/// A tool that answers at once.
async fn now(r: impl std::future::Future<Output = ToolResult>) -> Result<Answer, McpError> {
    r.await.map(Answer::Now)
}

pub const REGISTRY: &[HubTool] = &[
    HubTool {
        name: hqc_proto::mcp::LIST_SESSIONS,
        run: |app, caller, c| Box::pin(now(list_sessions(app, caller, c.args.clone()))),
    },
    HubTool {
        name: hqc_proto::mcp::ATTACH,
        run: |app, caller, c| Box::pin(now(attach(app, caller, &c.request_id, c.args.clone()))),
    },
    HubTool {
        name: hqc_proto::mcp::SEND,
        run: |app, caller, c| Box::pin(crate::peers::send(app, caller, c)),
    },
    HubTool {
        name: hqc_proto::mcp::ASK,
        run: |app, caller, c| Box::pin(crate::peers::ask(app, caller, c)),
    },
    HubTool {
        name: hqc_proto::mcp::SEARCH_HISTORY,
        run: |app, caller, c| Box::pin(now(search_history(app, caller, c.args.clone()))),
    },
    HubTool {
        name: hqc_proto::mcp::SPAWN,
        run: |app, caller, c| Box::pin(crate::spawn::spawn(app, caller, c)),
    },
    HubTool {
        name: hqc_proto::mcp::SCHEDULE,
        run: |app, caller, c| Box::pin(crate::schedules::tool(app, caller, c)),
    },
    HubTool {
        name: hqc_proto::mcp::MOVE,
        run: |app, caller, c| Box::pin(crate::moves::tool(app, caller, c)),
    },
];

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

pub(crate) fn internal(e: impl std::fmt::Display) -> McpError {
    tracing::error!(error = %e, "mcp tool failed");
    err(error_code::INTERNAL, "internal error")
}

/// `mcp.call` from `worker_id`: what to answer it with.
pub async fn call(app: &AppState, worker_id: Uuid, c: &McpCall) -> Called {
    match run(app, worker_id, c).await {
        Ok(Answer::Now(v)) => Called::Now(finished(worker_id, c, Ok(v))),
        Ok(Answer::Later) => {
            tracing::info!(%worker_id, request_id = %c.request_id, tool = %c.tool, "mcp call");
            Called::Later
        }
        Ok(Answer::Wait(f)) => {
            let c = c.clone();
            Called::Wait(Box::pin(async move {
                match f.await {
                    Ok(None) => {
                        tracing::info!(%worker_id, request_id = %c.request_id, tool = %c.tool, "mcp call");
                        None
                    }
                    Ok(Some(v)) => Some(finished(worker_id, &c, Ok(v))),
                    Err(e) => Some(finished(worker_id, &c, Err(e))),
                }
            }))
        }
        Err(e) => Called::Now(finished(worker_id, c, Err(e))),
    }
}

/// Logs the call's outcome; its `mcp.result`.
fn finished(worker_id: Uuid, c: &McpCall, outcome: ToolResult) -> McpResult {
    match outcome {
        Ok(v) => {
            tracing::info!(%worker_id, request_id = %c.request_id, tool = %c.tool, "mcp call");
            McpResult::ok(&c.request_id, v)
        }
        Err(e) => {
            tracing::info!(%worker_id, request_id = %c.request_id, tool = %c.tool, code = %e.code, "mcp call refused: {}", e.message);
            McpResult {
                request_id: c.request_id.clone(),
                result: None,
                error: Some(e),
            }
        }
    }
}

/// Longest `request_id` of an `mcp.call` (the worker sends a uuid).
pub const MAX_REQUEST_ID: usize = 128;

/// A `request_id` as it may reach another agent's input (the header of an ask, SPEC §7.1):
/// 1–[`MAX_REQUEST_ID`] characters of `A-Z a-z 0-9 . _ : -`.
pub fn is_request_id(r: &str) -> bool {
    (1..=MAX_REQUEST_ID).contains(&r.len())
        && r.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b':' | b'-'))
}

const CALLS_PER_WORKER: &str = "mcp_calls_worker";
const CALLS_PER_SESSION: &str = "mcp_calls_session";

/// `limit_exceeded` for a rate cap of SPEC §20.
pub(crate) fn too_often(what: &str) -> McpError {
    err(
        error_code::LIMIT_EXCEEDED,
        format!("too many {what}; wait a minute"),
    )
}

/// Spends one of `per_minute` in `bucket` for `key`.
pub(crate) fn per_minute(
    app: &AppState,
    bucket: &'static str,
    key: Uuid,
    per_minute: usize,
) -> bool {
    app.budget.spend(
        bucket,
        &key.to_string(),
        1,
        per_minute as u64,
        chrono::TimeDelta::minutes(1),
        chrono::Utc::now(),
    )
}

async fn run(app: &AppState, worker_id: Uuid, c: &McpCall) -> Result<Answer, McpError> {
    let limits = &app.config.limits;
    // Before the database: a worker flooding calls costs no lookups (SPEC §20).
    if !per_minute(
        app,
        CALLS_PER_WORKER,
        worker_id,
        limits.mcp_calls_per_worker_min,
    ) {
        return Err(too_often("calls from this worker"));
    }
    if !is_request_id(&c.request_id) {
        return Err(err(
            error_code::INVALID_ARGS,
            format!("request_id is not 1-{MAX_REQUEST_ID} characters of A-Z a-z 0-9 . _ : -"),
        ));
    }
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
    if caller.archived {
        return Err(err(
            error_code::UNAVAILABLE,
            "this session is archived; its agent acts no more",
        ));
    }
    if !per_minute(
        app,
        CALLS_PER_SESSION,
        caller.id,
        limits.mcp_calls_per_session_min,
    ) {
        return Err(too_often("hqc tool calls from this session"));
    }
    let tool = REGISTRY.iter().find(|t| t.name == c.tool).ok_or_else(|| {
        err(
            error_code::UNKNOWN_TOOL,
            format!("the hub has no tool {}", c.tool),
        )
    })?;
    (tool.run)(app, &caller, c).await
}

/// The arguments as `T`; absent arguments read as `{}`.
pub(crate) fn args<T: DeserializeOwned>(v: Value) -> Result<T, McpError> {
    let v = if v.is_null() { json!({}) } else { v };
    serde_json::from_value(v).map_err(|e| err(error_code::INVALID_ARGS, e.to_string()))
}

/// The owner's project by slug or id (SPEC §7); another owner's is `not_found`.
pub(crate) async fn owner_project(app: &AppState, owner: Uuid, p: &str) -> Result<Uuid, McpError> {
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
/// workers' service sessions left out, as `{address, title, status, subagents?, current?}`.
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
            // Background subagents run on after the turn: an idle session may still work.
            if s.subagents > 0 {
                v["subagents"] = json!(s.subagents);
            }
            if s.id == caller.id {
                v["current"] = json!(true);
            }
            v
        })
        .collect();
    Ok(json!({ "sessions": sessions, "truncated": truncated }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    project: Option<String>,
    #[serde(default)]
    fuzzy: bool,
    cursor: Option<String>,
}

/// `hqc_search_history(query, project?, fuzzy?, cursor?)` (SPEC §7, §15): the query of
/// `GET /api/search` for the calling session's owner, as `{results: [{address, title,
/// archived, seq?, type?, ts, snippet}], next_cursor?}`. No side effects.
async fn search_history(app: &AppState, caller: &SessionRow, v: Value) -> ToolResult {
    let a: SearchArgs = args(v)?;
    let project_id = match a.project {
        Some(p) => Some(owner_project(app, caller.owner_id, &p).await?),
        None => None,
    };
    let q = crate::search::Query {
        text: a.query,
        project_id,
        fuzzy: a.fuzzy,
        cursor: a.cursor,
        limit: crate::search::PAGE,
    };
    let page = crate::search::capped(app, caller.owner_id, &q)
        .await
        .map_err(|e| match e {
            crate::search::SearchError::Invalid(m) => err(error_code::INVALID_ARGS, m),
            crate::search::SearchError::Limited(m) => err(error_code::LIMIT_EXCEEDED, m),
            e @ crate::search::SearchError::Busy => err(error_code::UNAVAILABLE, e.to_string()),
            crate::search::SearchError::Db(e) => internal(e),
        })?;
    let results: Vec<Value> = page
        .results
        .into_iter()
        .map(|h| {
            let mut v = json!({
                "address": h.address,
                "title": h.title,
                "archived": h.archived,
                "ts": h.ts,
                "snippet": h.snippet,
            });
            if let (Some(seq), Some(kind)) = (h.seq, h.kind) {
                v["seq"] = json!(seq);
                v["type"] = json!(kind);
            }
            v
        })
        .collect();
    let mut out = json!({ "results": results });
    if let Some(c) = page.next_cursor {
        out["next_cursor"] = json!(c);
    }
    Ok(out)
}

/// The id of what call `request_id` of `tool` from session `caller` writes: the same for a
/// repeat of the call.
pub fn once_id(tool: &str, caller: Uuid, request_id: &str) -> Uuid {
    const NS: Uuid = Uuid::from_u128(0x0199_f3a2_7c10_7000_8000_6871_635f_6d63);
    Uuid::new_v5(&NS, format!("{tool}\n{caller}\n{request_id}").as_bytes())
}

/// `hqc_attach` as the worker sends it (SPEC §7): the worker has read the file and uploaded
/// it for the calling session (`PUT /worker/artifacts`); the hub writes the `attachment`
/// event authored by the agent, once per `request_id`.
async fn attach(app: &AppState, caller: &SessionRow, request_id: &str, v: Value) -> ToolResult {
    let a: AttachCall = args(v)?;
    let event_id = once_id(hqc_proto::mcp::ATTACH, caller.id, request_id);
    if let Some(row) = db::event(&app.db, event_id).await.map_err(internal)? {
        return attached(caller, &row);
    }
    let art = crate::artifacts::artifact(&app.db, a.artifact_id)
        .await
        .map_err(internal)?
        .filter(|r| r.owner_id == caller.owner_id);
    let linked = match &art {
        Some(r) => crate::artifacts::of_session(&app.db, caller.id, r.id)
            .await
            .map_err(internal)?,
        None => false,
    };
    let art = art.filter(|_| linked).ok_or_else(|| {
        err(
            error_code::NOT_FOUND,
            format!("no artifact {} uploaded for this session", a.artifact_id),
        )
    })?;
    let mut payload = json!({"artifact_id": art.id, "kind": art.kind, "name": art.name});
    if let Some(c) = a.caption.filter(|c| !c.trim().is_empty()) {
        payload["caption"] = json!(c);
    }
    let author = json!({"kind": "agent", "id": caller.id});
    let row = match crate::events::store_hub_event_once(
        app,
        caller,
        event_id,
        author,
        "attachment",
        payload,
    )
    .await
    .map_err(internal)?
    {
        Some(row) => row,
        // The same call running twice at once: the other one stored it.
        None => db::event(&app.db, event_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| internal("attachment event vanished"))?,
    };
    attached(caller, &row)
}

/// The result of `hqc_attach`, from its event.
fn attached(caller: &SessionRow, row: &db::EventRow) -> ToolResult {
    if row.session_id != caller.id || row.kind != "attachment" {
        return Err(internal(format!(
            "event {} is not this call's attachment",
            row.event_id
        )));
    }
    Ok(json!({
        "event_id": row.event_id,
        "artifact_id": row.payload["artifact_id"],
        "name": row.payload["name"],
        "kind": row.payload["kind"],
    }))
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
    fn once_ids() {
        let (s1, s2) = (Uuid::now_v7(), Uuid::now_v7());
        let a = once_id("hqc_attach", s1, "r1");
        assert_eq!(a, once_id("hqc_attach", s1, "r1"));
        assert_ne!(a, once_id("hqc_attach", s1, "r2"));
        assert_ne!(a, once_id("hqc_attach", s2, "r1"));
        assert_ne!(a, once_id("hqc_send", s1, "r1"));
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
