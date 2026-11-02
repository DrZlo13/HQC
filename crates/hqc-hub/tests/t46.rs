//! T46: worktree workspaces (SPEC §3 Workspace, §5.3, §6, §13). A worktree is made from an
//! origin, a ready workspace of the same project and worker that is not a worktree itself; it
//! goes through T45's `creating` → `ready` | `failed` with `workspace.create {kind: worktree}`,
//! the origin's `workspace.rules` sent first. An origin is not deleted while worktrees of it
//! exist; a worktree is deleted with `force` only when the user confirmed dirty files.

mod common;

use common::*;
use serde_json::{json, Value};

const ROOT: &str = "/w";
const URL: &str = "git@github.com:drzlo/c6-displays.git";

/// A ready clone at `path` (it must contain `missing`, so the fake worker says it is new).
async fn ready_clone(hub: &TestHub, w: &mut FakeWorker, pid: &str, path: &str) -> String {
    let body = json!({"kind": "clone", "project_id": pid, "worker_id": w.worker_id,
                      "path": path, "url": URL, "branch": "main"});
    let (s, v) = hub
        .while_worker_answers(w, hub.post("/api/workspaces", body))
        .await;
    assert_eq!(s, 201, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    create_frame(w, &id).await;
    created(w, json!({"request_id": id, "canonical": path})).await;
    id
}

/// `POST /api/workspaces {kind: worktree, ...}` plus `extra`, while the worker answers the
/// `fs.stat`s.
async fn worktree(
    hub: &TestHub,
    w: &mut FakeWorker,
    pid: &str,
    origin: &str,
    branch: &str,
    extra: Value,
) -> (u16, Value) {
    let mut body = json!({"kind": "worktree", "project_id": pid, "worker_id": w.worker_id,
                          "origin_workspace_id": origin, "branch": branch, "new_branch": true});
    for (k, v) in extra.as_object().unwrap() {
        body[k] = v.clone();
    }
    hub.while_worker_answers(w, hub.post("/api/workspaces", body))
        .await
}

#[tokio::test]
async fn worktree_of_a_workspace() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let mut w = FakeWorker::connect(&hub, &token).await.unwrap();
    w.hello(ROOT, hub.boot_id).await;
    let pid = hub.personal_project().await;
    let c6 = ready_clone(&hub, &mut w, &pid, "/w/c6-missing").await;
    // T46 shows the clone's branch too (`clone_branch` became `branch`, migration 0029).
    assert_eq!(workspace(&hub, &c6).await["branch"], "main");

    // The path defaults to `<origin-parent>/<origin-name>-<branch>`.
    let (s, v) = worktree(&hub, &mut w, &pid, &c6, "hqc/fix", json!({})).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(
        (
            &v["kind"],
            &v["state"],
            &v["created_by_hqc"],
            &v["origin_workspace_id"],
            &v["branch"],
            &v["path"]
        ),
        (
            &json!("worktree"),
            &json!("creating"),
            &json!(true),
            &json!(c6),
            &json!("hqc/fix"),
            &json!("/w/c6-missing-hqc-fix")
        )
    );
    let id = v["id"].as_str().unwrap().to_string();
    let rules_before = w.rules.len();
    let f = create_frame(&mut w, &id).await;
    assert_eq!(
        f["payload"],
        json!({"request_id": id, "kind": "worktree", "path": "/w/c6-missing-hqc-fix",
               "origin": "/w/c6-missing", "branch": "hqc/fix", "new_branch": true})
    );
    // The origin's rules went first: the worker knows it as a workspace.
    assert_eq!(w.rules.len(), rules_before + 1, "{:?}", w.rules);
    assert_eq!(
        w.rules[rules_before]["payload"]["workspace_path"],
        "/w/c6-missing"
    );
    created(
        &mut w,
        json!({"request_id": id, "canonical": "/w/c6-missing-hqc-fix"}),
    )
    .await;
    assert_eq!(workspace(&hub, &id).await["state"], "ready");
    let (s, sess) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": "in the worktree", "workspace_id": id}),
        )
        .await;
    assert_eq!(s, 201, "{sess}");
    let start = ack_start(&mut w, sess["id"].as_str().unwrap()).await;
    assert_eq!(start["payload"]["workspace_path"], "/w/c6-missing-hqc-fix");

    // The origin is not deleted while the worktree exists, not even its record.
    for q in ["", "?remove_folder=true"] {
        assert_eq!(hub.delete(&format!("/api/workspaces/{c6}{q}")).await, 409);
    }
    w.expect_silence(std::time::Duration::from_millis(200))
        .await;

    // Origin rules, refused before anything is stored or sent.
    let (_, other_project) = hub.post("/api/projects", json!({"name": "other"})).await;
    let other_project = other_project["id"].as_str().unwrap().to_string();
    let (s, plain) = hub.add_workspace(&mut w, &pid, "/w/repo").await;
    assert_eq!(s, 201, "{plain}");
    let plain = plain["id"].as_str().unwrap().to_string();
    let body = json!({"kind": "clone", "project_id": pid, "worker_id": w.worker_id,
                      "path": "/w/slow-missing", "url": URL});
    let (_, slow) = hub
        .while_worker_answers(&mut w, hub.post("/api/workspaces", body))
        .await;
    let slow = slow["id"].as_str().unwrap().to_string();
    create_frame(&mut w, &slow).await;
    let missing = uuid::Uuid::new_v4().to_string();
    let cases: Vec<(&str, &str, Value, u16, &str)> = vec![
        (&pid, &id, json!({}), 400, "invalid_origin"),
        (&other_project, &c6, json!({}), 400, "invalid_origin"),
        (&pid, &missing, json!({}), 400, "invalid_origin"),
        (&pid, &slow, json!({}), 400, "invalid_origin"),
        (&pid, &c6, json!({"branch": "a..b"}), 400, "invalid_branch"),
        (
            &pid,
            &c6,
            json!({"branch": "--force"}),
            400,
            "invalid_branch",
        ),
        (&pid, &c6, json!({"branch": ""}), 400, "invalid_branch"),
        (&pid, &c6, json!({"path": "/w/exists"}), 400, "path_exists"),
        // The default `/w/repo-x` exists on the fake worker.
        (&pid, &plain, json!({"branch": "x"}), 400, "path_exists"),
        (
            &pid,
            &c6,
            json!({"path": "/elsewhere/x-missing"}),
            400,
            "outside_allowed_roots",
        ),
        (
            &pid,
            &c6,
            json!({"path": "/w/repo/inner-missing"}),
            409,
            "workspace_conflict",
        ),
    ];
    for (project, origin, extra, status, code) in cases {
        let (s, v) = worktree(&hub, &mut w, project, origin, "hqc/x", extra.clone()).await;
        assert_eq!(
            (s, v["error"].as_str()),
            (status, Some(code)),
            "{origin} {extra}: {v}"
        );
    }
    let (s, v) = hub
        .post(
            "/api/workspaces",
            json!({"kind": "worktree", "project_id": pid, "worker_id": w.worker_id, "branch": "x"}),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("invalid_origin")));
    w.expect_silence(std::time::Duration::from_millis(200))
        .await;

    // Another worker of the same owner: the origin is not its workspace.
    let (_, token2) = hub.enroll("other-mac").await;
    let mut w2 = FakeWorker::connect(&hub, &token2).await.unwrap();
    w2.hello(ROOT, hub.boot_id).await;
    let (s, v) = worktree(&hub, &mut w2, &pid, &c6, "hqc/x", json!({})).await;
    assert_eq!((s, v["error"].as_str()), (400, Some("invalid_origin")));

    // A path given for a folder that is not git's on the worker: the worker refuses, the
    // record keeps the reason and still holds the origin until it is removed.
    let (s, v) = worktree(
        &hub,
        &mut w,
        &pid,
        &plain,
        "main",
        json!({"path": "/w/repo-missing", "new_branch": false}),
    )
    .await;
    assert_eq!(s, 201, "{v}");
    let bad = v["id"].as_str().unwrap().to_string();
    let f = create_frame(&mut w, &bad).await;
    assert!(f["payload"].get("new_branch").is_none(), "{f}");
    created(
        &mut w,
        json!({"request_id": bad, "error": "/w/repo is not the top folder of a git repository"}),
    )
    .await;
    let ws = workspace(&hub, &bad).await;
    assert_eq!(ws["state"], "failed", "{ws}");
    assert_eq!(hub.delete(&format!("/api/workspaces/{plain}")).await, 409);
    assert_eq!(hub.delete(&format!("/api/workspaces/{bad}")).await, 204);
    assert_eq!(hub.delete(&format!("/api/workspaces/{plain}")).await, 204);
    hub.finish().await;
}

/// A worktree in progress is asked for again after a reconnect (with its origin's rules) and
/// fails on a worker restart, like a clone.
#[tokio::test]
async fn worktree_resent_after_hello_and_failed_on_restart() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let mut w = FakeWorker::connect(&hub, &token).await.unwrap();
    w.hello(ROOT, hub.boot_id).await;
    let pid = hub.personal_project().await;
    let c6 = ready_clone(&hub, &mut w, &pid, "/w/c6-missing").await;
    let (s, v) = worktree(&hub, &mut w, &pid, &c6, "a", json!({})).await;
    assert_eq!(s, 201, "{v}");
    let a = v["id"].as_str().unwrap().to_string();
    create_frame(&mut w, &a).await;
    w.close().await;

    let mut w = FakeWorker::connect(&hub, &token).await.unwrap();
    w.hello(ROOT, hub.boot_id).await;
    let f = create_frame(&mut w, &a).await;
    assert_eq!(f["payload"]["origin"], "/w/c6-missing");
    assert!(w
        .rules
        .iter()
        .any(|r| r["payload"]["workspace_path"] == "/w/c6-missing"));
    created(
        &mut w,
        json!({"request_id": a, "canonical": "/w/c6-missing-a"}),
    )
    .await;
    assert_eq!(workspace(&hub, &a).await["state"], "ready");

    let (s, v) = worktree(&hub, &mut w, &pid, &c6, "b", json!({})).await;
    assert_eq!(s, 201, "{v}");
    let b = v["id"].as_str().unwrap().to_string();
    create_frame(&mut w, &b).await;
    w.close().await;
    let mut w = FakeWorker::connect(&hub, &token).await.unwrap();
    w.hello(ROOT, uuid::Uuid::new_v4()).await;
    let ws = workspace_when(&hub, &b, |ws| ws["state"] == "failed").await;
    assert!(ws["error"].as_str().unwrap().contains("restarted"), "{ws}");
    w.close().await;
    hub.finish().await;
}

/// Deleting a worktree goes through T45's inspection and `remove_folder`; `force` reaches the
/// worker only when the client says the user confirmed the dirty files. A project deleted as
/// a whole takes its origin and worktrees with it.
#[tokio::test]
async fn delete_a_worktree() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let mut w = FakeWorker::connect(&hub, &token).await.unwrap();
    w.hello(ROOT, hub.boot_id).await;
    let (_, p) = hub.post("/api/projects", json!({"name": "c6"})).await;
    let pid = p["id"].as_str().unwrap().to_string();
    let c6 = ready_clone(&hub, &mut w, &pid, "/w/c6-missing").await;
    let mut ids = vec![];
    for b in ["fix", "keep"] {
        let (s, v) = worktree(&hub, &mut w, &pid, &c6, b, json!({})).await;
        assert_eq!(s, 201, "{v}");
        let id = v["id"].as_str().unwrap().to_string();
        create_frame(&mut w, &id).await;
        let path = format!("/w/c6-missing-{b}");
        created(&mut w, json!({"request_id": id, "canonical": path})).await;
        ids.push(id);
    }

    let inspection = json!({"git": true, "dirty": ["notes.txt"], "dirty_count": 1,
                            "unpushed": [{"branch": "fix", "commits": 1}]});
    let ((s, v), f) = while_answering(
        &mut w,
        "workspace.inspect",
        inspection.clone(),
        hub.get(&format!("/api/workspaces/{}/inspect", ids[0])),
    )
    .await;
    assert_eq!((s, &v), (200, &inspection));
    assert_eq!(f["payload"]["path"], "/w/c6-missing-fix");

    // The worker refuses without force (dirty files): the record stays.
    let path = format!("/api/workspaces/{}?remove_folder=true", ids[0]);
    {
        let req = hub.delete(&path);
        tokio::pin!(req);
        let f = tokio::select! {
            _ = &mut req => panic!("answered before the worker got workspace.delete"),
            f = next_of(&mut w, "workspace.delete") => f,
        };
        assert!(f["payload"].get("force").is_none(), "{f}");
        w.reply(
        &f["id"],
        "error",
        json!({"code": "invalid_frame", "retryable": false,
               "message": "git worktree failed: fatal: '/w/c6-missing-fix' contains modified or untracked files, use --force to delete it"}),
    )
    .await;
        assert_eq!(req.await, 400);
    }
    assert_eq!(workspace(&hub, &ids[0]).await["state"], "ready");

    let (s, f) = while_answering(
        &mut w,
        "workspace.delete",
        json!({"removed": true}),
        hub.delete(&format!("{path}&force=true")),
    )
    .await;
    assert_eq!(s, 204);
    assert_eq!(
        f["payload"],
        json!({"request_id": f["id"], "path": "/w/c6-missing-fix", "force": true})
    );
    assert!(workspace(&hub, &ids[0]).await.is_null());
    // The other worktree still holds the origin.
    assert_eq!(hub.delete(&format!("/api/workspaces/{c6}")).await, 409);

    // The project goes as a whole, origin and worktree records together.
    assert_eq!(hub.delete(&format!("/api/projects/{pid}")).await, 204);
    assert!(workspace(&hub, &c6).await.is_null());
    assert!(workspace(&hub, &ids[1]).await.is_null());
    hub.finish().await;
}
