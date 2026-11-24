//! Remote MCP servers the owner keeps on the hub (SPEC 0.235, iteration 7, `docs/ITERATION-7.md`).
//!
//! ```text
//! CLI --stdio--> `hqc-worker mcp --upstream <name>` --mcp.upstream--> hub --HTTP--> server
//! ```
//!
//! - The owner adds a server once (`/api/mcp/servers`): a name, an https URL, headers, a timeout
//!   and the projects that get it. The header values are sealed with `HQC_MCP_KEY`
//!   (XChaCha20-Poly1305, the server's id as associated data) and never sent back.
//! - `session.start.mcp_servers` names the servers of the session's project; a worker whose
//!   environment has `hub_mcp` gives each to the CLI under its own name as a stdio relay, so the
//!   tools are `mcp__<name>__<tool>`, as with a direct connection.
//! - The relay's requests come as `mcp.upstream`; the hub checks the session token, that the
//!   server is the owner's and the session's project's, and makes the call over Streamable
//!   HTTP. One MCP session per hqc session and server (a server may keep state in it), opened on
//!   the first request and closed when the agent's process stops or after an hour unused.
//!   `serverInfo`, `instructions` and `tools/list` are cached per server for an hour.
//! - A result over [`UPSTREAM_INLINE`] is stored as a short-lived artifact of the owner, linked
//!   to the session, which the relay downloads; over [`UPSTREAM_MAX`] the call is a tool error.
//! - The server's URL must not lead into private or loopback addresses unless
//!   `HQC_MCP_ALLOW_PRIVATE=1`; the address checked is the address connected to, and redirects
//!   are not followed.
//! - Header values and tokens are in no log line, event or error.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use chrono::{DateTime, Utc};
use hqc_proto::{
    api::{McpServer, McpServerList, McpServerRequest},
    frame::{McpError, McpResult, McpUpstream, UpstreamAnswer, UpstreamArtifact},
    mcp::{error_code, valid_server_name, MAX_ARGS, UPSTREAM_INLINE, UPSTREAM_MAX},
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::{hash_secret, CurrentUser},
    db,
    error::{ApiError, ApiResult},
};

/// The MCP protocol version the hub asks a server for.
const PROTOCOL: &str = "2025-06-18";
/// How long `serverInfo`, `instructions` and the tools stay cached.
const CACHE: Duration = Duration::from_secs(3600);
/// An MCP session unused this long is closed.
const IDLE: Duration = Duration::from_secs(3600);
/// Limits of what the owner gives.
const MAX_HEADERS: usize = 8;
const MAX_HEADER_NAME: usize = 64;
const MAX_HEADER_VALUE: usize = 4096;
const MAX_URL: usize = 500;
const MAX_SERVERS: i64 = 50;
const DEFAULT_TIMEOUT_S: u32 = 300;
/// Headers the hub sets itself.
const OWN_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "content-type",
    "accept",
    "connection",
    "transfer-encoding",
    "mcp-session-id",
    "mcp-protocol-version",
];

// ---------------------------------------------------------------- state

/// The MCP sessions open to servers and the requests in flight, in memory: after a restart the
/// next request opens a new MCP session.
#[derive(Default)]
pub struct Upstreams {
    sessions: Mutex<HashMap<(Uuid, Uuid), Live>>,
    calls: Mutex<HashMap<String, tokio::task::AbortHandle>>,
    next_id: AtomicI64,
}

#[derive(Clone)]
struct Live {
    url: String,
    id: Option<String>,
    protocol: String,
    used: Instant,
}

impl Upstreams {
    /// A relay request being carried out, so that `mcp.cancel` can stop it.
    pub fn track(&self, request_id: String, task: tokio::task::AbortHandle) {
        self.calls.lock().unwrap().insert(request_id, task);
    }

    fn done(&self, request_id: &str) {
        self.calls.lock().unwrap().remove(request_id);
    }

    /// `mcp.cancel` of a relay request: stops it; `false` if it was not one.
    pub fn cancel(&self, request_id: &str) -> bool {
        match self.calls.lock().unwrap().remove(request_id) {
            Some(t) => {
                t.abort();
                true
            }
            None => false,
        }
    }

    fn id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed) + 1
    }
}

// ---------------------------------------------------------------- rows

#[derive(sqlx::FromRow, Clone)]
struct Row {
    id: Uuid,
    name: String,
    url: String,
    header_names: Vec<String>,
    sealed: Option<Vec<u8>>,
    timeout_s: i32,
    all_projects: bool,
    status: String,
    last_error: Option<String>,
    server_info: Option<Value>,
    instructions: Option<String>,
    tools: Option<Value>,
    cached_at: Option<DateTime<Utc>>,
}

const COLS: &str = "id, name, url, header_names, sealed, timeout_s, all_projects, \
                    status, last_error, server_info, instructions, tools, cached_at";

impl Row {
    fn fresh(&self) -> bool {
        self.cached_at
            .is_some_and(|t| (Utc::now() - t).to_std().is_ok_and(|age| age < CACHE))
    }

    fn tool_names(&self) -> Vec<String> {
        self.tools
            .as_ref()
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.get("name").and_then(Value::as_str).map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }
}

async fn projects_of(db: &sqlx::PgPool, server: Uuid) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "SELECT project_id FROM mcp_server_projects WHERE server_id = $1 ORDER BY project_id",
    )
    .bind(server)
    .fetch_all(db)
    .await
}

async fn to_api(db: &sqlx::PgPool, r: &Row) -> sqlx::Result<McpServer> {
    Ok(McpServer {
        id: r.id,
        name: r.name.clone(),
        url: r.url.clone(),
        header_names: r.header_names.clone(),
        timeout_s: r.timeout_s as u32,
        all_projects: r.all_projects,
        project_ids: projects_of(db, r.id).await?,
        status: r.status.clone(),
        last_error: r.last_error.clone(),
        tools: r.tool_names(),
        instructions: r.instructions.clone(),
    })
}

/// The names of the owner's servers that sessions of `project` get (`session.start`).
pub async fn names_for(db: &sqlx::PgPool, owner: Uuid, project: Uuid) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT name FROM mcp_servers m
         WHERE owner_id = $1
           AND (all_projects OR EXISTS (SELECT 1 FROM mcp_server_projects p
                                        WHERE p.server_id = m.id AND p.project_id = $2))
         ORDER BY name",
    )
    .bind(owner)
    .bind(project)
    .fetch_all(db)
    .await
}

async fn server_for(
    db: &sqlx::PgPool,
    owner: Uuid,
    project: Uuid,
    name: &str,
) -> sqlx::Result<Option<Row>> {
    sqlx::query_as(&format!(
        "SELECT {COLS} FROM mcp_servers m
         WHERE owner_id = $1 AND name = $3
           AND (all_projects OR EXISTS (SELECT 1 FROM mcp_server_projects p
                                        WHERE p.server_id = m.id AND p.project_id = $2))"
    ))
    .bind(owner)
    .bind(project)
    .bind(name)
    .fetch_optional(db)
    .await
}

// ---------------------------------------------------------------- sealing

fn cipher(key: &[u8; 32]) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(key.into())
}

/// `nonce || ciphertext` of the headers, bound to the server's id.
fn seal(key: &[u8; 32], id: Uuid, headers: &HashMap<String, String>) -> Vec<u8> {
    use rand::RngCore;
    let mut nonce = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce);
    let plain = serde_json::to_vec(headers).expect("headers serialize");
    let ct = cipher(key)
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &plain,
                aad: id.as_bytes(),
            },
        )
        .expect("encryption does not fail");
    let mut out = nonce.to_vec();
    out.extend(ct);
    out
}

fn unseal(key: &[u8; 32], id: Uuid, sealed: &[u8]) -> Option<HashMap<String, String>> {
    if sealed.len() < 24 {
        return None;
    }
    let (nonce, ct) = sealed.split_at(24);
    let plain = cipher(key)
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ct,
                aad: id.as_bytes(),
            },
        )
        .ok()?;
    serde_json::from_slice(&plain).ok()
}

/// The headers of a server as the hub sends them; `Err`: they cannot be read (no key, another
/// key).
fn headers_of(app: &AppState, r: &Row) -> Result<Vec<(String, String)>, String> {
    let Some(sealed) = &r.sealed else {
        return Ok(vec![]);
    };
    let key = app
        .config
        .mcp
        .key
        .as_ref()
        .ok_or("HQC_MCP_KEY is not set on the hub: its headers cannot be read")?;
    let mut h: Vec<(String, String)> = unseal(key, r.id, sealed)
        .ok_or("its headers cannot be read with this HQC_MCP_KEY: set them again")?
        .into_iter()
        .collect();
    h.sort();
    Ok(h)
}

// ---------------------------------------------------------------- the client

/// Why a request to a server failed.
#[derive(Debug)]
enum UpErr {
    /// Not reachable, 5xx, a malformed answer: `mcp_unavailable`.
    Unavailable(String),
    /// 401/403: the token or sign-in is refused.
    SignIn(String),
    Timeout(u32),
    TooLarge,
    /// 404 on a request with an MCP session: the server forgot it.
    Expired,
    /// A JSON-RPC error of the server.
    Rpc(Value),
}

impl UpErr {
    fn to_mcp(&self, server: &str) -> McpError {
        match self {
            UpErr::Unavailable(m) => McpError::new(
                error_code::MCP_UNAVAILABLE,
                format!("the MCP server {server} is unavailable: {m}"),
            ),
            UpErr::SignIn(m) => McpError::new(
                error_code::MCP_SIGN_IN,
                format!(
                    "the MCP server {server} refused hqc's credentials ({m}): set them again in \
                     Settings → MCP servers"
                ),
            ),
            UpErr::Timeout(s) => McpError::new(
                error_code::MCP_TIMEOUT,
                format!(
                    "the MCP server {server} did not answer in {s} s; the call may still have been \
                     carried out on the server: check before retrying"
                ),
            ),
            UpErr::TooLarge => McpError::new(
                error_code::MCP_TOO_LARGE,
                format!(
                    "the MCP server {server}'s result is over {} MiB",
                    UPSTREAM_MAX / (1024 * 1024)
                ),
            ),
            UpErr::Expired => McpError::new(
                error_code::MCP_UNAVAILABLE,
                format!("the MCP server {server} forgot the session"),
            ),
            UpErr::Rpc(e) => McpError::new(
                error_code::MCP_UNAVAILABLE,
                format!(
                    "the MCP server {server} answered with an error: {}",
                    e.get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("no message")
                ),
            ),
        }
    }
}

/// The address is not one a server URL may lead to without `HQC_MCP_ALLOW_PRIVATE`.
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_broadcast()
                || v.is_multicast()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
        }
        IpAddr::V6(v) => {
            if let Some(m) = v.to_ipv4_mapped() {
                return is_private(IpAddr::V4(m));
            }
            let s = v.segments()[0];
            v.is_loopback()
                || v.is_unspecified()
                || v.is_multicast()
                || (s & 0xfe00) == 0xfc00
                || (s & 0xffc0) == 0xfe80
        }
    }
}

/// An https URL, or http to a loopback host; the reason otherwise.
fn check_url(raw: &str) -> Result<reqwest::Url, String> {
    let raw = raw.trim();
    if raw.chars().count() > MAX_URL || raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "url is at most {MAX_URL} characters, without spaces"
        ));
    }
    let u = reqwest::Url::parse(raw).map_err(|e| format!("url: {e}"))?;
    let host = u.host_str().ok_or("url has no host")?.to_string();
    if !u.username().is_empty() || u.password().is_some() {
        return Err("url must not carry a user or a password: use a header".into());
    }
    let loopback = host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    match u.scheme() {
        "https" => Ok(u),
        "http" if loopback => Ok(u),
        _ => Err("url must be https (http only to a loopback host)".into()),
    }
}

/// A client that connects to the address checked here only, without redirects.
async fn client_for(
    app: &AppState,
    url: &str,
    timeout: Duration,
) -> Result<reqwest::Client, UpErr> {
    let u = check_url(url).map_err(UpErr::Unavailable)?;
    let host = u.host_str().unwrap_or_default().to_string();
    let port = u.port_or_known_default().unwrap_or(443);
    let bare = host.trim_matches(['[', ']']).to_string();
    let addrs: Vec<SocketAddr> = match bare.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((bare.as_str(), port))
            .await
            .map_err(|e| UpErr::Unavailable(format!("{host} does not resolve: {e}")))?
            .collect(),
    };
    if addrs.is_empty() {
        return Err(UpErr::Unavailable(format!("{host} does not resolve")));
    }
    if !app.config.mcp.allow_private && addrs.iter().any(|a| is_private(a.ip())) {
        return Err(UpErr::Unavailable(format!(
            "{host} is a private or loopback address; the hub reaches one only with \
             HQC_MCP_ALLOW_PRIVATE=1"
        )));
    }
    // No proxy from the environment: the connection must go to the address checked here.
    let mut b = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(timeout);
    if bare.parse::<IpAddr>().is_err() {
        b = b.resolve_to_addrs(&host, &addrs);
    }
    b.build()
        .map_err(|e| UpErr::Unavailable(format!("client: {e}")))
}

/// One JSON-RPC message to the server: its answer to request `want` (`None` for a
/// notification, answered `202`), and the MCP session the server named.
async fn post(
    http: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
    live: Option<&Live>,
    msg: &Value,
    want: Option<i64>,
) -> Result<(Option<Value>, Option<String>), UpErr> {
    let mut req = http
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(l) = live {
        if let Some(id) = &l.id {
            req = req.header("mcp-session-id", id.as_str());
        }
        req = req.header("mcp-protocol-version", l.protocol.as_str());
    }
    let mut resp = req.json(msg).send().await.map_err(|e| {
        if e.is_timeout() {
            UpErr::Timeout(0)
        } else {
            UpErr::Unavailable(short(&e))
        }
    })?;
    let status = resp.status();
    let session = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    if status.as_u16() == 404 && live.is_some_and(|l| l.id.is_some()) {
        return Err(UpErr::Expired);
    }
    if matches!(status.as_u16(), 401 | 403) {
        return Err(UpErr::SignIn(status.to_string()));
    }
    if want.is_none() {
        return if status.is_success() {
            Ok((None, session))
        } else {
            Err(UpErr::Unavailable(status.to_string()))
        };
    }
    if !status.is_success() {
        return Err(UpErr::Unavailable(status.to_string()));
    }
    let sse = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("text/event-stream"));
    let cap = UPSTREAM_MAX + 256 * 1024;
    let mut buf: Vec<u8> = Vec::new();
    let mut read = 0usize;
    let mut scanned = 0usize;
    loop {
        let chunk = resp.chunk().await.map_err(|e| {
            if e.is_timeout() {
                UpErr::Timeout(0)
            } else {
                UpErr::Unavailable(short(&e))
            }
        })?;
        let Some(chunk) = chunk else { break };
        read += chunk.len();
        if read > cap {
            return Err(UpErr::TooLarge);
        }
        buf.extend_from_slice(&chunk);
        if sse {
            if let Some(v) = sse_answer(&mut buf, want, &mut scanned) {
                return Ok((Some(v), session));
            }
        }
    }
    if sse {
        buf.extend_from_slice(b"\n\n");
        return sse_answer(&mut buf, want, &mut scanned)
            .map(|v| (Some(v), session))
            .ok_or_else(|| UpErr::Unavailable("the event stream ended without the answer".into()));
    }
    let v: Value = serde_json::from_slice(&buf)
        .map_err(|e| UpErr::Unavailable(format!("the answer is not JSON: {e}")))?;
    // A batch answer is not asked for; take the message with our id.
    let v = match v {
        Value::Array(a) => a
            .into_iter()
            .find(|m| m.get("id").and_then(Value::as_i64) == want)
            .ok_or_else(|| UpErr::Unavailable("no answer to the request".into()))?,
        v => v,
    };
    Ok((Some(v), session))
}

/// Takes the complete events out of `buf`; the message answering `want`, if one came.
/// `scanned`: how far `buf` was searched before, so a long event is not searched again from its
/// start at every chunk.
fn sse_answer(buf: &mut Vec<u8>, want: Option<i64>, scanned: &mut usize) -> Option<Value> {
    loop {
        // An event ends at a blank line, `\n\n` or `\r\n\r\n`.
        let from = scanned.saturating_sub(3).min(buf.len());
        let Some((end, skip)) = (from..buf.len()).find_map(|i| {
            if buf[i..].starts_with(b"\r\n\r\n") {
                Some((i, 4))
            } else if buf[i..].starts_with(b"\n\n") {
                Some((i, 2))
            } else {
                None
            }
        }) else {
            *scanned = buf.len();
            return None;
        };
        *scanned = 0;
        let event: Vec<u8> = buf.drain(..end + skip).take(end).collect();
        let event = String::from_utf8_lossy(&event);
        let data: Vec<&str> = event
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|l| l.strip_prefix(' ').unwrap_or(l))
            .collect();
        if data.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&data.join("\n")) else {
            continue;
        };
        if v.get("id").and_then(Value::as_i64) == want
            && (v.get("result").is_some() || v.get("error").is_some())
        {
            return Some(v);
        }
    }
}

fn short(e: &reqwest::Error) -> String {
    // reqwest's text names the URL, which may carry a path the owner thinks private; keep the kind.
    if e.is_connect() {
        "connection failed".into()
    } else if e.is_request() {
        "the request failed".into()
    } else if e.is_body() || e.is_decode() {
        "the answer broke off".into()
    } else {
        "the request failed".into()
    }
}

/// The JSON-RPC `result`, or the server's error.
fn result_of(v: Value) -> Result<Value, UpErr> {
    if let Some(e) = v.get("error") {
        return Err(UpErr::Rpc(e.clone()));
    }
    v.get("result")
        .cloned()
        .ok_or_else(|| UpErr::Unavailable("the answer has no result".into()))
}

/// `initialize` and `notifications/initialized`: a new MCP session and the server's own result.
async fn initialize(
    app: &AppState,
    http: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
) -> Result<(Live, Value), UpErr> {
    let id = app.mcp_upstream.id();
    let msg = json!({
        "jsonrpc": "2.0", "id": id, "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": "hqc", "version": env!("CARGO_PKG_VERSION")},
        },
    });
    let (answer, session) = post(http, url, headers, None, &msg, Some(id)).await?;
    let result = result_of(answer.unwrap_or(Value::Null))?;
    let live = Live {
        url: url.to_string(),
        id: session,
        protocol: result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(PROTOCOL)
            .to_string(),
        used: Instant::now(),
    };
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    post(http, url, headers, Some(&live), &note, None).await?;
    Ok((live, result))
}

/// Closes an MCP session at its server (`DELETE`), whatever the answer.
async fn close(app: &AppState, live: Live, headers: Vec<(String, String)>) {
    let Some(id) = live.id.clone() else { return };
    let Ok(http) = client_for(app, &live.url, Duration::from_secs(10)).await else {
        return;
    };
    let mut req = http.delete(&live.url).header("mcp-session-id", id);
    for (k, v) in &headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let _ = req.send().await;
}

/// The agent's process stopped: its MCP sessions are closed.
pub fn process_stopped(app: &AppState, session_id: Uuid) {
    let gone: Vec<((Uuid, Uuid), Live)> = {
        let mut g = app.mcp_upstream.sessions.lock().unwrap();
        let keys: Vec<(Uuid, Uuid)> = g.keys().filter(|k| k.0 == session_id).copied().collect();
        keys.into_iter()
            .filter_map(|k| g.remove(&k).map(|l| (k, l)))
            .collect()
    };
    for ((_, server), live) in gone {
        let app = app.clone();
        tokio::spawn(async move {
            let row: Option<Row> =
                sqlx::query_as(&format!("SELECT {COLS} FROM mcp_servers WHERE id = $1"))
                    .bind(server)
                    .fetch_optional(&app.db)
                    .await
                    .ok()
                    .flatten();
            let headers = row
                .as_ref()
                .and_then(|r| headers_of(&app, r).ok())
                .unwrap_or_default();
            close(&app, live, headers).await;
        });
    }
}

/// Closes the MCP sessions unused for [`IDLE`]; every ten minutes.
pub async fn idle_loop(app: AppState) {
    loop {
        tokio::time::sleep(Duration::from_secs(600)).await;
        let old: Vec<(Uuid, Uuid)> = app
            .mcp_upstream
            .sessions
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, l)| l.used.elapsed() > IDLE)
            .map(|(k, _)| *k)
            .collect();
        for (session, _) in old {
            process_stopped(&app, session);
        }
    }
}

// ---------------------------------------------------------------- the relay

/// `mcp.upstream` from `worker_id`: the `mcp.result` to answer it with.
pub async fn relay(app: &AppState, worker_id: Uuid, up: McpUpstream) -> McpResult {
    let rid = up.request_id.clone();
    let out = match run(app, worker_id, &up).await {
        Ok(answer) => McpResult::ok(&rid, serde_json::to_value(answer).unwrap_or(Value::Null)),
        Err(e) => {
            tracing::info!(%worker_id, request_id = %rid, server = %up.server, method = %up.method, code = %e.code, "mcp upstream refused: {}", e.message);
            McpResult {
                request_id: rid.clone(),
                result: None,
                error: Some(e),
            }
        }
    };
    app.mcp_upstream.done(&rid);
    out
}

async fn run(
    app: &AppState,
    worker_id: Uuid,
    up: &McpUpstream,
) -> Result<UpstreamAnswer, McpError> {
    use crate::mcp::{internal, per_minute, too_often};
    let limits = &app.config.limits;
    if !per_minute(
        app,
        "mcp_calls_worker",
        worker_id,
        limits.mcp_calls_per_worker_min,
    ) {
        return Err(too_often("calls from this worker"));
    }
    if !crate::mcp::is_request_id(&up.request_id) {
        return Err(McpError::new(error_code::INVALID_ARGS, "a bad request_id"));
    }
    if serde_json::to_vec(&up.params).map_err(internal)?.len() > MAX_ARGS {
        return Err(McpError::new(
            error_code::INVALID_ARGS,
            format!("arguments over {MAX_ARGS} bytes"),
        ));
    }
    let unauthorized = || {
        McpError::new(
            error_code::UNAUTHORIZED,
            "the session token is not one of a session this worker runs",
        )
    };
    if !crate::mcp::is_session_token(&up.session_token) {
        return Err(unauthorized());
    }
    let caller = db::session_by_token_hash(&app.db, worker_id, &hash_secret(&up.session_token))
        .await
        .map_err(internal)?
        .ok_or_else(unauthorized)?;
    if caller.archived {
        return Err(McpError::new(
            error_code::UNAVAILABLE,
            "this session is archived; its agent acts no more",
        ));
    }
    if !per_minute(
        app,
        "mcp_calls_session",
        caller.id,
        limits.mcp_calls_per_session_min,
    ) {
        return Err(too_often("MCP calls from this session"));
    }
    let row = server_for(&app.db, caller.owner_id, caller.project_id, &up.server)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            McpError::new(
                error_code::NOT_FOUND,
                format!(
                    "the hub keeps no MCP server {} for this session's project",
                    up.server
                ),
            )
        })?;
    let headers = headers_of(app, &row)
        .map_err(|m| McpError::new(error_code::MCP_UNAVAILABLE, format!("{}: {m}", row.name)))?;
    let result = match up.method.as_str() {
        "initialize" => {
            if row.fresh() && row.server_info.is_some() {
                json!({"serverInfo": row.server_info, "instructions": row.instructions})
            } else {
                let r = session(app, &row, &headers, caller.id, true).await;
                let info = match r {
                    Ok((_, Some(init))) => init,
                    Ok((_, None)) => {
                        json!({"serverInfo": row.server_info, "instructions": row.instructions})
                    }
                    Err(e) => return Err(failed(app, &row, e).await),
                };
                json!({"serverInfo": info.get("serverInfo"), "instructions": info.get("instructions")})
            }
        }
        "tools/list" => {
            if row.fresh() && row.tools.is_some() {
                json!({"tools": row.tools})
            } else {
                match list_tools(app, &row, &headers, caller.id).await {
                    Ok(t) => json!({"tools": t}),
                    Err(e) => return Err(failed(app, &row, e).await),
                }
            }
        }
        "tools/call" => match call(app, &row, &headers, caller.id, &up.params).await {
            Ok(v) => v,
            Err(e) => return Err(failed(app, &row, e).await),
        },
        m => {
            return Err(McpError::new(
                error_code::INVALID_ARGS,
                format!("{m} is not relayed"),
            ))
        }
    };
    set_status(app, row.id, None).await;
    encode(app, &caller, &row.name, result).await
}

/// Records the failure on the server and gives the tool error.
async fn failed(app: &AppState, row: &Row, e: UpErr) -> McpError {
    let e = match e {
        UpErr::Timeout(_) => UpErr::Timeout(row.timeout_s as u32),
        e => e,
    };
    let m = e.to_mcp(&row.name);
    set_status(app, row.id, Some(&m.message)).await;
    m
}

async fn set_status(app: &AppState, id: Uuid, error: Option<&str>) {
    let _ = sqlx::query(
        "UPDATE mcp_servers SET status = CASE WHEN $2::text IS NULL THEN 'ok' ELSE 'failing' END,
                                last_error = $2
         WHERE id = $1",
    )
    .bind(id)
    .bind(error)
    .execute(&app.db)
    .await;
}

/// The hqc session's MCP session with the server, opened if there is none (`init`: and the
/// server's `initialize` result when it was opened now, which refreshes the cache).
async fn session(
    app: &AppState,
    row: &Row,
    headers: &[(String, String)],
    sid: Uuid,
    init: bool,
) -> Result<(Live, Option<Value>), UpErr> {
    let key = (sid, row.id);
    if let Some(l) = app.mcp_upstream.sessions.lock().unwrap().get_mut(&key) {
        if l.url == row.url {
            l.used = Instant::now();
            return Ok((l.clone(), None));
        }
    }
    let http = client_for(app, &row.url, Duration::from_secs(60)).await?;
    let (live, result) = initialize(app, &http, &row.url, headers).await?;
    app.mcp_upstream
        .sessions
        .lock()
        .unwrap()
        .insert(key, live.clone());
    let _ = sqlx::query(
        "UPDATE mcp_servers SET server_info = $2, instructions = $3,
                                cached_at = CASE WHEN tools IS NULL THEN NULL ELSE cached_at END
         WHERE id = $1",
    )
    .bind(row.id)
    .bind(result.get("serverInfo"))
    .bind(result.get("instructions").and_then(Value::as_str))
    .execute(&app.db)
    .await;
    Ok((live, init.then_some(result)))
}

/// A request in the hqc session's MCP session; once more in a new one if the server forgot it.
async fn request(
    app: &AppState,
    row: &Row,
    headers: &[(String, String)],
    sid: Uuid,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, UpErr> {
    let http = client_for(app, &row.url, timeout).await?;
    for attempt in 0..2 {
        let (live, _) = session(app, row, headers, sid, false).await?;
        let id = app.mcp_upstream.id();
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        match post(&http, &row.url, headers, Some(&live), &msg, Some(id)).await {
            Err(UpErr::Expired) if attempt == 0 => {
                app.mcp_upstream
                    .sessions
                    .lock()
                    .unwrap()
                    .remove(&(sid, row.id));
            }
            Ok((answer, _)) => return result_of(answer.unwrap_or(Value::Null)),
            Err(e) => return Err(e),
        }
    }
    Err(UpErr::Expired)
}

async fn list_tools(
    app: &AppState,
    row: &Row,
    headers: &[(String, String)],
    sid: Uuid,
) -> Result<Value, UpErr> {
    let mut tools: Vec<Value> = vec![];
    let mut cursor: Option<String> = None;
    for _ in 0..20 {
        let params = match &cursor {
            Some(c) => json!({"cursor": c}),
            None => json!({}),
        };
        let r = request(
            app,
            row,
            headers,
            sid,
            "tools/list",
            params,
            Duration::from_secs(60),
        )
        .await?;
        if let Some(a) = r.get("tools").and_then(Value::as_array) {
            tools.extend(a.iter().cloned());
        }
        cursor = r
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(String::from);
        if cursor.is_none() {
            break;
        }
    }
    let tools = Value::Array(tools);
    let _ = sqlx::query("UPDATE mcp_servers SET tools = $2, cached_at = now() WHERE id = $1")
        .bind(row.id)
        .bind(&tools)
        .execute(&app.db)
        .await;
    Ok(tools)
}

async fn call(
    app: &AppState,
    row: &Row,
    headers: &[(String, String)],
    sid: Uuid,
    params: &Value,
) -> Result<Value, UpErr> {
    let timeout = Duration::from_secs(row.timeout_s.max(1) as u64);
    let params = json!({
        "name": params.get("name"),
        "arguments": params.get("arguments").cloned().unwrap_or_else(|| json!({})),
    });
    match tokio::time::timeout(
        timeout + Duration::from_secs(5),
        request(app, row, headers, sid, "tools/call", params, timeout),
    )
    .await
    {
        Ok(r) => r,
        Err(_) => Err(UpErr::Timeout(row.timeout_s as u32)),
    }
}

/// The answer as the frame carries it: in it, or as an artifact for a large one.
async fn encode(
    app: &AppState,
    caller: &db::SessionRow,
    server: &str,
    result: Value,
) -> Result<UpstreamAnswer, McpError> {
    let bytes = serde_json::to_vec(&result).map_err(crate::mcp::internal)?;
    if bytes.len() > UPSTREAM_MAX {
        return Err(UpErr::TooLarge.to_mcp(server));
    }
    if bytes.len() <= UPSTREAM_INLINE {
        return Ok(UpstreamAnswer::Upstream(result));
    }
    let size = bytes.len() as u64;
    let a = crate::artifacts::store_for_session(
        app,
        caller.owner_id,
        caller.id,
        &format!("{server}-result.json"),
        "application/json",
        bytes,
    )
    .await
    .map_err(crate::mcp::internal)?;
    // The relay fetches it at once; whatever is left goes after an hour.
    let (app2, id) = (app.clone(), a.id);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        crate::artifacts::forget(&app2, id).await;
    });
    Ok(UpstreamAnswer::UpstreamArtifact(UpstreamArtifact {
        id: a.id,
        size,
        sha256: a.sha256,
    }))
}

// ---------------------------------------------------------------- REST

fn bad(m: impl Into<String>) -> ApiError {
    ApiError::BadRequest(m.into())
}

/// Checks headers as the owner gives them.
fn check_headers(h: &[hqc_proto::api::McpHeader]) -> ApiResult<HashMap<String, String>> {
    if h.len() > MAX_HEADERS {
        return Err(bad(format!("at most {MAX_HEADERS} headers")));
    }
    let mut out = HashMap::new();
    for x in h {
        let name = x.name.trim();
        if name.is_empty()
            || name.len() > MAX_HEADER_NAME
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(bad("a header name is 1 to 64 letters, digits, '-' and '_'"));
        }
        if OWN_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(bad(format!("{name} is set by the hub")));
        }
        if x.value.is_empty()
            || x.value.len() > MAX_HEADER_VALUE
            || x.value.chars().any(|c| c.is_control())
        {
            return Err(bad(format!(
                "the value of {name} is 1 to {MAX_HEADER_VALUE} bytes on one line"
            )));
        }
        if reqwest::header::HeaderValue::from_str(&x.value).is_err() {
            return Err(bad(format!("the value of {name} is not a header value")));
        }
        if out.insert(name.to_string(), x.value.clone()).is_some() {
            return Err(bad(format!("{name} is given twice")));
        }
    }
    Ok(out)
}

async fn check_projects(app: &AppState, owner: Uuid, ids: &[Uuid]) -> ApiResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM projects WHERE id = ANY($1) AND owner_id = $2")
            .bind(ids)
            .bind(owner)
            .fetch_one(&app.db)
            .await?;
    if n as usize != ids.iter().collect::<std::collections::HashSet<_>>().len() {
        return Err(bad("project_ids: a project that is not yours"));
    }
    Ok(())
}

async fn own(app: &AppState, user: &CurrentUser, id: Uuid) -> ApiResult<Row> {
    sqlx::query_as(&format!(
        "SELECT {COLS} FROM mcp_servers WHERE id = $1 AND owner_id = $2"
    ))
    .bind(id)
    .bind(user.id)
    .fetch_optional(&app.db)
    .await?
    .ok_or(ApiError::NotFound)
}

async fn set_projects(app: &AppState, id: Uuid, ids: &[Uuid]) -> ApiResult<()> {
    sqlx::query("DELETE FROM mcp_server_projects WHERE server_id = $1")
        .bind(id)
        .execute(&app.db)
        .await?;
    sqlx::query(
        "INSERT INTO mcp_server_projects (server_id, project_id)
         SELECT $1, unnest($2::uuid[]) ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(ids)
    .execute(&app.db)
    .await?;
    Ok(())
}

fn timeout_of(t: Option<u32>) -> ApiResult<i32> {
    let t = t.unwrap_or(DEFAULT_TIMEOUT_S);
    if !(30..=1800).contains(&t) {
        return Err(bad("timeout_s is 30 to 1800"));
    }
    Ok(t as i32)
}

fn sealed_for(app: &AppState, id: Uuid, h: &HashMap<String, String>) -> ApiResult<Option<Vec<u8>>> {
    if h.is_empty() {
        return Ok(None);
    }
    let key = app.config.mcp.key.as_ref().ok_or_else(|| {
        ApiError::Unprocessable(
            "mcp_key_missing",
            "the hub keeps a server's headers only with HQC_MCP_KEY set".into(),
        )
    })?;
    Ok(Some(seal(key, id, h)))
}

/// `GET /api/mcp/servers`.
pub async fn list(
    State(app): State<AppState>,
    user: CurrentUser,
) -> ApiResult<Json<McpServerList>> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM mcp_servers WHERE owner_id = $1 ORDER BY name"
    ))
    .bind(user.id)
    .fetch_all(&app.db)
    .await?;
    let mut servers = vec![];
    for r in &rows {
        servers.push(to_api(&app.db, r).await?);
    }
    Ok(Json(McpServerList { servers }))
}

/// `POST /api/mcp/servers`: `name` and `url` required.
pub async fn create(
    State(app): State<AppState>,
    user: CurrentUser,
    Json(b): Json<McpServerRequest>,
) -> ApiResult<(StatusCode, Json<McpServer>)> {
    let name = b
        .name
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if !valid_server_name(&name) {
        return Err(bad(
            "name is 1 to 32 of a-z, 0-9, '-' and '_', not hqc, not starting or ending with '_' \
             nor holding '__'",
        ));
    }
    let url = check_url(b.url.as_deref().unwrap_or_default()).map_err(bad)?;
    let headers = check_headers(b.headers.as_deref().unwrap_or_default())?;
    let timeout = timeout_of(b.timeout_s)?;
    let projects = b.project_ids.clone().unwrap_or_default();
    check_projects(&app, user.id, &projects).await?;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM mcp_servers WHERE owner_id = $1")
        .bind(user.id)
        .fetch_one(&app.db)
        .await?;
    if n >= MAX_SERVERS {
        return Err(ApiError::Conflict(
            "too_many_servers",
            format!("at most {MAX_SERVERS} MCP servers"),
        ));
    }
    let id = db::new_id();
    let sealed = sealed_for(&app, id, &headers)?;
    let mut names: Vec<String> = headers.keys().cloned().collect();
    names.sort();
    let inserted = sqlx::query(
        "INSERT INTO mcp_servers (id, owner_id, name, url, header_names, sealed, timeout_s,
                                  all_projects)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT (owner_id, name) DO NOTHING",
    )
    .bind(id)
    .bind(user.id)
    .bind(&name)
    .bind(url.as_str())
    .bind(&names)
    .bind(sealed)
    .bind(timeout)
    .bind(b.all_projects.unwrap_or(true))
    .execute(&app.db)
    .await?;
    if inserted.rows_affected() == 0 {
        return Err(ApiError::Conflict(
            "name_taken",
            format!("you have an MCP server {name} already"),
        ));
    }
    set_projects(&app, id, &projects).await?;
    tracing::info!(server_id = %id, %name, "mcp server added");
    let r = own(&app, &user, id).await?;
    Ok((StatusCode::CREATED, Json(to_api(&app.db, &r).await?)))
}

/// `PATCH /api/mcp/servers/{id}`: the fields given; `headers` replaces them all.
pub async fn update(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<McpServerRequest>,
) -> ApiResult<Json<McpServer>> {
    let r = own(&app, &user, id).await?;
    let name = match &b.name {
        Some(n) => {
            let n = n.trim().to_string();
            if !valid_server_name(&n) {
                return Err(bad("name is 1 to 32 of a-z, 0-9, '-' and '_', not hqc"));
            }
            n
        }
        None => r.name.clone(),
    };
    let url = match &b.url {
        Some(u) => check_url(u).map_err(bad)?.to_string(),
        None => r.url.clone(),
    };
    let (names, sealed) = match &b.headers {
        Some(h) => {
            let h = check_headers(h)?;
            let mut names: Vec<String> = h.keys().cloned().collect();
            names.sort();
            (names, sealed_for(&app, id, &h)?)
        }
        None => (r.header_names.clone(), r.sealed.clone()),
    };
    let timeout = match b.timeout_s {
        Some(t) => timeout_of(Some(t))?,
        None => r.timeout_s,
    };
    if let Some(p) = &b.project_ids {
        check_projects(&app, user.id, p).await?;
    }
    // Another address or other credentials: what was cached and the sessions open are not its.
    let changed = url != r.url || b.headers.is_some();
    let updated = sqlx::query(
        "UPDATE mcp_servers SET name = $3, url = $4, header_names = $5, sealed = $6,
                                timeout_s = $7, all_projects = $8, updated_at = now(),
                                cached_at = CASE WHEN $9 THEN NULL ELSE cached_at END,
                                status = CASE WHEN $9 THEN 'unknown' ELSE status END,
                                last_error = CASE WHEN $9 THEN NULL ELSE last_error END
         WHERE id = $1 AND owner_id = $2
           AND NOT EXISTS (SELECT 1 FROM mcp_servers o
                           WHERE o.owner_id = $2 AND o.name = $3 AND o.id <> $1)",
    )
    .bind(id)
    .bind(user.id)
    .bind(&name)
    .bind(&url)
    .bind(&names)
    .bind(sealed)
    .bind(timeout)
    .bind(b.all_projects.unwrap_or(r.all_projects))
    .bind(changed)
    .execute(&app.db)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::Conflict(
            "name_taken",
            format!("you have an MCP server {name} already"),
        ));
    }
    if let Some(p) = &b.project_ids {
        set_projects(&app, id, p).await?;
    }
    if changed {
        forget_sessions(&app, id);
    }
    let r = own(&app, &user, id).await?;
    Ok(Json(to_api(&app.db, &r).await?))
}

fn forget_sessions(app: &AppState, server: Uuid) {
    app.mcp_upstream
        .sessions
        .lock()
        .unwrap()
        .retain(|k, _| k.1 != server);
}

/// `DELETE /api/mcp/servers/{id}`.
pub async fn delete(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    own(&app, &user, id).await?;
    sqlx::query("DELETE FROM mcp_servers WHERE id = $1 AND owner_id = $2")
        .bind(id)
        .bind(user.id)
        .execute(&app.db)
        .await?;
    forget_sessions(&app, id);
    tracing::info!(server_id = %id, "mcp server deleted");
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/mcp/servers/{id}/test`: `initialize` and `tools/list` now, in an MCP session of
/// its own that is closed after; the server as it is then.
pub async fn test(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<McpServer>> {
    let r = own(&app, &user, id).await?;
    let outcome: Result<(), UpErr> = async {
        let headers = headers_of(&app, &r).map_err(UpErr::Unavailable)?;
        // A session of its own, keyed by a fresh id: no agent's session is touched.
        let probe = Uuid::new_v4();
        let res = async {
            session(&app, &r, &headers, probe, true).await?;
            list_tools(&app, &r, &headers, probe).await?;
            let _ = sqlx::query("UPDATE mcp_servers SET cached_at = now() WHERE id = $1")
                .bind(r.id)
                .execute(&app.db)
                .await;
            Ok::<(), UpErr>(())
        }
        .await;
        process_stopped(&app, probe);
        res
    }
    .await;
    match outcome {
        Ok(()) => set_status(&app, r.id, None).await,
        Err(e) => {
            failed(&app, &r, e).await;
        }
    }
    let r = own(&app, &user, id).await?;
    Ok(Json(to_api(&app.db, &r).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_headers_open_only_with_the_key_and_the_id() {
        let key = [7u8; 32];
        let id = Uuid::new_v4();
        let h = HashMap::from([("Authorization".to_string(), "Bearer s3cret".to_string())]);
        let s = seal(&key, id, &h);
        assert!(!String::from_utf8_lossy(&s).contains("s3cret"));
        assert_eq!(unseal(&key, id, &s), Some(h));
        assert_eq!(unseal(&[8u8; 32], id, &s), None, "another key");
        assert_eq!(unseal(&key, Uuid::new_v4(), &s), None, "another server");
    }

    #[test]
    fn private_addresses() {
        for p in [
            "10.0.0.1",
            "192.168.1.229",
            "127.0.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.0.1",
        ] {
            assert!(is_private(p.parse().unwrap()), "{p}");
        }
        for p in ["8.8.8.8", "140.82.112.3", "2606:4700::1111"] {
            assert!(!is_private(p.parse().unwrap()), "{p}");
        }
    }

    #[test]
    fn urls() {
        assert!(check_url("https://api.githubcopilot.com/mcp/").is_ok());
        assert!(check_url("http://127.0.0.1:9000/mcp").is_ok());
        assert!(check_url("http://localhost/mcp").is_ok());
        assert!(check_url("http://example.com/mcp").is_err());
        assert!(check_url("ftp://example.com").is_err());
        assert!(check_url("https://user:pw@example.com").is_err());
    }

    #[test]
    fn sse_takes_the_answer_with_our_id() {
        let mut b = b"event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"ok\":true}}\n\n".to_vec();
        let v = sse_answer(&mut b, Some(7), &mut 0).unwrap();
        assert_eq!(v["result"]["ok"], true);
        let mut partial = b"data: {\"jsonrpc\":\"2.0\",\"id\":7,".to_vec();
        let mut scanned = 0;
        assert!(sse_answer(&mut partial, Some(7), &mut scanned).is_none());
        partial.extend_from_slice(b"\"result\":{}}\r\n\r\n");
        assert!(sse_answer(&mut partial, Some(7), &mut scanned).is_some());
    }
}
