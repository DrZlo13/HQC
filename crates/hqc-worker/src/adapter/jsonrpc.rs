//! JSON-RPC over an agent process's stdio, one message per line: the ACP adapter's (SPEC §8.2)
//! and Codex's App Server (SPEC §8.6).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use super::claude_code::engine::preview;

/// ACP's error code for "authentication required".
pub const AUTH_REQUIRED: i64 = -32000;
/// JSON-RPC's "method not found".
pub const METHOD_NOT_FOUND: i64 = -32601;

/// An error answer of the agent, or a connection that ended.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl RpcError {
    pub fn closed() -> Self {
        Self {
            code: -32099,
            message: "the agent's connection ended".into(),
        }
    }

    pub fn auth_required(&self) -> bool {
        self.code == AUTH_REQUIRED
    }
}

type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>;

pub struct Rpc {
    /// Messages carry `"jsonrpc": "2.0"` (ACP); Codex's App Server leaves it out.
    versioned: bool,
    writer: AsyncMutex<std::pin::Pin<Box<dyn AsyncWrite + Send>>>,
    next: AtomicU64,
    pending: Pending,
}

impl Rpc {
    pub fn new(stdin: std::pin::Pin<Box<dyn AsyncWrite + Send>>, versioned: bool) -> Arc<Self> {
        Arc::new(Self {
            versioned,
            writer: AsyncMutex::new(stdin),
            next: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
        })
    }

    async fn write(&self, msg: &Value) -> Result<(), RpcError> {
        let mut msg = msg.clone();
        if !self.versioned {
            if let Some(o) = msg.as_object_mut() {
                o.remove("jsonrpc");
            }
        }
        let mut line = msg.to_string();
        line.push('\n');
        let mut w = self.writer.lock().await;
        w.write_all(line.as_bytes())
            .await
            .map_err(|_| RpcError::closed())?;
        w.flush().await.map_err(|_| RpcError::closed())
    }

    /// Sends a request; the answer is awaited by the returned future, the write is done
    /// when this returns.
    pub async fn send(
        &self,
        method: &str,
        params: Value,
    ) -> Result<oneshot::Receiver<Result<Value, RpcError>>, RpcError> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if let Err(e) = self.write(&msg).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        Ok(rx)
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let rx = self.send(method, params).await?;
        rx.await.unwrap_or_else(|_| Err(RpcError::closed()))
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), RpcError> {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await
    }

    pub async fn respond(&self, id: Value, result: Value) {
        let _ = self
            .write(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
            .await;
    }

    pub async fn respond_error(&self, id: Value, code: i64, message: &str) {
        let _ = self
            .write(
                &json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
            )
            .await;
    }

    pub fn resolve(&self, v: &Value) {
        let Some(id) = v["id"].as_u64() else {
            return;
        };
        let Some(tx) = self.pending.lock().unwrap().remove(&id) else {
            return;
        };
        let r = match v.get("error") {
            Some(e) => Err(RpcError {
                code: e["code"].as_i64().unwrap_or(0),
                message: e["message"].as_str().unwrap_or("error").to_string(),
            }),
            None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = tx.send(r);
    }

    /// Fails every request still waiting: the connection is gone.
    pub fn close(&self) {
        for (_, tx) in self.pending.lock().unwrap().drain() {
            let _ = tx.send(Err(RpcError::closed()));
        }
    }
}

/// What the agent sends on its own: requests and notifications.
#[async_trait]
pub trait Handler: Send + Sync {
    async fn request(&self, rpc: &Arc<Rpc>, id: Value, method: &str, params: Value);
    async fn notification(&self, method: &str, params: Value);
}

pub struct NoHandler;

#[async_trait]
impl Handler for NoHandler {
    async fn request(&self, rpc: &Arc<Rpc>, id: Value, _: &str, _: Value) {
        rpc.respond_error(id, METHOD_NOT_FOUND, "not supported")
            .await;
    }
    async fn notification(&self, _: &str, _: Value) {}
}

/// Reads the agent's stdout line by line until it ends; every message is handled before the
/// next is read, so the events of a turn go out before the answer that ends it is resolved.
pub async fn read_loop(
    stdout: std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>,
    rpc: Arc<Rpc>,
    handler: Arc<dyn Handler>,
) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            if !line.trim().is_empty() {
                tracing::debug!(line = %preview(&line, 200), "not a JSON-RPC message from the agent");
            }
            continue;
        };
        match (v.get("method").and_then(Value::as_str), v.get("id")) {
            (Some(m), Some(id)) => {
                let params = v.get("params").cloned().unwrap_or(Value::Null);
                handler.request(&rpc, id.clone(), m, params).await
            }
            (Some(m), None) => {
                let params = v.get("params").cloned().unwrap_or(Value::Null);
                handler.notification(m, params).await
            }
            (None, Some(_)) => rpc.resolve(&v),
            (None, None) => {}
        }
    }
    rpc.close();
}
