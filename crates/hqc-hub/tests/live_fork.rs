//! SPEC 0.80 live end-to-end (§6 fork, §5.3, §8.1): a Claude Code session remembers APPLE,
//! then BANANA; a fork before the second message knows APPLE only, the source keeps both, and
//! the fork's feed has the copied history once, with no records imported by the mirror.
//!
//! Spends the subscription (`claude-haiku-5-5`, `--max-turns 2`, three short turns), so it
//! runs only with `HQC_LIVE_CLAUDE=1`:
//!
//! ```
//! HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_fork -- --nocapture
//! ```

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use hqc_worker::adapter::claude_code::process::OsSpawner;
use hqc_worker::adapter::claude_code::{ClaudeCodeAdapters, ClaudeOptions, McpLaunch};
use hqc_worker::config::Config;
use hqc_worker::connection::{ConnOptions, Status};
use hqc_worker::token::Secret;
use hqc_worker::{MirrorOptions, Worker};
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

/// Sends `text` into session `sid` and waits for the final answer to it.
async fn turn(hub: &TestHub, sid: &str, text: &str) -> (Value, String) {
    let (st, v) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": text}))
        .await;
    assert_eq!(st, 201, "{v}");
    let input = v["event"].clone();
    let answer = wait("the answer", 300, async || {
        hub.events(sid).await.into_iter().find(|e| {
            e["type"] == "agent.message"
                && e["payload"]["final"] == true
                && e["payload"]["in_reply_to"] == input["event_id"]
        })
    })
    .await;
    println!("{sid}: {text:?} -> {}", answer["payload"]["text"]);
    (
        input,
        answer["payload"]["text"].as_str().unwrap().to_uppercase(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn live_a_fork_knows_only_what_came_before() {
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
        .join(format!("spike-fork-live-{}", Uuid::now_v7()));
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
    // The mirror is on, as on a real worker: the fork's copied records must not come back
    // through it.
    let claude_dir = opts.config_dir.clone().expect("claude config dir");
    let adapters = ClaudeCodeAdapters::new(opts, Arc::new(OsSpawner)).with_mcp(McpLaunch {
        program,
        socket: socket.clone(),
    });
    let own = adapters.own_records();
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

    let (_, workers) = hub.get("/api/workers").await;
    let wid = workers["workers"][0]["id"].as_str().unwrap().to_string();
    let (st, p) = hub.post("/api/projects", json!({"name": "live"})).await;
    assert_eq!(st, 201);
    let pid = p["id"].as_str().unwrap().to_string();
    let (st, w) = hub
        .post(
            "/api/workspaces",
            json!({"project_id": pid, "worker_id": wid, "path": ws.display().to_string(),
                   "mirror_enabled": true}),
        )
        .await;
    assert_eq!(st, 201, "{w}");
    let (st, s) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": "Words", "workspace_id": w["id"], "model": MODEL}),
        )
        .await;
    assert_eq!(st, 201, "{s}");
    let a = s["id"].as_str().unwrap().to_string();
    turn(&hub, &a, "Remember the word APPLE. Reply only OK.").await;
    let (second, _) = turn(&hub, &a, "Also remember the word BANANA. Reply only OK.").await;

    let (st, f) = hub
        .post(
            &format!("/api/sessions/{a}/fork"),
            json!({"event_id": second["event_id"]}),
        )
        .await;
    assert_eq!(st, 201, "{f}");
    let b = f["id"].as_str().unwrap().to_string();
    let ask = "List every word I asked you to remember, comma separated, nothing else.";
    let (_, words) = turn(&hub, &b, ask).await;
    assert!(
        words.contains("APPLE") && !words.contains("BANANA"),
        "{words}"
    );

    // The copied history once, the fork line, then the new turn; nothing the mirror imported
    // (it polls every POLL: a few rounds).
    tokio::time::sleep(hqc_worker::mirror::POLL * 4).await;
    let events = hub.events(&b).await;
    let users: Vec<&Value> = events
        .iter()
        .filter(|e| e["type"] == "user.message")
        .map(|e| &e["payload"]["text"])
        .collect();
    assert_eq!(
        users,
        [
            &json!("Remember the word APPLE. Reply only OK."),
            &json!(ask)
        ],
        "{events:#?}"
    );
    assert!(events
        .iter()
        .any(|e| e["type"] == "system" && e["payload"]["code"] == "forked_from"));
    // Nor did it make an imported session of the fork's transcript.
    let (_, list) = hub.get(&format!("/api/projects/{pid}/sessions")).await;
    assert_eq!(list["sessions"].as_array().unwrap().len(), 2, "{list:#}");
    let held = hub.session(&b).await;
    assert!(held.get("held_by").is_none_or(Value::is_null), "{held}");

    // The source still has both.
    let (_, words) = turn(&hub, &a, ask).await;
    assert!(
        words.contains("APPLE") && words.contains("BANANA"),
        "{words}"
    );

    stop.cancel();
    task.await.unwrap().unwrap();
    hub.finish().await;
    let _ = std::fs::remove_dir_all(&run);
}
