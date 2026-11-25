//! SPEC 0.144 (iteration 5, T55): model providers of an owner. The hub keeps the descriptions
//! and hands one to the worker in `session.start`; a session gets only a provider its worker's
//! `hello` allows, and a change of provider restarts a running agent.

mod common;

use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const WS_PATH: &str = "/Users/home-mac/work/qwen";

/// A worker whose `hello` allows `providers`, with the workspace `WS_PATH` in the personal
/// project.
async fn worker_allowing(hub: &TestHub, token: &str, providers: Value) -> FakeWorker {
    let mut w = FakeWorker::connect(hub, token)
        .await
        .expect("worker connect");
    w.hello_with(WS_PATH, hub.boot_id, json!({ "providers": providers }))
        .await;
    let pid = hub.personal_project().await;
    let (s, v) = hub.add_workspace(&mut w, &pid, WS_PATH).await;
    assert_eq!(s, 201, "{v}");
    w
}

fn qwen() -> Value {
    json!({"name": "qwen-local", "label": "Qwen 3.8 (local)",
           "base_url": "http://192.168.1.229:8082/", "model": "qwen3.8-27b",
           "context_tokens": 200000})
}

#[tokio::test]
async fn providers_are_kept_and_checked() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let _w = worker_allowing(&hub, &token, json!(["qwen-local"])).await;

    let (s, p) = hub.post("/api/providers", qwen()).await;
    assert_eq!(s, 201, "{p}");
    // Stored without the trailing `/`, as the worker compares it; the worker that allows it.
    assert_eq!(p["base_url"], "http://192.168.1.229:8082");
    assert_eq!(p["workers"].as_array().unwrap().len(), 1, "{p}");
    let id = p["id"].as_str().unwrap().to_string();

    // One name per owner.
    let (s, v) = hub.post("/api/providers", qwen()).await;
    assert_eq!((s, v["error"].as_str()), (409, Some("provider_exists")));
    for bad in [
        json!({"name": "a b", "label": "x", "base_url": "http://x", "model": "m"}),
        json!({"name": "x", "label": "x", "base_url": "ftp://x", "model": "m"}),
        json!({"name": "x", "label": "x", "base_url": "http://x", "model": "--skip"}),
        json!({"name": "x", "label": "", "base_url": "http://x", "model": "m"}),
        json!({"name": "x", "label": "x", "base_url": "http://x", "model": "m", "context_tokens": 5}),
        json!({"name": "x", "label": "x", "base_url": "http://x", "model": "m", "api_key": "k"}),
    ] {
        let (s, v) = hub.post("/api/providers", bad.clone()).await;
        assert!(s == 400 || s == 422, "{bad}: {s} {v}");
    }

    let (s, list) = hub.get("/api/providers").await;
    assert_eq!(s, 200);
    assert_eq!(list["providers"].as_array().unwrap().len(), 1);

    let mut edited = qwen();
    edited["label"] = json!("Qwen (home GPU)");
    let (s, v) = hub.patch(&format!("/api/providers/{id}"), edited).await;
    assert_eq!((s, v["label"].as_str()), (200, Some("Qwen (home GPU)")));
    assert_eq!(hub.delete(&format!("/api/providers/{id}")).await, 204);
    assert_eq!(hub.delete(&format!("/api/providers/{id}")).await, 404);
    hub.finish().await;
}

#[tokio::test]
async fn a_session_runs_on_a_provider_its_worker_allows() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let mut w = worker_allowing(&hub, &token, json!(["qwen-local"])).await;
    let (_, p) = hub.post("/api/providers", qwen()).await;
    let pid = p["id"].as_str().unwrap().to_string();
    let (_, other) = hub
        .post(
            "/api/providers",
            json!({"name": "zai", "label": "GLM", "base_url": "https://api.z.ai/api/anthropic",
                   "model": "glm-5"}),
        )
        .await;
    let project = hub.personal_project().await;

    // The worker does not allow `zai`.
    let (s, v) = hub
        .post(
            &format!("/api/projects/{project}/sessions"),
            json!({"title": "glm", "provider_id": other["id"]}),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (422, Some("provider_not_allowed")),
        "{v}"
    );
    let (s, _) = hub
        .post(
            &format!("/api/projects/{project}/sessions"),
            json!({"title": "x", "provider_id": Uuid::now_v7()}),
        )
        .await;
    assert_eq!(s, 400);

    let (s, v) = hub
        .post(
            &format!("/api/projects/{project}/sessions"),
            json!({"title": "qwen", "provider_id": pid}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    assert_eq!(
        v["agent"]["provider"],
        json!({"id": pid, "label": "Qwen 3.8 (local)"})
    );
    let start = ack_start(&mut w, &sid).await;
    assert_eq!(
        start["payload"]["provider"],
        json!({"name": "qwen-local", "base_url": "http://192.168.1.229:8082",
               "model": "qwen3.8-27b", "context_tokens": 200000})
    );

    // In use: not deleted.
    let (s, v) = {
        let s = hub.delete(&format!("/api/providers/{pid}")).await;
        (s, Value::Null)
    };
    assert_eq!(s, 409, "{v}");

    // A running agent goes back to Claude Code's own endpoint with a restart.
    w.send(
        "session.state",
        json!({"session_id": sid, "process_state": "running", "locked_by": "hqc"}),
    )
    .await;
    w.expect("ack").await;
    let (s, v) = hub
        .patch(
            &format!("/api/sessions/{sid}"),
            json!({"provider_id": null}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(v["agent"].get("provider").is_none(), "{v}");
    let kill = w.expect("session.kill").await;
    assert_eq!(kill["payload"]["session_id"], sid.as_str());
    let start = ack_start(&mut w, &sid).await;
    assert!(start["payload"].get("provider").is_none(), "{start}");

    // A stopped agent gets the provider with its next start, no restart now.
    w.send(
        "session.state",
        json!({"session_id": sid, "process_state": "stopped", "locked_by": "none"}),
    )
    .await;
    w.expect("ack").await;
    let (s, v) = hub
        .patch(&format!("/api/sessions/{sid}"), json!({"provider_id": pid}))
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["agent"]["provider"]["label"], "Qwen 3.8 (local)");
    w.expect_silence(std::time::Duration::from_millis(300))
        .await;
    // #194: Auto's safety check does not pass through a provider.
    let (s, v) = hub
        .patch(
            &format!("/api/sessions/{sid}"),
            json!({"permission_mode": "auto"}),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (422, Some("auto_needs_anthropic"))
    );
    let (s, v) = hub
        .patch(
            &format!("/api/sessions/{sid}"),
            json!({"permission_mode": "auto", "provider_id": null}),
        )
        .await;
    assert_eq!(s, 200, "a Claude model again: {v}");
    let (s, v) = hub
        .patch(&format!("/api/sessions/{sid}"), json!({"provider_id": pid}))
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = hub
        .patch(
            &format!("/api/sessions/{sid}"),
            json!({"provider_id": other["id"]}),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (422, Some("provider_not_allowed"))
    );
    hub.finish().await;
}
