//! Pausing a session (SPEC §17.3). The hub stops a running turn as Stop does, then, when it has
//! ended, ends the agent's process as a restart does and neither starts it again nor delivers
//! input to it until the session is resumed: inputs typed meanwhile wait on the hub. Resuming
//! starts the agent with the same history and gives it, first, a note that it was paused and for
//! how long.

use axum::{
    extract::{Path, State},
    Json,
};
use chrono::{DateTime, Utc};
use hqc_proto::frame::HubFrame;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    api::{driven_from_hqc, not_system, session_for, Action},
    app::AppState,
    auth::CurrentUser,
    db, delivery,
    error::{ApiError, ApiResult},
    events::{self, elapsed, user_author},
    state,
    worker_ws::StartState,
};

/// `POST /api/sessions/{id}/pause`: the agent stops now when no turn runs; a running turn is
/// stopped as `POST /stop` stops it, and the agent then stops when that turn ends
/// ([`stop_if_paused`]). A paused session stays as it is.
pub async fn pause(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Write).await?;
    not_system(&s)?;
    not_room(&s)?;
    driven_from_hqc(&s)?;
    // The turn as it is when the pause is set: one still running is stopped now, and its end
    // stops the agent itself. `pause_cut_turn` keeps that for the note of the resume.
    let turn_open: Option<bool> = sqlx::query_scalar(
        "UPDATE sessions SET paused_at = now(), pause_cut_turn = turn_open, updated_at = now()
         WHERE id = $1 AND paused_at IS NULL RETURNING turn_open",
    )
    .bind(id)
    .fetch_optional(&app.db)
    .await?;
    match turn_open {
        Some(false) => stop_if_paused(&app, id).await?,
        Some(true) => {
            crate::api::stop_session(&app, &s).await?;
        }
        None => {}
    }
    app.session_changed(id);
    view(&app, &user, id).await
}

/// `POST /api/sessions/{id}/resume`: the note of [`note`] as a pending input ahead of the inputs
/// held meanwhile. The agent starts, resuming its transcript, when its process was ended; resumed
/// before the turn the pause stopped has ended, the process still runs and they go to it. A
/// session that is not paused stays as it is.
pub async fn resume(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Write).await?;
    not_system(&s)?;
    not_room(&s)?;
    driven_from_hqc(&s)?;
    // Read under the row's lock: the end of a turn may have set the pause's time again just now.
    let resumed: Option<(DateTime<Utc>, bool, bool, DateTime<Utc>)> = sqlx::query_as(
        "WITH p AS (SELECT paused_at, pause_cut_turn FROM sessions
                    WHERE id = $1 AND paused_at IS NOT NULL FOR UPDATE)
         UPDATE sessions s SET paused_at = NULL, pause_cut_turn = false, updated_at = now() FROM p
         WHERE s.id = $1 RETURNING p.paused_at, p.pause_cut_turn, s.turn_open, now()",
    )
    .bind(id)
    .fetch_optional(&app.db)
    .await?;
    if let Some((paused_at, cut_turn, turn_open, now)) = resumed {
        // Also when the stopped turn has not ended yet: it was cut, and the agent goes on only
        // on an input.
        events::store_hub_event(
            &app,
            &s,
            user_author(user.id),
            "user.message",
            json!({"text": note(paused_at, now, cut_turn), "artifact_ids": [], "resume": true}),
            None,
            Some("pending"),
        )
        .await?;
        delivery::kick(&app, id, !turn_open).await;
    }
    app.session_changed(id);
    view(&app, &user, id).await
}

/// The end of a turn of a paused session, or a pause with no turn running: the agent's process
/// ends as in a restart (`api::restart`), and nothing starts it again; `paused_at` becomes the
/// time it stopped. With the worker away only the time is set, and the kill goes after its next
/// `hello` (`worker_ws::after_hello`). A session that is not paused stays as it is.
pub async fn stop_if_paused(app: &AppState, session_id: Uuid) -> anyhow::Result<()> {
    let worker: Option<Option<Uuid>> = sqlx::query_scalar(
        "UPDATE sessions SET paused_at = now() WHERE id = $1 AND paused_at IS NOT NULL
         RETURNING worker_id",
    )
    .bind(session_id)
    .fetch_optional(&app.db)
    .await?;
    let Some(worker) = worker else {
        return Ok(());
    };
    if let Some(conn) = worker.and_then(|w| app.workers.get(w)) {
        // Under the lock, as a restart sends its kill.
        let mut st = conn.st.lock().await;
        conn.send(HubFrame::SessionKill { session_id });
        if let Some(sc) = st.sessions.get_mut(&session_id) {
            sc.start = StartState::NotStarted;
            sc.inflight.clear();
        }
    }
    app.session_changed(session_id);
    Ok(())
}

/// The input a resumed agent gets first (SPEC §17.3). Claude Code shows the agent the step a
/// pause cut as rejected by the user, so after a pause that stopped a turn (`cut_turn`) the note
/// says it was the pause. After a pause of an idle agent it does not: a rejected step there is
/// the user's own.
fn note(paused_at: DateTime<Utc>, now: DateTime<Utc>, cut_turn: bool) -> String {
    let note = format!(
        "[hqc] You were paused at {} UTC on {} and are resumed now: {} passed.",
        paused_at.format("%H:%M"),
        paused_at.format("%Y-%m-%d"),
        elapsed((now - paused_at).num_seconds()),
    );
    if cut_turn {
        format!("{note} {RESUME_HINT}")
    } else {
        note
    }
}

/// The second sentence of [`note`] after a pause that stopped a turn.
const RESUME_HINT: &str = "If the pause cut a step of your work (it shows as a tool use the user \
rejected or interrupted), that was the pause, not the user: go on with the task from where it \
stopped.";

/// A room has no agent of its own to pause (SPEC §7.1.1): its agents are paused one by one.
fn not_room(s: &db::SessionRow) -> ApiResult<()> {
    if s.room_mode.is_some() {
        return Err(ApiError::Conflict(
            "room",
            "a room has no agent of its own: its agents' sessions are paused and resumed".into(),
        ));
    }
    Ok(())
}

async fn view(app: &AppState, user: &CurrentUser, id: Uuid) -> ApiResult<Json<Value>> {
    let s = db::session(&app.db, id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(json!(state::view(app, user.id, s).await?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_says_when_and_how_long() {
        let at = DateTime::parse_from_rfc3339("2026-11-15T09:05:30Z")
            .unwrap()
            .with_timezone(&Utc);
        let now = at + chrono::Duration::seconds(4_830);
        let first = "[hqc] You were paused at 09:05 UTC on 2026-11-15 and are resumed now: 1 h \
                     20 min passed.";
        assert_eq!(note(at, now, false), first);
        assert_eq!(note(at, now, true), format!("{first} {RESUME_HINT}"));
    }
}
