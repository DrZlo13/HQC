//! SPEC §6 `POST /api/sessions/{id}/fork`, §5.3 `session.start.fork`: a session forked before
//! a message of its user.

mod common;

use common::*;
use serde_json::{json, Value};

const WS_PATH: &str = "/Users/home-mac/work/hqc";

/// A person's input, delivered to the worker and acknowledged; its event.
async fn input(hub: &TestHub, w: &mut FakeWorker, sid: &str, text: &str) -> Value {
    let (s, r) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": text}))
        .await;
    assert_eq!(s, 201, "{r}");
    let eid = r["event"]["event_id"].clone();
    let si = w.expect("session.input").await;
    w.reply(&si["id"], "ack", json!({"event_id": eid})).await;
    r["event"].clone()
}

async fn reply(w: &mut FakeWorker, sid: &str, text: &str) -> Value {
    let ev = agent_event(sid, "agent.message", json!({"text": text, "final": true}));
    w.event(ev.clone()).await;
    ev
}

#[tokio::test]
async fn a_fork_before_a_message_starts_from_the_conversation_up_to_it() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Words").await;
    let start = ack_start(&mut w, &sid).await;
    let m1 = input(&hub, &mut w, &sid, "Remember APPLE.").await;
    let a1 = reply(&mut w, &sid, "OK, APPLE.").await;
    let m2 = input(&hub, &mut w, &sid, "Remember BANANA.").await;
    let a2 = reply(&mut w, &sid, "OK, BANANA.").await;

    // Before the second message: the conversation up to the first reply.
    let (s, f) = hub
        .post(
            &format!("/api/sessions/{sid}/fork"),
            json!({"event_id": m2["event_id"]}),
        )
        .await;
    assert_eq!(s, 201, "{f}");
    let fid = f["id"].as_str().unwrap().to_string();
    assert_ne!(fid, sid);
    assert_eq!(f["title"], "Words (fork)");
    assert_eq!(f["unread"], 0);
    let fs = ack_start(&mut w, &fid).await;
    let runtime = start["payload"]["runtime_session_id"]
        .as_str()
        .map(str::to_string)
        .unwrap_or(sid.clone());
    assert_eq!(
        fs["payload"]["fork"],
        json!({"runtime_session_id": runtime, "at_event_ids": [a1["event_id"]]})
    );
    assert_eq!(fs["payload"]["mode"], "new");
    assert_eq!(
        fs["payload"]["workspace_path"],
        start["payload"]["workspace_path"]
    );

    let events = hub.events(&fid).await;
    let shown: Vec<(String, Value)> = events
        .iter()
        .map(|e| {
            (
                e["type"].as_str().unwrap().to_string(),
                e["payload"]["text"].clone(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("system".to_string(), json!("Session created")),
            ("user.message".into(), json!("Remember APPLE.")),
            ("agent.message".into(), json!("OK, APPLE.")),
            ("system".into(), events[3]["payload"]["text"].clone()),
        ]
    );
    assert_eq!(events[3]["payload"]["code"], "forked_from");
    assert!(events[3]["payload"]["text"]
        .as_str()
        .unwrap()
        .starts_with("Forked from personal/words before the message of "));
    // New ids, in order, and no input to deliver.
    let seqs: Vec<i64> = events.iter().map(|e| e["seq"].as_i64().unwrap()).collect();
    assert_eq!(seqs, [1, 2, 3, 4]);
    assert_ne!(events[1]["event_id"], m1["event_id"]);
    assert_ne!(events[2]["event_id"], a1["event_id"]);
    w.expect_silence(std::time::Duration::from_millis(300))
        .await;
    // The source is as it was.
    assert_eq!(hub.events(&sid).await.len(), 5);

    // Before the first message: nothing to fork, a new conversation.
    let (s, f) = hub
        .post(
            &format!("/api/sessions/{sid}/fork"),
            json!({"event_id": m1["event_id"]}),
        )
        .await;
    assert_eq!(s, 201, "{f}");
    let fid = f["id"].as_str().unwrap().to_string();
    let fs = ack_start(&mut w, &fid).await;
    assert!(fs["payload"].get("fork").is_none(), "{fs}");
    let types: Vec<Value> = hub
        .events(&fid)
        .await
        .iter()
        .map(|e| e["type"].clone())
        .collect();
    assert_eq!(types, [json!("system")]);

    // Not a person's message of this session: not found.
    for e in [a2["event_id"].clone(), json!(uuid::Uuid::now_v7())] {
        let (s, _) = hub
            .post(&format!("/api/sessions/{sid}/fork"), json!({"event_id": e}))
            .await;
        assert_eq!(s, 404);
    }
    hub.finish().await;
}
