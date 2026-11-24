//! Side questions (SPEC §17.5): a question to a session's agent that the runtime answers
//! outside the conversation, with no turn of its own and no trace in the agent's context. The
//! hub stores it as `side.question`, the worker asks the runtime (`session.side_question`) and
//! stores the answer as `side.answer`; the feed shows them as one block, and no input of the
//! agent carries either.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use hqc_proto::{
    api::{InputResponse, SideQuestionRequest},
    frame::{HubFrame, SideExchange},
};
use uuid::Uuid;

use crate::{
    api::{driven_from_hqc, not_system, session_for, Action},
    app::AppState,
    auth::CurrentUser,
    db, delivery,
    error::{ApiError, ApiResult},
    events::{self, user_author},
    worker_ws::Reply,
};

pub const WORKER_OFFLINE: &str = "The worker went offline before the answer";

/// The most earlier side questions that go with a new one as its `history`.
pub const HISTORY_LIMIT: i64 = 10;

/// `POST /api/sessions/{id}/side_question` {text}: stores `side.question` and sends the worker
/// `session.side_question`, starting the agent first when its process does not run. `409 room`
/// (a room has no agent of its own), `409 archived` and `409 locked_vscode` as for input, `409
/// paused` (a paused agent is not started), `409 not_askable` (a runtime other than Claude
/// Code has no side question) and `409 worker_offline`; an empty text is `400`.
pub async fn side_question(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<SideQuestionRequest>,
) -> ApiResult<(StatusCode, Json<InputResponse>)> {
    let s = session_for(&app, &user, id, Action::Write).await?;
    not_system(&s)?;
    if s.room_mode.is_some() {
        return Err(ApiError::Conflict(
            "room",
            "a room has no agent of its own to ask: ask one of its agents".into(),
        ));
    }
    driven_from_hqc(&s)?;
    if s.runtime != "claude_code" {
        return Err(ApiError::Conflict(
            "not_askable",
            "only a Claude Code agent answers a side question".into(),
        ));
    }
    if s.paused_at.is_some() {
        return Err(ApiError::Conflict(
            "paused",
            "the session is paused: resume it to ask its agent".into(),
        ));
    }
    if s.worker_id.and_then(|w| app.workers.get(w)).is_none() {
        return Err(ApiError::Conflict(
            "worker_offline",
            "the worker is offline".into(),
        ));
    }
    if b.text.trim().is_empty() {
        return Err(ApiError::BadRequest("text is required".into()));
    }
    let text = hqc_proto::replace_nul(&b.text).into_owned();
    let history = history(&app, id).await?;
    let row = events::store_hub_event(
        &app,
        &s,
        user_author(user.id),
        "side.question",
        serde_json::json!({ "text": text }),
        None,
        None,
    )
    .await?;
    // The start goes first, the frame after it: the worker runs them in this order.
    delivery::kick_for_side_question(&app, id).await;
    let sent = match s.worker_id.and_then(|w| app.workers.get(w)) {
        Some(conn) => {
            // Registered before it is sent: an `error` to it answers the question. A connection
            // whose close has taken its open questions takes no new one: that close answers only
            // the questions it took.
            let mut st = conn.st.lock().await;
            if conn.side_opened(id, row.event_id) {
                let fid = conn.send(HubFrame::SessionSideQuestion {
                    session_id: id,
                    event_id: row.event_id,
                    question: text,
                    history,
                });
                st.replies.insert(
                    fid,
                    Reply::SideQuestion {
                        session_id: id,
                        event_id: row.event_id,
                    },
                );
                true
            } else {
                false
            }
        }
        None => false,
    };
    if !sent {
        // The worker went away since the check above, or its connection closed: nothing will
        // answer the block.
        answer_error(&app, id, row.event_id, WORKER_OFFLINE).await?;
    }
    Ok((
        StatusCode::CREATED,
        Json(InputResponse {
            event: row.to_json(),
        }),
    ))
}

/// The earlier side questions of the session whose first answer (by `seq`) is a text, after
/// the owner's last `user.message`, oldest first, the last [`HISTORY_LIMIT`] of them.
async fn history(app: &AppState, session_id: Uuid) -> anyhow::Result<Vec<SideExchange>> {
    let mut rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT q.payload ->> 'text', a.text
         FROM events q
         JOIN LATERAL (SELECT payload ->> 'text' AS text FROM events
                       WHERE session_id = q.session_id AND type = 'side.answer'
                         AND payload ->> 'question' = q.event_id::text
                       ORDER BY seq LIMIT 1) a ON a.text IS NOT NULL
         WHERE q.session_id = $1 AND q.type = 'side.question'
           AND q.seq > COALESCE((SELECT max(seq) FROM events
                                 WHERE session_id = $1 AND type = 'user.message'), 0)
         ORDER BY q.seq DESC
         LIMIT $2",
    )
    .bind(session_id)
    .bind(HISTORY_LIMIT)
    .fetch_all(&app.db)
    .await?;
    rows.reverse();
    Ok(rows
        .into_iter()
        .map(|(question, response)| SideExchange { question, response })
        .collect())
}

/// Stores `side.answer {error}` for `question` of `session_id`, unless it has an answer already
/// (the worker's own may have come first).
pub async fn answer_error(
    app: &AppState,
    session_id: Uuid,
    question: Uuid,
    error: &str,
) -> anyhow::Result<()> {
    let Some(s) = db::session(&app.db, session_id).await? else {
        return Ok(());
    };
    if !db::side_answer_wanted(&app.db, session_id, question, db::new_id()).await? {
        return Ok(());
    }
    events::store_hub_event(
        app,
        &s,
        serde_json::json!({ "kind": "agent", "id": session_id }),
        "side.answer",
        serde_json::json!({ "question": question, "error": error }),
        None,
        None,
    )
    .await?;
    Ok(())
}
