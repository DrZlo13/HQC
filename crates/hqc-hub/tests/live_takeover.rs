//! T15 live end-to-end (SPEC §9, the iteration 2 criterion): a session started «as in VS
//! Code» (`claude -p "<prompt>"` in the folder), imported into a project, taken over from
//! the hub, one turn through the worker's adapter, then continued from outside with
//! `claude -p --resume`, which releases it back to VS Code.
//!
//! Spends the subscription (three `claude` runs, `claude-haiku-5-5`, `--max-turns 2`), so it
//! runs only with `HQC_LIVE_CLAUDE=1`; it waits the 30 s of silence takeover needs:
//!
//! ```
//! HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_takeover -- --nocapture
//! ```
//!
//! The hub runs in process against `HQC_TEST_DATABASE_URL`, the worker in process with the
//! real Claude Code adapter and the mirror over the real `~/.claude/projects`; the folder is
//! `tmp/spike-t15-live-<uuid>/ws` of this checkout.

mod common;

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

const MODEL: &str = "claude-haiku-5-5";

/// `claude -p <prompt>` in `dir`, the way a person runs it (prompt as argument).
fn claude_p(dir: &std::path::Path, session: &[&str], prompt: &str) {
    let out = std::process::Command::new("claude")
        .arg("-p")
        .arg(prompt)
        .args(["--model", MODEL, "--max-turns", "2"])
        .args(session)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("claude runs");
    println!(
        "claude -p {session:?}: {} {}",
        out.status,
        String::from_utf8_lossy(&out.stdout).trim()
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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

fn texts(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|e| {
            format!(
                "{} {}",
                e["type"].as_str().unwrap_or(""),
                e["payload"]["text"].as_str().unwrap_or("")
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn live_vscode_import_takeover_release() {
    if std::env::var("HQC_LIVE_CLAUDE").as_deref() != Ok("1") {
        eprintln!("skipped: set HQC_LIVE_CLAUDE=1 to run against the real claude");
        return;
    }
    let run = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tmp")
        .join(format!("spike-t15-live-{}", Uuid::now_v7()));
    std::fs::create_dir_all(run.join("ws")).unwrap();
    let run = run.canonicalize().unwrap();
    let ws = run.join("ws");
    let ws_path = ws.display().to_string();

    // 1. «VS Code»: a session started in the folder outside hqc.
    let runtime = Uuid::new_v4().to_string();
    claude_p(
        &ws,
        &["--session-id", &runtime],
        "Reply with exactly: VSCODE-LIVE",
    );

    // 2. Hub and worker.
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
        mcp_socket: None,
        logs: None,
        mirror: Some(MirrorOptions {
            projects_dir: claude_dir.join("projects"),
            // T15's scenario: the registry of live sessions is the T20 test's.
            sessions_dir: None,
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
    let transcript = paths::transcript_path(&claude_dir, &ws, &runtime.parse().unwrap());
    assert!(transcript.exists(), "{}", transcript.display());

    // 3. Import: the folder is a candidate; it becomes a mirrored workspace.
    let (_, workers) = hub.get("/api/workers").await;
    let wid = workers["workers"][0]["id"].as_str().unwrap().to_string();
    let (st, c) = hub.get(&format!("/api/workers/{wid}/candidates")).await;
    assert_eq!(st, 200, "{c}");
    let cand = c["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["path"] == ws_path.as_str())
        .unwrap_or_else(|| panic!("{ws_path} not among {c}"));
    assert_eq!(cand["sessions_count"], 1);
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

    let sid = wait("imported session", 30, async || {
        let (_, l) = hub.get(&format!("/api/projects/{pid}/sessions")).await;
        l["sessions"]
            .as_array()?
            .iter()
            .find(|s| s["agent"]["runtime_session_id"] == runtime.as_str())
            .map(|s| s["id"].as_str().unwrap().to_string())
    })
    .await;
    let evs = wait("imported history", 30, async || {
        let e = hub.events(&sid).await;
        texts(&e)
            .iter()
            .any(|t| t.contains("VSCODE-LIVE") && t.starts_with("agent.message"))
            .then_some(e)
    })
    .await;
    println!("imported: {:#?}", texts(&evs));
    let s = hub.session(&sid).await;
    println!("session: locked_by={} title={}", s["locked_by"], s["title"]);

    // 4. Read-only while VS Code has it; takeover after 30 s of silence.
    if s["locked_by"] == "vscode" {
        let (st, e) = hub
            .post(
                &format!("/api/sessions/{sid}/input"),
                json!({"text": "x", "client_op_id": "x"}),
            )
            .await;
        assert_eq!(
            (st, e["error"].as_str()),
            (409, Some("locked_vscode")),
            "{e}"
        );
        let age = std::fs::metadata(&transcript)
            .unwrap()
            .modified()
            .unwrap()
            .elapsed()
            .unwrap_or_default();
        if age < Duration::from_secs(30) {
            let (st, e) = hub
                .post(&format!("/api/sessions/{sid}/takeover"), json!({}))
                .await;
            assert_eq!(
                (st, e["error"].as_str()),
                (409, Some("vscode_active")),
                "{e}"
            );
        }
        println!("waiting for 30 s of silence");
        tokio::time::sleep(Duration::from_secs(31)).await;
    }
    let (st, v) = hub
        .post(&format!("/api/sessions/{sid}/takeover"), json!({}))
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["locked_by"], "hqc");

    // 5. One turn through the adapter (`--resume` of the same transcript).
    let (st, v) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "Reply with exactly: HQC-LIVE", "client_op_id": "live-1"}),
        )
        .await;
    assert_eq!(st, 201, "{v}");
    wait("the hqc turn", 180, async || {
        let e = hub.events(&sid).await;
        e.iter()
            .any(|e| {
                e["type"] == "agent.message"
                    && e["payload"]["final"] == true
                    && e["payload"]["text"]
                        .as_str()
                        .is_some_and(|t| t.contains("HQC-LIVE"))
            })
            .then_some(())
    })
    .await;
    let s = hub.session(&sid).await;
    assert_eq!(s["locked_by"], "hqc", "{s}");
    let lines = std::fs::read_to_string(&transcript).unwrap();
    assert!(
        lines.contains("HQC-LIVE"),
        "the adapter wrote the same transcript"
    );

    // 6. Continued from outside while hqc's process still runs: released to VS Code.
    claude_p(
        &ws,
        &["--resume", &runtime],
        "Reply with exactly: OUTSIDE-LIVE",
    );
    let s = wait("released", 60, async || {
        let s = hub.session(&sid).await;
        (s["locked_by"] == "vscode").then_some(s)
    })
    .await;
    assert_eq!(s["state"], "locked_vscode");
    let evs = wait("the outside turn mirrored", 30, async || {
        let e = hub.events(&sid).await;
        texts(&e)
            .iter()
            .any(|t| t.contains("OUTSIDE-LIVE") && t.starts_with("agent.message"))
            .then_some(e)
    })
    .await;
    let all = texts(&evs);
    println!("history: {all:#?}");
    let released = evs
        .iter()
        .filter(|e| e["payload"]["code"] == "session_released")
        .count();
    assert_eq!(released, 1, "{all:#?}");
    assert_eq!(
        evs.iter()
            .filter(|e| e["payload"]["code"] == "session_taken_over")
            .count(),
        1
    );
    // Every fact once: the adapter's records were not mirrored a second time.
    let hqc_answers = all
        .iter()
        .filter(|t| t.contains("agent.message HQC-LIVE"))
        .count();
    assert_eq!(hqc_answers, 1, "{all:#?}");

    stop.cancel();
    task.await.unwrap().unwrap();
    hub.finish().await;
    let _ = std::fs::remove_dir_all(&run);
}
