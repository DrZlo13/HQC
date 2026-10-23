//! T15 (SPEC §9): the mirror inside the worker, against a fake hub and the fake adapter.
//! The transcript is the `adapter-then-outside` fixture (2.1.308) moved into the test's
//! workspace: an adapter session continued by `claude -p --resume` from outside.

mod common;

use std::time::Duration;

use common::*;
use hqc_claude_jsonl::{parse_str, Record};
use hqc_worker::adapter::fake::{FakeAdapters, FakeCall};
use serde_json::{json, Value};
use uuid::Uuid;

const TRANSCRIPT: &str =
    include_str!("../../hqc-claude-jsonl/fixtures/adapter-then-outside-2.1.308.jsonl");
const STREAM: &str =
    include_str!("../../hqc-claude-jsonl/fixtures/stream-adapter-then-outside-2.1.308.jsonl");
const RUNTIME: &str = "0ac7a6fd-7974-49ec-8a6c-8c8d141d6630";

fn write_transcript(env: &Env) -> std::path::PathBuf {
    let (projects, _) = env.mirror.clone().unwrap();
    let folder = projects.join("-some-encoded-name");
    std::fs::create_dir_all(&folder).unwrap();
    let file = folder.join(format!("{RUNTIME}.jsonl"));
    let ws = env.workspace().display().to_string();
    std::fs::write(
        &file,
        TRANSCRIPT.replace("/tmp/hqc-fixture/adapter-then-outside", &ws),
    )
    .unwrap();
    file
}

fn stream_uuids() -> Vec<Uuid> {
    parse_str(STREAM)
        .0
        .iter()
        .filter_map(|r| match r {
            Record::User(m) | Record::Assistant(m) => Some(m.uuid),
            _ => None,
        })
        .collect()
}

async fn mirror_set(c: &mut HubConn, path: &str, enabled: bool) -> Value {
    let id = c
        .frame(
            "mirror.set",
            json!({"workspace_path": path, "enabled": enabled}),
        )
        .await;
    loop {
        let f = c.recv().await;
        if f["id"] == id.as_str() {
            return f;
        }
        c.skipped.push(f);
    }
}

/// While the worker's agent runs the session, the outside turn is held; the agent is
/// stopped after its turn, `session.state` says `locked_by: vscode`, the held records go
/// out as `mirror.event`, and further input is refused.
#[tokio::test]
async fn foreign_writer_releases_the_session() {
    let env = Env::with_mirror();
    let (_, own) = env.mirror.clone().unwrap();
    write_transcript(&env);
    let runtime: Uuid = RUNTIME.parse().unwrap();
    // What the Claude Code adapter would record while it runs the session.
    own.begin(runtime);
    for u in stream_uuids() {
        own.add(runtime, u);
    }

    let mut hub = FakeHub::start().await;
    let adapters = FakeAdapters::new();
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;

    let sid = Uuid::now_v7();
    let mut start = env.start_payload(sid);
    start["runtime_session_id"] = json!(RUNTIME);
    let id = c.frame("session.start", start).await;
    assert_eq!(c.expect("ack").await["id"], id.as_str());
    let eid = Uuid::now_v7();
    c.frame(
        "session.input",
        json!({"session_id": sid, "event_id": eid, "text": "go", "artifact_ids": [], "peer_prefix": []}),
    )
    .await;
    adapters
        .wait_for("input", |c| {
            c.iter().any(|x| matches!(x, FakeCall::Input { .. }))
        })
        .await;
    adapters.accept_input(sid, eid).await;

    // Outside allowed_roots: refused.
    let r = mirror_set(&mut c, "/", true).await;
    assert_eq!(r["type"], "error", "{r}");
    let ws = env.workspace().display().to_string();
    let r = mirror_set(&mut c, &ws, true).await;
    assert_eq!(r["type"], "ack", "{r}");

    // Held during the turn: no mirror.event, no kill, even after the grace period.
    assert!(c
        .maybe("mirror.event", Duration::from_millis(800))
        .await
        .is_none());
    assert!(!adapters.calls().contains(&FakeCall::Kill(sid)));
    adapters.end_turn(sid).await;
    adapters
        .wait_for("kill after the turn", |c| c.contains(&FakeCall::Kill(sid)))
        .await;
    loop {
        let st = c.expect("session.state").await;
        if st["payload"]["process_state"] == "stopped" {
            assert_eq!(st["payload"]["locked_by"], "vscode", "{st}");
            break;
        }
    }
    // The fake adapter does not end the registry entry: what the real one does on kill.
    own.end(runtime);

    let mut texts = vec![];
    for _ in 0..2 {
        let f = c.expect("mirror.event").await;
        let p = &f["payload"];
        assert_eq!(p["runtime_session_id"], RUNTIME);
        assert_eq!(p["workspace_path"], ws.as_str());
        assert_eq!(p["cwd"], ws.as_str());
        assert_eq!(p["event"]["session_id"], Uuid::nil().to_string());
        texts.push(p["event"]["payload"]["text"].as_str().unwrap().to_owned());
        c.send(json!({"id": f["id"], "type": "ack",
            "payload": {"event_id": p["event"]["event_id"], "seq": 1}}))
            .await;
    }
    assert_eq!(texts, ["Reply with exactly: OUTSIDE-TURN", "OUTSIDE-TURN"]);

    // Input is refused until a takeover (`session.start`).
    let in_id = c
        .frame(
            "session.input",
            json!({"session_id": sid, "event_id": Uuid::now_v7(), "text": "again", "artifact_ids": [], "peer_prefix": []}),
        )
        .await;
    let e = loop {
        let f = c.expect("error").await;
        if f["id"] == in_id.as_str() {
            break f;
        }
    };
    assert_eq!(e["payload"]["code"], "locked_vscode");
    assert_eq!(e["payload"]["retryable"], false);

    // Candidates: the workspace folder, one session.
    let cid = c
        .frame("workspace.list_candidates", json!({"request_id": "x"}))
        .await;
    let r = loop {
        let f = c.expect("result").await;
        if f["id"] == cid.as_str() {
            break f;
        }
    };
    assert_eq!(r["payload"]["candidates"][0]["path"], ws.as_str(), "{r}");
    assert_eq!(r["payload"]["candidates"][0]["sessions_count"], 1);
    w.stop().await;
}

/// The first `mirror.set` imports the whole history of a quiet transcript: every content
/// block of the main chain, with `recent` false since the file is older than 30 s.
#[tokio::test]
async fn mirror_set_imports_the_history() {
    let env = Env::with_mirror();
    let file = write_transcript(&env);
    let old = std::time::SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let mut hub = FakeHub::start().await;
    let adapters = FakeAdapters::new();
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let ws = env.workspace().display().to_string();
    assert_eq!(mirror_set(&mut c, &ws, true).await["type"], "ack");
    let mut types = vec![];
    for _ in 0..6 {
        let f = c.expect("mirror.event").await;
        assert_eq!(f["payload"]["recent"], false);
        types.push(f["payload"]["event"]["type"].as_str().unwrap().to_owned());
        c.send(json!({"id": f["id"], "type": "ack",
            "payload": {"event_id": f["payload"]["event"]["event_id"], "seq": 1}}))
            .await;
    }
    assert_eq!(
        types,
        [
            "user.message",
            "tool.call",
            "tool.result",
            "agent.message",
            "user.message",
            "agent.message"
        ]
    );
    // The empty thinking of the outside turn gives nothing.
    assert!(c
        .maybe("mirror.event", Duration::from_millis(400))
        .await
        .is_none());
    w.stop().await;
}
