//! Secrets and ssh keys for a session's agent (SPEC §6, §7, since 0.147). The hub keeps none
//! of them: a value goes from the client's request straight into the worker's `secret.put`
//! and is in no event, row, log line or error; the worker decides whether it takes it
//! (`session_secrets` in its `worker.toml`, a session of its owner, not restricted, an agent
//! that runs).
//!
//! - `POST /api/sessions/{id}/secrets` ([`give`]): the owner gives a secret or a key, on their
//!   own or as the answer to the agent's `hqc_secret` (`request_id`). The answer is what the
//!   worker told the agent's side (the path, the fingerprint), which the client puts into the
//!   message being written; the feed gets `system secret_given` with the name only.
//! - `hqc_secret(name, purpose, kind?, vault?)` ([`tool`]): a PendingRequest of kind [`KIND`]
//!   with an `approval.request` card (`tool: hqc_secret`, `secret {name, kind, vault?}`, the
//!   option `deny`);
//!   the owner's [`give`] closes it with the call's result and `approval.response {option_id:
//!   give}`, "Decline" with an error. Deferred as `hqc_move` is.
//! - The vault: where the worker reads Vaultwarden (`capabilities.vault` of its `hello`), the
//!   agent may name the entry that holds the secret (`vault`, shown on the card) and the owner
//!   gives that entry in place of a value; the worker reads it with `rbw` and the value does
//!   not pass through the hub. Elsewhere the agent's `vault` is dropped.
//! - `hqc_ssh_keygen(name, comment?)` ([`keygen`]): the worker makes an ed25519 key for the
//!   session's workspace and loads it; the agent gets the public half. No card: the agent gets
//!   nothing it could not make with `ssh-keygen`, and the private half stays out of the
//!   workspace.
//! - `GET /api/workspaces/{id}/keys`, `DELETE /api/workspaces/{id}/keys/{name}`: the keys the
//!   worker keeps for a workspace ([`keys`], [`delete_key`]).

use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use hqc_proto::{
    api::{SecretGiven, SecretRequest, WorkspaceKeyList},
    frame::{
        check_vault_entry, shown_line, HubFrame, McpCall, McpError, McpResult, ResultBody,
        SecretKind, SecretOutcome, SecretPut, SecretValue, WorkspaceKeyDelete,
        WorkspaceKeysRequest, MAX_SECRET,
    },
    mcp::error_code,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    api::{self, Action},
    app::AppState,
    auth::CurrentUser,
    db::{self, PendingRow, SessionRow, WorkerRow},
    error::{ApiError, ApiResult},
    events,
    mcp::{args, internal, Answer},
    requests,
    worker_ws::RequestError,
};

/// `kind` of the PendingRequest of an `hqc_secret` (SPEC §3).
pub const KIND: &str = "secret";
/// `system.code` of a secret or key given (SPEC §4).
pub const GIVEN: &str = "secret_given";
/// The option id the owner's give writes into `approval.response` (not among the card's
/// options: "Give" is the `POST`, not an answer).
pub const GIVE: &str = "give";
/// How long the hub waits for the worker's answer to `secret.put`: an ssh-add of a key.
const PUT_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest `purpose` of `hqc_secret`, in characters.
const MAX_PURPOSE: usize = 300;

/// A name as the worker takes it (`ssh_keys` names): letters, digits, `_`, `-`, `.`, 1 to 64,
/// not starting with `.` or `-`.
fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && !name.starts_with(['.', '-'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// The session's worker, if it is the session owner's and takes secrets; why not otherwise.
async fn secrets_worker(app: &AppState, s: &SessionRow) -> Result<WorkerRow, String> {
    let w = match s.worker_id {
        Some(w) => db::worker(&app.db, w).await.map_err(|e| e.to_string())?,
        None => None,
    };
    match w {
        Some(w) if w.owner_id != s.owner_id => {
            Err("the session runs on a worker of another owner: it takes no secrets".into())
        }
        Some(w) if !w.session_secrets => Err(format!(
            "worker {} takes no secrets: session_secrets is off in its worker.toml",
            w.name
        )),
        Some(w) => Ok(w),
        None => Err("the session has no worker".into()),
    }
}

/// The worker reads Vaultwarden through its `rbw` (`vault` and `session_secrets` on in its
/// `worker.toml`, which its `hello` told).
fn reads_vault(w: &WorkerRow) -> bool {
    w.capabilities["vault"] == true
}

/// `secret.put` on the worker; the worker's refusal is `Err(Some(message))`, offline
/// `Err(None)`.
async fn put(
    app: &AppState,
    worker: Uuid,
    p: impl FnOnce(String) -> SecretPut,
) -> Result<SecretOutcome, PutError> {
    let conn = app.workers.get(worker).ok_or(PutError::Offline)?;
    match conn
        .request(|id| HubFrame::SecretPut(p(id)), PUT_TIMEOUT)
        .await
    {
        Ok(ResultBody::Secret(r)) => Ok(r.secret),
        Ok(other) => Err(PutError::Refused(format!(
            "the worker answered secret.put with {}",
            serde_json::to_value(&other)
                .ok()
                .and_then(|v| v.as_object().map(|o| o
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(",")))
                .unwrap_or_default()
        ))),
        Err(RequestError::Offline) => Err(PutError::Offline),
        Err(RequestError::Timeout) => Err(PutError::Timeout),
        Err(RequestError::Worker(e)) => Err(PutError::Refused(e.message)),
    }
}

enum PutError {
    Offline,
    Timeout,
    Refused(String),
}

impl From<PutError> for ApiError {
    fn from(e: PutError) -> Self {
        match e {
            PutError::Offline => {
                ApiError::Conflict("worker_offline", "the worker is offline".into())
            }
            PutError::Timeout => ApiError::WorkerTimeout,
            PutError::Refused(m) => ApiError::Unprocessable("secret_refused", m),
        }
    }
}

impl From<PutError> for McpError {
    fn from(e: PutError) -> Self {
        match e {
            PutError::Offline => McpError::new(error_code::UNAVAILABLE, "the worker is offline"),
            PutError::Timeout => {
                McpError::new(error_code::UNAVAILABLE, "the worker did not answer in time")
            }
            PutError::Refused(m) => McpError::new(error_code::UNAVAILABLE, m),
        }
    }
}

/// What the feed says, without the value.
fn given_text(kind: SecretKind, name: &str, vault: Option<&str>, o: &SecretOutcome) -> String {
    let fp = o.fingerprint.as_deref().unwrap_or("");
    let from = vault
        .map(|e| format!(" from Vaultwarden entry {e}"))
        .unwrap_or_default();
    match kind {
        SecretKind::Secret => format!("Secret {name} given{from}"),
        SecretKind::SshKey if o.remembered => {
            format!("ssh key {name} ({fp}) given{from}, remembered for this workspace")
        }
        SecretKind::SshKey => format!("ssh key {name} ({fp}) given{from}"),
        SecretKind::SshKeygen => format!("ssh key {name} ({fp}) made for this workspace"),
    }
}

/// `POST /api/sessions/{id}/secrets` (SPEC §6): the session owner's, on a worker of theirs that
/// takes secrets; exactly one of `value` and `vault` (a Vaultwarden entry the worker reads
/// itself, only on a worker that reads Vaultwarden, else `409 unavailable`); `request_id`
/// answers that session's open `hqc_secret` with the same name and kind. `200` with what the worker told the agent's side; `409 worker_offline`, `422
/// secret_refused` with the worker's reason (no running agent, a bad key, a wrong
/// passphrase), `504 worker_timeout`. A refused or failed give leaves the request open.
pub async fn give(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<SecretRequest>,
) -> ApiResult<Json<SecretGiven>> {
    let s = api::session_for(&app, &user, id, Action::Write).await?;
    api::not_system(&s)?;
    if s.archived {
        return Err(ApiError::Conflict(
            "archived",
            "the session is archived".into(),
        ));
    }
    if b.kind == SecretKind::SshKeygen {
        return Err(ApiError::BadRequest(
            "kind is secret or ssh_key; the agent makes a key with hqc_ssh_keygen".into(),
        ));
    }
    if !valid_name(&b.name) {
        return Err(ApiError::BadRequest(
            "name is 1 to 64 letters, digits, '_', '-' and '.', not starting with '.' or '-'"
                .into(),
        ));
    }
    match (&b.value, &b.vault) {
        (Some(_), Some(_)) | (None, None) => {
            return Err(ApiError::BadRequest(
                "give exactly one of value and vault".into(),
            ))
        }
        (Some(v), None) if v.0.is_empty() || v.0.len() > MAX_SECRET => {
            return Err(ApiError::BadRequest(format!(
                "value is 1 byte to {} KiB",
                MAX_SECRET / 1024
            )))
        }
        (None, Some(e)) => check_vault_entry(e).map_err(ApiError::BadRequest)?,
        _ => {}
    }
    if b.kind == SecretKind::Secret && (b.remember || b.passphrase.is_some()) {
        return Err(ApiError::BadRequest(
            "remember and passphrase are for an ssh key".into(),
        ));
    }
    let worker = secrets_worker(&app, &s)
        .await
        .map_err(|m| ApiError::Conflict("unavailable", m))?;
    if b.vault.is_some() && !reads_vault(&worker) {
        return Err(ApiError::Conflict(
            "unavailable",
            format!(
                "worker {} reads no Vaultwarden: vault is off in its worker.toml",
                worker.name
            ),
        ));
    }
    let request = match &b.request_id {
        None => None,
        Some(rid) => {
            let p = db::find_open_request(&app.db, s.id, rid)
                .await?
                .filter(|p| p.kind == KIND)
                .ok_or_else(|| {
                    ApiError::Conflict("request_closed", "no open request for a secret".into())
                })?;
            let want = &p.payload["secret"];
            let kind = serde_json::to_value(b.kind).unwrap_or_default();
            if want["name"] != b.name.as_str() || want["kind"] != kind {
                return Err(ApiError::BadRequest(format!(
                    "the agent asked for {} {}",
                    want["kind"].as_str().unwrap_or("secret"),
                    want["name"].as_str().unwrap_or("")
                )));
            }
            Some(p)
        }
    };
    let SecretRequest {
        kind,
        name,
        value,
        vault,
        passphrase,
        remember,
        ..
    } = b;
    let outcome = put(&app, worker.id, |request_id| SecretPut {
        request_id,
        session_id: s.id,
        kind,
        name: name.clone(),
        value,
        vault: vault.clone(),
        passphrase,
        remember,
        comment: None,
    })
    .await?;
    tracing::info!(session_id = %s.id, ?kind, %name, ?vault, remembered = outcome.remembered, "secret given");
    events::system(
        &app,
        &s,
        GIVEN,
        &given_text(kind, &name, vault.as_deref(), &outcome),
    )
    .await?;
    if let Some(p) = request {
        answered(&app, &user, &s, p, &outcome).await?;
    }
    Ok(Json(outcome))
}

/// The owner gave what request `p` asked for: the request closes with the call's result, the
/// card with `approval.response {option_id: give}`.
async fn answered(
    app: &AppState,
    user: &CurrentUser,
    s: &SessionRow,
    p: PendingRow,
    o: &SecretOutcome,
) -> ApiResult<()> {
    let frame = json!({"type": "mcp.result", "payload": McpResult::ok(&p.request_id, json!({
        "name": p.payload["secret"]["name"],
        "note": o.note,
        "path": o.path,
        "fingerprint": o.fingerprint,
        "remembered": o.remembered,
    }))});
    if !db::close_request(&app.db, p.id, Some(&frame)).await? {
        // Closed meanwhile (the runtime gave up): the agent has the secret, the card is done.
        return Ok(());
    }
    events::store_hub_event(
        app,
        s,
        events::user_author(user.id),
        "approval.response",
        json!({"request_id": p.request_id, "option_id": GIVE, "by": user.id.to_string()}),
        None,
        None,
    )
    .await?;
    requests::deliver_result(
        app,
        &PendingRow {
            result: Some(frame),
            ..p
        },
    )
    .await;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretArgs {
    name: String,
    purpose: String,
    #[serde(default)]
    kind: Option<SecretKind>,
    #[serde(default)]
    vault: Option<String>,
}

/// `hqc_secret(name, purpose, kind?, vault?)` (SPEC §7): the owner is asked with a card in the
/// session's feed; the call is answered when they give it ([`give`]) or decline. Once per
/// `request_id`: a repeat finds its request, waiting or answered. `vault`, the entry the agent
/// proposes, goes on the card only where the worker reads Vaultwarden, and is dropped
/// elsewhere.
pub async fn tool(app: &AppState, caller: &SessionRow, c: &McpCall) -> Result<Answer, McpError> {
    let a: SecretArgs = args(c.args.clone())?;
    let kind = a.kind.unwrap_or(SecretKind::Secret);
    if kind == SecretKind::SshKeygen {
        return Err(McpError::new(
            error_code::INVALID_ARGS,
            "kind is secret or ssh_key; make a key with hqc_ssh_keygen",
        ));
    }
    if !valid_name(&a.name) {
        return Err(McpError::new(
            error_code::INVALID_ARGS,
            "name is 1 to 64 letters, digits, '_', '-' and '.', not starting with '.' or '-'",
        ));
    }
    if let Some(e) = &a.vault {
        check_vault_entry(e).map_err(|m| McpError::new(error_code::INVALID_ARGS, m))?;
    }
    let purpose = a.purpose.trim();
    if purpose.is_empty() || purpose.chars().count() > MAX_PURPOSE || !shown_line(purpose) {
        return Err(McpError::new(
            error_code::INVALID_ARGS,
            format!("purpose is one line of 1 to {MAX_PURPOSE} characters"),
        ));
    }
    if let Some(p) = db::request_by_id(&app.db, caller.id, KIND, &c.request_id)
        .await
        .map_err(internal)?
    {
        return match p.result {
            None => Ok(Answer::Later),
            Some(frame) => {
                crate::peers::result_acked(app, p.id)
                    .await
                    .map_err(internal)?;
                let r: McpResult =
                    serde_json::from_value(frame["payload"].clone()).map_err(internal)?;
                r.outcome().map(Answer::Now)
            }
        };
    }
    let worker = secrets_worker(app, caller)
        .await
        .map_err(|m| McpError::new(error_code::UNAVAILABLE, m))?;
    let mut secret = json!({"name": a.name, "kind": kind});
    if let Some(e) = a.vault.filter(|_| reads_vault(&worker)) {
        secret["vault"] = json!(e);
    }
    let payload = json!({
        "request_id": c.request_id,
        "tool": hqc_proto::mcp::SECRET,
        "description": purpose,
        "secret": secret,
        "options": [
            {"id": hqc_proto::frame::worker_approval::DENY, "label": "Decline", "canonical": "deny"},
        ],
    });
    if !db::open_request(
        &app.db,
        KIND,
        caller.id,
        &c.request_id,
        &payload,
        None,
        caller.worker_id,
    )
    .await
    .map_err(internal)?
    {
        return Ok(Answer::Later);
    }
    tracing::info!(session_id = %caller.id, request_id = %c.request_id, name = %a.name, "hqc_secret asks the owner");
    events::store_hub_event(
        app,
        caller,
        json!({"kind": "system"}),
        "approval.request",
        payload,
        None,
        None,
    )
    .await
    .map_err(internal)?;
    Ok(Answer::Later)
}

/// The `mcp.result` of an `hqc_secret` the owner declined.
pub fn declined(request_id: &str) -> Value {
    json!({"type": "mcp.result", "payload": McpResult::err(
        request_id,
        error_code::UNAVAILABLE,
        "the user declined to give the secret; go on without it or ask them in the chat",
    )})
}

/// The `mcp.result` of an `hqc_secret` whose request the hub cancelled.
pub fn cancelled(request_id: &str, reason: hqc_proto::event::CancelReason) -> Value {
    let why = serde_json::to_value(reason).unwrap_or_default();
    json!({"type": "mcp.result", "payload": McpResult::err(
        request_id,
        error_code::UNAVAILABLE,
        format!(
            "the request for the secret was cancelled ({})",
            why.as_str().unwrap_or("cancelled")
        ),
    )})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeygenArgs {
    name: String,
    #[serde(default)]
    comment: Option<String>,
}

/// `hqc_ssh_keygen(name, comment?)` (SPEC §7): `{name, fingerprint, public_key, note}`, off the
/// connection's loop (the worker answers `secret.put` on the connection the call came by).
pub async fn keygen(app: &AppState, caller: &SessionRow, c: &McpCall) -> Result<Answer, McpError> {
    let a: KeygenArgs = args(c.args.clone())?;
    if !valid_name(&a.name) {
        return Err(McpError::new(
            error_code::INVALID_ARGS,
            "name is 1 to 64 letters, digits, '_', '-' and '.', not starting with '.' or '-'",
        ));
    }
    if let Some(cm) = &a.comment {
        if cm.chars().count() > 100 || !shown_line(cm) {
            return Err(McpError::new(
                error_code::INVALID_ARGS,
                "comment is one line of at most 100 characters",
            ));
        }
    }
    if caller.workspace_id.is_none() {
        return Err(McpError::new(
            error_code::UNAVAILABLE,
            "this session has no workspace to keep a key for",
        ));
    }
    let worker = secrets_worker(app, caller)
        .await
        .map_err(|m| McpError::new(error_code::UNAVAILABLE, m))?;
    let (app, caller) = (app.clone(), caller.clone());
    Ok(Answer::Wait(Box::pin(async move {
        let name = a.name.clone();
        let o = put(&app, worker.id, |request_id| SecretPut {
            request_id,
            session_id: caller.id,
            kind: SecretKind::SshKeygen,
            name: a.name,
            value: None::<SecretValue>,
            vault: None,
            passphrase: None,
            remember: true,
            comment: a.comment,
        })
        .await?;
        tracing::info!(session_id = %caller.id, %name, "ssh key made for the workspace");
        events::system(
            &app,
            &caller,
            GIVEN,
            &given_text(SecretKind::SshKeygen, &name, None, &o),
        )
        .await
        .map_err(internal)?;
        Ok(Some(json!({
            "name": name,
            "fingerprint": o.fingerprint,
            "public_key": o.public_key,
            "note": o.note,
        })))
    })))
}

/// The workspace's worker, online; the hub asks it for the keys it keeps.
async fn ask_keys(
    app: &AppState,
    ws: &db::WorkspaceRow,
    build: impl FnOnce(String) -> HubFrame,
) -> ApiResult<ResultBody> {
    let conn = app
        .workers
        .get(ws.worker_id)
        .ok_or_else(|| ApiError::Conflict("worker_offline", "the worker is offline".into()))?;
    match conn.request(build, PUT_TIMEOUT).await {
        Ok(b) => Ok(b),
        Err(RequestError::Offline) => Err(ApiError::Conflict(
            "worker_offline",
            "the worker is offline".into(),
        )),
        Err(RequestError::Timeout) => Err(ApiError::WorkerTimeout),
        Err(RequestError::Worker(e)) => Err(ApiError::Unprocessable("secret_refused", e.message)),
    }
}

/// `GET /api/workspaces/{id}/keys` (SPEC §6): the names and fingerprints of the ssh keys the
/// worker keeps for the owner's workspace.
pub async fn keys(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<WorkspaceKeyList>> {
    let ws = api::workspace_for(&app, &user, id).await?;
    let path = ws.path.clone();
    match ask_keys(&app, &ws, |request_id| {
        HubFrame::WorkspaceKeys(WorkspaceKeysRequest { request_id, path })
    })
    .await?
    {
        ResultBody::WorkspaceKeys(r) => Ok(Json(r)),
        _ => Err(anyhow::anyhow!("workspace.keys answered with another result").into()),
    }
}

/// `DELETE /api/workspaces/{id}/keys/{name}`: `204`, `404` for a key the workspace has not.
pub async fn delete_key(
    State(app): State<AppState>,
    user: CurrentUser,
    Path((id, name)): Path<(Uuid, String)>,
) -> ApiResult<StatusCode> {
    let ws = api::workspace_for(&app, &user, id).await?;
    if !valid_name(&name) {
        return Err(ApiError::NotFound);
    }
    let path = ws.path.clone();
    match ask_keys(&app, &ws, |request_id| {
        HubFrame::WorkspaceKeyDelete(WorkspaceKeyDelete {
            request_id,
            path,
            name,
        })
    })
    .await?
    {
        ResultBody::WorkspaceKeyDeleted(r) if r.key_deleted => Ok(StatusCode::NO_CONTENT),
        ResultBody::WorkspaceKeyDeleted(_) => Err(ApiError::NotFound),
        _ => Err(anyhow::anyhow!("workspace.key_delete answered with another result").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_shown_lines() {
        for ok in ["WIFI_PASS", "pi-lab", "a.b", "x"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", ".x", "-x", "a/b", "a b", &"x".repeat(65)] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert!(shown_line("the Wi-Fi password of the camera"));
        for bad in ["a\nb", "a\u{202E}b", "a\u{2028}b", "a\u{0007}"] {
            assert!(!shown_line(bad), "{bad:?}");
        }
    }
}
