//! Favorites (SPEC §3, §6): sessions and projects a person marks as favorites, per user, with
//! `favorite_at` in the session object, its summary and the project object; `PUT` and
//! `DELETE .../favorite` need only read access and are idempotent.

mod common;

use chrono::{DateTime, Utc};
use common::*;
use serde_json::{json, Value};

fn at(v: &Value) -> Option<DateTime<Utc>> {
    v.get("favorite_at")
        .and_then(Value::as_str)
        .map(|s| s.parse().unwrap())
}

/// A session of `owner` in `project` with no workspace and no worker: nothing but the test
/// writes its row.
async fn session_of(hub: &TestHub, owner: uuid::Uuid, project: uuid::Uuid) -> String {
    hqc_hub::db::create_session(
        &hub.app.db,
        hqc_hub::db::NewSession {
            project_id: project,
            owner_id: owner,
            workspace_id: None,
            worker_id: None,
            cwd: ".",
            title: Some("Often"),
            model: None,
            autocompact: None,
            auto_approve: false,
            permission_mode: None,
        },
    )
    .await
    .unwrap()
    .to_string()
}

/// The summary line of `sid` in the next `session.summary` that satisfies `f`.
async fn summary_until(c: &mut FakeClient, sid: &str, f: impl Fn(&Value) -> bool) -> Value {
    loop {
        let u = c.until("session.summary").await;
        if u["session"]["session_id"] == sid && f(&u["session"]) {
            return u["session"].clone();
        }
    }
}

#[tokio::test]
async fn a_favorite_session_is_the_users_and_reaches_the_summary() {
    let hub = TestHub::start().await;
    let db = &hub.app.db;
    let me = hqc_hub::db::ensure_user(db, &hub.email).await.unwrap();
    let sid = session_of(&hub, me.id, me.personal_project_id.unwrap()).await;
    let before = hub.session(&sid).await;
    assert!(before.get("favorite_at").is_none(), "{before}");

    let mut c = FakeClient::connect(&hub).await;
    c.send(json!({"type": "subscribe_summary"})).await;
    let s = c.until("summary").await;
    let line = s["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["session_id"] == sid.as_str())
        .unwrap()
        .clone();
    assert!(line.get("favorite_at").is_none(), "{line}");

    let (s, v) = hub
        .put(&format!("/api/sessions/{sid}/favorite"), json!({}))
        .await;
    assert_eq!(s, 200, "{v}");
    let first = at(&v).expect("favorite_at");
    let line = summary_until(&mut c, &sid, |l| l.get("favorite_at").is_some()).await;
    assert_eq!(at(&line), Some(first));
    // The session object and the list carry it; the session row is not written.
    let now = hub.session(&sid).await;
    assert_eq!(at(&now), Some(first));
    assert_eq!(now["updated_at"], before["updated_at"]);
    let pid = hub.personal_project().await;
    let (_, list) = hub.get(&format!("/api/projects/{pid}/sessions")).await;
    let row = list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == sid.as_str())
        .unwrap()
        .clone();
    assert_eq!(at(&row), Some(first));

    // Idempotent: a second PUT keeps the first time.
    let (s, v) = hub
        .put(&format!("/api/sessions/{sid}/favorite"), json!({}))
        .await;
    assert_eq!((s, at(&v)), (200, Some(first)));

    // Another user's session is not found, either way.
    let stranger = hqc_hub::db::ensure_user(db, "stranger-fav@example.com")
        .await
        .unwrap();
    let their_project = hqc_hub::db::create_project(db, stranger.id, "Theirs", "")
        .await
        .unwrap();
    let theirs = session_of(&hub, stranger.id, their_project.id).await;
    let (s, _) = hub
        .put(&format!("/api/sessions/{theirs}/favorite"), json!({}))
        .await;
    assert_eq!(s, 404);
    assert_eq!(
        hub.delete(&format!("/api/sessions/{theirs}/favorite"))
            .await,
        404
    );

    // Off, twice; the summary follows.
    assert_eq!(
        hub.delete(&format!("/api/sessions/{sid}/favorite")).await,
        204
    );
    let line = summary_until(&mut c, &sid, |l| l.get("favorite_at").is_none()).await;
    assert!(line.get("favorite_at").is_none());
    assert!(hub.session(&sid).await.get("favorite_at").is_none());
    assert_eq!(
        hub.delete(&format!("/api/sessions/{sid}/favorite")).await,
        204
    );

    // Per user: another user's favorite of this session is not mine.
    sqlx::query("INSERT INTO session_favorites (user_id, session_id) VALUES ($1, $2::uuid)")
        .bind(stranger.id)
        .bind(&sid)
        .execute(db)
        .await
        .unwrap();
    assert!(hub.session(&sid).await.get("favorite_at").is_none());

    // Deleting the session drops its favorites.
    let (s, _) = hub
        .put(&format!("/api/sessions/{sid}/favorite"), json!({}))
        .await;
    assert_eq!(s, 200);
    assert_eq!(hub.delete(&format!("/api/sessions/{sid}")).await, 204);
    let left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM session_favorites WHERE session_id = $1::uuid")
            .bind(&sid)
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(left, 0);
    hub.finish().await;
}

#[tokio::test]
async fn a_favorite_project_is_in_the_project_object() {
    let hub = TestHub::start().await;
    let personal = hub.personal_project().await;
    let (s, p) = hub.post("/api/projects", json!({"name": "Boards"})).await;
    assert_eq!(s, 201, "{p}");
    let pid = p["id"].as_str().unwrap().to_string();
    assert!(p.get("favorite_at").is_none(), "{p}");

    let (s, v) = hub
        .put(&format!("/api/projects/{pid}/favorite"), json!({}))
        .await;
    assert_eq!(s, 200, "{v}");
    let first = at(&v).expect("favorite_at");
    let (s, v) = hub
        .put(&format!("/api/projects/{pid}/favorite"), json!({}))
        .await;
    assert_eq!(
        (s, at(&v)),
        (200, Some(first)),
        "a second PUT keeps the time"
    );

    // Every reply with the project carries it; the other project has none.
    let favorite = |list: &Value| -> Vec<(String, Option<DateTime<Utc>>)> {
        list["projects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p["id"].as_str().unwrap().to_string(), at(p)))
            .collect()
    };
    let (_, list) = hub.get("/api/projects").await;
    assert_eq!(
        favorite(&list),
        [(personal.clone(), None), (pid.clone(), Some(first))]
    );
    let (_, one) = hub.get(&format!("/api/projects/{pid}")).await;
    assert_eq!(at(&one), Some(first));
    let (s, patched) = hub
        .patch(&format!("/api/projects/{pid}"), json!({"color": "moss"}))
        .await;
    assert_eq!((s, at(&patched)), (200, Some(first)), "{patched}");
    let (s, ordered) = hub
        .put(
            "/api/me/project_order",
            json!({"project_ids": [pid, personal]}),
        )
        .await;
    assert_eq!(s, 200, "{ordered}");
    assert_eq!(
        favorite(&ordered),
        [(pid.clone(), Some(first)), (personal.clone(), None)]
    );

    // Another user's project is not found.
    let db = &hub.app.db;
    let stranger = hqc_hub::db::ensure_user(db, "stranger-favp@example.com")
        .await
        .unwrap();
    let theirs = hqc_hub::db::create_project(db, stranger.id, "Theirs", "")
        .await
        .unwrap()
        .id;
    let (s, _) = hub
        .put(&format!("/api/projects/{theirs}/favorite"), json!({}))
        .await;
    assert_eq!(s, 404);
    assert_eq!(
        hub.delete(&format!("/api/projects/{theirs}/favorite"))
            .await,
        404
    );

    // Off, twice.
    for _ in 0..2 {
        assert_eq!(
            hub.delete(&format!("/api/projects/{pid}/favorite")).await,
            204
        );
    }
    let (_, one) = hub.get(&format!("/api/projects/{pid}")).await;
    assert!(one.get("favorite_at").is_none(), "{one}");

    // Deleting the project drops its favorites.
    let (s, _) = hub
        .put(&format!("/api/projects/{pid}/favorite"), json!({}))
        .await;
    assert_eq!(s, 200);
    assert_eq!(hub.delete(&format!("/api/projects/{pid}")).await, 204);
    let left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM project_favorites WHERE project_id = $1::uuid")
            .bind(&pid)
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(left, 0);
    hub.finish().await;
}
