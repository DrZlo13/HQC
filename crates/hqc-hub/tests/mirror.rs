//! T15: import, mirror and takeover of Claude Code sessions (SPEC §9) with a fake worker:
//! `mirror.set` on import and after `hello`, sessions created by `mirror.event`,
//! `409 locked_vscode` on input, `POST /takeover` after 30 s of silence with `session.start
//! mode: resume`, and the release back to VS Code.

mod common;

use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const ROOT: &str = "/w";
const WS: &str = "/w/proj";

async fn project(hub: &TestHub, name: &str) -> String {
    let (s, p) = hub.post("/api/projects", json!({ "name": name })).await;
    assert_eq!(s, 201, "{p}");
    p["id"].as_str().unwrap().to_string()
}

/// `POST /api/workspaces` with `mirror_enabled`: the worker answers `fs.stat`, then gets
/// `mirror.set`.
async fn import_workspace(hub: &TestHub, w: &mut FakeWorker, pid: &str) -> Value {
    let body = json!({"project_id": pid, "worker_id": w.worker_id, "path": WS,
                      "mirror_enabled": true});
    let (res, set) = tokio::join!(hub.post("/api/workspaces", body), async {
        w.answer_fs().await;
        let set = w.expect("mirror.set").await;
        w.reply(&set["id"], "ack", json!({})).await;
        set
    });
    let (s, ws) = res;
    assert_eq!(s, 201, "{ws}");
    assert_eq!(ws["mirror_enabled"], true);
    assert_eq!(
        set["payload"],
        json!({"workspace_path": WS, "enabled": true})
    );
    ws
}

fn record_event(ty: &str, payload: Value, user: bool) -> Value {
    let mut e = json!({
        "session_id": Uuid::nil(),
        "event_id": Uuid::new_v4(),
        "ts": "2026-10-23T00:10:15.931Z",
        "author": if user { json!({"kind": "user", "id": Uuid::nil()}) }
                  else { json!({"kind": "agent", "id": Uuid::nil()}) },
        "type": ty,
        "payload": payload,
    });
    if user {
        e["delivery"] = json!("delivered");
        e["delivered_at"] = json!("2026-10-23T00:10:15.931Z");
    }
    e
}

/// Sends `mirror.event`; returns the `ack` payload.
async fn mirror_event(
    w: &mut FakeWorker,
    runtime: &str,
    event: Value,
    title: Option<&str>,
    recent: bool,
) -> Value {
    let mut p = json!({"workspace_path": WS, "runtime_session_id": runtime,
                       "cwd": format!("{WS}/sub"), "event": event, "recent": recent});
    if let Some(t) = title {
        p["title"] = json!(t);
    }
    let id = w.send("mirror.event", p).await;
    let ack = w.expect("ack").await;
    assert_eq!(ack["id"], id);
    ack["payload"].clone()
}

async fn sessions(hub: &TestHub, pid: &str) -> Vec<Value> {
    let (s, v) = hub.get(&format!("/api/projects/{pid}/sessions")).await;
    assert_eq!(s, 200, "{v}");
    v["sessions"].as_array().unwrap().clone()
}

/// VS Code session → import → `409 locked_vscode` → takeover refused while VS Code is
/// active, accepted after 30 s quiet → `session.start` resume with the transcript's uuid →
/// input goes through → another writer → release.
#[tokio::test]
async fn takeover_and_release() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online_bare(&hub, &token, ROOT, hub.boot_id).await;
    let pid = project(&hub, "cad").await;

    // Candidates come from the worker.
    let url = format!("/api/workers/{}/candidates", w.worker_id);
    let (res, _) = tokio::join!(hub.get(&url), async {
        let f = w.expect("workspace.list_candidates").await;
        assert_eq!(f["payload"]["request_id"], f["id"]);
        w.reply(
            &f["id"],
            "result",
            json!({"request_id": f["id"], "candidates": [
                {"path": WS, "sessions_count": 1, "last_active": "2026-10-23T00:10:17Z"}]}),
        )
        .await;
    });
    assert_eq!(res.0, 200, "{}", res.1);
    assert_eq!(res.1["candidates"][0]["path"], WS);

    import_workspace(&hub, &mut w, &pid).await;
    let runtime = Uuid::new_v4().to_string();
    let prompt = record_event(
        "user.message",
        json!({"text": "Run echo", "artifact_ids": []}),
        true,
    );
    let ack = mirror_event(&mut w, &runtime, prompt.clone(), None, true).await;
    assert!(ack["seq"].as_u64().is_some(), "{ack}");
    let answer = record_event(
        "agent.message",
        json!({"text": "VSCODE-DONE", "final": true, "in_reply_to": prompt["event_id"]}),
        false,
    );
    mirror_event(&mut w, &runtime, answer, Some("Echo in VS Code"), true).await;
    // The same record again (a resend): the stored seq, nothing new.
    let dup = mirror_event(&mut w, &runtime, prompt.clone(), None, true).await;
    assert_eq!(dup["seq"], ack["seq"]);

    let list = sessions(&hub, &pid).await;
    assert_eq!(list.len(), 1, "{list:?}");
    let s = &list[0];
    let sid = s["id"].as_str().unwrap().to_string();
    assert_eq!(s["locked_by"], "vscode");
    assert_eq!(s["state"], "locked_vscode");
    assert_eq!(s["cwd"], "sub");
    assert_eq!(s["title"], "Echo in VS Code");
    assert_eq!(s["title_source"], "runtime");
    assert_eq!(s["agent"]["runtime_session_id"], runtime.as_str());
    let evs = hub.events(&sid).await;
    let types: Vec<&str> = evs.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(types, ["system", "user.message", "agent.message"]);
    // The hub fills in the authors the worker cannot know.
    assert_eq!(evs[1]["author"]["kind"], "user");
    assert_ne!(evs[1]["author"]["id"], Uuid::nil().to_string());
    assert_eq!(evs[2]["author"], json!({"kind": "agent", "id": sid}));
    assert_eq!(evs[1]["ts"], "2026-10-23T00:10:15.931Z");

    // Read-only while VS Code drives it.
    let (st, e) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "hi", "client_op_id": "a"}),
        )
        .await;
    assert_eq!(
        (st, e["error"].as_str()),
        (409, Some("locked_vscode")),
        "{e}"
    );
    assert_eq!(e["hint"], "takeover");
    let (st, e) = hub
        .post(&format!("/api/sessions/{sid}/takeover"), json!({}))
        .await;
    assert_eq!(
        (st, e["error"].as_str()),
        (409, Some("vscode_active")),
        "{e}"
    );
    w.expect_silence(std::time::Duration::from_millis(200))
        .await;

    // 30 s later.
    sqlx::query(
        "UPDATE sessions SET vscode_active_at = now() - interval '31 seconds' WHERE id = $1::uuid",
    )
    .bind(&sid)
    .execute(&hub.app.db)
    .await
    .unwrap();
    let (st, v) = hub
        .post(&format!("/api/sessions/{sid}/takeover"), json!({}))
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["locked_by"], "hqc");
    let start = ack_start(&mut w, &sid).await;
    let p = &start["payload"];
    assert_eq!(p["mode"], "resume");
    assert_eq!(p["runtime_session_id"], runtime.as_str());
    assert_eq!(
        (p["workspace_path"].as_str(), p["cwd"].as_str()),
        (Some(WS), Some("sub"))
    );
    let evs = hub.events(&sid).await;
    let sys = evs.last().unwrap();
    assert_eq!(sys["payload"]["code"], "session_taken_over");
    assert!(sys["payload"]["text"].as_str().unwrap().contains("VS Code"));
    w.send(
        "session.state",
        json!({"session_id": sid, "process_state": "running", "runtime_session_id": runtime,
               "locked_by": "hqc"}),
    )
    .await;
    w.expect("ack").await;

    // Input goes through now.
    let (st, v) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "next", "client_op_id": "b"}),
        )
        .await;
    assert_eq!(st, 201, "{v}");
    let input = w.expect("session.input").await;
    w.reply(
        &input["id"],
        "ack",
        json!({"event_id": input["payload"]["event_id"]}),
    )
    .await;
    hub.wait_session(&sid, "turn running", |s| s["state"] == "running")
        .await;
    let (st, queued) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "queued", "client_op_id": "c"}),
        )
        .await;
    assert_eq!(st, 201);
    // The worker queues it (SPEC §17.2) and never acks: it stops before the next turn.
    w.expect("session.input").await;

    // VS Code writes again: the worker stops its agent after the turn and reports it.
    w.event(agent_event(
        &sid,
        "agent.message",
        json!({"text": "ok", "final": true, "in_reply_to": input["payload"]["event_id"]}),
    ))
    .await;
    w.send(
        "session.state",
        json!({"session_id": sid, "process_state": "stopped", "runtime_session_id": runtime,
               "locked_by": "vscode"}),
    )
    .await;
    w.expect("ack").await;
    let s = hub
        .wait_session(&sid, "released", |s| s["locked_by"] == "vscode")
        .await;
    assert_eq!(s["state"], "locked_vscode");
    let evs = hub.events(&sid).await;
    let released: Vec<&Value> = evs
        .iter()
        .filter(|e| e["payload"]["code"] == "session_released")
        .collect();
    assert_eq!(released.len(), 1, "{evs:?}");
    let q = evs
        .iter()
        .find(|e| e["event_id"] == queued["event"]["event_id"])
        .unwrap();
    assert_eq!(q["delivery"], "failed", "pending input fails on release");
    // The held VS Code records follow; a second release does not happen.
    let more = record_event(
        "user.message",
        json!({"text": "from vscode", "artifact_ids": []}),
        true,
    );
    mirror_event(&mut w, &runtime, more, None, true).await;
    let evs = hub.events(&sid).await;
    assert_eq!(
        evs.iter()
            .filter(|e| e["payload"]["code"] == "session_released")
            .count(),
        1
    );
    // Nothing is restarted for the failed or new input.
    w.expect_silence(std::time::Duration::from_millis(300))
        .await;

    // A reconnect: mirror.set again for the mirrored workspace.
    w.close().await;
    let (mut w, _) = FakeWorker::online_bare(&hub, &token, ROOT, hub.boot_id).await;
    let set = w.expect("mirror.set").await;
    assert_eq!(set["payload"]["workspace_path"], WS);
    hub.finish().await;
}

/// A transcript nobody wrote for 30 s is imported free (`locked_by: none`): input starts it
/// directly. A mirrored event while hqc runs the session releases it; events outside a
/// mirrored workspace are dropped.
#[tokio::test]
async fn quiet_import_and_live_release() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online_bare(&hub, &token, ROOT, hub.boot_id).await;
    let pid = project(&hub, "cad").await;
    import_workspace(&hub, &mut w, &pid).await;
    let runtime = Uuid::new_v4().to_string();
    let prompt = record_event(
        "user.message",
        json!({"text": "old", "artifact_ids": []}),
        true,
    );
    mirror_event(&mut w, &runtime, prompt, None, false).await;
    let list = sessions(&hub, &pid).await;
    let sid = list[0]["id"].as_str().unwrap().to_string();
    assert_eq!(list[0]["locked_by"], "none");
    assert_eq!(list[0]["title"], "Сессия 1");

    let (st, _) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "go", "client_op_id": "a"}),
        )
        .await;
    assert_eq!(st, 201);
    let start = ack_start(&mut w, &sid).await;
    assert_eq!(start["payload"]["runtime_session_id"], runtime.as_str());
    w.expect("session.input").await;
    w.send(
        "session.state",
        json!({"session_id": sid, "process_state": "running", "locked_by": "hqc"}),
    )
    .await;
    w.expect("ack").await;
    hub.wait_session(&sid, "hqc", |s| s["locked_by"] == "hqc")
        .await;

    // A recent record from elsewhere: VS Code has it now.
    let foreign = record_event(
        "user.message",
        json!({"text": "typed in vscode", "artifact_ids": []}),
        true,
    );
    mirror_event(&mut w, &runtime, foreign, None, true).await;
    let s = hub.session(&sid).await;
    assert_eq!(s["locked_by"], "vscode");
    let evs = hub.events(&sid).await;
    assert!(evs
        .iter()
        .any(|e| e["payload"]["code"] == "session_released"));

    // Outside the workspace, or not mirrored: dropped (ack without seq).
    let mut p = json!({"workspace_path": "/w/other", "runtime_session_id": runtime,
                       "cwd": "/w/other", "recent": false,
                       "event": record_event("agent.message", json!({"text": "x", "final": false}), false)});
    let id = w.send("mirror.event", p.clone()).await;
    let ack = w.expect("ack").await;
    assert_eq!(ack["id"], id);
    assert!(ack["payload"]["seq"].is_null(), "{ack}");
    p["workspace_path"] = json!(WS);
    p["cwd"] = json!("/w/elsewhere");
    w.send("mirror.event", p).await;
    let ack = w.expect("ack").await;
    assert!(ack["payload"]["seq"].is_null(), "{ack}");
    hub.finish().await;
}
