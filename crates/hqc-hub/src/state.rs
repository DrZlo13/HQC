//! Computed session state and queue length (SPEC §17.1). Neither is stored: both are derived
//! on every read from the session row, its events, open requests and worker presence.

use hqc_proto::{
    api::{Agent, Session, SpawnedBy},
    event::SessionState,
    frame::{LockedBy, PermissionMode, ProcessState, Runtime, TitleSource},
};
use serde::de::DeserializeOwned;
use serde_json::Value;
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
/// messages and `system`, except the worker's log lines in its service session (SPEC
/// §10.2): they would keep the badge lit.
const UNREAD_FILTER: &str = "((e.type IN ('approval.request', 'clarify.request', 'peer.message')
    OR (e.type = 'system' AND e.payload->>'code' IS DISTINCT FROM 'worker_log'))
    OR (e.type = 'agent.message' AND e.payload->>'final' = 'true'))";

#[derive(Debug, FromRow)]
struct Aggregates {
    id: Uuid,
    queue_len: i64,
    has_failed: bool,
    has_open_request: bool,
    last_read_seq: i64,
    unread: i64,
    /// The address of `spawned_by` (SPEC §7).
    spawned_by_address: Option<String>,
}

/// A stored enum name (`runtime`, `process_state`, `locked_by`) as its proto type.
fn stored_enum<T: DeserializeOwned>(name: &str, fallback: T) -> T {
    serde_json::from_value(Value::String(name.to_string())).unwrap_or(fallback)
}

async fn aggregates(db: &PgPool, user: Uuid, ids: &[Uuid]) -> sqlx::Result<Vec<Aggregates>> {
    sqlx::query_as(&format!(
        "SELECT s.id,
            (SELECT count(*) FROM events e WHERE e.session_id = s.id AND e.delivery = 'pending') AS queue_len,
            EXISTS (SELECT 1 FROM events e WHERE e.session_id = s.id AND e.delivery = 'failed') AS has_failed,
            EXISTS (SELECT 1 FROM pending_requests p WHERE p.session_id = s.id AND p.closed_at IS NULL
                    AND p.kind IN ('approval', 'clarify', 'worker_approval')) AS has_open_request,
            COALESCE(r.last_read_seq, 0) AS last_read_seq,
            (SELECT count(*) FROM events e WHERE e.session_id = s.id
                AND e.seq > COALESCE(r.last_read_seq, 0) AND {UNREAD_FILTER}) AS unread,
            (SELECT p.slug || '/' || sp.slug FROM sessions sp JOIN projects p ON p.id = sp.project_id
                WHERE sp.id = s.spawned_by) AS spawned_by_address
         FROM sessions s
         LEFT JOIN read_cursors r ON r.session_id = s.id AND r.user_id = $1
         WHERE s.id = ANY($2)"
    ))
    .bind(user)
    .bind(ids)
    .fetch_all(db)
    .await
}

/// The session objects (SPEC §6) of the given rows for `user` (unread is per user), in the
/// order given.
pub async fn views(
    app: &AppState,
    user: Uuid,
    rows: Vec<SessionRow>,
) -> sqlx::Result<Vec<Session>> {
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
            let count = |n: i64| n.max(0) as u64;
            Some(Session {
                id: s.id,
                project_id: s.project_id,
                owner_id: s.owner_id,
                workspace_id: s.workspace_id,
                worker_id: s.worker_id,
                cwd: s.cwd,
                title: s.title,
                title_source: stored_enum(&s.title_source, TitleSource::Default),
                slug: s.slug,
                agent: Agent {
                    runtime: stored_enum(&s.runtime, Runtime::ClaudeCode),
                    acp_agent: s.acp_agent,
                    model: s.model,
                    runtime_session_id: s.runtime_session_id,
                    process_state: stored_enum(&s.process_state, ProcessState::Stopped),
                    system_prompt: s.system_prompt,
                    disallowed_tools: serde_json::from_value(s.disallowed_tools)
                        .unwrap_or_default(),
                    restricted: s.restricted,
                    start_failures: s.start_failures.max(0) as u32,
                },
                auto_approve: s.auto_approve,
                wake_on_send: s.wake_on_send,
                locked_by: stored_enum(&s.locked_by, LockedBy::None),
                last_seq: count(s.last_seq),
                archived: s.archived,
                state,
                queue_len: count(a.queue_len),
                last_read_seq: count(a.last_read_seq),
                unread: count(a.unread),
                created_at: s.created_at,
                updated_at: s.updated_at,
                held_by: s.held_by.and_then(|v| serde_json::from_value(v).ok()),
                commands: s.commands.and_then(|v| serde_json::from_value(v).ok()),
                system: s.system,
                permission_mode: s.permission_mode.as_deref().and_then(PermissionMode::parse),
                effective_permission_mode: s
                    .effective_permission_mode
                    .as_deref()
                    .and_then(PermissionMode::parse),
                permission_capped_by: s.permission_capped_by,
                spawned_by: s.spawned_by.zip(a.spawned_by_address.clone()).map(
                    |(session_id, address)| SpawnedBy {
                        session_id,
                        address,
                    },
                ),
            })
        })
        .collect())
}

pub async fn view(app: &AppState, user: Uuid, row: SessionRow) -> sqlx::Result<Option<Session>> {
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
