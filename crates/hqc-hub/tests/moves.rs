//! A session moves to another folder and project of its worker (SPEC §3 Session, §6):
//! `POST /api/sessions/{id}/move {path, project?, create?}`.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const ROOT: &str = "/w";
const HQC: &str = "/w/hqc";

/// `hello` of a worker that moves transcripts (`claude_code.moves`, SPEC §5.4).
fn moves() -> Value {
    json!({"capabilities": {"claude_code": {"version": "2.1.312", "moves": true}, "acp_agents": []}})
}

/// A worker with `extra` in its `hello`, the workspace `/w/hqc` in the personal project and
/// a session there, started and reported `process_state`; the worker, the session and its
/// first `session.start`.
async fn session(hub: &TestHub, extra: Value, process_state: &str) -> (FakeWorker, String, Value) {
    let (_, token) = hub.enroll("home-mac").await;
    let mut w = FakeWorker::connect(hub, &token).await.unwrap();
    w.hello_with(ROOT, hub.boot_id, extra).await;
    let pid = hub.personal_project().await;
    let (s, v) = hub.add_workspace(&mut w, &pid, HQC).await;
    assert_eq!(s, 201, "{v}");
    let sid = hub.create_session("moving").await;
    let start = ack_start(&mut w, &sid).await;
    state(&mut w, &sid, process_state).await;
    (w, sid, start)
}

/// `session.state` with `process_state`, acked.
async fn state(w: &mut FakeWorker, sid: &str, process_state: &str) {
    let locked = if process_state == "stopped" {
        "none"
    } else {
        "hqc"
    };
    let id = w
        .send(
            "session.state",
            json!({"session_id": sid, "process_state": process_state, "locked_by": locked}),
        )
        .await;
    let ack = w.expect("ack").await;
    assert_eq!(ack["id"], id.as_str());
}

/// `POST /api/sessions/{sid}/move` while the worker answers `fs.stat`.
async fn move_to(hub: &TestHub, w: &mut FakeWorker, sid: &str, body: Value) -> (u16, Value) {
    hub.while_worker_answers(w, hub.post(&format!("/api/sessions/{sid}/move"), body))
        .await
}

async fn projects(hub: &TestHub) -> Vec<Value> {
    let (s, v) = hub.get("/api/projects").await;
    assert_eq!(s, 200, "{v}");
    v["projects"].as_array().unwrap().clone()
}

fn moved_events(events: &[Value]) -> Vec<&Value> {
    events
        .iter()
        .filter(|e| e["type"] == "system" && e["payload"]["code"] == "session_moved")
        .collect()
}

/// Into a new folder without `project`: a new project named after it, the folder its first
/// workspace (its default, mirrored), the session there with `cwd: .` and its slug; the old
/// project's saved order forgets it, the feed says where it went, and the running agent gets
/// a new `session.start` with the new folder. The same call again changes nothing.
#[tokio::test]
async fn a_session_moves_to_a_new_project() {
    let hub = TestHub::start().await;
    let (mut w, sid, first) = session(&hub, moves(), "running").await;
    let pid = hub.personal_project().await;
    let (s, v) = hub
        .put(
            &format!("/api/projects/{pid}/session_order"),
            json!({"mode": "custom", "session_ids": [sid]}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let before = projects(&hub).await.len();

    let (s, v) = move_to(&hub, &mut w, &sid, json!({"path": "/w/foo"})).await;
    assert_eq!(s, 200, "{v}");
    let new_pid = v["project_id"].as_str().unwrap().to_string();
    assert_ne!(new_pid, pid);
    assert_eq!(v["slug"], "moving");
    assert_eq!(v["cwd"], ".");
    let list = projects(&hub).await;
    assert_eq!(list.len(), before + 1);
    let p = list.iter().find(|p| p["id"] == new_pid.as_str()).unwrap();
    assert_eq!((&p["name"], &p["slug"]), (&json!("foo"), &json!("foo")));
    let (_, ws) = hub
        .get(&format!("/api/workspaces?project_id={new_pid}"))
        .await;
    let ws = ws["workspaces"].as_array().unwrap();
    assert_eq!(ws.len(), 1);
    assert_eq!(
        (&ws[0]["path"], &ws[0]["kind"], &ws[0]["mirror_enabled"]),
        (&json!("/w/foo"), &json!("dir"), &json!(true))
    );
    assert_eq!(v["workspace_id"], ws[0]["id"]);
    assert_eq!(p["default_workspace_id"], ws[0]["id"]);

    let start = w.expect("session.start").await;
    let sp = &start["payload"];
    assert_eq!(sp["session_id"], sid.as_str());
    assert_eq!(
        (&sp["workspace_path"], &sp["cwd"]),
        (&json!("/w/foo"), &json!("."))
    );
    assert_eq!(sp["mode"], "resume");
    assert_ne!(sp["session_token"], first["payload"]["session_token"]);
    w.reply(&start["id"], "ack", json!({})).await;
    assert!(
        w.mirror_sets
            .iter()
            .any(|m| m["payload"]["workspace_path"] == "/w/foo" && m["payload"]["enabled"] == true),
        "{:?}",
        w.mirror_sets
    );

    let events = hub.events(&sid).await;
    let moved = moved_events(&events);
    assert_eq!(moved.len(), 1, "{events:?}");
    let text = moved[0]["payload"]["text"].as_str().unwrap();
    assert!(text.contains("foo") && text.contains("/w/foo"), "{text}");
    let order: Vec<Uuid> =
        sqlx::query_scalar("SELECT session_ids FROM session_orders WHERE project_id = $1")
            .bind(pid.parse::<Uuid>().unwrap())
            .fetch_one(&hub.app.db)
            .await
            .unwrap();
    assert!(order.is_empty(), "{order:?}");

    // A repeat: the same answer, no new project and no second `session_moved`; the start
    // goes out again, in case the first call ended before it.
    let (s, again) = move_to(&hub, &mut w, &sid, json!({"path": "/w/foo"})).await;
    assert_eq!(s, 200, "{again}");
    assert_eq!(again["project_id"], new_pid.as_str());
    assert_eq!(projects(&hub).await.len(), before + 1);
    assert_eq!(moved_events(&hub.events(&sid).await).len(), 1);
    let start = w.expect("session.start").await;
    assert_eq!(start["payload"]["workspace_path"], "/w/foo");
    w.reply(&start["id"], "ack", json!({})).await;
    w.expect_silence(Duration::from_millis(300)).await;
    hub.finish().await;
}

/// A move whose call ended once the session was moved in the database (the client went away,
/// a database error), before the worker heard of it: a repeat sends what did not go out, the
/// workspace's `mirror.set` and the start in the new folder, and no second `session_moved`.
#[tokio::test]
async fn a_repeat_sends_what_did_not_go_out() {
    let hub = TestHub::start().await;
    let (mut w, sid, _) = session(&hub, moves(), "running").await;
    let (s, p) = hub.post("/api/projects", json!({"name": "Foo"})).await;
    assert_eq!(s, 201, "{p}");
    let pid: Uuid = p["id"].as_str().unwrap().parse().unwrap();
    let (s, ws) = hub.add_workspace(&mut w, &pid.to_string(), "/w/foo").await;
    assert_eq!(s, 201, "{ws}");
    let wsid: Uuid = ws["id"].as_str().unwrap().parse().unwrap();
    // The first call as far as its `session_moved`.
    let db = &hub.app.db;
    let id: Uuid = sid.parse().unwrap();
    let row = hqc_hub::db::session(db, id).await.unwrap().unwrap();
    hqc_hub::db::move_session(db, &row, pid, wsid)
        .await
        .unwrap();
    let moved = hqc_hub::db::session(db, id).await.unwrap().unwrap();
    hqc_hub::events::system(&hub.app, &moved, hqc_hub::moves::MOVED, "moved")
        .await
        .unwrap();
    w.expect_silence(Duration::from_millis(300)).await;
    w.mirror_sets.clear();

    let (s, v) = move_to(&hub, &mut w, &sid, json!({"path": "/w/foo"})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["workspace_id"], wsid.to_string());
    let start = w.expect("session.start").await;
    assert_eq!(start["payload"]["workspace_path"], "/w/foo");
    assert!(
        w.mirror_sets
            .iter()
            .any(|m| m["payload"]["workspace_path"] == "/w/foo" && m["payload"]["enabled"] == true),
        "{:?}",
        w.mirror_sets
    );
    assert_eq!(moved_events(&hub.events(&sid).await).len(), 1);
    hub.finish().await;
}

/// Into a folder that is a workspace of the owner already: that workspace and its project;
/// the slug gets `-2` when a session there has it; `project` must be that project.
#[tokio::test]
async fn into_a_workspace_of_another_project() {
    let hub = TestHub::start().await;
    let (mut w, sid, _) = session(&hub, moves(), "stopped").await;
    let (s, other) = hub.post("/api/projects", json!({"name": "Other"})).await;
    assert_eq!(s, 201, "{other}");
    let other_id = other["id"].as_str().unwrap();
    let (s, v) = hub.add_workspace(&mut w, other_id, "/w/other").await;
    assert_eq!(s, 201, "{v}");
    let (s, v) = hub
        .post(
            &format!("/api/projects/{other_id}/sessions"),
            json!({"title": "moving"}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    ack_start(&mut w, v["id"].as_str().unwrap()).await;
    let before = projects(&hub).await.len();

    let pid = hub.personal_project().await;
    let (s, v) = move_to(
        &hub,
        &mut w,
        &sid,
        json!({"path": "/w/other", "project": pid}),
    )
    .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("bad_request")), "{v}");

    let (s, v) = move_to(&hub, &mut w, &sid, json!({"path": "/w/other"})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["project_id"], other_id);
    assert_eq!(v["slug"], "moving-2");
    assert_eq!(projects(&hub).await.len(), before);
    hub.finish().await;
}

/// Into another folder of the same project: the session keeps its place in that project's
/// saved order.
#[tokio::test]
async fn within_its_project_it_keeps_its_place() {
    let hub = TestHub::start().await;
    let (mut w, sid, _) = session(&hub, moves(), "stopped").await;
    let pid = hub.personal_project().await;
    let (s, v) = hub.add_workspace(&mut w, &pid, "/w/two").await;
    assert_eq!(s, 201, "{v}");
    let next = hub.create_session("next").await;
    ack_start(&mut w, &next).await;
    let (s, v) = hub
        .put(
            &format!("/api/projects/{pid}/session_order"),
            json!({"mode": "custom", "session_ids": [sid, next]}),
        )
        .await;
    assert_eq!(s, 200, "{v}");

    let (s, v) = move_to(&hub, &mut w, &sid, json!({"path": "/w/two"})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["project_id"], pid.as_str());
    let order: Vec<Uuid> =
        sqlx::query_scalar("SELECT session_ids FROM session_orders WHERE project_id = $1")
            .bind(pid.parse::<Uuid>().unwrap())
            .fetch_one(&hub.app.db)
            .await
            .unwrap();
    let want: Vec<Uuid> = vec![sid.parse().unwrap(), next.parse().unwrap()];
    assert_eq!(order, want);
    hub.finish().await;
}

/// What the move refuses, each time with nothing created.
#[tokio::test]
async fn refusals() {
    let hub = TestHub::start().await;
    let (mut w, sid, _) = session(&hub, moves(), "stopped").await;
    let before = projects(&hub).await.len();

    // Inside its own workspace: a folder belongs to one project, its subfolders too.
    let (s, v) = move_to(&hub, &mut w, &sid, json!({"path": "/w/hqc/sub"})).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some("workspace_conflict")),
        "{v}"
    );
    assert_eq!(v["existing_path"], HQC);
    // With `create`: refused before the worker is asked, so it makes no folder.
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/move"),
            json!({"path": "/w/hqc/missing", "create": true}),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some("workspace_conflict")),
        "{v}"
    );
    w.expect_silence(Duration::from_millis(300)).await;
    // Outside allowed_roots, and a missing folder without `create`.
    let (s, v) = move_to(&hub, &mut w, &sid, json!({"path": "/etc"})).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some("outside_allowed_roots")),
        "{v}"
    );
    let (s, v) = move_to(&hub, &mut w, &sid, json!({"path": "/w/missing"})).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some("path_not_found")),
        "{v}"
    );
    // An unknown project.
    let (s, v) = move_to(
        &hub,
        &mut w,
        &sid,
        json!({"path": "/w/foo", "project": "nope"}),
    )
    .await;
    assert_eq!(s, 404, "{v}");
    // A workspace of the owner that is not ready: a clone still being made, or one that failed.
    let pid = hub.personal_project().await;
    let (s, v) = hub.add_workspace(&mut w, &pid, "/w/two").await;
    assert_eq!(s, 201, "{v}");
    for state in ["creating", "failed"] {
        sqlx::query("UPDATE workspaces SET state = $2 WHERE path = $1")
            .bind("/w/two")
            .bind(state)
            .execute(&hub.app.db)
            .await
            .unwrap();
        let (s, v) = move_to(&hub, &mut w, &sid, json!({"path": "/w/two"})).await;
        assert_eq!(
            (s, v["error"].as_str()),
            (409, Some("workspace_not_ready")),
            "{state}: {v}"
        );
        // With `create`: refused before the worker is asked, so it makes no folder.
        let (s, v) = hub
            .post(
                &format!("/api/sessions/{sid}/move"),
                json!({"path": "/w/two", "create": true}),
            )
            .await;
        assert_eq!(
            (s, v["error"].as_str()),
            (409, Some("workspace_not_ready")),
            "{state}, create: {v}"
        );
        w.expect_silence(Duration::from_millis(300)).await;
    }

    // Driven from VS Code, or held open by a live process outside hqc (SPEC §9.3).
    let id: Uuid = sid.parse().unwrap();
    sqlx::query("UPDATE sessions SET locked_by = 'vscode' WHERE id = $1")
        .bind(id)
        .execute(&hub.app.db)
        .await
        .unwrap();
    let (s, v) = move_to(&hub, &mut w, &sid, json!({"path": "/w/foo"})).await;
    assert_eq!((s, v["error"].as_str()), (409, Some("unavailable")), "{v}");
    sqlx::query(
        "UPDATE sessions SET locked_by = 'none',
            held_by = '{\"entrypoint\": \"claude-vscode\", \"status\": \"idle\"}' WHERE id = $1",
    )
    .bind(id)
    .execute(&hub.app.db)
    .await
    .unwrap();
    let (s, v) = move_to(&hub, &mut w, &sid, json!({"path": "/w/foo"})).await;
    assert_eq!((s, v["error"].as_str()), (409, Some("unavailable")), "{v}");
    sqlx::query("UPDATE sessions SET held_by = NULL, subagents = 1 WHERE id = $1")
        .bind(id)
        .execute(&hub.app.db)
        .await
        .unwrap();
    // A subagent still running.
    let (s, v) = move_to(&hub, &mut w, &sid, json!({"path": "/w/foo"})).await;
    assert_eq!((s, v["error"].as_str()), (409, Some("unavailable")), "{v}");
    sqlx::query("UPDATE sessions SET subagents = 0 WHERE id = $1")
        .bind(id)
        .execute(&hub.app.db)
        .await
        .unwrap();
    // On a worker of another user: refused before the worker is asked.
    let me = hqc_hub::db::ensure_user(&hub.app.db, &hub.email)
        .await
        .unwrap();
    let stranger = hqc_hub::db::ensure_user(&hub.app.db, "stranger-move@example.com")
        .await
        .unwrap();
    let worker_id = w.worker_id.clone();
    let owner = |u: Uuid| {
        sqlx::query("UPDATE workers SET owner_id = $2 WHERE id = $1::uuid")
            .bind(worker_id.clone())
            .bind(u)
            .execute(&hub.app.db)
    };
    owner(stranger.id).await.unwrap();
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/move"),
            json!({"path": "/w/foo"}),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (409, Some("unavailable")), "{v}");
    w.expect_silence(Duration::from_millis(300)).await;
    assert!(moved_events(&hub.events(&sid).await).is_empty());
    owner(me.id).await.unwrap();
    // Archived, as for a takeover.
    let (s, _) = hub
        .post(&format!("/api/sessions/{sid}/archive"), json!({}))
        .await;
    assert_eq!(s, 200);
    let kill = w.expect("session.kill").await;
    w.reply(&kill["id"], "ack", json!({})).await;
    let (s, v) = move_to(&hub, &mut w, &sid, json!({"path": "/w/foo"})).await;
    assert_eq!((s, v["error"].as_str()), (409, Some("archived")), "{v}");
    let (s, _) = hub
        .post(&format!("/api/sessions/{sid}/unarchive"), json!({}))
        .await;
    assert_eq!(s, 200);

    assert_eq!(projects(&hub).await.len(), before);
    assert!(moved_events(&hub.events(&sid).await).is_empty());
    // With `create` the worker makes the missing folder.
    let (s, v) = move_to(
        &hub,
        &mut w,
        &sid,
        json!({"path": "/w/missing", "create": true}),
    )
    .await;
    assert_eq!(s, 200, "{v}");

    // Offline.
    w.close().await;
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/move"),
            json!({"path": "/w/foo"}),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some("worker_offline")),
        "{v}"
    );
    hub.finish().await;
}

/// The refusals run again on the session read anew after `fs.stat`: a holder outside hqc that
/// shows up during the worker's round-trip refuses the move, and nothing is made.
#[tokio::test]
async fn refused_on_the_session_read_again() {
    let hub = TestHub::start().await;
    let (mut w, sid, _) = session(&hub, moves(), "running").await;
    let before = projects(&hub).await.len();
    let id: Uuid = sid.parse().unwrap();
    let url = format!("/api/sessions/{sid}/move");
    let req = hub.post(&url, json!({"path": "/w/foo"}));
    let worker = async {
        let f = w.expect("fs.stat").await;
        sqlx::query(
            "UPDATE sessions SET
                held_by = '{\"entrypoint\": \"claude-vscode\", \"status\": \"idle\"}' WHERE id = $1",
        )
        .bind(id)
        .execute(&hub.app.db)
        .await
        .unwrap();
        let (ty, payload) = w.fs_reply(&f);
        w.reply(&f["id"], ty, payload).await;
    };
    let ((s, v), ()) = tokio::join!(req, worker);
    assert_eq!((s, v["error"].as_str()), (409, Some("unavailable")), "{v}");
    assert_eq!(projects(&hub).await.len(), before);
    let (_, l) = hub.get("/api/workspaces").await;
    let paths: Vec<Option<&str>> = l["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["path"].as_str())
        .collect();
    assert_eq!(paths, [Some(HQC)], "{l}");
    assert!(moved_events(&hub.events(&sid).await).is_empty());
    w.expect_silence(Duration::from_millis(300)).await;
    hub.finish().await;
}

/// A worker that does not report `claude_code.moves` would start the agent without its
/// transcript: `409 unavailable`.
#[tokio::test]
async fn an_old_worker_does_not_move() {
    let hub = TestHub::start().await;
    let (mut w, sid, _) = session(&hub, json!({}), "running").await;
    let (s, v) = move_to(&hub, &mut w, &sid, json!({"path": "/w/foo"})).await;
    assert_eq!((s, v["error"].as_str()), (409, Some("unavailable")), "{v}");
    assert_eq!(hub.session(&sid).await["workspace_id"], {
        let (_, l) = hub.get("/api/workspaces").await;
        l["workspaces"][0]["id"].clone()
    });
    hub.finish().await;
}

/// A stopped agent is not woken: nothing is queued and no `session.start` goes out; its next
/// input starts it in the new folder.
#[tokio::test]
async fn a_stopped_session_starts_there_with_its_next_input() {
    let hub = TestHub::start().await;
    let (mut w, sid, _) = session(&hub, moves(), "stopped").await;
    let (s, v) = move_to(&hub, &mut w, &sid, json!({"path": "/w/foo"})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["queue_len"], 0, "{v}");
    w.expect_silence(Duration::from_millis(300)).await;
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "go on"}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let start = w.expect("session.start").await;
    assert_eq!(start["payload"]["workspace_path"], "/w/foo");
    hub.finish().await;
}
