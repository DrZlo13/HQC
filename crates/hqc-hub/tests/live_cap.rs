//! T39 live end-to-end (SPEC §7.1, §10.1): session A runs in Ask and asks session B, which
//! runs in Auto, to run a shell command. The question's chain carries A's Ask as a cap, so
//! B's worker switches B's running process to Ask for that turn: the command is asked about
//! (and denied here) instead of run by the classifier; the header shows who capped it, and
//! B is back in Auto when the turn ends.
//!
//! Spends the subscription (`claude-haiku-5-5`, `--max-turns 2`: A spends its first model
//! turn on ToolSearch and the second on the call; B its first on the command and the second
//! on the reply), so it runs only with `HQC_LIVE_CLAUDE=1`:
//!
//! ```
//! HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_cap -- --nocapture
//! ```
//!
//! The hub runs in process against `HQC_TEST_DATABASE_URL`, the worker in process with the
//! real Claude Code adapter; the MCP server is the `hqc-worker` binary, built by the test.
//! The folder is `tmp/spike-t39-live-<uuid>` of this checkout.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use hqc_worker::adapter::claude_code::process::OsSpawner;
use hqc_worker::adapter::claude_code::{ClaudeCodeAdapters, ClaudeOptions, McpLaunch};
use hqc_worker::config::Config;
use hqc_worker::connection::{ConnOptions, Status};
use hqc_worker::token::Secret;
use hqc_worker::Worker;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MODEL: &str = "claude-haiku-5-5";

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

#[tokio::test(flavor = "multi_thread")]
async fn live_a_in_ask_asks_b_in_auto_and_b_asks_too() {
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
        .join(format!("spike-t39-live-{}", Uuid::now_v7()));
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
        ask_mode: hqc_worker::capabilities::claude_permission_mode(None).await,
        ..ClaudeOptions::default()
    };
    let adapters = ClaudeCodeAdapters::new(opts, Arc::new(OsSpawner)).with_mcp(McpLaunch {
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
    let mut sids = vec![];
    for (title, mode) in [("Asker", "ask"), ("Helper", "auto")] {
        let (st, s) = hub
            .post(
                &format!("/api/projects/{pid}/sessions"),
                json!({"title": title, "workspace_id": w["id"], "model": MODEL,
                       "permission_mode": mode}),
            )
            .await;
        assert_eq!(st, 201, "{s}");
        sids.push(s["id"].as_str().unwrap().to_string());
    }
    let (a, b) = (sids[0].clone(), sids[1].clone());

    let (st, v) = hub
        .post(
            &format!("/api/sessions/{a}/input"),
            json!({"text": "Call the hqc_ask tool once with session \"live/helper\" and text \
                    \"Use the Bash tool to run exactly this command: touch t39-capped \
                    Then reply with one word: done\". Then tell me what it replied.",
                   "client_op_id": "live-cap"}),
        )
        .await;
    assert_eq!(st, 201, "{v}");

    // B's command is asked about: its turn runs in Ask under A's cap.
    let request = wait("B's approval request", 300, async || {
        hub.events(&b)
            .await
            .into_iter()
            .find(|e| e["type"] == "approval.request")
    })
    .await;
    println!("B's request: {request}");
    assert_eq!(request["payload"]["tool"], "Bash", "{request}");
    let v = hub.session(&b).await;
    println!("B while capped: {v}");
    assert_eq!(v["permission_mode"], "auto");
    assert_eq!(v["effective_permission_mode"], "ask");
    assert_eq!(v["permission_capped_by"], "live/asker");
    let question = hub
        .events(&b)
        .await
        .into_iter()
        .find(|e| e["type"] == "peer.message")
        .expect("the question in B");
    assert_eq!(question["chain"]["permission_cap"], "ask", "{question}");
    let deny = request["payload"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["canonical"] == "deny")
        .expect("a deny option")["id"]
        .clone();
    let rid = request["payload"]["request_id"].as_str().unwrap();
    let (st, r) = hub
        .post(
            &format!("/api/sessions/{b}/requests/{rid}"),
            json!({"option_id": deny}),
        )
        .await;
    assert_eq!(st, 200, "{r}");

    // B answers A; then it is back in its own Auto, no cap.
    let result = wait("A's hqc_ask result", 300, async || {
        let e = hub.events(&a).await;
        let call = e
            .iter()
            .find(|e| e["type"] == "tool.call" && e["payload"]["tool"] == "mcp__hqc__hqc_ask")?;
        e.iter()
            .find(|r| {
                r["type"] == "tool.result" && r["payload"]["call_id"] == call["payload"]["call_id"]
            })
            .cloned()
    })
    .await;
    println!("A's result: {result}");
    let v = wait("B back in auto", 60, async || {
        let v = hub.session(&b).await;
        (v["effective_permission_mode"] == "auto" && v.get("permission_capped_by").is_none())
            .then_some(v)
    })
    .await;
    println!("B after the turn: {v}");
    assert!(!ws.join("t39-capped").exists(), "the denied command ran");
    let eb = hub.events(&b).await;
    println!("B's events: {eb:#?}");

    stop.cancel();
    task.await.unwrap().unwrap();
    hub.finish().await;
    let _ = std::fs::remove_dir_all(&run);
}
