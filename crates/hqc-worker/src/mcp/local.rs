//! The MCP servers of the machine (iteration 9, T71, T71a): `hqc-worker mcp --local <name>`.
//!
//! ```text
//! CLI --stdio--> `hqc-worker mcp --local figma` --stdio--> the program (or --HTTP--> url)
//!                       |
//!                       +--unix socket--> daemon: the resolved spec, failures for the chat
//! ```
//!
//! An environment's `[environments.<env>.mcp_servers.<name>]` goes into the CLI's
//! `--mcp-config` as this launcher, never as the program itself: the values of its `env` and
//! `headers`, a Vaultwarden one read with `rbw` at every start, would otherwise be on the
//! CLI's command line. The launcher authenticates on the daemon's socket as the hqc server
//! does (the session token, a process inside the session's agent) and asks for the spec of
//! one server of its session ([`super::ToDaemon::Local`]); the daemon also checks that the
//! peer runs the worker's own binary, a guard against accidents: an agent of the same OS user
//! can still read what a server was given (its process environment, `rbw` itself).
//! The daemon resolves it ([`resolve`]): the program as an absolute path (from the daemon's
//! `PATH`), its environment (the child allowlist without `CLAUDE_*`/`ANTHROPIC_*`, then the
//! server's `env`), its working folder (the session's, unless `cwd`), or the URL and headers.
//! The spec is never logged or printed.
//!
//! The launcher then passes the messages between the CLI and the server, and pins the server's
//! tools ([`super::pins`]) unless its table says `pin = false`: it holds the server's answer to
//! `initialize`, tells the server `notifications/initialized` itself, reads the whole
//! `tools/list`, and checks the server's name and instructions and the tools before the agent
//! sees any of it. An answer of the server reaches the CLI only for a request of the CLI that
//! went to the server and is still open, so the server cannot answer the CLI's `tools/list`
//! itself. The
//! CLI's `tools/list` is answered from that checked list (names, descriptions and input
//! schemas only: exactly what was approved), and a `tools/call` of another name is refused. A
//! changed list fails the server: the CLI gets errors and the launcher exits, the daemon puts a
//! `system` line `mcp_tools_changed` into the chat. `notifications/tools/list_changed` is
//! checked the same way before the CLI hears of it. A server that cannot start (a missing
//! program, a value Vaultwarden did not give) says so in a `system` line `mcp_server_failed`.
//!
//! `http` is Streamable HTTP: every message is a `POST` with the headers, `Mcp-Session-Id` and
//! `MCP-Protocol-Version` once the server named them; an answer is JSON or an event stream,
//! whose messages all go to the CLI. Requests of the server outside an answer (the optional
//! `GET` stream) are not listened to. Redirects are not followed, so the headers go nowhere
//! else.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use super::{pins, FromDaemon, ToDaemon, MAX_LINE};
use crate::config::{LocalMcp, LocalMcpTransport, McpValue};

/// `system.code`: a local MCP server could not start (iteration 9).
pub const MCP_SERVER_FAILED: &str = "mcp_server_failed";
/// `system.code`: a local MCP server's tools differ from the pin, so it is off (T71a).
pub const MCP_TOOLS_CHANGED: &str = "mcp_tools_changed";

/// Longest message between the launcher and the server or the CLI.
const MAX_MESSAGE: usize = 16 * 1024 * 1024;
/// Most `tools/list` pages read for a pin.
const MAX_PAGES: usize = 100;
/// How long a program that lost its stdin has to exit before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(2);
/// Our own requests to the server; the CLI's ids are numbers.
const OWN_ID: &str = "hqc-pin-";

/// What the daemon gives the launcher. Holds secrets: no `Debug`, never logged or printed.
#[derive(Clone, Serialize, Deserialize)]
pub struct LocalSpec {
    pub environment: String,
    pub name: String,
    pub pin: bool,
    /// `$HQC_HOME/mcp-pins.json`.
    pub pins: PathBuf,
    pub transport: Resolved,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Resolved {
    Stdio {
        /// Absolute.
        command: PathBuf,
        args: Vec<String>,
        /// The whole environment of the program.
        env: Vec<(String, String)>,
        cwd: PathBuf,
    },
    Http {
        url: String,
        headers: Vec<(String, String)>,
    },
}

/// The environment a local server's program starts with before its own `env`: the child
/// allowlist of §8.1 without the variables of Claude Code and the Anthropic API, which are
/// no business of another program.
pub fn base_env() -> Vec<(String, String)> {
    crate::adapter::claude_code::child_env(std::env::vars())
        .into_iter()
        .filter(|(k, _)| !k.starts_with("CLAUDE_") && !k.starts_with("ANTHROPIC_"))
        .collect()
}

/// `command` as an absolute path: as written when it is one, else the first executable file
/// of that name in `path` (a `PATH` value).
pub fn find_program(command: &str, path: Option<&str>) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let runnable = |p: &Path| {
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if command.contains('/') {
        let p = PathBuf::from(command);
        return if runnable(&p) {
            Ok(p)
        } else {
            Err(format!(
                "the program {command} is missing or not executable"
            ))
        };
    }
    std::env::split_paths(path.unwrap_or(""))
        .filter(|d| d.is_absolute())
        .map(|d| d.join(command))
        .find(|p| runnable(p))
        .ok_or_else(|| format!("the program {command} is not found in PATH"))
}

async fn value(v: &McpValue, rbw: &Path, what: &str) -> Result<String, String> {
    match v {
        McpValue::Plain(s) => Ok(s.expose().to_string()),
        McpValue::Vault(r) => crate::vault::read_for(rbw, &r.entry, r.field.as_deref(), false)
            .await
            .map_err(|e| {
                format!(
                    "{what} could not be read from Vaultwarden entry {}: {e:#}",
                    r.entry
                )
            }),
    }
}

/// The daemon's half: the spec of server `name` of `local` for a session working in
/// `workdir`, its Vaultwarden values read now. `Err` says why, without a value.
pub async fn resolve(local: &LocalMcp, name: &str, workdir: &Path) -> Result<LocalSpec, String> {
    let Some(server) = local.servers.iter().find(|s| s.name == name) else {
        return Err(format!(
            "{name} is not an MCP server of the environment {}",
            local.environment
        ));
    };
    let transport = match &server.transport {
        LocalMcpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            let mut all = base_env();
            let path = all
                .iter()
                .find(|(k, _)| k == "PATH")
                .map(|(_, v)| v.clone());
            let command = find_program(command, path.as_deref())?;
            for (k, v) in env {
                let v = value(v, &local.rbw, &format!("env.{k}")).await?;
                all.retain(|(x, _)| x != k);
                all.push((k.clone(), v));
            }
            let cwd = cwd.clone().unwrap_or_else(|| workdir.to_path_buf());
            if !cwd.is_dir() {
                return Err(format!("the folder {} is missing", cwd.display()));
            }
            Resolved::Stdio {
                command,
                args: args.clone(),
                env: all,
                cwd,
            }
        }
        LocalMcpTransport::Http { url, headers } => {
            let mut all = Vec::new();
            for (k, v) in headers {
                let v = value(v, &local.rbw, &format!("headers.{k}")).await?;
                if v.contains(['\r', '\n']) {
                    return Err(format!("the value of headers.{k} has a line break"));
                }
                all.push((k.clone(), v));
            }
            Resolved::Http {
                url: url.clone(),
                headers: all,
            }
        }
    };
    Ok(LocalSpec {
        environment: local.environment.clone(),
        name: name.to_string(),
        pin: server.pin,
        pins: local.pins.clone(),
        transport,
    })
}

/// The launcher's connection to the daemon.
pub struct Daemon {
    rd: BufReader<tokio::net::unix::OwnedReadHalf>,
    wr: tokio::net::unix::OwnedWriteHalf,
    server: String,
}

impl Daemon {
    async fn connect(socket: &Path, token: Option<String>, server: &str) -> anyhow::Result<Self> {
        let token = token.with_context(|| format!("{} is not set", super::TOKEN_ENV))?;
        let stream = UnixStream::connect(socket)
            .await
            .context("the hqc worker daemon is not reachable")?;
        let (rd, wr) = stream.into_split();
        let mut d = Self {
            rd: BufReader::new(rd),
            wr,
            server: server.to_string(),
        };
        d.send(&ToDaemon::Hello {
            token,
            protocol_version: None,
            client: Some(json!({"name": "hqc-worker mcp --local", "server": server})),
        })
        .await?;
        match d.read().await? {
            FromDaemon::Ready => Ok(d),
            FromDaemon::Refused { error } => {
                anyhow::bail!("the daemon refused: {}: {}", error.code, error.message)
            }
            _ => anyhow::bail!("the daemon did not answer"),
        }
    }

    async fn send(&mut self, m: &ToDaemon) -> anyhow::Result<()> {
        let mut line = serde_json::to_vec(m)?;
        line.push(b'\n');
        self.wr.write_all(&line).await?;
        Ok(())
    }

    async fn read(&mut self) -> anyhow::Result<FromDaemon> {
        let mut buf = Vec::new();
        let n = (&mut self.rd)
            .take(MAX_LINE as u64 * 16)
            .read_until(b'\n', &mut buf)
            .await?;
        anyhow::ensure!(n > 0, "the daemon closed the connection");
        Ok(serde_json::from_slice(&buf)?)
    }

    async fn spec(&mut self) -> anyhow::Result<LocalSpec> {
        self.send(&ToDaemon::Local {
            id: 1,
            server: self.server.clone(),
        })
        .await?;
        match self.read().await? {
            FromDaemon::Result {
                result: Some(r),
                error: None,
                ..
            } => serde_json::from_value(r).context("the daemon's spec"),
            FromDaemon::Result { error: Some(e), .. } => {
                anyhow::bail!("{}: {}", e.code, e.message)
            }
            _ => anyhow::bail!("the daemon did not give the server's spec"),
        }
    }

    /// The chat hears of it (a `system` line) through the daemon.
    async fn failed(&mut self, code: &str, detail: &str) {
        let _ = self
            .send(&ToDaemon::LocalFailed {
                server: self.server.clone(),
                code: code.into(),
                detail: detail.into(),
            })
            .await;
        let _ = self.wr.flush().await;
    }
}

/// `hqc-worker mcp --local <name>`: serves the CLI on `input`/`output` until either side ends.
pub async fn run<R, W>(
    socket: &Path,
    token: Option<String>,
    name: &str,
    input: R,
    output: W,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut daemon = Daemon::connect(socket, token, name).await?;
    // The daemon puts the reason into the chat itself.
    let spec = daemon.spec().await?;
    let server = match start(&spec.transport) {
        Ok(s) => s,
        Err(e) => {
            daemon.failed(MCP_SERVER_FAILED, &format!("{e:#}")).await;
            return Err(e);
        }
    };
    let pin = spec
        .pin
        .then(|| (spec.pins.clone(), pins::key(&spec.environment, &spec.name)));
    serve(input, output, server, pin, Some(&mut daemon)).await
}

/// The other side of the launcher: the program, or the HTTP relay.
pub struct Server {
    pub tx: mpsc::UnboundedSender<Value>,
    pub rx: mpsc::UnboundedReceiver<Value>,
    child: Option<tokio::process::Child>,
}

impl Server {
    /// A server made of channels (tests).
    pub fn channels(tx: mpsc::UnboundedSender<Value>, rx: mpsc::UnboundedReceiver<Value>) -> Self {
        Self {
            tx,
            rx,
            child: None,
        }
    }
}

fn start(t: &Resolved) -> anyhow::Result<Server> {
    match t {
        Resolved::Stdio {
            command,
            args,
            env,
            cwd,
        } => start_stdio(command, args, env, cwd),
        Resolved::Http { url, headers } => start_http(url.clone(), headers.clone()),
    }
}

fn start_stdio(
    command: &Path,
    args: &[String],
    env: &[(String, String)],
    cwd: &Path,
) -> anyhow::Result<Server> {
    let mut child = tokio::process::Command::new(command)
        .args(args)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("could not start {}", command.display()))?;
    let mut stdin = child.stdin.take().expect("piped");
    let stdout = child.stdout.take().expect("piped");
    let (tx, mut to_rx) = mpsc::unbounded_channel::<Value>();
    let (from_tx, rx) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        while let Some(m) = to_rx.recv().await {
            let mut line = serde_json::to_vec(&m).expect("JSON serializes");
            line.push(b'\n');
            if stdin.write_all(&line).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
        // Dropping stdin tells the program to exit.
    });
    tokio::spawn(read_lines(stdout, from_tx, "the server"));
    Ok(Server {
        tx,
        rx,
        child: Some(child),
    })
}

/// JSON-RPC messages, one per line, from `r` into `tx` until EOF; other lines are skipped
/// with a note on stderr (which the CLI keeps in its MCP log).
async fn read_lines<R: AsyncRead + Unpin>(r: R, tx: mpsc::UnboundedSender<Value>, who: &str) {
    let mut r = BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match (&mut r)
            .take(MAX_MESSAGE as u64 + 1)
            .read_until(b'\n', &mut buf)
            .await
        {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if buf.len() > MAX_MESSAGE {
            eprintln!(
                "hqc-worker mcp --local: a message of {who} over {MAX_MESSAGE} bytes was dropped"
            );
            while !buf.ends_with(b"\n") {
                buf.clear();
                if !matches!(r.read_until(b'\n', &mut buf).await, Ok(n) if n > 0) {
                    return;
                }
            }
            continue;
        }
        if buf.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice::<Value>(&buf) {
            Ok(v) => {
                if tx.send(v).is_err() {
                    break;
                }
            }
            Err(_) => eprintln!("hqc-worker mcp --local: a line of {who} that is not JSON skipped"),
        }
    }
}

/// The HTTP relay's state shared by its requests.
#[derive(Default)]
struct HttpSession {
    id: Option<String>,
    protocol: Option<String>,
}

fn start_http(url: String, headers: Vec<(String, String)>) -> anyhow::Result<Server> {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .build()
        .context("an HTTP client")?;
    let (tx, mut to_rx) = mpsc::unbounded_channel::<Value>();
    let (from_tx, rx) = mpsc::unbounded_channel::<Value>();
    let session: Arc<Mutex<HttpSession>> = Arc::default();
    let headers = Arc::new(headers);
    let url = Arc::new(url);
    tokio::spawn(async move {
        while let Some(m) = to_rx.recv().await {
            let (http, url, headers, session, out) = (
                http.clone(),
                url.clone(),
                headers.clone(),
                session.clone(),
                from_tx.clone(),
            );
            // `initialize` goes first and alone: its answer names the session.
            let first = m.get("method").and_then(Value::as_str) == Some("initialize");
            let task = tokio::spawn(async move {
                let id = m.get("id").cloned().filter(|_| m.get("method").is_some());
                if let Err(e) = post(&http, &url, &headers, &session, &m, &out).await {
                    if let Some(id) = id {
                        let _ = out.send(json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {"code": -32603, "message": format!("the server at {url}: {e}")},
                        }));
                    }
                }
            });
            if first {
                let _ = task.await;
            }
        }
        // The CLI is gone: end the MCP session, if the server keeps one.
        let id = session.lock().unwrap().id.clone();
        if let Some(id) = id {
            let mut req = http.delete(url.as_str()).header("mcp-session-id", id);
            for (k, v) in headers.iter() {
                req = req.header(k.as_str(), v.as_str());
            }
            let _ = tokio::time::timeout(Duration::from_secs(2), req.send()).await;
        }
    });
    Ok(Server {
        tx,
        rx,
        child: None,
    })
}

/// One message to the HTTP server; whatever it answers goes to `out`.
async fn post(
    http: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
    session: &Mutex<HttpSession>,
    msg: &Value,
    out: &mpsc::UnboundedSender<Value>,
) -> anyhow::Result<()> {
    let mut req = http
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    {
        let s = session.lock().unwrap();
        if let Some(id) = &s.id {
            req = req.header("mcp-session-id", id.as_str());
        }
        if let Some(p) = &s.protocol {
            req = req.header("mcp-protocol-version", p.as_str());
        }
    }
    let mut resp = req
        .body(serde_json::to_vec(msg)?)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("{}", short(e)))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {status}");
    }
    let initialize = msg.get("method").and_then(Value::as_str) == Some("initialize");
    if initialize {
        if let Some(id) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            session.lock().unwrap().id = Some(id.to_string());
        }
    }
    let sse = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("text/event-stream"));
    let deliver = |v: Value| {
        let msgs = match v {
            Value::Array(a) => a,
            v => vec![v],
        };
        for m in msgs {
            if initialize && m.get("id") == msg.get("id") {
                if let Some(p) = m.pointer("/result/protocolVersion").and_then(Value::as_str) {
                    session.lock().unwrap().protocol = Some(p.to_string());
                }
            }
            let _ = out.send(m);
        }
    };
    let mut buf: Vec<u8> = Vec::new();
    let mut read = 0usize;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| anyhow::anyhow!("{}", short(e)))?
    {
        read += chunk.len();
        anyhow::ensure!(read <= MAX_MESSAGE, "an answer over {MAX_MESSAGE} bytes");
        buf.extend_from_slice(&chunk);
        if sse {
            for v in sse_events(&mut buf) {
                deliver(v);
            }
        }
    }
    if sse {
        buf.extend_from_slice(b"\n\n");
        for v in sse_events(&mut buf) {
            deliver(v);
        }
    } else if !buf.iter().all(u8::is_ascii_whitespace) {
        deliver(serde_json::from_slice(&buf).context("the answer is not JSON")?);
    }
    Ok(())
}

/// The complete events of an event stream taken out of `buf`, as JSON.
fn sse_events(buf: &mut Vec<u8>) -> Vec<Value> {
    let mut out = vec![];
    loop {
        let Some((end, skip)) = (0..buf.len()).find_map(|i| {
            if buf[i..].starts_with(b"\r\n\r\n") {
                Some((i, 4))
            } else if buf[i..].starts_with(b"\n\n") {
                Some((i, 2))
            } else {
                None
            }
        }) else {
            return out;
        };
        let event: Vec<u8> = buf.drain(..end + skip).take(end).collect();
        let event = String::from_utf8_lossy(&event);
        let data: Vec<&str> = event
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|l| l.strip_prefix(' ').unwrap_or(l))
            .collect();
        if let Ok(v) = serde_json::from_str::<Value>(&data.join("\n")) {
            out.push(v);
        }
    }
}

/// A transport error without the URL's query or anything of the request.
fn short(e: reqwest::Error) -> String {
    let e = e.without_url();
    let mut s = e.to_string();
    let mut src = std::error::Error::source(&e);
    while let Some(c) = src {
        s = format!("{s}: {c}");
        src = c.source();
    }
    s
}

/// What the launcher knows of the server's tools.
enum Tools {
    /// Not read yet: the CLI's `tools/list` and `tools/call` wait.
    Unknown,
    /// Checked: the CLI gets these (pinned fields only).
    Ready(Vec<Value>),
}

/// A `tools/list` of ours in progress.
struct Fetch {
    id: String,
    tools: Vec<Value>,
    pages: usize,
    /// Tell the CLI `notifications/tools/list_changed` once it is checked.
    announce: bool,
}

struct Proxy<'a> {
    cli: mpsc::UnboundedSender<Value>,
    server: mpsc::UnboundedSender<Value>,
    pin: Option<(PathBuf, String)>,
    tools: Tools,
    held: Vec<Value>,
    fetch: Option<Fetch>,
    /// A list change came during a fetch: read again after it.
    again: bool,
    seq: u64,
    daemon: Option<&'a mut Daemon>,
    /// The ids of the CLI's requests that went to the server and wait for its answer (as JSON
    /// text): an answer of the server to any other id is dropped, so that it cannot answer a
    /// request the launcher answers itself (`tools/list`) before the pin is checked.
    forwarded: HashSet<String>,
    /// The id of the CLI's `initialize`, until the server answers it.
    init_id: Option<String>,
    /// The server's answer to `initialize`, held until its name, instructions and tools are
    /// checked against the pin.
    init_answer: Option<Value>,
    /// The launcher told the server `notifications/initialized` (the CLI's own is dropped).
    initialized: bool,
    /// [`pins::server_meta`] of the server's answer to `initialize`.
    meta: Value,
}

/// Why the proxy stops.
enum Stop {
    CliGone,
    ServerGone,
    Failed,
}

/// Passes messages between the CLI (`input`/`output`) and `server`, pinning its tools when
/// `pin` (`(file, key)`) is given. Returns when either side ends or the pin fails.
pub async fn serve<R, W>(
    input: R,
    output: W,
    mut server: Server,
    pin: Option<(PathBuf, String)>,
    daemon: Option<&mut Daemon>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (cli_tx, mut cli_out) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        let mut output = output;
        while let Some(v) = cli_out.recv().await {
            let mut line = serde_json::to_vec(&v).expect("JSON serializes");
            line.push(b'\n');
            if output.write_all(&line).await.is_err() || output.flush().await.is_err() {
                break;
            }
        }
    });
    let (in_tx, mut cli_in) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(read_lines(input, in_tx, "the CLI"));
    let mut p = Proxy {
        cli: cli_tx,
        server: server.tx.clone(),
        pin,
        tools: Tools::Unknown,
        held: vec![],
        fetch: None,
        again: false,
        seq: 0,
        daemon,
        forwarded: HashSet::new(),
        init_id: None,
        init_answer: None,
        initialized: false,
        meta: Value::Null,
    };
    let stop = loop {
        tokio::select! {
            m = cli_in.recv() => match m {
                Some(m) => p.on_cli(m),
                None => break Stop::CliGone,
            },
            m = server.rx.recv() => match m {
                Some(m) => {
                    if !p.on_server(m).await {
                        break Stop::Failed;
                    }
                }
                None => break Stop::ServerGone,
            },
        }
    };
    if let Stop::ServerGone = stop {
        // Whatever the CLI still waits for will not come.
        if let Some(a) = p.init_answer.take() {
            p.error(&a, "the server exited");
        }
        for m in std::mem::take(&mut p.held) {
            p.error(&m, "the server exited");
        }
    }
    drop(p);
    drop(server.tx);
    if let Some(mut child) = server.child.take() {
        if tokio::time::timeout(EXIT_GRACE, child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
        }
    }
    let _ = writer.await;
    match stop {
        Stop::Failed => anyhow::bail!("the server's tools are not the approved ones"),
        _ => Ok(()),
    }
}

impl Proxy<'_> {
    fn error(&self, request: &Value, message: &str) {
        if let Some(id) = request.get("id") {
            let _ = self.cli.send(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32603, "message": message},
            }));
        }
    }

    fn on_cli(&mut self, m: Value) {
        if self.pin.is_none() {
            let _ = self.server.send(m);
            return;
        }
        let method = m.get("method").and_then(Value::as_str).unwrap_or("");
        let request = m.get("id").is_some();
        match (method, request) {
            ("initialize", true) => {
                self.init_id = m.get("id").map(Value::to_string);
                self.request(m);
            }
            // The launcher said it already, when the server answered `initialize`.
            ("notifications/initialized", false) if self.initialized => {}
            ("notifications/initialized", false) => {
                self.initialized = true;
                let _ = self.server.send(m);
                self.start_fetch(false);
            }
            ("tools/list" | "tools/call", true) => match &self.tools {
                Tools::Ready(_) => self.answer(m),
                Tools::Unknown => {
                    self.held.push(m);
                    if self.initialized {
                        self.start_fetch(false);
                    }
                }
            },
            ("notifications/cancelled", false) => {
                if let Some(id) = m.pointer("/params/requestId") {
                    self.forwarded.remove(&id.to_string());
                }
                let _ = self.server.send(m);
            }
            ("", _) | (_, false) => {
                // The CLI's answer to a request of the server, or a notification.
                let _ = self.server.send(m);
            }
            _ => self.request(m),
        }
    }

    /// A request of the CLI to the server: its answer is awaited.
    fn request(&mut self, m: Value) {
        if let Some(id) = m.get("id") {
            self.forwarded.insert(id.to_string());
        }
        let _ = self.server.send(m);
    }

    /// A `tools/list` or `tools/call` of the CLI once the tools are checked.
    fn answer(&mut self, m: Value) {
        let Tools::Ready(tools) = &self.tools else {
            return;
        };
        let id = m.get("id").cloned().unwrap_or(Value::Null);
        if m.get("method").and_then(Value::as_str) == Some("tools/list") {
            let _ = self
                .cli
                .send(json!({"jsonrpc": "2.0", "id": id, "result": {"tools": tools}}));
            return;
        }
        let name = m.pointer("/params/name").and_then(Value::as_str);
        if tools
            .iter()
            .any(|t| name.is_some() && t.get("name").and_then(Value::as_str) == name)
        {
            self.request(m);
        } else {
            let _ = self.cli.send(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32602, "message": format!("unknown tool {}", name.unwrap_or(""))},
            }));
        }
    }

    fn start_fetch(&mut self, announce: bool) {
        if let Some(f) = &mut self.fetch {
            f.announce |= announce;
            self.again |= announce;
            return;
        }
        self.fetch = Some(Fetch {
            id: String::new(),
            tools: vec![],
            pages: 0,
            announce,
        });
        self.page(None);
    }

    fn page(&mut self, cursor: Option<Value>) {
        self.seq += 1;
        let id = format!("{OWN_ID}{}", self.seq);
        let Some(f) = &mut self.fetch else { return };
        f.id = id.clone();
        f.pages += 1;
        let params = match cursor {
            Some(c) => json!({"cursor": c}),
            None => json!({}),
        };
        let _ = self
            .server
            .send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/list", "params": params}));
    }

    /// `false`: the server failed its pin and the launcher stops.
    async fn on_server(&mut self, m: Value) -> bool {
        if self.pin.is_none() {
            let _ = self.cli.send(m);
            return true;
        }
        if let Some(method) = m.get("method").and_then(Value::as_str) {
            if method == "notifications/tools/list_changed" && m.get("id").is_none() {
                // Checked before the CLI hears of it.
                self.start_fetch(true);
                return true;
            }
            // A request or notification of the server.
            let _ = self.cli.send(m);
            return true;
        }
        // An answer: to one of ours, or to a request of the CLI that went to the server and
        // was not answered yet. Anything else (an id the launcher answers itself, one it never
        // forwarded, a second answer, a batch) is dropped.
        let Some(id) = m.get("id") else {
            eprintln!("hqc-worker mcp --local: an answer of the server without an id dropped");
            return true;
        };
        if id.as_str().is_some_and(|id| id.starts_with(OWN_ID)) {
            return self.fetched(m).await;
        }
        let key = id.to_string();
        if !self.forwarded.remove(&key) {
            eprintln!("hqc-worker mcp --local: an answer of the server to {key}, which it was not asked, dropped");
            return true;
        }
        if self.init_id.as_deref() == Some(key.as_str()) {
            self.init_id = None;
            if let Some(r) = m.get("result").filter(|r| r.is_object()) {
                // Its name and instructions are pinned with its tools: the CLI gets the answer
                // once they are checked. The server is told it is initialized now, so that it
                // answers the launcher's `tools/list`.
                self.meta = pins::server_meta(r);
                self.init_answer = Some(m);
                self.initialized = true;
                let _ = self
                    .server
                    .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
                self.start_fetch(false);
                return true;
            }
        }
        let _ = self.cli.send(m);
        true
    }

    async fn fetched(&mut self, m: Value) -> bool {
        let Some(f) = &mut self.fetch else {
            return true;
        };
        if m.get("id").and_then(Value::as_str) != Some(f.id.as_str()) {
            return true;
        }
        // A server without tools answers an error: it has none.
        if let Some(page) = m.pointer("/result/tools").and_then(Value::as_array) {
            f.tools.extend(page.iter().cloned());
        }
        let next = m
            .pointer("/result/nextCursor")
            .filter(|c| !c.is_null())
            .cloned();
        if let Some(c) = next {
            if f.pages < MAX_PAGES {
                self.page(Some(c));
                return true;
            }
        }
        let f = self.fetch.take().expect("a fetch");
        let (path, key) = self.pin.clone().expect("pinned");
        let tools = f.tools.clone();
        let meta = self.meta.clone();
        let checked =
            tokio::task::spawn_blocking(move || pins::check(&path, &key, &meta, &tools)).await;
        let failure = match checked {
            Ok(Ok(pins::Check::Recorded | pins::Check::Same)) => None,
            Ok(Ok(pins::Check::Changed(d))) => Some((MCP_TOOLS_CHANGED, d.summary())),
            Ok(Err(e)) => Some((
                MCP_SERVER_FAILED,
                format!("its tools could not be checked against the pin: {e:#}"),
            )),
            Err(e) => Some((MCP_SERVER_FAILED, format!("the pin check failed: {e}"))),
        };
        if let Some((code, detail)) = failure {
            eprintln!("hqc-worker mcp --local: {code}: {detail}");
            if let Some(d) = self.daemon.as_mut() {
                d.failed(code, &detail).await;
            }
            if let Some(a) = self.init_answer.take() {
                self.error(
                    &a,
                    "this MCP server is off: it is not the one its owner approved",
                );
            }
            for h in std::mem::take(&mut self.held) {
                self.error(
                    &h,
                    "this MCP server is off: its tools are not the ones its owner approved",
                );
            }
            return false;
        }
        self.tools = Tools::Ready(pins::normalize(&f.tools).into_iter().map(shown).collect());
        if let Some(a) = self.init_answer.take() {
            let _ = self.cli.send(a);
        }
        for h in std::mem::take(&mut self.held) {
            self.answer(h);
        }
        if f.announce {
            let _ = self
                .cli
                .send(json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}));
        }
        if std::mem::take(&mut self.again) {
            self.start_fetch(true);
        }
        true
    }
}

/// A pinned tool as the CLI gets it: no `description` key when it had none.
fn shown(mut t: Value) -> Value {
    if let Some(o) = t.as_object_mut() {
        o.retain(|_, v| !v.is_null());
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_are_found_in_path() {
        let dir = tempfile::tempdir().unwrap();
        let prog = dir.path().join("figma-mcp");
        std::fs::write(&prog, "#!/bin/sh\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!("/nonexistent:relative:{}", dir.path().display());
        assert_eq!(find_program("figma-mcp", Some(&path)).unwrap(), prog);
        assert_eq!(find_program(prog.to_str().unwrap(), None).unwrap(), prog);
        assert!(find_program("figma-mcp", Some("/nonexistent"))
            .unwrap_err()
            .contains("not found in PATH"));
        // Not executable.
        std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(find_program("figma-mcp", Some(&path)).is_err());
    }

    #[test]
    fn the_base_env_has_no_claude_or_anthropic_variables() {
        for (k, _) in base_env() {
            assert!(
                !k.starts_with("CLAUDE_") && !k.starts_with("ANTHROPIC_") && !k.starts_with("HQC_"),
                "{k}"
            );
        }
    }

    #[test]
    fn event_streams_split_into_messages() {
        let mut buf = b"event: message\ndata: {\"id\":1}\n\ndata: {\"a\":\r\ndata: 2}\r\n\r\ndata: {\"partial\"".to_vec();
        let got = sse_events(&mut buf);
        assert_eq!(got, vec![json!({"id": 1}), json!({"a": 2})]);
        assert_eq!(buf, b"data: {\"partial\"");
    }
}
