//! A session's model and auto-compaction: set on creation, changed with `PATCH` (absent —
//! unchanged, `null` — the runtime's default), shown in `agent`, sent to the worker as
//! `session.model` / `session.autocompact` and with every `session.start`.

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
async fn a_session_is_created_with_a_model_and_an_autocompaction() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;

    let (s, v) = create(
        &hub,
        json!({"title": "Set", "model": "sonnet", "autocompact": 300_000}),
    )
    .await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    assert_eq!(
        (&v["agent"]["model"], &v["agent"]["autocompact"]),
        (&json!("sonnet"), &json!(300_000))
    );
    let start = ack_start(&mut w, &sid).await;
    assert_eq!(start["payload"]["model"], "sonnet");
    assert_eq!(start["payload"]["autocompact"], 300_000);

    // Off is 0.
    let (s, v) = create(&hub, json!({"title": "Off", "autocompact": 0})).await;
    assert_eq!(s, 201, "{v}");
    let off = v["id"].as_str().unwrap().to_string();
    assert_eq!(v["agent"]["autocompact"], 0);
    assert_eq!(ack_start(&mut w, &off).await["payload"]["autocompact"], 0);

    // Neither: the runtime's defaults, nothing in `agent` or `session.start`.
    let (s, v) = create(&hub, json!({"title": "Default"})).await;
    assert_eq!(s, 201, "{v}");
    let plain = v["id"].as_str().unwrap().to_string();
    assert!(v["agent"].get("autocompact").is_none(), "{v}");
    let start = ack_start(&mut w, &plain).await;
    assert!(start["payload"].get("autocompact").is_none(), "{start}");

    // A window outside 100k..=1M tokens, or a model that is no model name.
    for body in [
        json!({"autocompact": 99_999}),
        json!({"autocompact": 1_000_001}),
        json!({"model": "--resume"}),
    ] {
        let (s, v) = create(&hub, body.clone()).await;
        assert_eq!(
            (s, v["error"].as_str()),
            (400, Some("bad_request")),
            "{body}"
        );
    }
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

    let (s, v) = patch(json!({"model": "sonnet", "autocompact": 500_000})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (&v["agent"]["model"], &v["agent"]["autocompact"]),
        (&json!("sonnet"), &json!(500_000))
    );
    let f = w.expect("session.model").await;
    assert_eq!(f["payload"], json!({"session_id": sid, "model": "sonnet"}));
    w.reply(&f["id"], "ack", json!({})).await;
    let f = w.expect("session.autocompact").await;
    assert_eq!(
        f["payload"],
        json!({"session_id": sid, "autocompact": 500_000})
    );
    w.reply(&f["id"], "ack", json!({})).await;

    // Absent: unchanged, nothing sent.
    let (s, v) = patch(json!({"title": "Renamed"})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (&v["agent"]["model"], &v["agent"]["autocompact"]),
        (&json!("sonnet"), &json!(500_000))
    );

    // `null`: back to the runtime's defaults, said with `null`.
    let (s, v) = patch(json!({"model": null, "autocompact": null})).await;
    assert_eq!(s, 200, "{v}");
    assert!(v["agent"].get("model").is_none(), "{v}");
    assert!(v["agent"].get("autocompact").is_none(), "{v}");
    let f = w.expect("session.model").await;
    assert_eq!(f["payload"], json!({"session_id": sid, "model": null}));
    w.reply(&f["id"], "ack", json!({})).await;
    let f = w.expect("session.autocompact").await;
    assert_eq!(
        f["payload"],
        json!({"session_id": sid, "autocompact": null})
    );
    w.reply(&f["id"], "ack", json!({})).await;

    // Refused values change nothing, the rest of the body included.
    for body in [
        json!({"title": "Not this", "autocompact": 5_000}),
        json!({"title": "Not this", "model": "a b"}),
    ] {
        let (s, v) = patch(body.clone()).await;
        assert_eq!(
            (s, v["error"].as_str()),
            (400, Some("bad_request")),
            "{body}"
        );
    }
    assert_eq!(hub.session(&sid).await["title"], "Renamed");

    // A stopped agent starts with what the session has now.
    let (s, v) = patch(json!({"model": "haiku", "autocompact": 0})).await;
    assert_eq!(s, 200, "{v}");
    for ty in ["session.model", "session.autocompact"] {
        let f = w.expect(ty).await;
        w.reply(&f["id"], "ack", json!({})).await;
    }
    stopped(&mut w, &sid).await;
    let (s, v) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": "go"}))
        .await;
    assert_eq!(s, 201, "{v}");
    let start = ack_start(&mut w, &sid).await;
    assert_eq!(
        (&start["payload"]["model"], &start["payload"]["autocompact"]),
        (&json!("haiku"), &json!(0))
    );

    // Without a worker: stored, sent with the next start.
    w.close().await;
    let (s, v) = patch(json!({"autocompact": 1_000_000})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["agent"]["autocompact"], 1_000_000);
}

#[tokio::test]
async fn the_service_session_has_neither() {
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
    for body in [json!({"model": "opus"}), json!({"autocompact": 0})] {
        let (s, v) = hub.patch(&format!("/api/sessions/{sys}"), body).await;
        assert_eq!(
            (s, v["error"].as_str()),
            (409, Some("system_session")),
            "{v}"
        );
    }
}
