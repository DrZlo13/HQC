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

/// While the worker's agent runs the session, the outside turn is held for the grace
/// period; then the agent is stopped at once, mid-turn (SPEC §9.3, 0.30), `session.state`
/// says `locked_by: vscode`, the held records go out as `mirror.event`, and further input
/// is refused.
#[tokio::test]
async fn foreign_writer_releases_the_session() {
    let env = Env::with_mirror();
    let (_, own) = env.mirror.clone().unwrap();
    write_transcript(&env);
    let runtime: Uuid = RUNTIME.parse().unwrap();
    // What the Claude Code adapter would record while it runs the session.
    own.begin(runtime, None);
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

    // The turn never ends: after the grace period the agent is killed in the middle of it.
    adapters
        .wait_for("kill during the turn", |c| c.contains(&FakeCall::Kill(sid)))
        .await;
    loop {
        let st = c.expect("session.state").await;
        if st["payload"]["process_state"] == "stopped" {
            assert_eq!(st["payload"]["locked_by"], "vscode", "{st}");
            break;
        }
    }
    // The fake adapter does not end the registry entry: what the real one does on kill.
    own.end(runtime, None);

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

/// Merge of T21 and T22: the hub refuses the records of a new transcript with a retryable
/// `mirror_quota` (its daily limit of imported sessions). They leave the outbox, the
/// transcript is not sent again before `quota_retry` (800 ms here, an hour in production),
/// and then it is sent whole, with the same event ids.
#[tokio::test]
async fn mirror_quota_sends_the_transcript_again_later() {
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
    let mut first = vec![];
    for _ in 0..6 {
        let f = c.expect("mirror.event").await;
        first.push(event_id(&f));
        c.send(json!({"id": f["id"], "type": "error", "payload": {
            "code": "mirror_quota", "message": "daily limit", "retryable": true}}))
            .await;
    }
    let refused = std::time::Instant::now();
    assert!(c
        .maybe("mirror.event", Duration::from_millis(400))
        .await
        .is_none());
    let mut again = vec![];
    for _ in 0..6 {
        let f = c.expect("mirror.event").await;
        again.push(event_id(&f));
        c.send(json!({"id": f["id"], "type": "ack",
            "payload": {"event_id": f["payload"]["event"]["event_id"], "seq": 1}}))
            .await;
    }
    assert!(refused.elapsed() >= Duration::from_millis(700));
    assert_eq!(again, first);
    assert!(c
        .maybe("mirror.event", Duration::from_millis(400))
        .await
        .is_none());
    w.stop().await;
}

/// A Claude Code registry entry `<config>/sessions/<pid>.json` for `RUNTIME`, held by a live
/// process that is not the worker's (the test process itself).
fn write_holder(env: &Env, status: &str) -> std::path::PathBuf {
    let (projects, _) = env.mirror.clone().unwrap();
    let dir = projects.with_file_name("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let pid = std::process::id();
    let file = dir.join(format!("{pid}.json"));
    std::fs::write(
        &file,
        json!({"pid": pid, "sessionId": RUNTIME, "cwd": env.workspace(), "entrypoint": "claude-vscode",
            "kind": "interactive", "status": status, "updatedAt": 1792779848554u64})
        .to_string(),
    )
    .unwrap();
    file
}

/// The next holder report (`session.state` with the nil session id) for `RUNTIME`, acked:
/// it comes through the outbox.
async fn holder_report(c: &mut HubConn) -> Value {
    loop {
        let f = c.expect("session.state").await;
        if f["payload"]["session_id"] == Uuid::nil().to_string() {
            assert_eq!(f["payload"]["runtime_session_id"], RUNTIME, "{f}");
            c.send(json!({"id": f["id"], "type": "ack", "payload": {}}))
                .await;
            return f["payload"]["held_by"].clone();
        }
    }
}

/// T20 (SPEC §9.3): VS Code opens the transcript the worker's agent runs. The worker reports
/// the holder, stops the agent at once, mid-turn, and says `locked_by: vscode`; the holder is
/// reported again after a reconnect and as `null` once the process has gone. Mirroring is
/// not needed for any of it.
#[tokio::test]
async fn holder_releases_the_session() {
    let env = Env::with_mirror();
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

    let entry = write_holder(&env, "idle");
    assert_eq!(
        holder_report(&mut c).await,
        json!({"entrypoint": "claude-vscode", "status": "idle"})
    );
    // The turn does not end: VS Code continues from what it read, so the agent stops now
    // and the session is VS Code's.
    adapters
        .wait_for("kill during the turn", |c| c.contains(&FakeCall::Kill(sid)))
        .await;
    loop {
        let st = c.expect("session.state").await;
        if st["payload"]["session_id"] == sid.to_string()
            && st["payload"]["process_state"] == "stopped"
        {
            assert_eq!(st["payload"]["locked_by"], "vscode", "{st}");
            assert!(st["payload"].get("held_by").is_none(), "{st}");
            break;
        }
    }

    // A new status is a change; a reconnect repeats the holder.
    write_holder(&env, "busy");
    assert_eq!(holder_report(&mut c).await["status"], "busy");
    c.close().await;
    let mut c = hub.accept().await;
    c.handshake().await;
    assert_eq!(holder_report(&mut c).await["status"], "busy");

    // VS Code closed the session.
    std::fs::remove_file(entry).unwrap();
    assert_eq!(holder_report(&mut c).await, Value::Null);
    w.stop().await;
}
