//! T22: limits against a compromised worker or its token (SPEC §2.1, §20).

mod common;

use std::time::Duration;

use chrono::{TimeDelta, Utc};
use common::*;
use hqc_hub::config::Limits;
use serde_json::{json, Value};

const WS_PATH: &str = "/w/hqc";

fn request(request_id: &str, expires_in: TimeDelta) -> Value {
    json!({
        "request_id": request_id,
        "kind": "workspace.use",
        "description": "The hub wants to run sessions in /w/cad",
        "expires_at": Utc::now() + expires_in,
    })
}

/// Send `worker.approval.request`; the hub's reply (`ack` or `error`).
async fn ask(w: &mut FakeWorker, payload: Value) -> Value {
    let id = w.send("worker.approval.request", payload).await;
    let reply = w.recv().await;
    assert_eq!(reply["id"], id, "{reply}");
    reply
}

fn refused(reply: &Value) {
    assert_eq!(reply["type"], "error", "{reply}");
    assert_eq!(reply["payload"]["code"], "invalid_frame", "{reply}");
    assert_eq!(reply["payload"]["retryable"], false, "{reply}");
}

async fn subscribe_push(hub: &TestHub) {
    let (s, _) = hub
        .post(
            "/api/push/subscribe",
            json!({"endpoint": "https://fcm.googleapis.com/fcm/send/sub-1", "expirationTime": null,
                   "keys": {"p256dh": "k", "auth": "a"}}),
        )
        .await;
    assert_eq!(s, 201);
}

/// Lengths, `expires_at` and the number of open requests of a worker are limited; a
/// refused request stores nothing. A resend of a known request is still acknowledged.
#[tokio::test]
async fn worker_requests_are_limited() {
    let hub = TestHub::start_limited(Limits {
        worker_approvals_open: 3,
        ..Limits::default()
    })
    .await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, ack) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = ack["payload"]["system_session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let ten_min = TimeDelta::minutes(10);

    refused(&ask(&mut w, request(&"r".repeat(129), ten_min)).await);
    let mut long_kind = request("k", ten_min);
    long_kind["kind"] = json!("k".repeat(65));
    refused(&ask(&mut w, long_kind).await);
    let mut long_desc = request("d", ten_min);
    long_desc["description"] = json!("я".repeat(2049));
    refused(&ask(&mut w, long_desc).await);
    refused(&ask(&mut w, request("past", TimeDelta::seconds(-5))).await);
    refused(&ask(&mut w, request("far", TimeDelta::hours(25))).await);
    assert!(hub.events(&sid).await.is_empty());

    for i in 0..3 {
        assert_eq!(
            ask(&mut w, request(&format!("ok-{i}"), ten_min)).await["type"],
            "ack"
        );
    }
    refused(&ask(&mut w, request("one-too-many", ten_min)).await);
    // A resend of an open request is not a new one.
    assert_eq!(ask(&mut w, request("ok-0", ten_min)).await["type"], "ack");
    // Answering one frees a place.
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/ok-1"),
            json!({"option_id": "deny"}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let f = w.expect("worker.approval.response").await;
    w.reply(&f["id"], "ack", json!({})).await;
    assert_eq!(ask(&mut w, request("ok-3", ten_min)).await["type"], "ack");
    let types: Vec<Value> = hub
        .events(&sid)
        .await
        .iter()
        .map(|e| e["type"].clone())
        .collect();
    assert_eq!(
        types,
        [
            "approval.request",
            "approval.request",
            "approval.request",
            "approval.response",
            "approval.request"
        ]
    );
    hub.finish().await;
}

/// The worker's own requests push at most once per 10 s.
#[tokio::test]
async fn worker_requests_push_at_most_every_10_s() {
    let hub = TestHub::start().await;
    subscribe_push(&hub).await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, ack) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = ack["payload"]["system_session_id"]
        .as_str()
        .unwrap()
        .to_string();
    for i in 0..3 {
        let r = ask(&mut w, request(&format!("p-{i}"), TimeDelta::minutes(10))).await;
        assert_eq!(r["type"], "ack");
    }
    assert_eq!(hub.events(&sid).await.len(), 3);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let sent = hub.push.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].1["tag"], format!("{sid}:p-0"));
    hub.finish().await;
}
