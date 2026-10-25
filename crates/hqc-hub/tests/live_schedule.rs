//! T35 live end-to-end (SPEC §16, §7): session A's Claude Code turn calls `hqc_schedule`
//! for session B with a daily `cron`; the hub's clock is then moved to the run, the cron sends
//! the message into B as the owner's `user.message`, and B's agent answers it.
//!
//! Spends the subscription (`claude-haiku-5-5`, `--max-turns 2`: A spends its first model
//! turn on ToolSearch and the second on the call; B answers in one), so it runs only with
//! `HQC_LIVE_CLAUDE=1`:
//!
//! ```
//! HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_schedule -- --nocapture
//! ```
//!
//! The hub runs in process against `HQC_TEST_DATABASE_URL`, the worker in process with the
//! real Claude Code adapter; the MCP server is the `hqc-worker` binary, built by the test.
//! The folder is `tmp/spike-t35-live-<uuid>` of this checkout.

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
async fn live_a_schedules_a_message_into_b() {
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
        .join(format!("spike-t35-live-{}", Uuid::now_v7()));
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
    for title in ["Planner", "Target"] {
        let (st, s) = hub
            .post(
                &format!("/api/projects/{pid}/sessions"),
                json!({"title": title, "workspace_id": w["id"], "model": MODEL}),
            )
            .await;
        assert_eq!(st, 201, "{s}");
        sids.push(s["id"].as_str().unwrap().to_string());
    }
    let (a, b) = (sids[0].clone(), sids[1].clone());

    let prompt = "Call the hqc_schedule tool once with session \"live/target\", cron \
        \"0 9 * * *\", timezone \"Europe/Berlin\" and message \"Reply with exactly one word: \
        mango\". Then tell me what it returned.";
    let (st, v) = hub
        .post(
            &format!("/api/sessions/{a}/input"),
            json!({"text": prompt, "client_op_id": "live-schedule"}),
        )
        .await;
    assert_eq!(st, 201, "{v}");

    let (call, result) = wait("A's hqc_schedule result", 300, async || {
        let e = hub.events(&a).await;
        let call = e.iter().find(|e| {
            e["type"] == "tool.call" && e["payload"]["tool"] == "mcp__hqc__hqc_schedule"
        })?;
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
    assert!(preview.contains("live/target"), "{result}");
    let (st, l) = hub.get(&format!("/api/schedules?session_id={b}")).await;
    assert_eq!(st, 200);
    println!("B's schedules: {l:#}");
    let s = l["schedules"][0].clone();
    assert_eq!(s["cron"], "0 9 * * *");
    assert_eq!(s["timezone"], "Europe/Berlin");
    let next: chrono::DateTime<chrono::Utc> = s["next_run"].as_str().unwrap().parse().unwrap();

    // The run comes: the hub's clock moves there and the cron sends the message.
    hub.app.clock.set(next + chrono::TimeDelta::seconds(1));
    assert_eq!(hqc_hub::schedules::run_due(&hub.app).await.unwrap(), 1);
    let answer = wait("B's answer", 300, async || {
        let e = hub.events(&b).await;
        let input = e
            .iter()
            .find(|e| e["type"] == "user.message" && e["payload"]["schedule_id"] == s["id"])?;
        e.iter()
            .find(|e| {
                e["type"] == "agent.message"
                    && e["payload"]["final"] == true
                    && e["payload"]["in_reply_to"] == input["event_id"]
            })
            .cloned()
    })
    .await;
    println!("B's answer: {answer}");
    let text = answer["payload"]["text"].as_str().unwrap().to_lowercase();
    assert!(text.contains("mango"), "{answer}");

    stop.cancel();
    task.await.unwrap().unwrap();
    hub.finish().await;
    let _ = std::fs::remove_dir_all(&run);
}
