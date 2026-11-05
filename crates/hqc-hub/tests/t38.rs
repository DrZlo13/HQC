//! T38 (SPEC §3 Session, §5.3, §5.4, §6, §7): a session's permission mode. Created with a
//! mode or without one (the worker's default), switched with `PATCH`, reported back by the
//! worker; `hqc_spawn` never starts a session in `auto` and a peer message does not change
//! the mode; another owner's worker or session cannot be switched to `auto`.

mod common;

use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const WS_PATH: &str = "/Users/home-mac/work/hqc";

async fn create(hub: &TestHub, body: Value) -> (u16, Value) {
    let pid = hub.personal_project().await;
    hub.post(&format!("/api/projects/{pid}/sessions"), body)
        .await
}

/// The worker reports `session.state` of a running agent in `mode`.
async fn report(w: &mut FakeWorker, sid: &str, mode: &str) {
    let id = w
        .send(
            "session.state",
            json!({"session_id": sid, "process_state": "running", "locked_by": "hqc",
                   "permission_mode": mode}),
        )
        .await;
    let ack = w.expect("ack").await;
    assert_eq!(ack["id"], id.as_str());
}

#[tokio::test]
async fn a_session_is_created_with_a_mode_and_shows_what_the_worker_runs() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;

    let (s, v) = create(&hub, json!({"title": "Auto", "permission_mode": "auto"})).await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    assert_eq!(v["permission_mode"], "auto");
    assert!(v.get("effective_permission_mode").is_none(), "{v}");
    let start = ack_start(&mut w, &sid).await;
    assert_eq!(start["payload"]["permission_mode"], "auto");
    report(&mut w, &sid, "auto").await;
    let v = hub
        .wait_session(&sid, "reported mode", |v| {
            v["effective_permission_mode"] == "auto"
        })
        .await;
    assert_eq!(v["permission_mode"], "auto");

    // Without a mode the worker's `[permissions] mode` decides: nothing in `session.start`.
    let (s, v) = create(&hub, json!({"title": "Default"})).await;
    assert_eq!(s, 201, "{v}");
    let other = v["id"].as_str().unwrap().to_string();
    assert!(v.get("permission_mode").is_none(), "{v}");
    let start = ack_start(&mut w, &other).await;
    assert!(start["payload"].get("permission_mode").is_none(), "{start}");

    // Only ask or auto.
    let (s, v) = create(&hub, json!({"permission_mode": "bypassPermissions"})).await;
    assert_eq!(s, 422, "{v}");
}

#[tokio::test]
async fn a_switch_reaches_the_running_agent_and_the_next_start() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (s, v) = create(&hub, json!({"title": "S", "permission_mode": "ask"})).await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    ack_start(&mut w, &sid).await;
    report(&mut w, &sid, "ask").await;

    let (s, v) = hub
        .patch(
            &format!("/api/sessions/{sid}"),
            json!({"permission_mode": "auto"}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["permission_mode"], "auto");
    // Until the worker confirms, the session still runs in ask.
    assert_eq!(v["effective_permission_mode"], "ask");
    let f = w.expect("session.permission_mode").await;
    assert_eq!(
        f["payload"],
        json!({"session_id": sid, "permission_mode": "auto"})
    );
    w.reply(&f["id"], "ack", json!({})).await;

    // A worker with `allow_auto = false` keeps running ask, and says so.
    report(&mut w, &sid, "ask").await;
    let v = hub.session(&sid).await;
    assert_eq!(
        (&v["permission_mode"], &v["effective_permission_mode"]),
        (&json!("auto"), &json!("ask"))
    );

    // A stopped agent starts in the mode the session has now.
    let id = w
        .send(
            "session.state",
            json!({"session_id": sid, "process_state": "stopped", "locked_by": "none",
                   "permission_mode": "ask"}),
        )
        .await;
    assert_eq!(w.expect("ack").await["id"], id.as_str());
    let (s, v) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": "go"}))
        .await;
    assert_eq!(s, 201, "{v}");
    let start = ack_start(&mut w, &sid).await;
    assert_eq!(start["payload"]["permission_mode"], "auto");
    assert_eq!(start["payload"]["mode"], "resume");

    // Back to ask, without a worker: stored, sent with the next start.
    w.close().await;
    let (s, v) = hub
        .patch(
            &format!("/api/sessions/{sid}"),
            json!({"permission_mode": "ask"}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["permission_mode"], "ask");
}

/// A started session in the personal project.
async fn started(hub: &TestHub, w: &mut FakeWorker, body: Value) -> (String, String) {
    let (s, v) = create(hub, body).await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    let f = ack_start(w, &sid).await;
    (sid, f["payload"]["session_token"].as_str().unwrap().into())
}

/// The caller's turn is a human's: its input delivered.
async fn human_turn(hub: &TestHub, w: &mut FakeWorker, sid: &str) {
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "spawn a helper"}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let input = w.expect("session.input").await;
    w.reply(
        &input["id"],
        "ack",
        json!({"event_id": input["payload"]["event_id"]}),
    )
    .await;
    hub.wait_session(sid, "running", |v| v["state"] == "running")
        .await;
}

async fn call(w: &mut FakeWorker, token: &str, tool: &str, args: Value) -> (Value, Vec<Value>) {
    let rid = Uuid::now_v7().to_string();
    let id = w
        .send(
            "mcp.call",
            json!({"request_id": rid, "session_token": token, "tool": tool, "args": args}),
        )
        .await;
    let mut other = Vec::new();
    loop {
        let f = w.recv().await;
        if f["type"] == "mcp.result" && f["id"] == id.as_str() {
            return (f["payload"].clone(), other);
        }
        other.push(f);
    }
}

#[tokio::test]
async fn spawned_sessions_and_peer_messages_do_not_carry_auto() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = started(
        &hub,
        &mut w,
        json!({"title": "Spawner", "permission_mode": "auto"}),
    )
    .await;
    human_turn(&hub, &mut w, &a).await;

    // `hqc_spawn` has no argument for the mode.
    let (r, _) = call(
        &mut w,
        &ta,
        "hqc_spawn",
        json!({"project": "personal", "prompt": "hi", "permission_mode": "auto"}),
    )
    .await;
    assert_eq!(r["error"]["code"], "invalid_args", "{r}");

    // The new session runs in ask, though its spawner runs in auto.
    let (r, before) = call(
        &mut w,
        &ta,
        "hqc_spawn",
        json!({"project": "personal", "prompt": "Count the files."}),
    )
    .await;
    assert!(r.get("error").is_none(), "{r}");
    let start = match before.into_iter().find(|f| f["type"] == "session.start") {
        Some(f) => f,
        None => w.expect("session.start").await,
    };
    let b = start["payload"]["session_id"].as_str().unwrap().to_string();
    assert_eq!(start["payload"]["permission_mode"], "ask");
    assert_eq!(hub.session(&b).await["permission_mode"], "ask");
    w.reply(&start["id"], "ack", json!({})).await;
    let input = w.expect("session.input").await;
    w.reply(
        &input["id"],
        "ack",
        json!({"event_id": input["payload"]["event_id"]}),
    )
    .await;

    // A message from the auto session into an ask one leaves the mode alone.
    let (r, _) = call(
        &mut w,
        &ta,
        "hqc_send",
        json!({"session": "personal/session-2", "text": "and the lines too"}),
    )
    .await;
    assert!(r.get("error").is_none(), "{r}");
    let v = hub.session(&b).await;
    assert_eq!(v["permission_mode"], "ask");
    assert_eq!(hub.session(&a).await["permission_mode"], "auto");
}

#[tokio::test]
async fn only_the_worker_owner_switches_to_auto() {
    let hub = TestHub::start().await;
    let (worker_id, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (sid, _) = started(&hub, &mut w, json!({"title": "Mine"})).await;
    let db = &hub.app.db;
    let stranger = hqc_hub::db::ensure_user(db, "stranger@example.com")
        .await
        .unwrap();

    // Another user's session: not found, in either direction.
    let theirs = hqc_hub::db::create_project(db, stranger.id, "Theirs", "")
        .await
        .unwrap();
    let their_session = hqc_hub::db::create_session(
        db,
        hqc_hub::db::NewSession {
            project_id: theirs.id,
            owner_id: stranger.id,
            workspace_id: None,
            worker_id: None,
            cwd: ".",
            title: Some("Theirs"),
            model: None,
            auto_approve: false,
            permission_mode: None,
        },
    )
    .await
    .unwrap();
    for mode in ["ask", "auto"] {
        let (s, _) = hub
            .patch(
                &format!("/api/sessions/{their_session}"),
                json!({"permission_mode": mode}),
            )
            .await;
        assert_eq!(s, 404, "{mode}");
    }

    // My session on another owner's worker: ask yes, auto no.
    sqlx::query("UPDATE workers SET owner_id = $2 WHERE id = $1::uuid")
        .bind(&worker_id)
        .bind(stranger.id)
        .execute(db)
        .await
        .unwrap();
    let (s, v) = hub
        .patch(
            &format!("/api/sessions/{sid}"),
            json!({"permission_mode": "auto"}),
        )
        .await;
    assert_eq!(s, 403, "{v}");
    let (s, v) = hub
        .patch(
            &format!("/api/sessions/{sid}"),
            json!({"permission_mode": "ask"}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["permission_mode"], "ask");
    let f = w.expect("session.permission_mode").await;
    assert_eq!(f["payload"]["permission_mode"], "ask");
}
