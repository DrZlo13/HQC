//! T17: the worker's service session and confirmations outside sessions (SPEC §3 Worker,
//! §5.3/§5.4 `worker.approval.*`, §10.2, §16).

mod common;

use std::time::Duration;

use chrono::Utc;
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const WS_PATH: &str = "/w/hqc";

/// The service session of the only worker: from the personal project's list.
async fn system_session(hub: &TestHub) -> Value {
    let pid = hub.personal_project().await;
    let (s, v) = hub.get(&format!("/api/projects/{pid}/sessions")).await;
    assert_eq!(s, 200, "{v}");
    let list: Vec<Value> = v["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["system"] == true)
        .cloned()
        .collect();
    assert_eq!(list.len(), 1, "{v}");
    list[0].clone()
}

fn request(request_id: &str, expires_at: chrono::DateTime<Utc>) -> Value {
    json!({
        "request_id": request_id,
        "kind": "workspace.use",
        "description": "The hub wants to run sessions in /w/cad",
        "expires_at": expires_at,
    })
}

/// Send `worker.approval.request` and wait for the hub's `ack`.
async fn ask(w: &mut FakeWorker, payload: Value) {
    let id = w.send("worker.approval.request", payload).await;
    let ack = w.expect("ack").await;
    assert_eq!(ack["id"], id);
}

async fn wait_events(hub: &TestHub, sid: &str, n: usize) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let ev = hub.events(sid).await;
        if ev.len() >= n {
            return ev;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "only {} events: {ev:?}",
            ev.len()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Created at enrollment in the personal project: no workspace, no agent, the worker's
/// name; `hello.ack` and `GET /api/workers` name it. No input, no archive, no delete.
#[tokio::test]
async fn service_session_is_created_and_takes_no_input() {
    let hub = TestHub::start().await;
    let (worker_id, token) = hub.enroll("home-mac").await;
    let sys = system_session(&hub).await;
    let sid = sys["id"].as_str().unwrap().to_string();
    assert_eq!(sys["title"], "home-mac");
    assert_eq!(sys["worker_id"], worker_id.as_str());
    assert!(sys.get("workspace_id").is_none(), "{sys}");
    assert_eq!(sys["agent"]["runtime"], "none");
    assert_eq!(sys["state"], "worker_offline");

    let (mut w, ack) = FakeWorker::online(&hub, &token, WS_PATH).await;
    assert_eq!(ack["payload"]["system_session_id"], sid.as_str());
    let (_, workers) = hub.get("/api/workers").await;
    assert_eq!(workers["workers"][0]["system_session_id"], sid.as_str());
    hub.wait_session(&sid, "idle", |v| v["state"] == "idle")
        .await;

    let conflict = |s: u16, v: &Value| {
        assert_eq!(
            (s, v["error"].as_str()),
            (409, Some("system_session")),
            "{v}"
        );
    };
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "hello"}),
        )
        .await;
    conflict(s, &v);
    let (s, v) = hub
        .post(&format!("/api/sessions/{sid}/archive"), json!({}))
        .await;
    conflict(s, &v);
    let (s, v) = hub
        .post(&format!("/api/sessions/{sid}/stop"), json!({}))
        .await;
    conflict(s, &v);
    assert_eq!(hub.delete(&format!("/api/sessions/{sid}")).await, 409);
    let (s, _) = hub
        .patch(
            &format!("/api/sessions/{sid}"),
            json!({"auto_approve": true}),
        )
        .await;
    assert_eq!(s, 403);
    // Nothing was started for it.
    w.expect_silence(Duration::from_millis(200)).await;
    assert!(hub.events(&sid).await.is_empty());

    // Ordinary sessions in a folder the owner added say so to the worker (SPEC §10.2).
    let other = hub.create_session("work").await;
    let start = ack_start(&mut w, &other).await;
    assert_eq!(start["payload"]["owner_is_worker_owner"], true);
    assert_eq!(start["payload"]["requested_by_owner"], true);
    hub.finish().await;
}

/// `worker.approval.request` -> `approval.request` with `once` / `deny` in the service
/// session, a push to the owner; the owner's answer goes back as
/// `worker.approval.response`. A resend of the same request stores nothing new and gets the
/// answer again.
#[tokio::test]
async fn worker_approval_round_trip() {
    let hub = TestHub::start().await;
    let (s, _) = hub
        .post(
            "/api/push/subscribe",
            json!({"endpoint": "https://fcm.googleapis.com/fcm/send/sub-1", "expirationTime": null,
                   "keys": {"p256dh": "k", "auth": "a"}}),
        )
        .await;
    assert_eq!(s, 201);
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, ack) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = ack["payload"]["system_session_id"]
        .as_str()
        .unwrap()
        .to_string();

    ask(
        &mut w,
        request("wa-1", Utc::now() + chrono::TimeDelta::minutes(10)),
    )
    .await;
    let ev = wait_events(&hub, &sid, 1).await;
    assert_eq!(ev[0]["type"], "approval.request");
    assert_eq!(ev[0]["author"], json!({"kind": "system"}));
    let p = &ev[0]["payload"];
    assert_eq!(p["request_id"], "wa-1");
    assert_eq!(p["tool"], "workspace.use");
    assert_eq!(p["description"], "The hub wants to run sessions in /w/cad");
    let canon: Vec<&str> = p["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["canonical"].as_str().unwrap())
        .collect();
    assert_eq!(canon, ["once", "deny"]);
    hub.wait_session(&sid, "waiting", |v| v["state"] == "waiting")
        .await;

    // SPEC §16: pushed like any approval.
    let deadline = tokio::time::Instant::now() + WAIT;
    while hub.push.sent.lock().unwrap().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "no push");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    {
        let sent = hub.push.sent.lock().unwrap();
        let m: hqc_proto::api::PushMessage = serde_json::from_value(sent[0].1.clone()).unwrap();
        assert_eq!(m.kind, "approval.request");
        assert_eq!(m.session_id.to_string(), sid);
        assert_eq!(m.tag.as_deref(), Some(format!("{sid}:wa-1").as_str()));
        assert_eq!(m.title.as_deref(), Some("home-mac"));
    }

    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/wa-1"),
            json!({"option_id": "bogus"}),
        )
        .await;
    assert_eq!(s, 400, "{v}");
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/wa-1"),
            json!({"option_id": "once"}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let f = w.expect("worker.approval.response").await;
    assert_eq!(
        f["payload"],
        json!({"request_id": "wa-1", "option_id": "once"})
    );
    let ev = hub.events(&sid).await;
    assert_eq!(ev[1]["type"], "approval.response");
    assert_eq!(ev[1]["payload"]["option_id"], "once");
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/wa-1"),
            json!({"option_id": "deny"}),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (409, Some("request_closed")));
    hub.wait_session(&sid, "idle", |v| v["state"] == "idle")
        .await;

    // The worker did not ack the answer and asks again: the answer comes again, the event
    // is not written twice.
    ask(
        &mut w,
        request("wa-1", Utc::now() + chrono::TimeDelta::minutes(10)),
    )
    .await;
    let again = w.expect("worker.approval.response").await;
    assert_eq!(again["payload"]["option_id"], "once");
    w.reply(&again["id"], "ack", json!({})).await;
    assert_eq!(hub.events(&sid).await.len(), 2);

    // Log lines of the worker (SPEC §10.2) are stored, but do not count as unread.
    let before = hub.session(&sid).await["unread"].as_u64().unwrap();
    let log = json!({
        "session_id": sid,
        "event_id": Uuid::now_v7(),
        "ts": "2026-10-22T10:00:05Z",
        "author": {"kind": "system"},
        "type": "system",
        "payload": {"text": "hub connection lost", "code": "worker_log", "level": "warn"},
    });
    let a = w.event(log).await;
    assert!(a["seq"].is_u64(), "{a}");
    let ev = hub.events(&sid).await;
    assert_eq!(ev[2]["payload"]["level"], "warn");
    assert_eq!(hub.session(&sid).await["unread"].as_u64().unwrap(), before);
    hub.finish().await;
}

/// Only the worker's owner answers (SPEC §10.2, §11): with the session in the user's
/// project but the worker given to someone else, the answer is refused.
#[tokio::test]
async fn only_the_worker_owner_answers() {
    let hub = TestHub::start().await;
    let (worker_id, token) = hub.enroll("home-mac").await;
    let (mut w, ack) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = ack["payload"]["system_session_id"]
        .as_str()
        .unwrap()
        .to_string();
    ask(
        &mut w,
        request("wa-2", Utc::now() + chrono::TimeDelta::minutes(10)),
    )
    .await;
    wait_events(&hub, &sid, 1).await;

    let other = hqc_hub::db::ensure_user(&hub.app.db, "other@example.com")
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET owner_id = $2 WHERE id = $1")
        .bind(Uuid::parse_str(&worker_id).unwrap())
        .bind(other.id)
        .execute(&hub.app.db)
        .await
        .unwrap();
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/wa-2"),
            json!({"option_id": "once"}),
        )
        .await;
    assert_eq!(s, 403, "{v}");
    w.expect_silence(Duration::from_millis(200)).await;
    assert_eq!(hub.events(&sid).await.len(), 1);
    hub.finish().await;
}

/// Expiry: `approval.cancel {timeout}` and `worker.approval.response` with `deny`. A
/// request open when the worker restarts is cancelled with `worker_restart`, without frame.
#[tokio::test]
async fn worker_approval_expires_and_restart_cancels() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, ack) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = ack["payload"]["system_session_id"]
        .as_str()
        .unwrap()
        .to_string();

    ask(
        &mut w,
        request("wa-3", Utc::now() + chrono::TimeDelta::milliseconds(150)),
    )
    .await;
    let f = w.expect("worker.approval.response").await;
    assert_eq!(
        f["payload"],
        json!({"request_id": "wa-3", "option_id": "deny"})
    );
    w.reply(&f["id"], "ack", json!({})).await;
    let ev = wait_events(&hub, &sid, 2).await;
    assert_eq!(ev[1]["type"], "approval.cancel");
    assert_eq!(ev[1]["payload"]["reason"], "timeout");

    ask(
        &mut w,
        request("wa-4", Utc::now() + chrono::TimeDelta::minutes(10)),
    )
    .await;
    wait_events(&hub, &sid, 3).await;
    w.close().await;
    let (mut w, _) = FakeWorker::online_boot(&hub, &token, WS_PATH, Uuid::new_v4()).await;
    let ev = wait_events(&hub, &sid, 4).await;
    assert_eq!(ev[3]["type"], "approval.cancel");
    assert_eq!(ev[3]["payload"]["reason"], "worker_restart");
    w.expect_silence(Duration::from_millis(200)).await;
    hub.finish().await;
}

/// An answer given while the worker is offline reaches it after `hello` (same `boot_id`).
#[tokio::test]
async fn answer_while_offline_is_sent_after_hello() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, ack) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = ack["payload"]["system_session_id"]
        .as_str()
        .unwrap()
        .to_string();
    ask(
        &mut w,
        request("wa-5", Utc::now() + chrono::TimeDelta::minutes(10)),
    )
    .await;
    wait_events(&hub, &sid, 1).await;
    w.close().await;
    hub.wait_session(&sid, "offline", |v| v["state"] == "worker_offline")
        .await;
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/wa-5"),
            json!({"option_id": "deny"}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let f = w.expect("worker.approval.response").await;
    assert_eq!(
        f["payload"],
        json!({"request_id": "wa-5", "option_id": "deny"})
    );
    w.reply(&f["id"], "ack", json!({})).await;
    w.close().await;
    // Acked: not sent again.
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    w.expect_silence(Duration::from_millis(200)).await;
    hub.finish().await;
}

/// Migration 0006: workers registered before T17 get their service session.
#[tokio::test]
async fn migration_gives_old_workers_a_service_session() {
    const SEED: &str = r#"
        INSERT INTO users (id, email, display_name) VALUES
            ('0199f3a2-7c10-7000-8000-0000000000aa', '{email}', 'me');
        INSERT INTO projects (id, owner_id, name, slug, is_personal) VALUES
            ('0199f3a2-7c10-7000-8000-0000000000d1', '0199f3a2-7c10-7000-8000-0000000000aa',
             'Personal', 'personal', true);
        UPDATE users SET personal_project_id = '0199f3a2-7c10-7000-8000-0000000000d1';
        INSERT INTO workers (id, owner_id, name) VALUES
            ('0199f3a2-7c10-7000-8000-0000000000b1', '0199f3a2-7c10-7000-8000-0000000000aa',
             'home-mac');
    "#;
    let hub = TestHub::start_upgrading(6, SEED).await;
    let sys = system_session(&hub).await;
    assert_eq!(sys["title"], "home-mac");
    assert_eq!(sys["worker_id"], "0199f3a2-7c10-7000-8000-0000000000b1");
    assert_eq!(sys["agent"]["runtime"], "none");
    let (_, workers) = hub.get("/api/workers").await;
    assert_eq!(workers["workers"][0]["system_session_id"], sys["id"]);
    hub.finish().await;
}
