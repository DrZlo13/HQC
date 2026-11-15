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

/// SPEC §15: a fork's copy gets its search words without the answer waiting for them, and adds
/// none to `search_vocab` (the source's are there).
#[tokio::test]
async fn a_forks_copy_is_indexed_later_and_adds_no_words_to_the_vocabulary() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Words").await;
    ack_start(&mut w, &sid).await;
    input(&hub, &mut w, &sid, "Remember PINEAPPLE.").await;
    reply(&mut w, &sid, "OK, PINEAPPLE.").await;
    let m2 = input(&hub, &mut w, &sid, "Remember BANANA.").await;
    // The source's events put the word into the vocabulary; take it out to see a copy add it.
    let db = &hub.app.db;
    let vocab = "SELECT count(*) FROM search_vocab WHERE word LIKE 'pineappl%'";
    let words = || async {
        sqlx::query_scalar::<_, i64>(vocab)
            .fetch_one(db)
            .await
            .unwrap()
    };
    assert!(words().await > 0);
    sqlx::query("DELETE FROM search_vocab WHERE word LIKE 'pineappl%'")
        .execute(db)
        .await
        .unwrap();

    let fid = fork(&hub, &sid, &m2).await;
    let fork_id = uuid::Uuid::parse_str(&fid).unwrap();
    // Both copied texts with the word come into the index.
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let found: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM events
             WHERE session_id = $1 AND search_words @@ to_tsquery('hqc', 'pineapple')",
        )
        .bind(fork_id)
        .fetch_one(db)
        .await
        .unwrap();
        if found == 2 {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "indexed: {found}");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(words().await, 0);
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

/// SPEC §6: a message that came while a turn ran and waited for a turn of its own stands after
/// the turn it waited through; a fork before it keeps that turn: its calls and its answer, the
/// newest cut.
#[tokio::test]
async fn a_fork_before_a_message_that_waited_for_its_turn_keeps_the_turn_it_waited_through() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Words").await;
    ack_start(&mut w, &sid).await;
    let m1 = input(&hub, &mut w, &sid, "Run the tests.").await;
    w.event(agent_event(
        &sid,
        "tool.call",
        json!({"call_id": "c1", "tool": "Bash", "args_preview": "{\"command\":\"make test\"}"}),
    ))
    .await;
    // A slash command written meanwhile waits for the end of the turn (SPEC §17.2).
    let (s, r) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "/cost"}),
        )
        .await;
    assert_eq!(s, 201, "{r}");
    let m2 = r["event"].clone();
    let si = w.expect("session.input").await;
    w.event(agent_event(
        &sid,
        "tool.result",
        json!({"call_id": "c1", "result_preview": "ok", "error": false}),
    ))
    .await;
    let a1 = agent_event(
        &sid,
        "agent.message",
        json!({"text": "Tests pass.", "final": true, "in_reply_to": m1["event_id"]}),
    );
    w.event(a1.clone()).await;
    w.event(agent_event(
        &sid,
        "status",
        json!({"state": "idle", "in_reply_to": m1["event_id"]}),
    ))
    .await;
    w.reply(&si["id"], "ack", json!({"event_id": m2["event_id"]}))
        .await;
    w.event(agent_event(
        &sid,
        "agent.message",
        json!({"text": "Total cost: $0.01", "final": true, "in_reply_to": m2["event_id"]}),
    ))
    .await;

    let f = fork(&hub, &sid, &m2).await;
    let fs = ack_start(&mut w, &f).await;
    assert_eq!(
        fs["payload"]["fork"]["at_event_ids"],
        json!([a1["event_id"]])
    );
    let shown: Vec<(Value, Value)> = hub
        .events(&f)
        .await
        .iter()
        .map(|e| (e["type"].clone(), e["payload"]["text"].clone()))
        .collect();
    assert_eq!(
        &shown[1..5],
        [
            (json!("user.message"), json!("Run the tests.")),
            (json!("tool.call"), Value::Null),
            (json!("tool.result"), Value::Null),
            (json!("agent.message"), json!("Tests pass.")),
        ]
    );
    assert_eq!(shown.len(), 6, "{shown:?}");
    hub.finish().await;
}

/// SPEC §6: a message written during the agent's text that the agent took only after that turn
/// stands after the turn, as any message that waited does; a fork before it keeps the whole
/// turn, not only the text being written when it came.
#[tokio::test]
async fn a_fork_before_a_message_written_during_the_text_that_waited_keeps_the_whole_turn() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Words").await;
    ack_start(&mut w, &sid).await;
    let m1 = input(&hub, &mut w, &sid, "Run the tests.").await;
    w.event(agent_event(
        &sid,
        "agent.delta",
        json!({"text": "Let me run them."}),
    ))
    .await;
    let (s, r) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "And the linter."}),
        )
        .await;
    assert_eq!(s, 201, "{r}");
    let m2 = r["event"].clone();
    assert_eq!(m2["payload"]["during_text"], true);
    // The worker holds it to the end of the turn.
    let si = w.expect("session.input").await;
    let said = agent_event(
        &sid,
        "agent.message",
        json!({"text": "Let me run them.", "final": false}),
    );
    w.event(said.clone()).await;
    w.event(agent_event(
        &sid,
        "tool.call",
        json!({"call_id": "c1", "tool": "Bash", "args_preview": "{\"command\":\"make test\"}"}),
    ))
    .await;
    w.event(agent_event(
        &sid,
        "tool.result",
        json!({"call_id": "c1", "result_preview": "ok", "error": false}),
    ))
    .await;
    let a1 = agent_event(
        &sid,
        "agent.message",
        json!({"text": "Tests pass.", "final": true, "in_reply_to": m1["event_id"]}),
    );
    w.event(a1.clone()).await;
    w.event(agent_event(
        &sid,
        "status",
        json!({"state": "idle", "in_reply_to": m1["event_id"]}),
    ))
    .await;
    w.reply(&si["id"], "ack", json!({"event_id": m2["event_id"]}))
        .await;
    w.event(agent_event(
        &sid,
        "tool.call",
        json!({"call_id": "c2", "tool": "Bash", "args_preview": "{\"command\":\"make lint\"}"}),
    ))
    .await;
    w.event(agent_event(
        &sid,
        "agent.message",
        json!({"text": "Lint is clean.", "final": true, "in_reply_to": m2["event_id"]}),
    ))
    .await;

    let f = fork(&hub, &sid, &m2).await;
    let fs = ack_start(&mut w, &f).await;
    assert_eq!(
        fs["payload"]["fork"]["at_event_ids"],
        json!([a1["event_id"], said["event_id"]])
    );
    let shown: Vec<(Value, Value)> = hub
        .events(&f)
        .await
        .iter()
        .map(|e| (e["type"].clone(), e["payload"]["text"].clone()))
        .collect();
    assert_eq!(
        &shown[1..6],
        [
            (json!("user.message"), json!("Run the tests.")),
            (json!("agent.message"), json!("Let me run them.")),
            (json!("tool.call"), Value::Null),
            (json!("tool.result"), Value::Null),
            (json!("agent.message"), json!("Tests pass.")),
        ]
    );
    assert_eq!(shown.len(), 7, "{shown:?}");
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
    // Its note names the fork it was made from, not the copied `forked_from` of that fork.
    let middle = hub.session(&f2).await["slug"].as_str().unwrap().to_string();
    let text = sent_text(&hub, &mut w, &f3, "Remember FIG.").await;
    assert!(
        text.as_str().unwrap().starts_with(&format!(
            "[hqc fork] Forked from personal/{middle} before the message of "
        )),
        "{text}"
    );
    hub.finish().await;
}

/// After `/clear` the runtime goes on in another transcript (SPEC §8.1): a fork before a
/// message of the conversation before it cuts the transcript that conversation was written in,
/// one after it the new one.
#[tokio::test]
async fn a_fork_before_a_clear_cuts_the_transcript_before_it() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Words").await;
    let start = ack_start(&mut w, &sid).await;
    input(&hub, &mut w, &sid, "Remember APPLE.").await;
    let a1 = reply(&mut w, &sid, "OK, APPLE.").await;
    let m2 = input(&hub, &mut w, &sid, "/clear").await;
    let cleared = uuid::Uuid::new_v4().to_string();
    for runtime in [runtime_of(&start), cleared.clone(), cleared.clone()] {
        w.send(
            "session.state",
            json!({"session_id": sid, "process_state": "running", "locked_by": "hqc",
                "runtime_session_id": runtime}),
        )
        .await;
        w.expect("ack").await;
    }
    let a2 = reply(&mut w, &sid, "Nothing to remember.").await;
    let m3 = input(&hub, &mut w, &sid, "What did I say?").await;

    let f = fork(&hub, &sid, &m2).await;
    let fs = ack_start(&mut w, &f).await;
    assert_eq!(
        fs["payload"]["fork"],
        json!({"runtime_session_id": runtime_of(&start), "at_event_ids": [a1["event_id"]]})
    );
    let f = fork(&hub, &sid, &m3).await;
    let fs = ack_start(&mut w, &f).await;
    assert_eq!(
        fs["payload"]["fork"],
        json!({"runtime_session_id": cleared, "at_event_ids": [a2["event_id"]]})
    );
    hub.finish().await;
}

/// The text the worker gets for a person's input, acknowledged.
async fn sent_text(hub: &TestHub, w: &mut FakeWorker, sid: &str, text: &str) -> Value {
    let (s, r) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": text}))
        .await;
    assert_eq!(s, 201, "{r}");
    let si = w.expect("session.input").await;
    w.reply(
        &si["id"],
        "ack",
        json!({"event_id": r["event"]["event_id"]}),
    )
    .await;
    si["payload"]["text"].clone()
}

/// SPEC §6: the agent of a fork with copied history is told it is a copy with the first input
/// after the cut, also when that input is sent again after a reconnect; the feed keeps the
/// text as typed, and a later input, the source and a fork before the first message get none.
#[tokio::test]
async fn the_first_input_of_a_fork_tells_its_agent_it_is_a_copy() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Words").await;
    ack_start(&mut w, &sid).await;
    let m1 = input(&hub, &mut w, &sid, "Remember APPLE.").await;
    reply(&mut w, &sid, "OK, APPLE.").await;
    let m2 = input(&hub, &mut w, &sid, "Remember BANANA.").await;
    reply(&mut w, &sid, "OK, BANANA.").await;

    let f = fork(&hub, &sid, &m2).await;
    ack_start(&mut w, &f).await;
    // The source's inputs go as typed.
    assert_eq!(
        sent_text(&hub, &mut w, &sid, "Remember CHERRY.").await,
        "Remember CHERRY."
    );

    let own = format!(
        "personal/{}",
        hub.session(&f).await["slug"].as_str().unwrap()
    );
    let (s, r) = hub
        .post(
            &format!("/api/sessions/{f}/input"),
            json!({"text": "the typed text"}),
        )
        .await;
    assert_eq!(s, 201, "{r}");
    let eid = r["event"]["event_id"].clone();
    let si = w.expect("session.input").await;
    assert_eq!(si["payload"]["event_id"], eid);
    let text = si["payload"]["text"].as_str().unwrap().to_string();
    assert!(
        text.starts_with("[hqc fork] Forked from personal/words before the message of "),
        "{text}"
    );
    assert!(
        text.contains(&format!("; this session is {own}, separate from it")),
        "{text}"
    );
    assert!(text.ends_with("\n\nthe typed text"), "{text}");

    // Not acknowledged: sent again after a reconnect, with the note again.
    w.close().await;
    hub.wait_session(&f, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    ack_start(&mut w, &f).await;
    let again = w.expect("session.input").await;
    assert_eq!(again["payload"]["event_id"], eid);
    assert_eq!(again["payload"]["text"], text.as_str());
    w.reply(&again["id"], "ack", json!({"event_id": eid})).await;

    // The feed shows what was typed.
    let events = hub.events(&f).await;
    let stored = events.iter().find(|e| e["event_id"] == eid).unwrap();
    assert_eq!(stored["payload"]["text"], "the typed text");

    // The fork's next input goes as typed.
    reply(&mut w, &f, "OK.").await;
    assert_eq!(sent_text(&hub, &mut w, &f, "And then?").await, "And then?");

    // A fork before the first message has no copied history and no note.
    let f0 = fork(&hub, &sid, &m1).await;
    ack_start(&mut w, &f0).await;
    assert_eq!(sent_text(&hub, &mut w, &f0, "Hello.").await, "Hello.");
    hub.finish().await;
}

/// SPEC §6: a slash command first in a fork goes as typed, and a failed first input does not
/// count: the next input carries the note.
#[tokio::test]
async fn a_command_or_a_failed_input_leaves_the_note_to_the_next_input() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Words").await;
    ack_start(&mut w, &sid).await;
    input(&hub, &mut w, &sid, "Remember APPLE.").await;
    reply(&mut w, &sid, "OK, APPLE.").await;
    let m2 = input(&hub, &mut w, &sid, "Remember BANANA.").await;
    let note = "[hqc fork] Forked from personal/words before the message of ";

    let f = fork(&hub, &sid, &m2).await;
    ack_start(&mut w, &f).await;
    assert_eq!(sent_text(&hub, &mut w, &f, "/compact").await, "/compact");
    let text = sent_text(&hub, &mut w, &f, "Go on.").await;
    assert!(text.as_str().unwrap().starts_with(note), "{text}");

    let f = fork(&hub, &sid, &m2).await;
    ack_start(&mut w, &f).await;
    let (s, r) = hub
        .post(
            &format!("/api/sessions/{f}/input"),
            json!({"text": "Go on."}),
        )
        .await;
    assert_eq!(s, 201, "{r}");
    let si = w.expect("session.input").await;
    assert!(
        si["payload"]["text"].as_str().unwrap().starts_with(note),
        "{si}"
    );
    w.reply(
        &si["id"],
        "error",
        json!({"code": "internal", "message": "the inbox is gone", "retryable": false}),
    )
    .await;
    hub.wait_events(&f, "input_rejected", |e| {
        e.iter().any(|e| e["payload"]["code"] == "input_rejected")
    })
    .await;
    let text = sent_text(&hub, &mut w, &f, "Go on, then.").await;
    assert!(text.as_str().unwrap().starts_with(note), "{text}");
    hub.finish().await;
}
