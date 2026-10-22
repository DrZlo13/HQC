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
        tokio::spawn(async move {
            while let Ok((stream, _)) = l.accept().await {
                let tx = tx.clone();
                tokio::spawn(async move {
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
        Self { addr, conns }
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
}

pub fn event_id(frame: &Value) -> Uuid {
    frame["payload"]["event"]["event_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

/// A temp dir with `root/ws` as the only workspace.
pub struct Env {
    pub dir: tempfile::TempDir,
}

impl Env {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("root/ws/sub")).unwrap();
        Self { dir }
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

    pub fn config_toml(&self, hub_url: &str) -> String {
        format!(
            "hub_url = {:?}\nallowed_roots = [{:?}]\nidle_timeout = 0\nallow_foreign_sessions = false\n\
             claude_bin = \"/nonexistent/claude\"\n\n[workspace]\npath = {:?}\n",
            hub_url,
            self.root().display().to_string(),
            self.workspace().display().to_string(),
        )
    }

    pub fn config(&self, hub_url: &str) -> Config {
        Config::parse(&self.config_toml(hub_url)).unwrap()
    }

    /// `session.start` payload for the configured workspace.
    pub fn start_payload(&self, session_id: Uuid) -> Value {
        json!({
            "session_id": session_id,
            "runtime": "claude_code",
            "workspace_path": self.workspace().display().to_string(),
            "cwd": ".",
            "environment": "default",
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
    let w = Worker {
        config,
        token: Secret::new(TOKEN),
        adapters,
        capabilities: hqc_worker::capabilities::build(Some("2.1.307")),
        outbox_dir: env.outbox(),
        conn: fast_conn(),
        shutdown_timeout: Duration::from_secs(5),
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
                let resp = resp.to_string();
                let out = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
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
