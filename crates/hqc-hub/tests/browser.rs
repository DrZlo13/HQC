//! SPEC 0.148 (iteration 6): the owner gives a session's agent their Chrome. Only on a worker
//! whose `hello` has `browser`; a change restarts a running agent with `session.start.browser`.

mod common;

use common::*;
use serde_json::json;

const WS_PATH: &str = "/Users/drzlo/work/research";

async fn worker(hub: &TestHub, browser: bool) -> (FakeWorker, String) {
    let (_, token) = hub.enroll("drzlo-mac").await;
    let mut w = FakeWorker::connect(hub, &token)
        .await
        .expect("worker connect");
    w.hello_with(WS_PATH, hub.boot_id, json!({ "browser": browser }))
        .await;
    let pid = hub.personal_project().await;
    let (s, ws) = hub.add_workspace(&mut w, &pid, WS_PATH).await;
    assert_eq!(s, 201, "{ws}");
    let (s, v) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": "ebay", "workspace_id": ws["id"]}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    let start = ack_start(&mut w, &sid).await;
    assert!(
        start["payload"].get("browser").is_none(),
        "off by default: {start}"
    );
    (w, sid)
}

#[tokio::test]
async fn the_browser_is_given_per_session_and_restarts_the_agent() {
    let hub = TestHub::start().await;
    let (mut w, sid) = worker(&hub, true).await;
    let (_, v) = hub.get("/api/workers").await;
    assert_eq!(v["workers"][0]["browser"], true);
    w.send(
        "session.state",
        json!({"session_id": sid, "process_state": "running", "locked_by": "hqc"}),
    )
    .await;
    w.expect("ack").await;

    let url = format!("/api/sessions/{sid}");
    let (s, v) = hub.patch(&url, json!({"browser": true})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["agent"]["browser"], true);
    let kill = w.expect("session.kill").await;
    assert_eq!(kill["payload"]["session_id"], sid.as_str());
    let start = ack_start(&mut w, &sid).await;
    assert_eq!(start["payload"]["browser"], true, "{start}");

    // The same again changes nothing.
    let (s, _) = hub.patch(&url, json!({"browser": true})).await;
    assert_eq!(s, 200);
    w.expect_silence(std::time::Duration::from_millis(300))
        .await;

    // Off: back without it.
    let (s, v) = hub.patch(&url, json!({"browser": false})).await;
    assert_eq!(s, 200, "{v}");
    assert!(v["agent"].get("browser").is_none(), "{v}");
    w.expect("session.kill").await;
    let start = ack_start(&mut w, &sid).await;
    assert!(start["payload"].get("browser").is_none(), "{start}");
    hub.finish().await;
}

#[tokio::test]
async fn a_worker_without_the_browser_gives_none() {
    let hub = TestHub::start().await;
    let (mut w, sid) = worker(&hub, false).await;
    let (s, v) = hub
        .patch(&format!("/api/sessions/{sid}"), json!({"browser": true}))
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (422, Some("browser_not_allowed")),
        "{v}"
    );
    assert!(hub.session(&sid).await["agent"].get("browser").is_none());
    w.expect_silence(std::time::Duration::from_millis(300))
        .await;
    hub.finish().await;
}
