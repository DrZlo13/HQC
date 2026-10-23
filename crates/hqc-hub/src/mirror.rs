//! Sessions of Claude Code mirrored from a worker, and who drives them (SPEC §9).
//!
//! - `mirror.event` ([`ingest`]): the event is stored in the session of its transcript
//!   (`runtime_session_id` on that worker); an unknown transcript becomes a session in the
//!   workspace's project with `cwd` from the record, the `ai-title` as title, and
//!   `locked_by: vscode` if the worker saw the file written within 30 s, else `none`
//!   (SPEC §9.1). A new event marked `recent` makes the session `locked_by: vscode`; if hqc
//!   had it, that is a release ([`released`]).
//! - `locked_by` from the worker's `session.state` ([`next_lock`]): `vscode` wins, and a
//!   stopped process does not end a VS Code lock.
//! - `POST /takeover` ([`takeover`]): after 30 s of silence the session becomes
//!   `locked_by: hqc` and the worker gets `session.start` with `mode: resume`.
//! - After `hello` the worker gets `mirror.set` for every mirrored workspace of its own.

use std::path::Path;

use chrono::Utc;
use hqc_proto::{
    event::EventKind,
    frame::{Ack, HubFrame, MirrorEvent, MirrorSet, MIRROR_RECENT_S},
};
use serde_json::{json, Value};

use crate::{
    app::AppState,
    db::{self, Inserted, NewEvent, SessionRow},
    delivery,
    error::{ApiError, ApiResult},
    events,
    worker_ws::WorkerConn,
};

/// `system.code` written when hqc takes a session over from VS Code (SPEC §4, §9.3).
pub const TAKEN_OVER: &str = "session_taken_over";
/// `system.code` written when the session goes back to VS Code (SPEC §9.3).
pub const RELEASED: &str = "session_released";

const TAKEN_OVER_TEXT: &str = "Сессию подхватил hqc. Если она открыта в VS Code, её контекст \
    там устарел: после возврата переоткройте сессию в VS Code.";
const RELEASED_TEXT: &str = "Сессию продолжили в VS Code: hqc остановил агента после хода. \
    Чтобы вести её отсюда, подхватите её снова.";

/// `cwd` of the record relative to the workspace, `.` for the workspace itself; `None`
/// when it lies outside.
fn relative_cwd(workspace: &str, cwd: &str) -> Option<String> {
    let rel = Path::new(cwd).strip_prefix(workspace).ok()?;
    let s = rel.to_string_lossy().into_owned();
    if s.split('/').any(|c| c == "..") {
        return None;
    }
    Some(if s.is_empty() { ".".into() } else { s })
}

/// A `mirror.event` (SPEC §5.4): `ack {event_id, seq}`; without `seq` when the workspace is
/// not mirrored (any more) on this worker or the record lies outside it.
pub async fn ingest(
    app: &AppState,
    conn: &WorkerConn,
    raw: &Value,
    m: &MirrorEvent,
) -> anyhow::Result<Ack> {
    let event_id = m.event.event_id;
    let dropped = Ack {
        event_id: Some(event_id),
        seq: None,
    };
    let ws = match db::workspace_by_path(&app.db, conn.worker_id, &m.workspace_path).await? {
        Some(w) if w.mirror_enabled => w,
        _ => {
            tracing::debug!(worker_id = %conn.worker_id, path = %m.workspace_path, "mirror.event for a workspace that is not mirrored, dropped");
            return Ok(dropped);
        }
    };
    let Some(cwd) = relative_cwd(&ws.path, &m.cwd) else {
        tracing::warn!(worker_id = %conn.worker_id, cwd = %m.cwd, workspace = %ws.path, "mirror.event outside its workspace, dropped");
        return Ok(dropped);
    };
    if matches!(m.event.kind, EventKind::AgentDelta { .. }) {
        return Ok(dropped);
    }
    let title = m.title.as_deref().and_then(crate::worker_ws::runtime_title);
    let s = match db::session_by_runtime(&app.db, conn.worker_id, &m.runtime_session_id).await? {
        Some(s) => s,
        None => {
            let id = db::create_session(
                &app.db,
                db::NewSession {
                    project_id: ws.project_id,
                    owner_id: ws.owner_id,
                    workspace_id: Some(ws.id),
                    worker_id: Some(conn.worker_id),
                    cwd: &cwd,
                    title: None,
                    model: None,
                    auto_approve: false,
                },
            )
            .await?;
            let lock = if m.recent { "vscode" } else { "none" };
            db::adopt_transcript(&app.db, id, &m.runtime_session_id, lock).await?;
            if let Some(t) = &title {
                db::set_runtime_title(&app.db, id, t).await?;
            }
            let s = db::session(&app.db, id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("session {id} vanished"))?;
            tracing::info!(session_id = %id, runtime_session_id = %m.runtime_session_id, workspace = %ws.path, %cwd, locked_by = lock, "session imported from Claude Code");
            events::system(
                app,
                &s,
                "session_created",
                "Сессия импортирована из Claude Code",
            )
            .await?;
            s
        }
    };

    let kind_name = raw
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let author = if matches!(m.event.kind, EventKind::UserMessage { .. }) {
        events::user_author(s.owner_id)
    } else {
        json!({"kind": "agent", "id": s.id})
    };
    let new = NewEvent {
        event_id,
        session_id: s.id,
        ts: m.event.ts,
        author,
        kind: kind_name,
        payload: raw.get("payload").cloned().unwrap_or(Value::Null),
        chain: None,
        client_op_id: None,
        delivery: raw
            .get("delivery")
            .and_then(Value::as_str)
            .map(str::to_string),
        delivered_at: m.event.delivered_at,
    };
    let ack = match db::insert_event(&app.db, &new).await? {
        Inserted::New(row) => {
            events::after_mirror_store(app, &s, &row, &m.event.kind).await?;
            if m.recent && !s.archived {
                db::touch_vscode(&app.db, s.id).await?;
                if s.locked_by != "vscode" {
                    db::set_locked_by(&app.db, s.id, "vscode").await?;
                    if s.locked_by == "hqc" {
                        released(app, s.id).await?;
                    }
                }
            }
            Ack {
                event_id: Some(event_id),
                seq: Some(row.seq as u64),
            }
        }
        Inserted::Duplicate(seq) => Ack {
            event_id: Some(event_id),
            seq: Some(seq as u64),
        },
        Inserted::UnknownSession => dropped,
    };
    if let Some(t) = &title {
        db::set_runtime_title(&app.db, s.id, t).await?;
    }
    app.session_changed(s.id);
    Ok(ack)
}

/// The new `locked_by` when the worker reports `reported` for a session that has `prev`
/// (SPEC §9.3): the worker's `vscode` (it stopped its agent for another writer) always wins;
/// a VS Code lock is not ended by the worker's process stopping or by a process the hub
/// did not start for a takeover.
pub fn next_lock<'a>(prev: &'a str, reported: &'a str) -> &'a str {
    match (prev, reported) {
        (_, "vscode") | ("vscode", _) => "vscode",
        (_, r) => r,
    }
}

/// The session went from hqc to VS Code: `system session_released`, pending input fails
/// (it would otherwise wait for a takeover nobody asked for).
pub async fn released(app: &AppState, session_id: uuid::Uuid) -> anyhow::Result<()> {
    let Some(s) = db::session(&app.db, session_id).await? else {
        return Ok(());
    };
    tracing::info!(%session_id, "session continued in VS Code");
    for row in db::move_inputs(&app.db, session_id, "pending", "failed").await? {
        delivery::notify_delivery(app, &row);
    }
    events::system(app, &s, RELEASED, RELEASED_TEXT).await?;
    app.session_changed(session_id);
    Ok(())
}

/// `POST /api/sessions/{id}/takeover` (SPEC §9.3).
pub async fn takeover(app: &AppState, s: &SessionRow) -> ApiResult<()> {
    if s.archived {
        return Err(ApiError::Conflict(
            "archived",
            "the session is archived".into(),
        ));
    }
    if s.locked_by == "hqc" {
        return Ok(());
    }
    if let Some(at) = s.vscode_active_at {
        let quiet = (Utc::now() - at).num_seconds();
        if s.locked_by == "vscode" && quiet < MIRROR_RECENT_S as i64 {
            return Err(ApiError::Conflict(
                "vscode_active",
                format!(
                    "VS Code wrote to the session {quiet} s ago; take it over after {MIRROR_RECENT_S} s of silence"
                ),
            ));
        }
    }
    if !s.worker_id.is_some_and(|w| app.workers.is_online(w)) {
        return Err(ApiError::Conflict(
            "worker_offline",
            "the session's worker is offline".into(),
        ));
    }
    db::set_locked_by(&app.db, s.id, "hqc").await?;
    // `mode: resume` with the transcript's uuid: the adapter continues it with `--resume`.
    db::mark_start_sent(&app.db, s.id).await?;
    events::system(app, s, TAKEN_OVER, TAKEN_OVER_TEXT).await?;
    tracing::info!(session_id = %s.id, runtime_session_id = ?s.runtime_session_id, "session taken over from VS Code");
    delivery::kick(app, s.id, true).await;
    app.session_changed(s.id);
    Ok(())
}

/// After `hello`: the worker learns which of its folders are mirrored.
pub async fn after_hello(app: &AppState, conn: &WorkerConn) -> anyhow::Result<()> {
    for ws in db::mirrored_workspaces(&app.db, conn.worker_id).await? {
        set(conn, &ws.path, true);
    }
    Ok(())
}

pub fn set(conn: &WorkerConn, path: &str, enabled: bool) {
    conn.send(HubFrame::MirrorSet(MirrorSet {
        workspace_path: path.to_string(),
        enabled,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_inside_the_workspace() {
        assert_eq!(relative_cwd("/a/b", "/a/b").as_deref(), Some("."));
        assert_eq!(relative_cwd("/a/b", "/a/b/c/d").as_deref(), Some("c/d"));
        assert_eq!(relative_cwd("/a/b", "/a/bc"), None);
        assert_eq!(relative_cwd("/a/b", "/x"), None);
    }

    #[test]
    fn lock_transitions() {
        assert_eq!(next_lock("hqc", "vscode"), "vscode");
        assert_eq!(next_lock("vscode", "none"), "vscode");
        assert_eq!(next_lock("vscode", "hqc"), "vscode");
        assert_eq!(next_lock("none", "hqc"), "hqc");
        assert_eq!(next_lock("hqc", "none"), "none");
    }
}
