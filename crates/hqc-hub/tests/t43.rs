//! T43: environments and bindings on the worker (SPEC §3 Workspace, §5.1, §5.3, §14). The hub
//! learns them from `hello` and shows each workspace's environment; it sends none in
//! `session.start`, the worker resolves it itself.

mod common;

use common::*;
use serde_json::{json, Value};

const ROOT: &str = "/w";

fn infra(bindings: Value) -> Value {
    json!({
        "environments": [{"name": "infra", "permissions": {
            "unblock": ["Bash(ssh:*)"], "allow": ["Bash(ssh debian@192.168.1.20:*)"]}}],
        "env_bindings": bindings,
    })
}

async fn workspace(hub: &TestHub, id: &str) -> Value {
    let (s, list) = hub.get("/api/workspaces").await;
    assert_eq!(s, 200, "{list}");
    list["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == id)
        .cloned()
        .unwrap()
}

/// A workspace shows the environment of the longest binding from the last `hello`, and
/// follows a new `hello`; `session.start` carries no environment.
#[tokio::test]
async fn workspace_environment_follows_hello() {
    let hub = TestHub::start().await;
    let (worker_id, token) = hub.enroll("home-mac").await;
    let mut w = FakeWorker::connect(&hub, &token).await.unwrap();
    w.hello_with(
        ROOT,
        hub.boot_id,
        infra(json!([
            {"path": "/w/lab", "environment": "infra"},
            {"path": "/w/lab/pub", "environment": "default"},
        ])),
    )
    .await;
    let pid = hub.personal_project().await;
    let mut ids = Vec::new();
    for path in ["/w/lab/infra", "/w/labx", "/w/lab/pub/site"] {
        let (s, v) = hub.add_workspace(&mut w, &pid, path).await;
        assert_eq!(s, 201, "{path}: {v}");
        ids.push(v["id"].as_str().unwrap().to_string());
    }
    let env = |v: &Value| v["environment"].as_str().unwrap().to_string();
    assert_eq!(env(&workspace(&hub, &ids[0]).await), "infra");
    assert_eq!(
        env(&workspace(&hub, &ids[1]).await),
        "default",
        "/a/b vs /a/bc"
    );
    assert_eq!(env(&workspace(&hub, &ids[2]).await), "default");

    // The worker object keeps what hello said, rules included, for display.
    let (_, workers) = hub.get("/api/workers").await;
    let me = workers["workers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == worker_id.as_str())
        .unwrap()
        .clone();
    assert_eq!(me["environments"][0]["name"], "infra");
    assert_eq!(
        me["environments"][0]["permissions"]["unblock"],
        json!(["Bash(ssh:*)"])
    );

    // A session in the bound folder (the project's first workspace is its default): no
    // environment in session.start.
    let sid = hub.create_session("infra").await;
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "hi", "artifact_ids": [], "client_op_id": "c1"}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let start = ack_start(&mut w, &sid).await;
    assert_eq!(start["payload"]["workspace_path"], "/w/lab/infra");
    assert!(start["payload"].get("environment").is_none(), "{start}");
    w.close().await;

    // The worker restarts with other bindings: the workspaces follow.
    let mut w = FakeWorker::connect(&hub, &token).await.unwrap();
    w.hello_with(
        ROOT,
        uuid::Uuid::new_v4(),
        infra(json!([{"path": "/w", "environment": "infra"}])),
    )
    .await;
    assert_eq!(env(&workspace(&hub, &ids[0]).await), "infra");
    assert_eq!(env(&workspace(&hub, &ids[1]).await), "infra");
    assert_eq!(env(&workspace(&hub, &ids[2]).await), "infra");
    w.close().await;

    // An older worker that sends only names: no bindings, everything is default.
    let mut w = FakeWorker::connect(&hub, &token).await.unwrap();
    w.hello(ROOT, uuid::Uuid::new_v4()).await;
    for id in &ids {
        assert_eq!(env(&workspace(&hub, id).await), "default");
    }
    w.close().await;
    hub.finish().await;
}
