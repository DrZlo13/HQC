//! A session's model: set on creation, changed with `PATCH` (absent — unchanged, `null` —
//! the runtime's default), shown in `agent`, sent to the worker as `session.model` and with
//! every `session.start`.

mod common;

use common::*;
use serde_json::{json, Value};

const WS_PATH: &str = "/Users/home-mac/work/hqc";

async fn create(hub: &TestHub, body: Value) -> (u16, Value) {
    let pid = hub.personal_project().await;
    hub.post(&format!("/api/projects/{pid}/sessions"), body)
        .await
}

/// The worker says the session's agent stopped: the next input starts it again.
async fn stopped(w: &mut FakeWorker, sid: &str) {
    let id = w
        .send(
            "session.state",
            json!({"session_id": sid, "process_state": "stopped", "locked_by": "none"}),
        )
        .await;
    assert_eq!(w.expect("ack").await["id"], id.as_str());
}

#[tokio::test]
async fn a_session_is_created_with_a_model() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;

    let (s, v) = create(&hub, json!({"title": "Set", "model": "sonnet"})).await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    assert_eq!(v["agent"]["model"], "sonnet");
    let start = ack_start(&mut w, &sid).await;
    assert_eq!(start["payload"]["model"], "sonnet");

    // A model that is no model name.
    let (s, v) = create(&hub, json!({"model": "--resume"})).await;
    assert_eq!((s, v["error"].as_str()), (400, Some("bad_request")), "{v}");
}

#[tokio::test]
async fn a_change_reaches_the_running_agent_and_the_next_start() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (s, v) = create(&hub, json!({"title": "S", "model": "opus"})).await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    ack_start(&mut w, &sid).await;
    let patch = |body: Value| {
        let hub = &hub;
        let path = format!("/api/sessions/{sid}");
        async move { hub.patch(&path, body).await }
    };

    let (s, v) = patch(json!({"model": "sonnet"})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["agent"]["model"], "sonnet");
    let f = w.expect("session.model").await;
    assert_eq!(f["payload"], json!({"session_id": sid, "model": "sonnet"}));
    w.reply(&f["id"], "ack", json!({})).await;

    // Absent: unchanged, nothing sent.
    let (s, v) = patch(json!({"title": "Renamed"})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["agent"]["model"], "sonnet");

    // `null`: back to the runtime's default, said with `null`.
    let (s, v) = patch(json!({"model": null})).await;
    assert_eq!(s, 200, "{v}");
    assert!(v["agent"].get("model").is_none(), "{v}");
    let f = w.expect("session.model").await;
    assert_eq!(f["payload"], json!({"session_id": sid, "model": null}));
    w.reply(&f["id"], "ack", json!({})).await;

    // A refused value changes nothing, the rest of the body included.
    let (s, v) = patch(json!({"title": "Not this", "model": "a b"})).await;
    assert_eq!((s, v["error"].as_str()), (400, Some("bad_request")), "{v}");
    assert_eq!(hub.session(&sid).await["title"], "Renamed");

    // A stopped agent starts with what the session has now.
    let (s, v) = patch(json!({"model": "haiku"})).await;
    assert_eq!(s, 200, "{v}");
    let f = w.expect("session.model").await;
    w.reply(&f["id"], "ack", json!({})).await;
    stopped(&mut w, &sid).await;
    let (s, v) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": "go"}))
        .await;
    assert_eq!(s, 201, "{v}");
    let start = ack_start(&mut w, &sid).await;
    assert_eq!(start["payload"]["model"], "haiku");

    // Without a worker: stored, sent with the next start.
    w.close().await;
    let (s, v) = patch(json!({"model": "opus"})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["agent"]["model"], "opus");
}

#[tokio::test]
async fn the_service_session_has_no_model() {
    let hub = TestHub::start().await;
    hub.enroll("home-mac").await;
    let pid = hub.personal_project().await;
    let (_, v) = hub.get(&format!("/api/projects/{pid}/sessions")).await;
    let sys = v["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["system"] == true)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, v) = hub
        .patch(&format!("/api/sessions/{sys}"), json!({"model": "opus"}))
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some("system_session")),
        "{v}"
    );
}
