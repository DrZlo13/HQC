//! The Codex adapter against a fake App Server over in-memory pipes (schema of Codex 0.172.0).

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hqc_proto::event::{ApprovalCanonical, EventKind};
use hqc_proto::frame::{PermissionMode, Runtime, SessionInput};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use super::*;
use crate::adapter::claude_code::process::{ExitInfo, Spawned};

#[derive(Clone, Default)]
struct FakeCodex {
    got: Arc<Mutex<Vec<Value>>>,
    commands: Arc<Mutex<Vec<CommandSpec>>>,
    signed_out: bool,
    /// `thread/resume` answers with another thread (it lost the old one).
    lost: bool,
}

impl FakeCodex {
    fn got(&self) -> Vec<Value> {
        self.got.lock().unwrap().clone()
    }
    fn calls(&self, method: &str) -> Vec<Value> {
        self.got()
            .into_iter()
            .filter(|v| v["method"] == method)
            .collect()
    }
}

async fn send(out: &mut tokio::io::DuplexStream, v: Value) {
    out.write_all(format!("{v}\n").as_bytes()).await.unwrap();
}

fn note(method: &str, params: Value) -> Value {
    json!({"method": method, "params": params})
}

impl Spawner for FakeCodex {
    fn spawn(&self, cmd: &CommandSpec) -> io::Result<Spawned> {
        self.commands.lock().unwrap().push(cmd.clone());
        let (stdin, agent_in) = tokio::io::duplex(1 << 20);
        let (mut out, stdout) = tokio::io::duplex(1 << 20);
        let (sig_tx, sig_rx) = watch::channel::<Option<Sig>>(None);
        let me = self.clone();
        let mut stop = sig_rx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(agent_in).lines();
            let mut turns = 0;
            let play = async {
                while let Ok(Some(line)) = lines.next_line().await {
                    let v: Value = serde_json::from_str(&line).unwrap();
                    me.got.lock().unwrap().push(v.clone());
                    let id = v["id"].clone();
                    let reply = |result: Value| json!({"id": id, "result": result});
                    match v["method"].as_str() {
                        Some("initialize") => {
                            send(
                                &mut out,
                                reply(json!({"codexHome": "/h/.codex", "platformFamily": "unix",
                                "platformOs": "macos", "userAgent": "codex"})),
                            )
                            .await
                        }
                        Some("account/read") => {
                            let account = if me.signed_out {
                                json!({"requiresOpenaiAuth": true, "account": null})
                            } else {
                                json!({"requiresOpenaiAuth": true,
                                    "account": {"type": "chatgpt", "email": "a@b", "planType": "plus"}})
                            };
                            send(&mut out, reply(account)).await
                        }
                        Some("thread/start") => {
                            send(&mut out, reply(json!({"thread": {"id": "th1"}}))).await
                        }
                        Some("thread/resume") => {
                            let tid = if me.lost {
                                "other"
                            } else {
                                v["params"]["threadId"].as_str().unwrap()
                            };
                            send(&mut out, reply(json!({"thread": {"id": tid}}))).await
                        }
                        Some("turn/start") if v["params"]["input"][0]["text"] == "toobig" => {
                            send(&mut out, json!({"id": id, "error": {"code": -32600, "message": "input too large"}})).await
                        }
                        Some("turn/start") => {
                            turns += 1;
                            let tid = format!("u{turns}");
                            let text = v["params"]["input"][0]["text"]
                                .as_str()
                                .unwrap_or("")
                                .to_string();
                            send(&mut out, reply(json!({"turn": {"id": tid, "items": [], "status": "inProgress"}}))).await;
                            send(&mut out, note("turn/started", json!({"threadId": "th1", "turn": {"id": tid, "items": [], "status": "inProgress"}}))).await;
                            if let Some(rest) = text.strip_prefix("runat ") {
                                // `runat <cwd> <command>`: an approval with that `cwd`.
                                let (cwd, cmd) = rest.split_once(' ').unwrap();
                                send(&mut out, json!({"id": 900, "method": "item/commandExecution/requestApproval",
                                    "params": {"threadId": "th1", "turnId": tid, "itemId": "c1", "startedAtMs": 1,
                                        "command": cmd, "cwd": cwd}})).await;
                            } else if let Some(cmd) = text.strip_prefix("run ") {
                                send(&mut out, note("item/started", json!({"threadId": "th1", "turnId": tid, "startedAtMs": 1,
                                    "item": {"type": "commandExecution", "id": "c1", "command": cmd, "commandActions": [], "cwd": "/w", "status": "inProgress"}}))).await;
                                send(&mut out, json!({"id": 900, "method": "item/commandExecution/requestApproval",
                                    "params": {"threadId": "th1", "turnId": tid, "itemId": "c1", "startedAtMs": 1, "command": cmd}})).await;
                            } else if text == "wait" {
                                // Runs until interrupted.
                            } else {
                                send(&mut out, note("item/agentMessage/delta", json!({"threadId": "th1", "turnId": tid, "itemId": "m1", "delta": "Hi "}))).await;
                                send(&mut out, note("item/agentMessage/delta", json!({"threadId": "th1", "turnId": tid, "itemId": "m1", "delta": "there"}))).await;
                                // Another thread's (a subagent's) text is not this session's.
                                send(&mut out, note("item/agentMessage/delta", json!({"threadId": "sub", "turnId": "x", "itemId": "m9", "delta": "NOT MINE"}))).await;
                                send(&mut out, note("turn/plan/updated", json!({"threadId": "th1", "turnId": tid,
                                    "plan": [{"step": "Read", "status": "completed"}, {"step": "Write", "status": "inProgress"}]}))).await;
                                send(&mut out, note("thread/tokenUsage/updated", json!({"threadId": "th1", "turnId": tid,
                                    "tokenUsage": {"modelContextWindow": 400000,
                                        "last": {"inputTokens": 1000, "outputTokens": 20, "cachedInputTokens": 800, "reasoningOutputTokens": 5, "totalTokens": 1020},
                                        "total": {"inputTokens": 5000, "outputTokens": 100, "cachedInputTokens": 0, "reasoningOutputTokens": 5, "totalTokens": 5100}}}))).await;
                                send(&mut out, note("turn/completed", json!({"threadId": "th1", "turn": {"id": tid, "items": [], "status": "completed"}}))).await;
                            }
                        }
                        Some("turn/steer") => {
                            send(
                                &mut out,
                                reply(json!({"turnId": v["params"]["expectedTurnId"]})),
                            )
                            .await
                        }
                        Some("turn/interrupt") => {
                            send(&mut out, reply(json!({}))).await;
                            let tid = v["params"]["turnId"].clone();
                            send(&mut out, note("turn/completed", json!({"threadId": "th1", "turn": {"id": tid, "items": [], "status": "interrupted"}}))).await;
                        }
                        None if id == json!(900) => {
                            let ok = v["result"]["decision"] != "decline";
                            send(&mut out, note("item/completed", json!({"threadId": "th1", "turnId": "u1", "completedAtMs": 2,
                                "item": {"type": "commandExecution", "id": "c1", "command": "x", "commandActions": [], "cwd": "/w",
                                    "status": if ok { "completed" } else { "declined" }, "aggregatedOutput": "done", "exitCode": 0}}))).await;
                            send(&mut out, note("turn/completed", json!({"threadId": "th1", "turn": {"id": "u1", "items": [], "status": "completed"}}))).await;
                        }
                        _ => {}
                    }
                }
            };
            tokio::select! {
                _ = play => {}
                _ = stop.wait_for(Option::is_some) => {}
            }
        });
        let signal = Box::new(move |s: Sig| {
            let _ = sig_tx.send(Some(s));
            Ok(())
        });
        let mut rx = sig_rx;
        let exit = Box::pin(async move {
            let _ = rx.wait_for(Option::is_some).await;
            Ok(ExitInfo {
                code: None,
                signal: Some(15),
            })
        });
        Ok(Spawned {
            pid: None,
            stdin: Box::pin(stdin),
            stdout: Box::pin(stdout),
            stderr: None,
            signal,
            exit,
        })
    }
}

fn spec(workdir: &std::path::Path) -> StartSpec {
    let workdir = workdir.canonicalize().unwrap();
    StartSpec {
        session_id: Uuid::now_v7(),
        runtime: Runtime::Codex,
        model: Some("gpt-5.6".into()),
        workspace: workdir.clone(),
        workdir,
        environment: Arc::new(crate::config::Environment::default_of(
            &crate::permissions::Permissions::default(),
        )),
        runtime_session_id: None,
        system_prompt: None,
        disallowed_tools: vec![],
        restricted: false,
        auto_approve: false,
        session_token: crate::token::Secret::new("st"),
        ask_timeout_s: 600,
        permission_mode: PermissionMode::Ask,
        folder_rules: None,
        has_history: false,
        provider: None,
        provider_refused: None,
        secrets: None,
        browser: false,
        browser_refused: None,
        mcp_servers: vec![],
        local_mcp: None,
        skills: vec![],
        skill_dir: None,
        acp_agent: None,
        fork: None,
    }
}

fn adapters(fake: &FakeCodex) -> CodexAdapters {
    let dir = std::env::temp_dir().join("hqc-codex-tests");
    CodexAdapters::new(
        CodexOptions {
            codex: Some(Arc::new(CodexConfig {
                command: "/opt/test/codex".into(),
                environments: vec!["default".into()],
            })),
            mcp: Some(McpLaunch {
                program: "/opt/test/hqc-worker".into(),
                socket: dir.join("mcp.sock"),
            }),
            ssh: crate::ssh::Store::in_home(&dir),
            setup_timeout: Duration::from_secs(5),
            answer_timeout: Duration::from_secs(2),
            stop_grace: Duration::from_secs(5),
            kill_grace: Duration::from_millis(100),
        },
        Arc::new(fake.clone()),
    )
}

fn input(text: &str) -> SessionInput {
    SessionInput {
        session_id: Uuid::nil(),
        event_id: Uuid::now_v7(),
        text: text.into(),
        artifact_ids: vec![],
        peer_prefix: vec![],
        peer: None,
        permission_cap: None,
        schedule_id: None,
        human: true,
    }
}

async fn until(
    rx: &mut mpsc::Receiver<AdapterEvent>,
    pred: impl Fn(&AdapterEvent) -> bool,
) -> Vec<AdapterEvent> {
    let mut seen = Vec::new();
    loop {
        let e = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out; seen: {seen:#?}"))
            .expect("events end");
        let done = pred(&e);
        seen.push(e);
        if done {
            return seen;
        }
    }
}

fn kinds(events: &[AdapterEvent]) -> Vec<&EventKind> {
    events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::Event(e) => Some(&e.kind),
            AdapterEvent::ToolResult { event, .. } => Some(&event.kind),
            _ => None,
        })
        .collect()
}

async fn started(
    fake: &FakeCodex,
    s: &StartSpec,
) -> (Box<dyn Adapter>, mpsc::Receiver<AdapterEvent>) {
    let (tx, mut rx) = mpsc::channel(256);
    let mut ad = adapters(fake).create(s, tx);
    ad.start(s).await.unwrap();
    until(&mut rx, |e| matches!(e, AdapterEvent::PermissionMode(_))).await;
    (ad, rx)
}

#[test]
fn only_the_pinned_versions_are_spoken() {
    assert!(supported("0.172.0"));
    assert!(supported("0.172.3"));
    assert!(!supported("0.171.9"));
    assert!(!supported("0.173.0"));
}

#[tokio::test]
async fn a_thread_starts_in_the_sessions_mode_with_hqcs_servers() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeCodex::default();
    let s = spec(dir.path());
    let (tx, mut rx) = mpsc::channel(256);
    let mut ad = adapters(&fake).create(&s, tx);
    ad.start(&s).await.unwrap();
    let seen = until(&mut rx, |e| matches!(e, AdapterEvent::PermissionMode(_))).await;
    assert!(seen.contains(&AdapterEvent::RuntimeSessionId("th1".into())));
    assert_eq!(
        seen.last(),
        Some(&AdapterEvent::PermissionMode(PermissionMode::Ask))
    );
    let got = fake.got();
    // App Server messages carry no `jsonrpc` field.
    assert!(got.iter().all(|v| v.get("jsonrpc").is_none()), "{got:?}");
    assert_eq!(got[0]["method"], "initialize");
    assert_eq!(got[1]["method"], "initialized");
    let start = &fake.calls("thread/start")[0]["params"];
    assert_eq!(start["approvalPolicy"], "on-request");
    assert_eq!(start["sandbox"], "read-only");
    assert_eq!(start["model"], "gpt-5.6");
    assert_eq!(
        start["config"]["mcp_servers"]["hqc"]["command"],
        "/opt/test/hqc-worker"
    );
    // The token never goes into the thread's config: the App Server's environment holds it, and
    // the MCP server names it.
    assert_eq!(
        start["config"]["mcp_servers"]["hqc"]["env_vars"],
        json!(["HQC_SESSION_TOKEN"])
    );
    assert!(!start.to_string().contains("\"st\""), "{start}");
    assert_eq!(
        start["config"]["shell_environment_policy"]["exclude"],
        json!(["HQC_*"])
    );
    let env = fake.commands.lock().unwrap()[0].env.clone();
    assert!(env.contains(&("HQC_SESSION_TOKEN".to_string(), "st".to_string())));
    assert_eq!(
        fake.commands.lock().unwrap()[0].args,
        vec!["app-server".to_string()]
    );
    ad.kill().await.unwrap();
}

#[tokio::test]
async fn a_turn_streams_its_answer_plan_and_usage() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeCodex::default();
    let s = spec(dir.path());
    let (mut ad, mut rx) = started(&fake, &s).await;
    let i = input("hello");
    ad.input(&i).await.unwrap();
    let seen = until(&mut rx, |e| matches!(e, AdapterEvent::TurnEnded { .. })).await;
    let k = kinds(&seen);
    let finals: Vec<_> = k
        .iter()
        .filter_map(|k| match k {
            EventKind::AgentMessage {
                text,
                r#final: true,
                in_reply_to,
                ..
            } => Some((text.clone(), *in_reply_to)),
            _ => None,
        })
        .collect();
    assert_eq!(finals, vec![("Hi there".to_string(), Some(i.event_id))]);
    assert!(k
        .iter()
        .any(|k| matches!(k, EventKind::Tasks { items } if items.len() == 2)));
    assert!(k.iter().any(|k| matches!(
        k,
        EventKind::Usage {
            input_tokens: 1000,
            output_tokens: 20,
            cache_read: Some(800),
            context_tokens: Some(1020),
            context_limit: Some(400000),
            session_tokens: Some(5100),
            ..
        }
    )));
    // The first turn sends no policy: the thread started in its mode.
    let turn = &fake.calls("turn/start")[0]["params"];
    assert!(turn.get("sandboxPolicy").is_none(), "{turn}");
    ad.kill().await.unwrap();
}

#[tokio::test]
async fn a_denied_command_is_declined_and_another_is_asked() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeCodex::default();
    let s = spec(dir.path());
    let (mut ad, mut rx) = started(&fake, &s).await;
    ad.input(&input("run sudo rm -rf /x")).await.unwrap();
    let seen = until(&mut rx, |e| matches!(e, AdapterEvent::TurnEnded { .. })).await;
    assert!(kinds(&seen)
        .iter()
        .any(|k| matches!(k, EventKind::System { code, .. } if code == POLICY_DENIED)));
    assert!(!kinds(&seen)
        .iter()
        .any(|k| matches!(k, EventKind::ApprovalRequest { .. })));
    let answer = fake
        .got()
        .into_iter()
        .find(|v| v["id"] == 900 && v.get("result").is_some())
        .unwrap();
    assert_eq!(answer["result"]["decision"], "decline");
    ad.kill().await.unwrap();

    let fake = FakeCodex::default();
    let (mut ad, mut rx) = started(&fake, &s).await;
    ad.input(&input("run cargo build")).await.unwrap();
    let seen = until(&mut rx, |e| {
        matches!(e, AdapterEvent::Event(ev) if matches!(ev.kind, EventKind::ApprovalRequest { .. }))
    })
    .await;
    let Some(EventKind::ApprovalRequest {
        request_id,
        options,
        args_preview,
        ..
    }) = kinds(&seen).into_iter().last().cloned()
    else {
        panic!("no card")
    };
    assert_eq!(args_preview.as_deref(), Some("cargo build"));
    assert_eq!(
        options
            .iter()
            .map(|o| (o.id.as_str(), o.canonical))
            .collect::<Vec<_>>(),
        vec![
            ("accept", ApprovalCanonical::Once),
            ("acceptForSession", ApprovalCanonical::Session),
            ("decline", ApprovalCanonical::Deny),
            ("cancel", ApprovalCanonical::Deny)
        ]
    );
    ad.answer_approval(&request_id, "acceptForSession", None)
        .await
        .unwrap();
    let seen = until(&mut rx, |e| matches!(e, AdapterEvent::TurnEnded { .. })).await;
    assert!(kinds(&seen)
        .iter()
        .any(|k| matches!(k, EventKind::ToolResult { error: false, .. })));
    let answer = fake
        .got()
        .into_iter()
        .find(|v| v["id"] == 900 && v.get("result").is_some())
        .unwrap();
    assert_eq!(answer["result"]["decision"], "acceptForSession");
    ad.kill().await.unwrap();
}

#[tokio::test]
async fn input_during_a_turn_steers_it_and_stop_interrupts() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeCodex::default();
    let s = spec(dir.path());
    let (mut ad, mut rx) = started(&fake, &s).await;
    ad.input(&input("wait")).await.unwrap();
    until(&mut rx, |e| matches!(e, AdapterEvent::InputAccepted { .. })).await;
    // `turn/started` gives the turn's id; then it takes input.
    for _ in 0..100 {
        if ad.can_inject() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let more = input("also this");
    assert!(ad.inject_with(&more, &[]).await.unwrap());
    let seen = until(&mut rx, |e| matches!(e, AdapterEvent::InputAccepted { .. })).await;
    assert!(seen.contains(&AdapterEvent::InputAccepted {
        event_id: more.event_id,
        joined: true
    }));
    let steer = &fake.calls("turn/steer")[0]["params"];
    assert_eq!(steer["expectedTurnId"], "u1");
    ad.stop().await.unwrap();
    let seen = until(&mut rx, |e| matches!(e, AdapterEvent::TurnEnded { .. })).await;
    assert!(!kinds(&seen)
        .iter()
        .any(|k| matches!(k, EventKind::AgentMessage { r#final: true, .. })));
    assert_eq!(fake.calls("turn/interrupt")[0]["params"]["turnId"], "u1");
    ad.kill().await.unwrap();
}

#[tokio::test]
async fn a_mode_goes_with_the_next_turn_and_only_when_it_changed() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeCodex::default();
    let s = spec(dir.path());
    let (mut ad, mut rx) = started(&fake, &s).await;
    ad.set_permission_mode(PermissionMode::Auto).await.unwrap();
    until(&mut rx, |e| {
        *e == AdapterEvent::PermissionMode(PermissionMode::Auto)
    })
    .await;
    for _ in 0..2 {
        ad.input(&input("hello")).await.unwrap();
        until(&mut rx, |e| matches!(e, AdapterEvent::TurnEnded { .. })).await;
    }
    let turns = fake.calls("turn/start");
    assert_eq!(turns[0]["params"]["approvalPolicy"], "on-request");
    assert_eq!(
        turns[0]["params"]["sandboxPolicy"],
        json!({"type": "workspaceWrite"})
    );
    assert!(turns[1]["params"].get("sandboxPolicy").is_none());
    // During a turn a switch is refused: the supervisor asks again before the next one.
    ad.input(&input("wait")).await.unwrap();
    until(&mut rx, |e| matches!(e, AdapterEvent::InputAccepted { .. })).await;
    assert!(ad.set_permission_mode(PermissionMode::Ask).await.is_err());
    ad.kill().await.unwrap();
}

#[tokio::test]
async fn a_signed_out_codex_fails_the_start_with_a_line() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeCodex {
        signed_out: true,
        ..FakeCodex::default()
    };
    let s = spec(dir.path());
    let (tx, mut rx) = mpsc::channel(256);
    let mut ad = adapters(&fake).create(&s, tx);
    assert!(ad.start(&s).await.is_err());
    until(&mut rx, |e| {
        matches!(e, AdapterEvent::Event(ev) if matches!(&ev.kind, EventKind::System { code, .. } if code == AGENT_SIGNED_OUT))
    })
    .await;
    assert!(fake.calls("thread/start").is_empty());
}

#[tokio::test]
async fn a_thread_is_resumed_and_a_lost_one_started_anew() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = spec(dir.path());
    s.runtime_session_id = Some("th1".into());
    let fake = FakeCodex::default();
    let (mut ad, _rx) = started(&fake, &s).await;
    assert_eq!(
        fake.calls("thread/resume")[0]["params"]["excludeTurns"],
        true
    );
    assert!(fake.calls("thread/start").is_empty());
    ad.kill().await.unwrap();

    let fake = FakeCodex {
        lost: true,
        ..FakeCodex::default()
    };
    let (tx, mut rx) = mpsc::channel(256);
    let mut ad = adapters(&fake).create(&s, tx);
    ad.resume(&s).await.unwrap();
    let seen = until(&mut rx, |e| matches!(e, AdapterEvent::PermissionMode(_))).await;
    assert!(kinds(&seen)
        .iter()
        .any(|k| matches!(k, EventKind::System { code, .. } if code == CONVERSATION_LOST)));
    assert_eq!(fake.calls("thread/start").len(), 1);
    ad.kill().await.unwrap();
}

/// Security review of the Codex adapter: a command is allowed by a rule only when it runs in the
/// session's own folder, resolved; a symlink or `..` out of it, an unresolvable `cwd` or none is
/// a card.
#[tokio::test]
async fn a_rule_allows_a_command_only_in_the_sessions_own_folder() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("h")).unwrap();
    let mut s = spec(dir.path());
    s.environment = Arc::new(crate::config::Environment::default_of(
        &crate::permissions::Permissions {
            allow: vec!["Bash(ls:*)".into()],
            ..Default::default()
        },
    ));
    let root = s.workdir.display().to_string();
    for (cwd, auto) in [
        (root.clone(), true),
        (format!("{root}/h"), false),
        (format!("{root}/.."), false),
        (format!("{root}/missing"), false),
    ] {
        let fake = FakeCodex::default();
        let (mut ad, mut rx) = started(&fake, &s).await;
        ad.input(&input(&format!("runat {cwd} ls -la")))
            .await
            .unwrap();
        let seen = until(&mut rx, |e| {
            matches!(e, AdapterEvent::TurnEnded { .. })
                || matches!(e, AdapterEvent::Event(ev) if matches!(ev.kind, EventKind::ApprovalRequest { .. }))
        })
        .await;
        let card = kinds(&seen)
            .iter()
            .any(|k| matches!(k, EventKind::ApprovalRequest { .. }));
        assert_eq!(!card, auto, "{cwd}");
        ad.kill().await.unwrap();
    }
    // No `cwd` at all: a card too.
    let fake = FakeCodex::default();
    let (mut ad, mut rx) = started(&fake, &s).await;
    ad.input(&input("run ls -la")).await.unwrap();
    until(&mut rx, |e| {
        matches!(e, AdapterEvent::Event(ev) if matches!(ev.kind, EventKind::ApprovalRequest { .. }))
    })
    .await;
    ad.kill().await.unwrap();
}

/// Security review of the Codex adapter: a refused `turn/start` leaves its mode unsent, so the
/// next turn sends it again.
#[tokio::test]
async fn a_refused_turn_leaves_its_mode_to_be_sent_again() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeCodex::default();
    let s = spec(dir.path());
    let (mut ad, mut rx) = started(&fake, &s).await;
    ad.set_permission_mode(PermissionMode::Auto).await.unwrap();
    ad.input(&input("toobig")).await.unwrap();
    until(&mut rx, |e| matches!(e, AdapterEvent::TurnEnded { .. })).await;
    ad.input(&input("hello")).await.unwrap();
    until(&mut rx, |e| matches!(e, AdapterEvent::TurnEnded { .. })).await;
    let turns = fake.calls("turn/start");
    assert_eq!(turns.len(), 2);
    for t in &turns {
        assert_eq!(
            t["params"]["sandboxPolicy"],
            json!({"type": "workspaceWrite"}),
            "{t}"
        );
    }
    ad.kill().await.unwrap();
}
