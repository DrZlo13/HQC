//! T50 (SPEC §3 Session, §6, §17.4): `archived_at` on the session object, and the order of a
//! project's session list per user: the mode and the custom order in one record, only the
//! project's unarchived sessions in it, the missing ones (a new session) first.

mod common;

use common::*;
use serde_json::{json, Value};

const WS: &str = "/w/t50";

async fn order(hub: &TestHub, pid: &str) -> Value {
    let (s, o) = hub.get(&format!("/api/projects/{pid}/session_order")).await;
    assert_eq!(s, 200, "{o}");
    o
}

fn ids(o: &Value) -> Vec<String> {
    o["session_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn archived_at_is_set_on_archive_and_cleared_on_unarchive() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (_w, _) = FakeWorker::online(&hub, &token, WS).await;
    let sid = hub.create_session("Old SPI experiment").await;
    assert!(hub.session(&sid).await.get("archived_at").is_none());

    let before = chrono::Utc::now();
    let (s, v) = hub
        .post(&format!("/api/sessions/{sid}/archive"), json!({}))
        .await;
    assert_eq!(s, 200, "{v}");
    let at: chrono::DateTime<chrono::Utc> = v["archived_at"].as_str().unwrap().parse().unwrap();
    assert!(at >= before - chrono::Duration::seconds(5), "{v}");
    assert_eq!(v["state"], "archived");
    // In the list and the session object alike.
    let pid = hub.personal_project().await;
    let (_, list) = hub.get(&format!("/api/projects/{pid}/sessions")).await;
    let row = list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == sid.as_str())
        .unwrap()
        .clone();
    assert_eq!(row["archived_at"], v["archived_at"]);

    let (s, v) = hub
        .post(&format!("/api/sessions/{sid}/unarchive"), json!({}))
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(v.get("archived_at").is_none(), "{v}");

    // Archived before migration 0026: no time.
    sqlx::query("UPDATE sessions SET archived = true, archived_at = NULL WHERE id = $1::uuid")
        .bind(&sid)
        .execute(&hub.app.db)
        .await
        .unwrap();
    let v = hub.session(&sid).await;
    assert_eq!(v["archived"], true);
    assert!(v.get("archived_at").is_none(), "{v}");
    hub.finish().await;
}

#[tokio::test]
async fn the_order_round_trips_with_its_mode() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (_w, _) = FakeWorker::online(&hub, &token, WS).await;
    let pid = hub.personal_project().await;
    let a = hub.create_session("A").await;
    let b = hub.create_session("B").await;
    let c = hub.create_session("C").await;

    // No record yet: State, the sessions newest first.
    let o = order(&hub, &pid).await;
    assert_eq!(o["mode"], "state");
    assert_eq!(ids(&o), [c.as_str(), b.as_str(), a.as_str()]);

    let (s, o) = hub
        .put(
            &format!("/api/projects/{pid}/session_order"),
            json!({"mode": "custom", "session_ids": [a, c, b]}),
        )
        .await;
    assert_eq!(s, 200, "{o}");
    assert_eq!(
        (o["mode"].as_str(), ids(&o)),
        (Some("custom"), vec![a.clone(), c.clone(), b.clone()])
    );
    assert_eq!(order(&hub, &pid).await, o, "stored");

    // The mode alone keeps the order, and the order alone keeps the mode.
    let (s, o) = hub
        .put(
            &format!("/api/projects/{pid}/session_order"),
            json!({"mode": "recent"}),
        )
        .await;
    assert_eq!(s, 200, "{o}");
    assert_eq!(
        (o["mode"].as_str(), ids(&o)),
        (Some("recent"), vec![a.clone(), c.clone(), b.clone()])
    );
    let (_, o) = hub
        .put(
            &format!("/api/projects/{pid}/session_order"),
            json!({"session_ids": [b, a, c]}),
        )
        .await;
    assert_eq!(
        (o["mode"].as_str(), ids(&o)),
        (Some("recent"), vec![b.clone(), a.clone(), c.clone()])
    );

    let (s, _) = hub
        .put(
            &format!("/api/projects/{pid}/session_order"),
            json!({"mode": "name"}),
        )
        .await;
    assert_eq!(s, 422, "an unknown mode");
    hub.finish().await;
}

#[tokio::test]
async fn the_order_keeps_only_the_projects_live_sessions_and_a_new_one_goes_on_top() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (_w, _) = FakeWorker::online(&hub, &token, WS).await;
    let pid = hub.personal_project().await;
    let a = hub.create_session("A").await;
    let b = hub.create_session("B").await;
    let c = hub.create_session("C").await;

    // Another user's session in their own project.
    let db = &hub.app.db;
    let stranger = hqc_hub::db::ensure_user(db, "stranger-t50@example.com")
        .await
        .unwrap();
    let theirs = hqc_hub::db::create_project(db, stranger.id, "Theirs", "")
        .await
        .unwrap();
    let foreign = hqc_hub::db::create_session(
        db,
        hqc_hub::db::NewSession {
            project_id: theirs.id,
            owner_id: stranger.id,
            workspace_id: None,
            worker_id: None,
            cwd: ".",
            title: Some("Secret"),
            model: None,
            auto_approve: false,
            permission_mode: None,
        },
    )
    .await
    .unwrap()
    .to_string();
    // An archived one of this project.
    let (s, _) = hub
        .post(&format!("/api/sessions/{c}/archive"), json!({}))
        .await;
    assert_eq!(s, 200);

    // Unknown, foreign, archived and repeated ids are dropped; `b`, missing, goes first.
    let unknown = uuid::Uuid::new_v4().to_string();
    let (s, o) = hub
        .put(
            &format!("/api/projects/{pid}/session_order"),
            json!({"mode": "custom", "session_ids": [foreign, a, c, unknown, a]}),
        )
        .await;
    assert_eq!(s, 200, "{o}");
    assert_eq!(ids(&o), [b.as_str(), a.as_str()]);
    let (_, stored): (String, Vec<uuid::Uuid>) =
        sqlx::query_as("SELECT mode, session_ids FROM session_orders WHERE project_id = $1::uuid")
            .bind(&pid)
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(
        stored.iter().map(|u| u.to_string()).collect::<Vec<_>>(),
        [b.as_str(), a.as_str()],
        "stored as returned"
    );

    // A new session is on top; a restored one comes back as missing, on top too.
    let d = hub.create_session("D").await;
    assert_eq!(
        ids(&order(&hub, &pid).await),
        [d.as_str(), b.as_str(), a.as_str()]
    );
    let (s, _) = hub
        .post(&format!("/api/sessions/{c}/unarchive"), json!({}))
        .await;
    assert_eq!(s, 200);
    assert_eq!(
        ids(&order(&hub, &pid).await),
        [d.as_str(), c.as_str(), b.as_str(), a.as_str()]
    );
    // A deleted one drops out.
    assert_eq!(hub.delete(&format!("/api/sessions/{b}")).await, 204);
    assert_eq!(
        ids(&order(&hub, &pid).await),
        [d.as_str(), c.as_str(), a.as_str()]
    );

    // Another user's project is not found, to read or to write.
    let tp = theirs.id.to_string();
    let (s, _) = hub.get(&format!("/api/projects/{tp}/session_order")).await;
    assert_eq!(s, 404);
    let (s, _) = hub
        .put(
            &format!("/api/projects/{tp}/session_order"),
            json!({"session_ids": [foreign]}),
        )
        .await;
    assert_eq!(s, 404);
    hub.finish().await;
}
