//! T4 live acceptance against the real CLI. Spends the subscription, so it only runs with
//! `HQC_LIVE_CLAUDE=1` (never in CI):
//!
//! ```
//! HQC_LIVE_CLAUDE=1 cargo test -p hqc-worker --test claude_live -- --nocapture
//! ```
//!
//! One turn with `claude-haiku-5-5`, `--max-turns 2`, in a fresh folder under `tmp/`: the
//! model runs `touch hqc-live && echo hqc-live` through Bash, the adapter approves it with `once`; the events
//! must come in order and their ids must match the transcript's record uuids. Then the
//! process is killed and started again, which must pick `--resume` (no model call).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hqc_proto::event::{Event, EventKind};
use hqc_proto::frame::{Runtime, SessionInput};
use hqc_worker::adapter::claude_code::process::{CommandSpec, OsSpawner, Spawned, Spawner};
use hqc_worker::adapter::claude_code::{paths, ClaudeCodeAdapters, ClaudeOptions};
use hqc_worker::adapter::{AdapterEvent, AdapterFactory};
use hqc_worker::config::StartSpec;
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

/// `echo` alone is a read-only command the CLI runs without asking (2.1.308), so the
/// command also touches a file to get a `can_use_tool` request.
const PROMPT: &str =
    "Run the shell command: touch hqc-live && echo hqc-live. Then reply with exactly: DONE";

fn label(ev: &AdapterEvent) -> String {
    match ev {
        AdapterEvent::Event(e) => {
            let v = serde_json::to_value(&e.kind).unwrap();
            let ty = v["type"].as_str().unwrap().to_owned();
            match &e.kind {
                EventKind::AgentMessage { r#final, .. } => format!("{ty}:{final}"),
                EventKind::ToolCall { tool, .. } => format!("{ty}:{tool}"),
                _ => ty,
            }
        }
        AdapterEvent::InputAccepted { .. } => "accepted".into(),
        AdapterEvent::TurnEnded => "turn_ended".into(),
        AdapterEvent::Exited { error } => format!("exited:{error:?}"),
        AdapterEvent::RuntimeSessionId(s) => format!("runtime_session_id:{s}"),
    }
}

/// The real spawner, remembering command lines.
#[derive(Default)]
struct Recording(Mutex<Vec<CommandSpec>>);

impl Spawner for Recording {
    fn spawn(&self, cmd: &CommandSpec) -> std::io::Result<Spawned> {
        self.0.lock().unwrap().push(cmd.clone());
        OsSpawner.spawn(cmd)
    }
}

#[tokio::test]
async fn live_bash_turn_with_approval() {
    if std::env::var("HQC_LIVE_CLAUDE").as_deref() != Ok("1") {
        eprintln!("skipped: set HQC_LIVE_CLAUDE=1 to run against the real claude");
        return;
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp");
    let work = root.join(format!("live-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&work).unwrap();
    let work = work.canonicalize().unwrap();

    let opts = ClaudeOptions {
        extra_args: vec!["--max-turns".into(), "2".into()],
        ..ClaudeOptions::default()
    };
    let config_dir = opts.config_dir.clone().expect("claude config dir");
    let spawner = Arc::new(Recording::default());
    let factory = ClaudeCodeAdapters::new(opts, spawner.clone());
    let session_id = Uuid::now_v7();
    let claude_session = Uuid::new_v4();
    let spec = StartSpec {
        session_id,
        runtime: Runtime::ClaudeCode,
        model: Some("claude-haiku-5-5".into()),
        workspace: work.clone(),
        workdir: work.clone(),
        environment: "default".into(),
        runtime_session_id: Some(claude_session.to_string()),
        system_prompt: None,
        disallowed_tools: vec![],
        restricted: false,
        auto_approve: false,
        session_token: "st".into(),
        ask_timeout_s: 600,
    };
    let (tx, mut rx) = mpsc::channel(4096);
    let mut adapter = factory.create(&spec, tx);
    adapter.start(&spec).await.expect("claude starts");

    let input_id = Uuid::now_v7();
    adapter
        .input(&SessionInput {
            session_id,
            event_id: input_id,
            text: PROMPT.into(),
            artifact_ids: vec![],
            peer_prefix: vec![],
        })
        .await
        .unwrap();

    let mut evs = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    loop {
        let ev = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("turn did not end in 180 s")
            .expect("channel closed");
        evs.push(ev.clone());
        match &ev {
            AdapterEvent::Event(e) => {
                if let EventKind::ApprovalRequest {
                    request_id, tool, ..
                } = &e.kind
                {
                    println!("approval.request {tool} -> once");
                    adapter.answer_approval(request_id, "once").await.unwrap();
                }
            }
            AdapterEvent::TurnEnded => break,
            AdapterEvent::Exited { error } => panic!("claude exited: {error:?}"),
            _ => {}
        }
    }

    // Fold delta runs for the printout and the order check.
    let mut shape: Vec<String> = Vec::new();
    for ev in &evs {
        let l = label(ev);
        if l == "agent.delta" && shape.last().is_some_and(|p| p == "agent.delta") {
            continue;
        }
        shape.push(l);
    }
    println!("claude session {claude_session}");
    println!("events: {}", shape.join(" -> "));
    for ev in &evs {
        if let AdapterEvent::Event(e) = ev {
            if !matches!(e.kind, EventKind::AgentDelta { .. }) {
                println!(
                    "  {} {}",
                    e.event_id,
                    serde_json::to_string(&e.kind).unwrap()
                );
            }
        }
    }

    // Order: accepted, the Bash call, its approval, its result, the final DONE, usage,
    // status idle, end of turn (other text or thinking may come in between).
    let want = [
        "accepted",
        "tool.call:Bash",
        "approval.request",
        "tool.result",
        "agent.message:true",
        "usage",
        "status",
        "turn_ended",
    ];
    let mut at = 0;
    for w in want {
        let i = shape[at..]
            .iter()
            .position(|s| s == w)
            .unwrap_or_else(|| panic!("{w} missing after position {at}: {shape:?}"));
        at += i + 1;
    }
    assert_eq!(shape.last().unwrap(), "turn_ended");
    assert!(evs.contains(&AdapterEvent::InputAccepted { event_id: input_id }));
    let events: Vec<&Event> = evs
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::Event(e) => Some(e),
            _ => None,
        })
        .collect();
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::ToolResult { error: false, result_preview, .. } if result_preview.contains("hqc-live")
    )));
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::AgentMessage { text, r#final: true, in_reply_to: Some(r) } if text.contains("DONE") && *r == input_id
    )));

    // Event ids against the transcript.
    let transcript = paths::transcript_path(&config_dir, &work, &claude_session);
    println!("transcript {}", transcript.display());
    let text = std::fs::read_to_string(&transcript).expect("transcript at the encoded path");
    let records: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let expected: HashSet<Uuid> = records
        .iter()
        .filter(|r| (r["type"] == "assistant" || r["type"] == "user") && r["isSidechain"] != true)
        .filter(|r| {
            r["message"]["content"].as_array().is_some_and(|c| {
                c.iter().any(|b| match b["type"].as_str() {
                    Some("tool_use") | Some("tool_result") => true,
                    Some("text") => b["text"].as_str().is_some_and(|t| !t.is_empty()),
                    Some("thinking") => b["thinking"].as_str().is_some_and(|t| !t.is_empty()),
                    _ => false,
                })
            })
        })
        .map(|r| Event::id_from_claude_record(&r["uuid"].as_str().unwrap().parse().unwrap()))
        .collect();
    let got: HashSet<Uuid> = events
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                EventKind::ToolCall { .. }
                    | EventKind::ToolResult { .. }
                    | EventKind::AgentMessage { .. }
                    | EventKind::AgentThinking { .. }
            )
        })
        .map(|e| e.event_id)
        .collect();
    println!(
        "record-derived event ids: {} emitted, {} expected from the transcript",
        got.len(),
        expected.len()
    );
    assert_eq!(got, expected);

    // The process stays after the turn; kill, then a new start must resume.
    adapter.kill().await.unwrap();
    adapter.start(&spec).await.expect("claude --resume starts");
    tokio::time::sleep(Duration::from_secs(3)).await;
    while let Ok(ev) = rx.try_recv() {
        assert!(
            !matches!(ev, AdapterEvent::Exited { .. }),
            "resumed claude exited: {ev:?}"
        );
    }
    adapter.kill().await.unwrap();
    let cmds = spawner.0.lock().unwrap().clone();
    println!("first:  claude {}", cmds[0].args.join(" "));
    println!("second: claude {}", cmds[1].args.join(" "));
    assert!(cmds[0]
        .args
        .windows(2)
        .any(|w| w == ["--session-id", &claude_session.to_string()]));
    assert!(cmds[1]
        .args
        .windows(2)
        .any(|w| w == ["--resume", &claude_session.to_string()]));
    println!("resume: process stayed up, killed");
    let _ = std::fs::remove_dir_all(&work);
}
