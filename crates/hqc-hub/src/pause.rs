//! Pausing a session (SPEC §17.3). The hub lets a running turn end, then ends the agent's
//! process as a restart does and neither starts it again nor delivers input to it until the
//! session is resumed: inputs typed meanwhile wait on the hub. Resuming starts the agent with
//! the same history and gives it, first, a note that it was paused and for how long.

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
    events::{self, user_author},
    state,
    worker_ws::StartState,
};

/// `POST /api/sessions/{id}/pause`: the agent stops now when no turn runs, else when the turn
/// ends ([`stop_if_paused`]). A paused session stays as it is.
pub async fn pause(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Write).await?;
    not_system(&s)?;
    driven_from_hqc(&s)?;
    // The turn as it is when the pause is set: one that ends after it stops the agent itself.
    let turn_open: Option<bool> = sqlx::query_scalar(
        "UPDATE sessions SET paused_at = now(), updated_at = now()
         WHERE id = $1 AND paused_at IS NULL RETURNING turn_open",
    )
    .bind(id)
    .fetch_optional(&app.db)
    .await?;
    if turn_open == Some(false) {
        stop_if_paused(&app, id).await?;
    }
    app.session_changed(id);
    view(&app, &user, id).await
}

/// `POST /api/sessions/{id}/resume`. Paused during a turn that still runs, the agent was never
/// stopped: the inputs held meanwhile go to it. Otherwise the agent starts, resuming its
/// transcript, with the note of [`note`] as a pending input ahead of them. A session that is not
/// paused stays as it is.
pub async fn resume(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let s = session_for(&app, &user, id, Action::Write).await?;
    not_system(&s)?;
    driven_from_hqc(&s)?;
    // Read under the row's lock: the end of a turn may have set the pause's time again just now.
    let resumed: Option<(DateTime<Utc>, bool, DateTime<Utc>)> = sqlx::query_as(
        "WITH p AS (SELECT paused_at FROM sessions WHERE id = $1 AND paused_at IS NOT NULL FOR UPDATE)
         UPDATE sessions s SET paused_at = NULL, updated_at = now() FROM p
         WHERE s.id = $1 RETURNING p.paused_at, s.turn_open, now()",
    )
    .bind(id)
    .fetch_optional(&app.db)
    .await?;
    match resumed {
        None => {}
        Some((_, true, _)) => delivery::kick(&app, id, false).await,
        Some((paused_at, false, now)) => {
            events::store_hub_event(
                &app,
                &s,
                user_author(user.id),
                "user.message",
                json!({"text": note(paused_at, now), "artifact_ids": [], "resume": true}),
                None,
                Some("pending"),
            )
            .await?;
            delivery::kick(&app, id, true).await;
        }
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

/// The input a resumed agent gets first (SPEC §17.3).
fn note(paused_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    format!(
        "[hqc] You were paused at {} UTC on {} and are resumed now: {} passed.",
        paused_at.format("%H:%M"),
        paused_at.format("%Y-%m-%d"),
        elapsed((now - paused_at).num_seconds()),
    )
}

/// `45 s`, `12 min`, `1 h 20 min`, `3 d 2 h`: the two largest units, rounded down, a zero
/// second one left out.
fn elapsed(secs: i64) -> String {
    let (d, h, m) = (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{secs} s"),
        (0, 0, m) => format!("{m} min"),
        (0, h, 0) => format!("{h} h"),
        (0, h, m) => format!("{h} h {m} min"),
        (d, 0, _) => format!("{d} d"),
        (d, h, _) => format!("{d} d {h} h"),
    }
}

async fn view(app: &AppState, user: &CurrentUser, id: Uuid) -> ApiResult<Json<Value>> {
    let s = db::session(&app.db, id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(json!(state::view(app, user.id, s).await?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_names_the_two_largest_units() {
        assert_eq!(elapsed(0), "0 s");
        assert_eq!(elapsed(45), "45 s");
        assert_eq!(elapsed(60), "1 min");
        assert_eq!(elapsed(12 * 60 + 40), "12 min");
        assert_eq!(elapsed(3_600), "1 h");
        assert_eq!(elapsed(3_600 + 20 * 60 + 59), "1 h 20 min");
        assert_eq!(elapsed(86_400 + 59 * 60), "1 d");
        assert_eq!(elapsed(3 * 86_400 + 2 * 3_600 + 30 * 60), "3 d 2 h");
    }

    #[test]
    fn note_says_when_and_how_long() {
        let at = DateTime::parse_from_rfc3339("2026-11-15T09:05:30Z")
            .unwrap()
            .with_timezone(&Utc);
        let now = at + chrono::Duration::seconds(4_830);
        assert_eq!(
            note(at, now),
            "[hqc] You were paused at 09:05 UTC on 2026-11-15 and are resumed now: 1 h 20 min passed."
        );
    }
}
