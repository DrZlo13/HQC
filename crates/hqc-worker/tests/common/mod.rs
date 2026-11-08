//! Test doubles: a fake hub on tokio-tungstenite, a minimal HTTP responder for enrollment,
//! and an in-process worker wired to `FakeAdapters`.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use hqc_worker::adapter::fake::FakeAdapters;
use hqc_worker::adapter::AdapterFactory;
use hqc_worker::config::Config;
use hqc_worker::connection::{ConnOptions, Status};
use hqc_worker::token::Secret;
use hqc_worker::Worker;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub const TOKEN: &str = "wt_test_secret_0123456789";
pub const WAIT: Duration = Duration::from_secs(10);

pub struct FakeHub {
    pub addr: SocketAddr,
    conns: mpsc::Receiver<HubConn>,
    /// Served on `GET /worker/artifacts/{id}` on the same address (SPEC §5.3).
    pub artifacts: Artifacts,
}

/// An artifact of the fake hub: name, kind, `Content-Type`, content.
#[derive(Clone, Debug)]
pub struct FakeArtifact {
    pub name: String,
    pub kind: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// Path and `Authorization` of an artifact request.
pub type ArtifactRequest = (String, Option<String>);

/// A `PUT /worker/artifacts` the fake hub took (SPEC §5.4): the headers, lowercased, and
/// the body; it answered with `id`.
#[derive(Clone, Debug)]
pub struct Upload {
    pub id: Uuid,
    pub headers: std::collections::HashMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Clone, Default)]
pub struct Artifacts {
    pub files: Arc<std::sync::Mutex<std::collections::HashMap<Uuid, FakeArtifact>>>,
    /// Path and `Authorization` of every artifact request.
    pub requests: Arc<std::sync::Mutex<Vec<ArtifactRequest>>>,
    pub uploads: Arc<std::sync::Mutex<Vec<Upload>>>,
    /// Answer uploads only after a while.
    pub stall_uploads: Arc<std::sync::atomic::AtomicBool>,
}

impl Artifacts {
    pub fn insert(&self, id: Uuid, a: FakeArtifact) {
        self.files.lock().unwrap().insert(id, a);
    }

    pub fn uploads(&self) -> Vec<Upload> {
        self.uploads.lock().unwrap().clone()
    }

    /// Answers one plain HTTP request on `stream` if it is `GET /worker/artifacts/...` or
    /// `PUT /worker/artifacts`; gives the stream back otherwise (a WebSocket upgrade).
    async fn serve(&self, mut stream: TcpStream) -> Option<TcpStream> {
        let mut peek = [0u8; 64];
        let n = stream.peek(&mut peek).await.ok()?;
        if peek[..n].starts_with(b"PUT /worker/artifacts ") {
            self.take_upload(stream).await;
            return None;
        }
        if !peek[..n].starts_with(b"GET /worker/artifacts/") {
            return Some(stream);
        }
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        while find(&buf, b"\r\n\r\n").is_none() {
            let n = stream.read(&mut tmp).await.ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let head = String::from_utf8_lossy(&buf).to_string();
        let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
        let auth = head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("authorization")
                .then(|| v.trim().to_string())
        });
        self.requests.lock().unwrap().push((path.clone(), auth));
        let id: Option<Uuid> = path.rsplit('/').next().and_then(|s| s.parse().ok());
        let found = id.and_then(|id| self.files.lock().unwrap().get(&id).cloned());
        let out = match found {
            Some(a) => {
                let mut h = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nx-hqc-artifact-name: {}\r\n\
                     x-hqc-artifact-kind: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    a.mime,
                    pct(&a.name),
                    a.kind,
                    a.bytes.len()
                )
                .into_bytes();
                h.extend_from_slice(&a.bytes);
                h
            }
            None => {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
            }
        };
        let _ = stream.write_all(&out).await;
        let _ = stream.shutdown().await;
        None
    }
}

impl Artifacts {
    async fn take_upload(&self, mut stream: TcpStream) -> Option<()> {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 65536];
        let end = loop {
            if let Some(i) = find(&buf, b"\r\n\r\n") {
                break i + 4;
            }
            let n = stream.read(&mut tmp).await.ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&tmp[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..end]).to_string();
        let headers: std::collections::HashMap<String, String> = head
            .lines()
            .skip(1)
            .filter_map(|l| {
                let (k, v) = l.split_once(':')?;
                Some((k.trim().to_ascii_lowercase(), v.trim().to_string()))
            })
            .collect();
        let len: usize = headers.get("content-length")?.parse().ok()?;
        let mut body = buf[end..].to_vec();
        while body.len() < len {
            let n = stream.read(&mut tmp).await.ok()?;
            if n == 0 {
                return None;
            }
            body.extend_from_slice(&tmp[..n]);
        }
        let id = Uuid::now_v7();
        self.uploads
            .lock()
            .unwrap()
            .push(Upload { id, headers, body });
        if self.stall_uploads.load(std::sync::atomic::Ordering::SeqCst) {
            // Answered late: a worker that did not give up would send its call then.
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        let json = format!("{{\"id\":\"{id}\"}}");
        let out = format!(
            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{json}",
            json.len()
        );
        let _ = stream.write_all(out.as_bytes()).await;
        let _ = stream.shutdown().await;
        Some(())
    }
}

fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

pub struct HubConn {
    pub auth: Option<String>,
    pub path: String,
    ws: WebSocketStream<TcpStream>,
    /// Frames skipped by [`HubConn::expect`], in arrival order.
    pub skipped: Vec<Value>,
    next_id: u64,
}

impl FakeHub {
    pub async fn start() -> Self {
        Self::bind("127.0.0.1:0").await
    }

    pub async fn bind(addr: &str) -> Self {
        let l = TcpListener::bind(addr).await.unwrap();
        let addr = l.local_addr().unwrap();
        let (tx, conns) = mpsc::channel(8);
        let artifacts = Artifacts::default();
        let served = artifacts.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = l.accept().await {
                let tx = tx.clone();
                let served = served.clone();
                tokio::spawn(async move {
                    let Some(stream) = served.serve(stream).await else {
                        return;
                    };
                    let mut auth = None;
                    let mut path = String::new();
                    // The error type is fixed by tungstenite's `Callback`.
                    #[allow(clippy::result_large_err)]
                    let cb = |req: &Request, resp: Response| {
                        auth = req
                            .headers()
                            .get("authorization")
                            .map(|v| v.to_str().unwrap().to_string());
                        path = req.uri().path().to_string();
                        Ok(resp)
                    };
                    if let Ok(ws) = tokio_tungstenite::accept_hdr_async(stream, cb).await {
                        let _ = tx
                            .send(HubConn {
                                auth,
                                path,
                                ws,
                                skipped: vec![],
                                next_id: 0,
                            })
                            .await;
                    }
                });
            }
        });
        Self {
            addr,
            conns,
            artifacts,
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub async fn accept(&mut self) -> HubConn {
        tokio::time::timeout(WAIT, self.conns.recv())
            .await
            .expect("worker did not connect")
            .unwrap()
    }

    pub async fn no_connection_within(&mut self, d: Duration) {
        if let Ok(Some(_)) = tokio::time::timeout(d, self.conns.recv()).await {
            panic!("unexpected connection");
        }
    }
}

impl HubConn {
    pub async fn try_recv(&mut self, d: Duration) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + d;
        loop {
            let m = tokio::time::timeout_at(deadline, self.ws.next())
                .await
                .ok()??;
            match m {
                Ok(Message::Text(t)) => return Some(serde_json::from_str(&t).unwrap()),
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }

    pub async fn recv(&mut self) -> Value {
        self.try_recv(WAIT).await.expect("no frame from worker")
    }

    /// Next frame of type `ty`; others are kept in `skipped`.
    pub async fn expect(&mut self, ty: &str) -> Value {
        loop {
            let f = self.recv().await;
            if f["type"] == ty {
                return f;
            }
            self.skipped.push(f);
        }
    }

    /// Like `expect`, but `None` if nothing of type `ty` arrives within `d`.
    pub async fn maybe(&mut self, ty: &str, d: Duration) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + d;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let f = self.try_recv(left).await?;
            if f["type"] == ty {
                return Some(f);
            }
            self.skipped.push(f);
        }
    }

    pub async fn send(&mut self, v: Value) {
        self.ws.send(Message::text(v.to_string())).await.unwrap();
    }

    /// Sends a hub frame with a fresh `f-N` id; returns the id.
    pub async fn frame(&mut self, ty: &str, payload: Value) -> String {
        self.next_id += 1;
        let id = format!("f-{}", self.next_id);
        self.send(json!({"id": id, "type": ty, "payload": payload}))
            .await;
        id
    }

    /// Reads `hello`, answers `hello.ack`; returns the hello frame.
    pub async fn handshake(&mut self) -> Value {
        let hello = self.recv().await;
        assert_eq!(hello["type"], "hello", "first frame must be hello: {hello}");
        self.send(json!({
            "id": hello["id"], "type": "hello.ack",
            "payload": {"protocol_version": 1, "worker_id": Uuid::nil(), "server_time": "2026-10-22T10:00:00Z"}
        }))
        .await;
        hello
    }

    /// `handshake` of a hub that names the worker's service session (SPEC §10.2).
    pub async fn handshake_system(&mut self, system_session_id: Uuid) -> Value {
        let hello = self.recv().await;
        assert_eq!(hello["type"], "hello", "first frame must be hello: {hello}");
        self.send(json!({
            "id": hello["id"], "type": "hello.ack",
            "payload": {"protocol_version": 1, "worker_id": Uuid::nil(),
                        "server_time": "2026-10-22T10:00:00Z",
                        "system_session_id": system_session_id}
        }))
        .await;
        hello
    }

    pub async fn ack_event(&mut self, frame: &Value, seq: u64) {
        self.send(json!({
            "id": frame["id"], "type": "ack",
            "payload": {"event_id": frame["payload"]["event"]["event_id"], "seq": seq}
        }))
        .await;
    }

    pub async fn close(mut self) {
        let _ = self.ws.close(None).await;
    }

    /// A WebSocket ping, as the hub sends on its heartbeat; a closed connection is ignored.
    pub async fn ping(&mut self) {
        let _ = self.ws.send(Message::Ping(Default::default())).await;
    }
}

pub fn event_id(frame: &Value) -> Uuid {
    frame["payload"]["event"]["event_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

/// A temp dir with `root` as `allowed_roots` and `root/ws` as the workspace of its sessions.
pub struct Env {
    pub dir: tempfile::TempDir,
    /// `Some((projects dir, registry))`: the worker runs the mirror over that folder.
    pub mirror: Option<(PathBuf, Arc<hqc_worker::mirror::OwnRecords>)>,
}

impl Env {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("root/ws/sub")).unwrap();
        Self { dir, mirror: None }
    }

    /// With the mirror over `<tmp>/claude/projects`.
    pub fn with_mirror() -> Self {
        let mut e = Self::new();
        let projects = e.dir.path().join("claude/projects");
        std::fs::create_dir_all(&projects).unwrap();
        e.mirror = Some((projects, Arc::default()));
        e
    }

    pub fn root(&self) -> PathBuf {
        self.dir.path().join("root").canonicalize().unwrap()
    }

    pub fn workspace(&self) -> PathBuf {
        self.root().join("ws")
    }

    pub fn outbox(&self) -> PathBuf {
        self.dir.path().join("outbox")
    }

    pub fn known_paths(&self) -> PathBuf {
        self.dir.path().join("known_workspaces.json")
    }

    /// The daemon's MCP socket (SPEC §7.2).
    pub fn mcp_socket(&self) -> PathBuf {
        self.dir.path().join("mcp.sock")
    }

    pub fn config_toml(&self, hub_url: &str) -> String {
        format!(
            "hub_url = {:?}\nallowed_roots = [{:?}]\nidle_timeout = 0\nallow_foreign_sessions = false\n\
             claude_bin = \"/nonexistent/claude\"\n",
            hub_url,
            self.root().display().to_string(),
        )
    }

    pub fn config(&self, hub_url: &str) -> Config {
        Config::parse(&self.config_toml(hub_url)).unwrap()
    }

    /// `session.start` payload for the workspace `root/ws`.
    pub fn start_payload(&self, session_id: Uuid) -> Value {
        json!({
            "session_id": session_id,
            "mode": "new",
            "runtime": "claude_code",
            "workspace_path": self.workspace().display().to_string(),
            "cwd": ".",
            "runtime_session_id": Uuid::new_v4(),
            "disallowed_tools": [],
            "restricted": false,
            "auto_approve": false,
            "session_token": "st_test",
            "owner_is_worker_owner": true,
            "ask_timeout_s": 600
        })
    }
}

pub fn fast_conn() -> ConnOptions {
    ConnOptions {
        heartbeat: Duration::from_secs(20),
        dead_after: Duration::from_secs(60),
        backoff_min: Duration::from_millis(50),
        backoff_max: Duration::from_millis(200),
        version_retry: Duration::from_millis(300),
        hello_timeout: Duration::from_secs(5),
        flush_timeout: Duration::from_millis(500),
    }
}

pub struct RunningWorker {
    pub shutdown: CancellationToken,
    pub status: watch::Receiver<Status>,
    pub task: JoinHandle<anyhow::Result<()>>,
}

impl RunningWorker {
    pub async fn stop(self) {
        self.shutdown.cancel();
        tokio::time::timeout(WAIT, self.task)
            .await
            .expect("worker did not stop")
            .unwrap()
            .unwrap();
    }

    pub async fn wait_status(&mut self, pred: impl Fn(&Status) -> bool) -> Status {
        tokio::time::timeout(WAIT, self.status.wait_for(|s| pred(s)))
            .await
            .expect("status not reached")
            .unwrap()
            .clone()
    }
}

pub fn start_worker(env: &Env, hub_url: &str, adapters: &FakeAdapters) -> RunningWorker {
    start_worker_with(env, env.config(hub_url), adapters)
}

pub fn start_worker_with(env: &Env, config: Config, adapters: &FakeAdapters) -> RunningWorker {
    start_worker_factory(env, config, Arc::new(adapters.clone()))
}

pub fn start_worker_factory(
    env: &Env,
    config: Config,
    adapters: Arc<dyn AdapterFactory>,
) -> RunningWorker {
    start_worker_logs(env, config, adapters, None)
}

/// A worker that forwards the lines of `logs` to its service session (SPEC §10.2).
pub fn start_worker_logs(
    env: &Env,
    config: Config,
    adapters: Arc<dyn AdapterFactory>,
    logs: Option<hqc_worker::worker_log::LogReceiver>,
) -> RunningWorker {
    start_worker_opts(env, config, adapters, logs, fast_conn())
}

/// A worker with its own connection options.
pub fn start_worker_conn(
    env: &Env,
    hub_url: &str,
    adapters: &FakeAdapters,
    conn: ConnOptions,
) -> RunningWorker {
    start_worker_opts(
        env,
        env.config(hub_url),
        Arc::new(adapters.clone()),
        None,
        conn,
    )
}

fn start_worker_opts(
    env: &Env,
    config: Config,
    adapters: Arc<dyn AdapterFactory>,
    logs: Option<hqc_worker::worker_log::LogReceiver>,
    conn: ConnOptions,
) -> RunningWorker {
    let w = Worker {
        config,
        token: Secret::new(TOKEN),
        boot_id: Uuid::new_v4(),
        adapters,
        capabilities: hqc_worker::capabilities::build(Some("2.1.307")),
        outbox_dir: env.outbox(),
        known_paths_file: env.known_paths(),
        logs,
        conn,
        shutdown_timeout: Duration::from_secs(5),
        machine_id: None,
        mcp_socket: Some(env.mcp_socket()),
        mirror: env
            .mirror
            .clone()
            .map(|(projects, own)| hqc_worker::MirrorOptions {
                sessions_dir: Some(projects.with_file_name("sessions")),
                projects_dir: projects,
                state_dir: env.outbox().with_file_name("mirror"),
                own_records: own,
                poll: Duration::from_millis(100),
                grace: Duration::from_millis(300),
                quota_retry: Duration::from_millis(800),
            }),
    };
    let shutdown = CancellationToken::new();
    let (tx, status) = watch::channel(Status::Connecting);
    let task = tokio::spawn(w.run(shutdown.clone(), tx));
    RunningWorker {
        shutdown,
        status,
        task,
    }
}

/// A one-request-per-connection HTTP/1.1 server: `handler(path, body) -> (status, json)`.
pub async fn http_server<F>(handler: F) -> SocketAddr
where
    F: Fn(&str, Value) -> (u16, Value) + Send + Sync + 'static,
{
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let handler = Arc::new(handler);
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let handler = handler.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let head_end = loop {
                    let n = s.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(i) = find(&buf, b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                let len: usize = head
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse().ok())?
                    })
                    .unwrap_or(0);
                while buf.len() < head_end + len {
                    let n = s.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let body = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
                let (status, resp) = handler(&path, body);
                // A redirect: `{"location": ...}` becomes the Location header.
                let location = match resp["location"].as_str() {
                    Some(l) if (300..400).contains(&status) => format!("Location: {l}\r\n"),
                    _ => String::new(),
                };
                let resp = resp.to_string();
                let out = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{location}Content-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                    resp.len()
                );
                let _ = s.write_all(out.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    addr
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

pub fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_hqc-worker")
}

pub fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
