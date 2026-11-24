//! Signing a remote MCP server in with OAuth 2.1 (SPEC 0.235, T65), as MCP's authorization spec
//! has it.
//!
//! - Discovery: the server's `401` names its protected-resource metadata (RFC 9728), else the
//!   well-known paths are tried; that names the authorization server, whose metadata (RFC 8414,
//!   or OpenID's) gives the endpoints. Every URL is https (http only to loopback) and goes
//!   through the same address checks as the server's.
//! - Dynamic client registration (RFC 7591) when the owner gave no client id and the server
//!   offers it.
//! - `POST /api/mcp/servers/{id}/sign_in` answers the page to open; the state (bound to the owner,
//!   ten minutes) and the PKCE verifier stay in the hub's memory. The authorization server sends
//!   the browser back to `GET /api/mcp/oauth/callback`, behind the hub's sign-in like the rest of
//!   `/api/*`, which exchanges the code.
//! - The tokens and a client secret are sealed under `HQC_MCP_KEY` (associated data: the server's
//!   id and `oauth`). The access token is refreshed a minute before it expires, and once more when
//!   the server refuses it; a failed refresh asks the owner to sign in again.

use std::time::{Duration, Instant};

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    Json,
};
use base64::Engine;
use chrono::Utc;
use hqc_proto::api::McpSignIn;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{check_url, client_for, own, seal_bytes, unseal_bytes, Row, UpErr, COLS};
use crate::{
    app::AppState,
    auth::CurrentUser,
    error::{ApiError, ApiResult},
};

/// A sign-in started and not finished: its state is the key.
pub(super) struct Pending {
    owner: Uuid,
    server: Uuid,
    verifier: String,
    redirect_uri: String,
    at: Instant,
    /// What this sign-in discovered and registered: stored only when it succeeds, so that a
    /// sign-in started and dropped does not change where tokens go.
    meta: Meta,
    client_id: String,
    registered_secret: Option<String>,
}

const PENDING_FOR: Duration = Duration::from_secs(600);
const CALLBACK: &str = "/api/mcp/oauth/callback";

/// What is sealed in `oauth_sealed`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Secrets {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<i64>,
}

/// The authorization server as discovered.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Meta {
    /// The authorization server's issuer, as its metadata names it and as asked for.
    #[serde(default)]
    issuer: String,
    /// It sends `iss` back with the code (RFC 9207): the callback then requires it.
    #[serde(default)]
    iss_supported: bool,
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    registration_endpoint: Option<String>,
    #[serde(default)]
    token_auth_methods: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
}

fn aad(id: Uuid) -> Vec<u8> {
    let mut a = id.as_bytes().to_vec();
    a.extend_from_slice(b"oauth");
    a
}

fn key(app: &AppState) -> Result<&[u8; 32], UpErr> {
    app.config
        .mcp
        .key
        .as_ref()
        .ok_or_else(|| UpErr::Unavailable("HQC_MCP_KEY is not set on the hub".into()))
}

fn secrets_of(app: &AppState, r: &Row) -> Result<Secrets, UpErr> {
    let Some(sealed) = &r.oauth_sealed else {
        return Ok(Secrets::default());
    };
    let plain = unseal_bytes(key(app)?, &aad(r.id), sealed).ok_or_else(|| {
        UpErr::SignIn("its sign-in cannot be read with this HQC_MCP_KEY: sign in again".into())
    })?;
    serde_json::from_slice(&plain).map_err(|_| UpErr::SignIn("sign in again".into()))
}

fn seal_secrets(app: &AppState, id: Uuid, s: &Secrets) -> Result<Vec<u8>, UpErr> {
    let plain = serde_json::to_vec(s).expect("secrets serialize");
    Ok(seal_bytes(key(app)?, &aad(id), &plain))
}

/// The hub holds a token for the server (whether it still works is the next call's business).
pub(super) fn signed_in(r: &Row) -> bool {
    r.oauth_sealed.is_some() && r.oauth_meta.is_some() && r.status != "sign_in"
}

async fn row(app: &AppState, id: Uuid) -> Result<Row, UpErr> {
    sqlx::query_as(&format!("SELECT {COLS} FROM mcp_servers WHERE id = $1"))
        .bind(id)
        .fetch_optional(&app.db)
        .await
        .map_err(|e| UpErr::Unavailable(format!("database: {e}")))?
        .ok_or_else(|| UpErr::Unavailable("the server is gone".into()))
}

/// The access token to send, refreshed when due or with `force`. One refresh per server at a
/// time: a second caller waits and takes the new token.
pub(super) async fn bearer(app: &AppState, id: Uuid, force: bool) -> Result<String, UpErr> {
    let lock = {
        let mut g = app.mcp_upstream.refreshing.lock().unwrap();
        g.entry(id).or_default().clone()
    };
    let _held = lock.lock().await;
    let r = row(app, id).await?;
    let mut s = secrets_of(app, &r)?;
    let now = Utc::now().timestamp();
    let fresh = s.expires_at.is_none_or(|e| e > now + 60);
    // A token refreshed in the last seconds is the one a forced refresh would give: another
    // caller refused with the old one got there first.
    let just_refreshed = app
        .mcp_upstream
        .refreshed
        .lock()
        .unwrap()
        .get(&id)
        .is_some_and(|t| t.elapsed() < Duration::from_secs(10));
    if let Some(t) = &s.access_token {
        if fresh && (!force || just_refreshed) {
            return Ok(t.clone());
        }
    }
    let Some(refresh) = s.refresh_token.clone() else {
        return Err(UpErr::SignIn(
            "not signed in: sign in in Settings → MCP servers".into(),
        ));
    };
    let meta: Meta = r
        .oauth_meta
        .clone()
        .and_then(|m| serde_json::from_value(m).ok())
        .ok_or_else(|| UpErr::SignIn("sign in again".into()))?;
    let client_id = r
        .oauth_client_id
        .clone()
        .ok_or_else(|| UpErr::SignIn("sign in again".into()))?;
    let form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh),
        ("resource", r.url.clone()),
    ];
    let t = match token_request(app, &meta, &client_id, s.client_secret.as_deref(), form).await {
        Ok(t) => t,
        Err(e) => {
            let _ = sqlx::query(
                "UPDATE mcp_servers SET status = 'sign_in', last_error = $2 WHERE id = $1",
            )
            .bind(id)
            .bind("the sign-in expired: sign in again in Settings → MCP servers")
            .execute(&app.db)
            .await;
            return Err(e);
        }
    };
    s.access_token = Some(t.access_token.clone());
    if t.refresh_token.is_some() {
        s.refresh_token = t.refresh_token;
    }
    s.expires_at = t.expires_in.map(|e| now + e);
    store(app, id, &s).await?;
    app.mcp_upstream
        .refreshed
        .lock()
        .unwrap()
        .insert(id, Instant::now());
    Ok(t.access_token)
}

async fn store(app: &AppState, id: Uuid, s: &Secrets) -> Result<(), UpErr> {
    let sealed = seal_secrets(app, id, s)?;
    sqlx::query("UPDATE mcp_servers SET oauth_sealed = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(sealed)
        .execute(&app.db)
        .await
        .map_err(|e| UpErr::Unavailable(format!("database: {e}")))?;
    Ok(())
}

#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

fn form_body(pairs: &[(&str, String)]) -> String {
    let mut u = reqwest::Url::parse("http://x/").expect("a url");
    u.query_pairs_mut()
        .extend_pairs(pairs.iter().map(|(k, v)| (*k, v.as_str())));
    u.query().unwrap_or_default().to_string()
}

/// A token request; the client authenticates in the body or with basic auth, as the server takes.
async fn token_request(
    app: &AppState,
    meta: &Meta,
    client_id: &str,
    secret: Option<&str>,
    mut form: Vec<(&str, String)>,
) -> Result<Tokens, UpErr> {
    let http = client_for(app, &meta.token_endpoint, Duration::from_secs(30)).await?;
    let basic = secret.is_some()
        && meta
            .token_auth_methods
            .iter()
            .any(|m| m == "client_secret_basic")
        && !meta
            .token_auth_methods
            .iter()
            .any(|m| m == "client_secret_post");
    let mut req = http
        .post(&meta.token_endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json");
    if basic {
        req = req.basic_auth(client_id, secret);
    } else {
        form.push(("client_id", client_id.to_string()));
        if let Some(s) = secret {
            form.push(("client_secret", s.to_string()));
        }
    }
    let resp = req
        .body(form_body(&form))
        .send()
        .await
        .map_err(|e| UpErr::Unavailable(super::short(&e)))?;
    let status = resp.status();
    let body: Value = capped_json(resp)
        .await
        .ok_or_else(|| UpErr::Unavailable(format!("the token endpoint answered {status}")))?;
    if !status.is_success() {
        let err = word(body.get("error").and_then(Value::as_str).unwrap_or("error"));
        return Err(UpErr::SignIn(format!(
            "the authorization server refused: {err}"
        )));
    }
    serde_json::from_value(body)
        .map_err(|_| UpErr::Unavailable("the token endpoint gave no access token".into()))
}

async fn get_json(app: &AppState, url: &str) -> Option<Value> {
    check_url(url).ok()?;
    let http = client_for(app, url, Duration::from_secs(20)).await.ok()?;
    let r = http
        .get(url)
        .header("accept", "application/json")
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        return None;
    }
    capped_json(r).await
}

/// The JSON body of an answer from a server that may not be honest, at most 1 MiB.
async fn capped_json(mut r: reqwest::Response) -> Option<Value> {
    const MAX: usize = 1024 * 1024;
    if r.content_length().is_some_and(|n| n > MAX as u64) {
        return None;
    }
    let mut buf = Vec::new();
    while let Some(chunk) = r.chunk().await.ok()? {
        if buf.len() + chunk.len() > MAX {
            return None;
        }
        buf.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&buf).ok()
}

/// An error code of the authorization server as hqc repeats it: a short word, nothing else.
fn word(s: &str) -> String {
    let w: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .take(60)
        .collect();
    if w.is_empty() {
        "error".into()
    } else {
        w
    }
}

fn same_issuer(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// `https://host/a/b` -> (`https://host`, `/a/b`, or "" for `/`).
fn split(url: &str) -> Option<(String, String)> {
    let u = reqwest::Url::parse(url).ok()?;
    let origin = u.origin().ascii_serialization();
    let path = u.path().trim_end_matches('/').to_string();
    Some((origin, path))
}

/// The server's authorization server and its endpoints.
async fn discover(app: &AppState, r: &Row) -> Result<Meta, UpErr> {
    let no = |m: &str| UpErr::Unavailable(format!("OAuth discovery: {m}"));
    let (origin, path) = split(&r.url).ok_or_else(|| no("a bad url"))?;
    // The `401` of an unauthenticated request names the resource metadata.
    let mut candidates = vec![];
    if let Ok(http) = client_for(app, &r.url, Duration::from_secs(20)).await {
        let probe = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": super::PROTOCOL, "capabilities": {},
            "clientInfo": {"name": "hqc", "version": env!("CARGO_PKG_VERSION")}}});
        if let Ok(resp) = http
            .post(&r.url)
            .header("accept", "application/json, text/event-stream")
            .json(&probe)
            .send()
            .await
        {
            if let Some(m) = resp
                .headers()
                .get("www-authenticate")
                .and_then(|v| v.to_str().ok())
                .and_then(|h| param(h, "resource_metadata"))
            {
                candidates.push(m);
            }
        }
    }
    candidates.push(format!(
        "{origin}/.well-known/oauth-protected-resource{path}"
    ));
    candidates.push(format!("{origin}/.well-known/oauth-protected-resource"));
    let mut prm = None;
    for c in candidates {
        if let Some(v) = get_json(app, &c).await {
            prm = Some(v);
            break;
        }
    }
    let issuer = prm
        .as_ref()
        .and_then(|p| p.get("authorization_servers"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| origin.clone());
    let (is_origin, is_path) = split(&issuer).ok_or_else(|| no("a bad issuer"))?;
    let mut as_meta = None;
    for c in [
        format!("{is_origin}/.well-known/oauth-authorization-server{is_path}"),
        format!("{is_origin}/.well-known/openid-configuration{is_path}"),
        format!("{is_origin}{is_path}/.well-known/openid-configuration"),
    ] {
        if let Some(v) = get_json(app, &c).await {
            as_meta = Some(v);
            break;
        }
    }
    let m = as_meta.ok_or_else(|| no("the server names no authorization server metadata"))?;
    // RFC 8414 §3.3: the metadata must be the issuer's that was asked for, else another server
    // could hand out a third one's endpoints (a mix-up).
    let named = m.get("issuer").and_then(Value::as_str).unwrap_or_default();
    if !same_issuer(named, &issuer) {
        return Err(no(
            "the authorization server's metadata names another issuer",
        ));
    }
    let url = |k: &str| -> Result<String, UpErr> {
        let u = m
            .get(k)
            .and_then(Value::as_str)
            .ok_or_else(|| no(&format!("no {k}")))?;
        check_url(u).map_err(|e| no(&format!("{k}: {e}")))?;
        Ok(u.to_string())
    };
    let scope = r.oauth_scope.clone().or_else(|| {
        prm.as_ref()
            .and_then(|p| p.get("scopes_supported"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|s| !s.is_empty())
    });
    Ok(Meta {
        issuer: issuer.clone(),
        iss_supported: m
            .get("authorization_response_iss_parameter_supported")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        authorization_endpoint: url("authorization_endpoint")?,
        token_endpoint: url("token_endpoint")?,
        registration_endpoint: url("registration_endpoint").ok(),
        token_auth_methods: m
            .get("token_endpoint_auth_methods_supported")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        scope,
    })
}

/// A server that answered `401`: it signs in with OAuth when it names protected-resource
/// metadata (in `WWW-Authenticate` or at the well-known path) that names an authorization server.
pub(super) async fn offers_oauth(
    app: &AppState,
    url: &str,
    www_authenticate: Option<&str>,
) -> bool {
    let Some((origin, path)) = split(url) else {
        return false;
    };
    let mut candidates: Vec<String> = www_authenticate
        .and_then(|h| param(h, "resource_metadata"))
        .into_iter()
        .collect();
    candidates.push(format!(
        "{origin}/.well-known/oauth-protected-resource{path}"
    ));
    candidates.push(format!("{origin}/.well-known/oauth-protected-resource"));
    for c in candidates {
        if let Some(v) = get_json(app, &c).await {
            return v
                .get("authorization_servers")
                .and_then(Value::as_array)
                .is_some_and(|a| !a.is_empty());
        }
    }
    // An older server: its own authorization-server metadata at its origin.
    get_json(
        app,
        &format!("{origin}/.well-known/oauth-authorization-server"),
    )
    .await
    .is_some()
}

/// `name="value"` or `name=value` of a `WWW-Authenticate` header.
fn param(header: &str, name: &str) -> Option<String> {
    let i = header.find(&format!("{name}="))?;
    let rest = &header[i + name.len() + 1..];
    let v = match rest.strip_prefix('"') {
        Some(q) => q.split('"').next()?,
        None => rest.split([',', ' ']).next()?,
    };
    Some(v.to_string())
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn random() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    b64(&b)
}

/// Where the authorization server sends the browser back: the hub's public origin.
fn redirect_uri(app: &AppState, headers: &HeaderMap) -> String {
    if let Some(o) = app.config.public_origins.first() {
        return format!("{}{CALLBACK}", o.trim_end_matches('/'));
    }
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    let name = host.rsplit_once(':').map_or(host, |(h, _)| h);
    let scheme = if matches!(name, "localhost" | "127.0.0.1" | "[::1]") {
        "http"
    } else {
        "https"
    };
    format!("{scheme}://{host}{CALLBACK}")
}

fn bad(e: UpErr, name: &str) -> ApiError {
    ApiError::Unprocessable("sign_in_failed", e.to_mcp(name).message)
}

/// `POST /api/mcp/servers/{id}/sign_in`: the page to sign in on.
pub async fn sign_in(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Json<McpSignIn>> {
    let r = own(&app, &user, id).await?;
    if r.auth != "oauth" {
        return Err(ApiError::BadRequest(
            "this server signs in with headers".into(),
        ));
    }
    let meta = discover(&app, &r).await.map_err(|e| bad(e, &r.name))?;
    // The server now names another authorization server than it was signed in with: its client
    // and tokens are not for that one. The owner looks and sets the server again (which forgets
    // them) before anything goes there.
    let before: Option<Meta> = r
        .oauth_meta
        .clone()
        .and_then(|m| serde_json::from_value(m).ok());
    if let Some(b) = &before {
        if !same_issuer(&b.issuer, &meta.issuer) {
            return Err(ApiError::Conflict(
                "issuer_changed",
                format!(
                    "{} now names another authorization server ({}); edit the server to sign in \
                     there",
                    r.name, meta.issuer
                ),
            ));
        }
    }
    let redirect = redirect_uri(&app, &headers);
    let mut registered_secret = None;
    let client_id = match &r.oauth_client_id {
        Some(c) => c.clone(),
        None => {
            let reg = meta.registration_endpoint.clone().ok_or_else(|| {
                ApiError::Unprocessable(
                    "client_id_needed",
                    "the server offers no client registration: give a client id".into(),
                )
            })?;
            let http = client_for(&app, &reg, Duration::from_secs(20))
                .await
                .map_err(|e| bad(e, &r.name))?;
            let resp = http
                .post(&reg)
                .json(&json!({
                    "client_name": "hqc",
                    "redirect_uris": [redirect],
                    "grant_types": ["authorization_code", "refresh_token"],
                    "response_types": ["code"],
                    "token_endpoint_auth_method": "none",
                }))
                .send()
                .await
                .map_err(|e| bad(UpErr::Unavailable(super::short(&e)), &r.name))?;
            let v: Value = capped_json(resp).await.ok_or_else(|| {
                bad(
                    UpErr::Unavailable("the registration answered no JSON".into()),
                    &r.name,
                )
            })?;
            let c = v
                .get("client_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    bad(
                        UpErr::Unavailable("the registration gave no client_id".into()),
                        &r.name,
                    )
                })?
                .to_string();
            registered_secret = v
                .get("client_secret")
                .and_then(Value::as_str)
                .map(String::from);
            c
        }
    };
    key(&app).map_err(|e| bad(e, &r.name))?;
    let state = random();
    let verifier = random();
    let challenge = b64(&Sha256::digest(verifier.as_bytes()));
    let mut url = reqwest::Url::parse(&meta.authorization_endpoint)
        .map_err(|e| ApiError::BadRequest(format!("authorization_endpoint: {e}")))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", &redirect)
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("resource", &r.url);
        if let Some(sc) = &meta.scope {
            q.append_pair("scope", sc);
        }
    }
    {
        let mut g = app.mcp_upstream.oauth.lock().unwrap();
        g.retain(|_, p| p.at.elapsed() < PENDING_FOR);
        g.insert(
            state,
            Pending {
                owner: user.id,
                server: id,
                verifier,
                redirect_uri: redirect,
                at: Instant::now(),
                meta: meta.clone(),
                client_id: client_id.clone(),
                registered_secret,
            },
        );
    }
    tracing::info!(server_id = %id, "mcp oauth sign-in started");
    Ok(Json(McpSignIn {
        url: url.to_string(),
    }))
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error: Option<String>,
    /// RFC 9207: the issuer that answered.
    #[serde(default)]
    iss: Option<String>,
}

fn page(title: &str, text: &str) -> Response {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    Html(format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width\">\
         <title>{t}</title><body style=\"font-family:system-ui;padding:24px\"><h1>{t}</h1>\
         <p>{x}</p><p><a href=\"/#/settings\">Back to hqc</a></p>",
        t = esc(title),
        x = esc(text)
    ))
    .into_response()
}

/// `GET /api/mcp/oauth/callback`: where the authorization server sends the browser back.
pub async fn callback(
    State(app): State<AppState>,
    user: CurrentUser,
    Query(q): Query<CallbackQuery>,
) -> Response {
    let pending = q.state.as_deref().and_then(|st| {
        let mut g = app.mcp_upstream.oauth.lock().unwrap();
        g.remove(st)
    });
    let Some(p) = pending.filter(|p| p.owner == user.id && p.at.elapsed() < PENDING_FOR) else {
        return page(
            "Not signed in",
            "This sign-in is unknown or expired: start it again.",
        );
    };
    if let Some(e) = q.error {
        return page(
            "Not signed in",
            &format!("The authorization server said: {}.", word(&e)),
        );
    }
    // RFC 9207: a code from another issuer than the one this sign-in went to is refused.
    match &q.iss {
        Some(iss) if !same_issuer(iss, &p.meta.issuer) => {
            return page(
                "Not signed in",
                "The code came from another authorization server.",
            )
        }
        None if p.meta.iss_supported => {
            return page(
                "Not signed in",
                "The authorization server did not say who it is.",
            )
        }
        _ => {}
    }
    let Some(code) = q.code else {
        return page("Not signed in", "The authorization server gave no code.");
    };
    let r = match row(&app, p.server).await {
        Ok(r) => r,
        Err(_) => return page("Not signed in", "The server is gone."),
    };
    let (meta, client_id) = (p.meta, p.client_id);
    // A client the hub registered now has its own secret; one given by the owner keeps theirs.
    let mut s = secrets_of(&app, &r).unwrap_or_default();
    if p.registered_secret.is_some() || r.oauth_client_id.is_none() {
        s.client_secret = p.registered_secret;
    }
    let form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code),
        ("redirect_uri", p.redirect_uri),
        ("code_verifier", p.verifier),
        ("resource", r.url.clone()),
    ];
    let t = match token_request(&app, &meta, &client_id, s.client_secret.as_deref(), form).await {
        Ok(t) => t,
        Err(e) => return page("Not signed in", &e.to_mcp(&r.name).message),
    };
    s.access_token = Some(t.access_token);
    s.refresh_token = t.refresh_token;
    s.expires_at = t.expires_in.map(|e| Utc::now().timestamp() + e);
    let Ok(sealed) = seal_secrets(&app, r.id, &s) else {
        return page("Not signed in", "The hub could not keep the sign-in.");
    };
    // The endpoints, the client and the tokens together, under the refresh lock.
    let lock = {
        let mut g = app.mcp_upstream.refreshing.lock().unwrap();
        g.entry(r.id).or_default().clone()
    };
    let _held = lock.lock().await;
    let stored = sqlx::query(
        "UPDATE mcp_servers SET oauth_meta = $2, oauth_client_id = $3, oauth_sealed = $4,
                                status = 'unknown', last_error = NULL, cached_at = NULL,
                                updated_at = now()
         WHERE id = $1",
    )
    .bind(r.id)
    .bind(serde_json::to_value(&meta).expect("meta serializes"))
    .bind(&client_id)
    .bind(sealed)
    .execute(&app.db)
    .await;
    if stored.is_err() {
        return page("Not signed in", "The hub could not keep the sign-in.");
    }
    super::forget_sessions(&app, r.id);
    tracing::info!(server_id = %r.id, "mcp oauth signed in");
    page(
        &format!("Signed in to {}", r.name),
        "hqc keeps the sign-in and refreshes it itself. You can close this page.",
    )
}

/// The sealed secrets of a server set to OAuth with a client secret given by hand; no tokens.
pub(super) fn sealed_client(
    app: &AppState,
    id: Uuid,
    secret: Option<String>,
) -> ApiResult<Option<Vec<u8>>> {
    let s = Secrets {
        client_secret: secret,
        ..Secrets::default()
    };
    match seal_secrets(app, id, &s) {
        Ok(v) => Ok(Some(v)),
        Err(_) => Err(ApiError::Unprocessable(
            "mcp_key_missing",
            "the hub keeps a sign-in only with HQC_MCP_KEY set".into(),
        )),
    }
}

/// `DELETE /api/mcp/servers/{id}/sign_in`: forgets the tokens.
pub async fn sign_out(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let r = own(&app, &user, id).await?;
    let mut s = secrets_of(&app, &r).unwrap_or_default();
    s.access_token = None;
    s.refresh_token = None;
    s.expires_at = None;
    let sealed = seal_secrets(&app, id, &s).ok();
    sqlx::query(
        "UPDATE mcp_servers SET oauth_sealed = $2, status = 'sign_in', last_error = NULL WHERE id = $1",
    )
    .bind(id)
    .bind(sealed)
    .execute(&app.db)
    .await?;
    super::forget_sessions(&app, id);
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    #[test]
    fn www_authenticate_param() {
        let h = r#"Bearer error="invalid_token", resource_metadata="https://x/.well-known/oauth-protected-resource/mcp""#;
        assert_eq!(
            super::param(h, "resource_metadata").as_deref(),
            Some("https://x/.well-known/oauth-protected-resource/mcp")
        );
        assert_eq!(super::param("Bearer realm=x", "resource_metadata"), None);
    }
}
