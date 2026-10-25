//! T20 live (SPEC §9.3): a Claude Code process outside hqc holds a session open; the worker
//! sees it in Claude Code's registry of live sessions (`~/.claude/sessions`), the hub refuses
//! the takeover with `409 vscode_open`, and allows it at once when the process has gone.
//!
//! The holder is `claude -p` started in the background by `tools/spike/driver.py` (stream-json,
//! the way an SDK client runs it) and kept alive by the `slow` tool of
//! `tools/spike/slow-mcp.js`. Spends the subscription (one run, `claude-haiku-5-5`,
//! `--max-turns 2`: ToolSearch, then the tool), so it runs only with `HQC_LIVE_CLAUDE=1`:
//!
//! ```
//! HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_holder -- --nocapture
//! ```
//!
//! The hub runs in process against `HQC_TEST_DATABASE_URL`, the worker in process with the
//! real adapter, the mirror and the registry; the folder is `tmp/spike-t20-live-<uuid>/ws`.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use hqc_worker::adapter::claude_code::process::OsSpawner;
use hqc_worker::adapter::claude_code::{paths, ClaudeCodeAdapters, ClaudeOptions};
use hqc_worker::config::Config;
use hqc_worker::connection::{ConnOptions, Status};
use hqc_worker::token::Secret;
use hqc_worker::{MirrorOptions, Worker};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

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

/// The driver and its `claude`, killed if the test fails half way.
struct Holder(std::process::Child);

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn live_holder_blocks_takeover() {
    if std::env::var("HQC_LIVE_CLAUDE").as_deref() != Ok("1") {
        eprintln!("skipped: set HQC_LIVE_CLAUDE=1 to run against the real claude");
        return;
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let run = repo
        .join("tmp")
        .join(format!("spike-t20-live-{}", Uuid::now_v7()));
    std::fs::create_dir_all(run.join("ws")).unwrap();
    let ws = run.join("ws");
    let ws_path = ws.display().to_string();

    // 1. Hub and worker with the registry.
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
    let adapters = ClaudeCodeAdapters::new(opts, Arc::new(OsSpawner));
    let own = adapters.own_records();
    let mut capabilities = hqc_worker::capabilities::detect(None).await;
    hqc_worker::capabilities::with_sessions_registry(
        &mut capabilities,
        &claude_dir.join("sessions"),
    );
    assert_eq!(
        capabilities["claude_code"]["sessions_registry"], true,
        "{capabilities}"
    );
    let worker = Worker {
        config,
        token: Secret::new(&token),
        boot_id: Uuid::new_v4(),
        adapters: Arc::new(adapters),
        capabilities,
        outbox_dir: run.join("home/outbox"),
        conn: ConnOptions::default(),
        shutdown_timeout: Duration::from_secs(10),
        machine_id: None,
        known_paths_file: run.join("home/known_workspaces.json"),
        mcp_socket: None,
        logs: None,
        mirror: Some(MirrorOptions {
            projects_dir: claude_dir.join("projects"),
            sessions_dir: Some(claude_dir.join("sessions")),
            state_dir: run.join("home/mirror"),
            own_records: own,
            poll: hqc_worker::mirror::POLL,
            grace: hqc_worker::mirror::GRACE,
            quota_retry: hqc_worker::mirror::QUOTA_RETRY,
        }),
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

    // 2. The holder: `claude -p` waiting in a slow tool call.
    let spike = repo.join("tools/spike");
    let mcp = json!({"mcpServers": {"hqc": {"command": "node",
        "args": [spike.join("slow-mcp.js")],
        "env": {"SLOW_MCP_LOG": run.join("mcp.log")}}}})
    .to_string();
    let out = run.join("driver");
    let mut holder = Holder(
        std::process::Command::new("python3")
            .arg(spike.join("driver.py"))
            .arg(&out)
            .arg(".")
            .arg("once")
            .arg("Call the tool mcp__hqc__slow with seconds=40 exactly once. Then reply with exactly: DONE")
            .args(["--", "--permission-prompt-tool", "stdio", "--strict-mcp-config", "--mcp-config"])
            .arg(&mcp)
            .args(["--allowedTools", "mcp__hqc"])
            .current_dir(&ws)
            .stdin(std::process::Stdio::null())
            .spawn()
            .expect("driver.py runs"),
    );
    let runtime = wait("run.json", 20, async || {
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(out.join("run.json")).ok()?).ok()?;
        Some(v["session_id"].as_str()?.to_string())
    })
    .await;
    let transcript = paths::transcript_path(&claude_dir, &ws, &runtime.parse().unwrap());
    wait("the transcript", 60, async || {
        transcript.exists().then_some(())
    })
    .await;

    // 3. Import the folder; the session comes in held.
    let (_, workers) = hub.get("/api/workers").await;
    let wid = workers["workers"][0]["id"].as_str().unwrap().to_string();
    let (st, p) = hub.post("/api/projects", json!({"name": "live"})).await;
    assert_eq!(st, 201);
    let pid = p["id"].as_str().unwrap().to_string();
    let (st, w) = hub
        .post(
            "/api/workspaces",
            json!({"project_id": pid, "worker_id": wid, "path": ws_path, "mirror_enabled": true}),
        )
        .await;
    assert_eq!(st, 201, "{w}");
    let sid = wait("imported session", 60, async || {
        let (_, l) = hub.get(&format!("/api/projects/{pid}/sessions")).await;
        l["sessions"]
            .as_array()?
            .iter()
            .find(|s| s["agent"]["runtime_session_id"] == runtime.as_str())
            .map(|s| s["id"].as_str().unwrap().to_string())
    })
    .await;
    let s = wait("the holder", 30, async || {
        let s = hub.session(&sid).await;
        s.get("held_by").is_some().then_some(s)
    })
    .await;
    println!(
        "held: locked_by={} held_by={}",
        s["locked_by"], s["held_by"]
    );
    assert_eq!(s["locked_by"], "vscode", "{s}");
    assert_eq!(s["state"], "locked_vscode");
    assert_eq!(s["held_by"]["entrypoint"], "sdk-cli", "{s}");

    let (st, e) = hub
        .post(&format!("/api/sessions/{sid}/takeover"), json!({}))
        .await;
    assert_eq!((st, e["error"].as_str()), (409, Some("vscode_open")), "{e}");
    assert_eq!(e["held_by"], s["held_by"]);

    // 4. The process ends: free at once, no 30 s of silence needed.
    let rc = tokio::task::spawn_blocking(move || {
        let r = holder.0.wait();
        drop(holder);
        r
    })
    .await
    .unwrap()
    .unwrap();
    println!("driver: {rc}");
    let s = wait("the holder gone", 20, async || {
        let s = hub.session(&sid).await;
        (s["locked_by"] == "none").then_some(s)
    })
    .await;
    assert!(s.get("held_by").is_none(), "{s}");
    let age = std::fs::metadata(&transcript)
        .unwrap()
        .modified()
        .unwrap()
        .elapsed()
        .unwrap_or_default();
    println!("transcript written {age:?} ago");
    let (st, v) = hub
        .post(&format!("/api/sessions/{sid}/takeover"), json!({}))
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["locked_by"], "hqc");
    let evs = hub.events(&sid).await;
    assert!(evs
        .iter()
        .any(|e| e["payload"]["code"] == "session_taken_over"));

    stop.cancel();
    task.await.unwrap().unwrap();
    hub.finish().await;
    let _ = std::fs::remove_dir_all(&run);
}
