//! T19: enrollment that replaces an existing worker, revocation and deletion of workers
//! (SPEC §5.1, §6, §10.2, §17.4).

mod common;

use std::time::Duration;

use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

/// `POST /worker/enroll` as `name`: `(code, poll_token)`.
async fn start_enroll(hub: &TestHub, name: &str) -> (String, String) {
    let (s, e) = hub
        .post("/worker/enroll", json!({"name": name, "os": "linux"}))
        .await;
    assert_eq!(s, 200, "{e}");
    (
        e["code"].as_str().unwrap().to_string(),
        e["poll_token"].as_str().unwrap().to_string(),
    )
}

async fn poll(hub: &TestHub, poll_token: &str) -> Value {
    let (s, p) = hub
        .post("/worker/enroll/poll", json!({"poll_token": poll_token}))
        .await;
    assert_eq!(s, 200, "{p}");
    p
}

/// `GET /api/workers` (`?include_revoked=1` with `all`).
async fn workers(hub: &TestHub, all: bool) -> Vec<Value> {
    let q = if all { "?include_revoked=1" } else { "" };
    let (s, v) = hub.get(&format!("/api/workers{q}")).await;
    assert_eq!(s, 200, "{v}");
    v["workers"].as_array().unwrap().clone()
}

async fn wait_offline(hub: &TestHub, id: &str) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let list = workers(hub, true).await;
        let w = list.iter().find(|w| w["id"] == id).unwrap();
        if w["status"] != "online" {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "worker still online"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// `DELETE /api/workers/{id}?purge=1`: status and body.
async fn purge(hub: &TestHub, id: &str) -> (u16, Value) {
    let r = hub
        .http
        .delete(format!("{}/api/workers/{id}?purge=1", hub.base))
        .send()
        .await
        .unwrap();
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(Value::Null))
}

fn system_codes(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .filter(|e| e["type"] == "system")
        .map(|e| e["payload"]["code"].as_str().unwrap().to_string())
        .collect()
}

// Replacement moves the new token onto the old worker id: sessions, workspaces and the
// service session stay; the old token stops working; the worker learns it replaced one.
#[tokio::test]
async fn replacement_keeps_the_worker_id_and_its_sessions() {
    let hub = TestHub::start().await;
    let (wid, old_token) = hub.enroll("old-mac").await;
    let (w, ack) = FakeWorker::online(&hub, &old_token, "/tmp/t19").await;
    let system = ack["payload"]["system_session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let sid = hub.create_session("work").await;
    let before = hub.session(&sid).await;
    w.close().await;
    wait_offline(&hub, &wid).await;

    let (code, pt) = start_enroll(&hub, "new-mac").await;
    let (s, info) = hub.get(&format!("/api/workers/enroll/{code}")).await;
    assert_eq!(s, 200, "{info}");
    assert_eq!(info["replaceable"].as_array().unwrap().len(), 1, "{info}");
    let r = &info["replaceable"][0];
    assert_eq!(
        (r["id"].as_str(), r["name"].as_str(), r["status"].as_str()),
        (Some(wid.as_str()), Some("old-mac"), Some("offline"))
    );
    assert_eq!(r["sessions_count"], 1);

    let (s, c) = hub
        .post(
            "/api/workers/enroll/confirm",
            json!({"code": code, "replace_worker_id": wid}),
        )
        .await;
    assert_eq!(s, 200, "{c}");
    assert_eq!(
        (
            c["worker_id"].as_str(),
            c["name"].as_str(),
            c["replaced"].as_str()
        ),
        (Some(wid.as_str()), Some("new-mac"), Some("old-mac"))
    );
    // The old token stops working at confirmation.
    assert!(FakeWorker::connect(&hub, &old_token).await.is_err());

    let p = poll(&hub, &pt).await;
    assert_eq!(p["status"], "confirmed");
    assert_eq!(
        (p["worker_id"].as_str(), p["replaced"].as_str()),
        (Some(wid.as_str()), Some("old-mac"))
    );
    let token = p["token"].as_str().unwrap();
    assert_ne!(token, old_token);

    // No new worker row: one worker, renamed, with the new OS.
    let list = workers(&hub, true).await;
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(
        (list[0]["name"].as_str(), list[0]["os"].as_str()),
        (Some("new-mac"), Some("linux"))
    );

    // The new process is the same worker with the same service session; it is a restart.
    let (_w, ack) = FakeWorker::online_boot(&hub, token, "/tmp/t19", Uuid::new_v4()).await;
    assert_eq!(ack["payload"]["worker_id"], wid.as_str());
    assert_eq!(ack["payload"]["system_session_id"], system.as_str());
    let (_, list) = hub.get("/api/workspaces").await;
    assert_eq!(list["workspaces"].as_array().unwrap().len(), 1, "{list}");
    let after = hub.session(&sid).await;
    assert_eq!(after["worker_id"], before["worker_id"]);
    assert_eq!(after["workspace_id"], before["workspace_id"]);
    assert_ne!(after["state"], "worker_offline");

    let sys = hub.session(&system).await;
    assert_eq!(sys["title"], "new-mac");
    let events = hub.events(&system).await;
    let replaced = events
        .iter()
        .find(|e| e["payload"]["code"] == "worker_replaced")
        .expect("worker_replaced in the service session");
    assert_eq!(replaced["payload"]["old_name"], "old-mac");
    assert_eq!(replaced["payload"]["new_name"], "new-mac");
    hub.finish().await;
}

// An online worker and another user's (or an unknown) worker cannot be replaced; the code
// stays usable.
#[tokio::test]
async fn online_and_foreign_workers_are_not_replaced() {
    let hub = TestHub::start().await;
    let (wid, token) = hub.enroll("busy").await;
    let (_w, _) = FakeWorker::online(&hub, &token, "/tmp/t19b").await;

    let (code, pt) = start_enroll(&hub, "intruder").await;
    let (_, info) = hub.get(&format!("/api/workers/enroll/{code}")).await;
    assert!(
        info.get("replaceable").is_none(),
        "online workers are not offered: {info}"
    );
    let (s, e) = hub
        .post(
            "/api/workers/enroll/confirm",
            json!({"code": code, "replace_worker_id": wid}),
        )
        .await;
    assert_eq!(
        (s, e["error"].as_str()),
        (409, Some("worker_online")),
        "{e}"
    );

    let other = hqc_hub::db::ensure_user(&hub.app.db, "someone-else@example.com")
        .await
        .unwrap();
    let foreign = Uuid::now_v7();
    sqlx::query("INSERT INTO workers (id, owner_id, name) VALUES ($1, $2, 'theirs')")
        .bind(foreign)
        .bind(other.id)
        .execute(&hub.app.db)
        .await
        .unwrap();
    for id in [foreign, Uuid::now_v7()] {
        let (s, e) = hub
            .post(
                "/api/workers/enroll/confirm",
                json!({"code": code, "replace_worker_id": id}),
            )
            .await;
        assert_eq!(s, 400, "{e}");
    }
    let theirs: String = sqlx::query_scalar("SELECT name FROM workers WHERE id = $1")
        .bind(foreign)
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
    assert_eq!(theirs, "theirs");

    // Refusals did not use up the code.
    let (s, c) = hub
        .post("/api/workers/enroll/confirm", json!({"code": code}))
        .await;
    assert_eq!(s, 200, "{c}");
    assert!(c.get("replaced").is_none());
    assert_ne!(c["worker_id"], wid.as_str());
    let p = poll(&hub, &pt).await;
    assert!(p.get("replaced").is_none(), "{p}");
    hub.finish().await;
}

// Revocation archives the service session with `system worker_revoked`; the revoked worker
// is listed only on request; replacing it lifts the revocation and brings the session back.
#[tokio::test]
async fn revocation_archives_the_service_session_and_replacement_restores_it() {
    let hub = TestHub::start().await;
    let (wid, token) = hub.enroll("gone").await;
    let (mut w, ack) = FakeWorker::online(&hub, &token, "/tmp/t19c").await;
    let system = ack["payload"]["system_session_id"]
        .as_str()
        .unwrap()
        .to_string();

    assert_eq!(hub.delete(&format!("/api/workers/{wid}")).await, 204);
    w.expect_closed().await;
    let sys = hub.session(&system).await;
    assert_eq!(sys["archived"], true);
    assert!(system_codes(&hub.events(&system).await).contains(&"worker_revoked".to_string()));
    assert!(workers(&hub, false).await.is_empty());
    let all = workers(&hub, true).await;
    assert_eq!(all[0]["status"], "revoked");
    assert!(all[0]["revoked_at"].is_string());
    assert_eq!(hub.delete(&format!("/api/workers/{wid}")).await, 404);

    let (code, pt) = start_enroll(&hub, "gone").await;
    let (_, info) = hub.get(&format!("/api/workers/enroll/{code}")).await;
    assert_eq!(info["replaceable"][0]["status"], "revoked", "{info}");
    let (s, c) = hub
        .post(
            "/api/workers/enroll/confirm",
            json!({"code": code, "replace_worker_id": wid}),
        )
        .await;
    assert_eq!(s, 200, "{c}");
    let p = poll(&hub, &pt).await;
    let (_w, ack) = FakeWorker::online(&hub, p["token"].as_str().unwrap(), "/tmp/t19c").await;
    assert_eq!(ack["payload"]["worker_id"], wid.as_str());
    assert_eq!(workers(&hub, false).await.len(), 1);
    let sys = hub.session(&system).await;
    assert_eq!(sys["archived"], false);
    assert_eq!(
        system_codes(&hub.events(&system).await),
        ["worker_revoked", "worker_replaced"]
    );
    hub.finish().await;
}

// Deleting a revoked worker: refused while sessions work there (`409 worker_has_sessions`
// with their number) and for a worker that is not revoked; otherwise the worker, its
// service session and its workspaces go, archived sessions stay without them.
#[tokio::test]
async fn purge_deletes_a_revoked_worker_without_live_sessions() {
    let hub = TestHub::start().await;
    let (wid, token) = hub.enroll("old").await;
    let (w, ack) = FakeWorker::online(&hub, &token, "/tmp/t19d").await;
    let system = ack["payload"]["system_session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let sid = hub.create_session("keep").await;
    w.close().await;

    let (s, e) = purge(&hub, &wid).await;
    assert_eq!(
        (s, e["error"].as_str()),
        (409, Some("worker_not_revoked")),
        "{e}"
    );
    assert_eq!(hub.delete(&format!("/api/workers/{wid}")).await, 204);
    let (s, e) = purge(&hub, &wid).await;
    assert_eq!(
        (s, e["error"].as_str(), e["sessions_count"].as_i64()),
        (409, Some("worker_has_sessions"), Some(1)),
        "{e}"
    );

    let (s, v) = hub
        .post(&format!("/api/sessions/{sid}/archive"), json!({}))
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, e) = purge(&hub, &wid).await;
    assert_eq!(s, 204, "{e}");
    assert!(workers(&hub, true).await.is_empty());
    let (s, _) = hub.get(&format!("/api/sessions/{system}")).await;
    assert_eq!(s, 404, "the service session is deleted");
    let (_, list) = hub.get("/api/workspaces").await;
    assert!(list["workspaces"].as_array().unwrap().is_empty(), "{list}");
    let (_, me) = hub.get("/api/me").await;
    let pid = me["personal_project_id"].as_str().unwrap();
    let (_, p) = hub.get(&format!("/api/projects/{pid}")).await;
    assert!(p["default_workspace_id"].is_null(), "{p}");
    // The archived session is history: no worker, no workspace.
    let kept = hub.session(&sid).await;
    assert_eq!(kept["archived"], true);
    assert!(
        kept["worker_id"].is_null() && kept["workspace_id"].is_null(),
        "{kept}"
    );
    let (s, _) = purge(&hub, &wid).await;
    assert_eq!(s, 404);
    hub.finish().await;
}

// Another user's worker is neither revoked nor deleted.
#[tokio::test]
async fn foreign_workers_are_not_deleted() {
    let hub = TestHub::start().await;
    let other = hqc_hub::db::ensure_user(&hub.app.db, "owner-2@example.com")
        .await
        .unwrap();
    let foreign = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO workers (id, owner_id, name, revoked_at) VALUES ($1, $2, 'theirs', now())",
    )
    .bind(foreign)
    .bind(other.id)
    .execute(&hub.app.db)
    .await
    .unwrap();
    let (s, _) = purge(&hub, &foreign.to_string()).await;
    assert_eq!(s, 404);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM workers WHERE id = $1")
        .bind(foreign)
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
    assert_eq!(n, 1);
    hub.finish().await;
}
