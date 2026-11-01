//! T51 (SPEC §6, §7, §7.1): what the feeds of both sessions need to show an exchange. The
//! reply to an ask gets `reply_delivered_at` in the target's events once the asking worker
//! acknowledged it (and only then: a rejected result is no delivery); a buffered send gets
//! `delivered_at` when the input that carried it is acknowledged; the results of `hqc_send`
//! and `hqc_ask` name the `seq` of the message in the target (`message_seq`).

mod common;

use std::time::Duration;

use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const WS_PATH: &str = "/Users/home-mac/work/hqc";
const OTHER_PATH: &str = "/Users/home-mac/work/other";

/// A (`personal/asker`) on the first worker, B (`personal/helper`) on the second.
struct Setup {
    w1: FakeWorker,
    w2: FakeWorker,
    a: (String, String),
    b: (String, String),
}

async fn setup(hub: &TestHub) -> Setup {
    let (_, t1) = hub.enroll("one").await;
    let (_, t2) = hub.enroll("two").await;
    let (mut w1, _) = FakeWorker::online(hub, &t1, WS_PATH).await;
    let (mut w2, _) = FakeWorker::online(hub, &t2, OTHER_PATH).await;
    let (_, list) = hub.get("/api/workspaces").await;
    let ws_of = |path: &str| -> String {
        list["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["path"] == path)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let a = started(hub, &mut w1, "Asker", &ws_of(WS_PATH)).await;
    let b = started(hub, &mut w2, "Helper", &ws_of(OTHER_PATH)).await;
    Setup { w1, w2, a, b }
}

async fn started(hub: &TestHub, w: &mut FakeWorker, title: &str, ws: &str) -> (String, String) {
    let pid = hub.personal_project().await;
    let (s, v) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": title, "workspace_id": ws}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    let f = ack_start(w, &sid).await;
    (sid, f["payload"]["session_token"].as_str().unwrap().into())
}

async fn ack_input(w: &mut FakeWorker, input: &Value) {
    w.reply(
        &input["id"],
        "ack",
        json!({"event_id": input["payload"]["event_id"]}),
    )
    .await;
}

/// A asks B; B's worker takes the question and answers `text`. The `mcp.result` frame A's
/// worker got (not acknowledged) and the reply's `event_id`.
async fn asked_and_answered(s: &mut Setup, text: &str) -> (Value, Value, String) {
    let rid = Uuid::now_v7().to_string();
    s.w1.send(
        "mcp.call",
        json!({"request_id": rid, "session_token": s.a.1, "tool": "hqc_ask",
               "args": {"text": "Check the file", "session": "personal/helper"}}),
    )
    .await;
    s.w1.expect("ack").await;
    let input = s.w2.expect("session.input").await;
    ack_input(&mut s.w2, &input).await;
    let reply = agent_event(
        &s.b.0,
        "agent.message",
        json!({"text": text, "final": true, "in_reply_to": input["payload"]["event_id"]}),
    );
    let reply_id = reply["event_id"].as_str().unwrap().to_string();
    s.w2.event(reply).await;
    let r = s.w1.expect("mcp.result").await;
    (r, input, reply_id)
}

fn by_id<'a>(events: &'a [Value], id: &str) -> &'a Value {
    events.iter().find(|e| e["event_id"] == id).unwrap()
}

#[tokio::test]
async fn the_reply_gets_reply_delivered_at_after_the_ack() {
    let hub = TestHub::start().await;
    let mut s = setup(&hub).await;
    let b = s.b.0.clone();
    let mut client = FakeClient::connect(&hub).await;
    client.subscribe(&b, 0).await;

    let (r, input, reply_id) = asked_and_answered(&mut s, "Done.").await;
    let question = by_id(
        &hub.events(&b).await,
        input["payload"]["event_id"].as_str().unwrap(),
    )
    .clone();
    assert_eq!(question["type"], "peer.message");
    assert!(question["delivered_at"].is_string(), "{question}");
    // The asker's client finds the question by `message_seq`.
    assert_eq!(
        r["payload"]["result"]["message_seq"], question["seq"],
        "{r}"
    );
    // Not acknowledged yet: no time.
    let reply = by_id(&hub.events(&b).await, &reply_id).clone();
    assert!(reply.get("reply_delivered_at").is_none(), "{reply}");
    assert_eq!(client.until("event").await["event"]["type"], "peer.message");
    assert_eq!(
        client.until("event").await["event"]["event_id"],
        reply_id.as_str()
    );

    s.w1.reply(&r["id"], "ack", json!({})).await;
    // The subscriber gets the reply again, with the time.
    let again = loop {
        let f = client.until("event").await;
        if f["event"]["event_id"] == reply_id.as_str() {
            break f["event"].clone();
        }
    };
    let at = again["reply_delivered_at"]
        .as_str()
        .expect("reply_delivered_at")
        .to_string();
    assert_eq!(again["seq"], reply["seq"]);
    // The events API and a new subscription have it too, unchanged by a repeat of the ask.
    let events = hub.events(&b).await;
    assert_eq!(by_id(&events, &reply_id)["reply_delivered_at"], at.as_str());
    let mut late = FakeClient::connect(&hub).await;
    let backlog = late.subscribe(&b, 0).await;
    assert_eq!(
        by_id(&backlog, &reply_id)["reply_delivered_at"],
        at.as_str()
    );
    // Nothing else of B's carries it.
    assert_eq!(
        events
            .iter()
            .filter(|e| e.get("reply_delivered_at").is_some())
            .count(),
        1
    );
}

#[tokio::test]
async fn a_rejected_result_is_not_a_delivery() {
    let hub = TestHub::start().await;
    let mut s = setup(&hub).await;
    let (r, _, reply_id) = asked_and_answered(&mut s, "Done.").await;
    s.w1.reply(
        &r["id"],
        "error",
        json!({"code": "not_found", "message": "no such call", "retryable": false}),
    )
    .await;
    s.w1.expect_silence(Duration::from_millis(200)).await;
    let reply = by_id(&hub.events(&s.b.0).await, &reply_id).clone();
    assert!(reply.get("reply_delivered_at").is_none(), "{reply}");
}

#[tokio::test]
async fn a_buffered_send_gets_delivered_at_with_the_input_that_carried_it() {
    let hub = TestHub::start().await;
    let mut s = setup(&hub).await;
    let b = s.b.0.clone();
    hub.buffer_sends(&b).await;
    let id =
        s.w1.send(
            "mcp.call",
            json!({"request_id": Uuid::now_v7().to_string(), "session_token": s.a.1,
                   "tool": "hqc_send", "args": {"text": "FYI", "session": "personal/helper"}}),
        )
        .await;
    let r = s.w1.expect("mcp.result").await;
    assert_eq!(r["id"], id.as_str());
    let seq = r["payload"]["result"]["message_seq"].clone();
    assert_eq!(r["payload"]["result"]["delivery"], "buffered", "{r}");
    let sent = hub
        .events(&b)
        .await
        .into_iter()
        .find(|e| e["seq"] == seq)
        .unwrap();
    assert_eq!(sent["type"], "peer.message");
    assert!(sent.get("delivered_at").is_none(), "{sent}");

    hub.post(&format!("/api/sessions/{b}/input"), json!({"text": "go"}))
        .await;
    let input = s.w2.expect("session.input").await;
    assert_eq!(input["payload"]["peer_prefix"][0]["text"], "FYI");
    ack_input(&mut s.w2, &input).await;
    // The ack is handled after the frame is read: wait for it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let sent = loop {
        let sent = hub
            .events(&b)
            .await
            .into_iter()
            .find(|e| e["seq"] == seq)
            .unwrap();
        if sent["delivered_at"].is_string() {
            break sent;
        }
        assert!(tokio::time::Instant::now() < deadline, "{sent}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(sent.get("delivery").is_none(), "{sent}");
}
