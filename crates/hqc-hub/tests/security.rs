//! T9: protections before use (SPEC §20): Origin, security headers, limits of `/worker/*`
//! and of the WebSockets, push endpoints, the enrollment confirmation screen, the default
//! worker, a revoked worker's `hello`.

mod common;

use common::*;
use hqc_hub::config::Limits;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

const WS_PATH: &str = "/Users/home-mac/work/hqc";

/// A request with extra headers; `(status, json body, response headers)`.
async fn send(
    hub: &TestHub,
    method: reqwest::Method,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (u16, Value, reqwest::header::HeaderMap) {
    let mut r = hub.http.request(method, format!("{}{path}", hub.base));
    for (k, v) in headers {
        r = r.header(*k, *v);
    }
    if let Some(b) = body {
        r = r.json(&b);
    }
    let r = r.send().await.unwrap();
    let (s, h) = (r.status().as_u16(), r.headers().clone());
    (s, r.json().await.unwrap_or(Value::Null), h)
}

/// `/worker/ws` upgrade from `ip` (as Cloudflare reports it); `Err` carries the HTTP status.
async fn worker_ws_from(hub: &TestHub, token: &str, ip: &str) -> Result<(), String> {
    let mut req = format!("{}/worker/ws", hub.ws_base)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    req.headers_mut()
        .insert("CF-Connecting-IP", ip.parse().unwrap());
    tokio_tungstenite::connect_async(req)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tokio::test]
async fn origin_is_checked_on_api_ws_and_writes() {
    let hub = TestHub::start().await;
    // WebSocket: own origin ok, foreign or missing origin 403.
    let mut c = FakeClient::connect(&hub).await;
    c.send(json!({"type": "ping"})).await;
    assert_eq!(c.recv().await["type"], "pong");
    let e = FakeClient::connect_origin(&hub, None, Some("https://evil.example"))
        .await
        .err()
        .expect("foreign origin refused");
    assert!(e.contains("403"), "{e}");
    let e = FakeClient::connect_origin(&hub, None, None)
        .await
        .err()
        .expect("missing origin refused");
    assert!(e.contains("403"), "{e}");
    // Plain http is only an origin for loopback hosts.
    let e = FakeClient::connect_origin(&hub, None, Some("http://hqc.example.com"))
        .await
        .err()
        .expect("http origin of another host refused");
    assert!(e.contains("403"), "{e}");

    // A POST without a body (no CORS preflight): foreign origin 403, own or none passes.
    let sid = Uuid::now_v7();
    let stop = format!("/api/sessions/{sid}/stop");
    let post = reqwest::Method::POST;
    let (s, v, _) = send(
        &hub,
        post.clone(),
        &stop,
        &[("Origin", "https://evil.example")],
        None,
    )
    .await;
    assert_eq!((s, v["error"].as_str()), (403, Some("forbidden")));
    let own = hub.base.clone();
    let (s, _, _) = send(&hub, post.clone(), &stop, &[("Origin", &own)], None).await;
    assert_eq!(s, 404, "own origin reaches the handler");
    let (s, _, _) = send(&hub, post, &stop, &[], None).await;
    assert_eq!(s, 404, "no origin (not a browser) reaches the handler");
    // GET is not a write.
    let (s, _, _) = send(
        &hub,
        reqwest::Method::GET,
        "/api/me",
        &[("Origin", "https://evil.example")],
        None,
    )
    .await;
    assert_eq!(s, 200);
    // /worker/* is not /api/*.
    let (s, _, _) = send(
        &hub,
        reqwest::Method::POST,
        "/worker/enroll",
        &[("Origin", "https://evil.example")],
        Some(json!({"name": "w"})),
    )
    .await;
    assert_eq!(s, 200);
    hub.finish().await;
}

#[tokio::test]
async fn security_headers_on_every_response() {
    let hub = TestHub::start().await;
    let sid = Uuid::now_v7();
    for (method, path) in [
        (reqwest::Method::GET, "/health".to_string()),
        (reqwest::Method::GET, "/api/me".to_string()),
        (reqwest::Method::GET, format!("/api/sessions/{sid}")),
        (reqwest::Method::GET, "/no/such/file".to_string()),
        (reqwest::Method::POST, "/worker/enroll/poll".to_string()),
    ] {
        let (_, _, h) = send(&hub, method, &path, &[], None).await;
        let get = |k: &str| h.get(k).map(|v| v.to_str().unwrap().to_string());
        let csp = get("content-security-policy").unwrap_or_else(|| panic!("no CSP on {path}"));
        for d in [
            "default-src 'self'",
            "connect-src 'self' wss: https:",
            "img-src 'self' data:",
            "style-src 'self' 'unsafe-inline'",
            "object-src 'none'",
            "base-uri 'none'",
            "frame-ancestors 'none'",
        ] {
            assert!(csp.contains(d), "{path}: {csp} lacks {d}");
        }
        assert!(!csp.contains("script-src"), "scripts fall back to 'self'");
        assert_eq!(get("x-frame-options").as_deref(), Some("DENY"), "{path}");
        assert_eq!(
            get("x-content-type-options").as_deref(),
            Some("nosniff"),
            "{path}"
        );
        assert_eq!(
            get("referrer-policy").as_deref(),
            Some("no-referrer"),
            "{path}"
        );
    }
    hub.finish().await;
}

#[tokio::test]
async fn worker_routes_are_limited_per_address() {
    let hub = TestHub::start_limited(Limits {
        enroll_per_min: 3,
        enroll_active_per_ip: 2,
        poll_per_min: 4,
        ws_auth_failures_per_min: 2,
        ..Limits::default()
    })
    .await;
    let enroll = |ip: &'static str| {
        let hub = &hub;
        async move {
            send(
                hub,
                reqwest::Method::POST,
                "/worker/enroll",
                &[("CF-Connecting-IP", ip)],
                Some(json!({"name": "w", "os": "linux"})),
            )
            .await
            .0
        }
    };
    // Two live codes per address, then 429 although the minute is not used up.
    assert_eq!(enroll("203.0.113.1").await, 200);
    assert_eq!(enroll("203.0.113.1").await, 200);
    assert_eq!(enroll("203.0.113.1").await, 429);
    assert_eq!(
        enroll("203.0.113.1").await,
        429,
        "and the minute is used up"
    );
    // Another address is not affected.
    assert_eq!(enroll("203.0.113.2").await, 200);

    for i in 0..5 {
        let (s, _, _) = send(
            &hub,
            reqwest::Method::POST,
            "/worker/enroll/poll",
            &[("CF-Connecting-IP", "203.0.113.3")],
            Some(json!({"poll_token": "hqce_nope"})),
        )
        .await;
        assert_eq!(s, if i < 4 { 200 } else { 429 }, "poll {i}");
    }

    // Bad worker tokens: two per minute and address, then 429 even for a good token.
    let (_, token) = hub.enroll("home-mac").await;
    for bad in ["hqcw_wrong", &format!("hqcw_{}", "A".repeat(43))] {
        let e = worker_ws_from(&hub, bad, "203.0.113.4").await.unwrap_err();
        assert!(e.contains("401"), "{e}");
    }
    let e = worker_ws_from(&hub, &token, "203.0.113.4")
        .await
        .unwrap_err();
    assert!(e.contains("429"), "{e}");
    worker_ws_from(&hub, &token, "203.0.113.5")
        .await
        .expect("another address connects");

    // Token lookups in flight are bounded: with every permit taken, 503.
    let permits = hub
        .app
        .ws_auth
        .clone()
        .acquire_many_owned(Limits::default().ws_auth_concurrency as u32)
        .await
        .unwrap();
    let e = worker_ws_from(&hub, &token, "203.0.113.6")
        .await
        .unwrap_err();
    assert!(e.contains("503"), "{e}");
    drop(permits);
    worker_ws_from(&hub, &token, "203.0.113.6").await.unwrap();
    hub.finish().await;
}

#[tokio::test]
async fn hello_of_a_revoked_worker_is_refused() {
    let hub = TestHub::start().await;
    let (worker_id, token) = hub.enroll("home-mac").await;
    // The token passed the upgrade; the worker is revoked before its hello.
    let mut w = FakeWorker::connect(&hub, &token).await.unwrap();
    assert_eq!(hub.delete(&format!("/api/workers/{worker_id}")).await, 204);
    w.send(
        "hello",
        json!({"protocol_version": hqc_proto::PROTOCOL_VERSION, "boot_id": Uuid::new_v4(),
               "capabilities": {"workspace_path": WS_PATH}, "environments": ["default"],
               "env_bindings": [], "allowed_roots": [WS_PATH], "allow_foreign_sessions": false,
               "share_local_models": false, "local_models": []}),
    )
    .await;
    let f = w.expect("error").await;
    assert_eq!(f["payload"]["code"], "unauthorized");
    w.expect_closed().await;
    let (_, list) = hub.get("/api/workers").await;
    assert_eq!(list["workers"], json!([]));
    hub.finish().await;
}

#[tokio::test]
async fn field_and_message_limits() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let pid = hub.personal_project().await;
    let create = |body: Value| {
        let (hub, pid) = (&hub, &pid);
        async move {
            hub.post(&format!("/api/projects/{pid}/sessions"), body)
                .await
                .0
        }
    };
    assert_eq!(create(json!({"title": "x".repeat(201)})).await, 400);
    assert_eq!(create(json!({"model": "--resume"})).await, 400);
    assert_eq!(create(json!({"model": "a b"})).await, 400);
    assert_eq!(create(json!({"model": "m".repeat(65)})).await, 400);
    let (s, v) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": "ж".repeat(200), "model": "claude-haiku-5-5"}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    ack_start(&mut w, &sid).await;
    let (s, _) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "hi", "client_op_id": "o".repeat(129)}),
        )
        .await;
    assert_eq!(s, 400);
    let (s, _) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "hi", "client_op_id": "o".repeat(128)}),
        )
        .await;
    assert_eq!(s, 201);
    let r = hub
        .http
        .patch(format!("{}/api/sessions/{sid}", hub.base))
        .json(&json!({"title": "t".repeat(201)}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);

    // WebSocket messages over 1 MiB end the connection.
    let mut c = FakeClient::connect(&hub).await;
    c.send_unchecked(json!({"type": "ping", "pad": "x".repeat(1 << 20)}))
        .await;
    c.expect_closed().await;
    w.send_unchecked(json!({"id": "w-big", "type": "event",
                            "payload": {"event": {"pad": "x".repeat(1 << 20)}}}))
        .await;
    w.expect_closed().await;
    hub.finish().await;
}

#[tokio::test]
async fn push_endpoints_are_push_services_of_their_owner() {
    let hub = TestHub::start().await;
    let sub = |endpoint: &str| json!({"endpoint": endpoint, "keys": {"p256dh": "k", "auth": "a"}});
    for bad in [
        "https://attacker.example/redirect",
        "https://169.254.169.254/latest/meta-data",
        "http://fcm.googleapis.com/fcm/send/x",
    ] {
        let (s, v) = hub.post("/api/push/subscribe", sub(bad)).await;
        assert_eq!(s, 400, "{bad}: {v}");
    }
    for i in 0..10 {
        let (s, v) = hub
            .post(
                "/api/push/subscribe",
                sub(&format!("https://fcm.googleapis.com/fcm/send/{i}")),
            )
            .await;
        assert_eq!(s, 201, "{v}");
    }
    let (s, v) = hub
        .post(
            "/api/push/subscribe",
            sub("https://web.push.apple.com/eleventh"),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (409, Some("push_limit")));
    // Refreshing an own subscription is not a new one.
    let (s, _) = hub
        .post(
            "/api/push/subscribe",
            sub("https://fcm.googleapis.com/fcm/send/3"),
        )
        .await;
    assert_eq!(s, 201);

    // Another user's endpoint is not taken over.
    let other = hqc_hub::db::ensure_user(&hub.app.db, "other@example.com")
        .await
        .unwrap();
    let theirs = "https://updates.push.services.mozilla.com/wpush/v2/theirs";
    hqc_hub::db::upsert_push_sub(&hub.app.db, other.id, theirs, "k", "a", 10)
        .await
        .unwrap();
    // The user is at the limit; free a slot so that the conflict is what answers.
    let r = hub
        .http
        .delete(format!("{}/api/push/subscribe", hub.base))
        .json(&json!({"endpoint": "https://fcm.googleapis.com/fcm/send/0"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 204);
    let (s, v) = hub.post("/api/push/subscribe", sub(theirs)).await;
    assert_eq!((s, v["error"].as_str()), (409, Some("conflict")));
    let owner: Uuid =
        sqlx::query_scalar("SELECT user_id FROM push_subscriptions WHERE endpoint = $1")
            .bind(theirs)
            .fetch_one(&hub.app.db)
            .await
            .unwrap();
    assert_eq!(owner, other.id);
    hub.finish().await;
}

#[tokio::test]
async fn enrollment_shows_who_asks_before_confirming() {
    let hub = TestHub::start().await;
    let (s, e, _) = send(
        &hub,
        reqwest::Method::POST,
        "/worker/enroll",
        &[("CF-IPCountry", "de")],
        Some(json!({"name": "attacker-box", "os": "linux"})),
    )
    .await;
    assert_eq!(s, 200);
    let code = e["code"].as_str().unwrap();
    let (s, info) = hub
        .get(&format!(
            "/api/workers/enroll/{}",
            code.to_lowercase().replace('-', "")
        ))
        .await;
    assert_eq!(s, 200, "{info}");
    let info: hqc_proto::api::EnrollInfo = serde_json::from_value(info).unwrap();
    assert_eq!(
        (
            info.name.as_str(),
            info.os.as_str(),
            info.country.as_deref()
        ),
        ("attacker-box", "linux", Some("DE"))
    );
    assert!((chrono::Utc::now() - info.created_at).num_seconds() < 60);
    assert_eq!(hub.get("/api/workers/enroll/AAAA-AAAA").await.0, 404);
    let (s, _) = hub
        .post("/api/workers/enroll/confirm", json!({"code": code}))
        .await;
    assert_eq!(s, 200);
    assert_eq!(
        hub.get(&format!("/api/workers/enroll/{code}")).await.0,
        404,
        "a confirmed code is gone"
    );
    // Without CF-IPCountry there is no country.
    let (_, e) = hub.post("/worker/enroll", json!({"name": "w"})).await;
    let (_, info) = hub
        .get(&format!(
            "/api/workers/enroll/{}",
            e["code"].as_str().unwrap()
        ))
        .await;
    assert!(info.get("country").is_none(), "{info}");
    hub.finish().await;
}

#[tokio::test]
async fn a_fresh_worker_is_not_the_default() {
    let hub = TestHub::start().await;
    let pid = hub.personal_project().await;
    // Enrolled but never connected: no default worker at all.
    let (_, never) = hub.enroll("never-connected").await;
    let (s, v) = hub
        .post(&format!("/api/projects/{pid}/sessions"), json!({}))
        .await;
    assert_eq!((s, v["error"].as_str()), (409, Some("no_worker")));
    drop(never);

    let (home_id, home_token) = hub.enroll("home-mac").await;
    let (mut home, _) = FakeWorker::online(&hub, &home_token, WS_PATH).await;
    let sid = hub.create_session("first").await;
    ack_start(&mut home, &sid).await;
    assert_eq!(hub.session(&sid).await["worker_id"], home_id.as_str());

    // A worker enrolled later (say, through a phished code) connects and is online too:
    // new sessions still go to the worker the user has been using.
    let (_, fresh_token) = hub.enroll("fresh").await;
    let (_fresh, _) = FakeWorker::online(&hub, &fresh_token, "/tmp/fresh").await;
    let sid = hub.create_session("second").await;
    assert_eq!(hub.session(&sid).await["worker_id"], home_id.as_str());
    hub.finish().await;
}
