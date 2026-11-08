//! A session forks before a message of its user (SPEC §6 `POST /api/sessions/{id}/fork`):
//! a new session in the same project, folder and worker, whose conversation is the source's
//! up to that message, the message itself left out.
//!
//! - Only a Claude Code session with a workspace and a worker of its owner; the message is a
//!   `user.message` of a person in it (`404` otherwise).
//! - The cut is the source's last `agent.message` before the message: `session.start.fork`
//!   names the source transcript and those events, newest first, and the worker forks the
//!   conversation at the record of the first one it finds (SPEC §5.3, §8.1). With none (the
//!   message was the first), the new session starts with no conversation and no fork.
//! - The new session's feed gets the source's events before the message ([`COPIED`]) under
//!   new ids, read already, and a `system forked_from`. The worker's history is the CLI's:
//!   requests, statuses and usage of the source are not copied.

use serde_json::json;
use uuid::Uuid;

use crate::{
    app::AppState,
    db::{self, SessionRow},
    delivery,
    error::{ApiError, ApiResult},
    events,
};

/// `system.code` at the end of a forked session's copied history.
pub const FORKED_FROM: &str = "forked_from";
/// The event types of the source a fork's feed shows.
const COPIED: &[&str] = &[
    "user.message",
    "agent.message",
    "agent.thinking",
    "tool.call",
    "tool.result",
    "attachment",
    "peer.message",
    "system",
];
/// How many of the source's last `agent.message` events the worker may look for.
const CUTS: i64 = 20;

/// Forks `s` (the caller manages it) before its event `event_id`; the new session.
pub async fn fork(app: &AppState, s: &SessionRow, event_id: Uuid) -> ApiResult<SessionRow> {
    if s.runtime != "claude_code" {
        return Err(ApiError::Conflict(
            "not_forkable",
            "only a Claude Code session forks".into(),
        ));
    }
    let (Some(workspace_id), Some(worker_id)) = (s.workspace_id, s.worker_id) else {
        return Err(ApiError::Conflict(
            "not_forkable",
            "the session has no folder on a worker".into(),
        ));
    };
    let worker = db::worker(&app.db, worker_id).await?;
    if !worker.is_some_and(|w| w.owner_id == s.owner_id && w.revoked_at.is_none()) {
        return Err(ApiError::Conflict(
            "not_forkable",
            "the session's worker is not its owner's".into(),
        ));
    }
    let at = db::event(&app.db, event_id)
        .await?
        .filter(|e| e.session_id == s.id && e.kind == "user.message" && e.author["kind"] == "user")
        .ok_or(ApiError::NotFound)?;
    let cuts = db::agent_messages_before(&app.db, s.id, at.seq, CUTS).await?;
    let title: String = format!("{} (fork)", s.title)
        .chars()
        .take(crate::api::MAX_TITLE)
        .collect();
    let id = db::create_session(
        &app.db,
        db::NewSession {
            project_id: s.project_id,
            owner_id: s.owner_id,
            workspace_id: Some(workspace_id),
            worker_id: Some(worker_id),
            cwd: &s.cwd,
            title: Some(&title),
            model: s.model.as_deref(),
            auto_approve: s.auto_approve,
            permission_mode: s
                .permission_mode
                .as_deref()
                .and_then(hqc_proto::frame::PermissionMode::parse),
        },
    )
    .await?;
    if !cuts.is_empty() {
        let source = s.runtime_session_id.clone().unwrap_or(s.id.to_string());
        let f = json!({"runtime_session_id": source, "at_event_ids": cuts});
        db::set_session_fork(&app.db, id, &f).await?;
        db::copy_events(&app.db, s.id, id, at.seq, COPIED).await?;
    }
    let new = db::session(&app.db, id).await?.ok_or(ApiError::NotFound)?;
    let from = crate::peers::address(app, s).await?;
    let row = events::system(
        app,
        &new,
        FORKED_FROM,
        &format!(
            "Forked from {from} before the message of {}",
            at.ts.format("%Y-%m-%d %H:%M UTC")
        ),
    )
    .await?;
    db::set_read_cursor(&app.db, s.owner_id, id, row.seq).await?;
    tracing::info!(session_id = %id, from = %s.id, cuts = cuts.len(), "session forked");
    delivery::kick(app, id, true).await;
    db::session(&app.db, id).await?.ok_or(ApiError::NotFound)
}
