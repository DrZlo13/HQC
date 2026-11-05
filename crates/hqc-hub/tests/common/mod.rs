//! Test harness: a hub on a random port over an isolated Postgres schema, a fake worker and
//! a fake client speaking the real protocols.
//!
//! Database: `HQC_TEST_DATABASE_URL` (default `postgresql://localhost/hqc_test`). Each hub
//! gets its own schema, dropped by `TestHub::finish`.

#![allow(dead_code)]

use std::{
    collections::VecDeque,
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use hqc_hub::{
    config::{ArtifactConfig, AuthConfig, Limits, Timing},
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
        title_request: Duration::from_millis(300),
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
        Self::start_upgraded(auth, email, limits, timing, None, |_| {}).await
    }

    /// Dev auth with a fresh user; `f` adjusts the artifact store (in a fresh temp dir).
    pub async fn start_artifacts(f: impl FnOnce(&mut ArtifactConfig)) -> Self {
        Self::start_config(|c| f(&mut c.artifacts)).await
    }

    /// Dev auth with a fresh user; `f` adjusts the config.
    pub async fn start_config(f: impl FnOnce(&mut Config)) -> Self {
        let email = format!("user-{}@example.com", Uuid::new_v4().simple());
        Self::start_upgraded(
            AuthConfig::Dev {
                email: email.clone(),
            },
            email,
            Limits::default(),
            test_timing(),
            None,
            f,
        )
        .await
    }

    /// Dev auth with a fresh user over a database that went through the migrations before
    /// `version` only, then got `seed` (`{email}` replaced by the user's email), and is then
    /// migrated to the current schema: an upgrade of a hub with existing data.
    pub async fn start_upgrading(version: i64, seed: &str) -> Self {
        let email = format!("user-{}@example.com", Uuid::new_v4().simple());
        let seed = seed.replace("{email}", &email);
        Self::start_upgraded(
            AuthConfig::Dev {
                email: email.clone(),
            },
            email,
            Limits::default(),
            test_timing(),
            Some((version, seed)),
            |_| {},
        )
        .await
    }

    async fn start_upgraded(
        auth: AuthConfig,
        email: String,
        limits: Limits,
        timing: Timing,
        upgrade: Option<(i64, String)>,
        configure: impl FnOnce(&mut Config),
    ) -> Self {
        let url = db_url();
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap_or_else(|e| panic!("connect to {url} (HQC_TEST_DATABASE_URL): {e}"));
        // pg_trgm (migration 0016) is one per database: it lives in `public`, outside the
        // schemas the tests drop, and every test's search path reaches it there. Concurrent
        // creates may race; the loser finds it on the next try.
        for _ in 0..3 {
            if sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public")
                .execute(&admin)
                .await
                .is_ok()
            {
                break;
            }
        }
        let schema = format!("t_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let search_path = format!("{schema},public");
        let opts = PgConnectOptions::from_str(&url)
            .unwrap()
            .options([("search_path", search_path.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await
            .unwrap();
        if let Some((version, seed)) = upgrade {
            let before = sqlx::migrate::Migrator {
                migrations: std::borrow::Cow::Owned(
                    MIGRATOR
                        .iter()
                        .filter(|m| m.version < version)
                        .cloned()
                        .collect(),
                ),
                ..sqlx::migrate::Migrator::DEFAULT
            };
            before.run(&pool).await.unwrap();
            sqlx::raw_sql(&seed).execute(&pool).await.unwrap();
        }
        MIGRATOR.run(&pool).await.unwrap();

        let art = ArtifactConfig::new(
            std::env::temp_dir().join(format!("hqc-artifacts-{}", Uuid::new_v4().simple())),
        );
        let mut config = Config {
            database_url: url,
            listen: "127.0.0.1:0".into(),
            auth,
            vapid: None,
            client_dir: None,
            timing,
            limits,
            artifacts: art,
            public_origins: vec![],
            client_ip_header: None,
        };
        configure(&mut config);
        std::fs::create_dir_all(&config.artifacts.dir).unwrap();
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
        let _ = std::fs::remove_dir_all(&self.app.config.artifacts.dir);
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

    /// `POST /api/artifacts` with the field `file` (and `kind`, if given).
    pub async fn upload(
        &self,
        name: &str,
        mime: &str,
        bytes: Vec<u8>,
        kind: Option<&str>,
    ) -> (u16, Value) {
        let mut part = reqwest::multipart::Part::bytes(bytes).file_name(name.to_string());
        if !mime.is_empty() {
            part = part.mime_str(mime).unwrap();
        }
        let mut form = reqwest::multipart::Form::new().part("file", part);
        if let Some(k) = kind {
            form = form.text("kind", k.to_string());
        }
        let r = self
            .http
            .post(format!("{}/api/artifacts", self.base))
            .multipart(form)
            .send()
            .await
            .unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
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

    /// `POST /api/workspaces` in `project` on `w`'s worker while `w` answers the `fs.*`
    /// requests it gets meanwhile (none, when the hub refuses before asking).
    pub async fn add_workspace(
        &self,
        w: &mut FakeWorker,
        project: &str,
        path: &str,
    ) -> (u16, Value) {
        let body = json!({"project_id": project, "worker_id": w.worker_id, "path": path});
        self.while_worker_answers(w, self.post("/api/workspaces", body))
            .await
    }

    /// Run a request while `w` answers the `fs.*` frames the hub sends it.
    pub async fn while_worker_answers<F: std::future::Future<Output = (u16, Value)>>(
        &self,
        w: &mut FakeWorker,
        req: F,
    ) -> (u16, Value) {
        tokio::pin!(req);
        loop {
            tokio::select! {
                r = &mut req => return r,
                Some(f) = recv_json(&mut w.ws, WAIT) => {
                    if f["type"] == "mirror.set"
                        || f["type"] == "workspace.rules"
                        || f["type"] == "workspace.create"
                    {
                        // Every workspace is mirrored by default (T21); a clone is asked for
                        // before the answer (T45).
                        w.queue.push_back(f);
                        continue;
                    }
                    let (ty, payload) = w.fs_reply(&f);
                    w.reply(&f["id"], ty, payload).await;
                }
            }
        }
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

    /// Sends to session `id` wait in its send buffer instead of waking it (`wake_on_send`
    /// off; on by default since T52), for the tests of the buffer.
    pub async fn buffer_sends(&self, id: &str) {
        sqlx::query("UPDATE sessions SET wake_on_send = false WHERE id = $1")
            .bind(id.parse::<Uuid>().unwrap())
            .execute(&self.app.db)
            .await
            .unwrap();
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
///
/// `mirror.set` frames (every workspace is mirrored by default since T21) are set aside in
/// [`FakeWorker::mirror_sets`] unless [`FakeWorker::see_mirror_set`] is on, so tests that
/// are not about the mirror see the frames they expect.
pub struct FakeWorker {
    ws: Ws,
    next: u64,
    /// Frames read ahead of [`FakeWorker::recv`].
    queue: VecDeque<Value>,
    pub see_mirror_set: bool,
    pub mirror_sets: Vec<Value>,
    /// The same for `session.cleanup` (T21): [`FakeWorker::see_cleanup`], `cleanups`.
    pub see_cleanup: bool,
    pub cleanups: Vec<Value>,
    /// The same for `title.request` (T24): set aside unanswered, so the hub falls back to the
    /// prompt after `timing.title_request`; [`FakeWorker::see_titles`] shows them.
    pub see_titles: bool,
    pub titles: Vec<Value>,
    /// The same for `workspace.rules` (T48, sent for every workspace after `hello`):
    /// [`FakeWorker::see_rules`], `rules`.
    pub see_rules: bool,
    pub rules: Vec<Value>,
    /// `allowed_roots` of its `hello`; [`FakeWorker::fs_reply`] answers by them.
    pub roots: Vec<String>,
    /// From `hello.ack`.
    pub worker_id: String,
}

impl FakeWorker {
    pub async fn connect(hub: &TestHub, token: &str) -> Result<Self, String> {
        let mut req = format!("{}/worker/ws", hub.ws_base)
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("Authorization", format!("Bearer {token}").parse().unwrap());
        match tokio_tungstenite::connect_async(req).await {
            Ok((ws, _)) => Ok(Self {
                ws,
                next: 1,
                queue: VecDeque::new(),
                see_mirror_set: false,
                mirror_sets: vec![],
                see_cleanup: false,
                cleanups: vec![],
                see_titles: false,
                titles: vec![],
                see_rules: false,
                rules: vec![],
                roots: vec![],
                worker_id: String::new(),
            }),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Connect and complete `hello` with `allowed_roots = [workspace]`, with the hub's
    /// [`TestHub::boot_id`]: every `online` of a test is the same worker process
    /// reconnecting. The first time, `workspace` is added to the personal project through
    /// `POST /api/workspaces` (the worker answering `fs.stat`), so sessions can be created.
    /// Returns `hello.ack`.
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
        let (mut w, ack) = Self::online_bare(hub, token, workspace, boot_id).await;
        let (_, list) = hub.get("/api/workspaces").await;
        let known = list["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["worker_id"] == w.worker_id.as_str() && x["path"] == workspace);
        if !known {
            let pid = hub.personal_project().await;
            let (s, v) = hub.add_workspace(&mut w, &pid, workspace).await;
            assert_eq!(s, 201, "{v}");
        }
        (w, ack)
    }

    /// Connect and `hello` with `allowed_roots = [root]`, no workspace added.
    pub async fn online_bare(
        hub: &TestHub,
        token: &str,
        root: &str,
        boot_id: Uuid,
    ) -> (Self, Value) {
        let mut w = Self::connect(hub, token).await.expect("worker connect");
        let ack = w.hello(root, boot_id).await;
        (w, ack)
    }

    pub async fn hello(&mut self, root: &str, boot_id: Uuid) -> Value {
        self.hello_with(root, boot_id, json!({})).await
    }

    /// `hello` with the fields of `extra` replacing the defaults (`environments`, ...).
    pub async fn hello_with(&mut self, root: &str, boot_id: Uuid, extra: Value) -> Value {
        let mut payload = json!({
            "protocol_version": hqc_proto::PROTOCOL_VERSION,
            "boot_id": boot_id,
            "capabilities": {"claude_code": {"version": "2.1.307"}, "acp_agents": []},
            "environments": ["default"],
            "env_bindings": [],
            "allowed_roots": [root],
            "allow_foreign_sessions": false,
            "share_local_models": false,
            "local_models": []
        });
        for (k, v) in extra.as_object().unwrap() {
            payload[k] = v.clone();
        }
        let id = self.send("hello", payload).await;
        let ack = self.recv().await;
        assert_eq!(ack["type"], "hello.ack", "{ack}");
        assert_eq!(ack["id"], id);
        self.roots = vec![root.to_string()];
        self.worker_id = ack["payload"]["worker_id"].as_str().unwrap().to_string();
        ack
    }

    /// What this fake answers to `fs.stat` / `fs.list_dirs` (SPEC §5.3), by its `roots`:
    /// a path containing `missing` does not exist, one ending in `.txt` is a file, a path
    /// ending in `/link` resolves to its parent, and `fs.list_dirs` outside the roots is an
    /// error.
    pub fn fs_reply(&self, frame: &Value) -> (&'static str, Value) {
        let p = &frame["payload"];
        let rid = p["request_id"].clone();
        let inside = |path: &str| {
            self.roots
                .iter()
                .any(|r| std::path::Path::new(path).starts_with(r))
        };
        match frame["type"].as_str() {
            Some("fs.stat") => {
                let path = p["path"].as_str().unwrap().trim_end_matches('/');
                let canonical = path.strip_suffix("/link").unwrap_or(path);
                if !inside(canonical) {
                    return (
                        "result",
                        json!({"request_id": rid, "exists": false, "is_dir": false,
                               "inside_allowed_roots": false}),
                    );
                }
                // `create` (SPEC 0.64): the worker makes a missing folder.
                if path.contains("missing") && p["create"] != true {
                    return (
                        "result",
                        json!({"request_id": rid, "exists": false, "is_dir": false,
                               "inside_allowed_roots": true}),
                    );
                }
                (
                    "result",
                    json!({"request_id": rid, "exists": true, "is_dir": !path.ends_with(".txt"),
                           "canonical": canonical, "inside_allowed_roots": true}),
                )
            }
            Some("fs.list_dirs") => match p["path"].as_str() {
                None => {
                    let entries: Vec<Value> = self
                        .roots
                        .iter()
                        .map(|r| json!({"name": r.rsplit('/').next().unwrap(), "path": r}))
                        .collect();
                    ("result", json!({"request_id": rid, "entries": entries}))
                }
                Some(path) if inside(path) => (
                    "result",
                    json!({"request_id": rid, "entries": [
                        {"name": "a", "path": format!("{path}/a")},
                        {"name": "b", "path": format!("{path}/b")}
                    ]}),
                ),
                Some(path) => (
                    "error",
                    json!({"code": "invalid_frame", "retryable": false,
                           "message": format!("{path} is outside allowed_roots")}),
                ),
            },
            _ => panic!("not an fs request: {frame}"),
        }
    }

    /// Answer the next frame, which must be an `fs.*` request, with [`Self::fs_reply`].
    pub async fn answer_fs(&mut self) -> Value {
        let f = self.recv().await;
        let (ty, payload) = self.fs_reply(&f);
        self.reply(&f["id"], ty, payload).await;
        f
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
        self.recv_within(WAIT)
            .await
            .expect("worker: no frame from hub")
    }

    async fn recv_within(&mut self, d: Duration) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + d;
        loop {
            let f = match self.queue.pop_front() {
                Some(f) => f,
                None => {
                    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                    recv_json(&mut self.ws, left).await?
                }
            };
            if f["type"] == "mirror.set" && !self.see_mirror_set {
                self.mirror_sets.push(f);
                continue;
            }
            if f["type"] == "session.cleanup" && !self.see_cleanup {
                self.cleanups.push(f);
                continue;
            }
            if f["type"] == "title.request" && !self.see_titles {
                self.titles.push(f);
                continue;
            }
            if f["type"] == "workspace.rules" && !self.see_rules {
                self.rules.push(f);
                continue;
            }
            return Some(f);
        }
    }

    /// The next frame, which must have type `ty`.
    pub async fn expect(&mut self, ty: &str) -> Value {
        let f = self.recv().await;
        assert_eq!(f["type"], ty, "unexpected frame {f}");
        f
    }

    pub async fn expect_silence(&mut self, d: Duration) {
        if let Some(f) = self.recv_within(d).await {
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

// ---------------------------------------------------------------- workspaces hqc makes (T45, T46)

/// The workspace `id` as `GET /api/workspaces` lists it, `Null` once it is gone.
pub async fn workspace(hub: &TestHub, id: &str) -> Value {
    let (s, list) = hub.get("/api/workspaces").await;
    assert_eq!(s, 200, "{list}");
    list["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == id)
        .cloned()
        .unwrap_or(Value::Null)
}
/// The `workspace.create` for `id`, acked like the worker does.
pub async fn create_frame(w: &mut FakeWorker, id: &str) -> Value {
    let f = w.expect("workspace.create").await;
    assert_eq!(f["payload"]["request_id"], id, "{f}");
    w.reply(&f["id"], "ack", json!({})).await;
    f
}

pub async fn created(w: &mut FakeWorker, payload: Value) {
    let id = w.send("workspace.created", payload).await;
    let ack = w.expect("ack").await;
    assert_eq!(ack["id"], id);
}

/// Run `req` while the worker answers the one frame of type `ty` it gets with `answer`.
pub async fn while_answering<T>(
    w: &mut FakeWorker,
    ty: &str,
    answer: Value,
    req: impl std::future::Future<Output = T>,
) -> (T, Value) {
    tokio::pin!(req);
    let f = tokio::select! {
        _ = &mut req => panic!("answered before the worker got {ty}"),
        f = next_of(w, ty) => f,
    };
    let mut payload = answer;
    payload["request_id"] = f["payload"]["request_id"].clone();
    w.reply(&f["id"], "result", payload).await;
    (req.await, f)
}
/// The workspace once `pred` holds for it (the hub handles a worker's `error` on its own time).
pub async fn workspace_when(hub: &TestHub, id: &str, pred: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let ws = workspace(hub, id).await;
        if pred(&ws) {
            return ws;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "workspace stays {ws}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// The next frame of type `ty`, skipping others (`session.kill` of an archived session).
pub async fn next_of(w: &mut FakeWorker, ty: &str) -> Value {
    loop {
        let f = w.recv().await;
        if f["type"] == ty {
            return f;
        }
    }
}
