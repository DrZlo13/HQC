//! A session forks before a message of its user (SPEC §6 `POST /api/sessions/{id}/fork`):
//! a new session in the same project, folder and worker, whose conversation is the source's
//! up to that message, the message itself left out.
//!
//! - Only a Claude Code session with a workspace and a worker of its owner; the message is a
//!   `user.message` of a person in it (`404` otherwise).
//! - The cut is the source's last `agent.message` before the message, or, for one that waited
//!   for a turn of its own, the last of the turns it waited through ([`waited_place`]), for
//!   another one with `during_text`, the answer the client shows above it (SPEC §6):
//!   `session.start.fork` names the source transcript and those events, newest first, and the
//!   worker forks the conversation at the record of the first one it finds (SPEC §5.3, §8.1).
//!   With none (the message was the first), the new session starts with no conversation and no
//!   fork.
//! - The new session's feed gets the source's events before the message (and the events of
//!   those turns but their inputs, or that answer, last) ([`COPIED`]) under new ids, read
//!   already, and a `system forked_from`. A copy keeps
//!   its origin: the id the runtime's record gave the event and the transcript the record was
//!   written in. A fork of
//!   a fork cuts by them in that transcript when the cut is in the copied history, since the
//!   copies' ids are in no transcript and the source fork's own transcript may not exist (the
//!   CLI writes it at its first message) or its session may be gone. The worker's history is the CLI's:
//!   requests, statuses and usage of the source are not copied.
//! - The copy goes in without its search words; a task writes them as soon as it is in, the
//!   answer not waiting for it ([`db::index_copied`]), and none goes into `search_vocab`, which
//!   has them from the source.

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
    // A message that waited for a turn of its own stands after the turns it waited through
    // (SPEC §6): the fork keeps their events, not the inputs among them. The client shows the
    // answer whose text was being written when a message that did not wait came above it: the
    // fork keeps that answer. Only inputs like the message lie between them. The fork also keeps
    // the events after the message and before `until` that are no input.
    let until = match waited_place(&db::turn_marks(&app.db, s.id).await?, at.seq) {
        Some(p) => Some(p),
        None if at.payload["during_text"] == true => {
            answer_above(app, s, &at).await?.map(|a| a.seq + 1)
        }
        None => None,
    };
    // The records the cut may be at, in the transcript of the newest: a fork's own turns are in
    // its transcript, its copied history in the one it was first written in (a fork that never
    // ran, or that was deleted, has none of it). Only those after the last `/clear` before the
    // message: a fork before the first answer after it starts a new conversation.
    let bound = until.unwrap_or(at.seq);
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
        let n = db::copy_events(&app.db, s.id, id, at.seq, until, COPIED, &runtime).await?;
        // The copy's search words, without the answer waiting for them (SPEC §15): written
        // with the copy, those of a long session kept the user waiting for seconds.
        let pool = app.db.clone();
        tokio::spawn(async move {
            if let Err(e) = db::index_copied(&pool, id, n).await {
                tracing::warn!(session_id = %id, error = %e, "a fork's copy not indexed for search");
            }
        });
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

/// What the fork's agent is told in front of the first input after the cut (SPEC §6): the
/// session it was copied from and its own address. `None` for any other input and for a
/// session with no copied history.
pub async fn note(
    app: &AppState,
    s: &SessionRow,
    input_seq: i64,
) -> anyhow::Result<Option<String>> {
    let Some(from) = db::fork_cut_before_input(&app.db, s.id, input_seq, FORKED_FROM).await? else {
        return Ok(None);
    };
    let own = crate::peers::address(app, s).await?;
    Ok(Some(format!(
        "[hqc fork] {from}. The conversation above was copied from that session; this session \
         is {own}, separate from it from here on."
    )))
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

/// Where the client shows the input at `seq` of `marks` (a session's events in `seq` order)
/// when it waited for a turn of its own (SPEC §6), by the client's rule
/// (`client/src/timeline.ts`, `placeQueued`): `Some(p)`, before the event at `seq` `p`
/// (`i64::MAX`: at the bottom); `None`, at its own place.
pub fn waited_place(marks: &[db::TurnMark], seq: i64) -> Option<i64> {
    let mut next_end = vec![None; marks.len()];
    let mut end = None;
    for (i, m) in marks.iter().enumerate().rev() {
        next_end[i] = end;
        if ends_turn(m) {
            end = Some(i);
        }
    }
    // Indexes of the waiting inputs, oldest first.
    let mut waiting: Vec<usize> = Vec::new();
    // The first `n` waiting inputs, with the joined ones right behind them: whether the input
    // at `seq` is among them.
    let take = |waiting: &mut Vec<usize>, mut n: usize| {
        while n > 0 && n < waiting.len() && joined(&marks[waiting[n]]) {
            n += 1;
        }
        waiting.drain(..n).any(|w| marks[w].seq == seq)
    };
    let head_delivered = |waiting: &[usize]| {
        waiting
            .first()
            .is_some_and(|&w| marks[w].delivery.as_deref() == Some("delivered"))
    };
    let mut open = false;
    for (i, m) in marks.iter().enumerate() {
        if is_input(m) {
            if joined(m) && !waiting.is_empty() {
                waiting.push(i);
                continue;
            }
            let id = m.event_id.to_string();
            let waited = m.delivery.as_deref() == Some("pending")
                || next_end[i].is_some_and(|e| end_of(&marks[e]) != Some(id.as_str()));
            if (open || !waiting.is_empty()) && may_wait(m) && waited {
                waiting.push(i);
                continue;
            }
            if m.seq == seq {
                return None;
            }
            if m.delivery.as_deref() != Some("failed") && !joined(m) {
                open = true;
            }
            continue;
        }
        if ends_turn(m) {
            // The end of a waiting input's own turn: it started without work of the agent before.
            let own = waiting
                .iter()
                .position(|&w| end_of(m) == Some(marks[w].event_id.to_string().as_str()));
            let taken = match own {
                Some(k) => take(&mut waiting, k + 1),
                None if !open
                    && m.kind != "status"
                    && end_of(m).is_none()
                    && head_delivered(&waiting) =>
                {
                    take(&mut waiting, 1)
                }
                None => false,
            };
            if taken {
                return Some(m.seq);
            }
            open = false;
            continue;
        }
        if !open && agent_work(m) {
            if head_delivered(&waiting) && take(&mut waiting, 1) {
                return Some(m.seq);
            }
            open = true;
        }
    }
    waiting
        .iter()
        .any(|&w| marks[w].seq == seq)
        .then_some(i64::MAX)
}

/// The event ends a turn (SPEC §17.1): the final `agent.message`, `status` `idle` or `error`,
/// `system turn_lost`.
fn ends_turn(m: &db::TurnMark) -> bool {
    match m.kind.as_str() {
        "agent.message" => m.is_final,
        "status" => matches!(m.state.as_deref(), Some("idle" | "error")),
        "system" => m.code.as_deref() == Some("turn_lost"),
        _ => false,
    }
}

/// The input whose turn this event ends (`in_reply_to`).
fn end_of(m: &db::TurnMark) -> Option<&str> {
    matches!(m.kind.as_str(), "agent.message" | "status")
        .then_some(m.in_reply_to.as_deref())
        .flatten()
}

/// An input the hub queued for the agent (SPEC §5.2): it has a `delivery`.
fn is_input(m: &db::TurnMark) -> bool {
    matches!(m.kind.as_str(), "user.message" | "peer.message") && m.delivery.is_some()
}

fn joined(m: &db::TurnMark) -> bool {
    m.kind == "user.message" && m.joined
}

/// Come while a turn runs, it waits for a turn of its own: not `joined`, not failed, and not
/// taken when it was written (a mirrored one is delivered at its own `ts`); one with
/// `during_text` too.
fn may_wait(m: &db::TurnMark) -> bool {
    !joined(m)
        && m.delivery.as_deref() != Some("failed")
        && !m.delivered_at.is_some_and(|d| d <= m.ts)
}

/// An event of the agent's work in a turn: `status running`, `system context_compacted`, or
/// what its runtime wrote but an input and the turn's `usage`.
fn agent_work(m: &db::TurnMark) -> bool {
    match m.kind.as_str() {
        "status" => m.state.as_deref() == Some("running"),
        "system" if m.code.as_deref() == Some("context_compacted") => true,
        "peer.message" | "usage" => false,
        _ => m.author_kind.as_deref() == Some("agent"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn at(s: f64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis((s * 1000.0) as i64).unwrap()
    }

    fn id(seq: i64) -> String {
        Uuid::from_u128(seq as u128).to_string()
    }

    fn mark(seq: i64, kind: &str, author: &str) -> db::TurnMark {
        db::TurnMark {
            seq,
            event_id: Uuid::from_u128(seq as u128),
            ts: at(seq as f64),
            kind: kind.into(),
            author_kind: Some(author.into()),
            is_final: false,
            state: None,
            code: None,
            in_reply_to: None,
            joined: false,
            delivery: None,
            delivered_at: None,
        }
    }

    /// The user's message at `seq`, delivered at `delivered`, else pending.
    fn input(seq: i64, delivered: Option<f64>) -> db::TurnMark {
        db::TurnMark {
            delivery: Some(delivered.map_or("pending", |_| "delivered").into()),
            delivered_at: delivered.map(at),
            ..mark(seq, "user.message", "user")
        }
    }

    fn joined(seq: i64, delivered: f64) -> db::TurnMark {
        db::TurnMark {
            joined: true,
            ..input(seq, Some(delivered))
        }
    }

    fn work(seq: i64) -> db::TurnMark {
        mark(seq, "tool.call", "agent")
    }

    fn answer(seq: i64, to: i64) -> db::TurnMark {
        db::TurnMark {
            is_final: true,
            in_reply_to: Some(id(to)),
            ..mark(seq, "agent.message", "agent")
        }
    }

    fn idle(seq: i64, to: i64) -> db::TurnMark {
        db::TurnMark {
            state: Some("idle".into()),
            in_reply_to: Some(id(to)),
            ..mark(seq, "status", "agent")
        }
    }

    fn system(seq: i64, code: &str) -> db::TurnMark {
        db::TurnMark {
            code: Some(code.into()),
            ..mark(seq, "system", "system")
        }
    }

    #[test]
    fn an_input_that_waited_stands_before_the_work_of_its_own_turn() {
        let m = [
            input(1, Some(1.2)),
            work(2),
            input(3, Some(6.1)),
            work(4),
            answer(5, 1),
            idle(6, 1),
            work(7),
            answer(8, 3),
        ];
        assert_eq!(waited_place(&m, 3), Some(7));
        assert_eq!(waited_place(&m, 1), None);
        // Still pending: at the bottom.
        let m = [input(1, Some(1.2)), work(2), input(3, None)];
        assert_eq!(waited_place(&m, 3), Some(i64::MAX));
        // Sent while no turn ran, or mirrored (delivered at its own `ts`): its own place.
        let m = [input(1, Some(1.2)), answer(2, 1), input(3, Some(3.1))];
        assert_eq!(waited_place(&m, 3), None);
        let m = [
            input(1, Some(1.0)),
            work(2),
            input(3, Some(3.0)),
            answer(4, 1),
        ];
        assert_eq!(waited_place(&m, 3), None);
    }

    #[test]
    fn a_joined_input_behind_a_waiting_one_stays_behind_it() {
        let m = [
            input(1, Some(1.2)),
            mark(2, "agent.thinking", "agent"),
            input(3, Some(6.1)),
            joined(4, 7.1),
            answer(5, 1),
            idle(6, 1),
            mark(7, "agent.thinking", "agent"),
            answer(8, 4),
        ];
        assert_eq!(waited_place(&m, 3), Some(7));
        assert_eq!(waited_place(&m, 4), Some(7));
    }

    #[test]
    fn a_compaction_starts_the_turn_and_a_lost_turn_ends_it() {
        let m = [
            input(1, Some(1.2)),
            work(2),
            input(3, Some(7.1)),
            work(4),
            answer(5, 1),
            mark(6, "usage", "agent"),
            idle(7, 1),
            system(8, "context_compacted"),
            mark(9, "usage", "agent"),
            idle(10, 3),
        ];
        assert_eq!(waited_place(&m, 3), Some(8));
        let m = [
            input(1, Some(1.2)),
            work(2),
            input(3, Some(6.1)),
            work(4),
            answer(5, 1),
            idle(6, 1),
            system(7, "turn_lost"),
            input(8, Some(8.1)),
            work(9),
            answer(10, 8),
        ];
        assert_eq!(waited_place(&m, 3), Some(7));
        assert_eq!(waited_place(&m, 8), None);
    }
}
