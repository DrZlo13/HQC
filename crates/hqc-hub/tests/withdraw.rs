//! SPEC 0.219: `POST /api/sessions/{id}/inputs/{event_id}/withdraw` and `.../now`, the owner's
//! messages that wait for the agent taken back or sent into the running turn at once.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{json, Value};

const WS_PATH: &str = "/Users/home-mac/work/hqc";

/// A person's input of session `sid`: its event.
async fn post_input(hub: &TestHub, sid: &str, text: &str) -> Value {
    let (s, r) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": text}))
        .await;
    assert_eq!(s, 201, "{r}");
    r["event"].clone()
}

fn withdraw_path(sid: &str, event: &Value) -> String {
    format!(
        "/api/sessions/{sid}/inputs/{}/withdraw",
        event["event_id"].as_str().unwrap()
    )
}

fn now_path(sid: &str, event: &Value) -> String {
    format!(
        "/api/sessions/{sid}/inputs/{}/now",
        event["event_id"].as_str().unwrap()
    )
}

/// The texts of the user messages the hub's feed pages of the session return, withdrawn ones
/// too: a client is told of them by the event itself and hides them (SPEC 0.219).
async fn shown(hub: &TestHub, sid: &str) -> Vec<String> {
    hub.events(sid)
        .await
        .iter()
        .filter(|e| e["type"] == "user.message")
        .map(|e| e["payload"]["text"].as_str().unwrap().to_string())
        .collect()
}

/// [`shown`] as a client's timeline shows it: without the messages taken back.
async fn visible(hub: &TestHub, sid: &str) -> Vec<String> {
    hub.events(sid)
        .await
        .iter()
        .filter(|e| e["type"] == "user.message" && e["delivery"] != "withdrawn")
        .map(|e| e["payload"]["text"].as_str().unwrap().to_string())
        .collect()
}

/// A message withdrawn before the worker has it (the worker is away) is `delivery: withdrawn`:
/// an open client is told, the feed pages and the backlog return it as withdrawn (a device that
/// missed the frame learns it from them), no search or fork has it, nothing is delivered when the
/// worker comes, and the queue is empty.
#[tokio::test]
async fn a_message_the_worker_never_had_is_withdrawn_on_the_hub() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Withdraw").await;
    ack_start(&mut w, &sid).await;
    let m1 = post_input(&hub, &sid, "Remember APPLE.").await;
    let si = w.expect("session.input").await;
    w.reply(&si["id"], "ack", json!({"event_id": m1["event_id"]}))
        .await;
    hub.wait_session(&sid, "delivered", |v| v["queue_len"] == 0)
        .await;
    w.event(agent_event(
        &sid,
        "agent.message",
        json!({"text": "OK, APPLE.", "final": true}),
    ))
    .await;
    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;

    let mut c = FakeClient::connect(&hub).await;
    c.subscribe(&sid, 0).await;
    let m2 = post_input(&hub, &sid, "Remember a quokkaword.").await;
    c.until("event").await;
    assert_eq!(hub.session(&sid).await["queue_len"], 1);
    let (s, r) = hub.post(&withdraw_path(&sid, &m2), json!({})).await;
    assert_eq!((s, &r), (200, &json!({"withdrawn": true})));
    let d = c.until("delivery").await;
    assert_eq!(
        (&d["event_id"], &d["delivery"]),
        (&m2["event_id"], &json!("withdrawn"))
    );
    assert_eq!(hub.session(&sid).await["queue_len"], 0);

    // The pages of the feed, from the start or backwards, and the backlog of a new subscription
    // return it as withdrawn: a client that missed the `delivery` frame hides it by that.
    assert_eq!(visible(&hub, &sid).await, ["Remember APPLE."]);
    let (s, v) = hub
        .get(&format!("/api/sessions/{sid}/events?before_seq=1000"))
        .await;
    assert_eq!(s, 200, "{v}");
    let back = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event_id"] == m2["event_id"])
        .expect("the page backwards returns the withdrawn message");
    assert_eq!(back["delivery"], "withdrawn");
    let mut late = FakeClient::connect(&hub).await;
    let backlog = late.subscribe(&sid, 0).await;
    let seen = backlog
        .iter()
        .find(|e| e["event_id"] == m2["event_id"])
        .expect("the backlog returns the withdrawn message");
    assert_eq!(seen["delivery"], "withdrawn");
    // Search does not find it.
    let (s, v) = hub.get("/api/search?q=quokkaword").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["results"].as_array().unwrap().len(), 0, "{v}");
    // The worker comes back: nothing to deliver.
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    w.expect_silence(Duration::from_millis(300)).await;

    // A fork before a later message copies the conversation without it.
    let m3 = post_input(&hub, &sid, "Remember BANANA.").await;
    ack_start(&mut w, &sid).await;
    let si = w.expect("session.input").await;
    assert_eq!(si["payload"]["event_id"], m3["event_id"]);
    w.reply(&si["id"], "ack", json!({"event_id": m3["event_id"]}))
        .await;
    let (s, f) = hub
        .post(
            &format!("/api/sessions/{sid}/fork"),
            json!({"event_id": m3["event_id"]}),
        )
        .await;
    assert_eq!(s, 201, "{f}");
    let fork = f["id"].as_str().unwrap();
    ack_start(&mut w, fork).await;
    assert_eq!(shown(&hub, fork).await, ["Remember APPLE."]);

    // It is withdrawn once: a second press says so, and no worker is asked.
    let (s, r) = hub.post(&withdraw_path(&sid, &m2), json!({})).await;
    assert_eq!((s, r["error"].as_str()), (409, Some("withdrawn")), "{r}");
    assert_eq!(r["message"], "this message was taken back already");
    hub.finish().await;
}

/// A message the worker holds is taken back by the worker's word: `session.withdraw` asks, `true`
/// withdraws it and nothing is delivered of it later, `false` is `409 already_taken` and the
/// message goes on to its ack.
#[tokio::test]
async fn a_message_the_worker_holds_is_withdrawn_by_its_word() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Withdraw held").await;
    ack_start(&mut w, &sid).await;

    let m1 = post_input(&hub, &sid, "First").await;
    let si = w.expect("session.input").await;
    assert_eq!(si["payload"]["event_id"], m1["event_id"]);
    let path = withdraw_path(&sid, &m1);
    let answer = async {
        let f = w.expect("session.withdraw").await;
        assert_eq!(
            f["payload"],
            json!({"request_id": f["id"], "session_id": sid, "event_id": m1["event_id"]})
        );
        w.reply(
            &f["id"],
            "result",
            json!({"request_id": f["id"], "withdrawn": true}),
        )
        .await;
        w
    };
    let ((s, r), mut w) = tokio::join!(hub.post(&path, json!({})), answer);
    assert_eq!((s, &r), (200, &json!({"withdrawn": true})));
    assert!(visible(&hub, &sid).await.is_empty());
    assert_eq!(hub.session(&sid).await["queue_len"], 0);

    // The worker had taken the second one already.
    let m2 = post_input(&hub, &sid, "Second").await;
    let si = w.expect("session.input").await;
    assert_eq!(
        si["payload"]["event_id"], m2["event_id"],
        "the first is not delivered again"
    );
    let path = withdraw_path(&sid, &m2);
    let answer = async {
        let f = w.expect("session.withdraw").await;
        w.reply(
            &f["id"],
            "result",
            json!({"request_id": f["id"], "withdrawn": false}),
        )
        .await;
        w
    };
    let ((s, r), mut w) = tokio::join!(hub.post(&path, json!({})), answer);
    assert_eq!(
        (s, r["error"].as_str()),
        (409, Some("already_taken")),
        "{r}"
    );
    assert_eq!(visible(&hub, &sid).await, ["Second"]);
    w.reply(&si["id"], "ack", json!({"event_id": m2["event_id"]}))
        .await;
    hub.wait_session(&sid, "delivered", |v| v["queue_len"] == 0)
        .await;
    let (s, r) = hub.post(&path, json!({})).await;
    assert_eq!(
        (s, r["error"].as_str()),
        (409, Some("already_taken")),
        "delivered: {r}"
    );
    w.expect_silence(Duration::from_millis(200)).await;
    hub.finish().await;
}

/// Not the owner's waiting message: an unknown event, an event that is no user message, a
/// session archived, one the worker's service session is, are refused; the worker is not asked.
#[tokio::test]
async fn only_the_owners_waiting_message_is_withdrawn_or_sent_now() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Refusals").await;
    ack_start(&mut w, &sid).await;
    let m = post_input(&hub, &sid, "Waiting").await;
    w.expect("session.input").await;

    let unknown = json!({"event_id": uuid::Uuid::new_v4()});
    for path in [withdraw_path(&sid, &unknown), now_path(&sid, &unknown)] {
        let (s, _) = hub.post(&path, json!({})).await;
        assert_eq!(s, 404, "{path}");
    }
    // The session's own system line is no message.
    let system = hub
        .events(&sid)
        .await
        .into_iter()
        .find(|e| e["type"] == "system")
        .unwrap();
    for path in [withdraw_path(&sid, &system), now_path(&sid, &system)] {
        let (s, _) = hub.post(&path, json!({})).await;
        assert_eq!(s, 404, "{path}");
    }
    // A note of the hub's own is a `user.message` too, but the owner typed none of it: the
    // note of a subagent's leftovers waits for its ack, and is no message to take back.
    w.send(
        "session.subagent_leftovers",
        json!({"session_id": sid, "subagent": "Mapper", "ended_s": 1805,
               "commands": [{"task_id": "bx1", "description": "tail -f x", "running_s": 1850}]}),
    )
    .await;
    // The frame is acked, and the note goes as an input.
    let got = [w.recv().await, w.recv().await];
    let input = got.iter().find(|f| f["type"] == "session.input").unwrap();
    let note = json!({"event_id": input["payload"]["event_id"]});
    for path in [withdraw_path(&sid, &note), now_path(&sid, &note)] {
        let (s, r) = hub.post(&path, json!({})).await;
        assert_eq!(s, 404, "{path}: {r}");
    }
    // An event of another session is no message of this one.
    let other = hub.create_session("Another").await;
    ack_start(&mut w, &other).await;
    for path in [withdraw_path(&other, &m), now_path(&other, &m)] {
        let (s, _) = hub.post(&path, json!({})).await;
        assert_eq!(s, 404, "{path}");
    }
    w.expect_silence(Duration::from_millis(200)).await;

    let (s, _) = hub
        .post(&format!("/api/sessions/{sid}/archive"), json!({}))
        .await;
    assert_eq!(s, 200);
    for path in [withdraw_path(&sid, &m), now_path(&sid, &m)] {
        let (s, r) = hub.post(&path, json!({})).await;
        assert_eq!(
            (s, r["error"].as_str()),
            (409, Some("archived")),
            "{path}: {r}"
        );
    }
    hub.finish().await;
}

/// Send now asks the worker with `session.input_now` for a message it holds; `sent_now` comes
/// back as it is, `error not_now` is `409 not_now`; a message the worker was never given (it is
/// away) is `409 not_now` with no frame sent; the message stays what it was.
#[tokio::test]
async fn send_now_goes_to_the_worker() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Send now").await;
    ack_start(&mut w, &sid).await;
    let m = post_input(&hub, &sid, "Now please").await;
    let si = w.expect("session.input").await;
    let path = now_path(&sid, &m);

    let answer = async {
        let f = w.expect("session.input_now").await;
        assert_eq!(
            f["payload"],
            json!({"request_id": f["id"], "session_id": sid, "event_id": m["event_id"]})
        );
        w.reply(
            &f["id"],
            "result",
            json!({"request_id": f["id"], "sent_now": true}),
        )
        .await;
        w
    };
    let ((s, r), mut w) = tokio::join!(hub.post(&path, json!({})), answer);
    assert_eq!((s, &r), (200, &json!({"sent_now": true})));

    let answer = async {
        let f = w.expect("session.input_now").await;
        w.reply(
            &f["id"],
            "result",
            json!({"request_id": f["id"], "sent_now": false}),
        )
        .await;
        w
    };
    let ((s, r), mut w) = tokio::join!(hub.post(&path, json!({})), answer);
    assert_eq!((s, &r), (200, &json!({"sent_now": false})));

    let answer = async {
        let f = w.expect("session.input_now").await;
        w.reply(
            &f["id"],
            "error",
            json!({"code": "not_now", "message": "no turn takes input now", "retryable": false}),
        )
        .await;
        w
    };
    let ((s, r), mut w) = tokio::join!(hub.post(&path, json!({})), answer);
    assert_eq!((s, r["error"].as_str()), (409, Some("not_now")), "{r}");
    assert_eq!(shown(&hub, &sid).await, ["Now please"]);
    w.reply(&si["id"], "ack", json!({"event_id": m["event_id"]}))
        .await;
    hub.wait_session(&sid, "delivered", |v| v["queue_len"] == 0)
        .await;

    // Never handed to the worker: nothing to move.
    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let m2 = post_input(&hub, &sid, "Later").await;
    let (s, r) = hub.post(&now_path(&sid, &m2), json!({})).await;
    assert_eq!((s, r["error"].as_str()), (409, Some("not_now")), "{r}");
    hub.finish().await;
}

/// The fork's cut points and the generated title read the session's events by their own
/// queries: a message the owner took back is none of them, as it is none of the feed (SPEC 0.219).
#[tokio::test]
async fn a_withdrawn_message_is_none_of_a_forks_cut_or_a_title() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Withdraw").await;
    // The worker never answers the start: the session waits with its messages queued.
    w.expect("session.start").await;
    let id: uuid::Uuid = sid.parse().unwrap();
    let m1 = post_input(&hub, &sid, "Take this back.").await;
    let (s, r) = hub.post(&withdraw_path(&sid, &m1), json!({})).await;
    assert_eq!((s, &r), (200, &json!({"withdrawn": true})));
    let m2 = post_input(&hub, &sid, "Keep this one.").await;

    let first = hqc_hub::db::first_text(&hub.app.db, id, "user.message")
        .await
        .unwrap();
    assert_eq!(first.as_deref(), Some("Keep this one."));
    let marks = hqc_hub::db::turn_marks(&hub.app.db, id).await.unwrap();
    let ids: Vec<String> = marks
        .iter()
        .filter(|m| m.kind == "user.message")
        .map(|m| m.event_id.to_string())
        .collect();
    assert_eq!(ids, [m2["event_id"].as_str().unwrap()]);
    let after = hqc_hub::db::first_after_inputs_during_text(
        &hub.app.db,
        id,
        m1["seq"].as_i64().unwrap() - 1,
    )
    .await
    .unwrap()
    .expect("an event after the one before the withdrawn message");
    assert_ne!(after.event_id.to_string(), m1["event_id"].as_str().unwrap());
}

/// SPEC 0.219: `human: true` is in the frame of what the owner typed, and in no other input:
/// the note of a subagent's leftovers, which is a `user.message` of the hub's own, has none, and
/// the owner's input with messages of other sessions in front of it has none either (the worker
/// writes `origin: human` for `human` alone).
#[tokio::test]
async fn only_the_owners_input_frame_is_human() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Human").await;
    ack_start(&mut w, &sid).await;

    let m = post_input(&hub, &sid, "Typed by the owner").await;
    assert_eq!(m["payload"]["human"], true, "{m}");
    let si = w.expect("session.input").await;
    assert_eq!(si["payload"]["human"], true, "{si}");
    w.reply(&si["id"], "ack", json!({"event_id": m["event_id"]}))
        .await;
    hub.wait_session(&sid, "delivered", |v| v["queue_len"] == 0)
        .await;

    w.send(
        "session.subagent_leftovers",
        json!({"session_id": sid, "subagent": "Mapper", "ended_s": 1805,
               "commands": [{"task_id": "bx1", "description": "tail -f x", "running_s": 1850}]}),
    )
    .await;
    let got = [w.recv().await, w.recv().await];
    let note = got.iter().find(|f| f["type"] == "session.input").unwrap();
    assert!(note["payload"].get("human").is_none(), "{note}");
    hub.finish().await;
}

/// SPEC 0.219: a message the hub handed to a worker is held by it across a reconnect: after the
/// connection it was handed on is gone the hub cannot tell whether the agent has taken it, and
/// withdraws nothing on its own word (`409 worker_offline`); it is delivered again as it was,
/// not lost and not twice. One never handed (the worker was away) is withdrawn on the hub.
#[tokio::test]
async fn a_message_handed_in_an_earlier_connection_is_not_withdrawn_without_the_worker() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Handed").await;
    ack_start(&mut w, &sid).await;
    let m = post_input(&hub, &sid, "Handed over").await;
    let si = w.expect("session.input").await;
    assert_eq!(si["payload"]["event_id"], m["event_id"]);
    // The worker keeps it, unacknowledged, and the connection goes.
    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;

    let (s, r) = hub.post(&withdraw_path(&sid, &m), json!({})).await;
    assert_eq!(
        (s, r["error"].as_str()),
        (409, Some("worker_offline")),
        "{r}"
    );
    assert_eq!(hub.session(&sid).await["queue_len"], 1);
    assert_eq!(visible(&hub, &sid).await, ["Handed over"]);

    // It is handed again, and its worker can be asked about it.
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    ack_start(&mut w, &sid).await;
    let again = w.expect("session.input").await;
    assert_eq!(again["payload"]["event_id"], m["event_id"]);
    let path = withdraw_path(&sid, &m);
    let answer = async {
        let f = w.expect("session.withdraw").await;
        w.reply(
            &f["id"],
            "result",
            json!({"request_id": f["id"], "withdrawn": true}),
        )
        .await;
        w
    };
    let ((s, r), w) = tokio::join!(hub.post(&path, json!({})), answer);
    assert_eq!((s, &r), (200, &json!({"withdrawn": true})));
    assert_eq!(hub.session(&sid).await["queue_len"], 0);

    // Never handed: the worker is away when the message is typed.
    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let m2 = post_input(&hub, &sid, "Not handed").await;
    let (s, r) = hub.post(&withdraw_path(&sid, &m2), json!({})).await;
    assert_eq!((s, &r), (200, &json!({"withdrawn": true})));
    hub.finish().await;
}

/// SPEC 0.219: the hub marks an input handed before it sends it, and sends none that is no
/// longer pending: a withdrawal between the read of the pending inputs and the send leaves the
/// message unsent; marked once, it keeps its first time.
#[tokio::test]
async fn an_input_withdrawn_meanwhile_is_not_handed_over() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Handed mark").await;
    // The worker does not answer the start yet: nothing is handed.
    let start = w.expect("session.start").await;
    let m1 = post_input(&hub, &sid, "Taken back").await;
    let m2 = post_input(&hub, &sid, "Kept").await;
    let id = |m: &Value| -> uuid::Uuid { m["event_id"].as_str().unwrap().parse().unwrap() };
    let handed_at = |m: &Value| {
        let e = id(m);
        let db = hub.app.db.clone();
        async move {
            sqlx::query_scalar::<_, Option<chrono::DateTime<chrono::Utc>>>(
                "SELECT handed_at FROM events WHERE event_id = $1",
            )
            .bind(e)
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };
    assert!(handed_at(&m1).await.is_none());
    let (s, r) = hub.post(&withdraw_path(&sid, &m1), json!({})).await;
    assert_eq!((s, &r), (200, &json!({"withdrawn": true})));
    // What `kick` does with an input its read of the pending ones still lists.
    assert!(!hqc_hub::db::mark_input_handed(&hub.app.db, id(&m1))
        .await
        .unwrap());
    assert!(handed_at(&m1).await.is_none());
    assert!(hqc_hub::db::mark_input_handed(&hub.app.db, id(&m2))
        .await
        .unwrap());
    let first = handed_at(&m2).await.expect("marked");
    assert!(hqc_hub::db::mark_input_handed(&hub.app.db, id(&m2))
        .await
        .unwrap());
    assert_eq!(handed_at(&m2).await, Some(first), "the first time stays");

    // The start is answered: the kept input is handed, the one taken back never.
    w.reply(&start["id"], "ack", json!({})).await;
    let si = w.expect("session.input").await;
    assert_eq!(si["payload"]["event_id"], m2["event_id"]);
    w.expect_silence(Duration::from_millis(300)).await;
    hub.finish().await;
}

/// SPEC 0.219: a worker's own `error` for `session.withdraw` or `session.input_now` reaches the
/// owner as `502` with the worker's code, not as `400 invalid_path`; `not_now` stays `409`.
#[tokio::test]
async fn a_workers_error_for_withdraw_or_send_now_is_a_502() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Errors").await;
    ack_start(&mut w, &sid).await;
    let m = post_input(&hub, &sid, "Held").await;
    w.expect("session.input").await;
    for (path, ty) in [
        (withdraw_path(&sid, &m), "session.withdraw"),
        (now_path(&sid, &m), "session.input_now"),
    ] {
        let answer = async {
            let f = w.expect(ty).await;
            w.reply(
                &f["id"],
                "error",
                json!({"code": "internal", "message": "the runtime is hung", "retryable": true}),
            )
            .await;
            w
        };
        let ((s, r), back) = tokio::join!(hub.post(&path, json!({})), answer);
        w = back;
        assert_eq!(
            (s, r["error"].as_str(), r["message"].as_str()),
            (502, Some("internal"), Some("the runtime is hung")),
            "{ty}: {r}"
        );
    }
    // The message is as it was.
    assert_eq!(visible(&hub, &sid).await, ["Held"]);
    assert_eq!(hub.session(&sid).await["queue_len"], 1);
    hub.finish().await;
}

/// Pauses `sid` the way a worker sees it: `session.kill`, answered. The worker's queue is
/// cleared by it, and so are the hub's books of what it was handed.
async fn pause(hub: &TestHub, w: &mut FakeWorker, sid: &str) {
    let (s, r) = hub
        .post(&format!("/api/sessions/{sid}/pause"), json!({}))
        .await;
    assert_eq!(s, 200, "{r}");
    let kill = w.expect("session.kill").await;
    w.send(
        "session.state",
        json!({"session_id": sid, "process_state": "stopped", "locked_by": "none"}),
    )
    .await;
    w.expect("ack").await;
    w.reply(&kill["id"], "ack", json!({})).await;
}

/// SPEC 0.219: a kill (a pause) clears the hub's books of what the worker was handed and the
/// worker's queue, but the worker's connection stays: a handed message of a paused session is
/// taken back by asking that connection, whatever its start state, and the worker holds
/// nothing. While the request is outstanding the hub hands the message to nobody and a second
/// take-back of it is `409 withdrawing`. Never sent after the resume.
#[tokio::test]
async fn a_handed_message_of_a_paused_session_is_withdrawn() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Paused withdraw").await;
    ack_start(&mut w, &sid).await;
    let m = post_input(&hub, &sid, "Taken back in a pause").await;
    let si = w.expect("session.input").await;
    assert_eq!(si["payload"]["event_id"], m["event_id"]);
    pause(&hub, &mut w, &sid).await;

    let path = withdraw_path(&sid, &m);
    let script = async {
        let f = w.expect("session.withdraw").await;
        assert_eq!(f["payload"]["event_id"], m["event_id"]);
        // The first request is outstanding.
        let (s, r) = hub.post(&path, json!({})).await;
        assert_eq!(
            (s, r["error"].as_str(), r["message"].as_str()),
            (
                409,
                Some("withdrawing"),
                Some("this message is being taken back already")
            ),
            "{r}"
        );
        // The session resumes meanwhile: the hub starts it, and does not hand the message that
        // is being taken back to its worker.
        let (s, r) = hub
            .post(&format!("/api/sessions/{sid}/resume"), json!({}))
            .await;
        assert_eq!(s, 200, "{r}");
        let start = w.expect("session.start").await;
        w.reply(&start["id"], "ack", json!({})).await;
        let note = w.expect("session.input").await;
        assert_ne!(note["payload"]["event_id"], m["event_id"], "{note}");
        w.reply(
            &note["id"],
            "ack",
            json!({"event_id": note["payload"]["event_id"]}),
        )
        .await;
        w.reply(
            &f["id"],
            "result",
            json!({"request_id": f["id"], "withdrawn": true}),
        )
        .await;
        w
    };
    let ((s, r), mut w) = tokio::join!(hub.post(&path, json!({})), script);
    assert_eq!((s, &r), (200, &json!({"withdrawn": true})));
    assert!(visible(&hub, &sid)
        .await
        .iter()
        .all(|t| t != "Taken back in a pause"));
    // It is out for good: the hub hands the agent no more input of it.
    w.expect_silence(Duration::from_millis(300)).await;
    let (s, r) = hub.post(&path, json!({})).await;
    assert_eq!((s, r["error"].as_str()), (409, Some("withdrawn")), "{r}");
    hub.finish().await;
}

/// The worker answers `session.withdraw` after the hub gave up (`504`): the answer is not acted
/// on (SPEC 0.219, §6 limitations), the message is neither withdrawn nor handed again. A second
/// take-back asks the worker again, and its `withdrawn: true` takes the message back.
#[tokio::test]
async fn a_late_answer_is_dropped_and_a_second_take_back_withdraws() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Late withdraw").await;
    ack_start(&mut w, &sid).await;
    let m = post_input(&hub, &sid, "Taken back, late").await;
    let si = w.expect("session.input").await;
    assert_eq!(si["payload"]["event_id"], m["event_id"]);

    let path = withdraw_path(&sid, &m);
    let ((s, r), f) = tokio::join!(hub.post(&path, json!({})), w.expect("session.withdraw"));
    assert_eq!(
        (s, r["error"].as_str()),
        (504, Some("worker_timeout")),
        "{r}"
    );
    w.reply(
        &f["id"],
        "result",
        json!({"request_id": f["id"], "withdrawn": true}),
    )
    .await;
    w.expect_silence(Duration::from_millis(300)).await;
    assert_eq!(visible(&hub, &sid).await, ["Taken back, late"]);

    let script = async {
        let f = w.expect("session.withdraw").await;
        assert_eq!(f["payload"]["event_id"], m["event_id"]);
        w.reply(
            &f["id"],
            "result",
            json!({"request_id": f["id"], "withdrawn": true}),
        )
        .await;
    };
    let ((s, r), ()) = tokio::join!(hub.post(&path, json!({})), script);
    assert_eq!((s, &r), (200, &json!({"withdrawn": true})));
    assert!(visible(&hub, &sid)
        .await
        .iter()
        .all(|t| t != "Taken back, late"));
    hub.finish().await;
}

/// The race in `kick`: it reads the pending inputs, and the owner takes one back before `kick`
/// marks it handed. The row is held by a transaction that withdraws it; `kick` has read it
/// pending and waits on its `UPDATE` (the lock wait is seen in `pg_stat_activity`); the commit
/// lets `kick` go on, and it must not send what it finds withdrawn (its `continue`), only the
/// other input.
#[tokio::test]
async fn kick_sends_no_input_withdrawn_while_it_marks_it_handed() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Kick race").await;
    // The worker does not answer the start yet: nothing is handed.
    let start = w.expect("session.start").await;
    let m1 = post_input(&hub, &sid, "Taken back in the race").await;
    let m2 = post_input(&hub, &sid, "Kept").await;
    let id1: uuid::Uuid = m1["event_id"].as_str().unwrap().parse().unwrap();

    let mut tx = hub.app.db.begin().await.unwrap();
    sqlx::query("SELECT event_id FROM events WHERE event_id = $1 FOR UPDATE")
        .bind(id1)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    // The tests share one server: only a backend this transaction blocks counts.
    let holder: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    // `kick` reads both inputs pending and waits on the lock of the first one's `UPDATE`.
    w.reply(&start["id"], "ack", json!({})).await;
    let mut waiting = false;
    for _ in 0..100 {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity
             WHERE $1 = ANY(pg_blocking_pids(pid)) AND query LIKE 'UPDATE events SET handed_at%'",
        )
        .bind(holder)
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
        if n > 0 {
            waiting = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(waiting, "kick did not wait on the row");
    sqlx::query(
        "UPDATE events SET delivery = 'withdrawn' WHERE event_id = $1 AND delivery = 'pending'",
    )
    .bind(id1)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let si = w.expect("session.input").await;
    assert_eq!(si["payload"]["event_id"], m2["event_id"], "{si}");
    w.expect_silence(Duration::from_millis(300)).await;
    hub.finish().await;
}
