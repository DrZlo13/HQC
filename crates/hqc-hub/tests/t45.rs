//! T45: workspaces hqc clones, and deleting what it created (SPEC §3 Workspace, §5.3, §5.4, §6,
//! §13). A clone is `creating` until the worker's `workspace.created`, then `ready` or `failed`;
//! the hub asks again after a reconnect and fails it on a worker restart. Deleting a folder hqc
//! created goes through `workspace.inspect` and `workspace.delete`.

mod common;

use common::*;
use serde_json::{json, Value};

const ROOT: &str = "/w";
const URL: &str = "git@github.com:drzlo/c6-displays.git";

async fn workspace(hub: &TestHub, id: &str) -> Value {
    let (s, list) = hub.get("/api/workspaces").await;
    assert_eq!(s, 200, "{list}");
    list["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == id)
        .cloned()
        .unwrap_or(Value::Null)
}

/// `POST /api/workspaces {kind: clone}` while the worker answers the `fs.stat`s (the fake says
/// a path containing `missing` does not exist).
async fn clone(
    hub: &TestHub,
    w: &mut FakeWorker,
    pid: &str,
    path: &str,
    url: &str,
) -> (u16, Value) {
    let body = json!({"kind": "clone", "project_id": pid, "worker_id": w.worker_id,
                      "path": path, "url": url});
    hub.while_worker_answers(w, hub.post("/api/workspaces", body))
        .await
}

/// The `workspace.create` for `id`, acked like the worker does.
async fn create_frame(w: &mut FakeWorker, id: &str) -> Value {
    let f = w.expect("workspace.create").await;
    assert_eq!(f["payload"]["request_id"], id, "{f}");
    w.reply(&f["id"], "ack", json!({})).await;
    f
}

async fn created(w: &mut FakeWorker, payload: Value) {
    let id = w.send("workspace.created", payload).await;
    let ack = w.expect("ack").await;
    assert_eq!(ack["id"], id);
}

/// Run `req` while the worker answers the one frame of type `ty` it gets with `answer`.
async fn while_answering<T>(
    w: &mut FakeWorker,
    ty: &str,
    answer: Value,
    req: impl std::future::Future<Output = T>,
) -> (T, Value) {
    tokio::pin!(req);
    let f = tokio::select! {
        _ = &mut req => panic!("answered before the worker got {ty}"),
        f = next_of(w, ty) => f,
    };
    let mut payload = answer;
    payload["request_id"] = f["payload"]["request_id"].clone();
    w.reply(&f["id"], "result", payload).await;
    (req.await, f)
}

#[tokio::test]
async fn clone_becomes_ready_or_failed() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let mut w = FakeWorker::connect(&hub, &token).await.unwrap();
    w.hello(ROOT, hub.boot_id).await;
    let pid = hub.personal_project().await;

    let (s, v) = clone(&hub, &mut w, &pid, "/w/c6-missing", URL).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(
        (&v["kind"], &v["state"], &v["created_by_hqc"]),
        (&json!("clone"), &json!("creating"), &json!(true))
    );
    let id = v["id"].as_str().unwrap().to_string();
    let f = create_frame(&mut w, &id).await;
    assert_eq!(
        f["payload"],
        json!({"request_id": id, "kind": "clone", "path": "/w/c6-missing", "url": URL})
    );
    // No session in a folder that is not there yet.
    let (s, v) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": "x"}),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (409, Some("workspace_not_ready")));
    // Nor is it deleted under the running clone.
    assert_eq!(hub.delete(&format!("/api/workspaces/{id}")).await, 409);

    created(
        &mut w,
        json!({"request_id": id, "canonical": "/w/c6-missing"}),
    )
    .await;
    let ws = workspace(&hub, &id).await;
    assert_eq!(ws["state"], "ready", "{ws}");
    assert!(ws.get("error").is_none());
    // Mirrored like any new workspace, once the folder exists.
    assert!(w
        .mirror_sets
        .iter()
        .any(|m| m["payload"]["workspace_path"] == "/w/c6-missing"));
    // The same outcome again (a repeated request) changes nothing.
    created(
        &mut w,
        json!({"request_id": id, "canonical": "/w/c6-missing"}),
    )
    .await;
    assert_eq!(workspace(&hub, &id).await["state"], "ready");
    let sid = hub.create_session("in the clone").await;
    ack_start(&mut w, &sid).await;

    // A clone that fails keeps its record with the reason; removing it removes the record.
    let (s, v) = clone(
        &hub,
        &mut w,
        &pid,
        "/w/bad-missing",
        "https://example.com/x.git",
    )
    .await;
    assert_eq!(s, 201, "{v}");
    let bad = v["id"].as_str().unwrap().to_string();
    create_frame(&mut w, &bad).await;
    created(
        &mut w,
        json!({"request_id": bad, "error": "git clone failed: not found"}),
    )
    .await;
    let ws = workspace(&hub, &bad).await;
    assert_eq!(
        (&ws["state"], &ws["error"]),
        (&json!("failed"), &json!("git clone failed: not found"))
    );
    // A late success does not revive it.
    created(
        &mut w,
        json!({"request_id": bad, "canonical": "/w/bad-missing"}),
    )
    .await;
    assert_eq!(workspace(&hub, &bad).await["state"], "failed");
    assert_eq!(hub.delete(&format!("/api/workspaces/{bad}")).await, 204);
    assert!(workspace(&hub, &bad).await.is_null());

    // Refused before anything is stored or sent.
    let cases = [
        ("/w/x-missing", "ext::sh", 400, "invalid_url"),
        ("/w/x-missing", "http://example.com/x", 400, "invalid_url"),
        ("/w/existing", URL, 400, "path_exists"),
        ("/elsewhere/x-missing", URL, 400, "outside_allowed_roots"),
        ("/w/missing/x", URL, 400, "path_not_found"),
        ("/w/c6-missing", URL, 409, "workspace_conflict"),
        ("relative-missing", URL, 400, "bad_request"),
    ];
    for (path, url, status, code) in cases {
        let (s, v) = clone(&hub, &mut w, &pid, path, url).await;
        assert_eq!(
            (s, v["error"].as_str()),
            (status, Some(code)),
            "{path}: {v}"
        );
    }
    let (s, v) = hub
        .post(
            "/api/workspaces",
            json!({"kind": "clone", "project_id": pid, "worker_id": w.worker_id,
                   "path": "/w/y-missing", "url": URL, "branch": "--upload-pack=x"}),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("invalid_branch")));
    w.expect_silence(std::time::Duration::from_millis(200))
        .await;

    // Offline: nothing is created.
    let wid = w.worker_id.clone();
    w.close().await;
    let (s, v) = hub
        .post(
            "/api/workspaces",
            json!({"kind": "clone", "project_id": pid, "worker_id": wid,
                   "path": "/w/z-missing", "url": URL}),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (409, Some("worker_offline")));
    hub.finish().await;
}

/// The workspace once `pred` holds for it (the hub handles a worker's `error` on its own time).
async fn workspace_when(hub: &TestHub, id: &str, pred: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let ws = workspace(hub, id).await;
        if pred(&ws) {
            return ws;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "workspace stays {ws}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// The next frame of type `ty`, skipping others (`session.kill` of an archived session).
async fn next_of(w: &mut FakeWorker, ty: &str) -> Value {
    loop {
        let f = w.recv().await;
        if f["type"] == ty {
            return f;
        }
    }
}

/// A clone in progress survives a dropped connection: the hub asks again after `hello` and
/// takes the answer then. A new worker process fails it.
#[tokio::test]
async fn reconnect_and_restart_during_a_clone() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let mut w = FakeWorker::connect(&hub, &token).await.unwrap();
    w.hello(ROOT, hub.boot_id).await;
    let pid = hub.personal_project().await;
    let (s, v) = clone(&hub, &mut w, &pid, "/w/a-missing", URL).await;
    assert_eq!(s, 201, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    create_frame(&mut w, &id).await;
    w.close().await;

    // The same process reconnects: asked again with the same request.
    let mut w = FakeWorker::connect(&hub, &token).await.unwrap();
    w.hello(ROOT, hub.boot_id).await;
    let f = create_frame(&mut w, &id).await;
    assert_eq!(f["payload"]["path"], "/w/a-missing");
    assert_eq!(workspace(&hub, &id).await["state"], "creating");
    created(
        &mut w,
        json!({"request_id": id, "canonical": "/w/a-missing"}),
    )
    .await;
    assert_eq!(workspace(&hub, &id).await["state"], "ready");

    // A worker before T45 refuses the frame: the clone fails with its message.
    let (s, v) = clone(&hub, &mut w, &pid, "/w/old-missing", URL).await;
    assert_eq!(s, 201, "{v}");
    let old = v["id"].as_str().unwrap().to_string();
    let f = w.expect("workspace.create").await;
    w.reply(
        &f["id"],
        "error",
        json!({"code": "invalid_frame", "message": "unknown variant `workspace.create`", "retryable": false}),
    )
    .await;
    let ws = workspace_when(&hub, &old, |ws| ws["state"] == "failed").await;
    assert!(
        ws["error"].as_str().unwrap().contains("unknown variant"),
        "{ws}"
    );

    // A restart while cloning: failed, and not asked for again.
    let (s, v) = clone(&hub, &mut w, &pid, "/w/b-missing", URL).await;
    assert_eq!(s, 201, "{v}");
    let b = v["id"].as_str().unwrap().to_string();
    create_frame(&mut w, &b).await;
    w.close().await;
    let mut w = FakeWorker::connect(&hub, &token).await.unwrap();
    w.hello(ROOT, uuid::Uuid::new_v4()).await;
    w.expect_silence(std::time::Duration::from_millis(300))
        .await;
    let ws = workspace(&hub, &b).await;
    assert_eq!(ws["state"], "failed", "{ws}");
    assert!(ws["error"].as_str().unwrap().contains("restarted"), "{ws}");
    assert_eq!(workspace(&hub, &id).await["state"], "ready");
    w.close().await;
    hub.finish().await;
}

/// The deletion dialog's inspection, and `remove_folder`, which only a ready folder hqc created
/// takes and which the worker carries out before the record goes.
#[tokio::test]
async fn delete_with_remove_folder() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let mut w = FakeWorker::connect(&hub, &token).await.unwrap();
    w.hello(ROOT, hub.boot_id).await;
    let pid = hub.personal_project().await;
    let (s, dir) = hub.add_workspace(&mut w, &pid, "/w/plain").await;
    assert_eq!(s, 201, "{dir}");
    let dir = dir["id"].as_str().unwrap().to_string();
    let (_, v) = clone(&hub, &mut w, &pid, "/w/c-missing", URL).await;
    let id = v["id"].as_str().unwrap().to_string();
    create_frame(&mut w, &id).await;
    created(
        &mut w,
        json!({"request_id": id, "canonical": "/w/c-missing"}),
    )
    .await;

    let inspection = json!({"git": true, "dirty": ["src/panel.rs"], "dirty_count": 1,
                            "unpushed": [{"branch": "hqc/fix", "commits": 2}]});
    let ((s, v), f) = while_answering(
        &mut w,
        "workspace.inspect",
        inspection.clone(),
        hub.get(&format!("/api/workspaces/{id}/inspect")),
    )
    .await;
    assert_eq!((s, &v), (200, &inspection));
    assert_eq!(f["payload"]["path"], "/w/c-missing");
    let (s, _) = hub.get(&format!("/api/workspaces/{dir}/inspect")).await;
    assert_eq!(s, 400, "a folder hqc did not create is not inspected");
    assert_eq!(
        hub.delete(&format!("/api/workspaces/{dir}?remove_folder=true"))
            .await,
        400
    );

    // A session that is not archived keeps it, before the worker is asked.
    let (s, sess) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": "x", "workspace_id": id}),
        )
        .await;
    assert_eq!(s, 201, "{sess}");
    let sid = sess["id"].as_str().unwrap();
    ack_start(&mut w, sid).await;
    assert_eq!(
        hub.delete(&format!("/api/workspaces/{id}?remove_folder=true"))
            .await,
        409
    );
    w.expect_silence(std::time::Duration::from_millis(200))
        .await;
    let (s, v) = hub
        .post(&format!("/api/sessions/{sid}/archive"), json!({}))
        .await;
    assert!(s < 300, "{v}");

    // The worker refuses: nothing is deleted.
    let path = format!("/api/workspaces/{id}?remove_folder=true");
    let f = {
        let req = hub.delete(&path);
        tokio::pin!(req);
        let f = tokio::select! {
            _ = &mut req => panic!("answered before the worker got workspace.delete"),
            f = next_of(&mut w, "workspace.delete") => f,
        };
        w.reply(
            &f["id"],
            "error",
            json!({"code": "invalid_frame", "message": "not a folder this worker created", "retryable": false}),
        )
        .await;
        assert_eq!(req.await, 400);
        f
    };
    assert_eq!(f["payload"]["path"], "/w/c-missing");
    assert_eq!(workspace(&hub, &id).await["state"], "ready");

    let (s, f) = while_answering(
        &mut w,
        "workspace.delete",
        json!({"removed": true}),
        hub.delete(&path),
    )
    .await;
    assert_eq!(s, 204);
    assert_eq!(f["payload"]["path"], "/w/c-missing");
    assert!(workspace(&hub, &id).await.is_null());

    // Offline: nothing is deleted.
    let (_, v) = clone(&hub, &mut w, &pid, "/w/d-missing", URL).await;
    let d = v["id"].as_str().unwrap().to_string();
    create_frame(&mut w, &d).await;
    created(
        &mut w,
        json!({"request_id": d, "canonical": "/w/d-missing"}),
    )
    .await;
    w.close().await;
    assert_eq!(
        hub.delete(&format!("/api/workspaces/{d}?remove_folder=true"))
            .await,
        409
    );
    assert_eq!(workspace(&hub, &d).await["state"], "ready");
    // The record alone goes without the worker.
    assert_eq!(hub.delete(&format!("/api/workspaces/{d}")).await, 204);
    hub.finish().await;
}
