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
    let sent: Vec<Value> = claude.stdin();
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
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
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
