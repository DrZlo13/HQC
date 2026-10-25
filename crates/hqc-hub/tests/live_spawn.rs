//! T34 live end-to-end (SPEC §7): session A's Claude Code turn calls `hqc_spawn`; the hub
//! creates session B in the same workspace and B's first turn runs on the prompt, which B's
//! agent gets as a message from A.
//!
//! Spends the subscription (`claude-haiku-5-5`, `--max-turns 2`: A spends its first model
//! turn on ToolSearch and the second on the call), so it runs only with
//! `HQC_LIVE_CLAUDE=1`:
//!
//! ```
//! HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_spawn -- --nocapture
//! ```
//!
//! The hub runs in process against `HQC_TEST_DATABASE_URL`, the worker in process with the
//! real Claude Code adapter; the MCP server is the `hqc-worker` binary, built by the test.
//! The folder is `tmp/spike-t34-live-<uuid>` of this checkout.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use hqc_worker::adapter::claude_code::process::OsSpawner;
use hqc_worker::adapter::claude_code::{paths, ClaudeCodeAdapters, ClaudeOptions, McpLaunch};
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
async fn live_a_spawns_b_and_bs_first_turn_runs() {
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
        .join(format!("spike-t34-live-{}", Uuid::now_v7()));
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
    let claude_dir = opts.config_dir.clone().expect("claude config dir");
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
    let (st, s) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": "Spawner", "workspace_id": w["id"], "model": MODEL}),
        )
        .await;
    assert_eq!(st, 201, "{s}");
    let a = s["id"].as_str().unwrap().to_string();

    let (st, v) = hub
        .post(
            &format!("/api/sessions/{a}/input"),
            json!({"text": "Call the hqc_spawn tool once with project \"live\" and prompt \
                    \"Reply with exactly one word: pineapple\". Then tell me the address it returned.",
                   "client_op_id": "live-spawn"}),
        )
        .await;
    assert_eq!(st, 201, "{v}");

    // A's call and its result: B's address.
    let (call, result) = wait("A's hqc_spawn result", 300, async || {
        let e = hub.events(&a).await;
        let call = e
            .iter()
            .find(|e| e["type"] == "tool.call" && e["payload"]["tool"] == "mcp__hqc__hqc_spawn")?;
        let result = e.iter().find(|r| {
            r["type"] == "tool.result" && r["payload"]["call_id"] == call["payload"]["call_id"]
        })?;
        Some((call.clone(), result.clone()))
    })
    .await;
    println!("A's call: {call}\nA's result: {result}");
    assert_ne!(result["payload"]["error"], true, "{result}");
    let preview = result["payload"]["result_preview"]
        .as_str()
        .unwrap_or_default();
    assert!(preview.contains("live/session-2"), "{result}");

    // B: in the same workspace, started by A, its first turn answers the prompt.
    let (_, list) = hub.get(&format!("/api/projects/{pid}/sessions")).await;
    let b = list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["slug"] == "session-2")
        .expect("B in the project")
        .clone();
    assert_eq!(b["workspace_id"], w["id"]);
    assert_eq!(b["agent"]["model"], MODEL);
    assert_eq!(
        b["spawned_by"],
        json!({"session_id": a, "address": "live/spawner"})
    );
    let b = b["id"].as_str().unwrap().to_string();
    let (prompt, answer) = wait("B's first answer", 300, async || {
        let e = hub.events(&b).await;
        let prompt = e.iter().find(|e| e["type"] == "peer.message")?.clone();
        let answer = e
            .iter()
            .find(|e| {
                e["type"] == "agent.message"
                    && e["payload"]["final"] == true
                    && e["payload"]["in_reply_to"] == prompt["event_id"]
            })?
            .clone();
        Some((prompt, answer))
    })
    .await;
    println!("B's prompt: {prompt}\nB's answer: {answer}");
    assert_eq!(prompt["payload"]["from"], "live/spawner");
    assert_eq!(prompt["payload"]["mode"], "send");
    assert_eq!(prompt["author"]["id"], a.as_str());
    let reply = answer["payload"]["text"].as_str().unwrap();
    assert!(reply.to_lowercase().contains("pineapple"), "{answer}");

    // B's agent got the prompt with A's header (its transcript).
    let s = hub.session(&b).await;
    let runtime: Uuid = s["agent"]["runtime_session_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let transcript =
        std::fs::read_to_string(paths::transcript_path(&claude_dir, &ws, &runtime)).unwrap();
    assert!(
        transcript.contains("[hqc peer from live/spawner | mode=send]"),
        "the header in B's transcript"
    );

    stop.cancel();
    task.await.unwrap().unwrap();
    hub.finish().await;
    let _ = std::fs::remove_dir_all(&run);
}
