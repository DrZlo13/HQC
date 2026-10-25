//! `hqc-worker mcp` (SPEC §7.2): a stdio MCP server, JSON-RPC 2.0 with one message per line,
//! started by the runtime itself for one session.
//!
//! Hand-written rather than an MCP SDK crate: the server needs `initialize`, `ping`,
//! `tools/list`, `tools/call` and `notifications/cancelled` over stdio, about 300 lines;
//! an SDK would bring its own runtime layer, macros and transports for the same five methods,
//! and a protocol version we would still have to match against the runtime by hand.
//!
//! - The token comes only from [`super::TOKEN_ENV`]; it goes to the daemon in the socket's
//!   first line and nowhere else (not into results, errors or logs).
//! - `initialize` answers the runtime's protocol version when it is one of
//!   [`PROTOCOL_VERSIONS`], else the newest of them.
//! - `tools/list` is [`hqc_proto::mcp::TOOLS`]; `tools/call` of another name is a JSON-RPC
//!   error. A call goes to the daemon; its error comes back as a tool result with
//!   `isError: true` and the text `<code>: <message>`.
//! - `notifications/cancelled` forwards the cancel to the daemon and the call gets no answer
//!   (MCP). At EOF on stdin the process exits; the daemon sees the socket close and cancels
//!   whatever was still open.
//! - The daemon connection is opened on the first `tools/call` and kept; if it breaks, the
//!   open calls fail and the next call connects again.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hqc_proto::frame::McpError;
use hqc_proto::mcp::{error_code, MAX_ARGS, TOOLS};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

use super::{FromDaemon, ToDaemon, MAX_LINE};

/// Newest first; Claude Code 2.1.308 asks for `2025-11-25` (T30 live run `live_mcp`).
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

pub struct McpServer {
    token: Option<String>,
    socket: PathBuf,
}

/// The waiting `tools/call`s of one daemon connection, by the connection's call id.
type Waiting = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, McpError>>>>>;

struct Daemon {
    tx: mpsc::UnboundedSender<ToDaemon>,
    waiting: Waiting,
    /// Cleared when the daemon closes the connection.
    alive: Arc<AtomicBool>,
}

impl McpServer {
    /// `token`: the value of [`super::TOKEN_ENV`], `None` if unset.
    pub fn new(token: Option<String>, socket: PathBuf) -> Self {
        Self { token, socket }
    }

    /// Serves until EOF on `input`.
    pub async fn serve<R, W>(self, input: R, output: W) -> anyhow::Result<()>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
        let writer = tokio::spawn(async move {
            let mut output = output;
            while let Some(v) = out_rx.recv().await {
                let mut line = serde_json::to_vec(&v).expect("JSON-RPC messages serialize");
                line.push(b'\n');
                if output.write_all(&line).await.is_err() || output.flush().await.is_err() {
                    break;
                }
            }
        });
        let mut lines = BufReader::new(input);
        let mut buf = Vec::new();
        let mut session = Session {
            server: &self,
            out: out_tx,
            daemon: None,
            protocol_version: None,
            client: None,
            calls: Arc::default(),
            next_call: 0,
        };
        loop {
            buf.clear();
            let n = (&mut lines)
                .take(MAX_LINE as u64 + 1)
                .read_until(b'\n', &mut buf)
                .await?;
            if n == 0 {
                break;
            }
            if buf.len() > MAX_LINE {
                session.error(Value::Null, INVALID_REQUEST, "message too long");
                // The rest of the line is not a message either.
                while !buf.ends_with(b"\n") {
                    buf.clear();
                    if lines.read_until(b'\n', &mut buf).await? == 0 {
                        break;
                    }
                }
                continue;
            }
            if buf.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match serde_json::from_slice::<Value>(&buf) {
                Ok(msg) => session.on_message(msg).await,
                Err(e) => session.error(Value::Null, PARSE_ERROR, &format!("parse error: {e}")),
            }
        }
        drop(session);
        let _ = writer.await;
        Ok(())
    }
}

struct Session<'a> {
    server: &'a McpServer,
    out: mpsc::UnboundedSender<Value>,
    daemon: Option<Daemon>,
    protocol_version: Option<String>,
    client: Option<Value>,
    /// JSON-RPC id of an open `tools/call` -> the daemon call id.
    calls: Arc<Mutex<HashMap<String, u64>>>,
    next_call: u64,
}

impl Session<'_> {
    fn respond(&self, id: Value, result: Value) {
        let _ = self
            .out
            .send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    fn error(&self, id: Value, code: i64, message: &str) {
        let _ = self
            .out
            .send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}));
    }

    async fn on_message(&mut self, msg: Value) {
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // A response to a request of ours: we send none.
            if msg.get("id").is_none() {
                self.error(Value::Null, INVALID_REQUEST, "not a JSON-RPC message");
            }
            return;
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = msg.get("id").cloned() else {
            // A notification.
            if method == "notifications/cancelled" {
                if let Some(rid) = params.get("requestId") {
                    self.cancel(rid);
                }
            }
            return;
        };
        match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str);
                let version = asked
                    .filter(|v| PROTOCOL_VERSIONS.contains(v))
                    .unwrap_or(PROTOCOL_VERSIONS[0]);
                self.protocol_version = Some(version.to_string());
                self.client = params.get("clientInfo").cloned();
                self.respond(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {"name": "hqc", "version": env!("CARGO_PKG_VERSION")},
                        "instructions": "hqc tools act with the rights of this session's owner.",
                    }),
                );
            }
            "ping" => self.respond(id, json!({})),
            "tools/list" => {
                let tools: Vec<Value> = TOOLS.iter().map(|t| t.listing()).collect();
                self.respond(id, json!({ "tools": tools }));
            }
            "tools/call" => self.call(id, &params).await,
            _ => self.error(id, METHOD_NOT_FOUND, &format!("method {method} not found")),
        }
    }

    async fn call(&mut self, id: Value, params: &Value) {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return self.error(id, INVALID_PARAMS, "tools/call without name");
        };
        if hqc_proto::mcp::tool(name).is_none() {
            return self.error(id, INVALID_PARAMS, &format!("unknown tool {name}"));
        }
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let size = serde_json::to_vec(&args).map(|v| v.len()).unwrap_or(0);
        if size > MAX_ARGS {
            return self.tool_error(
                id,
                &McpError::new(
                    error_code::INVALID_ARGS,
                    format!("arguments are {size} bytes, more than {MAX_ARGS}"),
                ),
            );
        }
        if let Err(e) = self.connect().await {
            return self.tool_error(id, &e);
        }
        self.next_call += 1;
        let call_id = self.next_call;
        let daemon = self.daemon.as_ref().expect("connected");
        let (tx, rx) = oneshot::channel();
        daemon.waiting.lock().unwrap().insert(call_id, tx);
        let sent = daemon.tx.send(ToDaemon::Call {
            id: call_id,
            tool: name.to_string(),
            args,
        });
        if sent.is_err() {
            daemon.waiting.lock().unwrap().remove(&call_id);
            self.daemon = None;
            return self.tool_error(
                id,
                &McpError::new(error_code::UNAVAILABLE, "the hqc worker daemon is gone"),
            );
        }
        let key = id.to_string();
        self.calls.lock().unwrap().insert(key.clone(), call_id);
        let (out, calls) = (self.out.clone(), self.calls.clone());
        tokio::spawn(async move {
            // Dropped without an answer: cancelled, no response (MCP).
            let Ok(outcome) = rx.await else { return };
            calls.lock().unwrap().remove(&key);
            let _ = out.send(json!({"jsonrpc": "2.0", "id": id, "result": tool_result(outcome)}));
        });
    }

    fn tool_error(&self, id: Value, e: &McpError) {
        self.respond(id, tool_result(Err(e.clone())));
    }

    fn cancel(&mut self, request_id: &Value) {
        let Some(call_id) = self.calls.lock().unwrap().remove(&request_id.to_string()) else {
            return;
        };
        if let Some(d) = &self.daemon {
            d.waiting.lock().unwrap().remove(&call_id);
            let _ = d.tx.send(ToDaemon::Cancel { id: call_id });
        }
    }

    /// Opens and authenticates the daemon connection unless a live one is there.
    async fn connect(&mut self) -> Result<(), McpError> {
        if self
            .daemon
            .as_ref()
            .is_some_and(|d| d.alive.load(Ordering::SeqCst))
        {
            return Ok(());
        }
        self.daemon = None;
        let unavailable = |m: String| McpError::new(error_code::UNAVAILABLE, m);
        let Some(token) = self.server.token.clone() else {
            return Err(McpError::new(
                error_code::UNAUTHORIZED,
                format!("{} is not set", super::TOKEN_ENV),
            ));
        };
        let stream = UnixStream::connect(&self.server.socket)
            .await
            .map_err(|e| unavailable(format!("the hqc worker daemon is not reachable: {e}")))?;
        let (rd, mut wr) = stream.into_split();
        let mut rd = BufReader::new(rd);
        let hello = ToDaemon::Hello {
            token,
            protocol_version: self.protocol_version.clone(),
            client: self.client.clone(),
        };
        let mut line = serde_json::to_vec(&hello).expect("hello serializes");
        line.push(b'\n');
        wr.write_all(&line)
            .await
            .map_err(|e| unavailable(format!("the hqc worker daemon: {e}")))?;
        let mut buf = Vec::new();
        match read_reply(&mut rd, &mut buf).await {
            Some(FromDaemon::Ready) => {}
            Some(FromDaemon::Refused { error }) => return Err(error),
            _ => return Err(unavailable("the hqc worker daemon did not answer".into())),
        }
        let waiting: Waiting = Arc::default();
        let (tx, mut rx) = mpsc::unbounded_channel::<ToDaemon>();
        tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                let mut line = serde_json::to_vec(&m).expect("daemon messages serialize");
                line.push(b'\n');
                if wr.write_all(&line).await.is_err() {
                    break;
                }
            }
        });
        let (w, alive) = (waiting.clone(), Arc::new(AtomicBool::new(true)));
        let alive_r = alive.clone();
        tokio::spawn(async move {
            let mut buf = Vec::new();
            while let Some(m) = read_reply(&mut rd, &mut buf).await {
                if let FromDaemon::Result { id, result, error } = m {
                    let outcome = match (result, error) {
                        (_, Some(e)) => Err(e),
                        (Some(r), None) => Ok(r),
                        (None, None) => Ok(Value::Null),
                    };
                    if let Some(tx) = w.lock().unwrap().remove(&id) {
                        let _ = tx.send(outcome);
                    }
                }
            }
            // The daemon went away: the open calls fail, the next call reconnects.
            alive_r.store(false, Ordering::SeqCst);
            for (_, tx) in w.lock().unwrap().drain() {
                let _ = tx.send(Err(unavailable(
                    "the connection to the hqc worker daemon was lost".into(),
                )));
            }
        });
        self.daemon = Some(Daemon { tx, waiting, alive });
        Ok(())
    }
}

async fn read_reply<R: tokio::io::AsyncBufRead + Unpin>(
    r: &mut R,
    buf: &mut Vec<u8>,
) -> Option<FromDaemon> {
    buf.clear();
    let n = (&mut *r)
        .take(MAX_LINE as u64 * 16)
        .read_until(b'\n', buf)
        .await
        .ok()?;
    if n == 0 {
        return None;
    }
    serde_json::from_slice(buf).ok()
}

/// The `tools/call` result: the hub's JSON as text, or the error as `isError` text.
pub fn tool_result(outcome: Result<Value, McpError>) -> Value {
    match outcome {
        Ok(v) => json!({
            "content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}],
            "isError": false,
        }),
        Err(e) => json!({
            "content": [{"type": "text", "text": format!("{}: {}", e.code, e.message)}],
            "isError": true,
        }),
    }
}

#[cfg(test)]
mod tests;
