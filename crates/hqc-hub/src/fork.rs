//! A session forks before a message of its user (SPEC §6 `POST /api/sessions/{id}/fork`):
//! a new session in the same project, folder and worker, whose conversation is the source's
//! up to that message, the message itself left out.
//!
//! - Only a Claude Code session with a workspace and a worker of its owner; the message is a
//!   `user.message` of a person in it (`404` otherwise).
//! - The cut is the source's last `agent.message` before the message, or, for a message with
//!   `during_text`, the answer the client shows above it (SPEC §6): `session.start.fork`
//!   names the source transcript and those events, newest first, and the worker forks the
//!   conversation at the record of the first one it finds (SPEC §5.3, §8.1). With none (the
//!   message was the first), the new session starts with no conversation and no fork.
//! - The new session's feed gets the source's events before the message (and that answer,
//!   last) ([`COPIED`]) under new ids, read already, and a `system forked_from`. A copy keeps
//!   its origin: the id the runtime's record gave the event and the transcript the record was
//!   written in. A fork of
//!   a fork cuts by them in that transcript when the cut is in the copied history, since the
//!   copies' ids are in no transcript and the source fork's own transcript may not exist (the
//!   CLI writes it at its first message) or its session may be gone. The worker's history is the CLI's:
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
    let runtime = s.runtime_session_id.clone().unwrap_or(s.id.to_string());
    // The client shows the answer whose text was being written when the message came above it
    // (SPEC §6): the fork keeps that answer. Only inputs like the message lie between them.
    let above = if at.payload["during_text"] == true {
        answer_above(app, s, &at).await?
    } else {
        None
    };
    // The records the cut may be at, in the transcript of the newest: a fork's own turns are in
    // its transcript, its copied history in the one it was first written in (a fork that never
    // ran, or that was deleted, has none of it).
    let bound = above.as_ref().map_or(at.seq, |a| a.seq + 1);
    let records = db::agent_messages_before(&app.db, s.id, bound, CUTS, &runtime).await?;
    let source = records.first().map(|(_, r)| r.clone());
    let cuts: Vec<Uuid> = records
        .iter()
        .filter(|(_, r)| Some(r) == source.as_ref())
        .map(|(e, _)| *e)
        .collect();
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
    if let Some(source) = source {
        let f = json!({"runtime_session_id": source, "at_event_ids": cuts});
        db::set_session_fork(&app.db, id, &f).await?;
        let also = above.map(|a| a.seq);
        db::copy_events(&app.db, s.id, id, at.seq, also, COPIED, &runtime).await?;
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

/// The agent's `agent.message` the client shows above `at`, an input that came while the
/// agent's text was being written (SPEC §6): the first event after `at` and the inputs like it
/// that follow, when it is the agent's message and answers none of them.
async fn answer_above(
    app: &AppState,
    s: &SessionRow,
    at: &db::EventRow,
) -> sqlx::Result<Option<db::EventRow>> {
    let Some(a) = db::first_after_inputs_during_text(&app.db, s.id, at.seq).await? else {
        return Ok(None);
    };
    if a.kind != "agent.message" || a.author["kind"] != "agent" {
        return Ok(None);
    }
    let answers = a.payload["in_reply_to"]
        .as_str()
        .and_then(|i| i.parse::<Uuid>().ok());
    let answered = match answers {
        Some(i) => db::event(&app.db, i).await?,
        None => None,
    };
    let answers_one =
        answered.is_some_and(|e| e.session_id == s.id && e.seq >= at.seq && e.seq < a.seq);
    Ok((!answers_one).then_some(a))
}
