//! SPEC 0.235 (iteration 7, T64): remote MCP servers the owner keeps on the hub, relayed to the
//! agents' CLI through `mcp.upstream`. A fake MCP server speaks Streamable HTTP; a fake worker
//! sends what the relay would.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use common::*;
use serde_json::{json, Value};
use sha2::Digest;

const WS_PATH: &str = "/Users/home-mac/work/hqc";
const TOKEN: &str = "Bearer sekret-of-the-mcp-server";

#[derive(Default)]
struct Seen {
    auth: Vec<String>,
    sessions: Vec<String>,
    closed: Vec<String>,
    methods: Vec<String>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Seen>>);

async fn mcp(State(f): State<Fake>, h: HeaderMap, Json(m): Json<Value>) -> Response {
    let auth = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let sid = h
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let method = m["method"].as_str().unwrap_or("").to_string();
    {
        let mut s = f.0.lock().unwrap();
        s.auth.push(auth.clone());
        s.methods.push(method.clone());
        if let Some(id) = &sid {
            s.sessions.push(id.clone());
        }
    }
    if auth != TOKEN {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let id = m["id"].clone();
    match method.as_str() {
        "initialize" => {
            let n = f.0.lock().unwrap().methods.iter().filter(|m| *m == "initialize").count();
            (
                [("mcp-session-id", format!("s-{n}"))],
                Json(json!({"jsonrpc": "2.0", "id": id, "result": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fake", "version": "1.0"},
                    "instructions": "call ping first",
                }})),
            )
                .into_response()
        }
        "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
        "tools/list" => Json(json!({"jsonrpc": "2.0", "id": id, "result": {"tools": [
            {"name": "ping", "inputSchema": {"type": "object"}},
            {"name": "big", "inputSchema": {"type": "object"}},
        ]}}))
        .into_response(),
        "tools/call" => match m["params"]["name"].as_str() {
            // An SSE answer with a notification before it.
            Some("ping") => (
                [("content-type", "text/event-stream")],
                format!(
                    "event: message\ndata: {}\n\nevent: message\ndata: {}\n\n",
                    json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {"progress": 1}}),
                    json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": format!("pong {}", m["params"]["arguments"]["x"])}]}}),
                ),
            )
                .into_response(),
            Some("big") => Json(json!({"jsonrpc": "2.0", "id": id, "result": {
                "content": [{"type": "text", "text": "x".repeat(3 * 1024 * 1024)}],
            }}))
            .into_response(),
            _ => Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": "no such tool"}}))
                .into_response(),
        },
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn close(State(f): State<Fake>, h: HeaderMap) -> StatusCode {
    if let Some(id) = h.get("mcp-session-id").and_then(|v| v.to_str().ok()) {
        f.0.lock().unwrap().closed.push(id.to_string());
    }
    StatusCode::OK
}

async fn fake_server() -> (String, Fake) {
    let fake = Fake::default();
    let app = Router::new()
        .route("/mcp", post(mcp).delete(close))
        .with_state(fake.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (format!("http://{addr}/mcp"), fake)
}

/// Sends `mcp.upstream` and waits for its `mcp.result`.
async fn upstream(
    w: &mut FakeWorker,
    token: &str,
    server: &str,
    method: &str,
    params: Value,
) -> Value {
    let rid = uuid::Uuid::now_v7().to_string();
    w.send(
        "mcp.upstream",
        json!({"request_id": rid, "session_token": token, "server": server, "method": method, "params": params}),
    )
    .await;
    loop {
        let f = w.recv().await;
        if f["type"] == "mcp.result" && f["payload"]["request_id"] == rid.as_str() {
            return f["payload"].clone();
        }
    }
}

#[tokio::test]
async fn a_server_is_relayed_with_the_hubs_token_and_never_shown() {
    let hub = TestHub::start().await;
    let (url, fake) = fake_server().await;
    let (s, v) = hub
        .post(
            "/api/mcp/servers",
            json!({"name": "fake", "url": url, "headers": [{"name": "Authorization", "value": TOKEN}]}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["header_names"], json!(["Authorization"]));
    let (_, list) = hub.get("/api/mcp/servers").await;
    assert!(!list.to_string().contains("sekret"), "{list}");
    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mcp_servers t WHERE row_to_json(t)::text LIKE '%sekret%'",
    )
    .fetch_one(&hub.app.db)
    .await
    .unwrap();
    assert_eq!(rows, 0, "the value is sealed");

    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("with mcp").await;
    let start = ack_start(&mut w, &sid).await;
    assert_eq!(start["payload"]["mcp_servers"], json!(["fake"]));
    let st = start["payload"]["session_token"]
        .as_str()
        .unwrap()
        .to_string();

    let r = upstream(&mut w, &st, "fake", "initialize", json!({})).await;
    assert_eq!(r["result"]["upstream"]["serverInfo"]["name"], "fake", "{r}");
    assert_eq!(r["result"]["upstream"]["instructions"], "call ping first");
    let r = upstream(&mut w, &st, "fake", "tools/list", json!({})).await;
    let names: Vec<&str> = r["result"]["upstream"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["ping", "big"], "{r}");
    let r = upstream(
        &mut w,
        &st,
        "fake",
        "tools/call",
        json!({"name": "ping", "arguments": {"x": 1}}),
    )
    .await;
    assert_eq!(
        r["result"]["upstream"]["content"][0]["text"], "pong 1",
        "{r}"
    );
    {
        let s = fake.0.lock().unwrap();
        assert!(s.auth.iter().all(|a| a == TOKEN), "{:?}", s.auth);
        // One MCP session for the hqc session: the call went in the session `initialize` opened.
        assert!(s.sessions.iter().all(|x| x == "s-1"), "{:?}", s.sessions);
    }

    // A large result comes as an artifact the worker downloads, whole.
    let r = upstream(&mut w, &st, "fake", "tools/call", json!({"name": "big"})).await;
    let a = &r["result"]["upstream_artifact"];
    assert!(a["id"].is_string(), "{r}");
    let body = hub
        .http
        .get(format!(
            "{}/worker/artifacts/{}",
            hub.base,
            a["id"].as_str().unwrap()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(body.len() as u64, a["size"].as_u64().unwrap());
    let sha: String = sha2::Sha256::digest(&body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(sha, a["sha256"].as_str().unwrap());
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        v["content"][0]["text"].as_str().unwrap().len(),
        3 * 1024 * 1024
    );

    // The server's own error is a tool error.
    let r = upstream(&mut w, &st, "fake", "tools/call", json!({"name": "nope"})).await;
    assert_eq!(r["error"]["code"], "mcp_unavailable", "{r}");
    assert!(r["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no such tool"));

    // Another session's token, a server it does not have, a method not relayed.
    let r = upstream(
        &mut w,
        "hqcs_wrongwrongwrongwrongwrongwrongwrongwrongw",
        "fake",
        "tools/list",
        json!({}),
    )
    .await;
    assert_eq!(r["error"]["code"], "unauthorized", "{r}");
    let r = upstream(&mut w, &st, "other", "tools/list", json!({})).await;
    assert_eq!(r["error"]["code"], "not_found", "{r}");
    let r = upstream(&mut w, &st, "fake", "resources/list", json!({})).await;
    assert_eq!(r["error"]["code"], "invalid_args", "{r}");

    // The hub's own check: a restricted session gets nothing, whatever its worker sends.
    sqlx::query("UPDATE sessions SET restricted = true WHERE id = $1::uuid")
        .bind(&sid)
        .execute(&hub.app.db)
        .await
        .unwrap();
    let r = upstream(&mut w, &st, "fake", "tools/list", json!({})).await;
    assert_eq!(r["error"]["code"], "not_found", "{r}");
    sqlx::query("UPDATE sessions SET restricted = false WHERE id = $1::uuid")
        .bind(&sid)
        .execute(&hub.app.db)
        .await
        .unwrap();

    // The agent's process stops: its MCP session is closed at the server.
    w.send(
        "session.state",
        json!({"session_id": sid, "process_state": "stopped", "locked_by": "hqc"}),
    )
    .await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while fake.0.lock().unwrap().closed.is_empty() {
        assert!(std::time::Instant::now() < deadline, "no DELETE");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(fake.0.lock().unwrap().closed, ["s-1"]);
    hub.finish().await;
}

#[tokio::test]
async fn a_server_of_other_projects_is_not_given() {
    let hub = TestHub::start().await;
    let (url, _fake) = fake_server().await;
    let (s, p) = hub.post("/api/projects", json!({"name": "Other"})).await;
    assert_eq!(s, 201, "{p}");
    let (s, v) = hub
        .post(
            "/api/mcp/servers",
            json!({"name": "fake", "url": url, "headers": [{"name": "Authorization", "value": TOKEN}],
                   "all_projects": false, "project_ids": [p["id"]]}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("personal").await;
    let start = ack_start(&mut w, &sid).await;
    assert!(start["payload"].get("mcp_servers").is_none(), "{start}");
    let st = start["payload"]["session_token"]
        .as_str()
        .unwrap()
        .to_string();
    let r = upstream(&mut w, &st, "fake", "tools/list", json!({})).await;
    assert_eq!(r["error"]["code"], "not_found", "{r}");
    hub.finish().await;
}

#[tokio::test]
async fn what_the_owner_gives_is_checked() {
    let hub = TestHub::start().await;
    let (url, fake) = fake_server().await;
    for (body, code) in [
        (json!({"name": "hqc", "url": url}), 400),
        (json!({"name": "a__b", "url": url}), 400),
        (json!({"name": "ok", "url": "http://example.com/mcp"}), 400),
        (
            json!({"name": "ok", "url": url, "headers": [{"name": "Host", "value": "x"}]}),
            400,
        ),
        (
            json!({"name": "ok", "url": url, "headers": [{"name": "A", "value": "x\ny"}]}),
            400,
        ),
        (json!({"name": "ok", "url": url, "timeout_s": 5}), 400),
        (json!({"name": "ok", "url": url, "extra": 1}), 422),
    ] {
        let (s, v) = hub.post("/api/mcp/servers", body.clone()).await;
        assert_eq!(s, code, "{body} -> {v}");
    }
    let (s, v) = hub
        .post(
            "/api/mcp/servers",
            json!({"name": "fake", "url": url, "headers": [{"name": "Authorization", "value": "Bearer wrong"}]}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    let (s, _) = hub
        .post("/api/mcp/servers", json!({"name": "fake", "url": url}))
        .await;
    assert_eq!(s, 409, "the name is taken");
    // Test: refused credentials say so.
    let (s, v) = hub
        .post(&format!("/api/mcp/servers/{id}/test"), json!({}))
        .await;
    assert_eq!((s, v["status"].as_str()), (200, Some("failing")), "{v}");
    assert!(v["last_error"].as_str().unwrap().contains("refused"), "{v}");
    // New headers: the test passes and lists the tools.
    let (s, v) = hub
        .patch(
            &format!("/api/mcp/servers/{id}"),
            json!({"headers": [{"name": "Authorization", "value": TOKEN}], "timeout_s": 60}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = hub
        .post(&format!("/api/mcp/servers/{id}/test"), json!({}))
        .await;
    assert_eq!((s, v["status"].as_str()), (200, Some("ok")), "{v}");
    assert_eq!(v["tools"], json!(["ping", "big"]));
    assert_eq!(v["instructions"], "call ping first");
    // The test's MCP session is closed after it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while fake.0.lock().unwrap().closed.is_empty() {
        assert!(std::time::Instant::now() < deadline, "no DELETE");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(hub.delete(&format!("/api/mcp/servers/{id}")).await, 204);
    let (_, list) = hub.get("/api/mcp/servers").await;
    assert_eq!(list["servers"], json!([]));
    hub.finish().await;
}

#[tokio::test]
async fn without_a_key_no_headers_and_private_addresses_only_when_allowed() {
    let hub = TestHub::start_config(|c| {
        c.mcp.key = None;
        c.mcp.allow_private = false;
    })
    .await;
    let (url, _fake) = fake_server().await;
    let (s, v) = hub
        .post(
            "/api/mcp/servers",
            json!({"name": "fake", "url": url, "headers": [{"name": "Authorization", "value": TOKEN}]}),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (422, Some("mcp_key_missing")),
        "{v}"
    );
    let (s, v) = hub
        .post("/api/mcp/servers", json!({"name": "open", "url": url}))
        .await;
    assert_eq!(s, 201, "{v}");
    let id = v["id"].as_str().unwrap();
    let (_, v) = hub
        .post(&format!("/api/mcp/servers/{id}/test"), json!({}))
        .await;
    assert_eq!(v["status"], "failing", "{v}");
    assert!(
        v["last_error"]
            .as_str()
            .unwrap()
            .contains("HQC_MCP_ALLOW_PRIVATE"),
        "{v}"
    );
    hub.finish().await;
}

// ---------------------------------------------------------------- OAuth (T65)

#[derive(Default)]
struct AuthSeen {
    registered: usize,
    challenge: Option<String>,
    refreshed_with: Vec<String>,
    bearers: Vec<String>,
}

#[derive(Clone)]
struct AuthFake {
    base: Arc<Mutex<String>>,
    seen: Arc<Mutex<AuthSeen>>,
}

async fn auth_mcp(State(f): State<AuthFake>, h: HeaderMap, Json(m): Json<Value>) -> Response {
    let bearer = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_string();
    f.seen.lock().unwrap().bearers.push(bearer.clone());
    if bearer != "access-2" {
        let base = f.base.lock().unwrap().clone();
        return (
            StatusCode::UNAUTHORIZED,
            [(
                "www-authenticate",
                format!(
                    "Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\""
                ),
            )],
        )
            .into_response();
    }
    let id = m["id"].clone();
    match m["method"].as_str() {
        Some("initialize") => Json(json!({"jsonrpc": "2.0", "id": id, "result": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "serverInfo": {"name": "linear-like", "version": "1"}}}))
        .into_response(),
        Some("notifications/initialized") => StatusCode::ACCEPTED.into_response(),
        Some("tools/list") => Json(json!({"jsonrpc": "2.0", "id": id, "result": {"tools": [{"name": "issues", "inputSchema": {"type": "object"}}]}})).into_response(),
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn prm(State(f): State<AuthFake>) -> Json<Value> {
    let base = f.base.lock().unwrap().clone();
    Json(
        json!({"resource": format!("{base}/mcp"), "authorization_servers": [base], "scopes_supported": ["read"]}),
    )
}

async fn as_meta(State(f): State<AuthFake>) -> Json<Value> {
    let b = f.base.lock().unwrap().clone();
    Json(
        json!({"issuer": b, "authorization_endpoint": format!("{b}/authorize"),
                "token_endpoint": format!("{b}/token"), "registration_endpoint": format!("{b}/register"),
                "code_challenge_methods_supported": ["S256"]}),
    )
}

async fn register(State(f): State<AuthFake>, Json(m): Json<Value>) -> (StatusCode, Json<Value>) {
    f.seen.lock().unwrap().registered += 1;
    assert!(m["redirect_uris"][0]
        .as_str()
        .unwrap()
        .ends_with("/api/mcp/oauth/callback"));
    (StatusCode::CREATED, Json(json!({"client_id": "client-1"})))
}

async fn authorize(
    State(f): State<AuthFake>,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> Response {
    assert_eq!(q["client_id"], "client-1");
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["scope"], "read");
    f.seen.lock().unwrap().challenge = Some(q["code_challenge"].clone());
    let to = format!("{}?code=code-1&state={}", q["redirect_uri"], q["state"]);
    (StatusCode::FOUND, [("location", to)]).into_response()
}

async fn token(State(f): State<AuthFake>, body: String) -> Response {
    let q: HashMap<String, String> = body
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), urlencoding_decode(v)))
        .collect();
    match q["grant_type"].as_str() {
        "authorization_code" => {
            let want = f.seen.lock().unwrap().challenge.clone().unwrap();
            let got = base64_url(&sha2::Sha256::digest(q["code_verifier"].as_bytes()));
            if got != want || q["code"] != "code-1" {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "invalid_grant"})),
                )
                    .into_response();
            }
            // Expires at once: the first call refreshes it.
            Json(json!({"access_token": "access-1", "refresh_token": "refresh-1", "expires_in": 1}))
                .into_response()
        }
        "refresh_token" => {
            f.seen
                .lock()
                .unwrap()
                .refreshed_with
                .push(q["refresh_token"].clone());
            if q["refresh_token"] != "refresh-1" {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "invalid_grant"})),
                )
                    .into_response();
            }
            Json(json!({"access_token": "access-2", "refresh_token": "refresh-2", "expires_in": 3600})).into_response()
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

fn urlencoding_decode(v: &str) -> String {
    let v = v.replace('+', " ");
    let b = v.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            out.push(u8::from_str_radix(&v[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

fn base64_url(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

#[tokio::test]
async fn oauth_signs_in_with_pkce_and_refreshes_the_token() {
    let hub = TestHub::start().await;
    let f = AuthFake {
        base: Arc::default(),
        seen: Arc::default(),
    };
    let app = Router::new()
        .route("/mcp", post(auth_mcp))
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            axum::routing::get(prm),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            axum::routing::get(as_meta),
        )
        .route("/register", post(register))
        .route("/authorize", axum::routing::get(authorize))
        .route("/token", post(token))
        .with_state(f.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    *f.base.lock().unwrap() = base.clone();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

    let (s, v) = hub
        .post(
            "/api/mcp/servers",
            json!({"name": "linear", "url": format!("{base}/mcp"), "auth": "oauth"}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(
        (
            v["auth"].as_str(),
            v["signed_in"].as_bool(),
            v["status"].as_str()
        ),
        (Some("oauth"), Some(false), Some("sign_in"))
    );
    let id = v["id"].as_str().unwrap().to_string();

    let (s, v) = hub
        .post(&format!("/api/mcp/servers/{id}/sign_in"), json!({}))
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        f.seen.lock().unwrap().registered,
        1,
        "dynamic client registration"
    );
    // The browser: the authorization server sends it back to the hub's callback.
    let no_redirect = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let r = no_redirect
        .get(v["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    let back = r.headers()["location"].to_str().unwrap().to_string();
    let path = back.splitn(4, '/').nth(3).unwrap();
    let page = hub
        .http
        .get(format!("{}/{path}", hub.base))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("Signed in to linear"), "{page}");
    // The same state again is refused.
    let again = hub
        .http
        .get(format!("{}/{path}", hub.base))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(again.contains("unknown or expired"), "{again}");
    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mcp_servers t WHERE row_to_json(t)::text LIKE '%access-1%' OR row_to_json(t)::text LIKE '%refresh-1%'",
    )
    .fetch_one(&hub.app.db)
    .await
    .unwrap();
    assert_eq!(rows, 0, "the tokens are sealed");

    // The first use refreshes the expired token; the rotated refresh token is kept.
    let (s, v) = hub
        .post(&format!("/api/mcp/servers/{id}/test"), json!({}))
        .await;
    assert_eq!((s, v["status"].as_str()), (200, Some("ok")), "{v}");
    assert_eq!(v["tools"], json!(["issues"]));
    assert_eq!(v["signed_in"], true);
    assert_eq!(f.seen.lock().unwrap().refreshed_with, ["refresh-1"]);
    assert!(f
        .seen
        .lock()
        .unwrap()
        .bearers
        .iter()
        .any(|b| b == "access-2"));

    // Signed out: the next use asks to sign in again.
    assert_eq!(
        hub.delete(&format!("/api/mcp/servers/{id}/sign_in")).await,
        204
    );
    let (_, v) = hub
        .post(&format!("/api/mcp/servers/{id}/test"), json!({}))
        .await;
    assert_eq!(v["signed_in"], false, "{v}");
    assert!(v["last_error"].as_str().unwrap().contains("sign in"), "{v}");
    hub.finish().await;
}

// ---------------------------------------------------------------- probes (SPEC 0.236)

async fn open_server() -> String {
    // Answers without credentials and names itself.
    async fn h(Json(m): Json<Value>) -> Response {
        match m["method"].as_str() {
            Some("initialize") => (
                [("mcp-session-id", "probe-1")],
                Json(json!({"jsonrpc": "2.0", "id": m["id"], "result": {
                    "protocolVersion": "2025-06-18", "capabilities": {},
                    "serverInfo": {"name": "context7-mcp", "version": "1"}}})),
            )
                .into_response(),
            _ => StatusCode::ACCEPTED.into_response(),
        }
    }
    let app = Router::new().route("/mcp", post(h).delete(|| async { StatusCode::OK }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}/mcp")
}

#[tokio::test]
async fn a_server_url_is_probed_for_its_name_and_sign_in() {
    let hub = TestHub::start().await;
    let open = open_server().await;
    let (s, v) = hub.post("/api/mcp/probe", json!({"url": open})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (v["auth"].as_str(), v["name"].as_str()),
        (Some("none"), Some("context7")),
        "{v}"
    );
    // A server that wants a token and names no authorization server.
    let (token_url, _) = fake_server().await;
    let (_, v) = hub.post("/api/mcp/probe", json!({"url": token_url})).await;
    assert_eq!(v["auth"], "token", "{v}");
    let (s, _) = hub.post("/api/mcp/probe", json!({"url": "ftp://x"})).await;
    assert_eq!(s, 400);
    hub.finish().await;
}

#[tokio::test]
async fn a_provider_url_is_probed_by_a_worker() {
    let hub = TestHub::start().await;
    let (s, v) = hub
        .post(
            "/api/providers/probe",
            json!({"base_url": "http://192.168.1.229:8082"}),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some("worker_offline")),
        "{v}"
    );
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let ((s, v), f) = while_answering(
        &mut w,
        "provider.probe",
        json!({"provider_probe": {"models": [{"id": "qwen3.8-27b", "context_tokens": 131072}, {"id": "qwen3.8-flash-next"}]}}),
        hub.post("/api/providers/probe", json!({"base_url": "http://192.168.1.229:8082/"})),
    )
    .await;
    assert_eq!(
        f["payload"]["base_url"], "http://192.168.1.229:8082",
        "trailing / dropped"
    );
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["models"][0]["id"], "qwen3.8-27b");
    assert_eq!(v["models"][0]["context_tokens"], 131072);
    assert!(v["worker_id"].is_string());
    hub.finish().await;
}

#[tokio::test]
async fn a_refused_registration_says_its_status_and_type() {
    let hub = TestHub::start().await;
    let f = AuthFake {
        base: Arc::default(),
        seen: Arc::default(),
    };
    let app = Router::new()
        .route("/mcp", post(auth_mcp))
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            axum::routing::get(prm),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            axum::routing::get(as_meta),
        )
        .route(
            "/register",
            post(|| async {
                (
                    StatusCode::FORBIDDEN,
                    [("content-type", "text/html; charset=utf-8")],
                    "<html>Forbidden</html>",
                )
            }),
        )
        .with_state(f.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    *f.base.lock().unwrap() = base.clone();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

    let (s, v) = hub
        .post(
            "/api/mcp/servers",
            json!({"name": "figma", "url": format!("{base}/mcp"), "auth": "oauth"}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    let (s, v) = hub
        .post(&format!("/api/mcp/servers/{id}/sign_in"), json!({}))
        .await;
    assert_ne!(s, 200, "{v}");
    let text = v.to_string();
    assert!(
        text.contains("the registration answered 403 (text/html), not JSON"),
        "{text}"
    );
    assert!(
        !text.contains("Forbidden</html>"),
        "no text of the page: {text}"
    );
    hub.finish().await;
}
