//! Archived projects (SPEC §3 Project, §6): `POST /api/projects/{id}/archive` and `/unarchive`
//! take a project off `GET /api/projects` and back, `?archived=1` lists the archived ones; the
//! sessions of an archived project are not touched.

mod common;

use common::*;
use serde_json::{json, Value};

fn ids(list: &Value) -> Vec<String> {
    list["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn an_archived_project_leaves_the_list_and_comes_back() {
    let hub = TestHub::start().await;
    let db = &hub.app.db;
    let personal = hub.personal_project().await;
    let (_, boards) = hub.post("/api/projects", json!({"name": "Boards"})).await;
    let (_, kiln) = hub.post("/api/projects", json!({"name": "Kiln"})).await;
    let boards = boards["id"].as_str().unwrap().to_string();
    let kiln = kiln["id"].as_str().unwrap().to_string();
    let me = hqc_hub::db::ensure_user(db, &hub.email).await.unwrap();
    let sid = hqc_hub::db::create_session(
        db,
        hqc_hub::db::NewSession {
            project_id: boards.parse().unwrap(),
            owner_id: me.id,
            workspace_id: None,
            worker_id: None,
            cwd: ".",
            title: Some("Layout"),
            model: None,
            auto_approve: false,
            permission_mode: None,
        },
    )
    .await
    .unwrap()
    .to_string();
    let before = hub.session(&sid).await;
    let (_, list) = hub.get("/api/projects").await;
    assert_eq!(ids(&list), [personal.clone(), boards.clone(), kiln.clone()]);
    assert!(list["projects"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p.get("archived_at").is_none()));
    let (_, none) = hub.get("/api/projects?archived=1").await;
    assert_eq!(ids(&none), Vec::<String>::new());
    let (s, ordered) = hub
        .put(
            "/api/me/project_order",
            json!({"project_ids": [boards, kiln, personal]}),
        )
        .await;
    assert_eq!(s, 200, "{ordered}");

    let (s, p) = hub
        .post(&format!("/api/projects/{boards}/archive"), json!({}))
        .await;
    assert_eq!(s, 200, "{p}");
    let since = p["archived_at"].as_str().expect("archived_at").to_string();
    let (_, list) = hub.get("/api/projects").await;
    assert_eq!(ids(&list), [kiln.clone(), personal.clone()]);
    let (_, archived) = hub.get("/api/projects?archived=1").await;
    assert_eq!(ids(&archived), std::slice::from_ref(&boards));
    assert_eq!(archived["projects"][0]["archived_at"], since.as_str());
    // The project itself still answers. The bar's order, saved without it as the client does,
    // keeps its place: first.
    let (s, one) = hub.get(&format!("/api/projects/{boards}")).await;
    assert_eq!(
        (s, one["archived_at"].as_str()),
        (200, Some(since.as_str()))
    );
    let (s, ordered) = hub
        .put(
            "/api/me/project_order",
            json!({"project_ids": [kiln, personal]}),
        )
        .await;
    assert_eq!(s, 200, "{ordered}");
    assert_eq!(ids(&ordered), [kiln.clone(), personal.clone()]);
    // Its session is as it was: listed, not archived, not written.
    let now = hub.session(&sid).await;
    assert_eq!(now["archived"], false);
    assert_eq!(now["updated_at"], before["updated_at"]);
    let (_, all) = hub.get("/api/sessions").await;
    assert!(all["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["id"] == sid.as_str()));

    // Again: the first time stays.
    let (s, again) = hub
        .post(&format!("/api/projects/{boards}/archive"), json!({}))
        .await;
    assert_eq!(
        (s, again["archived_at"].as_str()),
        (200, Some(since.as_str()))
    );

    // Back, in its place of the order.
    let (s, back) = hub
        .post(&format!("/api/projects/{boards}/unarchive"), json!({}))
        .await;
    assert_eq!(s, 200, "{back}");
    assert!(back.get("archived_at").is_none(), "{back}");
    let (_, list) = hub.get("/api/projects").await;
    assert_eq!(ids(&list), [boards.clone(), kiln.clone(), personal.clone()]);
    let (_, none) = hub.get("/api/projects?archived=1").await;
    assert_eq!(ids(&none), Vec::<String>::new());
    hub.finish().await;
}

/// The client saves the bar's order without the archived projects: each keeps its place, right
/// after the project before it when that one is still there, else at its old index.
#[tokio::test]
async fn a_saved_order_keeps_the_archived_projects_places() {
    let hub = TestHub::start().await;
    let personal = hub.personal_project().await;
    let mut made = Vec::new();
    for name in ["Boards", "Kiln", "Cad"] {
        let (_, p) = hub.post("/api/projects", json!({"name": name})).await;
        made.push(p["id"].as_str().unwrap().to_string());
    }
    let [boards, kiln, cad] = [made[0].clone(), made[1].clone(), made[2].clone()];
    let put = |list: Vec<&String>| json!({ "project_ids": list });
    let (s, _) = hub
        .put(
            "/api/me/project_order",
            put(vec![&boards, &kiln, &cad, &personal]),
        )
        .await;
    assert_eq!(s, 200);
    let archive = |id: &String, action: &str| format!("/api/projects/{id}/{action}");

    // Kiln archived; the bar reordered without it: it stays right after Boards.
    hub.post(&archive(&kiln, "archive"), json!({})).await;
    let (_, ordered) = hub
        .put("/api/me/project_order", put(vec![&cad, &boards, &personal]))
        .await;
    assert_eq!(
        ids(&ordered),
        [cad.clone(), boards.clone(), personal.clone()]
    );
    // Cad archived too, it was first; the bar reordered without both: Cad at its index, Kiln
    // after Boards.
    hub.post(&archive(&cad, "archive"), json!({})).await;
    let (_, ordered) = hub
        .put("/api/me/project_order", put(vec![&personal, &boards]))
        .await;
    assert_eq!(ids(&ordered), [personal.clone(), boards.clone()]);

    for id in [&cad, &kiln] {
        let (s, _) = hub.post(&archive(id, "unarchive"), json!({})).await;
        assert_eq!(s, 200);
    }
    let (_, list) = hub.get("/api/projects").await;
    assert_eq!(ids(&list), [cad, personal, boards, kiln]);
    hub.finish().await;
}

/// Someone who never saved an order sees the projects oldest first: an archived one keeps its
/// place in that order when the bar is first reordered.
#[tokio::test]
async fn an_archived_project_keeps_its_place_without_a_saved_order() {
    let hub = TestHub::start().await;
    let personal = hub.personal_project().await;
    let mut made = Vec::new();
    for name in ["Boards", "Kiln", "Cad"] {
        let (_, p) = hub.post("/api/projects", json!({"name": name})).await;
        made.push(p["id"].as_str().unwrap().to_string());
    }
    let [boards, kiln, cad] = [made[0].clone(), made[1].clone(), made[2].clone()];
    let (s, _) = hub
        .post(&format!("/api/projects/{kiln}/archive"), json!({}))
        .await;
    assert_eq!(s, 200);
    // The bar showed Personal, Boards, Cad; reordered without Kiln, which stood after Boards.
    let (_, ordered) = hub
        .put(
            "/api/me/project_order",
            json!({"project_ids": [boards, personal, cad]}),
        )
        .await;
    assert_eq!(
        ids(&ordered),
        [boards.clone(), personal.clone(), cad.clone()]
    );
    let (s, _) = hub
        .post(&format!("/api/projects/{kiln}/unarchive"), json!({}))
        .await;
    assert_eq!(s, 200);
    let (_, list) = hub.get("/api/projects").await;
    assert_eq!(ids(&list), [boards, kiln, personal, cad]);
    hub.finish().await;
}

#[tokio::test]
async fn the_personal_project_and_another_users_are_not_archived() {
    let hub = TestHub::start().await;
    let db = &hub.app.db;
    let personal = hub.personal_project().await;
    let (s, v) = hub
        .post(&format!("/api/projects/{personal}/archive"), json!({}))
        .await;
    assert_eq!((s, v["error"].as_str()), (409, Some("personal_project")));
    let (_, list) = hub.get("/api/projects").await;
    assert_eq!(ids(&list), [personal]);

    let stranger = hqc_hub::db::ensure_user(db, "stranger-arch@example.com")
        .await
        .unwrap();
    let theirs = hqc_hub::db::create_project(db, stranger.id, "Theirs", "")
        .await
        .unwrap()
        .id;
    for action in ["archive", "unarchive"] {
        let (s, _) = hub
            .post(&format!("/api/projects/{theirs}/{action}"), json!({}))
            .await;
        assert_eq!(s, 404, "{action}");
    }
    let (s, _) = hub
        .post(
            &format!("/api/projects/{}/archive", uuid::Uuid::now_v7()),
            json!({}),
        )
        .await;
    assert_eq!(s, 404);
    hub.finish().await;
}
