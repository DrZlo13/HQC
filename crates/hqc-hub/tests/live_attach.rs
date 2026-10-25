//! T31 live end-to-end (SPEC §7, §12): a Claude Code turn calls `hqc_attach` on a small file
//! of its workspace through the real pipe (CLI -> `hqc-worker mcp` -> the daemon, which reads
//! and uploads the file with `PUT /worker/artifacts` -> `mcp.call` -> hub), and the
//! `attachment` event authored by the agent appears in the session with the file's content.
//!
//! Spends the subscription (`claude-haiku-5-5`, `--max-turns 2`: the first model turn goes to
//! `ToolSearch`), so it runs only with `HQC_LIVE_CLAUDE=1`:
//!
//! ```
//! HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_attach -- --nocapture
//! ```
//!
//! The hub runs in process against `HQC_TEST_DATABASE_URL`, the worker in process with the
//! real Claude Code adapter; the MCP server is the `hqc-worker` binary, built by the test.
//! The folder is `tmp/spike-t31-live-<uuid>` of this checkout.

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
const CONTENT: &str = "hqc T31 live attachment\n";

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
async fn live_attach_writes_the_event() {
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
        .join(format!("spike-t31-live-{}", Uuid::now_v7()));
    std::fs::create_dir_all(run.join("ws")).unwrap();
    std::fs::create_dir_all(run.join("home")).unwrap();
    let run = run.canonicalize().unwrap();
    let ws = run.join("ws");
    std::fs::write(ws.join("report.txt"), CONTENT).unwrap();
    let socket = hqc_worker::mcp::socket_path(&run.join("home"));

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
    let adapters = ClaudeCodeAdapters::new(opts, Arc::new(OsSpawner)).with_mcp(McpLaunch {
        program: worker_binary(),
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
            json!({"title": "Live attach", "workspace_id": w["id"], "model": MODEL}),
        )
        .await;
    assert_eq!(st, 201, "{s}");
    let sid = s["id"].as_str().unwrap().to_string();

    let (st, v) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "Call the hqc_attach tool once with path \"report.txt\" and caption \
                            \"live\". Do nothing else.",
                   "client_op_id": "live-attach"}),
        )
        .await;
    assert_eq!(st, 201, "{v}");
    let deadline = Instant::now() + Duration::from_secs(240);
    let evs: Vec<Value> = loop {
        let e = hub.events(&sid).await;
        let done = e.iter().any(|e| {
            (e["type"] == "agent.message" && e["payload"]["final"] == true)
                || (e["type"] == "status"
                    && (e["payload"]["state"] == "idle" || e["payload"]["state"] == "error"))
        });
        if done {
            break e;
        }
        assert!(Instant::now() < deadline, "timed out waiting for the turn");
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    println!("events: {evs:#?}");

    let call = evs
        .iter()
        .find(|e| e["type"] == "tool.call" && e["payload"]["tool"] == "mcp__hqc__hqc_attach")
        .expect("the agent called hqc_attach");
    let result = evs
        .iter()
        .find(|e| {
            e["type"] == "tool.result" && e["payload"]["call_id"] == call["payload"]["call_id"]
        })
        .expect("its result");
    assert_ne!(result["payload"]["error"], true, "{result}");
    let att: Vec<&Value> = evs.iter().filter(|e| e["type"] == "attachment").collect();
    assert_eq!(att.len(), 1, "{att:#?}");
    let a = att[0];
    assert_eq!(a["author"], json!({"kind": "agent", "id": sid}));
    assert_eq!(a["payload"]["name"], "report.txt");
    assert_eq!(a["payload"]["kind"], "text");
    assert_eq!(a["payload"]["caption"], "live");
    // Between the call and its result.
    let seq = |e: &Value| e["seq"].as_i64().unwrap();
    assert!(
        seq(call) < seq(a) && seq(a) < seq(result),
        "{call} {a} {result}"
    );
    assert!(result.to_string().contains(a["event_id"].as_str().unwrap()));

    // The content as the user downloads it.
    let aid = a["payload"]["artifact_id"].as_str().unwrap();
    let (st, link) = hub.get(&format!("/api/artifacts/{aid}/url")).await;
    assert_eq!(st, 200, "{link}");
    let body = hub
        .http
        .get(format!("{}{}", hub.base, link["url"].as_str().unwrap()))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, CONTENT);

    stop.cancel();
    task.await.unwrap().unwrap();
    hub.finish().await;
    let _ = std::fs::remove_dir_all(&run);
}
