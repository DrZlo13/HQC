//! T47: ephemeral worktrees for spawned sessions (SPEC §3 Workspace, §7, §13.2, §17.4).
//! `hqc_spawn` with `isolation: "worktree"` makes a worktree of the target workspace through
//! T46's flow, waits for it, and starts the session there; a failed worktree fails the call
//! and leaves nothing. Archiving the last session of an ephemeral workspace marks it for
//! deletion; unarchiving clears the mark.

mod common;

use common::*;
use hqc_hub::config::Limits;
use serde_json::{json, Value};
use uuid::Uuid;

const ROOT: &str = "/w";
const ORIGIN: &str = "/w/c6-missing";

/// A worker with the ready clone [`ORIGIN`] (the personal project's default workspace) and a
/// session started there; the clone's id, the session's id and token.
async fn setup(hub: &TestHub) -> (FakeWorker, String, String, String) {
    let (_, token) = hub.enroll("home-mac").await;
    let mut w = FakeWorker::connect(hub, &token).await.unwrap();
    w.hello(ROOT, hub.boot_id).await;
    let pid = hub.personal_project().await;
    let body = json!({"kind": "clone", "project_id": pid, "worker_id": w.worker_id,
                      "path": ORIGIN, "url": "git@github.com:drzlo/c6-displays.git"});
    let (s, v) = hub
        .while_worker_answers(&mut w, hub.post("/api/workspaces", body))
        .await;
    assert_eq!(s, 201, "{v}");
    let origin = v["id"].as_str().unwrap().to_string();
    create_frame(&mut w, &origin).await;
    created(&mut w, json!({"request_id": origin, "canonical": ORIGIN})).await;
    let (s, v) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": "lead", "workspace_id": origin}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    let f = ack_start(&mut w, &sid).await;
    let t = f["payload"]["session_token"].as_str().unwrap().to_string();
    (w, origin, sid, t)
}

fn call(token: &str, rid: &str, args: Value) -> Value {
    json!({"request_id": rid, "session_token": token, "tool": "hqc_spawn", "args": args})
}

/// How the fake worker answers the `workspace.create` of a spawn's worktree.
enum Outcome {
    Made,
    Fails(&'static str),
}

/// `hqc_spawn` with `request_id` `rid` while the worker answers the `fs.stat`s, the worktree
/// (`outcome`) and the new session's `session.start`: the `mcp.result` payload and the
/// `workspace.create` frame, if one came.
async fn spawn_with(
    w: &mut FakeWorker,
    token: &str,
    rid: &str,
    args: Value,
    outcome: Outcome,
) -> (Value, Option<Value>) {
    let id = w.send("mcp.call", call(token, rid, args)).await;
    let mut create = None;
    loop {
        let f = w.recv().await;
        match f["type"].as_str().unwrap() {
            "mcp.result" => {
                assert_eq!(f["id"], id.as_str(), "{f}");
                assert_eq!(f["payload"]["request_id"], rid);
                return (f["payload"].clone(), create);
            }
            "fs.stat" => {
                let (ty, payload) = w.fs_reply(&f);
                w.reply(&f["id"], ty, payload).await;
            }
            "workspace.create" => {
                w.reply(&f["id"], "ack", json!({})).await;
                let p = &f["payload"];
                let answer = match outcome {
                    Outcome::Made => json!({"request_id": p["request_id"], "canonical": p["path"]}),
                    Outcome::Fails(e) => json!({"request_id": p["request_id"], "error": e}),
                };
                w.send("workspace.created", answer).await;
                create = Some(f);
            }
            "session.start" => w.reply(&f["id"], "ack", json!({})).await,
            "ack" | "session.input" => {}
            _ => panic!("unexpected frame {f}"),
        }
    }
}

async fn spawn(
    w: &mut FakeWorker,
    token: &str,
    args: Value,
    outcome: Outcome,
) -> (Value, Option<Value>) {
    spawn_with(w, token, &Uuid::now_v7().to_string(), args, outcome).await
}

async fn session_count(hub: &TestHub) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM sessions WHERE NOT system")
        .fetch_one(&hub.app.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn spawn_in_a_worktree_and_the_mark_for_deletion() {
    let hub = TestHub::start().await;
    let (mut w, origin, lead, token) = setup(&hub).await;
    let args =
        json!({"project": "personal", "prompt": "fix the backlight", "isolation": "worktree"});
    let rid = Uuid::now_v7().to_string();
    let (r, create) = spawn_with(&mut w, &token, &rid, args.clone(), Outcome::Made).await;
    assert!(r.get("error").is_none(), "{r}");
    // The branch is named after the new session's slug, the folder after the branch (T46).
    assert_eq!(
        r["result"],
        json!({"address": "personal/session-2",
               "worktree": {"path": "/w/c6-missing-hqc-session-2", "branch": "hqc/session-2"}})
    );
    let create = create.expect("workspace.create");
    let ws_id = create["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        create["payload"],
        json!({"request_id": ws_id, "kind": "worktree", "path": "/w/c6-missing-hqc-session-2",
               "origin": ORIGIN, "branch": "hqc/session-2", "new_branch": true})
    );
    let ws = workspace(&hub, &ws_id).await;
    assert_eq!(
        (
            &ws["kind"],
            &ws["state"],
            &ws["ephemeral"],
            &ws["marked_for_deletion"],
            &ws["origin_workspace_id"],
            &ws["created_by_hqc"],
            &ws["mirror_enabled"]
        ),
        (
            &json!("worktree"),
            &json!("ready"),
            &json!(true),
            &json!(false),
            &json!(origin),
            &json!(true),
            &json!(false)
        )
    );
    // The session works in the worktree and knows its spawner; the prompt is its first input.
    let sid: Uuid = sqlx::query_scalar("SELECT id FROM sessions WHERE slug = 'session-2'")
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
    let sid = sid.to_string();
    let s = hub.session(&sid).await;
    assert_eq!(
        (&s["workspace_id"], &s["title"]),
        (&json!(ws_id), &json!("Session 2"))
    );
    assert_eq!(s["spawned_by"]["session_id"], lead.as_str());
    let first = hub.events(&sid).await;
    assert!(
        first
            .iter()
            .any(|e| e["type"] == "peer.message" && e["payload"]["text"] == "fix the backlight"),
        "{first:?}"
    );

    // A repeat of the call answers the same at once and makes nothing.
    let id = w.send("mcp.call", call(&token, &rid, args)).await;
    let f = w.expect("mcp.result").await;
    assert_eq!(
        (f["id"].as_str(), &f["payload"]["result"]),
        (Some(id.as_str()), &r["result"])
    );
    w.expect_silence(std::time::Duration::from_millis(300))
        .await;

    // Archiving its last session marks the worktree; unarchiving clears the mark. Nothing is
    // deleted on the worker.
    let (s, v) = hub
        .post(&format!("/api/sessions/{sid}/archive"), json!({}))
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(workspace(&hub, &ws_id).await["marked_for_deletion"], true);
    assert_eq!(workspace(&hub, &origin).await["marked_for_deletion"], false);
    let (s, v) = hub
        .post(&format!("/api/sessions/{sid}/unarchive"), json!({}))
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(workspace(&hub, &ws_id).await["marked_for_deletion"], false);
    // A session the user starts there keeps it unmarked while the spawned one is archived.
    let pid = hub.personal_project().await;
    let (s, other) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": "also", "workspace_id": ws_id}),
        )
        .await;
    assert_eq!(s, 201, "{other}");
    hub.post(&format!("/api/sessions/{sid}/archive"), json!({}))
        .await;
    assert_eq!(workspace(&hub, &ws_id).await["marked_for_deletion"], false);
    let other = other["id"].as_str().unwrap();
    hub.post(&format!("/api/sessions/{other}/archive"), json!({}))
        .await;
    assert_eq!(workspace(&hub, &ws_id).await["marked_for_deletion"], true);

    // Deleting it is T45's deletion: the worker removes the worktree.
    let (status, f) = while_answering(
        &mut w,
        "workspace.delete",
        json!({"removed": true}),
        hub.delete(&format!("/api/workspaces/{ws_id}?remove_folder=true")),
    )
    .await;
    assert_eq!(status, 204);
    assert_eq!(f["payload"]["path"], "/w/c6-missing-hqc-session-2");
    assert_eq!(workspace(&hub, &ws_id).await, Value::Null);
    hub.finish().await;
}

#[tokio::test]
async fn a_failed_worktree_fails_the_call_and_leaves_nothing() {
    let hub = TestHub::start().await;
    let (mut w, _, _, token) = setup(&hub).await;
    let before = session_count(&hub).await;
    let (r, create) = spawn(
        &mut w,
        &token,
        json!({"project": "personal", "prompt": "go", "isolation": "worktree", "branch": "feature/x"}),
        Outcome::Fails("a branch named feature/x exists already"),
    )
    .await;
    assert_eq!(r["error"]["code"], "unavailable", "{r}");
    let m = r["error"]["message"].as_str().unwrap();
    assert!(m.contains("a branch named feature/x exists already"), "{m}");
    // An explicit branch, new, and its default folder.
    let create = create.unwrap();
    assert_eq!(
        (
            &create["payload"]["branch"],
            &create["payload"]["path"],
            &create["payload"]["new_branch"]
        ),
        (
            &json!("feature/x"),
            &json!("/w/c6-missing-feature-x"),
            &json!(true)
        )
    );
    let id = create["payload"]["request_id"].as_str().unwrap();
    assert_eq!(workspace(&hub, id).await, Value::Null);
    assert_eq!(session_count(&hub).await, before);
    hub.finish().await;
}

#[tokio::test]
async fn refusals_store_and_send_nothing() {
    let hub = TestHub::start().await;
    let (mut w, _, _, token) = setup(&hub).await;
    let (r, _) = spawn(
        &mut w,
        &token,
        json!({"project": "personal", "prompt": "go", "isolation": "worktree"}),
        Outcome::Made,
    )
    .await;
    let worktree = r["result"]["worktree"]["path"]
        .as_str()
        .unwrap()
        .to_string();
    let before = session_count(&hub).await;
    let cases = [
        json!({"project": "personal", "prompt": "go", "isolation": "container"}),
        json!({"project": "personal", "prompt": "go", "branch": "x"}),
        // One level of worktrees (T46): a worktree is no origin.
        json!({"project": "personal", "prompt": "go", "isolation": "worktree", "workspace": worktree}),
        json!({"project": "personal", "prompt": "go", "isolation": "worktree", "branch": "a..b"}),
        json!({"project": "personal", "prompt": "go", "isolation": "worktree", "branch": ""}),
    ];
    for args in cases {
        let (r, create) = spawn(&mut w, &token, args.clone(), Outcome::Made).await;
        assert_eq!(r["error"]["code"], "invalid_args", "{args}: {r}");
        assert!(create.is_none(), "{args}");
    }
    let (r, _) = spawn(
        &mut w,
        &token,
        json!({"project": "personal", "prompt": "go", "isolation": "worktree", "workspace": "nope"}),
        Outcome::Made,
    )
    .await;
    assert_eq!(r["error"]["code"], "not_found", "{r}");
    assert_eq!(session_count(&hub).await, before);
    let ephemeral: i64 = sqlx::query_scalar("SELECT count(*) FROM workspaces WHERE ephemeral")
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
    assert_eq!(ephemeral, 1);
    hub.finish().await;
}

/// A worktree being made counts as a spawn, the connection keeps reading meanwhile, and
/// parallel spawns get their own branches.
#[tokio::test]
async fn worktrees_being_made_count_against_the_limits() {
    let hub = TestHub::start_limited(Limits {
        spawns_per_hour: 2,
        ..Default::default()
    })
    .await;
    let (mut w, _, _, token) = setup(&hub).await;
    let isolated = json!({"project": "personal", "prompt": "go", "isolation": "worktree"});
    let (a, b) = (Uuid::now_v7().to_string(), Uuid::now_v7().to_string());
    w.send("mcp.call", call(&token, &a, isolated.clone())).await;
    w.send("mcp.call", call(&token, &b, isolated.clone())).await;
    let mut creates = Vec::new();
    while creates.len() < 2 {
        let f = w.recv().await;
        match f["type"].as_str().unwrap() {
            "fs.stat" => {
                let (ty, payload) = w.fs_reply(&f);
                w.reply(&f["id"], ty, payload).await;
            }
            "workspace.create" => {
                w.reply(&f["id"], "ack", json!({})).await;
                creates.push(f);
            }
            _ => panic!("unexpected frame {f}"),
        }
    }
    let mut branches: Vec<&str> = creates
        .iter()
        .map(|f| f["payload"]["branch"].as_str().unwrap())
        .collect();
    branches.sort();
    assert_eq!(branches, ["hqc/session-2", "hqc/session-3"]);

    // Both worktrees are still being made: a third spawn is over the limit, and it is
    // answered while the other two wait.
    let id = w
        .send(
            "mcp.call",
            call(
                &token,
                &Uuid::now_v7().to_string(),
                json!({"project": "personal", "prompt": "go"}),
            ),
        )
        .await;
    let f = w.expect("mcp.result").await;
    assert_eq!(f["id"], id.as_str());
    assert_eq!(f["payload"]["error"]["code"], "limit_exceeded", "{f}");
    assert!(f["payload"]["error"]["message"]
        .as_str()
        .unwrap()
        .contains("hour"));

    // The worker answers both; each call gets its session.
    for c in &creates {
        let p = &c["payload"];
        w.send(
            "workspace.created",
            json!({"request_id": p["request_id"], "canonical": p["path"]}),
        )
        .await;
    }
    let mut results = Vec::new();
    while results.len() < 2 {
        let f = w.recv().await;
        match f["type"].as_str().unwrap() {
            "session.start" => w.reply(&f["id"], "ack", json!({})).await,
            "session.input" | "ack" => {}
            "mcp.result" => results.push(f["payload"].clone()),
            _ => panic!("unexpected frame {f}"),
        }
    }
    for r in results {
        let addr = r["result"]["address"].as_str().unwrap();
        let branch = r["result"]["worktree"]["branch"].as_str().unwrap();
        assert_eq!(
            format!("hqc/{}", addr.strip_prefix("personal/").unwrap()),
            branch
        );
    }
    hub.finish().await;
}
