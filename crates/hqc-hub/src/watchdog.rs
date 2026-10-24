//! Turn watchdog (SPEC §17.3): a session that is `running` while its worker sends nothing
//! for it (no event, `agent.delta` included) for `HQC_TURN_WATCHDOG_S` loses the turn.
//!
//! The clock of a session is [`crate::app::Inner::turn_activity`]: the last event from the
//! worker, or the sweep that first saw the session `running`. A session that is not
//! `running` (waiting for an answer, worker offline, no turn) leaves the map, so its clock
//! starts again when it is `running` once more. The map lives in memory: after a hub
//! restart every open turn gets a full period.
//!
//! A sweep reads only sessions with an open turn (partial index `sessions_turn_open`,
//! migration 0012) and runs every [`sweep_step`], at most every 10 s.

use std::collections::HashSet;
use std::time::Duration;

use tokio::time::Instant;
use uuid::Uuid;

use crate::{app::AppState, db, events};

/// Longest pause between sweeps: a turn is lost at most this late (T22: a sweep every
/// second read every session with an open turn).
const MAX_STEP: Duration = Duration::from_secs(10);

/// Pause between sweeps: a tenth of the limit, at least `expiry_interval`, at most
/// [`MAX_STEP`].
pub fn sweep_step(limit: Duration, expiry_interval: Duration) -> Duration {
    (limit / 10).max(expiry_interval).min(MAX_STEP)
}

/// Checks every [`sweep_step`].
pub async fn turn_watchdog_loop(app: AppState, limit: Duration) {
    let mut tick = tokio::time::interval(sweep_step(limit, app.config.timing.expiry_interval));
    loop {
        tick.tick().await;
        if let Err(e) = sweep(&app, limit).await {
            tracing::error!(error = %e, "turn watchdog");
        }
    }
}

async fn sweep(app: &AppState, limit: Duration) -> anyhow::Result<()> {
    let running: HashSet<Uuid> = db::running_turns(&app.db)
        .await?
        .into_iter()
        .filter(|(_, w)| w.is_some_and(|w| app.workers.is_online(w)))
        .map(|(id, _)| id)
        .collect();
    let now = Instant::now();
    let lost: Vec<(Uuid, Duration)> = {
        let mut seen = app.turn_activity.lock().unwrap();
        seen.retain(|id, _| running.contains(id));
        running
            .iter()
            .filter_map(|id| {
                let idle = now - *seen.entry(*id).or_insert(now);
                (idle >= limit).then_some((*id, idle))
            })
            .collect()
    };
    for (id, idle) in lost {
        app.turn_activity.lock().unwrap().remove(&id);
        events::turn_timed_out(app, id, idle).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step() {
        let s = Duration::from_secs;
        // The default 15 min: every 10 s, not every second.
        assert_eq!(sweep_step(s(900), s(1)), s(10));
        assert_eq!(sweep_step(s(30), s(1)), s(3));
        assert_eq!(
            sweep_step(Duration::from_millis(600), Duration::from_millis(50)),
            Duration::from_millis(60)
        );
        assert_eq!(sweep_step(s(5), s(1)), s(1));
    }
}
