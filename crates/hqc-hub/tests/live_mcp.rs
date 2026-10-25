//! T30 live end-to-end (SPEC §7, §7.2): a Claude Code turn calls `hqc_list_sessions` through
//! the real pipe (CLI -> `hqc-worker mcp` -> the daemon's socket -> `mcp.call` -> hub) and
//! sees its own session; a second turn checks with Bash that the agent's environment has no
//! `HQC_*` variable. Then the session token is looked for where it must not be: the
//! transcript, `~/.claude.json`, the hub's events, the worker's outbox.
//!
//! Spends the subscription (`claude-haiku-5-5`, `--max-turns 2`, two prompts in one process),
//! so it runs only with `HQC_LIVE_CLAUDE=1`:
//!
//! ```
//! HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_mcp -- --nocapture
//! ```
//!
//! The hub runs in process against `HQC_TEST_DATABASE_URL`, the worker in process with the
//! real Claude Code adapter; the MCP server is the `hqc-worker` binary, built by the test.
//! The folder is `tmp/spike-t30-live-<uuid>` of this checkout.

mod common;

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use common::*;
use hqc_worker::adapter::claude_code::process::{CommandSpec, OsSpawner, Spawned, Spawner};
use hqc_worker::adapter::claude_code::{paths, ClaudeCodeAdapters, ClaudeOptions, McpLaunch};
use hqc_worker::config::Config;
use hqc_worker::connection::{ConnOptions, Status};
use hqc_worker::token::Secret;
use hqc_worker::Worker;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, ReadBuf};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MODEL: &str = "claude-haiku-5-5";

/// The CLI's stdout, copied as it is read.
struct Tee {
    inner: Pin<Box<dyn AsyncRead + Send>>,
    copy: Arc<Mutex<Vec<u8>>>,
}

impl AsyncRead for Tee {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let r = self.inner.as_mut().poll_read(cx, buf);
        let new = buf.filled()[before..].to_vec();
        self.copy.lock().unwrap().extend_from_slice(&new);
        r
    }
}

/// The real spawner, remembering command lines and stdout; clones share them.
#[derive(Default, Clone)]
struct Recording {
    commands: Arc<Mutex<Vec<CommandSpec>>>,
    stdout: Arc<Mutex<Vec<u8>>>,
}

impl Spawner for Recording {
    fn spawn(&self, cmd: &CommandSpec) -> std::io::Result<Spawned> {
        self.commands.lock().unwrap().push(cmd.clone());
        let mut s = OsSpawner.spawn(cmd)?;
        s.stdout = Box::pin(Tee {
            inner: s.stdout,
            copy: self.stdout.clone(),
        });
        Ok(s)
    }
}

async fn wait<T>(what: &str, secs: u64, mut f: impl AsyncFnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// The `hqc-worker` binary next to this test's, built first.
fn worker_binary() -> std::path::PathBuf {
    let st = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", "hqc-worker", "--bin", "hqc-worker"])
        .status()
        .expect("cargo build runs");
    assert!(st.success());
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(|d| d.parent())
        .unwrap()
        .join("hqc-worker");
    assert!(bin.exists(), "{}", bin.display());
    bin
}

/// Runs one prompt; answers an approval with `once` if one comes. Returns the turn's events.
async fn turn(hub: &TestHub, sid: &str, text: &str, op: &str) -> Vec<Value> {
    let before = hub.events(sid).await.len();
    let (st, v) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": text, "client_op_id": op}),
        )
        .await;
    assert_eq!(st, 201, "{v}");
    let mut answered = std::collections::HashSet::new();
    wait("the turn", 240, async || {
        let e = hub.events(sid).await;
        let new = &e[before..];
        for r in new.iter().filter(|e| e["type"] == "approval.request") {
            let rid = r["payload"]["request_id"].as_str().unwrap().to_string();
            if answered.insert(rid.clone()) {
                let once = r["payload"]["options"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|o| o["canonical"] == "once")
                    .unwrap()["id"]
                    .clone();
                let (st, v) = hub
                    .post(
                        &format!("/api/sessions/{sid}/requests/{rid}"),
                        json!({"option_id": once}),
                    )
                    .await;
                println!("approved {rid}: {st} {v}");
            }
        }
        let done = new.iter().any(|e| {
            (e["type"] == "agent.message" && e["payload"]["final"] == true)
                || (e["type"] == "status"
                    && (e["payload"]["state"] == "idle" || e["payload"]["state"] == "error"))
        });
        done.then(|| new.to_vec())
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn live_list_sessions_sees_its_own_session() {
    if std::env::var("HQC_LIVE_CLAUDE").as_deref() != Ok("1") {
        eprintln!("skipped: set HQC_LIVE_CLAUDE=1 to run against the real claude");
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter("hqc_worker=info,hqc_hub=info")
        .with_test_writer()
        .try_init();
    let run = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tmp")
        .join(format!("spike-t30-live-{}", Uuid::now_v7()));
    std::fs::create_dir_all(run.join("ws")).unwrap();
    std::fs::create_dir_all(run.join("home")).unwrap();
    let run = run.canonicalize().unwrap();
    let ws = run.join("ws");
    let socket = hqc_worker::mcp::socket_path(&run.join("home"));
    let program = worker_binary();

    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let config = Config::parse(&format!(
        "hub_url = {:?}\nallowed_roots = [{:?}]\nidle_timeout = 0\nallow_foreign_sessions = false\n",
        hub.base,
        run.display().to_string()
    ))
    .unwrap();
    let opts = ClaudeOptions {
        extra_args: vec!["--max-turns".into(), "2".into()],
        permission_mode: hqc_worker::capabilities::claude_permission_mode(None).await,
        ..ClaudeOptions::default()
    };
    let claude_dir = opts.config_dir.clone().expect("claude config dir");
    let rec = Recording::default();
    let adapters = ClaudeCodeAdapters::new(opts, Arc::new(rec.clone())).with_mcp(McpLaunch {
        program,
        socket: socket.clone(),
    });
    let worker = Worker {
        config,
        token: Secret::new(&token),
        boot_id: Uuid::new_v4(),
        adapters: Arc::new(adapters),
        capabilities: hqc_worker::capabilities::detect(None).await,
        outbox_dir: run.join("home/outbox"),
        conn: ConnOptions::default(),
        shutdown_timeout: Duration::from_secs(10),
        machine_id: None,
        known_paths_file: run.join("home/known_workspaces.json"),
        mcp_socket: Some(socket),
        logs: None,
        mirror: None,
    };
    let stop = CancellationToken::new();
    let (status_tx, mut status) = tokio::sync::watch::channel(Status::Connecting);
    let task = tokio::spawn(worker.run(stop.clone(), status_tx));
    tokio::time::timeout(
        Duration::from_secs(20),
        status.wait_for(|s| matches!(s, Status::Connected { .. })),
    )
    .await
    .expect("worker connects")
    .unwrap();

    let (_, workers) = hub.get("/api/workers").await;
    let wid = workers["workers"][0]["id"].as_str().unwrap().to_string();
    let (st, p) = hub.post("/api/projects", json!({"name": "live"})).await;
    assert_eq!(st, 201);
    let pid = p["id"].as_str().unwrap().to_string();
    let (st, w) = hub
        .post(
            "/api/workspaces",
            json!({"project_id": pid, "worker_id": wid, "path": ws.display().to_string(),
                   "mirror_enabled": false}),
        )
        .await;
    assert_eq!(st, 201, "{w}");
    let (st, s) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": "Live MCP", "workspace_id": w["id"], "model": MODEL}),
        )
        .await;
    assert_eq!(st, 201, "{s}");
    let sid = s["id"].as_str().unwrap().to_string();

    // 1. The tool call.
    let evs = turn(
        &hub,
        &sid,
        "Call the hqc_list_sessions tool once, then reply with the address of the session \
         marked current.",
        "live-1",
    )
    .await;
    println!("turn 1: {evs:#?}");
    let call = evs
        .iter()
        .find(|e| e["type"] == "tool.call" && e["payload"]["tool"] == "mcp__hqc__hqc_list_sessions")
        .expect("the agent called hqc_list_sessions");
    let result = evs
        .iter()
        .find(|e| {
            e["type"] == "tool.result" && e["payload"]["call_id"] == call["payload"]["call_id"]
        })
        .expect("its result");
    let text = result.to_string();
    assert!(text.contains("live/live-mcp"), "{result}");
    assert!(text.contains("current"), "{result}");
    assert_ne!(result["payload"]["error"], true, "{result}");

    // 2. The agent's environment.
    let evs = turn(
        &hub,
        &sid,
        "Run this shell command and reply with its output only: env | grep -c '^HQC_' || true",
        "live-2",
    )
    .await;
    println!("turn 2: {evs:#?}");
    let bash = evs
        .iter()
        .find(|e| e["type"] == "tool.result")
        .expect("the Bash call ran");
    println!("bash: {bash}");

    // Where the token must not be.
    let cmd = rec.commands.lock().unwrap()[0].clone();
    let i = cmd.args.iter().position(|a| a == "--mcp-config").unwrap();
    let cfg: Value = serde_json::from_str(&cmd.args[i + 1]).unwrap();
    let session_token = cfg["mcpServers"]["hqc"]["env"]["HQC_SESSION_TOKEN"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(session_token.starts_with("hqcs_"));
    assert!(cmd
        .env
        .iter()
        .any(|(k, v)| k == "MCP_TOOL_TIMEOUT" && v == "660000"));
    assert!(!cmd.env.iter().any(|(k, _)| k.starts_with("HQC_")));
    assert!(!bash.to_string().contains(&session_token));
    let s = hub.session(&sid).await;
    let runtime: Uuid = s["agent"]["runtime_session_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let transcript = paths::transcript_path(&claude_dir, &ws, &runtime);
    let lines = std::fs::read(&transcript).unwrap();
    assert!(
        contains(&lines, "hqc_list_sessions"),
        "the transcript has the call"
    );
    assert!(!contains(&lines, &session_token), "token in the transcript");
    if let Some(home) = std::env::var_os("HOME") {
        let cj = std::path::Path::new(&home).join(".claude.json");
        if let Ok(b) = std::fs::read(&cj) {
            assert!(!contains(&b, &session_token), "token in ~/.claude.json");
        }
    }
    let all = serde_json::to_string(&hub.events(&sid).await).unwrap();
    assert!(!all.contains(&session_token), "token in the hub's events");
    for e in std::fs::read_dir(run.join("home/outbox")).unwrap() {
        let b = std::fs::read(e.unwrap().path()).unwrap();
        assert!(!contains(&b, &session_token), "token in the outbox");
    }

    // What the CLI reports about its MCP servers: only `hqc` (`--strict-mcp-config`).
    let out = String::from_utf8_lossy(&rec.stdout.lock().unwrap()).into_owned();
    let init: Value = out
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["type"] == "system" && v["subtype"] == "init")
        .expect("system/init");
    println!(
        "claude_code_version={} mcp_servers={} mcp tools={:?}",
        init["claude_code_version"],
        init["mcp_servers"],
        init["tools"].as_array().map(|t| t
            .iter()
            .filter(|x| x.as_str().is_some_and(|s| s.starts_with("mcp__")))
            .collect::<Vec<_>>())
    );
    let servers: Vec<&str> = init["mcp_servers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(servers, ["hqc"], "{}", init["mcp_servers"]);
    assert!(!out.contains(&session_token), "token on the CLI's stdout");

    stop.cancel();
    task.await.unwrap().unwrap();
    hub.finish().await;
    let _ = std::fs::remove_dir_all(&run);
}
