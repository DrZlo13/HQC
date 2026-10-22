//! Computed session state and queue length (SPEC §17.1). Neither is stored: both are derived
//! on every read from the session row, its events, open requests and worker presence.

use chrono::{DateTime, Utc};
use hqc_proto::event::SessionState;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::{app::AppState, db::SessionRow};

/// Inputs of the §17.1 table.
#[derive(Debug, Clone, Copy, Default)]
pub struct StateInputs {
    pub archived: bool,
    pub locked_vscode: bool,
    /// The session runs on a worker that is not connected.
    pub worker_offline: bool,
    pub open_request: bool,
    pub failed_input: bool,
    pub last_turn_error: bool,
    pub turn_open: bool,
    pub queue_len: i64,
}

/// Priority top-down, first true wins (SPEC §17.1).
pub fn compute_state(i: StateInputs) -> SessionState {
    if i.archived {
        SessionState::Archived
    } else if i.locked_vscode {
        SessionState::LockedVscode
    } else if i.worker_offline {
        SessionState::WorkerOffline
    } else if i.open_request {
        SessionState::Waiting
    } else if i.failed_input || i.last_turn_error {
        SessionState::Error
    } else if i.turn_open {
        SessionState::Running
    } else if i.queue_len > 0 {
        SessionState::Queued
    } else {
        SessionState::Idle
    }
}

/// Events that count as unread (SPEC §6): the final `agent.message`, requests, peer
/// messages and `system`.
const UNREAD_FILTER: &str =
    "(e.type IN ('approval.request', 'clarify.request', 'peer.message', 'system')
    OR (e.type = 'agent.message' AND e.payload->>'final' = 'true'))";

#[derive(Debug, FromRow)]
struct Aggregates {
    id: Uuid,
    queue_len: i64,
    has_failed: bool,
    has_open_request: bool,
    last_read_seq: i64,
    unread: i64,
}

/// A session as the API returns it.
#[derive(Debug, Clone, Serialize)]
pub struct SessionView {
    pub id: Uuid,
    pub project_id: Uuid,
    pub owner_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub worker_id: Option<Uuid>,
    pub cwd: String,
    pub title: String,
    pub slug: String,
    pub agent: Value,
    pub auto_approve: bool,
    pub wake_on_send: bool,
    pub locked_by: String,
    pub last_seq: i64,
    pub archived: bool,
    pub state: SessionState,
    pub queue_len: i64,
    pub last_read_seq: i64,
    pub unread: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl SessionView {
    /// The compact form of `subscribe_summary` (SPEC §6).
    pub fn summary(&self) -> Value {
        json!({
            "session_id": self.id,
            "project_id": self.project_id,
            "title": self.title,
            "state": self.state,
            "queue_len": self.queue_len,
            "unread": self.unread,
            "last_seq": self.last_seq,
            "last_read_seq": self.last_read_seq,
        })
    }
}

async fn aggregates(db: &PgPool, user: Uuid, ids: &[Uuid]) -> sqlx::Result<Vec<Aggregates>> {
    sqlx::query_as(&format!(
        "SELECT s.id,
            (SELECT count(*) FROM events e WHERE e.session_id = s.id AND e.delivery = 'pending') AS queue_len,
            EXISTS (SELECT 1 FROM events e WHERE e.session_id = s.id AND e.delivery = 'failed') AS has_failed,
            EXISTS (SELECT 1 FROM pending_requests p WHERE p.session_id = s.id AND p.closed_at IS NULL
                    AND p.kind IN ('approval', 'clarify')) AS has_open_request,
            COALESCE(r.last_read_seq, 0) AS last_read_seq,
            (SELECT count(*) FROM events e WHERE e.session_id = s.id
                AND e.seq > COALESCE(r.last_read_seq, 0) AND {UNREAD_FILTER}) AS unread
         FROM sessions s
         LEFT JOIN read_cursors r ON r.session_id = s.id AND r.user_id = $1
         WHERE s.id = ANY($2)"
    ))
    .bind(user)
    .bind(ids)
    .fetch_all(db)
    .await
}

/// Views of the given sessions for `user` (unread is per user), in the order given.
pub async fn views(
    app: &AppState,
    user: Uuid,
    rows: Vec<SessionRow>,
) -> sqlx::Result<Vec<SessionView>> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let aggs = aggregates(&app.db, user, &ids).await?;
    Ok(rows
        .into_iter()
        .filter_map(|s| {
            let a = aggs.iter().find(|a| a.id == s.id)?;
            let worker_offline = s.worker_id.is_some_and(|w| !app.workers.is_online(w));
            let state = compute_state(StateInputs {
                archived: s.archived,
                locked_vscode: s.locked_by == "vscode",
                worker_offline,
                open_request: a.has_open_request,
                failed_input: a.has_failed,
                last_turn_error: s.last_turn_error,
                turn_open: s.turn_open,
                queue_len: a.queue_len,
            });
            Some(SessionView {
                id: s.id,
                project_id: s.project_id,
                owner_id: s.owner_id,
                workspace_id: s.workspace_id,
                worker_id: s.worker_id,
                cwd: s.cwd,
                title: s.title,
                slug: s.slug,
                agent: json!({
                    "runtime": s.runtime,
                    "acp_agent": s.acp_agent,
                    "model": s.model,
                    "runtime_session_id": s.runtime_session_id,
                    "process_state": s.process_state,
                    "system_prompt": s.system_prompt,
                    "disallowed_tools": s.disallowed_tools,
                    "restricted": s.restricted,
                    "start_failures": s.start_failures,
                }),
                auto_approve: s.auto_approve,
                wake_on_send: s.wake_on_send,
                locked_by: s.locked_by,
                last_seq: s.last_seq,
                archived: s.archived,
                state,
                queue_len: a.queue_len,
                last_read_seq: a.last_read_seq,
                unread: a.unread,
                created_at: s.created_at,
                updated_at: s.updated_at,
            })
        })
        .collect())
}

pub async fn view(
    app: &AppState,
    user: Uuid,
    row: SessionRow,
) -> sqlx::Result<Option<SessionView>> {
    Ok(views(app, user, vec![row]).await?.pop())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_order() {
        let all = StateInputs {
            archived: true,
            locked_vscode: true,
            worker_offline: true,
            open_request: true,
            failed_input: true,
            last_turn_error: true,
            turn_open: true,
            queue_len: 1,
        };
        assert_eq!(compute_state(all), SessionState::Archived);
        let i = StateInputs {
            archived: false,
            ..all
        };
        assert_eq!(compute_state(i), SessionState::LockedVscode);
        let i = StateInputs {
            locked_vscode: false,
            ..i
        };
        assert_eq!(compute_state(i), SessionState::WorkerOffline);
        let i = StateInputs {
            worker_offline: false,
            ..i
        };
        assert_eq!(compute_state(i), SessionState::Waiting);
        let i = StateInputs {
            open_request: false,
            ..i
        };
        assert_eq!(compute_state(i), SessionState::Error);
        let i = StateInputs {
            failed_input: false,
            ..i
        };
        assert_eq!(compute_state(i), SessionState::Error);
        let i = StateInputs {
            last_turn_error: false,
            ..i
        };
        assert_eq!(compute_state(i), SessionState::Running);
        let i = StateInputs {
            turn_open: false,
            ..i
        };
        assert_eq!(compute_state(i), SessionState::Queued);
        let i = StateInputs { queue_len: 0, ..i };
        assert_eq!(compute_state(i), SessionState::Idle);
    }
}
