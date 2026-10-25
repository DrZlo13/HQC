//! T32 live end-to-end (SPEC §7.1): session A's Claude Code turn calls `hqc_ask` on session
//! B (both on this machine, two hub sessions); B's agent gets the question with the peer
//! header, answers, and B's final message comes back to A as the tool result.
//!
//! Spends the subscription (`claude-haiku-5-5`, `--max-turns 2`: A spends its first model
//! turn on ToolSearch and the second on the call, so it never answers after the result; the
//! test checks the result), so it runs only with `HQC_LIVE_CLAUDE=1`:
//!
//! ```
//! HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_peers -- --nocapture
//! ```
//!
//! The hub runs in process against `HQC_TEST_DATABASE_URL`, the worker in process with the
//! real Claude Code adapter; the MCP server is the `hqc-worker` binary, built by the test.
//! The folder is `tmp/spike-t32-live-<uuid>` of this checkout.

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
async fn live_a_asks_b_and_gets_its_reply() {
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
        .join(format!("spike-t32-live-{}", Uuid::now_v7()));
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
    let mut sids = vec![];
    for title in ["Asker", "Helper"] {
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

    let (st, v) = hub
        .post(
            &format!("/api/sessions/{a}/input"),
            json!({"text": "Call the hqc_ask tool once with session \"live/helper\" and text \
                    \"Reply with exactly one word: pineapple\". Then tell me what it replied.",
                   "client_op_id": "live-ask"}),
        )
        .await;
    assert_eq!(st, 201, "{v}");

    // A's call and its result: B's final answer.
    let (call, result) = wait("A's hqc_ask result", 300, async || {
        let e = hub.events(&a).await;
        let call = e
            .iter()
            .find(|e| e["type"] == "tool.call" && e["payload"]["tool"] == "mcp__hqc__hqc_ask")?;
        let result = e.iter().find(|r| {
            r["type"] == "tool.result" && r["payload"]["call_id"] == call["payload"]["call_id"]
        })?;
        Some((call.clone(), result.clone()))
    })
    .await;
    println!("A's call: {call}\nA's result: {result}");
    let eb = hub.events(&b).await;
    println!("B's events: {eb:#?}");
    let question = eb
        .iter()
        .find(|e| e["type"] == "peer.message")
        .expect("the question in B");
    assert_eq!(question["payload"]["from"], "live/asker");
    assert_eq!(question["payload"]["mode"], "ask");
    assert_eq!(question["author"]["id"], a.as_str());
    let answer = eb
        .iter()
        .find(|e| {
            e["type"] == "agent.message"
                && e["payload"]["final"] == true
                && e["payload"]["in_reply_to"] == question["event_id"]
        })
        .expect("B's final answer to the question");
    let reply = answer["payload"]["text"].as_str().unwrap();
    assert!(reply.to_lowercase().contains("pineapple"), "{answer}");
    assert_ne!(result["payload"]["error"], true, "{result}");
    let preview = result["payload"]["result_preview"]
        .as_str()
        .unwrap_or_default();
    assert!(preview.contains("live/helper"), "{result}");
    assert!(preview.to_lowercase().contains("pineapple"), "{result}");

    // B's agent got the question with the header (its transcript).
    let s = hub.session(&b).await;
    let runtime: Uuid = s["agent"]["runtime_session_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let transcript =
        std::fs::read_to_string(paths::transcript_path(&claude_dir, &ws, &runtime)).unwrap();
    let header = format!(
        "[hqc peer from live/asker | mode=ask | request_id={}",
        question["payload"]["request_id"].as_str().unwrap()
    );
    assert!(transcript.contains(&header), "the header in B's transcript");
    // Nothing waits any more.
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pending_requests WHERE kind = 'ask' AND closed_at IS NULL",
    )
    .fetch_one(&hub.app.db)
    .await
    .unwrap();
    assert_eq!(open, 0);

    stop.cancel();
    task.await.unwrap().unwrap();
    hub.finish().await;
    let _ = std::fs::remove_dir_all(&run);
}
