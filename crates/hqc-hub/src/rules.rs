//! hqc's allow rules of a workspace (T48, SPEC §3 Workspace, §5.3, §6, §14).
//!
//! Stored on the hub, owner only, and handed to the worker with `session.start` and as
//! `workspace.rules` whenever the list changes and after every `hello`; the worker applies
//! them itself in `can_use_tool`, after its `deny`. `GET .../permissions` passes the
//! worker's `permissions.inspect` through for the rules screen.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use hqc_proto::{
    api::{AddWorkspaceRules, WorkspacePermissions, WorkspaceRuleList},
    frame::{HubFrame, PermissionsInspect, ResultBody, WorkspaceRules},
    rules::{parse_allow_rule, MAX_RULES},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    api::{ask_worker, workspace_for},
    app::AppState,
    auth::CurrentUser,
    db,
    error::{ApiError, ApiResult},
    worker_ws::WorkerConn,
};

/// Longest command or tool name `GET .../permissions` checks.
const MAX_CHECK: usize = 16 * 1024;

async fn list_of(app: &AppState, workspace_id: Uuid) -> ApiResult<Json<WorkspaceRuleList>> {
    let rules = db::workspace_rules(&app.db, workspace_id).await?;
    Ok(Json(WorkspaceRuleList {
        rules: rules.iter().map(db::RuleRow::to_api).collect(),
    }))
}

/// `GET /api/workspaces/{id}/rules`.
pub async fn list(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<WorkspaceRuleList>> {
    let ws = workspace_for(&app, &user, id).await?;
    list_of(&app, ws.id).await
}

/// `POST /api/workspaces/{id}/rules {rules[]}`: every rule must be a valid hqc rule
/// (`422 invalid_rule`, nothing saved), at most [`MAX_RULES`] per folder (`422
/// too_many_rules`); rules the folder has already are kept. The worker gets the new list at
/// once if it is connected, else after `hello`.
pub async fn add(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<AddWorkspaceRules>,
) -> ApiResult<Json<WorkspaceRuleList>> {
    let ws = workspace_for(&app, &user, id).await?;
    if b.rules.is_empty() {
        return Err(ApiError::BadRequest("rules must not be empty".into()));
    }
    for r in &b.rules {
        parse_allow_rule(r)
            .map_err(|e| ApiError::Unprocessable("invalid_rule", format!("{r}: {e}")))?;
    }
    if !db::add_workspace_rules(&app.db, ws.id, &b.rules, MAX_RULES).await? {
        return Err(ApiError::Unprocessable(
            "too_many_rules",
            format!("a folder has at most {MAX_RULES} hqc rules"),
        ));
    }
    tracing::info!(workspace_id = %ws.id, rules = ?b.rules, "hqc rules added");
    send(&app, &ws).await?;
    list_of(&app, ws.id).await
}

/// `DELETE /api/workspaces/{id}/rules/{rule_id}`.
pub async fn delete(
    State(app): State<AppState>,
    user: CurrentUser,
    Path((id, rule_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let ws = workspace_for(&app, &user, id).await?;
    if !db::delete_workspace_rule(&app.db, ws.id, rule_id).await? {
        return Err(ApiError::NotFound);
    }
    tracing::info!(workspace_id = %ws.id, %rule_id, "hqc rule removed");
    send(&app, &ws).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct CheckQuery {
    pub command: Option<String>,
    pub tool: Option<String>,
}

/// `GET /api/workspaces/{id}/permissions?command=|tool=`: the worker's `permissions.inspect`
/// for the folder; the worker must be online (`409 worker_offline`, `504 worker_timeout`).
pub async fn inspect(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Query(q): Query<CheckQuery>,
) -> ApiResult<Json<WorkspacePermissions>> {
    let ws = workspace_for(&app, &user, id).await?;
    let clean = |s: Option<String>| -> ApiResult<Option<String>> {
        match s.filter(|s| !s.trim().is_empty()) {
            Some(s) if s.len() > MAX_CHECK || s.contains('\0') => Err(ApiError::BadRequest(
                format!("the check is longer than {MAX_CHECK} bytes or has NUL"),
            )),
            s => Ok(s),
        }
    };
    let (command, tool) = (clean(q.command)?, clean(q.tool)?);
    let path = ws.path.clone();
    match ask_worker(&app, ws.worker_id, |request_id| {
        HubFrame::PermissionsInspect(PermissionsInspect {
            request_id,
            path,
            command,
            tool,
        })
    })
    .await?
    {
        ResultBody::Inspect(r) => Ok(Json(r)),
        other => Err(anyhow::anyhow!("permissions.inspect answered with {other:?}").into()),
    }
}

/// `workspace.rules` with the folder's current list, if its worker is connected.
pub async fn send(app: &AppState, ws: &db::WorkspaceRow) -> anyhow::Result<()> {
    if let Some(conn) = app.workers.get(ws.worker_id) {
        send_to(app, &conn, ws).await?;
    }
    Ok(())
}

async fn send_to(app: &AppState, conn: &WorkerConn, ws: &db::WorkspaceRow) -> anyhow::Result<()> {
    conn.send(HubFrame::WorkspaceRules(WorkspaceRules {
        workspace_path: ws.path.clone(),
        allow_rules: db::workspace_rule_texts(&app.db, ws.id).await?,
    }));
    Ok(())
}

/// After `hello`: every workspace of the worker, also one without rules, so a list emptied
/// while the worker was away is emptied there too.
pub async fn after_hello(app: &AppState, conn: &WorkerConn) -> anyhow::Result<()> {
    for ws in db::workspaces_of_worker(&app.db, conn.worker_id).await? {
        send_to(app, conn, &ws).await?;
    }
    Ok(())
}
