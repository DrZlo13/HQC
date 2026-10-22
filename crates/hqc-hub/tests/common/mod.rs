//! Test harness: a hub on a random port over an isolated Postgres schema, a fake worker and
//! a fake client speaking the real protocols.
//!
//! Database: `HQC_TEST_DATABASE_URL` (default `postgresql://localhost/hqc_test`). Each hub
//! gets its own schema, dropped by `TestHub::finish`.

#![allow(dead_code)]

use std::{
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use hqc_hub::{
    config::{AuthConfig, Limits, Timing},
    db::{PushSubRow, MIGRATOR},
    push::{BoxFuture, PushError, PushSender},
    AppState, Config,
};
use serde_json::{json, Value};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    PgPool,
};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    tungstenite::{client::IntoClientRequest, Message},
    MaybeTlsStream, WebSocketStream,
};
use uuid::Uuid;

pub const WAIT: Duration = Duration::from_secs(5);

pub fn db_url() -> String {
    std::env::var("HQC_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgresql://localhost/hqc_test".into())
}

/// Records pushes instead of sending them.
#[derive(Default)]
pub struct RecordingPush {
    pub sent: Mutex<Vec<(String, Value)>>,
}

impl PushSender for RecordingPush {
    fn public_key(&self) -> Option<String> {
        Some("test-vapid-public-key".into())
    }
    fn send<'a>(
        &'a self,
        sub: &'a PushSubRow,
        payload: &'a [u8],
    ) -> BoxFuture<'a, Result<(), PushError>> {
        let v: Value = serde_json::from_slice(payload).unwrap();
        self.sent.lock().unwrap().push((sub.endpoint.clone(), v));
        Box::pin(async { Ok(()) })
    }
}

pub struct TestHub {
    pub app: AppState,
    pub base: String,
    pub ws_base: String,
    pub http: reqwest::Client,
    pub push: Arc<RecordingPush>,
    pub email: String,
    /// `boot_id` of [`FakeWorker::online`].
    pub boot_id: Uuid,
    schema: String,
    admin: PgPool,
}

pub fn test_timing() -> Timing {
    Timing {
        heartbeat: Duration::from_secs(20),
        worker_idle_timeout: Duration::from_secs(60),
        start_retry_delay: Duration::from_millis(30),
        max_start_failures: 3,
        expiry_interval: Duration::from_millis(100),
        enroll_ttl: Duration::from_secs(600),
        ask_timeout_s: 600,
        turn_watchdog: None,
    }
}

impl TestHub {
    /// Dev auth with a fresh user.
    pub async fn start() -> Self {
        let email = format!("user-{}@example.com", Uuid::new_v4().simple());
        Self::start_with(
            AuthConfig::Dev {
                email: email.clone(),
            },
            email,
        )
        .await
    }

    pub async fn start_with(auth: AuthConfig, email: String) -> Self {
        Self::start_full(auth, email, Limits::default()).await
    }

    /// Dev auth with a fresh user and the given limits.
    pub async fn start_limited(limits: Limits) -> Self {
        let email = format!("user-{}@example.com", Uuid::new_v4().simple());
        Self::start_full(
            AuthConfig::Dev {
                email: email.clone(),
            },
            email,
            limits,
        )
        .await
    }

    pub async fn start_full(auth: AuthConfig, email: String, limits: Limits) -> Self {
        Self::start_timed(auth, email, limits, test_timing()).await
    }

    /// Dev auth with a fresh user and the given timing.
    pub async fn start_timing(timing: Timing) -> Self {
        let email = format!("user-{}@example.com", Uuid::new_v4().simple());
        Self::start_timed(
            AuthConfig::Dev {
                email: email.clone(),
            },
            email,
            Limits::default(),
            timing,
        )
        .await
    }

    pub async fn start_timed(
        auth: AuthConfig,
        email: String,
        limits: Limits,
        timing: Timing,
    ) -> Self {
        let url = db_url();
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap_or_else(|e| panic!("connect to {url} (HQC_TEST_DATABASE_URL): {e}"));
        let schema = format!("t_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let opts = PgConnectOptions::from_str(&url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await
            .unwrap();
        MIGRATOR.run(&pool).await.unwrap();

        let config = Config {
            database_url: url,
            listen: "127.0.0.1:0".into(),
            auth,
            vapid: None,
            client_dir: None,
            timing,
            limits,
        };
        let push = Arc::new(RecordingPush::default());
        let app = hqc_hub::start(pool, config, push.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let service = hqc_hub::service(app.clone());
        tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
        TestHub {
            app,
            base: format!("http://{addr}"),
            ws_base: format!("ws://{addr}"),
            http: reqwest::Client::new(),
            push,
            email,
            boot_id: Uuid::new_v4(),
            schema,
            admin,
        }
    }

    pub async fn finish(self) {
        self.app.db.close().await;
        let _ = sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await;
    }

    pub async fn get(&self, path: &str) -> (u16, Value) {
        let r = self
            .http
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    pub async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self
            .http
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .await
            .unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    pub async fn put(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self
            .http
            .put(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .await
            .unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    pub async fn patch(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self
            .http
            .patch(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .await
            .unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    pub async fn delete(&self, path: &str) -> u16 {
        self.http
            .delete(format!("{}{path}", self.base))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    pub async fn personal_project(&self) -> String {
        let (s, me) = self.get("/api/me").await;
        assert_eq!(s, 200, "{me}");
        me["personal_project_id"].as_str().unwrap().to_string()
    }

    /// Full enrollment (SPEC §5.1): code from the worker, confirmation by the user, token by poll.
    pub async fn enroll(&self, name: &str) -> (String, String) {
        let (s, e) = self
            .post(
                "/worker/enroll",
                // Unknown fields are ignored (SPEC §5.1).
                json!({"name": name, "os": "macos", "protocol_version": hqc_proto::PROTOCOL_VERSION,
                       "arch": "arm64"}),
            )
            .await;
        assert_eq!(s, 200, "{e}");
        let (s, p) = self
            .post(
                "/worker/enroll/poll",
                json!({"poll_token": e["poll_token"]}),
            )
            .await;
        assert_eq!((s, p["status"].as_str()), (200, Some("pending")));
        let (s, c) = self
            .post("/api/workers/enroll/confirm", json!({"code": e["code"]}))
            .await;
        assert_eq!(s, 200, "{c}");
        let (s, p) = self
            .post(
                "/worker/enroll/poll",
                json!({"poll_token": e["poll_token"]}),
            )
            .await;
        assert_eq!((s, p["status"].as_str()), (200, Some("confirmed")), "{p}");
        assert_eq!(p["worker_id"], c["worker_id"]);
        (
            p["worker_id"].as_str().unwrap().to_string(),
            p["token"].as_str().unwrap().to_string(),
        )
    }

    pub async fn create_session(&self, title: &str) -> String {
        let pid = self.personal_project().await;
        let (s, v) = self
            .post(
                &format!("/api/projects/{pid}/sessions"),
                json!({"title": title}),
            )
            .await;
        assert_eq!(s, 201, "{v}");
        v["id"].as_str().unwrap().to_string()
    }

    pub async fn session(&self, id: &str) -> Value {
        let (s, v) = self.get(&format!("/api/sessions/{id}")).await;
        assert_eq!(s, 200, "{v}");
        v
    }

    pub async fn events(&self, id: &str) -> Vec<Value> {
        let (s, v) = self.get(&format!("/api/sessions/{id}/events")).await;
        assert_eq!(s, 200, "{v}");
        v["events"].as_array().unwrap().clone()
    }

    /// Poll until `f` holds for the session view.
    pub async fn wait_session(&self, id: &str, what: &str, f: impl Fn(&Value) -> bool) -> Value {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let v = self.session(id).await;
            if f(&v) {
                return v;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timeout waiting for {what}: {v}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn recv_json(ws: &mut Ws, timeout: Duration) -> Option<Value> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let m = tokio::time::timeout_at(deadline, ws.next()).await.ok()??;
        match m.expect("websocket error") {
            Message::Text(t) => return Some(serde_json::from_str(t.as_str()).unwrap()),
            Message::Close(_) => return None,
            _ => continue,
        }
    }
}

/// A worker speaking SPEC §5 by hand.
pub struct FakeWorker {
    ws: Ws,
    next: u64,
}

impl FakeWorker {
    pub async fn connect(hub: &TestHub, token: &str) -> Result<Self, String> {
        let mut req = format!("{}/worker/ws", hub.ws_base)
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("Authorization", format!("Bearer {token}").parse().unwrap());
        match tokio_tungstenite::connect_async(req).await {
            Ok((ws, _)) => Ok(Self { ws, next: 1 }),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Connect and complete `hello` announcing `workspace_path`, with the hub's
    /// [`TestHub::boot_id`]: every `online` of a test is the same worker process
    /// reconnecting. Returns `hello.ack`.
    pub async fn online(hub: &TestHub, token: &str, workspace: &str) -> (Self, Value) {
        Self::online_boot(hub, token, workspace, hub.boot_id).await
    }

    /// `online` as a worker process with its own `boot_id` (a new value = a restart).
    pub async fn online_boot(
        hub: &TestHub,
        token: &str,
        workspace: &str,
        boot_id: Uuid,
    ) -> (Self, Value) {
        let mut w = Self::connect(hub, token).await.expect("worker connect");
        let ack = w.hello(workspace, boot_id).await;
        (w, ack)
    }

    pub async fn hello(&mut self, workspace: &str, boot_id: Uuid) -> Value {
        let id = self
            .send(
                "hello",
                json!({
                    "protocol_version": hqc_proto::PROTOCOL_VERSION,
                    "boot_id": boot_id,
                    "capabilities": {"claude_code": {"version": "2.1.307"}, "acp_agents": [],
                                     "workspace_path": workspace},
                    "environments": ["default"],
                    "env_bindings": [],
                    "allowed_roots": [workspace],
                    "allow_foreign_sessions": false,
                    "share_local_models": false,
                    "local_models": []
                }),
            )
            .await;
        let ack = self.recv().await;
        assert_eq!(ack["type"], "hello.ack", "{ack}");
        assert_eq!(ack["id"], id);
        ack
    }

    pub async fn send(&mut self, ty: &str, payload: Value) -> String {
        let id = format!("w-{}", self.next);
        self.next += 1;
        self.send_raw(json!({"id": id, "type": ty, "payload": payload}))
            .await;
        id
    }

    pub async fn send_raw(&mut self, v: Value) {
        self.ws
            .send(Message::Text(v.to_string().into()))
            .await
            .unwrap();
    }

    /// Send that may fail: the hub may close the connection while this is still writing.
    pub async fn send_unchecked(&mut self, v: Value) {
        let _ = self.ws.send(Message::Text(v.to_string().into())).await;
    }

    pub async fn reply(&mut self, id: &Value, ty: &str, payload: Value) {
        self.send_raw(json!({"id": id, "type": ty, "payload": payload}))
            .await;
    }

    pub async fn recv(&mut self) -> Value {
        recv_json(&mut self.ws, WAIT)
            .await
            .expect("worker: no frame from hub")
    }

    /// The next frame, which must have type `ty`.
    pub async fn expect(&mut self, ty: &str) -> Value {
        let f = self.recv().await;
        assert_eq!(f["type"], ty, "unexpected frame {f}");
        f
    }

    pub async fn expect_silence(&mut self, d: Duration) {
        if let Some(f) = recv_json(&mut self.ws, d).await {
            panic!("worker: unexpected frame {f}");
        }
    }

    /// Wait until the hub closes the connection.
    pub async fn expect_closed(&mut self) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            match tokio::time::timeout_at(deadline, self.ws.next()).await {
                Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_)))) => return,
                Ok(Some(Ok(_))) => continue,
                Err(_) => panic!("worker connection not closed"),
            }
        }
    }

    /// Send an event frame; returns the hub's `ack` payload.
    pub async fn event(&mut self, event: Value) -> Value {
        let id = self.send("event", json!({ "event": event })).await;
        let ack = self.expect("ack").await;
        assert_eq!(ack["id"], id);
        ack["payload"].clone()
    }

    pub async fn close(mut self) {
        let _ = self.ws.close(None).await;
    }
}

/// Answer `session.start` with ack.
pub async fn ack_start(w: &mut FakeWorker, session_id: &str) -> Value {
    let f = w.expect("session.start").await;
    assert_eq!(f["payload"]["session_id"], session_id);
    w.reply(&f["id"], "ack", json!({})).await;
    f
}

pub fn agent_event(session_id: &str, ty: &str, payload: Value) -> Value {
    json!({
        "session_id": session_id,
        "event_id": Uuid::now_v7(),
        "ts": "2026-10-22T10:00:05Z",
        "author": {"kind": "agent", "id": session_id},
        "type": ty,
        "payload": payload,
    })
}

/// A client on `/api/ws`.
pub struct FakeClient {
    ws: Ws,
}

impl FakeClient {
    pub async fn connect(hub: &TestHub) -> Self {
        Self::connect_with(hub, None).await.expect("client connect")
    }

    /// With the hub's own `Origin`, as a browser on the hub's page sends it (SPEC §20).
    pub async fn connect_with(hub: &TestHub, jwt: Option<&str>) -> Result<Self, String> {
        Self::connect_origin(hub, jwt, Some(&hub.base)).await
    }

    pub async fn connect_origin(
        hub: &TestHub,
        jwt: Option<&str>,
        origin: Option<&str>,
    ) -> Result<Self, String> {
        let mut req = format!("{}/api/ws", hub.ws_base)
            .into_client_request()
            .unwrap();
        if let Some(t) = jwt {
            req.headers_mut()
                .insert("Cf-Access-Jwt-Assertion", t.parse().unwrap());
        }
        if let Some(o) = origin {
            req.headers_mut().insert("Origin", o.parse().unwrap());
        }
        match tokio_tungstenite::connect_async(req).await {
            Ok((ws, _)) => Ok(Self { ws }),
            Err(e) => Err(e.to_string()),
        }
    }

    pub async fn send(&mut self, v: Value) {
        self.ws
            .send(Message::Text(v.to_string().into()))
            .await
            .unwrap();
    }

    pub async fn recv(&mut self) -> Value {
        recv_json(&mut self.ws, WAIT)
            .await
            .expect("client: no frame from hub")
    }

    /// Send that may fail: the hub may close the connection while this is still writing.
    pub async fn send_unchecked(&mut self, v: Value) {
        let _ = self.ws.send(Message::Text(v.to_string().into())).await;
    }

    /// Skip frames until one with type `ty`.
    pub async fn until(&mut self, ty: &str) -> Value {
        loop {
            let f = self.recv().await;
            if f["type"] == ty {
                return f;
            }
        }
    }

    /// Subscribe and collect the backlog up to `subscribed`.
    pub async fn subscribe(&mut self, session_id: &str, after_seq: i64) -> Vec<Value> {
        self.send(json!({"type": "subscribe", "session_id": session_id, "after_seq": after_seq}))
            .await;
        let mut backlog = vec![];
        loop {
            let f = self.recv().await;
            match f["type"].as_str() {
                Some("event") => backlog.push(f["event"].clone()),
                Some("subscribed") => return backlog,
                _ => panic!("unexpected frame during subscribe: {f}"),
            }
        }
    }

    pub async fn expect_silence(&mut self, d: Duration) {
        if let Some(f) = recv_json(&mut self.ws, d).await {
            panic!("client: unexpected frame {f}");
        }
    }

    /// Wait until the hub closes the connection.
    pub async fn expect_closed(&mut self) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            match tokio::time::timeout_at(deadline, self.ws.next()).await {
                Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_)))) => return,
                Ok(Some(Ok(_))) => continue,
                Err(_) => panic!("client connection not closed"),
            }
        }
    }
}
