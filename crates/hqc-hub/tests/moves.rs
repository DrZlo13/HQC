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

// ---------------------------------------------------------------- hqc_move (SPEC §7)

/// A session in [`HQC`] on a worker that moves (`hello` with [`moves`] and `extra`), its agent
/// running in `mode`, in the turn of a person's input: the worker, the session, its token and
/// that input's `event_id`.
async fn agent(hub: &TestHub, mode: &str, extra: Value) -> (FakeWorker, String, String, Value) {
    let (_, token) = hub.enroll("home-mac").await;
    let mut w = FakeWorker::connect(hub, &token).await.unwrap();
    let mut hello = moves();
    for (k, v) in extra.as_object().unwrap() {
        hello[k] = v.clone();
    }
    w.hello_with(ROOT, hub.boot_id, hello).await;
    let pid = hub.personal_project().await;
    let (s, v) = hub.add_workspace(&mut w, &pid, HQC).await;
    assert_eq!(s, 201, "{v}");
    let (s, v) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": "moving", "permission_mode": mode}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    let start = ack_start(&mut w, &sid).await;
    let id = w
        .send(
            "session.state",
            json!({"session_id": sid, "process_state": "running", "locked_by": "hqc",
                   "permission_mode": mode}),
        )
        .await;
    assert_eq!(w.expect("ack").await["id"], id.as_str());
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "move this into foo"}),
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
    let token = start["payload"]["session_token"]
        .as_str()
        .unwrap()
        .to_string();
    (w, sid, token, input["payload"]["event_id"].clone())
}

/// What the worker saw while one `hqc_move` ran.
struct Call {
    /// The `mcp.result` payload.
    result: Value,
    /// The `session.start`s it acked.
    starts: Vec<Value>,
    /// The `session.input`s it got.
    inputs: Vec<Value>,
    /// How many of them came before any `session.start`.
    early: usize,
    /// The `fs.stat`s it answered.
    stats: Vec<Value>,
}

/// `hqc_move` with `args` and `request_id` `rid` from the session of `token`, in the turn of
/// input `turn`, while the worker answers `fs.stat` (a folder with `denied` in its path it
/// cannot make) and acks `session.start`.
async fn call_move(w: &mut FakeWorker, token: &str, rid: &str, turn: &Value, args: Value) -> Call {
    let id = w
        .send(
            "mcp.call",
            json!({"request_id": rid, "session_token": token, "tool": "hqc_move",
                   "args": args, "turn_input": turn}),
        )
        .await;
    let (mut starts, mut inputs, mut early, mut stats) = (Vec::new(), Vec::new(), 0, Vec::new());
    loop {
        let f = w.recv().await;
        match f["type"].as_str().unwrap() {
            "mcp.result" => {
                assert_eq!(f["id"], id.as_str(), "{f}");
                assert_eq!(f["payload"]["request_id"], rid);
                return Call {
                    result: f["payload"].clone(),
                    starts,
                    inputs,
                    early,
                    stats,
                };
            }
            "fs.stat" if f["payload"]["path"].as_str().unwrap().contains("denied") => {
                let e = json!({"code": "invalid_frame", "retryable": false,
                               "message": "mkdir /w/denied: permission denied"});
                w.reply(&f["id"], "error", e).await;
                stats.push(f);
            }
            "fs.stat" => {
                let (ty, payload) = w.fs_reply(&f);
                w.reply(&f["id"], ty, payload).await;
                stats.push(f);
            }
            "session.start" => {
                w.reply(&f["id"], "ack", json!({})).await;
                starts.push(f);
            }
            "session.input" => {
                early += usize::from(starts.is_empty());
                inputs.push(f);
            }
            "ack" => {}
            _ => panic!("unexpected frame {f}"),
        }
    }
}

/// The feed's `user.message`s with `text`.
fn inputs_with<'a>(events: &'a [Value], text: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|e| e["type"] == "user.message" && e["payload"]["text"] == text)
        .collect()
}

/// The `approval.request`s of the feed.
fn move_requests(events: &[Value]) -> Vec<&Value> {
    events
        .iter()
        .filter(|e| e["type"] == "approval.request")
        .collect()
}

/// The session's PendingRequests of kind `move`.
async fn open_moves(hub: &TestHub, sid: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM pending_requests
         WHERE session_id = $1 AND kind = 'move' AND closed_at IS NULL",
    )
    .bind(sid.parse::<Uuid>().unwrap())
    .fetch_one(&hub.app.db)
    .await
    .unwrap()
}

/// The next `session.input`; an `mcp.result` sent again meanwhile is acked and skipped (an
/// answer and the `ack` of a `session.start` may both send it, SPEC §5.2).
async fn next_input(w: &mut FakeWorker) -> Value {
    loop {
        let f = w.recv().await;
        match f["type"].as_str().unwrap() {
            "session.input" => return f,
            "mcp.result" => w.reply(&f["id"], "ack", json!({})).await,
            _ => panic!("unexpected frame {f}"),
        }
    }
}

/// The wake input after a move (`c.inputs`, or the next one) checked: the owner's
/// `user.message` "you moved…" into `path`, without `peer`, in the call's chain moved on and
/// in the session's own mode (`auto` here: no `ask` cap), once, after `session_moved`.
async fn wake_input(hub: &TestHub, w: &mut FakeWorker, sid: &str, c: Call, path: &str) -> String {
    let input = match c.inputs.into_iter().next() {
        Some(f) => f,
        None => next_input(w).await,
    };
    let p = &input["payload"];
    assert_eq!(p["session_id"], sid);
    assert!(p["peer"].is_null(), "{p}");
    assert_ne!(p["permission_cap"], "ask", "{p}");
    let text = p["text"].as_str().unwrap().to_string();
    assert!(text.contains(path), "{text}");
    let events = hub.events(sid).await;
    let wake = inputs_with(&events, &text);
    assert_eq!(wake.len(), 1, "{events:?}");
    assert_eq!(wake[0]["event_id"], p["event_id"]);
    assert_eq!(wake[0]["author"]["kind"], "user");
    assert_eq!(wake[0]["chain"]["hops"], 1, "{}", wake[0]);
    assert!(wake[0]["seq"].as_i64() > moved_events(&events)[0]["seq"].as_i64());
    text
}

/// The agent moves its own session into a workspace of its owner in the environment of its
/// folder now: at once, nothing asked, and it learns its new address. Then its owner's input
/// "you moved…" waits in its queue, delivered after the new `session.start`. The same call
/// again answers the same and stores nothing.
#[tokio::test]
async fn the_agent_moves_itself() {
    let hub = TestHub::start().await;
    let (mut w, sid, token, turn) = agent(&hub, "auto", json!({})).await;
    let (s, other) = hub.post("/api/projects", json!({"name": "Other"})).await;
    assert_eq!(s, 201, "{other}");
    let (s, v) = hub
        .add_workspace(&mut w, other["id"].as_str().unwrap(), "/w/other")
        .await;
    assert_eq!(s, 201, "{v}");
    let before = projects(&hub).await.len();
    let rid = Uuid::now_v7().to_string();
    let args = json!({"path": "/w/other"});

    let c = call_move(&mut w, &token, &rid, &turn, args.clone()).await;
    let result = c.result["result"].clone();
    assert_eq!(
        result,
        json!({"address": "other/moving", "project": "other", "path": "/w/other"}),
        "{}",
        c.result
    );
    assert!(c.stats.iter().all(|f| f["payload"]["create"] != true));
    assert_eq!(projects(&hub).await.len(), before);
    assert_eq!(c.starts.len(), 1);
    assert_eq!(c.starts[0]["payload"]["workspace_path"], "/w/other");
    assert_eq!(c.early, 0);
    let text = wake_input(&hub, &mut w, &sid, c, "/w/other").await;
    assert!(move_requests(&hub.events(&sid).await).is_empty());
    assert_eq!(open_moves(&hub, &sid).await, 0);

    // A repeat (the answer lost, SPEC §5.2).
    let again = call_move(&mut w, &token, &rid, &turn, args).await;
    assert_eq!(again.result["result"], result);
    let events = hub.events(&sid).await;
    assert_eq!(inputs_with(&events, &text).len(), 1);
    assert_eq!(moved_events(&events).len(), 1);
    hub.finish().await;
}

/// `hqc_move` that waits for the owner: the call's frame gets `ack` (the worker answering
/// `fs.stat` meanwhile); the `fs.stat`s it answered.
async fn call_deferred(
    w: &mut FakeWorker,
    token: &str,
    rid: &str,
    turn: &Value,
    args: Value,
) -> Vec<Value> {
    let id = w
        .send(
            "mcp.call",
            json!({"request_id": rid, "session_token": token, "tool": "hqc_move",
                   "args": args, "turn_input": turn}),
        )
        .await;
    let mut stats = Vec::new();
    loop {
        let f = w.recv().await;
        match f["type"].as_str().unwrap() {
            "ack" if f["id"] == id.as_str() => return stats,
            "fs.stat" => {
                let (ty, payload) = w.fs_reply(&f);
                w.reply(&f["id"], ty, payload).await;
                stats.push(f);
            }
            "ack" => {}
            _ => panic!("unexpected frame {f}"),
        }
    }
}

/// The owner's answer `option` to the move request `rid` of session `sid`, then what the
/// worker gets until the call's own `mcp.result`, which it acks: it answers `fs.stat` and acks
/// `session.start` once the result is stored, so the result goes once, after that `ack`.
async fn answer_move(
    hub: &TestHub,
    w: &mut FakeWorker,
    sid: &str,
    rid: &str,
    option: &str,
) -> Call {
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/{rid}"),
            json!({"option_id": option}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (mut starts, mut inputs, mut early, mut stats) = (Vec::new(), Vec::new(), 0, Vec::new());
    loop {
        let f = w.recv().await;
        match f["type"].as_str().unwrap() {
            "mcp.result" => {
                assert_eq!(f["payload"]["request_id"], rid, "{f}");
                w.reply(&f["id"], "ack", json!({})).await;
                return Call {
                    result: f["payload"].clone(),
                    starts,
                    inputs,
                    early,
                    stats,
                };
            }
            "fs.stat" => {
                let (ty, payload) = w.fs_reply(&f);
                w.reply(&f["id"], ty, payload).await;
                stats.push(f);
            }
            "session.start" => {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                loop {
                    let stored: bool = sqlx::query_scalar(
                        "SELECT result IS NOT NULL FROM pending_requests
                         WHERE session_id = $1 AND kind = 'move' AND request_id = $2",
                    )
                    .bind(sid.parse::<Uuid>().unwrap())
                    .bind(rid)
                    .fetch_one(&hub.app.db)
                    .await
                    .unwrap();
                    if stored {
                        break;
                    }
                    assert!(tokio::time::Instant::now() < deadline, "no result stored");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                w.reply(&f["id"], "ack", json!({})).await;
                starts.push(f);
            }
            "session.input" => {
                early += usize::from(starts.is_empty());
                inputs.push(f);
            }
            "ack" => {}
            _ => panic!("unexpected frame {f}"),
        }
    }
}

/// Into a folder that is not there yet: the call waits for the owner, nothing is made. The
/// session's feed has the hub's `approval.request` (tool `hqc_move`, the path and why it
/// asks, `once` / `deny`), the Requests list has it as `move`, the session waits; a repeat of
/// the call waits too, without a second card; a text answer is refused. The owner allows it:
/// the folder is made, the session moves, the call gets its result as its own `mcp.result`,
/// and the wake input follows; a repeat now answers the same.
#[tokio::test]
async fn a_new_folder_waits_for_the_owner() {
    let hub = TestHub::start().await;
    let (mut w, sid, token, turn) = agent(&hub, "auto", json!({})).await;
    let before = projects(&hub).await.len();
    let rid = Uuid::now_v7().to_string();
    let args = json!({"path": "/w/missing-foo"});

    let stats = call_deferred(&mut w, &token, &rid, &turn, args.clone()).await;
    assert!(
        !stats.is_empty() && stats.iter().all(|f| f["payload"]["create"] != true),
        "{stats:?}"
    );
    let events = hub.events(&sid).await;
    let req = move_requests(&events);
    assert_eq!(req.len(), 1, "{events:?}");
    assert_eq!(req[0]["author"], json!({"kind": "system"}));
    let p = &req[0]["payload"];
    assert_eq!(
        (&p["request_id"], &p["tool"]),
        (&json!(rid), &json!("hqc_move"))
    );
    let d = p["description"].as_str().unwrap();
    assert!(
        d.contains("/w/missing-foo") && d.contains("new folder"),
        "{d}"
    );
    let options: Vec<&str> = p["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    assert_eq!(options, ["once", "deny"]);
    let (s, list) = hub.get("/api/requests").await;
    assert_eq!(s, 200, "{list}");
    let r = &list["requests"].as_array().unwrap()[0];
    assert_eq!(
        (&r["kind"], &r["request_id"]),
        (&json!("move"), &json!(rid))
    );
    assert_eq!(hub.session(&sid).await["state"], "waiting");
    assert_eq!(projects(&hub).await.len(), before);

    // A repeat while it waits (the `ack` lost, SPEC §5.2).
    let again = call_deferred(&mut w, &token, &rid, &turn, args.clone()).await;
    assert!(again.is_empty(), "{again:?}");
    assert_eq!(move_requests(&hub.events(&sid).await).len(), 1);
    // No text with the answer.
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/{rid}"),
            json!({"option_id": "deny", "message": "stay"}),
        )
        .await;
    assert_eq!(s, 400, "{v}");

    let c = answer_move(&hub, &mut w, &sid, &rid, "once").await;
    let result = c.result["result"].clone();
    assert_eq!(
        result,
        json!({"address": "missing-foo/moving", "project": "missing-foo", "path": "/w/missing-foo"}),
        "{}",
        c.result
    );
    assert!(c.stats.iter().any(|f| f["payload"]["create"] == true));
    assert_eq!(projects(&hub).await.len(), before + 1);
    assert_eq!(c.starts.len(), 1);
    assert_eq!(c.starts[0]["payload"]["workspace_path"], "/w/missing-foo");
    assert_eq!(c.early, 0);
    let text = wake_input(&hub, &mut w, &sid, c, "/w/missing-foo").await;
    let events = hub.events(&sid).await;
    let answer: Vec<&Value> = events
        .iter()
        .filter(|e| e["type"] == "approval.response")
        .collect();
    assert_eq!(answer.len(), 1, "{events:?}");
    assert_eq!(answer[0]["payload"]["option_id"], "once");
    assert_eq!(answer[0]["author"]["kind"], "user");
    let (_, list) = hub.get("/api/requests").await;
    assert!(list["requests"].as_array().unwrap().is_empty(), "{list}");

    // A repeat now: the stored result, nothing new.
    let again = call_move(&mut w, &token, &rid, &turn, args).await;
    assert_eq!(again.result["result"], result);
    assert!(again.stats.is_empty());
    let events = hub.events(&sid).await;
    assert_eq!(inputs_with(&events, &text).len(), 1);
    assert_eq!(moved_events(&events).len(), 1);
    hub.finish().await;
}

/// The owner denies the move: the call gets an error, nothing is made and nothing moves.
#[tokio::test]
async fn the_owner_denies_a_move() {
    let hub = TestHub::start().await;
    let (mut w, sid, token, turn) = agent(&hub, "auto", json!({})).await;
    let before = projects(&hub).await.len();
    let workspace = hub.session(&sid).await["workspace_id"].clone();
    let rid = Uuid::now_v7().to_string();
    call_deferred(
        &mut w,
        &token,
        &rid,
        &turn,
        json!({"path": "/w/missing-bar"}),
    )
    .await;

    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/{rid}"),
            json!({"option_id": "deny"}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let f = w.expect("mcp.result").await;
    w.reply(&f["id"], "ack", json!({})).await;
    let e = &f["payload"]["error"];
    assert_eq!(e["code"], "unavailable", "{f}");
    assert!(
        e["message"].as_str().unwrap().contains("did not allow"),
        "{f}"
    );
    w.expect_silence(Duration::from_millis(300)).await;
    assert_eq!(projects(&hub).await.len(), before);
    assert_eq!(hub.session(&sid).await["workspace_id"], workspace);
    let events = hub.events(&sid).await;
    assert!(moved_events(&events).is_empty());
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "user.message")
            .count(),
        1
    );
    assert_eq!(open_moves(&hub, &sid).await, 0);
    hub.finish().await;
}

/// An existing folder that is no workspace yet asks too; allowed, a move the core refuses
/// (inside the session's own workspace) gives the call that error.
#[tokio::test]
async fn an_allowed_move_can_fail() {
    let hub = TestHub::start().await;
    let (mut w, sid, token, turn) = agent(&hub, "auto", json!({})).await;
    let before = projects(&hub).await.len();
    let rid = Uuid::now_v7().to_string();
    call_deferred(&mut w, &token, &rid, &turn, json!({"path": "/w/hqc/sub"})).await;
    let events = hub.events(&sid).await;
    let d = move_requests(&events)[0]["payload"]["description"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        d.contains("not one of your workspaces") && d.contains("/w/hqc/sub"),
        "{d}"
    );

    let c = answer_move(&hub, &mut w, &sid, &rid, "once").await;
    let e = &c.result["error"];
    assert_eq!(e["code"], "invalid_args", "{}", c.result);
    assert!(e["message"]
        .as_str()
        .unwrap()
        .contains("workspace_conflict"));
    assert!(c.starts.is_empty() && c.inputs.is_empty());
    assert_eq!(projects(&hub).await.len(), before);
    assert!(moved_events(&hub.events(&sid).await).is_empty());
    hub.finish().await;
}

/// A workspace of the owner bound to another environment asks, naming both environments.
#[tokio::test]
async fn another_environment_asks() {
    let hub = TestHub::start().await;
    let bindings = json!({"environments": ["default", "prod"],
                          "env_bindings": [{"path": "/w/prod", "environment": "prod"}]});
    let (mut w, sid, token, turn) = agent(&hub, "auto", bindings).await;
    let pid = hub.personal_project().await;
    let (s, v) = hub.add_workspace(&mut w, &pid, "/w/prod/app").await;
    assert_eq!(s, 201, "{v}");
    let workspace = hub.session(&sid).await["workspace_id"].clone();
    let rid = Uuid::now_v7().to_string();
    let stats = call_deferred(&mut w, &token, &rid, &turn, json!({"path": "/w/prod/app"})).await;
    assert!(stats.iter().all(|f| f["payload"]["create"] != true));
    let events = hub.events(&sid).await;
    let d = move_requests(&events)[0]["payload"]["description"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        d.contains("your workspace") && d.contains("prod") && d.contains("default"),
        "{d}"
    );
    assert_eq!(open_moves(&hub, &sid).await, 1);
    assert_eq!(hub.session(&sid).await["workspace_id"], workspace);
    hub.finish().await;
}

/// Only the session owner answers a move of it: anyone else is `403` and the request stays
/// open. (Over HTTP another user does not see the session at all, `404`, SPEC §11.)
#[tokio::test]
async fn only_the_owner_answers_a_move() {
    let hub = TestHub::start().await;
    let (mut w, sid, token, turn) = agent(&hub, "auto", json!({})).await;
    let rid = Uuid::now_v7().to_string();
    call_deferred(
        &mut w,
        &token,
        &rid,
        &turn,
        json!({"path": "/w/missing-baz"}),
    )
    .await;

    let other = hqc_hub::db::ensure_user(&hub.app.db, "other@example.com")
        .await
        .unwrap();
    let user = hqc_hub::auth::CurrentUser {
        id: other.id,
        email: other.email,
        display_name: other.display_name,
        timezone: other.timezone,
        personal_project_id: other.personal_project_id.unwrap(),
        theme: Default::default(),
    };
    let s = hqc_hub::db::session(&hub.app.db, sid.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    let body = hqc_hub::requests::AnswerBody {
        option_id: Some("once".into()),
        answers: None,
        message: None,
    };
    let r = hqc_hub::requests::answer(&hub.app, &user, &s, &rid, body).await;
    assert!(
        matches!(r, Err(hqc_hub::error::ApiError::Forbidden(_))),
        "{r:?}"
    );
    w.expect_silence(Duration::from_millis(300)).await;
    assert_eq!(open_moves(&hub, &sid).await, 1);
    assert_eq!(hub.session(&sid).await["state"], "waiting");
    hub.finish().await;
}

/// `POST /stop` cancels an open move request as it does an approval: `approval.cancel
/// {stopped}` in the feed, an error to the call, then `session.stop`; nothing is made.
#[tokio::test]
async fn stop_cancels_a_move_request() {
    let hub = TestHub::start().await;
    let (mut w, sid, token, turn) = agent(&hub, "auto", json!({})).await;
    let before = projects(&hub).await.len();
    let rid = Uuid::now_v7().to_string();
    call_deferred(
        &mut w,
        &token,
        &rid,
        &turn,
        json!({"path": "/w/missing-qux"}),
    )
    .await;

    let (s, v) = hub
        .post(&format!("/api/sessions/{sid}/stop"), json!({}))
        .await;
    assert_eq!(s, 200, "{v}");
    let f = w.expect("mcp.result").await;
    w.reply(&f["id"], "ack", json!({})).await;
    assert_eq!(f["payload"]["request_id"], rid.as_str());
    let e = &f["payload"]["error"];
    assert_eq!(e["code"], "unavailable", "{f}");
    assert!(e["message"].as_str().unwrap().contains("cancelled"), "{f}");
    let stop = w.expect("session.stop").await;
    w.reply(&stop["id"], "ack", json!({})).await;
    let events = hub.events(&sid).await;
    let cancel: Vec<&Value> = events
        .iter()
        .filter(|e| e["type"] == "approval.cancel")
        .collect();
    assert_eq!(cancel.len(), 1, "{events:?}");
    assert_eq!(
        cancel[0]["payload"],
        json!({"request_id": rid, "reason": "stopped"})
    );
    assert_eq!(open_moves(&hub, &sid).await, 0);
    assert_eq!(projects(&hub).await.len(), before);
    assert!(moved_events(&events).is_empty());
    hub.finish().await;
}

/// The error code and message of an `hqc_move` that is refused before anything starts.
async fn refusal(w: &mut FakeWorker, token: &str, turn: &Value, args: Value) -> (String, String) {
    let c = call_move(w, token, &Uuid::now_v7().to_string(), turn, args).await;
    assert!(c.result.get("result").is_none(), "{}", c.result);
    assert!(c.starts.is_empty() && c.inputs.is_empty());
    let e = &c.result["error"];
    (
        e["code"].as_str().unwrap().to_string(),
        e["message"].as_str().unwrap().to_string(),
    )
}

/// What `hqc_move` refuses, with the codes of SPEC §7: nothing moves, nothing is queued,
/// nothing is asked.
#[tokio::test]
async fn the_tool_refuses() {
    let hub = TestHub::start().await;
    let (mut w, sid, token, turn) = agent(&hub, "auto", json!({})).await;
    // A workspace of the owner in the same environment: the move goes at once, no request.
    let pid = hub.personal_project().await;
    let (s, v) = hub.add_workspace(&mut w, &pid, "/w/two").await;
    assert_eq!(s, 201, "{v}");
    let before = projects(&hub).await.len();
    let workspace = hub.session(&sid).await["workspace_id"].clone();

    // Outside allowed_roots, or a folder the worker cannot stat: its reason.
    let (code, m) = refusal(&mut w, &token, &turn, json!({"path": "/etc"})).await;
    assert_eq!(code, "invalid_args");
    assert!(m.contains("allowed_roots"), "{m}");
    let (code, m) = refusal(&mut w, &token, &turn, json!({"path": "/w/denied"})).await;
    assert_eq!(code, "invalid_args");
    assert!(m.contains("permission denied"), "{m}");
    // An unknown project.
    let (code, _) = refusal(
        &mut w,
        &token,
        &turn,
        json!({"path": "/w/two", "project": "nope"}),
    )
    .await;
    assert_eq!(code, "not_found");

    // A turn capped to `ask` by another agent's chain (SPEC §7.1): refused before `fs.stat`.
    let id: Uuid = sid.parse().unwrap();
    sqlx::query("UPDATE sessions SET permission_capped_by = 'p/other' WHERE id = $1")
        .bind(id)
        .execute(&hub.app.db)
        .await
        .unwrap();
    let c = call_move(
        &mut w,
        &token,
        &Uuid::now_v7().to_string(),
        &turn,
        json!({"path": "/w/two"}),
    )
    .await;
    assert!(c.stats.is_empty() && c.starts.is_empty(), "{}", c.result);
    assert_eq!(c.result["error"]["code"], "unavailable", "{}", c.result);
    assert!(c.result["error"]["message"]
        .as_str()
        .unwrap()
        .contains("p/other"));
    sqlx::query("UPDATE sessions SET permission_capped_by = NULL WHERE id = $1")
        .bind(id)
        .execute(&hub.app.db)
        .await
        .unwrap();

    // A chain at its hops limit moves nothing (SPEC §7.1).
    let set_chain = |chain: Option<Value>| {
        let (db, ev) = (
            hub.app.db.clone(),
            turn.as_str().unwrap().parse::<Uuid>().unwrap(),
        );
        async move {
            sqlx::query("UPDATE events SET chain = $2 WHERE event_id = $1")
                .bind(ev)
                .bind(chain)
                .execute(&db)
                .await
                .unwrap();
        }
    };
    set_chain(Some(
        json!({"chain_id": Uuid::now_v7(), "depth": 0, "hops": 20}),
    ))
    .await;
    let (code, _) = refusal(&mut w, &token, &turn, json!({"path": "/w/foo"})).await;
    assert_eq!(code, "limit_exceeded");
    set_chain(None).await;

    // A subagent still running: refused by the move itself, nothing queued.
    sqlx::query("UPDATE sessions SET subagents = 1 WHERE id = $1")
        .bind(id)
        .execute(&hub.app.db)
        .await
        .unwrap();
    let (code, m) = refusal(&mut w, &token, &turn, json!({"path": "/w/two"})).await;
    assert_eq!(code, "unavailable");
    assert!(m.contains("subagent"), "{m}");
    sqlx::query("UPDATE sessions SET subagents = 0 WHERE id = $1")
        .bind(id)
        .execute(&hub.app.db)
        .await
        .unwrap();

    // Not run by hqc now, or driven from VS Code.
    for locked in ["none", "vscode"] {
        sqlx::query("UPDATE sessions SET locked_by = $2 WHERE id = $1")
            .bind(id)
            .bind(locked)
            .execute(&hub.app.db)
            .await
            .unwrap();
        let (code, _) = refusal(&mut w, &token, &turn, json!({"path": "/w/foo"})).await;
        assert_eq!(code, "unavailable", "{locked}");
    }

    assert_eq!(projects(&hub).await.len(), before);
    assert_eq!(hub.session(&sid).await["workspace_id"], workspace);
    let events = hub.events(&sid).await;
    assert!(moved_events(&events).is_empty());
    assert!(move_requests(&events).is_empty());
    let inputs = events
        .iter()
        .filter(|e| e["type"] == "user.message")
        .count();
    assert_eq!(inputs, 1, "only the person's: {events:?}");
    hub.finish().await;
}
