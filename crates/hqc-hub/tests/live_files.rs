//! T41 live end-to-end (SPEC §7, §7.1, §12): session A's Claude Code turn calls `hqc_ask` on
//! session B (both on this machine, in two workspaces); B's agent attaches a file of its
//! workspace with `hqc_attach` and answers; A's tool result lists a local path in A's own
//! `.hqc/inbox` with the same content.
//!
//! Spends the subscription (`claude-haiku-5-5`, `--max-turns 2`), so it runs only with
//! `HQC_LIVE_CLAUDE=1`:
//!
//! ```
//! HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_files -- --nocapture
//! ```
//!
//! B needs two model turns (`hqc_attach`, then its answer), so the CLI runs with
//! `ENABLE_TOOL_SEARCH=false` (a wrapper script in the run folder): the hqc tools are loaded
//! up front instead of costing a `ToolSearch` turn. The hub runs in process against
//! `HQC_TEST_DATABASE_URL`, the worker in process with the real Claude Code adapter; the MCP
//! server is the `hqc-worker` binary, built by the test. The folder is
//! `tmp/spike-t41-live-<uuid>` of this checkout.

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
use serde_json::{json, Value};
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

/// `claude` on `PATH`.
fn claude_on_path() -> std::path::PathBuf {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join("claude"))
        .find(|p| p.is_file())
        .expect("claude on PATH")
}

#[tokio::test(flavor = "multi_thread")]
async fn live_a_asks_b_and_gets_its_file() {
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
        .join(format!("spike-t41-live-{}", Uuid::now_v7()));
    for d in ["ws-a", "ws-b", "home"] {
        std::fs::create_dir_all(run.join(d)).unwrap();
    }
    let run = run.canonicalize().unwrap();
    let (ws_a, ws_b) = (run.join("ws-a"), run.join("ws-b"));
    let content = format!("T41 live report {}\n", Uuid::now_v7());
    std::fs::write(ws_b.join("report.txt"), &content).unwrap();
    let wrapper = run.join("claude");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nENABLE_TOOL_SEARCH=false exec {:?} \"$@\"\n",
            claude_on_path().display().to_string()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
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
        claude_bin: Some(wrapper),
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
    let mut sids = vec![];
    for (title, ws) in [("Asker", &ws_a), ("Helper", &ws_b)] {
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
                    \"Call the hqc_attach tool once with path report.txt, then reply with \
                    exactly one word: done\". Then tell me the path in the result's files.",
                   "client_op_id": "live-files"}),
        )
        .await;
    assert_eq!(st, 201, "{v}");

    // B attaches and answers.
    let (attachment, answer) = wait("B's attachment and answer", 300, async || {
        let e = hub.events(&b).await;
        let a = e.iter().find(|e| e["type"] == "attachment")?.clone();
        let f = e
            .iter()
            .find(|e| e["type"] == "agent.message" && e["payload"]["final"] == true)?
            .clone();
        Some((a, f))
    })
    .await;
    println!("B's attachment: {attachment}\nB's answer: {answer}");
    assert_eq!(attachment["payload"]["name"], "report.txt");

    // A's call and its result, with a local path to the file.
    let (call, result) = wait("A's hqc_ask result", 120, async || {
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
    assert_ne!(result["payload"]["error"], true, "{result}");
    let rid: String = sqlx::query_scalar(
        "SELECT request_id FROM pending_requests WHERE kind = 'ask' AND session_id = $1",
    )
    .bind(a.parse::<Uuid>().unwrap())
    .fetch_one(&hub.app.db)
    .await
    .unwrap();
    let local = ws_a.join(format!(".hqc/inbox/{rid}/report.txt"));
    assert_eq!(std::fs::read_to_string(&local).unwrap(), content);
    let preview = result["payload"]["result_preview"]
        .as_str()
        .unwrap_or_default();
    let shown: Value = serde_json::from_str(preview).expect("the result is JSON");
    assert_eq!(shown["from"], "live/helper");
    assert_eq!(
        shown["files"],
        json!([{"name": "report.txt", "kind": "text", "path": local.display().to_string(),
                "size": content.len()}]),
        "{preview}"
    );
    let ea = hub.events(&a).await;
    let last = ea
        .iter()
        .rev()
        .find(|e| e["type"] == "agent.message")
        .map(|e| e["payload"]["text"].clone());
    println!("A's last message: {last:?}");

    stop.cancel();
    task.await.unwrap().unwrap();
    hub.finish().await;
    let _ = std::fs::remove_dir_all(&run);
}
