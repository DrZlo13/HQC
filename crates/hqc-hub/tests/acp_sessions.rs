//! SPEC 0.266 (iteration 10, §8.2, §6): `POST /api/projects/{id}/sessions` with `runtime: "acp"`
//! and `acp_agent` makes a session of an ACP agent the worker named in `hello`; one it did not
//! name is `422 acp_agent_unavailable`, and a provider never goes with one.

mod common;

use common::*;
use serde_json::{json, Value};

const WS_PATH: &str = "/Users/home-mac/work/lab/acp";

/// A worker whose `hello` names the ACP agent `agy`, its workspace in the personal project:
/// `(worker, project id, workspace id)`.
async fn setup(hub: &TestHub) -> (FakeWorker, String, String) {
    let (_, token) = hub.enroll("home-mac").await;
    let mut w = FakeWorker::connect(hub, &token)
        .await
        .expect("worker connect");
    let capabilities = json!({"claude_code": {"version": "2.1.307"}, "acp_agents": [
        {"name": "agy", "version": "1.5.18", "environments": ["default"],
         "load_session": true, "resume": true, "image": true}]});
    w.hello_with(
        WS_PATH,
        hub.boot_id,
        json!({ "capabilities": capabilities }),
    )
    .await;
    let pid = hub.personal_project().await;
    let (s, ws) = hub.add_workspace(&mut w, &pid, WS_PATH).await;
    assert_eq!(s, 201, "{ws}");
    (w, pid, ws["id"].as_str().unwrap().to_string())
}

async fn create(hub: &TestHub, pid: &str, body: Value) -> (u16, Value) {
    hub.post(&format!("/api/projects/{pid}/sessions"), body)
        .await
}

#[tokio::test]
async fn a_session_of_an_acp_agent_the_worker_named() {
    let hub = TestHub::start().await;
    let (mut w, pid, ws) = setup(&hub).await;
    let (s, v) = create(
        &hub,
        &pid,
        json!({"workspace_id": ws, "runtime": "acp", "acp_agent": "agy"}),
    )
    .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["agent"]["runtime"], "acp");
    assert_eq!(v["agent"]["acp_agent"], "agy");
    // The agent reports its own session id once it made one.
    assert!(v["agent"]["runtime_session_id"].is_null(), "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    let start = ack_start(&mut w, &sid).await;
    assert_eq!(start["payload"]["runtime"], "acp");
    assert_eq!(start["payload"]["acp_agent"], "agy");
    assert!(start["payload"]["runtime_session_id"].is_null(), "{start}");
    hub.finish().await;
}

#[tokio::test]
async fn an_agent_the_worker_did_not_name_is_refused() {
    let hub = TestHub::start().await;
    let (_w, pid, ws) = setup(&hub).await;
    let (s, v) = create(
        &hub,
        &pid,
        json!({"workspace_id": ws, "runtime": "acp", "acp_agent": "codex"}),
    )
    .await;
    assert_eq!(s, 422, "{v}");
    assert_eq!(v["error"], "acp_agent_unavailable", "{v}");
    hub.finish().await;
}

#[tokio::test]
async fn an_acp_session_never_takes_a_provider_and_the_pair_goes_together() {
    let hub = TestHub::start().await;
    let (_w, pid, ws) = setup(&hub).await;
    let provider = uuid::Uuid::new_v4().to_string();
    for body in [
        json!({"workspace_id": ws, "runtime": "acp", "acp_agent": "agy", "provider_id": provider}),
        json!({"workspace_id": ws, "runtime": "acp"}),
        json!({"workspace_id": ws, "acp_agent": "agy"}),
        json!({"workspace_id": ws, "runtime": "claude_code", "acp_agent": "agy"}),
        json!({"workspace_id": ws, "runtime": "bare"}),
    ] {
        let (s, v) = create(&hub, &pid, body.clone()).await;
        assert_eq!(s, 400, "{body} -> {v}");
    }
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM sessions WHERE runtime = 'acp'")
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
    hub.finish().await;
}
