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
        AdapterEvent::Title(t) => format!("title:{t}"),
        AdapterEvent::Commands(c) => format!("commands:{}", c.len()),
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

/// T9 (SPEC §8.1): a repository's `.claude/settings.json` with `defaultMode:
/// bypassPermissions` and a hook is not applied (`--setting-sources user,local`,
/// `--permission-mode`), `always` is still offered and writes its rule to
/// `.claude/settings.local.json`; the agent runs with the cleared environment and the
/// `--disallowedTools` / `--append-system-prompt` flags.
///
/// ```
/// HQC_LIVE_CLAUDE=1 cargo test -p hqc-worker --test claude_live live_repo_settings_ignored -- --nocapture
/// ```
#[tokio::test]
async fn live_repo_settings_ignored() {
    if std::env::var("HQC_LIVE_CLAUDE").as_deref() != Ok("1") {
        eprintln!("skipped: set HQC_LIVE_CLAUDE=1 to run against the real claude");
        return;
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp");
    let work = root.join(format!("live-settings-{}", Uuid::now_v7()));
    std::fs::create_dir_all(work.join(".claude")).unwrap();
    let work = work.canonicalize().unwrap();
    // Claude Code keeps settings.local.json at the git root of cwd, not at cwd: make this
    // workspace its own repository, otherwise `always` writes into the hqc checkout.
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&work)
        .status()
        .unwrap()
        .success());
    std::fs::write(
        work.join(".claude/settings.json"),
        serde_json::json!({
            "permissions": {"defaultMode": "bypassPermissions"},
            "hooks": {"PreToolUse": [{"matcher": "*", "hooks": [
                {"type": "command", "command": "touch hook-ran"}
            ]}]}
        })
        .to_string(),
    )
    .unwrap();

    let opts = ClaudeOptions {
        extra_args: vec!["--max-turns".into(), "2".into()],
        permission_mode: hqc_worker::capabilities::claude_permission_mode(None).await,
        ..ClaudeOptions::default()
    };
    let spawner = Arc::new(Recording::default());
    let factory = ClaudeCodeAdapters::new(opts, spawner.clone());
    let session_id = Uuid::now_v7();
    let spec = StartSpec {
        session_id,
        runtime: Runtime::ClaudeCode,
        model: Some("claude-haiku-5-5".into()),
        workspace: work.clone(),
        workdir: work.clone(),
        environment: "default".into(),
        runtime_session_id: Some(Uuid::new_v4().to_string()),
        system_prompt: Some("Be brief.".into()),
        disallowed_tools: vec!["WebFetch".into()],
        restricted: false,
        auto_approve: false,
        session_token: hqc_worker::token::Secret::new("st"),
        ask_timeout_s: 600,
    };
    let (tx, mut rx) = mpsc::channel(4096);
    let mut adapter = factory.create(&spec, tx);
    adapter.start(&spec).await.expect("claude starts");
    adapter
        .input(&SessionInput {
            session_id,
            event_id: Uuid::now_v7(),
            text: PROMPT.into(),
            artifact_ids: vec![],
            peer_prefix: vec![],
            peer: None,
        })
        .await
        .unwrap();

    let mut options_seen = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    loop {
        let ev = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("turn did not end in 180 s")
            .expect("channel closed");
        match &ev {
            AdapterEvent::Event(e) => {
                if let EventKind::ApprovalRequest {
                    request_id,
                    tool,
                    options,
                    ..
                } = &e.kind
                {
                    let ids: Vec<String> = options.iter().map(|o| o.id.clone()).collect();
                    println!("approval.request {tool} {ids:?} -> always");
                    options_seen = Some(ids);
                    adapter
                        .answer_approval(request_id, "always", None)
                        .await
                        .unwrap();
                }
            }
            AdapterEvent::TurnEnded => break,
            AdapterEvent::Exited { error } => panic!("claude exited: {error:?}"),
            _ => {}
        }
    }
    adapter.kill().await.unwrap();
    let cmd = spawner.0.lock().unwrap()[0].clone();
    println!("claude {}", cmd.args.join(" "));

    let ids = options_seen.expect("bypassPermissions from the repo did not apply: approval asked");
    assert!(
        ids.iter().any(|i| i == "always"),
        "always is offered with --setting-sources user,local: {ids:?}"
    );
    assert!(work.join("hqc-live").exists(), "the command ran");
    assert!(!work.join("hook-ran").exists(), "the repo's hook ran");
    let local = std::fs::read_to_string(work.join(".claude/settings.local.json"))
        .expect("always wrote settings.local.json");
    println!("settings.local.json: {local}");
    assert!(local.contains("Bash"), "{local}");
    let _ = std::fs::remove_dir_all(&work);
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
        permission_mode: hqc_worker::capabilities::claude_permission_mode(None).await,
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
        session_token: hqc_worker::token::Secret::new("st"),
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
            peer: None,
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
                    adapter
                        .answer_approval(request_id, "once", None)
                        .await
                        .unwrap();
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

/// T12 (SPEC §8.1, §14): `[permissions]` through `--settings`. One turn, three `touch`
/// calls: the one in `deny` neither runs nor asks (`system policy_denied`), the one in
/// `allow` runs without `approval.request`, the one in neither list asks.
///
/// ```
/// HQC_LIVE_CLAUDE=1 cargo test -p hqc-worker --test claude_live live_permission_policy -- --nocapture
/// ```
#[tokio::test]
async fn live_permission_policy() {
    use hqc_worker::permissions::{PermissionMode, Permissions};
    if std::env::var("HQC_LIVE_CLAUDE").as_deref() != Ok("1") {
        eprintln!("skipped: set HQC_LIVE_CLAUDE=1 to run against the real claude");
        return;
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp");
    let work = root.join(format!("spike-t12-live-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&work).unwrap();
    let work = work.canonicalize().unwrap();
    // Its own repository, so that nothing reads or writes the hqc checkout's settings.
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&work)
        .status()
        .unwrap()
        .success());

    let mut deny = Permissions::default().deny;
    deny.push("Bash(touch hqc-denied:*)".into());
    let opts = ClaudeOptions {
        extra_args: vec!["--max-turns".into(), "2".into()],
        permission_mode: hqc_worker::capabilities::claude_permission_mode(None).await,
        permissions: Permissions {
            mode: PermissionMode::Ask,
            allow: vec!["Bash(touch hqc-allowed:*)".into()],
            deny,
        },
        ..ClaudeOptions::default()
    };
    let spawner = Arc::new(Recording::default());
    let factory = ClaudeCodeAdapters::new(opts, spawner.clone());
    let session_id = Uuid::now_v7();
    let spec = StartSpec {
        session_id,
        runtime: Runtime::ClaudeCode,
        model: Some("claude-haiku-5-5".into()),
        workspace: work.clone(),
        workdir: work.clone(),
        environment: "default".into(),
        runtime_session_id: Some(Uuid::new_v4().to_string()),
        system_prompt: None,
        disallowed_tools: vec![],
        restricted: false,
        auto_approve: false,
        session_token: hqc_worker::token::Secret::new("st"),
        ask_timeout_s: 600,
    };
    let (tx, mut rx) = mpsc::channel(4096);
    let mut adapter = factory.create(&spec, tx);
    adapter.start(&spec).await.expect("claude starts");
    adapter
        .input(&SessionInput {
            session_id,
            event_id: Uuid::now_v7(),
            text: "In ONE message, make exactly these three Bash tool calls in parallel, each \
                   command verbatim as its own call: (1) touch hqc-allowed (2) touch hqc-denied \
                   (3) touch hqc-asked. Then reply with exactly: DONE"
                .into(),
            artifact_ids: vec![],
            peer_prefix: vec![],
            peer: None,
        })
        .await
        .unwrap();

    let mut asked = Vec::new();
    let mut system = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    loop {
        let ev = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("turn did not end in 180 s")
            .expect("channel closed");
        match &ev {
            AdapterEvent::Event(e) => match &e.kind {
                EventKind::ApprovalRequest {
                    request_id,
                    description,
                    policy_hint,
                    ..
                } => {
                    println!("approval.request {description:?} hint {policy_hint:?} -> once");
                    asked.push(description.clone());
                    adapter
                        .answer_approval(request_id, "once", None)
                        .await
                        .unwrap();
                }
                EventKind::System { code, text, .. } => {
                    println!("system {code}: {text}");
                    system.push((code.clone(), text.clone()));
                }
                EventKind::ToolCall { args_preview, .. } => println!("tool.call {args_preview}"),
                EventKind::ToolResult { result_preview, .. } => {
                    println!("tool.result {result_preview}")
                }
                _ => {}
            },
            AdapterEvent::TurnEnded => break,
            AdapterEvent::Exited { error } => panic!("claude exited: {error:?}"),
            _ => {}
        }
    }
    adapter.kill().await.unwrap();
    let cmd = spawner.0.lock().unwrap()[0].clone();
    println!("claude {}", cmd.args.join(" "));

    assert!(work.join("hqc-allowed").exists(), "the allowed command ran");
    assert!(!work.join("hqc-denied").exists(), "the denied command ran");
    assert!(
        work.join("hqc-asked").exists(),
        "the asked command ran after once"
    );
    assert_eq!(
        asked.len(),
        1,
        "only the command outside both lists asks: {asked:?}"
    );
    assert!(asked[0].contains("hqc-asked"), "{asked:?}");
    assert!(
        system
            .iter()
            .any(|(c, t)| c == "policy_denied" && t.contains("hqc-denied")),
        "{system:?}"
    );
    let _ = std::fs::remove_dir_all(&work);
}

/// T13 (SPEC §8.1): the adapter reports the transcript's last `ai-title` as
/// [`AdapterEvent::Title`] before `TurnEnded`, and nothing if there is none. No tools, one
/// model call. Found on 2.1.308 (2026-10-23): with `--input-format stream-json` the CLI
/// writes no `ai-title` at all, not even 5 s after the turn; only a prompt given as an
/// argument got one (fixture `tool-call-2.1.307`).
///
/// ```
/// HQC_LIVE_CLAUDE=1 cargo test -p hqc-worker --test claude_live live_ai_title -- --nocapture
/// ```
#[tokio::test]
async fn live_ai_title() {
    if std::env::var("HQC_LIVE_CLAUDE").as_deref() != Ok("1") {
        eprintln!("skipped: set HQC_LIVE_CLAUDE=1 to run against the real claude");
        return;
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp");
    let work = root.join(format!("live-title-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&work).unwrap();
    let work = work.canonicalize().unwrap();
    let opts = ClaudeOptions {
        extra_args: vec!["--max-turns".into(), "2".into()],
        permission_mode: hqc_worker::capabilities::claude_permission_mode(None).await,
        ..ClaudeOptions::default()
    };
    let config_dir = opts.config_dir.clone().expect("claude config dir");
    let factory = ClaudeCodeAdapters::new(opts, Arc::new(Recording::default()));
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
        session_token: hqc_worker::token::Secret::new("st"),
        ask_timeout_s: 600,
    };
    let (tx, mut rx) = mpsc::channel(4096);
    let mut adapter = factory.create(&spec, tx);
    adapter.start(&spec).await.expect("claude starts");
    adapter
        .input(&SessionInput {
            session_id,
            event_id: Uuid::now_v7(),
            text: "What is the boiling point of water at sea level in Celsius? Answer in one \
                   short sentence, without tools."
                .into(),
            artifact_ids: vec![],
            peer_prefix: vec![],
            peer: None,
        })
        .await
        .unwrap();
    let mut labels = Vec::new();
    let mut title = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    loop {
        let ev = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("turn did not end in 180 s")
            .expect("channel closed");
        let l = label(&ev);
        if l != "agent.delta" {
            labels.push(l);
        }
        match ev {
            AdapterEvent::Title(t) => title = Some(t),
            AdapterEvent::TurnEnded => break,
            AdapterEvent::Exited { error } => panic!("claude exited: {error:?}"),
            _ => {}
        }
    }
    let transcript = paths::transcript_path(&config_dir, &work, &claude_session);
    let types = |text: &str| -> Vec<String> {
        text.lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .map(|r| r["type"].as_str().unwrap_or("?").to_string())
            .collect()
    };
    println!("events: {}", labels.join(" -> "));
    let at_end = std::fs::read_to_string(&transcript).unwrap_or_default();
    println!("transcript at turn end: {:?}", types(&at_end));
    tokio::time::sleep(Duration::from_secs(5)).await;
    let later = std::fs::read_to_string(&transcript).unwrap_or_default();
    println!("transcript 5 s later:   {:?}", types(&later));
    for l in later.lines().filter(|l| l.contains("\"ai-title\"")) {
        println!("  {l}");
    }
    adapter.kill().await.unwrap();
    println!("title: {title:?}");
    assert_eq!(
        title,
        hqc_claude_jsonl::last_ai_title(&at_end),
        "the last ai-title of the transcript"
    );
    let _ = std::fs::remove_dir_all(&work);
}

/// T24 (SPEC §3 Session, §8.1): `title.request` as the worker runs it — one `claude -p` with
/// the fixed flags of `hqc_worker::title::args` in an empty folder. A title comes back; no
/// transcript appears under `<config>/projects` and no record of the process stays in
/// `<config>/sessions`; the folder is removed.
///
/// ```
/// HQC_LIVE_CLAUDE=1 cargo test -p hqc-worker --test claude_live live_session_title -- --nocapture
/// ```
#[tokio::test]
async fn live_session_title() {
    if std::env::var("HQC_LIVE_CLAUDE").as_deref() != Ok("1") {
        eprintln!("skipped: set HQC_LIVE_CLAUDE=1 to run against the real claude");
        return;
    }
    let config_dir = paths::default_config_dir().expect("claude config dir");
    let jsonl = |dir: &std::path::Path| -> HashSet<std::path::PathBuf> {
        let mut out = HashSet::new();
        for d in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            for f in std::fs::read_dir(d.path()).into_iter().flatten().flatten() {
                if f.path().extension().is_some_and(|e| e == "jsonl") {
                    out.insert(f.path());
                }
            }
        }
        out
    };
    let projects = config_dir.join("projects");
    let before = jsonl(&projects);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp");
    std::fs::create_dir_all(&root).unwrap();
    let root = root
        .canonicalize()
        .unwrap()
        .join(format!("live-titles-{}", Uuid::now_v7()));
    let titler =
        hqc_worker::title::Titler::new("claude".into(), root.clone(), hqc_worker::title::TIMEOUT);
    let started = std::time::Instant::now();
    let title = titler
        .title(
            "Почини сборку: cargo test падает на hqc-hub, тест сторожевого таймера ждёт 600 мс",
            "Готово: на CI раннер медленнее, поднял лимит теста до 2,5 с, теперь зелёный.",
        )
        .await
        .expect("a title");
    let caps = hqc_worker::capabilities::detect(None).await;
    eprintln!(
        "claude {}: title in {:?}: {title:?}",
        caps["claude_code"]["version"],
        started.elapsed()
    );
    assert!(!title.is_empty() && title.chars().count() <= 60);
    assert!(!title.contains('\n') && !title.contains('"') && !title.contains('«'));
    // The prompt is in English; the title follows the language of the conversation.
    assert!(title.chars().any(|c| matches!(c, 'А'..='я')), "{title}");

    let slot = root.join("0");
    assert!(!slot.exists(), "the run folder is removed");
    let encoded = projects.join(paths::encode_project_dir(&slot.display().to_string()));
    assert!(!encoded.exists(), "no transcript folder for the run");
    let new: Vec<_> = jsonl(&projects).difference(&before).cloned().collect();
    eprintln!("transcripts created meanwhile (other sessions of this machine): {new:?}");
    assert!(new.iter().all(|p| !p.starts_with(&encoded)), "{new:?}");
    let root_s = root.display().to_string();
    let sessions = std::fs::read_dir(config_dir.join("sessions"));
    for f in sessions.into_iter().flatten().flatten() {
        let text = std::fs::read_to_string(f.path()).unwrap_or_default();
        assert!(!text.contains(&root_s), "{}: {text}", f.path().display());
    }
    let state =
        std::fs::read_to_string(config_dir.with_file_name(".claude.json")).unwrap_or_default();
    eprintln!(
        "~/.claude.json mentions the run folder: {}",
        state.contains(&root_s)
    );
    let _ = std::fs::remove_dir_all(&root);
}
