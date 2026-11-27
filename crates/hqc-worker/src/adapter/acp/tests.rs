//! The ACP adapter against a fake agent that speaks ACP v1 over in-memory pipes.

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
use crate::config::{AcpModes, EnvVars};

/// What the fake agent does; every message it got is recorded.
#[derive(Clone, Default)]
struct FakeAgent {
    got: Arc<Mutex<Vec<Value>>>,
    commands: Arc<Mutex<Vec<CommandSpec>>>,
    /// `session/new` fails with this error code.
    new_error: Option<i64>,
    /// `initialize` offers `session/resume`.
    resume: bool,
}

impl FakeAgent {
    fn got(&self) -> Vec<Value> {
        self.got.lock().unwrap().clone()
    }

    fn methods(&self) -> Vec<String> {
        self.got()
            .iter()
            .filter_map(|v| v["method"].as_str().map(str::to_owned))
            .collect()
    }
}

async fn send(out: &mut tokio::io::DuplexStream, v: Value) {
    out.write_all(format!("{v}\n").as_bytes()).await.unwrap();
}

fn update(update: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": "session/update",
        "params": {"sessionId": "s1", "update": update}})
}

impl Spawner for FakeAgent {
    fn spawn(&self, cmd: &CommandSpec) -> io::Result<Spawned> {
        self.commands.lock().unwrap().push(cmd.clone());
        let (stdin, agent_in) = tokio::io::duplex(1 << 20);
        let (mut out, stdout) = tokio::io::duplex(1 << 20);
        let (sig_tx, sig_rx) = watch::channel::<Option<Sig>>(None);
        let me = self.clone();
        let mut stop = sig_rx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(agent_in).lines();
            // The prompt waiting for its answer, and what it waits for.
            let mut prompt: Option<(Value, &str)> = None;
            let play = async {
                while let Ok(Some(line)) = lines.next_line().await {
                    let v: Value = serde_json::from_str(&line).unwrap();
                    me.got.lock().unwrap().push(v.clone());
                    let id = v["id"].clone();
                    let reply =
                        |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
                    match v["method"].as_str() {
                        Some("initialize") => {
                            let mut caps = json!({"loadSession": true});
                            if me.resume {
                                caps["sessionCapabilities"] = json!({"resume": {}});
                            }
                            send(
                                &mut out,
                                reply(json!({"protocolVersion": 1,
                                "agentCapabilities": caps,
                                "agentInfo": {"name": "fake", "version": "1.2.3"}})),
                            )
                            .await
                        }
                        Some("session/new") => match me.new_error {
                            Some(code) => {
                                send(
                                    &mut out,
                                    json!({"jsonrpc": "2.0", "id": id,
                                "error": {"code": code, "message": "Authentication required"}}),
                                )
                                .await
                            }
                            None => {
                                send(
                                    &mut out,
                                    reply(json!({"sessionId": "s1",
                                "modes": {"currentModeId": "default", "availableModes": [
                                    {"id": "default", "name": "Default"},
                                    {"id": "accept-edits", "name": "Accept edits"},
                                    {"id": "broken", "name": "Broken"}]},
                                "configOptions": [{"id": "model", "name": "Model",
                                    "category": "model", "type": "select", "currentValue": "m1",
                                    "options": [{"value": "m1", "name": "M1"},
                                        {"value": "m2", "name": "M2"}]}]})),
                                )
                                .await
                            }
                        },
                        Some("session/set_mode") if v["params"]["modeId"] == "broken" => {
                            send(
                                &mut out,
                                json!({"jsonrpc": "2.0", "id": id,
                                "error": {"code": -32602, "message": "no such mode"}}),
                            )
                            .await
                        }
                        Some(
                            "session/resume"
                            | "session/load"
                            | "session/set_mode"
                            | "session/set_config_option"
                            | "session/close",
                        ) => send(&mut out, reply(json!({}))).await,
                        Some("session/prompt") => {
                            let text = v["params"]["prompt"][0]["text"].as_str().unwrap_or("");
                            if let Some(cmd) = text.strip_prefix("run ") {
                                send(
                                    &mut out,
                                    update(json!({"sessionUpdate": "tool_call",
                                    "toolCallId": "t1", "title": format!("Run {cmd}"),
                                    "kind": "execute", "status": "pending",
                                    "rawInput": {"command": cmd}})),
                                )
                                .await;
                                send(&mut out, json!({"jsonrpc": "2.0", "id": 900,
                                    "method": "session/request_permission",
                                    "params": {"sessionId": "s1",
                                        "toolCall": {"toolCallId": "t1", "title": format!("Run {cmd}"),
                                            "kind": "execute", "rawInput": {"command": cmd}},
                                        "options": [
                                            {"optionId": "yes", "name": "Allow", "kind": "allow_once"},
                                            {"optionId": "always", "name": "Allow for this session", "kind": "allow_always"},
                                            {"optionId": "no", "name": "Deny", "kind": "reject_once"}]}})).await;
                                prompt = Some((id.clone(), "permission"));
                            } else if let Some(tc) = text.strip_prefix("ask ") {
                                // A permission request with this `toolCall`, as an agent sends it.
                                let tc: Value = serde_json::from_str(tc).unwrap();
                                send(&mut out, json!({"jsonrpc": "2.0", "id": 900,
                                    "method": "session/request_permission",
                                    "params": {"sessionId": "s1", "toolCall": tc,
                                        "options": [
                                            {"optionId": "yes", "name": "Allow", "kind": "allow_once"},
                                            {"optionId": "no", "name": "Deny", "kind": "reject_once"}]}})).await;
                                prompt = Some((id.clone(), "permission"));
                            } else if let Some(m) = text.strip_prefix("selfmode ") {
                                // The agent moves itself (its own command, a tool).
                                send(
                                    &mut out,
                                    update(json!({"sessionUpdate": "current_mode_update",
                                    "currentModeId": m})),
                                )
                                .await;
                                send(&mut out, reply(json!({"stopReason": "end_turn"}))).await;
                            } else if text == "wait" {
                                prompt = Some((id.clone(), "cancel"));
                            } else {
                                send(
                                    &mut out,
                                    update(json!({"sessionUpdate": "agent_thought_chunk",
                                    "content": {"type": "text", "text": "Thinking."}})),
                                )
                                .await;
                                send(
                                    &mut out,
                                    update(json!({"sessionUpdate": "agent_message_chunk",
                                    "content": {"type": "text", "text": "Hi "}})),
                                )
                                .await;
                                send(
                                    &mut out,
                                    update(json!({"sessionUpdate": "agent_message_chunk",
                                    "content": {"type": "text", "text": "there"}})),
                                )
                                .await;
                                send(
                                    &mut out,
                                    update(json!({"sessionUpdate": "usage_update",
                                    "used": 1200, "size": 200000})),
                                )
                                .await;
                                send(
                                    &mut out,
                                    update(json!({"sessionUpdate": "session_info_update",
                                    "title": "Greeting"})),
                                )
                                .await;
                                send(&mut out, reply(json!({"stopReason": "end_turn"}))).await;
                            }
                        }
                        Some("session/cancel") => {
                            if let Some((pid, _)) = prompt.take() {
                                send(
                                    &mut out,
                                    json!({"jsonrpc": "2.0", "id": pid,
                                    "result": {"stopReason": "cancelled"}}),
                                )
                                .await;
                            }
                        }
                        None if id == json!(900) => {
                            // The answer to the permission request.
                            let status = if v["result"]["outcome"]["optionId"] == "yes" {
                                "completed"
                            } else {
                                "failed"
                            };
                            send(&mut out, update(json!({"sessionUpdate": "tool_call_update",
                                "toolCallId": "t1", "status": status,
                                "content": [{"type": "content", "content": {"type": "text", "text": "done"}}]}))).await;
                            if let Some((pid, _)) = prompt.take() {
                                send(
                                    &mut out,
                                    json!({"jsonrpc": "2.0", "id": pid,
                                    "result": {"stopReason": "end_turn"}}),
                                )
                                .await;
                            }
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

fn agent(modes: AcpModes) -> Arc<AcpAgent> {
    Arc::new(AcpAgent {
        name: "fake".into(),
        command: "/opt/test/fake-agent".into(),
        args: vec!["acp".into()],
        env: EnvVars::default(),
        environments: vec!["default".into()],
        modes,
        allow_all_args: vec![],
        warning: None,
    })
}

fn spec(workdir: &std::path::Path, agent: Arc<AcpAgent>) -> StartSpec {
    let workdir = workdir.canonicalize().unwrap();
    StartSpec {
        session_id: Uuid::now_v7(),
        runtime: Runtime::Acp,
        model: None,
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
        acp_agent: Some(agent),
        fork: None,
    }
}

fn adapters(fake: &FakeAgent) -> AcpAdapters {
    let dir = std::env::temp_dir().join("hqc-acp-tests");
    AcpAdapters::new(
        AcpOptions {
            mcp: Some(McpLaunch {
                program: "/opt/test/hqc-worker".into(),
                socket: dir.join("mcp.sock"),
            }),
            ssh: crate::ssh::Store::in_home(&dir),
            setup_timeout: Duration::from_secs(5),
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

/// Events until `until` holds for one; panics after 5 s.
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

#[tokio::test]
async fn a_turn_streams_and_ends_with_the_final_message() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeAgent::default();
    let a = agent(AcpModes::default());
    let s = spec(dir.path(), a);
    let (tx, mut rx) = mpsc::channel(256);
    let mut ad = adapters(&fake).create(&s, tx);
    ad.start(&s).await.unwrap();
    let seen = until(&mut rx, |e| matches!(e, AdapterEvent::PermissionMode(_))).await;
    assert!(seen.contains(&AdapterEvent::RuntimeSessionId("s1".into())));
    // No `modes` in the table: never switched, runs Ask.
    assert!(!fake.methods().contains(&"session/set_mode".to_string()));
    assert_eq!(
        seen.last(),
        Some(&AdapterEvent::PermissionMode(PermissionMode::Ask))
    );
    // hqc's MCP server goes in `mcpServers`, its token in its `env`.
    let new = fake
        .got()
        .into_iter()
        .find(|v| v["method"] == "session/new")
        .unwrap();
    assert_eq!(new["params"]["mcpServers"][0]["name"], "hqc");
    assert_eq!(
        new["params"]["mcpServers"][0]["command"],
        "/opt/test/hqc-worker"
    );
    assert_eq!(new["params"]["mcpServers"][0]["env"][0]["value"], "st");
    let init = fake
        .got()
        .into_iter()
        .find(|v| v["method"] == "initialize")
        .unwrap();
    assert_eq!(init["params"]["clientCapabilities"]["terminal"], false);
    let cmd = fake.commands.lock().unwrap()[0].clone();
    assert_eq!(cmd.args, vec!["acp".to_string()]);

    let i = input("hello");
    ad.input(&i).await.unwrap();
    let seen = until(&mut rx, |e| matches!(e, AdapterEvent::TurnEnded { .. })).await;
    assert!(seen.contains(&AdapterEvent::InputAccepted {
        event_id: i.event_id,
        joined: false
    }));
    assert!(seen.contains(&AdapterEvent::Title("Greeting".into())));
    let k = kinds(&seen);
    assert!(k
        .iter()
        .any(|k| matches!(k, EventKind::AgentThinking { text } if text == "Thinking.")));
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
    assert!(k.iter().any(|k| matches!(
        k,
        EventKind::Usage {
            context_tokens: Some(1200),
            context_limit: Some(200000),
            ..
        }
    )));
    assert!(k.iter().any(|k| matches!(
        k,
        EventKind::Status {
            state: SessionState::Idle,
            ..
        }
    )));
    ad.kill().await.unwrap();
}

#[tokio::test]
async fn a_denied_command_is_refused_without_a_card() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeAgent::default();
    let s = spec(dir.path(), agent(AcpModes::default()));
    let (tx, mut rx) = mpsc::channel(256);
    let mut ad = adapters(&fake).create(&s, tx);
    ad.start(&s).await.unwrap();
    until(&mut rx, |e| matches!(e, AdapterEvent::PermissionMode(_))).await;
    // `Bash(sudo:*)` is in the default deny list.
    ad.input(&input("run sudo rm -rf /tmp/x")).await.unwrap();
    let seen = until(&mut rx, |e| matches!(e, AdapterEvent::TurnEnded { .. })).await;
    let k = kinds(&seen);
    assert!(!k
        .iter()
        .any(|k| matches!(k, EventKind::ApprovalRequest { .. })));
    assert!(k
        .iter()
        .any(|k| matches!(k, EventKind::System { code, .. } if code == POLICY_DENIED)));
    let answer = fake.got().into_iter().find(|v| v["id"] == 900).unwrap();
    assert_eq!(answer["result"]["outcome"]["optionId"], "no");
    ad.kill().await.unwrap();
}

#[tokio::test]
async fn a_command_is_asked_with_the_agents_options() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeAgent::default();
    let s = spec(dir.path(), agent(AcpModes::default()));
    let (tx, mut rx) = mpsc::channel(256);
    let mut ad = adapters(&fake).create(&s, tx);
    ad.start(&s).await.unwrap();
    until(&mut rx, |e| matches!(e, AdapterEvent::PermissionMode(_))).await;
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
    let ids: Vec<_> = options
        .iter()
        .map(|o| (o.id.as_str(), o.canonical))
        .collect();
    assert_eq!(
        ids,
        vec![
            ("yes", ApprovalCanonical::Once),
            ("always", ApprovalCanonical::Session),
            ("no", ApprovalCanonical::Deny)
        ]
    );
    ad.answer_approval(&request_id, "yes", None).await.unwrap();
    let seen = until(&mut rx, |e| matches!(e, AdapterEvent::TurnEnded { .. })).await;
    assert!(kinds(&seen)
        .iter()
        .any(|k| matches!(k, EventKind::ToolResult { error: false, result_preview, .. } if result_preview.starts_with("done"))));
    ad.kill().await.unwrap();
}

#[tokio::test]
async fn stop_cancels_the_turn_without_a_final_message() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeAgent::default();
    let s = spec(dir.path(), agent(AcpModes::default()));
    let (tx, mut rx) = mpsc::channel(256);
    let mut ad = adapters(&fake).create(&s, tx);
    ad.start(&s).await.unwrap();
    until(&mut rx, |e| matches!(e, AdapterEvent::PermissionMode(_))).await;
    ad.input(&input("wait")).await.unwrap();
    until(&mut rx, |e| matches!(e, AdapterEvent::InputAccepted { .. })).await;
    ad.stop().await.unwrap();
    let seen = until(&mut rx, |e| matches!(e, AdapterEvent::TurnEnded { .. })).await;
    assert!(!kinds(&seen)
        .iter()
        .any(|k| matches!(k, EventKind::AgentMessage { r#final: true, .. })));
    assert!(fake.methods().contains(&"session/cancel".to_string()));
    ad.kill().await.unwrap();
}

#[tokio::test]
async fn modes_and_the_model_are_set_by_the_agents_ids() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeAgent::default();
    let a = agent(AcpModes {
        ask: Some("default".into()),
        auto: Some("accept-edits".into()),
        allow_all: None,
        ask_also: vec![],
    });
    let mut s = spec(dir.path(), a);
    // Allow all asked, the table has none: the next lower, Auto.
    s.permission_mode = PermissionMode::AllowAll;
    s.model = Some("m2".into());
    let (tx, mut rx) = mpsc::channel(256);
    let mut ad = adapters(&fake).create(&s, tx);
    ad.start(&s).await.unwrap();
    let seen = until(&mut rx, |e| matches!(e, AdapterEvent::PermissionMode(_))).await;
    assert_eq!(
        seen.last(),
        Some(&AdapterEvent::PermissionMode(PermissionMode::Auto))
    );
    // The model is set after the mode; wait for it.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let got = fake.got();
    let set_mode = got
        .iter()
        .find(|v| v["method"] == "session/set_mode")
        .unwrap();
    assert_eq!(set_mode["params"]["modeId"], "accept-edits");
    let set_model = got
        .iter()
        .find(|v| v["method"] == "session/set_config_option")
        .unwrap();
    assert_eq!(set_model["params"]["configId"], "model");
    assert_eq!(set_model["params"]["value"], "m2");
    ad.set_permission_mode(PermissionMode::Ask).await.unwrap();
    until(&mut rx, |e| {
        *e == AdapterEvent::PermissionMode(PermissionMode::Ask)
    })
    .await;
    ad.kill().await.unwrap();
}

#[tokio::test]
async fn a_signed_out_agent_fails_the_start_with_a_line() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeAgent {
        new_error: Some(-32000),
        ..FakeAgent::default()
    };
    let s = spec(dir.path(), agent(AcpModes::default()));
    let (tx, mut rx) = mpsc::channel(256);
    let mut ad = adapters(&fake).create(&s, tx);
    assert!(ad.start(&s).await.is_err());
    let seen = until(&mut rx, |e| {
        matches!(e, AdapterEvent::Event(ev) if matches!(&ev.kind, EventKind::System { code, .. } if code == AGENT_SIGNED_OUT))
    })
    .await;
    assert!(!seen.is_empty());
}

#[tokio::test]
async fn a_session_is_continued_by_resume() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeAgent {
        resume: true,
        ..FakeAgent::default()
    };
    let mut s = spec(dir.path(), agent(AcpModes::default()));
    s.runtime_session_id = Some("s1".into());
    let (tx, mut rx) = mpsc::channel(256);
    let mut ad = adapters(&fake).create(&s, tx);
    ad.resume(&s).await.unwrap();
    let seen = until(&mut rx, |e| matches!(e, AdapterEvent::PermissionMode(_))).await;
    assert!(!seen
        .iter()
        .any(|e| matches!(e, AdapterEvent::RuntimeSessionId(_))));
    let m = fake.methods();
    assert!(m.contains(&"session/resume".to_string()));
    assert!(!m.contains(&"session/new".to_string()));
    ad.kill().await.unwrap();
}

#[test]
fn execute_commands_are_read_from_common_keys() {
    assert_eq!(
        execute_command(&json!({"command": "ls -la"})).as_deref(),
        Some("ls -la")
    );
    assert_eq!(
        execute_command(&json!({"cmd": ["git", "commit", "-m", "a b"]})).as_deref(),
        Some("git commit -m 'a b'")
    );
    assert_eq!(execute_command(&json!({"path": "x"})), None);
    assert_eq!(execute_command(&json!({"command": ["ls", 1]})), None);
}

#[test]
fn an_agent_runs_the_highest_mode_its_table_has() {
    let a = agent(AcpModes {
        ask: Some("a".into()),
        auto: None,
        allow_all: None,
        ask_also: vec![],
    });
    assert_eq!(a.mode_for(PermissionMode::AllowAll), PermissionMode::Ask);
    let mut b = (*agent(AcpModes::default())).clone();
    b.allow_all_args = vec!["--yolo".into()];
    assert_eq!(
        b.mode_for(PermissionMode::AllowAll),
        PermissionMode::AllowAll
    );
    assert_eq!(b.mode_for(PermissionMode::Auto), PermissionMode::Ask);
}

/// Security review of part 1: only a mode the agent took is reported; a switch it refused, or
/// one its table has no id for, is an error, so the supervisor starts it again lower (T39).
#[tokio::test]
async fn a_mode_the_agent_did_not_take_is_never_reported() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeAgent::default();
    let a = agent(AcpModes {
        ask: Some("default".into()),
        auto: Some("broken".into()),
        allow_all: None,
        ask_also: vec![],
    });
    let s = spec(dir.path(), a);
    let (tx, mut rx) = mpsc::channel(256);
    let mut ad = adapters(&fake).create(&s, tx);
    ad.start(&s).await.unwrap();
    until(&mut rx, |e| {
        *e == AdapterEvent::PermissionMode(PermissionMode::Ask)
    })
    .await;
    // The agent refuses `broken`: an error, and no Auto reported.
    assert!(ad.set_permission_mode(PermissionMode::Auto).await.is_err());
    // Allow all has no id: Auto's `broken` again, refused.
    assert!(ad
        .set_permission_mode(PermissionMode::AllowAll)
        .await
        .is_err());
    ad.kill().await.unwrap();
    while let Ok(e) = rx.try_recv() {
        assert_ne!(e, AdapterEvent::PermissionMode(PermissionMode::Auto));
    }

    // A start in a mode the agent refuses falls to Ask, set and reported.
    let mut s2 = spec(dir.path(), s.acp_agent.clone().unwrap());
    s2.permission_mode = PermissionMode::Auto;
    let (tx, mut rx) = mpsc::channel(256);
    let mut ad = adapters(&fake).create(&s2, tx);
    ad.start(&s2).await.unwrap();
    let seen = until(&mut rx, |e| matches!(e, AdapterEvent::PermissionMode(_))).await;
    assert_eq!(
        seen.last(),
        Some(&AdapterEvent::PermissionMode(PermissionMode::Ask))
    );
    ad.kill().await.unwrap();
}

#[tokio::test]
async fn allow_all_by_a_start_flag_is_left_only_by_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeAgent::default();
    let mut a = (*agent(AcpModes {
        ask: Some("default".into()),
        auto: None,
        allow_all: None,
        ask_also: vec![],
    }))
    .clone();
    a.allow_all_args = vec!["--yolo".into()];
    let mut s = spec(dir.path(), Arc::new(a));
    s.permission_mode = PermissionMode::AllowAll;
    let (tx, mut rx) = mpsc::channel(256);
    let mut ad = adapters(&fake).create(&s, tx);
    ad.start(&s).await.unwrap();
    until(&mut rx, |e| {
        *e == AdapterEvent::PermissionMode(PermissionMode::AllowAll)
    })
    .await;
    assert!(fake.commands.lock().unwrap()[0]
        .args
        .contains(&"--yolo".to_string()));
    assert!(ad.set_permission_mode(PermissionMode::Ask).await.is_err());
    ad.kill().await.unwrap();
}

/// Security review of part 1: a request without `kind` is ACP's `other`, and one whose
/// `rawInput` says more than the command is not allowed by a rule; `deny` applies to both.
#[tokio::test]
async fn only_a_plain_execute_request_is_allowed_by_a_rule() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeAgent::default();
    let perms = crate::permissions::Permissions {
        allow: vec!["Bash(ls:*)".into()],
        ..Default::default()
    };
    let mut s = spec(dir.path(), agent(AcpModes::default()));
    s.environment = Arc::new(crate::config::Environment::default_of(&perms));
    let (tx, mut rx) = mpsc::channel(256);
    let mut ad = adapters(&fake).create(&s, tx);
    ad.start(&s).await.unwrap();
    until(&mut rx, |e| matches!(e, AdapterEvent::PermissionMode(_))).await;
    // One request: answered by the worker (`Some(option)`), or a card (`None`).
    let mut ask = async |tc: Value| -> Option<String> {
        ad.input(&input(&format!("ask {tc}"))).await.unwrap();
        let seen = until(&mut rx, |e| {
            matches!(e, AdapterEvent::TurnEnded { .. })
                || matches!(e, AdapterEvent::Event(ev) if matches!(ev.kind, EventKind::ApprovalRequest { .. }))
        })
        .await;
        if let Some(EventKind::ApprovalRequest { request_id, .. }) =
            kinds(&seen).into_iter().last().cloned()
        {
            ad.answer_approval(&request_id, "no", None).await.unwrap();
            until(&mut rx, |e| matches!(e, AdapterEvent::TurnEnded { .. })).await;
            return None;
        }
        let answer = fake
            .got()
            .into_iter()
            .rev()
            .find(|v| v["id"] == 900)
            .unwrap();
        answer["result"]["outcome"]["optionId"]
            .as_str()
            .map(str::to_owned)
    };
    let allowed = ask(json!({"toolCallId": "a", "title": "ls", "kind": "execute",
        "rawInput": {"command": "ls -la", "description": "list"}}))
    .await;
    assert_eq!(allowed.as_deref(), Some("yes"));
    let no_kind =
        ask(json!({"toolCallId": "b", "title": "ls", "rawInput": {"command": "ls"}})).await;
    assert_eq!(no_kind, None);
    let with_cwd = ask(json!({"toolCallId": "c", "title": "ls", "kind": "execute",
        "rawInput": {"command": "ls", "cwd": "/etc"}}))
    .await;
    assert_eq!(with_cwd, None);
    let denied = ask(
        json!({"toolCallId": "d", "title": "sudo", "kind": "execute",
        "rawInput": {"command": "sudo ls", "env": {"X": "1"}}}),
    )
    .await;
    assert_eq!(denied.as_deref(), Some("no"));
    ad.kill().await.unwrap();
}

/// Security re-review: a mode the agent moved to itself that its table does not name counts as
/// Allow all, and a switch to the mode hqc last set is not skipped after it.
#[tokio::test]
async fn an_unknown_mode_the_agent_moved_to_is_allow_all() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeAgent::default();
    let s = spec(
        dir.path(),
        agent(AcpModes {
            ask: Some("default".into()),
            auto: Some("accept-edits".into()),
            allow_all: None,
            ask_also: vec![],
        }),
    );
    let (tx, mut rx) = mpsc::channel(256);
    let mut ad = adapters(&fake).create(&s, tx);
    ad.start(&s).await.unwrap();
    until(&mut rx, |e| {
        *e == AdapterEvent::PermissionMode(PermissionMode::Ask)
    })
    .await;
    ad.input(&input("selfmode yolo")).await.unwrap();
    let seen = until(&mut rx, |e| matches!(e, AdapterEvent::TurnEnded { .. })).await;
    assert!(seen.contains(&AdapterEvent::PermissionMode(PermissionMode::AllowAll)));
    // Back to Ask: sent again, not skipped.
    let before = fake
        .got()
        .iter()
        .filter(|v| v["method"] == "session/set_mode")
        .count();
    ad.set_permission_mode(PermissionMode::Ask).await.unwrap();
    let after = fake
        .got()
        .iter()
        .filter(|v| v["method"] == "session/set_mode" && v["params"]["modeId"] == "default")
        .count();
    assert!(
        after > 0
            && fake
                .got()
                .iter()
                .filter(|v| v["method"] == "session/set_mode")
                .count()
                > before
    );
    until(&mut rx, |e| {
        *e == AdapterEvent::PermissionMode(PermissionMode::Ask)
    })
    .await;
    ad.kill().await.unwrap();
}
