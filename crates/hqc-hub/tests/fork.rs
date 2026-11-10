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

/// The runtime session id a `session.start` names.
fn runtime_of(start: &Value) -> String {
    start["payload"]["runtime_session_id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn fork(hub: &TestHub, sid: &str, at: &Value) -> String {
    let (s, f) = hub
        .post(
            &format!("/api/sessions/{sid}/fork"),
            json!({"event_id": at["event_id"]}),
        )
        .await;
    assert_eq!(s, 201, "{f}");
    f["id"].as_str().unwrap().to_string()
}

/// SPEC §6: a fork before a message that came while the agent's text was being written keeps
/// the answer the client shows above it: the newest cut and the last copied event.
#[tokio::test]
async fn a_fork_before_a_message_written_during_the_answer_keeps_the_answer() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Words").await;
    ack_start(&mut w, &sid).await;
    let m1 = input(&hub, &mut w, &sid, "Remember APPLE.").await;
    w.event(agent_event(&sid, "agent.delta", json!({"text": "OK"})))
        .await;
    let (s, r) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "And BANANA."}),
        )
        .await;
    assert_eq!(s, 201, "{r}");
    let m2 = r["event"].clone();
    assert_eq!(m2["payload"]["during_text"], true);
    let si = w.expect("session.input").await;
    w.reply(
        &si["id"],
        "ack",
        json!({"event_id": m2["event_id"], "joined": true}),
    )
    .await;
    let a1 = agent_event(
        &sid,
        "agent.message",
        json!({"text": "OK, APPLE.", "final": true, "in_reply_to": m1["event_id"]}),
    );
    w.event(a1.clone()).await;

    let f = fork(&hub, &sid, &m2).await;
    let fs = ack_start(&mut w, &f).await;
    assert_eq!(
        fs["payload"]["fork"]["at_event_ids"],
        json!([a1["event_id"]])
    );
    let texts: Vec<Value> = hub
        .events(&f)
        .await
        .iter()
        .map(|e| e["payload"]["text"].clone())
        .collect();
    assert_eq!(
        &texts[1..3],
        [json!("Remember APPLE."), json!("OK, APPLE.")]
    );
    assert!(!texts.contains(&json!("And BANANA.")), "{texts:?}");
    hub.finish().await;
}

/// SPEC §6: an answer to the message itself is no answer shown above it, so a fork before the
/// message neither cuts at it nor copies it.
#[tokio::test]
async fn a_fork_before_a_message_written_during_the_answer_leaves_out_its_own_answer() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Words").await;
    ack_start(&mut w, &sid).await;
    input(&hub, &mut w, &sid, "Remember APPLE.").await;
    let a0 = reply(&mut w, &sid, "OK, APPLE.").await;
    input(&hub, &mut w, &sid, "Remember BANANA.").await;
    w.event(agent_event(&sid, "agent.delta", json!({"text": "OK"})))
        .await;
    let (s, r) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "And CHERRY."}),
        )
        .await;
    assert_eq!(s, 201, "{r}");
    let m2 = r["event"].clone();
    assert_eq!(m2["payload"]["during_text"], true);
    let si = w.expect("session.input").await;
    w.reply(
        &si["id"],
        "ack",
        json!({"event_id": m2["event_id"], "joined": true}),
    )
    .await;
    w.event(agent_event(
        &sid,
        "agent.message",
        json!({"text": "OK, CHERRY.", "final": true, "in_reply_to": m2["event_id"]}),
    ))
    .await;

    let f = fork(&hub, &sid, &m2).await;
    let fs = ack_start(&mut w, &f).await;
    assert_eq!(
        fs["payload"]["fork"]["at_event_ids"],
        json!([a0["event_id"]])
    );
    let texts: Vec<Value> = hub
        .events(&f)
        .await
        .iter()
        .map(|e| e["payload"]["text"].clone())
        .collect();
    assert!(texts.contains(&json!("Remember BANANA.")), "{texts:?}");
    assert!(!texts.contains(&json!("OK, CHERRY.")), "{texts:?}");
    hub.finish().await;
}

/// A fork of a fork: before a message of its copied history it cuts in the transcript that
/// history was first written in, by the ids the runtime gave it, even with the fork between
/// never run and the first session deleted; before a message after its own turns, in its own
/// transcript.
#[tokio::test]
async fn a_fork_of_a_fork_cuts_where_the_records_are() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let a = hub.create_session("Words").await;
    let start = ack_start(&mut w, &a).await;
    input(&hub, &mut w, &a, "Remember APPLE.").await;
    let a1 = reply(&mut w, &a, "OK, APPLE.").await;
    input(&hub, &mut w, &a, "Remember BANANA.").await;
    reply(&mut w, &a, "OK, BANANA.").await;
    let m3 = input(&hub, &mut w, &a, "Remember CHERRY.").await;

    // The first fork, before CHERRY; it never runs a turn.
    let f1 = fork(&hub, &a, &m3).await;
    let s1 = ack_start(&mut w, &f1).await;
    let copied = hub.events(&f1).await;
    let banana = copied
        .iter()
        .find(|e| e["payload"]["text"] == "Remember BANANA.")
        .unwrap()
        .clone();
    assert_eq!(
        s1["payload"]["fork"]["runtime_session_id"],
        runtime_of(&start)
    );

    // A fork of it before BANANA, a copied message: A's transcript, A's record of APPLE's
    // answer, also once A is deleted (its transcript stays on the worker, SPEC §17.4).
    assert_eq!(hub.delete(&format!("/api/sessions/{a}")).await, 204);
    w.expect("session.kill").await;
    let f2 = fork(&hub, &f1, &banana).await;
    let s2 = ack_start(&mut w, &f2).await;
    assert_eq!(
        s2["payload"]["fork"],
        json!({"runtime_session_id": runtime_of(&start), "at_event_ids": [a1["event_id"]]})
    );
    let texts: Vec<Value> = hub
        .events(&f2)
        .await
        .iter()
        .map(|e| e["payload"]["text"].clone())
        .collect();
    assert_eq!(
        &texts[1..3],
        [json!("Remember APPLE."), json!("OK, APPLE.")]
    );

    // The second fork runs a turn of its own; a fork of it after that turn cuts in its own
    // transcript, at its own answer only.
    input(&hub, &mut w, &f2, "Remember DATE.").await;
    let own = reply(&mut w, &f2, "OK, DATE.").await;
    let m = input(&hub, &mut w, &f2, "Remember ELDER.").await;
    let f3 = fork(&hub, &f2, &m).await;
    let s3 = ack_start(&mut w, &f3).await;
    assert_eq!(
        s3["payload"]["fork"],
        json!({"runtime_session_id": runtime_of(&s2), "at_event_ids": [own["event_id"]]})
    );
    hub.finish().await;
}
