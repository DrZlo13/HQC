//! Turn watchdog (SPEC §17.3): a session that is `running` while its worker sends nothing
//! for it (no event, `agent.delta` included) for `HQC_TURN_WATCHDOG_S` loses the turn.
//!
//! The clock of a session is [`crate::app::Inner::turn_activity`]: the last event from the
//! worker, or the sweep that first saw the session `running`. A session that is not
//! `running` (waiting for an answer, worker offline, no turn) leaves the map, so its clock
//! starts again when it is `running` once more. The map lives in memory: after a hub
//! restart every open turn gets a full period.

use std::collections::HashSet;
use std::time::Duration;

use tokio::time::Instant;
use uuid::Uuid;

use crate::{app::AppState, db, events};

/// Checks every `expiry_interval`.
pub async fn turn_watchdog_loop(app: AppState, limit: Duration) {
    let mut tick = tokio::time::interval(app.config.timing.expiry_interval);
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
