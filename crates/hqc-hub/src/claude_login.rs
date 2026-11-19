//! Signing a worker's own Claude Code in from hqc (SPEC 0.185, §6).
//!
//! The tokens stay on the worker: each machine runs `claude auth login` itself, and the hub only
//! relays the sign-in page's address and the code the owner pastes back. The code is a secret
//! used once: it passes through in memory and is never logged, stored or put into an event.

use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use hqc_proto::frame::{
    ClaudeAuthRequest, ClaudeAuthStatus, ClaudeLogin, ClaudeLoginCancel, ClaudeLoginCode,
    ClaudeLoginDone, HubFrame, ResultBody, SecretValue,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    api::own_worker,
    app::AppState,
    auth::CurrentUser,
    db::WorkerRow,
    error::{ApiError, ApiResult},
    worker_ws::RequestError,
};

/// `claude auth status` answers in seconds.
const STATUS_TIMEOUT: Duration = Duration::from_secs(20);
/// The worker waits up to 30 s for the CLI to print the sign-in page.
const START_TIMEOUT: Duration = Duration::from_secs(45);
/// The worker waits up to 60 s for the CLI to take the code.
const CODE_TIMEOUT: Duration = Duration::from_secs(80);

/// The body of `POST /api/workers/{id}/claude/login/{login_id}`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeBody {
    code: SecretValue,
}

/// The owner's worker that can sign its Claude Code in (`capabilities.claude_code.login`).
async fn login_worker(app: &AppState, user: &CurrentUser, id: Uuid) -> ApiResult<WorkerRow> {
    let w = own_worker(app, user, id).await?;
    if w.capabilities["claude_code"]["login"].as_bool() != Some(true) {
        return Err(ApiError::Unprocessable(
            "unsupported",
            "this worker cannot sign its Claude Code in from hqc: update it".into(),
        ));
    }
    Ok(w)
}

fn offline() -> ApiError {
    ApiError::Conflict("worker_offline", "the worker is offline".into())
}

async fn ask(
    app: &AppState,
    worker: &WorkerRow,
    timeout: Duration,
    build: impl FnOnce(String) -> HubFrame,
) -> ApiResult<ResultBody> {
    let conn = app.workers.get(worker.id).ok_or_else(offline)?;
    match conn.request(build, timeout).await {
        Ok(b) => Ok(b),
        Err(RequestError::Offline) => Err(offline()),
        Err(RequestError::Timeout) => Err(ApiError::WorkerTimeout),
        Err(RequestError::Worker(e)) => Err(refused(e.message)),
    }
}

/// The worker's refusal: `login_running: …` and `no_login: …` have codes of their own.
fn refused(message: String) -> ApiError {
    if message.starts_with("login_running") {
        ApiError::Conflict("login_running", message)
    } else if message.starts_with("no_login") {
        ApiError::NotFound
    } else {
        ApiError::Unprocessable("login_failed", message)
    }
}

fn other(frame: &str) -> ApiError {
    anyhow::anyhow!("{frame} answered with another result").into()
}

/// The sign-in page, only Anthropic's own over https: the client opens it on the owner's
/// phone, so a worker must not send the owner anywhere else.
pub(crate) fn anthropic_page(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.contains(['@', ':']) {
        return false;
    }
    ["claude.com", "claude.ai"]
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// `GET /api/workers/{id}/claude`: how the worker's Claude Code is signed in.
pub async fn status(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ClaudeAuthStatus>> {
    let w = login_worker(&app, &user, id).await?;
    match ask(&app, &w, STATUS_TIMEOUT, |request_id| {
        HubFrame::ClaudeAuth(ClaudeAuthRequest { request_id })
    })
    .await?
    {
        ResultBody::ClaudeAuth(r) => Ok(Json(r.claude_auth)),
        _ => Err(other("claude.auth")),
    }
}

/// `POST /api/workers/{id}/claude/login`: starts `claude auth login` on the worker; the answer
/// is the page to sign in on.
pub async fn login(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ClaudeLogin>> {
    let w = login_worker(&app, &user, id).await?;
    let login = match ask(&app, &w, START_TIMEOUT, |request_id| {
        HubFrame::ClaudeLogin(ClaudeAuthRequest { request_id })
    })
    .await?
    {
        ResultBody::ClaudeLogin(r) => r.login,
        _ => return Err(other("claude.login")),
    };
    if !anthropic_page(&login.url) {
        tracing::warn!(worker_id = %w.id, "a worker's sign-in page is not Anthropic's");
        if let Some(conn) = app.workers.get(w.id) {
            conn.send(HubFrame::ClaudeLoginCancel(ClaudeLoginCancel {
                request_id: Uuid::new_v4().to_string(),
                login_id: login.login_id,
            }));
        }
        return Err(ApiError::Unprocessable(
            "login_failed",
            "the worker's sign-in page is not on claude.com or claude.ai".into(),
        ));
    }
    Ok(Json(login))
}

/// `POST /api/workers/{id}/claude/login/{login_id} {code}`: the code the sign-in page gave.
pub async fn code(
    State(app): State<AppState>,
    user: CurrentUser,
    Path((id, login_id)): Path<(Uuid, String)>,
    Json(body): Json<CodeBody>,
) -> ApiResult<Json<ClaudeLoginDone>> {
    let w = login_worker(&app, &user, id).await?;
    let code = body.code.0.trim();
    if code.is_empty() || code.len() > 4096 || login_id.len() > 128 {
        return Err(ApiError::BadRequest("a code of 1..4096 bytes".into()));
    }
    let code = SecretValue(code.to_owned());
    match ask(&app, &w, CODE_TIMEOUT, |request_id| {
        HubFrame::ClaudeLoginCode(ClaudeLoginCode {
            request_id,
            login_id,
            code,
        })
    })
    .await?
    {
        ResultBody::ClaudeLoginDone(r) => Ok(Json(r.login_done)),
        _ => Err(other("claude.login_code")),
    }
}

/// `DELETE /api/workers/{id}/claude/login/{login_id}`: stops the sign-in.
pub async fn cancel(
    State(app): State<AppState>,
    user: CurrentUser,
    Path((id, login_id)): Path<(Uuid, String)>,
) -> ApiResult<StatusCode> {
    let w = login_worker(&app, &user, id).await?;
    let conn = app.workers.get(w.id).ok_or_else(offline)?;
    conn.send(HubFrame::ClaudeLoginCancel(ClaudeLoginCancel {
        request_id: Uuid::new_v4().to_string(),
        login_id,
    }));
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::anthropic_page;

    #[test]
    fn only_anthropics_page_over_https() {
        for ok in [
            "https://claude.com/cai/oauth/authorize?code=true",
            "https://claude.ai/oauth",
            "https://platform.claude.com/x",
        ] {
            assert!(anthropic_page(ok), "{ok}");
        }
        for bad in [
            "http://claude.com/x",
            "https://claude.com.evil.io/x",
            "https://evilclaude.com/x",
            "https://claude.com@evil.io/x",
            "https://claude.com:444/x",
            "javascript:alert(1)",
        ] {
            assert!(!anthropic_page(bad), "{bad}");
        }
    }
}
