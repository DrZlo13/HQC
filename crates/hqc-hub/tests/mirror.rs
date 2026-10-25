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
    w.see_mirror_set = true;
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

    // VS Code writes again: the worker stops its agent at once and reports it.
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
    w.see_mirror_set = true;
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
    w.see_mirror_set = true;
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
    assert_eq!(list[0]["title"], "Session 1");

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

const VSCODE_IDLE: &str = r#"{"entrypoint": "claude-vscode", "status": "idle"}"#;

fn vscode_idle() -> Value {
    serde_json::from_str(VSCODE_IDLE).unwrap()
}

/// A holder report: `session.state` with the nil session id (SPEC §5.4, §9.3).
async fn holder_report(w: &mut FakeWorker, runtime: &str, held_by: Value) {
    let id = w
        .send(
            "session.state",
            json!({"session_id": Uuid::nil(), "process_state": "stopped",
                   "runtime_session_id": runtime, "locked_by": "none", "held_by": held_by}),
        )
        .await;
    let ack = w.expect("ack").await;
    assert_eq!(ack["id"], id);
}

/// The worker reads Claude Code's registry (`hello.capabilities`, SPEC §9.3).
async fn with_registry(hub: &TestHub, w: &FakeWorker) {
    sqlx::query(
        "UPDATE workers SET capabilities = jsonb_set(capabilities, '{claude_code,sessions_registry}', 'true')
         WHERE id = $1::uuid",
    )
    .bind(&w.worker_id)
    .execute(&hub.app.db)
    .await
    .unwrap();
}

/// T20: a session open in VS Code is `locked_vscode` even with no recent write, takeover is
/// `409 vscode_open` while it is held, and allowed at once (no 30 s) when it is closed.
#[tokio::test]
async fn holder_blocks_takeover_until_closed() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online_bare(&hub, &token, ROOT, hub.boot_id).await;
    w.see_mirror_set = true;
    with_registry(&hub, &w).await;
    let pid = project(&hub, "cad").await;
    import_workspace(&hub, &mut w, &pid).await;
    let runtime = Uuid::new_v4().to_string();

    // An old record of a session that is open, idle, in VS Code.
    let mut p = json!({"workspace_path": WS, "runtime_session_id": runtime, "cwd": WS,
        "recent": false, "held_by": vscode_idle(),
        "event": record_event("user.message", json!({"text": "old", "artifact_ids": []}), true)});
    w.send("mirror.event", p.clone()).await;
    w.expect("ack").await;
    let s = &sessions(&hub, &pid).await[0];
    let sid = s["id"].as_str().unwrap().to_string();
    assert_eq!(s["locked_by"], "vscode", "{s}");
    assert_eq!(s["state"], "locked_vscode");
    assert_eq!(s["held_by"], vscode_idle());
    let at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT vscode_held_at FROM sessions WHERE id = $1::uuid")
            .bind(&sid)
            .fetch_one(&hub.app.db)
            .await
            .unwrap();
    assert!(at.is_some());

    let (st, e) = hub
        .post(&format!("/api/sessions/{sid}/takeover"), json!({}))
        .await;
    assert_eq!((st, e["error"].as_str()), (409, Some("vscode_open")), "{e}");
    assert_eq!(
        e["message"],
        "the session is open in VS Code: close it there and take it over again"
    );
    assert_eq!(e["held_by"], vscode_idle());
    let (st, e) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "hi", "client_op_id": "a"}),
        )
        .await;
    assert_eq!((st, e["error"].as_str()), (409, Some("locked_vscode")));
    assert_eq!(e["held_by"], vscode_idle());

    // VS Code writes (recent); then the user closes the session there.
    p["recent"] = json!(true);
    p["event"] = record_event("agent.message", json!({"text": "ok", "final": true}), false);
    w.send("mirror.event", p.clone()).await;
    w.expect("ack").await;
    holder_report(&mut w, &runtime, Value::Null).await;
    let s = hub.session(&sid).await;
    assert_eq!(s["locked_by"], "none", "{s}");
    assert!(s.get("held_by").is_none(), "{s}");
    // The last records the process wrote before it exited come after: written within 30 s,
    // but nobody holds the transcript, so the session stays free (the live T20 run).
    p["held_by"] = Value::Null;
    p["event"] = record_event(
        "agent.message",
        json!({"text": "bye", "final": true}),
        false,
    );
    w.send("mirror.event", p).await;
    w.expect("ack").await;
    assert_eq!(hub.session(&sid).await["locked_by"], "none");
    // A new transcript written just now, nobody holding it: imported free.
    let other = Uuid::new_v4().to_string();
    let q = json!({"workspace_path": WS, "runtime_session_id": other, "cwd": WS,
        "recent": true, "held_by": null,
        "event": record_event("user.message", json!({"text": "x", "artifact_ids": []}), true)});
    w.send("mirror.event", q).await;
    w.expect("ack").await;
    let fresh = sessions(&hub, &pid)
        .await
        .into_iter()
        .find(|s| s["agent"]["runtime_session_id"] == other.as_str())
        .unwrap();
    assert_eq!(fresh["locked_by"], "none", "{fresh}");
    // A report for a transcript the hub does not know is acked and ignored.
    holder_report(&mut w, &Uuid::new_v4().to_string(), vscode_idle()).await;

    // Written a moment ago, but nobody holds it: no 30 s wait with the registry.
    let (st, v) = hub
        .post(&format!("/api/sessions/{sid}/takeover"), json!({}))
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["locked_by"], "hqc");
    let start = ack_start(&mut w, &sid).await;
    assert_eq!(start["payload"]["mode"], "resume");
    hub.finish().await;
}

/// T20: VS Code opens a session hqc runs: it is VS Code's at once, `session_released` says
/// `vscode_open`, pending input fails; the worker's later `vscode` does not release twice;
/// closing it in VS Code frees it once hqc's process has stopped. A reconnect forgets the
/// holders until the worker reports them again.
#[tokio::test]
async fn holder_releases_hqc() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online_bare(&hub, &token, ROOT, hub.boot_id).await;
    w.see_mirror_set = true;
    let pid = project(&hub, "cad").await;
    import_workspace(&hub, &mut w, &pid).await;
    let runtime = Uuid::new_v4().to_string();
    let prompt = record_event(
        "user.message",
        json!({"text": "old", "artifact_ids": []}),
        true,
    );
    mirror_event(&mut w, &runtime, prompt, None, false).await;
    let sid = sessions(&hub, &pid).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, _) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "go", "client_op_id": "a"}),
        )
        .await;
    assert_eq!(st, 201);
    ack_start(&mut w, &sid).await;
    let input = w.expect("session.input").await;
    w.reply(
        &input["id"],
        "ack",
        json!({"event_id": input["payload"]["event_id"]}),
    )
    .await;
    w.send(
        "session.state",
        json!({"session_id": sid, "process_state": "running", "runtime_session_id": runtime,
               "locked_by": "hqc"}),
    )
    .await;
    w.expect("ack").await;
    hub.wait_session(&sid, "running", |s| s["state"] == "running")
        .await;
    let (_, queued) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "queued", "client_op_id": "b"}),
        )
        .await;
    w.expect("session.input").await;

    holder_report(&mut w, &runtime, vscode_idle()).await;
    let s = hub.session(&sid).await;
    assert_eq!(s["locked_by"], "vscode", "{s}");
    assert_eq!(s["held_by"], vscode_idle());
    let evs = hub.events(&sid).await;
    let rel: Vec<&Value> = evs
        .iter()
        .filter(|e| e["payload"]["code"] == "session_released")
        .collect();
    assert_eq!(rel.len(), 1, "{evs:?}");
    assert_eq!(rel[0]["payload"]["reason"], "vscode_open");
    assert!(rel[0]["payload"]["text"]
        .as_str()
        .unwrap()
        .contains("close it in VS Code"));
    let q = evs
        .iter()
        .find(|e| e["event_id"] == queued["event"]["event_id"])
        .unwrap();
    assert_eq!(q["delivery"], "failed");

    // Closed in VS Code while hqc's process still finishes its turn: still VS Code's.
    holder_report(&mut w, &runtime, Value::Null).await;
    assert_eq!(hub.session(&sid).await["locked_by"], "vscode");
    // The worker stopped its agent.
    w.send(
        "session.state",
        json!({"session_id": sid, "process_state": "stopped", "runtime_session_id": runtime,
               "locked_by": "vscode"}),
    )
    .await;
    w.expect("ack").await;
    let evs = hub.events(&sid).await;
    assert_eq!(
        evs.iter()
            .filter(|e| e["payload"]["code"] == "session_released")
            .count(),
        1
    );
    // Opened and closed again: free at once.
    holder_report(&mut w, &runtime, vscode_idle()).await;
    holder_report(&mut w, &runtime, Value::Null).await;
    assert_eq!(hub.session(&sid).await["locked_by"], "none");

    // Held across a reconnect: the hub forgets the holder, the lock stays.
    holder_report(&mut w, &runtime, vscode_idle()).await;
    w.close().await;
    let (mut w, _) = FakeWorker::online_bare(&hub, &token, ROOT, hub.boot_id).await;
    w.see_mirror_set = true;
    w.expect("mirror.set").await;
    let s = hub.session(&sid).await;
    assert_eq!(s["locked_by"], "vscode");
    assert!(s.get("held_by").is_none(), "{s}");
    holder_report(&mut w, &runtime, vscode_idle()).await;
    assert_eq!(hub.session(&sid).await["held_by"], vscode_idle());
    hub.finish().await;
}

/// T20: `system session_forked` from the mirror is stored as a system event with its
/// fields, once per fork point.
#[tokio::test]
async fn forked_event_is_stored() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online_bare(&hub, &token, ROOT, hub.boot_id).await;
    w.see_mirror_set = true;
    let pid = project(&hub, "cad").await;
    import_workspace(&hub, &mut w, &pid).await;
    let runtime = Uuid::new_v4().to_string();
    let prompt = record_event(
        "user.message",
        json!({"text": "old", "artifact_ids": []}),
        true,
    );
    mirror_event(&mut w, &runtime, prompt, None, false).await;
    let at = Uuid::new_v4();
    let forked = json!({
        "session_id": Uuid::nil(), "event_id": Uuid::new_v4(), "ts": "2026-10-23T18:20:41Z",
        "author": {"kind": "system"}, "type": "system",
        "payload": {"text": "VS Code continued from an earlier point: 37 turns from hqc are not in its context",
                    "code": "session_forked", "at_uuid": at, "branch_from": "vscode",
                    "orphaned_turns": 37}
    });
    let ack = mirror_event(&mut w, &runtime, forked.clone(), None, true).await;
    assert!(ack["seq"].as_u64().is_some(), "{ack}");
    let dup = mirror_event(&mut w, &runtime, forked, None, true).await;
    assert_eq!(dup["seq"], ack["seq"]);
    let sid = sessions(&hub, &pid).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let evs = hub.events(&sid).await;
    let f: Vec<&Value> = evs
        .iter()
        .filter(|e| e["payload"]["code"] == "session_forked")
        .collect();
    assert_eq!(f.len(), 1, "{evs:?}");
    assert_eq!(f[0]["author"], json!({"kind": "system"}));
    assert_eq!(f[0]["payload"]["orphaned_turns"], 37);
    assert_eq!(f[0]["payload"]["branch_from"], "vscode");
    assert_eq!(f[0]["payload"]["at_uuid"], at.to_string());
    hub.finish().await;
}
