//! Model providers of an owner (SPEC §3 Provider, §6, since 0.144): an Anthropic-compatible
//! endpoint with one model, picked per session. The hub keeps the descriptions in one place;
//! a worker runs one only if its `worker.toml` allows the name with the same `base_url`, and
//! keeps the key (SPEC §14). `GET/POST /api/providers`, `PATCH/DELETE /api/providers/{id}`.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use hqc_proto::api::{ModelProvider, ProviderList, ProviderRequest};
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::CurrentUser,
    db::{self, ProviderFields, ProviderRow, SessionRow, WorkerRow},
    error::{ApiError, ApiResult},
};

/// Longest label, name, model and `base_url`, in characters.
const MAX_LABEL: usize = 80;
const MAX_NAME: usize = 64;
const MAX_MODEL: usize = 200;
const MAX_URL: usize = 500;

fn bad(m: impl Into<String>) -> ApiError {
    ApiError::BadRequest(m.into())
}

/// The fields as the hub stores them, or why they are refused. The worker checks again what
/// it runs (`Config::provider`).
fn fields(b: &ProviderRequest) -> ApiResult<ProviderFields<'_>> {
    let name = b.name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(bad(format!(
            "name is 1 to {MAX_NAME} letters, digits, '-' and '_', as in [providers.<name>]"
        )));
    }
    let label = b.label.trim();
    if label.is_empty() || label.chars().count() > MAX_LABEL || label.contains('\n') {
        return Err(bad(format!(
            "label is one line of 1 to {MAX_LABEL} characters"
        )));
    }
    let base_url = b.base_url.trim().trim_end_matches('/');
    if !(base_url.starts_with("http://") || base_url.starts_with("https://"))
        || base_url.chars().count() > MAX_URL
        || base_url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(bad("base_url must be an http:// or https:// URL"));
    }
    let model = b.model.trim();
    if model.is_empty()
        || model.chars().count() > MAX_MODEL
        || model.starts_with('-')
        || model.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(bad("model is a model name without spaces"));
    }
    let context_tokens = match b.context_tokens {
        Some(n) if !(1_000..=10_000_000).contains(&n) => {
            return Err(bad("context_tokens is 1000 to 10000000"))
        }
        Some(n) => Some(n as i32),
        None => None,
    };
    Ok(ProviderFields {
        name,
        label,
        base_url,
        model,
        context_tokens,
    })
}

/// The owner's provider; another owner's looks like a missing one.
async fn own(app: &AppState, owner: Uuid, id: Uuid) -> ApiResult<ProviderRow> {
    match db::provider(&app.db, id).await? {
        Some(p) if p.owner_id == owner => Ok(p),
        _ => Err(ApiError::NotFound),
    }
}

fn view(p: ProviderRow, workers: &[WorkerRow]) -> ModelProvider {
    ModelProvider {
        workers: workers
            .iter()
            .filter(|w| w.allows_provider(&p.name))
            .map(|w| w.id)
            .collect(),
        context_tokens: p.context_tokens.and_then(|n| u32::try_from(n).ok()),
        id: p.id,
        name: p.name,
        label: p.label,
        base_url: p.base_url,
        model: p.model,
        created_at: p.created_at,
        updated_at: p.updated_at,
    }
}

async fn reply(app: &AppState, owner: Uuid, p: ProviderRow) -> ApiResult<Json<ModelProvider>> {
    let workers = db::workers_of(&app.db, owner).await?;
    Ok(Json(view(p, &workers)))
}

pub async fn list(State(app): State<AppState>, user: CurrentUser) -> ApiResult<Json<ProviderList>> {
    let workers = db::workers_of(&app.db, user.id).await?;
    let providers = db::providers_of(&app.db, user.id)
        .await?
        .into_iter()
        .map(|p| view(p, &workers))
        .collect();
    Ok(Json(ProviderList { providers }))
}

pub async fn create(
    State(app): State<AppState>,
    user: CurrentUser,
    Json(b): Json<ProviderRequest>,
) -> ApiResult<(StatusCode, Json<ModelProvider>)> {
    let f = fields(&b)?;
    let Some(p) = db::insert_provider(&app.db, user.id, &f).await? else {
        return Err(ApiError::Conflict(
            "provider_exists",
            format!("you have a provider named {:?}", f.name),
        ));
    };
    Ok((StatusCode::CREATED, reply(&app, user.id, p).await?))
}

/// Replaces every field. Its sessions get the new description with their next start; a
/// running agent keeps the endpoint it started on until then.
pub async fn update(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<ProviderRequest>,
) -> ApiResult<Json<ModelProvider>> {
    own(&app, user.id, id).await?;
    let f = fields(&b)?;
    let Some(p) = db::update_provider(&app.db, id, &f).await? else {
        return Err(ApiError::Conflict(
            "provider_exists",
            format!("you have a provider named {:?}", f.name),
        ));
    };
    for s in db::sessions_on_provider(&app.db, id).await? {
        app.session_changed(s);
    }
    reply(&app, user.id, p).await
}

pub async fn delete(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    own(&app, user.id, id).await?;
    if !db::delete_provider(&app.db, id).await? {
        return Err(ApiError::Conflict(
            "provider_in_use",
            "sessions run on this provider: switch them to another first".into(),
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The body of `POST /api/providers/probe` (SPEC 0.236).
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeBody {
    base_url: String,
    /// The provider's name as typed: a worker that allows it is asked first.
    #[serde(default)]
    name: Option<String>,
}

/// `POST /api/providers/probe {base_url, name?}` (SPEC 0.236): the models the endpoint lists,
/// as one of the owner's online workers reads them (the endpoint is usually in the owner's own
/// network, which the hub does not reach). A worker that allows `name` is asked first, then the
/// others, until one answers with models. `{models[{id, context_tokens?}], error?, worker_id?}`.
pub async fn probe(
    State(app): State<AppState>,
    user: CurrentUser,
    Json(b): Json<ProbeBody>,
) -> ApiResult<Json<serde_json::Value>> {
    use hqc_proto::frame::{HubFrame, ProviderProbeRequest, ResultBody};
    let base_url = b.base_url.trim().trim_end_matches('/').to_string();
    if !(base_url.starts_with("http://") || base_url.starts_with("https://"))
        || base_url.chars().count() > MAX_URL
        || base_url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(bad("base_url must be an http:// or https:// URL"));
    }
    let mut workers: Vec<WorkerRow> = db::workers_of(&app.db, user.id)
        .await?
        .into_iter()
        .filter(|w| w.revoked_at.is_none() && app.workers.get(w.id).is_some())
        .collect();
    if workers.is_empty() {
        return Err(ApiError::Conflict(
            "worker_offline",
            "no worker of yours is online to read the endpoint".into(),
        ));
    }
    let name = b.name.as_deref().unwrap_or_default();
    workers.sort_by_key(|w| !w.allows_provider(name));
    let mut last = json_error("no worker answered");
    for w in workers.iter().take(3) {
        let Some(conn) = app.workers.get(w.id) else {
            continue;
        };
        let url = base_url.clone();
        let answer = conn
            .request(
                |request_id| {
                    HubFrame::ProviderProbe(ProviderProbeRequest {
                        request_id,
                        base_url: url,
                    })
                },
                std::time::Duration::from_secs(30),
            )
            .await;
        match answer {
            Ok(ResultBody::ProviderProbe(r)) if !r.provider_probe.models.is_empty() => {
                let mut v = serde_json::to_value(&r.provider_probe).unwrap_or_default();
                v["worker_id"] = serde_json::json!(w.id);
                return Ok(Json(v));
            }
            Ok(ResultBody::ProviderProbe(r)) => {
                last = json_error(&format!(
                    "{}: {}",
                    w.name,
                    r.provider_probe.error.as_deref().unwrap_or("no models")
                ));
            }
            // An older worker does not know the frame.
            _ => last = json_error(&format!("{} cannot read endpoints: update it", w.name)),
        }
    }
    Ok(Json(last))
}

fn json_error(m: &str) -> serde_json::Value {
    serde_json::json!({"models": [], "error": m})
}

/// Provider `id` may run sessions of `owner` on `worker`: it is the owner's, and the worker's
/// last `hello` allowed it (SPEC §14). The worker checks its `base_url` itself at the start.
pub async fn check(app: &AppState, owner: Uuid, worker: &WorkerRow, id: Uuid) -> ApiResult<()> {
    let p = match db::provider(&app.db, id).await? {
        Some(p) if p.owner_id == owner => p,
        _ => return Err(bad("unknown provider")),
    };
    if !worker.allows_provider(&p.name) {
        return Err(ApiError::Unprocessable(
            "provider_not_allowed",
            format!(
                "worker {} does not allow the provider {:?}: add [providers.{}] to its worker.toml",
                worker.name, p.name, p.name
            ),
        ));
    }
    Ok(())
}

/// [`check`] for a session's owner and worker.
pub async fn check_for_session(app: &AppState, s: &SessionRow, id: Uuid) -> ApiResult<()> {
    let worker = match s.worker_id {
        Some(w) => db::worker(&app.db, w).await?,
        None => None,
    };
    let Some(worker) = worker else {
        return Err(bad("the session has no worker to run a provider"));
    };
    check(app, s.owner_id, &worker, id).await
}
