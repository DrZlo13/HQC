//! T4 integration: the supervisor (T3) drives the Claude Code adapter, whose `claude` is a
//! script replaying the T0 `permission-allow` run. A fake hub starts the session, sends the
//! input and answers the approval.

mod common;

use std::sync::Arc;

use common::*;
use hqc_worker::adapter::claude_code::scripted::ScriptedClaude;
use hqc_worker::adapter::claude_code::{ClaudeCodeAdapters, ClaudeOptions};
use serde_json::{json, Value};
use uuid::Uuid;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../hqc-claude-jsonl/fixtures");

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURES}/{name}")).unwrap()
}

#[tokio::test]
async fn hub_runs_a_claude_code_turn_with_an_approval() {
    let env = Env::new();
    let claude_dir = env.dir.path().join("claude-config");
    let stream = fixture("stream-permission-allow-2.1.307.jsonl");
    let stdin = fixture("stdin-permission-allow-2.1.307.jsonl");
    let claude = ScriptedClaude::new(&stream);
    let factory = ClaudeCodeAdapters::new(
        ClaudeOptions {
            config_dir: Some(claude_dir),
            ..ClaudeOptions::default()
        },
        Arc::new(claude.clone()),
    );
    let mut hub = FakeHub::start().await;
    let w = start_worker_factory(&env, env.config(&hub.url()), Arc::new(factory));
    let mut c = hub.accept().await;
    c.handshake().await;

    let sid = Uuid::now_v7();
    let mut start = env.start_payload(sid);
    let claude_session = Uuid::new_v4();
    start["runtime_session_id"] = json!(claude_session);
    start["model"] = json!("claude-haiku-5-5");
    let id = c.frame("session.start", start).await;
    let ack = c.expect("ack").await;
    assert_eq!(ack["id"], id.as_str(), "{ack}");

    let cmd = &claude.commands()[0];
    assert_eq!(cmd.program, std::path::PathBuf::from("claude"));
    assert_eq!(cmd.cwd, env.workspace());
    let pos = cmd.args.iter().position(|a| a == "--session-id").unwrap();
    assert_eq!(cmd.args[pos + 1], claude_session.to_string());

    let prompt: Value = serde_json::from_str(stdin.lines().next().unwrap()).unwrap();
    let input_id = Uuid::now_v7();
    let input_frame = c
        .frame(
            "session.input",
            json!({"session_id": sid, "event_id": input_id,
                   "text": prompt["message"]["content"], "artifact_ids": [], "peer_prefix": []}),
        )
        .await;

    let mut seq = 0;
    let mut types = vec![];
    // The input ack goes straight out, events go through the outbox: their relative order
    // is not fixed, so acks are collected aside.
    let mut deltas = String::new();
    let mut acked = false;
    let mut approval_frame = None;
    let mut approval_acked = false;
    let mut idle = false;
    while !(idle && acked && approval_acked && !deltas.is_empty()) {
        let f = c.recv().await;
        match f["type"].as_str().unwrap() {
            "ack" if f["id"] == input_frame.as_str() => {
                assert_eq!(f["payload"]["event_id"], json!(input_id));
                acked = true;
                continue;
            }
            "ack" if approval_frame.as_deref() == f["id"].as_str() => {
                assert_eq!(f["payload"], json!({}));
                approval_acked = true;
                continue;
            }
            "event" => {}
            _ => {
                c.skipped.push(f);
                continue;
            }
        }
        let ev = &f["payload"]["event"];
        let ty = ev["type"].as_str().unwrap().to_string();
        assert_eq!(ev["session_id"], json!(sid));
        assert_eq!(ev["author"], json!({"kind": "agent", "id": sid}));
        // `agent.delta` is sent directly too and is not stored: no ack, no fixed place.
        if ty == "agent.delta" {
            deltas.push_str(ev["payload"]["text"].as_str().unwrap());
            continue;
        }
        seq += 1;
        c.ack_event(&f, seq).await;
        types.push(ty.clone());
        match ty.as_str() {
            "approval.request" => {
                let p = &ev["payload"];
                assert_eq!(p["tool"], "Bash");
                let once = p["options"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|o| o["canonical"] == "once")
                    .unwrap();
                let rid = p["request_id"].clone();
                approval_frame = Some(
                    c.frame(
                        "approval.response",
                        json!({"session_id": sid, "request_id": rid, "option_id": once["id"]}),
                    )
                    .await,
                );
            }
            "agent.message" => {
                assert_eq!(ev["payload"]["final"], true);
                assert_eq!(ev["payload"]["text"], "DONE");
                assert_eq!(ev["payload"]["in_reply_to"], json!(input_id));
            }
            "status" => idle = true,
            _ => {}
        }
    }
    assert!(acked && approval_acked);
    assert_eq!(deltas, "DONE");
    assert_eq!(
        types,
        [
            "tool.call",
            "approval.request",
            "tool.result",
            "agent.message",
            "usage",
            "status"
        ]
    );
    // Each user message has a fresh `uuid` (SPEC §8.1); the spike wrote none.
    let mut sent: Vec<Value> = claude.stdin();
    for v in sent.iter_mut().filter(|v| v["type"] == "user") {
        let u: Uuid = v["uuid"].as_str().unwrap().parse().unwrap();
        assert_eq!(u.get_version_num(), 4, "{v}");
        v.as_object_mut().unwrap().remove("uuid");
    }
    let want: Vec<Value> = stdin
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(sent, want);

    // A second input goes to the same process (no new spawn).
    let input2 = Uuid::now_v7();
    c.frame(
        "session.input",
        json!({"session_id": sid, "event_id": input2, "text": "again", "artifact_ids": [], "peer_prefix": []}),
    )
    .await;
    // The worker writes it to the CLI's stdin when it gets to it: on a busy runner that is
    // later than any fixed pause.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while claude
        .stdin()
        .last()
        .map(|v| v["message"]["content"].clone())
        != Some(json!("again"))
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:?}",
            claude.stdin()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(claude.commands().len(), 1);
    assert_eq!(
        claude.stdin().last().unwrap()["message"]["content"],
        "again"
    );

    // session.kill: SIGTERM, then `stopped`.
    c.frame("session.kill", json!({"session_id": sid})).await;
    loop {
        let st = c.expect("session.state").await;
        if st["payload"]["process_state"] == "stopped" {
            break;
        }
    }
    assert_eq!(
        claude.signals(),
        [hqc_worker::adapter::claude_code::process::Sig::Terminate]
    );
    w.stop().await;
}

/// SPEC §5.2: a local slash command (`/name x`, a `result` and no echo) is taken at its turn's
/// end; its `ack` reaches the hub before that turn's `status idle`, else the hub would open a
/// turn on it after that turn ended. Several sessions: the connection sends the outbox and its
/// direct frames in an order of its own.
#[tokio::test]
async fn a_local_commands_ack_goes_before_its_status() {
    let env = Env::new();
    let stream = r#"{"type":"result","subtype":"success","is_error":false,"num_turns":0,"duration_ms":15,"result":"Session renamed to: x","total_cost_usd":0,"usage":{"input_tokens":0,"output_tokens":0}}"#;
    let claude = ScriptedClaude::new(stream);
    let factory = ClaudeCodeAdapters::new(
        ClaudeOptions {
            config_dir: Some(env.dir.path().join("claude-config")),
            ..ClaudeOptions::default()
        },
        Arc::new(claude.clone()),
    );
    let mut hub = FakeHub::start().await;
    let w = start_worker_factory(&env, env.config(&hub.url()), Arc::new(factory));
    let mut c = hub.accept().await;
    c.handshake().await;
    let mut seq = 0;
    for round in 0..6 {
        let sid = Uuid::now_v7();
        let id = c.frame("session.start", env.start_payload(sid)).await;
        loop {
            let f = c.recv().await;
            if f["type"] == "ack" && f["id"] == id.as_str() {
                break;
            }
        }
        let input_id = Uuid::now_v7();
        let input = c
            .frame(
                "session.input",
                json!({"session_id": sid, "event_id": input_id, "text": "/name x",
                       "artifact_ids": [], "peer_prefix": []}),
            )
            .await;
        let mut acked = false;
        loop {
            let f = c.recv().await;
            if f["type"] == "ack" && f["id"] == input.as_str() {
                acked = true;
            } else if f["type"] == "event" {
                seq += 1;
                c.ack_event(&f, seq).await;
                if f["payload"]["event"]["type"] == "status" {
                    assert!(acked, "round {round}: the status before the input's ack");
                    break;
                }
            }
        }
    }
    w.stop().await;
}

/// T26: "Reply with text instead". The hub's `approval.response` with the deny option
/// and `message` reaches Claude Code as `control_response` `deny` whose `message` carries the
/// user's text (the model gets it as the tool result, SPEC §10.1).
#[tokio::test]
async fn deny_with_message_reaches_control_response() {
    let env = Env::new();
    let claude = ScriptedClaude::new(&fixture("stream-permission-deny-2.1.307.jsonl"));
    let stdin = fixture("stdin-permission-deny-2.1.307.jsonl");
    let factory = ClaudeCodeAdapters::new(
        ClaudeOptions {
            config_dir: Some(env.dir.path().join("claude-config")),
            ..ClaudeOptions::default()
        },
        Arc::new(claude.clone()),
    );
    let mut hub = FakeHub::start().await;
    let w = start_worker_factory(&env, env.config(&hub.url()), Arc::new(factory));
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    c.frame("session.start", env.start_payload(sid)).await;
    c.expect("ack").await;
    let prompt: Value = serde_json::from_str(stdin.lines().next().unwrap()).unwrap();
    c.frame(
        "session.input",
        json!({"session_id": sid, "event_id": Uuid::now_v7(),
               "text": prompt["message"]["content"], "artifact_ids": [], "peer_prefix": []}),
    )
    .await;

    let text = "Don't create the file, just say DONE";
    let mut seq = 0;
    let answer = loop {
        let f = c.recv().await;
        if f["type"] != "event" || f["payload"]["event"]["type"] == "agent.delta" {
            continue;
        }
        seq += 1;
        c.ack_event(&f, seq).await;
        let ev = &f["payload"]["event"];
        if ev["type"] == "approval.request" {
            let p = &ev["payload"];
            let deny = p["options"]
                .as_array()
                .unwrap()
                .iter()
                .find(|o| o["canonical"] == "deny")
                .unwrap();
            break c
                .frame(
                    "approval.response",
                    json!({"session_id": sid, "request_id": p["request_id"],
                           "option_id": deny["id"], "message": text}),
                )
                .await;
        }
    };
    loop {
        let f = c.recv().await;
        if f["type"] == "ack" && f["id"] == answer.as_str() {
            break;
        }
        if f["type"] == "event" && f["payload"]["event"]["type"] != "agent.delta" {
            seq += 1;
            c.ack_event(&f, seq).await;
        }
    }
    let sent = claude.stdin();
    let resp = &sent[1]["response"]["response"];
    assert_eq!(resp["behavior"], "deny", "{}", sent[1]);
    let want = format!(
        "{}{text}",
        hqc_worker::adapter::claude_code::engine::DENY_WITH_TEXT
    );
    assert_eq!(resp["message"], want.as_str());
    // The turn goes on with the user's text, as after the VS Code tab's text answer.
    assert_eq!(resp["interrupt"], false);
    w.stop().await;
}

/// SPEC §5.4: the CLI's list of background tasks goes to the hub in `session.state`: a command
/// the agent runs in the background (`task_type: local_bash`) makes `background_tasks: 1`, the
/// empty list makes it 0 (absent).
#[tokio::test]
async fn background_tasks_go_in_session_state() {
    let env = Env::new();
    let claude = ScriptedClaude::new(concat!(
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"bq3k1x9wd","task_type":"local_bash"}]}"#,
        "\n",
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[]}"#,
    ));
    let factory = ClaudeCodeAdapters::new(
        ClaudeOptions {
            config_dir: Some(env.dir.path().join("claude-config")),
            ..ClaudeOptions::default()
        },
        Arc::new(claude.clone()),
    );
    let mut hub = FakeHub::start().await;
    let w = start_worker_factory(&env, env.config(&hub.url()), Arc::new(factory));
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    c.frame("session.start", env.start_payload(sid)).await;
    c.expect("ack").await;
    c.frame(
        "session.input",
        json!({"session_id": sid, "event_id": Uuid::now_v7(), "text": "make all",
               "artifact_ids": [], "peer_prefix": []}),
    )
    .await;
    loop {
        let st = c.expect("session.state").await;
        if st["payload"]["background_tasks"] == 1 {
            break;
        }
    }
    let st = c.expect("session.state").await;
    assert_eq!(st["payload"]["process_state"], "running", "{st}");
    assert!(st["payload"].get("background_tasks").is_none(), "{st}");
    w.stop().await;
}

/// SPEC §5.4, §8.1, §17.3: a retried API request goes to the hub in `session.state.activity`,
/// and the response that comes ends it; each beat of a long call (`tool_progress` with
/// `heartbeat: true`) sends `tokens.rate` again, the rate unchanged, which keeps the hub's
/// turn watchdog off a call that prints nothing.
#[tokio::test]
async fn activity_and_heartbeats_reach_the_hub() {
    let env = Env::new();
    let claude = ScriptedClaude::new(concat!(
        r#"{"type":"system","subtype":"api_retry","attempt":3,"max_retries":10,"retry_delay_ms":8000,"error_status":529,"error":"overloaded","uuid":"0f6c1d2e-8888-4a55-9d0e-1a2b3c4d5e01","session_id":"s"}"#,
        "\n",
        r#"{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"message_start","message":{}},"uuid":"0f6c1d2e-8888-4a55-9d0e-1a2b3c4d5e02","session_id":"s"}"#,
        "\n",
        r#"{"type":"tool_progress","tool_use_id":"toolu_01Hb-heartbeat-0","tool_name":"mcp__slow__run","parent_tool_use_id":"toolu_01Hb","elapsed_time_seconds":30,"heartbeat":true,"uuid":"0f6c1d2e-8888-4a55-9d0e-1a2b3c4d5e03","session_id":"s"}"#,
        "\n",
        r#"{"type":"tool_progress","tool_use_id":"toolu_01Hb-heartbeat-1","tool_name":"mcp__slow__run","parent_tool_use_id":"toolu_01Hb","elapsed_time_seconds":60,"heartbeat":true,"uuid":"0f6c1d2e-8888-4a55-9d0e-1a2b3c4d5e04","session_id":"s"}"#,
    ));
    let factory = ClaudeCodeAdapters::new(
        ClaudeOptions {
            config_dir: Some(env.dir.path().join("claude-config")),
            ..ClaudeOptions::default()
        },
        Arc::new(claude.clone()),
    );
    let mut hub = FakeHub::start().await;
    let w = start_worker_factory(&env, env.config(&hub.url()), Arc::new(factory));
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    c.frame("session.start", env.start_payload(sid)).await;
    c.expect("ack").await;
    c.frame(
        "session.input",
        json!({"session_id": sid, "event_id": Uuid::now_v7(), "text": "run the slow tool",
               "artifact_ids": [], "peer_prefix": []}),
    )
    .await;
    // The states from the first with an activity on, and the rates sent.
    let mut states: Vec<Option<Value>> = vec![];
    let mut rates = vec![];
    while states.len() < 2 || rates.len() < 2 {
        let f = c.recv().await;
        if f["type"] == "session.state" {
            let a = f["payload"].get("activity").cloned();
            if a.is_some() || !states.is_empty() {
                states.push(a);
            }
        } else if f["type"] == "event" && f["payload"]["event"]["type"] == "tokens.rate" {
            rates.push(f["payload"]["event"]["payload"]["output_per_s"].clone());
        }
    }
    assert_eq!(
        states,
        [
            Some(json!({"kind": "retrying", "attempt": 3, "max_retries": 10,
                "retry_delay_ms": 8000, "error_status": 529})),
            None
        ]
    );
    assert_eq!(rates, [json!(0), json!(0)]);
    w.stop().await;
}

/// SPEC §4, §8.1: a model switch the CLI refuses (`error` with the `model` it kept) reaches the
/// hub as `system model_not_switched` with that model, and the worker keeps it as the
/// session's: a `session.model` naming it switches nothing, another model is asked for.
#[tokio::test]
async fn a_refused_model_switch_keeps_the_model() {
    let env = Env::new();
    let claude = ScriptedClaude::new(
        r#"{"type":"control_response","response":{"subtype":"error","request_id":"hqc-model-0199f3a2-7c10-7000-8000-000000000001","error":"Model claude-nope is not available","error_code":"not_found","model":"claude-opus-5-5"}}"#,
    );
    let factory = ClaudeCodeAdapters::new(
        ClaudeOptions {
            config_dir: Some(env.dir.path().join("claude-config")),
            ..ClaudeOptions::default()
        },
        Arc::new(claude.clone()),
    );
    let mut hub = FakeHub::start().await;
    let w = start_worker_factory(&env, env.config(&hub.url()), Arc::new(factory));
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    c.frame("session.start", env.start_payload(sid)).await;
    c.expect("ack").await;
    c.frame(
        "session.input",
        json!({"session_id": sid, "event_id": Uuid::now_v7(), "text": "hi",
               "artifact_ids": [], "peer_prefix": []}),
    )
    .await;
    let mut seq = 0;
    loop {
        let f = c.expect("event").await;
        seq += 1;
        c.ack_event(&f, seq).await;
        let p = &f["payload"]["event"]["payload"];
        if p["code"] == "model_not_switched" {
            assert_eq!(p["model"], "claude-opus-5-5", "{f}");
            break;
        }
    }
    for model in ["claude-opus-5-5", "claude-sonnet-5"] {
        let id = c
            .frame("session.model", json!({"session_id": sid, "model": model}))
            .await;
        loop {
            let f = c.expect("ack").await;
            if f["id"] == id.as_str() {
                break;
            }
        }
    }
    let switches = || -> Vec<Value> {
        claude
            .stdin()
            .into_iter()
            .filter(|v| v["request"]["subtype"] == "set_model")
            .map(|v| v["request"]["model"].clone())
            .collect()
    };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while switches().is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:?}",
            claude.stdin()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(switches(), [json!("claude-sonnet-5")]);
    w.stop().await;
}

/// SPEC §5.3, §5.4: a Bash call the CLI runs in the foreground goes to the hub in
/// `session.state.foreground_calls`; `session.background` with its id is acked and reaches the
/// CLI as `control_request {subtype: background_tasks, tool_use_id}`.
#[tokio::test]
async fn a_foreground_call_is_moved_to_the_background() {
    let env = Env::new();
    let call = "toolu_01FoCFY4DmUbVxXAembc44PX";
    let claude = ScriptedClaude::new(&format!(
        r#"{{"type":"system","subtype":"task_started","task_id":"bfis28nrx","tool_use_id":"{call}","description":"Sleep for 15 seconds","is_backgrounded":false,"task_type":"local_bash"}}"#
    ));
    let factory = ClaudeCodeAdapters::new(
        ClaudeOptions {
            config_dir: Some(env.dir.path().join("claude-config")),
            ..ClaudeOptions::default()
        },
        Arc::new(claude.clone()),
    );
    let mut hub = FakeHub::start().await;
    let w = start_worker_factory(&env, env.config(&hub.url()), Arc::new(factory));
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    c.frame("session.start", env.start_payload(sid)).await;
    c.expect("ack").await;
    c.frame(
        "session.input",
        json!({"session_id": sid, "event_id": Uuid::now_v7(), "text": "sleep 15",
               "artifact_ids": [], "peer_prefix": []}),
    )
    .await;
    loop {
        let st = c.expect("session.state").await;
        if st["payload"]["foreground_calls"] == json!([call]) {
            break;
        }
    }
    let id = c
        .frame(
            "session.background",
            json!({"session_id": sid, "call_id": call}),
        )
        .await;
    loop {
        let f = c.expect("ack").await;
        if f["id"] == id.as_str() {
            break;
        }
    }
    // Written before the ack; the script records it on its own task.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let req = loop {
        if let Some(r) = claude
            .stdin()
            .into_iter()
            .find(|v| v["type"] == "control_request")
        {
            break r;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:?}",
            claude.stdin()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert_eq!(
        req["request"],
        json!({"subtype": "background_tasks", "tool_use_id": call})
    );
    assert!(req["request_id"]
        .as_str()
        .unwrap()
        .starts_with("hqc-background-"));
    w.stop().await;
}

/// The `SSH_AUTH_SOCK` and `HQC_SECRETS` of the latest process the scripted CLI was started as.
fn process_env(claude: &ScriptedClaude) -> (String, String) {
    let cmd = claude.commands().last().unwrap().clone();
    let get = |k: &str| {
        cmd.env
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| {
                panic!(
                    "{k} not in {:?}",
                    cmd.env.iter().map(|(k, _)| k).collect::<Vec<_>>()
                )
            })
    };
    (get("SSH_AUTH_SOCK"), get("HQC_SECRETS"))
}

/// The keys an ssh-agent holds, as `ssh-add -l` lists them.
fn agent_keys(socket: &str) -> String {
    let out = std::process::Command::new("ssh-add")
        .arg("-l")
        .env_clear()
        .env("SSH_AUTH_SOCK", socket)
        .env("PATH", std::env::var_os("PATH").unwrap())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A fresh ed25519 key with `passphrase` (empty: none), as a user pastes it.
fn keygen(dir: &std::path::Path, name: &str, passphrase: &str) -> String {
    let path = dir.join(name);
    let ok = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", passphrase, "-C", name, "-f"])
        .arg(&path)
        .status()
        .unwrap()
        .success();
    assert!(ok);
    std::fs::read_to_string(path).unwrap()
}

/// SPEC 0.147 (T57): with `session_secrets` the agent's process gets `HQC_SECRETS`, an
/// ssh-agent of its own (empty at first) and the secret tools; `secret.put` writes a secret,
/// loads a key, keeps one for the workspace that the next process loads, makes one; refusals
/// never carry the value.
#[tokio::test]
async fn secrets_and_keys_reach_the_running_agent() {
    let env = Env::new();
    // Under /tmp: ssh-agent sockets must stay short.
    let home = tempfile::Builder::new()
        .prefix("hqc-t57-")
        .tempdir_in("/tmp")
        .unwrap();
    let home_path = home.path().canonicalize().unwrap();
    let claude = ScriptedClaude::new(&fixture("stream-permission-allow-2.1.307.jsonl"));
    let factory = ClaudeCodeAdapters::new(
        ClaudeOptions {
            config_dir: Some(env.dir.path().join("claude-config")),
            ssh: hqc_worker::ssh::Store::in_home(&home_path),
            mcp: Some(hqc_worker::adapter::claude_code::McpLaunch {
                program: "/bin/hqc-worker".into(),
                socket: env.mcp_socket(),
            }),
            ..ClaudeOptions::default()
        },
        Arc::new(claude.clone()),
    );
    let mut config = hqc_worker::config::Config::parse(&format!(
        "{}session_secrets = true\n",
        env.config_toml("http://127.0.0.1:1")
    ))
    .unwrap();
    let mut hub = FakeHub::start().await;
    config.hub_url = hub.url();
    config.secrets = hqc_worker::secrets::Stores::in_home(&home_path);
    let stores = config.secrets.clone();
    let w = start_worker_factory(&env, config, Arc::new(factory));
    let mut c = hub.accept().await;
    let hello = c.handshake().await;
    assert_eq!(hello["payload"]["session_secrets"], true);

    let sid = Uuid::now_v7();
    c.frame("session.start", env.start_payload(sid)).await;
    c.expect("ack").await;
    let (socket, dir) = process_env(&claude);
    assert!(
        dir.starts_with(&home_path.join("secrets").display().to_string()),
        "{dir}"
    );
    assert!(agent_keys(&socket).contains("no identities") || agent_keys(&socket).is_empty());
    let cmd = claude.commands()[0].clone();
    let mcp = &cmd.args[cmd.args.iter().position(|a| a == "--mcp-config").unwrap() + 1];
    assert!(mcp.contains("\"HQC_SECRET_TOOLS\":\"1\""), "{mcp}");

    let put = |kind: &str, name: &str, extra: Value| {
        let mut p = json!({"request_id": "", "session_id": sid, "kind": kind, "name": name});
        p.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        p
    };
    // Sent with its frame id as request_id; the answer comes under the same id.
    async fn ask(c: &mut HubConn, mut payload: Value) -> Value {
        let id = format!("h-{}", Uuid::new_v4().simple());
        payload["request_id"] = json!(id);
        c.send(json!({"id": id, "type": "secret.put", "payload": payload}))
            .await;
        loop {
            let f = c.recv().await;
            if f["id"] == id.as_str() {
                return f;
            }
        }
    }

    // A secret: a file of the process, the agent told the path only.
    let r = ask(
        &mut c,
        put("secret", "WIFI_PASS", json!({"value": "hunter2-wifi"})),
    )
    .await;
    assert_eq!(r["type"], "result", "{r}");
    let s = &r["payload"]["secret"];
    assert_eq!(s["path"], "$HQC_SECRETS/WIFI_PASS");
    assert!(!r.to_string().contains("hunter2"), "{r}");
    assert_eq!(
        std::fs::read_to_string(format!("{dir}/WIFI_PASS")).unwrap(),
        "hunter2-wifi"
    );

    // A key with a passphrase: wrong, then right, and kept for the workspace.
    let keys = tempfile::tempdir().unwrap();
    let pi = keygen(keys.path(), "pi-lab", "pass");
    let r = ask(
        &mut c,
        put(
            "ssh_key",
            "pi-lab",
            json!({"value": pi, "passphrase": "nope", "remember": true}),
        ),
    )
    .await;
    assert_eq!(r["type"], "error", "{r}");
    let msg = r["payload"]["message"].as_str().unwrap();
    assert!(
        msg.contains("passphrase") && !msg.contains("nope") && !msg.contains("OPENSSH"),
        "{msg}"
    );
    let r = ask(
        &mut c,
        put(
            "ssh_key",
            "pi-lab",
            json!({"value": pi, "passphrase": "pass", "remember": true}),
        ),
    )
    .await;
    assert_eq!(r["type"], "result", "{r}");
    let fp = r["payload"]["secret"]["fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(r["payload"]["secret"]["remembered"].as_bool().unwrap());
    assert!(agent_keys(&socket).contains(&fp), "loaded now");
    // Made by the worker: only the public half comes back.
    let r = ask(&mut c, put("ssh_keygen", "pi-kitchen", json!({}))).await;
    let made = &r["payload"]["secret"];
    assert!(
        made["public_key"]
            .as_str()
            .unwrap()
            .starts_with("ssh-ed25519 "),
        "{r}"
    );
    assert!(!r.to_string().contains("PRIVATE"), "{r}");
    let made_fp = made["fingerprint"].as_str().unwrap().to_string();
    assert!(agent_keys(&socket).contains(&made_fp));
    // The same name again is refused, and so is a remembered plain secret and a bad name.
    for p in [
        put("ssh_keygen", "pi-kitchen", json!({})),
        put("secret", "X", json!({"value": "v", "remember": true})),
        put("secret", "../x", json!({"value": "v"})),
        put("secret", "EMPTY", json!({"value": ""})),
    ] {
        let r = ask(&mut c, p.clone()).await;
        assert_eq!(r["type"], "error", "{p} -> {r}");
    }

    // The workspace keeps the two keys; the next process loads them, the secret is gone.
    let kept = stores.workspace(&env.workspace()).list().unwrap();
    assert_eq!(
        kept.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(),
        ["pi-kitchen", "pi-lab"]
    );
    c.frame("session.kill", json!({"session_id": sid})).await;
    c.expect("ack").await;
    assert!(
        !std::path::Path::new(&dir).exists(),
        "the secrets went with the process"
    );
    c.frame("session.start", env.start_payload(sid)).await;
    c.expect("ack").await;
    let (socket2, dir2) = process_env(&claude);
    assert_ne!(dir2, dir);
    let listed = agent_keys(&socket2);
    assert!(
        listed.contains(&fp) && listed.contains(&made_fp),
        "{listed}"
    );
    assert!(!std::path::Path::new(&dir2).join("WIFI_PASS").exists());

    // The keys of the workspace, and one forgotten.
    let path = env.workspace().display().to_string();
    c.send(json!({"id": "k-1", "type": "workspace.keys", "payload": {"request_id": "k-1", "path": path}}))
        .await;
    let r = loop {
        let f = c.recv().await;
        if f["id"] == "k-1" {
            break f;
        }
    };
    assert_eq!(r["payload"]["keys"].as_array().unwrap().len(), 2, "{r}");
    c.send(json!({"id": "k-2", "type": "workspace.key_delete",
                  "payload": {"request_id": "k-2", "path": path, "name": "pi-lab"}}))
        .await;
    let r = loop {
        let f = c.recv().await;
        if f["id"] == "k-2" {
            break f;
        }
    };
    assert_eq!(r["payload"]["key_deleted"], true, "{r}");
    assert_eq!(stores.workspace(&env.workspace()).list().unwrap().len(), 1);
    w.stop().await;
}

/// SPEC 0.147: without `session_secrets` (the default) the process gets none of it and
/// `secret.put` is refused.
#[tokio::test]
async fn without_session_secrets_nothing_is_given() {
    let env = Env::new();
    let claude = ScriptedClaude::new(&fixture("stream-permission-allow-2.1.307.jsonl"));
    let factory = ClaudeCodeAdapters::new(
        ClaudeOptions {
            config_dir: Some(env.dir.path().join("claude-config")),
            ..ClaudeOptions::default()
        },
        Arc::new(claude.clone()),
    );
    let mut hub = FakeHub::start().await;
    let w = start_worker_factory(&env, env.config(&hub.url()), Arc::new(factory));
    let mut c = hub.accept().await;
    let hello = c.handshake().await;
    assert!(hello["payload"].get("session_secrets").is_none(), "{hello}");
    let sid = Uuid::now_v7();
    c.frame("session.start", env.start_payload(sid)).await;
    c.expect("ack").await;
    let cmd = claude.commands()[0].clone();
    assert!(!cmd
        .env
        .iter()
        .any(|(k, _)| k == "HQC_SECRETS" || k == "SSH_AUTH_SOCK"));
    c.send(
        json!({"id": "s-1", "type": "secret.put", "payload": {"request_id": "s-1",
                  "session_id": sid, "kind": "secret", "name": "A", "value": "topsecret"}}),
    )
    .await;
    let r = loop {
        let f = c.recv().await;
        if f["id"] == "s-1" {
            break f;
        }
    };
    assert_eq!(r["type"], "error", "{r}");
    assert!(!r.to_string().contains("topsecret"));
    w.stop().await;
}

/// SPEC §4, §5.3: a background subagent goes to the hub in `subagents.live` as `stoppable`;
/// `session.stop_task` with its `task_id` is acked and reaches the CLI as `control_request
/// {subtype: stop_task, task_id}`.
#[tokio::test]
async fn a_subagent_is_stopped_alone() {
    let env = Env::new();
    let task = "a52af2b436ded4cea";
    let claude = ScriptedClaude::new(&format!(
        r#"{{"type":"system","subtype":"task_started","task_id":"{task}","tool_use_id":"toolu_01B2anRdzK42h65tA2gdwhb2","description":"Sleep and reply with number","subagent_type":"general-purpose","is_backgrounded":true,"spawn_depth":1,"task_type":"local_agent"}}"#
    ));
    let factory = ClaudeCodeAdapters::new(
        ClaudeOptions {
            config_dir: Some(env.dir.path().join("claude-config")),
            ..ClaudeOptions::default()
        },
        Arc::new(claude.clone()),
    );
    let mut hub = FakeHub::start().await;
    let w = start_worker_factory(&env, env.config(&hub.url()), Arc::new(factory));
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    c.frame("session.start", env.start_payload(sid)).await;
    c.expect("ack").await;
    c.frame(
        "session.input",
        json!({"session_id": sid, "event_id": Uuid::now_v7(), "text": "sleep in a subagent",
               "artifact_ids": [], "peer_prefix": []}),
    )
    .await;
    let agents = loop {
        let f = c.expect("event").await;
        if f["payload"]["event"]["type"] == "subagents.live" {
            break f["payload"]["event"]["payload"]["agents"].clone();
        }
    };
    assert_eq!(agents[0]["task_id"], task, "{agents}");
    assert_eq!(agents[0]["stoppable"], true, "{agents}");
    let id = c
        .frame(
            "session.stop_task",
            json!({"session_id": sid, "task_id": task}),
        )
        .await;
    loop {
        let f = c.expect("ack").await;
        if f["id"] == id.as_str() {
            break;
        }
    }
    // Written before the ack; the script records it on its own task.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let req = loop {
        if let Some(r) = claude
            .stdin()
            .into_iter()
            .find(|v| v["type"] == "control_request")
        {
            break r;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:?}",
            claude.stdin()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert_eq!(
        req["request"],
        json!({"subtype": "stop_task", "task_id": task})
    );
    assert!(req["request_id"]
        .as_str()
        .unwrap()
        .starts_with("hqc-stop-task-"));
    w.stop().await;
}
