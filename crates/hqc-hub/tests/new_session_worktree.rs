//! SPEC 0.265: `POST /api/projects/{id}/sessions` with `isolation: "worktree"` starts the
//! owner's new session in a new ephemeral worktree of the workspace, by `hqc_spawn`'s path
//! (T47): the request waits until the worker made the worktree, a failed one leaves nothing
//! (`409 branch_exists` for a new branch that exists, so the client can offer the existing one),
//! and a new branch may start from a `base` on a worker that says it can.

mod common;

use common::*;
use serde_json::{json, Value};

const ROOT: &str = "/w";
const ORIGIN: &str = "/w/c6-missing";

/// A worker (with `worktree_base` when `base`) and the ready clone [`ORIGIN`], the personal
/// project's default workspace; the project's and the clone's ids.
async fn setup(hub: &TestHub, base: bool) -> (FakeWorker, String, String) {
    let (_, token) = hub.enroll("home-mac").await;
    let mut w = FakeWorker::connect(hub, &token).await.unwrap();
    let mut caps = json!({"claude_code": {"version": "2.1.307"}, "acp_agents": []});
    if base {
        caps["worktree_base"] = json!(true);
    }
    w.hello_with(ROOT, hub.boot_id, json!({"capabilities": caps}))
        .await;
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
    (w, pid, origin)
}

/// `POST .../sessions` with `body` while the worker answers the `fs.stat`s, the worktree
/// (`Ok(())`: made, `Err(e)`: fails with `e`) and the session's `session.start`: the answer and
/// the `workspace.create` frame, if one came.
async fn create(
    hub: &TestHub,
    w: &mut FakeWorker,
    pid: &str,
    body: Value,
    outcome: Result<(), &str>,
) -> ((u16, Value), Option<Value>) {
    let path = format!("/api/projects/{pid}/sessions");
    let req = hub.post(&path, body);
    tokio::pin!(req);
    let mut create = None;
    loop {
        tokio::select! {
            r = &mut req => return (r, create),
            Some(f) = w.recv_within(WAIT) => match f["type"].as_str().unwrap() {
                "fs.stat" => {
                    let (ty, payload) = w.fs_reply(&f);
                    w.reply(&f["id"], ty, payload).await;
                }
                "workspace.create" => {
                    w.reply(&f["id"], "ack", json!({})).await;
                    let p = &f["payload"];
                    let answer = match outcome {
                        Ok(()) => json!({"request_id": p["request_id"], "canonical": p["path"]}),
                        Err(e) => json!({"request_id": p["request_id"], "error": e}),
                    };
                    w.send("workspace.created", answer).await;
                    create = Some(f);
                }
                "session.start" => w.reply(&f["id"], "ack", json!({})).await,
                "ack" | "workspace.rules" | "mirror.set" => {}
                _ => panic!("unexpected frame {f}"),
            },
        }
    }
}

async fn counts(hub: &TestHub) -> (i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM sessions WHERE NOT system),
                (SELECT count(*) FROM workspaces WHERE ephemeral)",
    )
    .fetch_one(&hub.app.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_new_session_in_a_new_worktree() {
    let hub = TestHub::start().await;
    let (mut w, pid, origin) = setup(&hub, true).await;
    // The default branch is named after the new session, which takes that N.
    let ((s, v), f) = create(
        &hub,
        &mut w,
        &pid,
        json!({"workspace_id": origin, "isolation": "worktree", "permission_mode": "ask"}),
        Ok(()),
    )
    .await;
    assert_eq!(s, 201, "{v}");
    let f = f.expect("workspace.create");
    let ws_id = f["payload"]["request_id"].as_str().unwrap().to_string();
    assert_eq!(
        f["payload"],
        json!({"request_id": ws_id, "kind": "worktree", "path": "/w/c6-missing-hqc-session-1",
               "origin": ORIGIN, "branch": "hqc/session-1", "new_branch": true})
    );
    assert_eq!(
        (&v["workspace_id"], &v["title"], &v["slug"]),
        (&json!(ws_id), &json!("Session 1"), &json!("session-1"))
    );
    let ws = workspace(&hub, &ws_id).await;
    assert_eq!(
        (
            &ws["state"],
            &ws["ephemeral"],
            &ws["origin_workspace_id"],
            &ws["branch"]
        ),
        (
            &json!("ready"),
            &json!(true),
            &json!(origin),
            &json!("hqc/session-1")
        )
    );
    assert!(v["spawned_by"].is_null(), "the owner's own session: {v}");

    // A branch of the owner's, new from a base; the folder is named after the branch.
    let ((s, v), f) = create(
        &hub,
        &mut w,
        &pid,
        json!({"isolation": "worktree", "branch": "fix/backlight", "base": "origin/main"}),
        Ok(()),
    )
    .await;
    assert_eq!(s, 201, "{v}");
    let p = &f.unwrap()["payload"];
    assert_eq!(
        (&p["path"], &p["branch"], &p["new_branch"], &p["base"]),
        (
            &json!("/w/c6-missing-fix-backlight"),
            &json!("fix/backlight"),
            &json!(true),
            &json!("origin/main")
        )
    );
    assert_eq!(v["title"], "Session 2");
    hub.finish().await;
}

#[tokio::test]
async fn an_existing_branch_is_refused_then_checked_out() {
    let hub = TestHub::start().await;
    let (mut w, pid, origin) = setup(&hub, true).await;
    let before = counts(&hub).await;
    let exists = hqc_proto::frame::branch_exists("side");
    let ((s, v), f) = create(
        &hub,
        &mut w,
        &pid,
        json!({"workspace_id": origin, "isolation": "worktree", "branch": "side"}),
        Err(&exists),
    )
    .await;
    assert_eq!(s, 409, "{v}");
    assert_eq!(v["error"], "branch_exists", "{v}");
    assert!(v["message"].as_str().unwrap().contains(&exists), "{v}");
    let id = f.unwrap()["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(workspace(&hub, &id).await, Value::Null);
    assert_eq!(counts(&hub).await, before);

    // Another failure is `worktree_failed`.
    let ((s, v), _) = create(
        &hub,
        &mut w,
        &pid,
        json!({"isolation": "worktree", "branch": "other"}),
        Err("not the top folder of a git repository"),
    )
    .await;
    assert_eq!((s, &v["error"]), (409, &json!("worktree_failed")), "{v}");
    assert_eq!(counts(&hub).await, before);

    // The existing branch, checked out as it is: no `new_branch`, no base.
    let ((s, v), f) = create(
        &hub,
        &mut w,
        &pid,
        json!({"isolation": "worktree", "branch": "side", "existing_branch": true}),
        Ok(()),
    )
    .await;
    assert_eq!(s, 201, "{v}");
    let p = &f.unwrap()["payload"];
    assert_eq!(
        (&p["branch"], &p["new_branch"], &p["base"]),
        (&json!("side"), &Value::Null, &Value::Null)
    );
    hub.finish().await;
}

#[tokio::test]
async fn refusals_make_nothing() {
    let hub = TestHub::start().await;
    let (mut w, pid, _) = setup(&hub, false).await;
    let ((s, v), _) = create(&hub, &mut w, &pid, json!({"isolation": "worktree"}), Ok(())).await;
    assert_eq!(s, 201, "{v}");
    let worktree = v["workspace_id"].as_str().unwrap().to_string();
    let before = counts(&hub).await;
    let cases = [
        (json!({"isolation": "container"}), 400, "bad_request"),
        (json!({"branch": "x"}), 400, "bad_request"),
        (json!({"base": "main"}), 400, "bad_request"),
        (
            json!({"isolation": "worktree", "existing_branch": true}),
            400,
            "bad_request",
        ),
        (
            json!({"isolation": "worktree", "branch": "x", "base": "main", "existing_branch": true}),
            400,
            "bad_request",
        ),
        (
            json!({"isolation": "worktree", "branch": "a..b"}),
            400,
            "invalid_branch",
        ),
        (
            json!({"isolation": "worktree", "branch": "x", "base": "--orphan"}),
            400,
            "invalid_base",
        ),
        // One level of worktrees (T46): a worktree is no origin.
        (
            json!({"isolation": "worktree", "workspace_id": worktree}),
            400,
            "invalid_origin",
        ),
        // A worker without `worktree_base` would start from HEAD.
        (
            json!({"isolation": "worktree", "branch": "x", "base": "main"}),
            409,
            "worker_outdated",
        ),
        (
            json!({"isolation": "worktree", "model": "bad model"}),
            400,
            "bad_request",
        ),
    ];
    for (body, status, code) in cases {
        let ((s, v), f) = create(&hub, &mut w, &pid, body.clone(), Ok(())).await;
        assert_eq!(
            (s, v["error"].as_str()),
            (status, Some(code)),
            "{body}: {v}"
        );
        assert!(f.is_none(), "{body}");
    }
    assert_eq!(counts(&hub).await, before);
    hub.finish().await;
}
