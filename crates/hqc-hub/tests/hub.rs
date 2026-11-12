//! T2 acceptance (docs/ITERATION-1.md): a fake worker over `/worker/ws`, a fake client over
//! `/api/ws` and REST, against Postgres from `HQC_TEST_DATABASE_URL`.

mod common;

use std::time::Duration;

use chrono::Utc;
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const WS_PATH: &str = "/Users/home-mac/work/hqc";

fn approval_payload(request_id: &str, expires_at: &str) -> Value {
    json!({
        "request_id": request_id,
        "tool": "Bash",
        "description": "cargo test",
        "options": [
            {"id": "allow", "label": "Allow once", "canonical": "once"},
            {"id": "allow_session", "label": "Allow for this session", "canonical": "session"},
            {"id": "deny", "label": "Deny", "canonical": "deny"}
        ],
        "expires_at": expires_at,
    })
}

/// 1. hello -> hello.ack; session.start after REST create; event -> ack with seq; visible
/// through /api/ws and GET events, payload as sent.
#[tokio::test]
async fn worker_event_reaches_client() {
    let hub = TestHub::start().await;
    let (worker_id, token) = hub.enroll("home-mac").await;
    let (mut w, ack) = FakeWorker::online(&hub, &token, WS_PATH).await;
    assert_eq!(ack["payload"]["worker_id"], worker_id.as_str());
    assert_eq!(
        ack["payload"]["protocol_version"],
        hqc_proto::PROTOCOL_VERSION
    );

    let sid = hub.create_session("Run tests").await;
    let start = ack_start(&mut w, &sid).await;
    let p = &start["payload"];
    assert_eq!(p["runtime"], "claude_code");
    assert_eq!(p["workspace_path"], WS_PATH);
    assert_eq!(p["cwd"], ".");
    assert!(
        p.get("environment").is_none(),
        "the worker resolves it (T43)"
    );
    assert_eq!(p["owner_is_worker_owner"], true);
    assert_eq!(p["ask_timeout_s"], 600);
    assert!(p["session_token"].as_str().unwrap().starts_with("hqcs_"));
    Uuid::parse_str(p["runtime_session_id"].as_str().unwrap()).unwrap();

    let mut c = FakeClient::connect(&hub).await;
    let backlog = c.subscribe(&sid, 0).await;
    assert_eq!(backlog.len(), 1);
    assert_eq!(backlog[0]["type"], "system");
    assert_eq!(backlog[0]["payload"]["code"], "session_created");
    assert_eq!(backlog[0]["seq"], 1);

    // An unknown payload field must survive storage (SPEC §5.2: payload as received).
    let ev = agent_event(
        &sid,
        "agent.message",
        json!({"text": "All tests pass.", "final": true, "future_field": {"kept": [1, 2]}}),
    );
    let a = w.event(ev.clone()).await;
    assert_eq!(a, json!({"event_id": ev["event_id"], "seq": 2}));

    let live = c.until("event").await["event"].clone();
    assert_eq!(live["seq"], 2);
    for k in ["event_id", "session_id", "ts", "author", "type", "payload"] {
        assert_eq!(live[k], ev[k], "{k}");
    }

    let events = hub.events(&sid).await;
    assert_eq!(events.len(), 2);
    assert_eq!(events[1], live);

    // Unknown event type: stored and passed on as is (SPEC §4).
    let unknown = agent_event(
        &sid,
        "plan.update",
        json!({"steps": [{"text": "x", "done": false}]}),
    );
    assert_eq!(w.event(unknown.clone()).await["seq"], 3);
    let live = c.until("event").await["event"].clone();
    assert_eq!(live["type"], "plan.update");
    assert_eq!(live["payload"], unknown["payload"]);

    // Unknown session: ack without seq, dropped (SPEC §5.2).
    let stray = agent_event(
        &Uuid::now_v7().to_string(),
        "agent.message",
        json!({"text": "?", "final": true}),
    );
    assert_eq!(
        w.event(stray.clone()).await,
        json!({"event_id": stray["event_id"]})
    );

    // A known type with a bad payload is a broken frame (SPEC §4, §5.2).
    let id = w
        .send(
            "event",
            json!({"event": agent_event(&sid, "agent.message", json!({"bogus": 1}))}),
        )
        .await;
    let e = w.expect("error").await;
    assert_eq!(e["id"], id);
    assert_eq!(e["payload"]["code"], "invalid_frame");

    assert_eq!(hub.session(&sid).await["last_seq"], 3);
    hub.finish().await;
}

/// 2. The same event_id twice: one stored event, the same seq.
#[tokio::test]
async fn duplicate_event_keeps_seq() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("dup").await;
    ack_start(&mut w, &sid).await;

    let ev = agent_event(
        &sid,
        "tool.call",
        json!({"call_id": "toolu_01", "tool": "Bash", "args_preview": "ls"}),
    );
    let first = w.event(ev.clone()).await;
    let other = w
        .event(agent_event(&sid, "agent.thinking", json!({"text": "hm"})))
        .await;
    let again = w.event(ev.clone()).await;
    assert_eq!(first["seq"], 2);
    assert_eq!(other["seq"], 3);
    assert_eq!(again, first);

    let events = hub.events(&sid).await;
    assert_eq!(events.len(), 3);
    assert_eq!(
        events
            .iter()
            .filter(|e| e["event_id"] == ev["event_id"])
            .count(),
        1
    );
    assert_eq!(hub.session(&sid).await["last_seq"], 3);
    hub.finish().await;
}

/// T10. U+0000 (binary command output) is stored as U+FFFD: Postgres refuses it in jsonb
/// and text (SPEC §5.2). Same for client input and titles.
#[tokio::test]
async fn nul_is_stored_as_replacement_char() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("nul\u{0}title").await;
    ack_start(&mut w, &sid).await;
    assert_eq!(hub.session(&sid).await["title"], "nul\u{FFFD}title");
    let mut c = FakeClient::connect(&hub).await;
    c.subscribe(&sid, 0).await;

    let ev = agent_event(
        &sid,
        "tool.result",
        json!({"call_id": "toolu_01", "result_preview": "ELF\u{0}\u{0}bin", "error": false,
               "extra\u{0}": ["\u{0}"]}),
    );
    let a = w.event(ev.clone()).await;
    assert_eq!(a, json!({"event_id": ev["event_id"], "seq": 2}));
    let live = c.until("event").await["event"].clone();
    assert_eq!(live["seq"], 2);
    let want = json!({"call_id": "toolu_01", "result_preview": "ELF\u{FFFD}\u{FFFD}bin",
                      "error": false, "extra\u{FFFD}": ["\u{FFFD}"]});
    assert_eq!(live["payload"], want);
    assert_eq!(hub.events(&sid).await[1]["payload"], want);

    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "a\u{0}b"}),
        )
        .await;
    assert!(s < 300, "{s} {v}");
    let input = c.until("event").await["event"].clone();
    assert_eq!(input["payload"]["text"], "a\u{FFFD}b");
    hub.finish().await;
}

/// T10. An event the database refuses (here a `client_op_id` too long for its index) gets
/// `invalid_frame`, not retryable: the worker drops it. A database that is gone stays
/// `internal`, retryable (SPEC §5.2).
#[tokio::test]
async fn refused_event_is_not_retryable_but_db_outage_is() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("refused").await;
    ack_start(&mut w, &sid).await;

    // Random hex does not compress below the btree row limit (2704 bytes).
    let long: String = (0..100)
        .map(|_| Uuid::new_v4().simple().to_string())
        .collect();
    let mut ev = agent_event(&sid, "agent.thinking", json!({"text": "hm"}));
    ev["client_op_id"] = json!(long);
    let id = w.send("event", json!({ "event": ev })).await;
    let e = w.expect("error").await;
    assert_eq!(e["id"], id);
    assert_eq!(e["payload"]["code"], "invalid_frame", "{e}");
    assert_eq!(e["payload"]["retryable"], false);

    // The connection and the session still work.
    let ok = agent_event(&sid, "agent.thinking", json!({"text": "ok"}));
    assert_eq!(w.event(ok).await["seq"], 2);

    hub.app.db.close().await;
    let id = w
        .send(
            "event",
            json!({"event": agent_event(&sid, "agent.thinking", json!({"text": "later"}))}),
        )
        .await;
    let e = w.expect("error").await;
    assert_eq!(e["id"], id);
    assert_eq!(e["payload"]["code"], "internal", "{e}");
    assert_eq!(e["payload"]["retryable"], true);
    hub.finish().await;
}

/// 3a. Input sent while the worker is offline is delivered after its hello.
#[tokio::test]
async fn pending_input_is_delivered_after_hello() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("offline").await;
    ack_start(&mut w, &sid).await;
    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;

    let (s, r) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "Run the tests", "client_op_id": "op-1"}),
        )
        .await;
    assert_eq!(s, 201, "{r}");
    let input = r["event"].clone();
    assert_eq!(input["type"], "user.message");
    assert_eq!(input["delivery"], "pending");
    assert_eq!(input["client_op_id"], "op-1");
    assert_eq!(
        input["payload"],
        json!({"text": "Run the tests", "artifact_ids": []})
    );

    // The same client_op_id is the same input.
    let (s, again) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "Run the tests", "client_op_id": "op-1"}),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(again["event"]["event_id"], input["event_id"]);

    let v = hub.session(&sid).await;
    assert_eq!(
        (v["state"].as_str(), v["queue_len"].as_i64()),
        (Some("worker_offline"), Some(1))
    );

    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    ack_start(&mut w, &sid).await;
    let si = w.expect("session.input").await;
    assert_eq!(
        si["payload"],
        json!({"session_id": sid, "event_id": input["event_id"], "text": "Run the tests",
               "artifact_ids": [], "peer_prefix": []})
    );
    w.reply(&si["id"], "ack", json!({"event_id": input["event_id"]}))
        .await;

    let v = hub
        .wait_session(&sid, "delivered", |v| {
            v["queue_len"] == 0 && v["state"] == "running"
        })
        .await;
    assert_eq!(v["agent"]["start_failures"], 0);
    let events = hub.events(&sid).await;
    let stored = events
        .iter()
        .find(|e| e["event_id"] == input["event_id"])
        .unwrap();
    assert_eq!(stored["delivery"], "delivered");
    assert!(stored["delivered_at"].is_string());

    // The final answer ends the turn.
    w.event(agent_event(
        &sid,
        "agent.message",
        json!({"text": "done", "final": true, "in_reply_to": input["event_id"]}),
    ))
    .await;
    hub.wait_session(&sid, "idle", |v| v["state"] == "idle")
        .await;
    hub.finish().await;
}

/// 3b. Three retryable start errors make the input failed; retry brings it back.
#[tokio::test]
async fn start_failures_fail_input_and_retry_requeues() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("broken").await;
    ack_start(&mut w, &sid).await;
    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let (s, r) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": "hi"}))
        .await;
    assert_eq!(s, 201);
    let input_id = r["event"]["event_id"].clone();

    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    for _ in 0..3 {
        let f = w.expect("session.start").await;
        w.reply(
            &f["id"],
            "error",
            json!({"code": "agent_start_failed", "message": "claude: command not found", "retryable": true}),
        )
        .await;
    }
    // The row and the computed state are read by separate queries: wait for both.
    let v = hub
        .wait_session(&sid, "error", |v| {
            v["state"] == "error" && v["agent"]["start_failures"] == 3
        })
        .await;
    assert_eq!(v["queue_len"], 0);
    w.expect_silence(Duration::from_millis(300)).await;

    let events = hub.events(&sid).await;
    let input = events.iter().find(|e| e["event_id"] == input_id).unwrap();
    assert_eq!(input["delivery"], "failed");
    let sys = events.last().unwrap();
    assert_eq!(sys["type"], "system");
    assert_eq!(sys["author"], json!({"kind": "system"}));
    assert_eq!(sys["payload"]["code"], "agent_start_failed");

    let (s, r) = hub
        .post(&format!("/api/sessions/{sid}/retry"), json!({}))
        .await;
    assert_eq!((s, r["requeued"].as_i64()), (200, Some(1)));
    ack_start(&mut w, &sid).await;
    let si = w.expect("session.input").await;
    assert_eq!(si["payload"]["event_id"], input_id);
    w.reply(&si["id"], "ack", json!({"event_id": input_id}))
        .await;
    let v = hub
        .wait_session(&sid, "running", |v| v["state"] == "running")
        .await;
    assert_eq!(v["agent"]["start_failures"], 0);
    hub.finish().await;
}

/// 3b'. An input written after a failed one that reaches the agent ends the `error`: the owner
/// has gone on. The failed input stays failed, for Retry (SPEC §17.1).
#[tokio::test]
async fn a_later_delivered_input_ends_the_error_of_a_failed_one() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("full").await;
    ack_start(&mut w, &sid).await;
    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let (_, r) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "first"}),
        )
        .await;
    let failed_id = r["event"]["event_id"].clone();

    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    for _ in 0..3 {
        let f = w.expect("session.start").await;
        w.reply(
            &f["id"],
            "error",
            json!({"code": "agent_start_failed", "message": "8 agents already run on this worker (max_sessions)", "retryable": true}),
        )
        .await;
    }
    hub.wait_session(&sid, "error", |v| v["state"] == "error")
        .await;

    let (s, r) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "again"}),
        )
        .await;
    assert_eq!(s, 201);
    let input_id = r["event"]["event_id"].clone();
    ack_start(&mut w, &sid).await;
    let si = w.expect("session.input").await;
    assert_eq!(si["payload"]["event_id"], input_id);
    w.reply(&si["id"], "ack", json!({"event_id": input_id}))
        .await;
    w.event(agent_event(
        &sid,
        "agent.message",
        json!({"text": "done", "final": true, "in_reply_to": input_id}),
    ))
    .await;
    hub.wait_session(&sid, "idle", |v| v["state"] == "idle")
        .await;

    let events = hub.events(&sid).await;
    let failed = events.iter().find(|e| e["event_id"] == failed_id).unwrap();
    assert_eq!(failed["delivery"], "failed");
    hub.finish().await;
}

/// 3c. A failed start is reported like the real worker does it: `session.state` starting and
/// stopped around the `error`, in either order. The `stopped` must not restart the agent at
/// once: starts are retried after `start_retry_delay`, three in all, and the input fails with
/// a single `agent_start_failed` (SPEC §5.2).
#[tokio::test]
async fn failed_start_with_state_frames_waits_between_retries() {
    let hub = TestHub::start().await;
    let delay = test_timing().start_retry_delay;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("broken").await;
    ack_start(&mut w, &sid).await;
    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let (s, _) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": "hi"}))
        .await;
    assert_eq!(s, 201);

    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let state = |ps: &str| json!({"session_id": sid, "process_state": ps, "locked_by": "none"});
    let error = json!({"code": "agent_start_failed", "message": "claude exited right after start", "retryable": true});
    let mut last: Option<tokio::time::Instant> = None;
    let mut acks = 0;
    for i in 0..3 {
        // Acks to our `session.state` frames may come before or after the next start.
        let f = loop {
            let f = w.recv().await;
            if f["type"] == "ack" {
                acks += 1;
            } else {
                assert_eq!(f["type"], "session.start", "unexpected frame {f}");
                break f;
            }
        };
        let now = tokio::time::Instant::now();
        if let Some(prev) = last {
            assert!(
                now - prev >= delay,
                "start {i} came {:?} after the previous one",
                now - prev
            );
        }
        last = Some(now);
        w.send("session.state", state("starting")).await;
        if i % 2 == 0 {
            w.send("session.state", state("stopped")).await;
            w.reply(&f["id"], "error", error.clone()).await;
        } else {
            w.reply(&f["id"], "error", error.clone()).await;
            w.send("session.state", state("stopped")).await;
        }
    }
    hub.wait_session(&sid, "error", |v| {
        v["state"] == "error" && v["agent"]["start_failures"] == 3
    })
    .await;
    for _ in acks..6 {
        w.expect("ack").await;
    }
    w.expect_silence(delay * 10).await;
    let failed = hub
        .events(&sid)
        .await
        .into_iter()
        .filter(|e| e["payload"]["code"] == "agent_start_failed")
        .count();
    assert_eq!(failed, 1);
    hub.finish().await;
}

/// T26: "Reply with text instead" — a deny answer with `message` stores it in
/// `approval.response` and carries it in the frame to the worker; with an allow option, blank
/// or too long it is refused and the request stays open.
#[tokio::test]
async fn approval_deny_with_message() {
    let hub = TestHub::start().await;
    let (_, me) = hub.get("/api/me").await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("deny with text").await;
    ack_start(&mut w, &sid).await;
    w.event(agent_event(
        &sid,
        "approval.request",
        approval_payload("req-m", "2099-01-01T00:00:00Z"),
    ))
    .await;
    hub.wait_session(&sid, "waiting", |v| v["state"] == "waiting")
        .await;

    let path = format!("/api/sessions/{sid}/requests/req-m");
    let long = "x".repeat(hqc_hub::requests::MAX_ANSWER_MESSAGE + 1);
    for bad in [
        json!({"option_id": "allow", "message": "do it"}),
        json!({"option_id": "deny", "message": "  "}),
        json!({"option_id": "deny", "message": long}),
    ] {
        let (s, r) = hub.post(&path, bad).await;
        assert_eq!(s, 400, "{r}");
    }
    let text = "Leave the root alone, look only at ~/work/hqc";
    let (s, r) = hub
        .post(&path, json!({"option_id": "deny", "message": text}))
        .await;
    assert_eq!(s, 200, "{r}");
    let f = w.expect("approval.response").await;
    assert_eq!(
        f["payload"],
        json!({"session_id": sid, "request_id": "req-m", "option_id": "deny", "message": text})
    );
    w.reply(&f["id"], "ack", json!({})).await;
    let events = hub.events(&sid).await;
    let resp = events.last().unwrap();
    assert_eq!(resp["type"], "approval.response");
    assert_eq!(
        resp["payload"],
        json!({"request_id": "req-m", "option_id": "deny", "by": me["id"], "message": text})
    );
    hub.finish().await;
}

/// 4. approval.request -> POST requests -> approval.response to the worker; expiry gives
/// approval.cancel and the deny frame.
#[tokio::test]
async fn approval_answer_and_expiry() {
    let hub = TestHub::start().await;
    let (_, me) = hub.get("/api/me").await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("approve").await;
    ack_start(&mut w, &sid).await;

    w.event(agent_event(
        &sid,
        "approval.request",
        approval_payload("req-1", "2099-01-01T00:00:00Z"),
    ))
    .await;
    hub.wait_session(&sid, "waiting", |v| v["state"] == "waiting")
        .await;

    let path = format!("/api/sessions/{sid}/requests/req-1");
    let (s, _) = hub.post(&path, json!({"option_id": "nope"})).await;
    assert_eq!(s, 400);
    let (s, r) = hub.post(&path, json!({"option_id": "allow_session"})).await;
    assert_eq!(s, 200, "{r}");
    let f = w.expect("approval.response").await;
    assert_eq!(
        f["payload"],
        json!({"session_id": sid, "request_id": "req-1", "option_id": "allow_session"})
    );
    w.reply(&f["id"], "ack", json!({})).await;
    // First answer wins.
    let (s, r) = hub.post(&path, json!({"option_id": "deny"})).await;
    assert_eq!((s, r["error"].as_str()), (409, Some("request_closed")));
    let (s, _) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/missing"),
            json!({"option_id": "deny"}),
        )
        .await;
    assert_eq!(s, 404);

    let events = hub.events(&sid).await;
    let resp = events.last().unwrap();
    assert_eq!(resp["type"], "approval.response");
    assert_eq!(resp["author"], json!({"kind": "user", "id": me["id"]}));
    assert_eq!(
        resp["payload"],
        json!({"request_id": "req-1", "option_id": "allow_session", "by": me["id"]})
    );
    hub.wait_session(&sid, "not waiting", |v| v["state"] == "idle")
        .await;

    // Expiry (SPEC §10.1): approval.cancel {timeout} and approval.response with the deny option.
    let soon = (Utc::now() + chrono::Duration::milliseconds(300)).to_rfc3339();
    w.event(agent_event(
        &sid,
        "approval.request",
        approval_payload("req-2", &soon),
    ))
    .await;
    let f = w.expect("approval.response").await;
    assert_eq!(
        f["payload"],
        json!({"session_id": sid, "request_id": "req-2", "option_id": "deny"})
    );
    w.reply(&f["id"], "ack", json!({})).await;
    let events = hub.events(&sid).await;
    let cancel = events.last().unwrap();
    assert_eq!(cancel["type"], "approval.cancel");
    assert_eq!(cancel["author"], json!({"kind": "system"}));
    assert_eq!(
        cancel["payload"],
        json!({"request_id": "req-2", "reason": "timeout"})
    );
    let (s, _) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/req-2"),
            json!({"option_id": "allow"}),
        )
        .await;
    assert_eq!(s, 409);
    hub.wait_session(&sid, "idle", |v| v["state"] == "idle")
        .await;
    hub.finish().await;
}

/// Clarify: answer by id; stop cancels an open one with `clarify.response {answers: {}}`.
#[tokio::test]
async fn clarify_answer_and_cancel_on_stop() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("clarify").await;
    ack_start(&mut w, &sid).await;

    let q = json!({"request_id": "c-1", "questions": [{"id": "q1", "text": "Which?", "options": ["Hub", "Worker"]}]});
    w.event(agent_event(&sid, "clarify.request", q)).await;
    let (s, _) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/c-1"),
            json!({"answers": {}}),
        )
        .await;
    assert_eq!(s, 400, "empty answers are reserved for cancellation");
    let (s, _) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/c-1"),
            json!({"answers": {"q1": "Hub"}}),
        )
        .await;
    assert_eq!(s, 200);
    let f = w.expect("clarify.response").await;
    assert_eq!(
        f["payload"],
        json!({"session_id": sid, "request_id": "c-1", "answers": {"q1": "Hub"}})
    );
    w.reply(&f["id"], "ack", json!({})).await;

    let q = json!({"request_id": "c-2", "questions": [{"id": "q1", "text": "Sure?"}]});
    w.event(agent_event(&sid, "clarify.request", q)).await;
    let (s, r) = hub
        .post(&format!("/api/sessions/{sid}/stop"), json!({}))
        .await;
    assert_eq!((s, r["sent"].as_bool()), (200, Some(true)));
    let f = w.expect("clarify.response").await;
    assert_eq!(
        f["payload"],
        json!({"session_id": sid, "request_id": "c-2", "answers": {}})
    );
    w.expect("session.stop").await;
    let events = hub.events(&sid).await;
    let cancel = events.last().unwrap();
    assert_eq!(cancel["type"], "clarify.cancel");
    assert_eq!(
        cancel["payload"],
        json!({"request_id": "c-2", "reason": "stopped"})
    );
    hub.finish().await;
}

/// A worker's cancel (the Claude Code adapter's for a request still open when the turn ends)
/// closes its request; sent again, it gets its `seq`. One for a request the hub closed already
/// (stop cancels first) is not stored a second time.
#[tokio::test]
async fn a_workers_cancel_closes_its_request_once() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("cancel").await;
    ack_start(&mut w, &sid).await;
    let far = "2099-01-01T00:00:00Z";

    w.event(agent_event(
        &sid,
        "approval.request",
        approval_payload("req-1", far),
    ))
    .await;
    hub.wait_session(&sid, "waiting", |v| v["state"] == "waiting")
        .await;
    let cancel = agent_event(
        &sid,
        "approval.cancel",
        json!({"request_id": "req-1", "reason": "stopped"}),
    );
    let seq = w.event(cancel.clone()).await["seq"].clone();
    assert!(seq.is_u64(), "{seq}");
    hub.wait_session(&sid, "idle", |v| v["state"] == "idle")
        .await;
    assert_eq!(w.event(cancel).await["seq"], seq);

    w.event(agent_event(
        &sid,
        "approval.request",
        approval_payload("req-2", far),
    ))
    .await;
    let (s, _) = hub
        .post(&format!("/api/sessions/{sid}/stop"), json!({}))
        .await;
    assert_eq!(s, 200);
    w.expect("approval.response").await;
    w.expect("session.stop").await;
    let ack = w
        .event(agent_event(
            &sid,
            "approval.cancel",
            json!({"request_id": "req-2", "reason": "stopped"}),
        ))
        .await;
    assert!(ack["seq"].is_null(), "{ack}");
    let cancels: Vec<(Value, Value)> = hub
        .events(&sid)
        .await
        .iter()
        .filter(|e| e["type"] == "approval.cancel")
        .map(|e| {
            (
                e["payload"]["request_id"].clone(),
                e["author"]["kind"].clone(),
            )
        })
        .collect();
    assert_eq!(
        cancels,
        [
            (json!("req-1"), json!("agent")),
            (json!("req-2"), json!("system"))
        ]
    );
    hub.finish().await;
}

/// A worker's cancel closes its request in the transaction that stores it: when storing it
/// fails (here a trigger refuses it, as a database lost between the two would), the request
/// stays open and the worker's resend of the same event stores it and closes the request.
#[tokio::test]
async fn a_workers_cancel_not_stored_leaves_its_request_open() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("cancel").await;
    ack_start(&mut w, &sid).await;
    w.event(agent_event(
        &sid,
        "approval.request",
        approval_payload("req-1", "2099-01-01T00:00:00Z"),
    ))
    .await;
    hub.wait_session(&sid, "waiting", |v| v["state"] == "waiting")
        .await;
    sqlx::raw_sql(
        "CREATE FUNCTION refuse_cancel() RETURNS trigger LANGUAGE plpgsql AS
           $$ BEGIN RAISE EXCEPTION 'refused by the test'; END $$;
         CREATE TRIGGER refuse_cancel BEFORE INSERT ON events FOR EACH ROW
           WHEN (NEW.type = 'approval.cancel') EXECUTE FUNCTION refuse_cancel();",
    )
    .execute(&hub.app.db)
    .await
    .unwrap();
    let open = || async {
        sqlx::query_scalar::<_, bool>(
            "SELECT closed_at IS NULL FROM pending_requests WHERE request_id = 'req-1'",
        )
        .fetch_one(&hub.app.db)
        .await
        .unwrap()
    };
    let cancel = agent_event(
        &sid,
        "approval.cancel",
        json!({"request_id": "req-1", "reason": "stopped"}),
    );
    w.send("event", json!({ "event": cancel.clone() })).await;
    let e = w.expect("error").await;
    assert_eq!(e["payload"]["retryable"], true, "{e}");
    assert!(open().await);

    sqlx::raw_sql("DROP TRIGGER refuse_cancel ON events")
        .execute(&hub.app.db)
        .await
        .unwrap();
    let ack = w.event(cancel).await;
    assert!(ack["seq"].is_u64(), "{ack}");
    assert!(!open().await);
    hub.wait_session(&sid, "idle", |v| v["state"] == "idle")
        .await;
    hub.finish().await;
}

/// An answer given while the worker is offline reaches it after hello.
#[tokio::test]
async fn answer_while_offline_is_delivered_after_hello() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("offline answer").await;
    ack_start(&mut w, &sid).await;
    w.event(agent_event(
        &sid,
        "approval.request",
        approval_payload("req-9", "2099-01-01T00:00:00Z"),
    ))
    .await;
    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let (s, _) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/req-9"),
            json!({"option_id": "allow"}),
        )
        .await;
    assert_eq!(s, 200);

    // Same boot_id: the worker still holds the request; session.start comes first.
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let st = ack_start(&mut w, &sid).await;
    assert_eq!(st["payload"]["mode"], "resume");
    let f = w.expect("approval.response").await;
    assert_eq!(f["payload"]["option_id"], "allow");
    w.reply(&f["id"], "ack", json!({})).await;
    w.expect_silence(Duration::from_millis(200)).await;
    hub.finish().await;
}

/// SPEC §5.3: `session.start.has_history` once the session's feed has an `agent.message`, so
/// the worker can tell a lost transcript from a session that never talked.
#[tokio::test]
async fn session_start_says_when_the_session_has_talked() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("history").await;
    let st = ack_start(&mut w, &sid).await;
    assert!(st["payload"].get("has_history").is_none(), "{st}");
    w.event(agent_event(
        &sid,
        "agent.message",
        json!({"text": "Hi.", "final": true}),
    ))
    .await;

    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let (s, _) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "go on"}),
        )
        .await;
    assert_eq!(s, 201);
    let st = ack_start(&mut w, &sid).await;
    assert_eq!(st["payload"]["has_history"], true, "{st}");
    hub.finish().await;
}

/// SPEC §5.2, §17.3: a reconnect with the same boot_id keeps an open approval; a new
/// boot_id (worker restart) cancels it with `worker_restart`, without a frame, and loses the
/// turn.
#[tokio::test]
async fn boot_id_tells_restart_from_reconnect() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("boot").await;
    let st = ack_start(&mut w, &sid).await;
    assert_eq!(st["payload"]["mode"], "new");
    let (s, r) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": "go"}))
        .await;
    assert_eq!(s, 201);
    let eid = r["event"]["event_id"].clone();
    let si = w.expect("session.input").await;
    w.reply(&si["id"], "ack", json!({"event_id": eid})).await;
    w.event(agent_event(
        &sid,
        "approval.request",
        approval_payload("req-b", "2099-01-01T00:00:00Z"),
    ))
    .await;
    hub.wait_session(&sid, "waiting", |v| v["state"] == "waiting")
        .await;

    // Connection drop, same process: nothing is cancelled, the session is restarted.
    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let st = ack_start(&mut w, &sid).await;
    assert_eq!(st["payload"]["mode"], "resume");
    w.expect_silence(Duration::from_millis(300)).await;
    hub.wait_session(&sid, "still waiting", |v| v["state"] == "waiting")
        .await;
    let events = hub.events(&sid).await;
    assert!(
        !events.iter().any(|e| e["type"] == "approval.cancel"
            || e["type"] == "system" && e["payload"]["code"] == "turn_lost"),
        "reconnect must not cancel: {events:?}"
    );

    // A new boot_id: the worker restarted.
    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let (mut w, _) = FakeWorker::online_boot(&hub, &token, WS_PATH, Uuid::new_v4()).await;
    // No pending input and no open request left: no session.start, no deny frame.
    w.expect_silence(Duration::from_millis(300)).await;
    let v = hub
        .wait_session(&sid, "idle after restart", |v| v["state"] == "idle")
        .await;
    assert_eq!(v["agent"]["process_state"], "stopped");
    let events = hub
        .wait_events(&sid, "turn_lost", |es| {
            es.iter()
                .any(|e| e["type"] == "system" && e["payload"]["code"] == "turn_lost")
        })
        .await;
    let cancel = events
        .iter()
        .find(|e| e["type"] == "approval.cancel")
        .expect("approval.cancel");
    assert_eq!(
        cancel["payload"],
        json!({"request_id": "req-b", "reason": "worker_restart"})
    );
    let (s, _) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/req-b"),
            json!({"option_id": "allow"}),
        )
        .await;
    assert_eq!(s, 409, "the cancelled request is closed");
    hub.finish().await;
}

/// SPEC §5.2: the input ack bypasses the worker's outbox and can arrive after the final
/// answer of its turn; the turn must not be reopened.
#[tokio::test]
async fn late_input_ack_does_not_reopen_the_turn() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("late ack").await;
    ack_start(&mut w, &sid).await;
    let (_, r) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": "hi"}))
        .await;
    let eid = r["event"]["event_id"].clone();
    let si = w.expect("session.input").await;
    w.event(agent_event(
        &sid,
        "agent.message",
        json!({"text": "done", "final": true, "in_reply_to": eid}),
    ))
    .await;
    w.reply(&si["id"], "ack", json!({"event_id": eid})).await;
    let v = hub
        .wait_session(&sid, "delivered", |v| v["queue_len"] == 0)
        .await;
    assert_eq!(v["state"], "idle", "{v}");
    hub.finish().await;
}

/// SPEC §17.1: a turn the runtime starts by itself (Claude Code after a background task's end)
/// has no input whose `ack` opens it; the worker's `status running` does while the agent's
/// process runs, and `status idle` closes it. SPEC §17.3: the same event sent again from the
/// outbox of a restarted worker, after its `hello` stopped the session, opens none.
#[tokio::test]
async fn a_worker_status_running_opens_a_turn() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("own turn").await;
    ack_start(&mut w, &sid).await;
    w.send(
        "session.state",
        json!({"session_id": sid, "process_state": "running", "locked_by": "hqc"}),
    )
    .await;
    w.expect("ack").await;
    assert_eq!(hub.session(&sid).await["state"], "idle");
    w.event(agent_event(&sid, "status", json!({"state": "running"})))
        .await;
    assert_eq!(hub.session(&sid).await["state"], "running");
    w.event(agent_event(&sid, "status", json!({"state": "idle"})))
        .await;
    assert_eq!(hub.session(&sid).await["state"], "idle");

    // A new boot_id: the worker restarted; its outbox sends a `status running` again.
    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let (mut w, _) = FakeWorker::online_boot(&hub, &token, WS_PATH, Uuid::new_v4()).await;
    hub.wait_session(&sid, "stopped", |v| {
        v["agent"]["process_state"] == "stopped"
    })
    .await;
    w.event(agent_event(&sid, "status", json!({"state": "running"})))
        .await;
    assert_eq!(hub.session(&sid).await["state"], "idle");
    hub.finish().await;
}

/// SPEC §5.2: after hello the hub sends session.start (mode resume) and only after its ack
/// the pending input, in seq order.
#[tokio::test]
async fn session_start_precedes_input_after_hello() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("order").await;
    ack_start(&mut w, &sid).await;
    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let mut ids = vec![];
    for text in ["one", "two"] {
        let (s, r) = hub
            .post(&format!("/api/sessions/{sid}/input"), json!({"text": text}))
            .await;
        assert_eq!(s, 201);
        ids.push(r["event"]["event_id"].clone());
    }

    // A restarted worker (new boot_id) still gets its pending input, after session.start.
    let (mut w, _) = FakeWorker::online_boot(&hub, &token, WS_PATH, Uuid::new_v4()).await;
    let st = w.expect("session.start").await;
    assert_eq!(st["payload"]["mode"], "resume");
    w.expect_silence(Duration::from_millis(200)).await;
    w.reply(&st["id"], "ack", json!({})).await;
    for id in &ids {
        let si = w.expect("session.input").await;
        assert_eq!(&si["payload"]["event_id"], id);
    }
    hub.finish().await;
}

/// 5. after_seq gives only newer events; agent.delta reaches the client without seq and is
/// not stored.
#[tokio::test]
async fn after_seq_and_delta() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("stream").await;
    ack_start(&mut w, &sid).await;
    w.event(agent_event(&sid, "agent.thinking", json!({"text": "a"})))
        .await;
    w.event(agent_event(&sid, "agent.thinking", json!({"text": "b"})))
        .await;

    let mut c = FakeClient::connect(&hub).await;
    let backlog = c.subscribe(&sid, 2).await;
    assert_eq!(backlog.len(), 1);
    assert_eq!(backlog[0]["seq"], 3);
    assert_eq!(backlog[0]["payload"]["text"], "b");

    let delta = agent_event(&sid, "agent.delta", json!({"text": "Runn"}));
    let a = w.event(delta.clone()).await;
    assert_eq!(
        a,
        json!({"event_id": delta["event_id"]}),
        "no seq for agent.delta"
    );
    let live = c.until("event").await["event"].clone();
    assert_eq!(live, delta);
    assert!(live.get("seq").is_none());

    w.event(agent_event(
        &sid,
        "agent.message",
        json!({"text": "Running", "final": false}),
    ))
    .await;
    let live = c.until("event").await["event"].clone();
    assert_eq!(live["seq"], 4);

    let events = hub.events(&sid).await;
    assert_eq!(events.len(), 4);
    assert!(events.iter().all(|e| e["type"] != "agent.delta"));
    let (_, page) = hub
        .get(&format!("/api/sessions/{sid}/events?after_seq=2&limit=1"))
        .await;
    let page = page["events"].as_array().unwrap();
    assert_eq!((page.len(), page[0]["seq"].as_i64()), (1, Some(3)));
    hub.finish().await;
}

/// A client that subscribes mid-answer gets the text written so far as one `agent.delta`,
/// then only the deltas after it; the answer stored ends the text (SPEC §6).
#[tokio::test]
async fn subscribe_mid_answer_gets_the_text_so_far() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("stream").await;
    ack_start(&mut w, &sid).await;
    for t in ["Hello ", "world"] {
        w.event(agent_event(&sid, "agent.delta", json!({"text": t})))
            .await;
    }

    let mut c = FakeClient::connect(&hub).await;
    let backlog = c.subscribe(&sid, 0).await;
    let copy = backlog.last().unwrap();
    assert_eq!(copy["type"], "agent.delta");
    assert!(copy.get("seq").is_none());
    assert_eq!(copy["payload"]["text"], "Hello world");

    w.event(agent_event(&sid, "agent.delta", json!({"text": ", how"})))
        .await;
    let live = c.until("event").await["event"].clone();
    assert_eq!(live["payload"]["text"], ", how");

    w.event(agent_event(
        &sid,
        "agent.message",
        json!({"text": "Hello world, how", "final": false}),
    ))
    .await;
    assert_eq!(c.until("event").await["event"]["type"], "agent.message");
    let mut late = FakeClient::connect(&hub).await;
    let backlog = late.subscribe(&sid, 0).await;
    assert!(backlog.iter().all(|e| e["type"] != "agent.delta"));
    hub.finish().await;
}

/// SPEC §6: an input stored while the agent's text is being written has
/// `payload.during_text`, so clients show it under that text; once the text is stored, the next
/// input has none.
#[tokio::test]
async fn an_input_during_the_answer_text_is_marked() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("during").await;
    ack_start(&mut w, &sid).await;
    w.event(agent_event(&sid, "agent.delta", json!({"text": "Writ"})))
        .await;
    let (s, r) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "meanwhile"}),
        )
        .await;
    assert_eq!(s, 201);
    let during = r["event"]["event_id"].clone();
    let si = w.expect("session.input").await;
    w.reply(&si["id"], "ack", json!({"event_id": during})).await;
    w.event(agent_event(
        &sid,
        "agent.message",
        json!({"text": "Writing", "final": false}),
    ))
    .await;
    let (s, r) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "after"}),
        )
        .await;
    assert_eq!(s, 201);
    let after = r["event"]["event_id"].clone();
    let events = hub.events(&sid).await;
    let stored = |id: &Value| events.iter().find(|e| &e["event_id"] == id).unwrap();
    assert_eq!(stored(&during)["payload"]["during_text"], true);
    assert!(
        stored(&after)["payload"].get("during_text").is_none(),
        "{}",
        stored(&after)
    );
    hub.finish().await;
}

/// Web Push on approval.request when nobody watches; no push while a client is subscribed.
#[tokio::test]
async fn push_only_without_live_subscription() {
    let hub = TestHub::start().await;
    let (s, v) = hub.get("/api/push/vapid").await;
    assert_eq!(
        (s, v["public_key"].as_str()),
        (200, Some("test-vapid-public-key"))
    );
    let (s, _) = hub
        .post(
            "/api/push/subscribe",
            json!({"endpoint": "https://fcm.googleapis.com/fcm/send/sub-1", "expirationTime": null,
                   "keys": {"p256dh": "k", "auth": "a"}}),
        )
        .await;
    assert_eq!(s, 201);
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("push").await;
    ack_start(&mut w, &sid).await;

    w.event(agent_event(
        &sid,
        "approval.request",
        approval_payload("req-1", "2099-01-01T00:00:00Z"),
    ))
    .await;
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if !hub.push.sent.lock().unwrap().is_empty() {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "no push");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    {
        let sent = hub.push.sent.lock().unwrap();
        assert_eq!(sent[0].0, "https://fcm.googleapis.com/fcm/send/sub-1");
        // SPEC §16: {session_id, type, title?, body?, tag?, payload?}, payload = the event.
        let m: hqc_proto::api::PushMessage = serde_json::from_value(sent[0].1.clone()).unwrap();
        assert_eq!(m.kind, "approval.request");
        assert_eq!(m.session_id.to_string(), sid);
        assert_eq!(m.tag.as_deref(), Some(format!("{sid}:req-1").as_str()));
        let ev = m.payload.expect("the event fits");
        assert_eq!(ev["type"], "approval.request");
        assert_eq!(ev["payload"]["request_id"], "req-1");
        assert!(ev["seq"].is_u64());
    }

    let mut c = FakeClient::connect(&hub).await;
    c.subscribe(&sid, 0).await;
    w.event(agent_event(
        &sid,
        "agent.message",
        json!({"text": "done", "final": true}),
    ))
    .await;
    c.until("event").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(hub.push.sent.lock().unwrap().len(), 1);
    hub.finish().await;
}

/// SPEC §6: a delivery change reaches the subscriber as a `delivery` frame (the event is not
/// re-sent); unread counts the final agent.message but not the intermediate one; REST and
/// summary shapes are the hqc-proto types.
#[tokio::test]
async fn delivery_frame_unread_and_shapes() {
    use hqc_proto::{
        api::{EventPage, InputResponse, Session, SessionList},
        client_frame::ServerFrame,
    };
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("shapes").await;
    ack_start(&mut w, &sid).await;
    let mut c = FakeClient::connect(&hub).await;
    c.subscribe(&sid, 0).await;

    let (s, r) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "hi", "client_op_id": "op-shapes"}),
        )
        .await;
    assert_eq!(s, 201);
    let r: InputResponse = serde_json::from_value(r).unwrap();
    let input_id = r.event["event_id"].clone();
    let echo = c.until("event").await;
    assert_eq!(echo["event"], r.event, "the echo is the stored event");
    let si = w.expect("session.input").await;
    w.reply(&si["id"], "ack", json!({"event_id": input_id}))
        .await;
    let d = c.until("delivery").await;
    match serde_json::from_value::<ServerFrame>(d.clone()).unwrap() {
        ServerFrame::Delivery {
            event_id,
            seq,
            delivery,
            delivered_at,
            ..
        } => {
            assert_eq!(event_id.to_string(), input_id.as_str().unwrap());
            assert_eq!(Some(seq), r.event["seq"].as_u64());
            assert_eq!(delivery, hqc_proto::event::Delivery::Delivered);
            assert!(delivered_at.is_some());
        }
        other => panic!("not a delivery frame: {other:?}"),
    }

    let read = hub.session(&sid).await["last_seq"].as_i64().unwrap();
    hub.put(
        &format!("/api/sessions/{sid}/read_cursor"),
        json!({"last_read_seq": read}),
    )
    .await;
    for fin in [false, true] {
        w.event(agent_event(
            &sid,
            "agent.message",
            json!({"text": "x", "final": fin, "in_reply_to": input_id}),
        ))
        .await;
    }
    let v: Session = serde_json::from_value(
        hub.wait_session(&sid, "idle", |v| v["state"] == "idle")
            .await,
    )
    .unwrap();
    assert_eq!(v.unread, 1, "only the final agent.message is unread");

    let pid = hub.personal_project().await;
    let (_, list) = hub.get(&format!("/api/projects/{pid}/sessions")).await;
    let list: SessionList = serde_json::from_value(list).unwrap();
    assert_eq!(list.sessions[0].summary(), v.summary());
    let (_, page) = hub.get(&format!("/api/sessions/{sid}/events")).await;
    let page: EventPage = serde_json::from_value(page).unwrap();
    assert_eq!(page.last_seq, v.last_seq);
    let (s, err) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/nope"),
            json!({"option_id": "x"}),
        )
        .await;
    assert_eq!(s, 404);
    let err: hqc_proto::api::ApiError = serde_json::from_value(err).unwrap();
    assert_eq!(err.error, "not_found");
    hub.finish().await;
}

/// subscribe_summary gives state and queue_len and follows changes.
#[tokio::test]
async fn summary_follows_state() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("summary").await;
    ack_start(&mut w, &sid).await;

    let mut c = FakeClient::connect(&hub).await;
    c.send(json!({"type": "subscribe_summary"})).await;
    let s = c.until("summary").await;
    let me = s["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["session_id"] == sid.as_str())
        .unwrap()
        .clone();
    assert_eq!(me["state"], "idle");
    assert_eq!(me["unread"], 1, "session_created counts as unread");

    w.event(agent_event(
        &sid,
        "approval.request",
        approval_payload("r", "2099-01-01T00:00:00Z"),
    ))
    .await;
    loop {
        let u = c.until("session.summary").await;
        if u["session"]["state"] == "waiting" {
            break;
        }
    }
    let (s, r) = hub
        .put(
            &format!("/api/sessions/{sid}/read_cursor"),
            json!({"last_read_seq": 2}),
        )
        .await;
    assert_eq!((s, r["last_read_seq"].as_i64()), (200, Some(2)));
    assert_eq!(hub.session(&sid).await["unread"], 0);
    hub.finish().await;
}

/// Archive cancels open requests, fails pending input and kills the process (SPEC §17.4).
#[tokio::test]
async fn archive_cancels_and_fails() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("archive").await;
    ack_start(&mut w, &sid).await;
    w.event(agent_event(
        &sid,
        "approval.request",
        approval_payload("req-1", "2099-01-01T00:00:00Z"),
    ))
    .await;

    let (s, v) = hub
        .post(&format!("/api/sessions/{sid}/archive"), json!({}))
        .await;
    assert_eq!((s, v["state"].as_str()), (200, Some("archived")));
    w.expect("session.kill").await;
    let f = w.expect("approval.response").await;
    assert_eq!(f["payload"]["option_id"], "deny");
    let events = hub.events(&sid).await;
    assert_eq!(events.last().unwrap()["payload"]["reason"], "archived");
    let (s, _) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": "x"}))
        .await;
    assert_eq!(s, 409);
    let (s, v) = hub
        .post(&format!("/api/sessions/{sid}/unarchive"), json!({}))
        .await;
    assert_eq!((s, v["state"].as_str()), (200, Some("idle")));
    hub.finish().await;
}

/// T13 (SPEC §17.3): a `running` session whose worker sends nothing (not even
/// `agent.delta`) for the watchdog period loses its turn: `system turn_lost` "no events for
/// N s", state `error` until the next input. Waiting for an answer does not count.
#[tokio::test]
async fn turn_watchdog_closes_a_silent_turn() {
    // Generous enough for a slow CI runner: REST and WebSocket round trips between the
    // steps below must stay well under it (it flaked at 600 ms on GitHub Actions).
    let limit = Duration::from_millis(2500);
    let hub = TestHub::start_timing(hqc_hub::config::Timing {
        expiry_interval: Duration::from_millis(50),
        turn_watchdog: Some(limit),
        ..test_timing()
    })
    .await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("silent").await;
    ack_start(&mut w, &sid).await;
    async fn input(hub: &TestHub, w: &mut FakeWorker, sid: &str) {
        let (s, r) = hub
            .post(&format!("/api/sessions/{sid}/input"), json!({"text": "go"}))
            .await;
        assert_eq!(s, 201, "{r}");
        assert_ne!(hub.session(sid).await["state"], "error");
        let si = w.expect("session.input").await;
        w.reply(
            &si["id"],
            "ack",
            json!({"event_id": r["event"]["event_id"]}),
        )
        .await;
        hub.wait_session(sid, "running", |v| v["state"] == "running")
            .await;
    }
    fn lost(events: &[Value]) -> Vec<&Value> {
        events
            .iter()
            .filter(|e| e["payload"]["code"] == "turn_lost")
            .collect()
    }

    input(&hub, &mut w, &sid).await;
    // A delta keeps the turn alive.
    tokio::time::sleep(limit * 2 / 3).await;
    w.event(agent_event(&sid, "agent.delta", json!({"text": "…"})))
        .await;
    tokio::time::sleep(limit * 2 / 3).await;
    assert_eq!(hub.session(&sid).await["state"], "running");
    assert!(lost(&hub.events(&sid).await).is_empty());

    let v = hub
        .wait_session(&sid, "error", |v| v["state"] == "error")
        .await;
    assert_eq!(v["queue_len"], 0);
    let events = hub
        .wait_events(&sid, "turn_lost", |es| !lost(es).is_empty())
        .await;
    let l = lost(&events);
    assert_eq!(l.len(), 1, "{l:?}");
    assert!(
        l[0]["payload"]["text"]
            .as_str()
            .unwrap()
            .starts_with("no events for "),
        "{}",
        l[0]
    );
    assert_eq!(l[0]["author"]["kind"], "system");

    // The next input clears the error; an open approval stops the clock.
    input(&hub, &mut w, &sid).await;
    w.event(agent_event(
        &sid,
        "approval.request",
        approval_payload("req-w", "2099-01-01T00:00:00Z"),
    ))
    .await;
    tokio::time::sleep(limit * 2).await;
    assert_eq!(hub.session(&sid).await["state"], "waiting");
    let (s, _) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/req-w"),
            json!({"option_id": "allow"}),
        )
        .await;
    assert_eq!(s, 200);
    w.expect("approval.response").await;
    hub.wait_session(&sid, "running", |v| v["state"] == "running")
        .await;
    hub.wait_session(&sid, "error", |v| v["state"] == "error")
        .await;
    hub.wait_events(&sid, "the second turn_lost", |es| lost(es).len() == 2)
        .await;
    hub.finish().await;
}

/// T13: a session without a title is "Session N"; the runtime's title (`session.state.title`)
/// replaces a default or runtime title, never one the user set (SPEC §3 Session).
#[tokio::test]
async fn runtime_title_does_not_override_the_user() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let pid = hub.personal_project().await;
    let mut ids = vec![];
    for n in 1..=2 {
        let (s, v) = hub
            .post(&format!("/api/projects/{pid}/sessions"), json!({}))
            .await;
        assert_eq!(s, 201, "{v}");
        assert_eq!(v["title"], format!("Session {n}"));
        assert_eq!(v["title_source"], "default");
        assert_eq!(v["slug"], format!("session-{n}"));
        ack_start(&mut w, v["id"].as_str().unwrap()).await;
        ids.push(v["id"].as_str().unwrap().to_string());
    }
    let (a, b) = (&ids[0], &ids[1]);
    let state = |sid: &str, title: &str| json!({"session_id": sid, "process_state": "running", "locked_by": "hqc", "title": title});

    w.send("session.state", state(a, "  Fix the\nflaky test "))
        .await;
    w.expect("ack").await;
    let v = hub.session(a).await;
    assert_eq!(v["title"], "Fix the flaky test");
    assert_eq!(v["title_source"], "runtime");
    assert_eq!(v["slug"], "session-1", "the slug stays");
    w.send("session.state", state(a, "Second title")).await;
    w.expect("ack").await;
    assert_eq!(hub.session(a).await["title"], "Second title");

    let (s, v) = hub
        .patch(&format!("/api/sessions/{b}"), json!({"title": "Mine"}))
        .await;
    assert_eq!((s, v["title_source"].as_str()), (200, Some("user")), "{v}");
    w.send("session.state", state(b, "Runtime title")).await;
    w.expect("ack").await;
    let v = hub.session(b).await;
    assert_eq!(v["title"], "Mine");
    assert_eq!(v["title_source"], "user");

    // A title given at creation is the user's too.
    let sid = hub.create_session("Named").await;
    ack_start(&mut w, &sid).await;
    w.send("session.state", state(&sid, "Runtime title")).await;
    w.expect("ack").await;
    assert_eq!(hub.session(&sid).await["title"], "Named");
    hub.finish().await;
}

/// T27 (SPEC §5.4, §6): `session.state.commands` is stored on the session and shown in the
/// session object; a frame without it keeps the list, names are cleaned and deduplicated.
#[tokio::test]
async fn runtime_commands_are_kept_on_the_session() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Commands").await;
    ack_start(&mut w, &sid).await;
    assert!(
        hub.session(&sid).await.get("commands").is_none(),
        "no list yet"
    );
    let state = |commands: Value| {
        let mut v = json!({"session_id": sid, "process_state": "running", "locked_by": "hqc"});
        if !commands.is_null() {
            v["commands"] = commands;
        }
        v
    };

    let long = "x".repeat(101);
    let sent = json!([
        "compact",
        "/review",
        "compact",
        "",
        "two words",
        long,
        "a:b"
    ]);
    w.send("session.state", state(sent)).await;
    w.expect("ack").await;
    let v = hub.session(&sid).await;
    assert_eq!(v["commands"], json!(["compact", "review", "a:b"]), "{v}");

    w.send("session.state", state(Value::Null)).await;
    w.expect("ack").await;
    assert_eq!(hub.session(&sid).await["commands"][1], "review", "kept");
    let pid = hub.personal_project().await;
    let (_, list) = hub.get(&format!("/api/projects/{pid}/sessions")).await;
    let listed = list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == sid.as_str())
        .unwrap()
        .clone();
    assert_eq!(listed["commands"][0], "compact", "in the list too");

    let many: Vec<String> = (0..600).map(|i| format!("c{i}")).collect();
    w.send("session.state", state(json!(many))).await;
    w.expect("ack").await;
    let v = hub.session(&sid).await;
    assert_eq!(v["commands"].as_array().unwrap().len(), 500);
    hub.finish().await;
}

/// SPEC §17.2: an input the worker reports `joined` went into the turn already running. It is
/// delivered but opens no turn: its ack, which can come after that turn's final answer (it
/// bypasses the worker's outbox), must not open the turn again.
#[tokio::test]
async fn joined_input_ack_opens_no_turn() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("joined").await;
    ack_start(&mut w, &sid).await;
    let (_, r) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": "A"}))
        .await;
    let a = r["event"]["event_id"].clone();
    let si = w.expect("session.input").await;
    w.reply(&si["id"], "ack", json!({"event_id": a})).await;
    hub.wait_session(&sid, "running", |v| v["state"] == "running")
        .await;
    let (_, r) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": "B"}))
        .await;
    let b = r["event"]["event_id"].clone();
    let si = w.expect("session.input").await;
    assert_eq!(si["payload"]["event_id"], b);
    w.event(agent_event(
        &sid,
        "agent.message",
        json!({"text": "done", "final": true, "in_reply_to": a}),
    ))
    .await;
    // The turn is over; B is not delivered yet.
    hub.wait_session(&sid, "turn closed", |v| v["state"] == "queued")
        .await;
    w.reply(&si["id"], "ack", json!({"event_id": b, "joined": true}))
        .await;
    // The hub reads a worker's frames in order: once this one is acked, so is the ack.
    w.send(
        "session.state",
        json!({"session_id": sid, "process_state": "running", "locked_by": "hqc"}),
    )
    .await;
    w.expect("ack").await;
    let v = hub.session(&sid).await;
    assert_eq!(
        (v["queue_len"].as_u64(), v["state"].as_str()),
        (Some(0), Some("idle")),
        "{v}"
    );
    let events = hub.events(&sid).await;
    let stored = events.iter().find(|e| e["event_id"] == b).unwrap();
    assert_eq!(stored["delivery"], "delivered");
    hub.finish().await;
}

/// SPEC §17.2, §6: a `joined` ack is kept on the message (`payload.joined`, what the chat's
/// "joined the turn" reads) and told to the chat in the `delivery` frame; a plain ack keeps
/// and tells neither.
#[tokio::test]
async fn a_joined_ack_is_kept_on_the_message_and_sent_to_the_chat() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("joined flag").await;
    ack_start(&mut w, &sid).await;
    let mut c = FakeClient::connect(&hub).await;
    c.subscribe(&sid, 0).await;
    let (_, r) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": "A"}))
        .await;
    let a = r["event"]["event_id"].clone();
    let si = w.expect("session.input").await;
    w.reply(&si["id"], "ack", json!({"event_id": a})).await;
    let d = c.until("delivery").await;
    assert_eq!((&d["event_id"], &d["delivery"]), (&a, &json!("delivered")));
    assert!(d.get("joined").is_none(), "{d}");

    let (_, r) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": "B"}))
        .await;
    let b = r["event"]["event_id"].clone();
    let si = w.expect("session.input").await;
    assert_eq!(si["payload"]["event_id"], b);
    w.reply(&si["id"], "ack", json!({"event_id": b, "joined": true}))
        .await;
    let d = c.until("delivery").await;
    assert_eq!((&d["event_id"], &d["joined"]), (&b, &json!(true)), "{d}");

    let events = hub.events(&sid).await;
    let payload =
        |id: &Value| events.iter().find(|e| e["event_id"] == *id).unwrap()["payload"].clone();
    assert!(payload(&a).get("joined").is_none(), "{}", payload(&a));
    assert_eq!(payload(&b)["joined"], true);
    // The subscription's backlog is the same stored event.
    let mut c2 = FakeClient::connect(&hub).await;
    let backlog = c2.subscribe(&sid, 0).await;
    let again = backlog.iter().find(|e| e["event_id"] == b).unwrap();
    assert_eq!(again["payload"]["joined"], true);
    hub.finish().await;
}

/// SPEC §5.4, §6: `session.state.subagents` is kept on the session, in the session object and
/// its summary; a report without it is 0 (absent), and so is every session of a restarted
/// worker.
#[tokio::test]
async fn subagents_are_kept_on_the_session() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Subagents").await;
    ack_start(&mut w, &sid).await;
    assert!(hub.session(&sid).await.get("subagents").is_none());
    let mut c = FakeClient::connect(&hub).await;
    c.send(json!({"type": "subscribe_summary"})).await;
    c.until("summary").await;

    let state = |n: Option<u32>| {
        let mut v = json!({"session_id": sid, "process_state": "running", "locked_by": "hqc"});
        if let Some(n) = n {
            v["subagents"] = json!(n);
        }
        v
    };
    w.send("session.state", state(Some(2))).await;
    w.expect("ack").await;
    assert_eq!(hub.session(&sid).await["subagents"], 2);
    loop {
        let u = c.until("session.summary").await;
        if u["session"]["subagents"] == 2 {
            break;
        }
    }
    w.send("session.state", state(None)).await;
    w.expect("ack").await;
    assert!(hub.session(&sid).await.get("subagents").is_none());

    w.send("session.state", state(Some(3))).await;
    w.expect("ack").await;
    assert_eq!(hub.session(&sid).await["subagents"], 3);
    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let (_w, _) = FakeWorker::online_boot(&hub, &token, WS_PATH, Uuid::new_v4()).await;
    let v = hub
        .wait_session(&sid, "stopped", |v| {
            v["agent"]["process_state"] == "stopped"
        })
        .await;
    assert!(v.get("subagents").is_none(), "{v}");
    hub.finish().await;
}

/// SPEC §4, §6: `subagents.live` is not stored, and a client that subscribes gets the latest
/// list before `subscribed`. A worker that restarts while a subagent runs never sends the
/// empty list: the hub drops the kept one itself, its subscribers get an empty list, a new
/// subscriber none.
#[tokio::test]
async fn a_worker_restart_drops_the_kept_subagents() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Subagents").await;
    ack_start(&mut w, &sid).await;
    let mut c = FakeClient::connect(&hub).await;
    c.subscribe(&sid, 0).await;

    let agents = json!([{"task_id": "a1", "description": "Fix the build", "duration_ms": 1000}]);
    let live = agent_event(&sid, "subagents.live", json!({ "agents": agents }));
    let a = w.event(live.clone()).await;
    assert_eq!(a, json!({"event_id": live["event_id"]}), "no seq");
    assert_eq!(c.until("event").await["event"], live);
    let events = hub.events(&sid).await;
    assert!(
        events.iter().all(|e| e["type"] != "subagents.live"),
        "{events:?}"
    );
    let mut late = FakeClient::connect(&hub).await;
    let backlog = late.subscribe(&sid, 0).await;
    let kept = backlog.last().unwrap();
    assert_eq!(kept["type"], "subagents.live", "{backlog:?}");
    assert_eq!(kept["payload"]["agents"][0]["task_id"], "a1");
    assert!(kept["payload"]["agents"][0]["duration_ms"].as_u64() >= Some(1000));

    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let (_w, _) = FakeWorker::online_boot(&hub, &token, WS_PATH, Uuid::new_v4()).await;
    let gone = loop {
        let e = c.until("event").await["event"].clone();
        if e["type"] == "subagents.live" {
            break e;
        }
    };
    assert_eq!(gone["session_id"], sid.as_str());
    assert_eq!(gone["payload"]["agents"], json!([]), "{gone}");
    let mut fresh = FakeClient::connect(&hub).await;
    let backlog = fresh.subscribe(&sid, 0).await;
    assert!(
        backlog.iter().all(|e| e["type"] != "subagents.live"),
        "{backlog:?}"
    );
    hub.finish().await;
}

/// SPEC §4: the empty list goes directly and is lost with a broken link, and a worker that
/// reconnects without a restart repeats only a list that is not empty. A `session.state` that
/// counts no subagent but a background task keeps the list, which holds the agent's background
/// commands; one that counts neither drops the kept list, and so does deleting the session; the
/// subscribers get an empty list, a new subscriber none.
#[tokio::test]
async fn a_count_of_none_drops_the_kept_subagents() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Subagents").await;
    ack_start(&mut w, &sid).await;
    let mut c = FakeClient::connect(&hub).await;
    c.subscribe(&sid, 0).await;
    let agents = json!([{"task_id": "a1", "description": "Fix the build", "duration_ms": 1000}]);
    let live = || agent_event(&sid, "subagents.live", json!({ "agents": agents }));
    async fn next_live(c: &mut FakeClient) -> Value {
        loop {
            let e = c.until("event").await["event"].clone();
            if e["type"] == "subagents.live" {
                break e;
            }
        }
    }

    w.event(live()).await;
    assert_eq!(
        next_live(&mut c).await["payload"]["agents"][0]["task_id"],
        "a1"
    );
    w.send(
        "session.state",
        json!({"session_id": sid, "process_state": "running", "locked_by": "hqc", "subagents": 0,
               "background_tasks": 1}),
    )
    .await;
    w.expect("ack").await;
    let mut kept = FakeClient::connect(&hub).await;
    let backlog = kept.subscribe(&sid, 0).await;
    assert!(
        backlog
            .iter()
            .any(|e| e["type"] == "subagents.live" && e["payload"]["agents"][0]["task_id"] == "a1"),
        "{backlog:?}"
    );
    // No empty list came between: the next one the subscriber gets is the worker's.
    w.event(live()).await;
    assert_eq!(
        next_live(&mut c).await["payload"]["agents"][0]["task_id"],
        "a1"
    );
    w.send(
        "session.state",
        json!({"session_id": sid, "process_state": "running", "locked_by": "hqc", "subagents": 0}),
    )
    .await;
    w.expect("ack").await;
    assert_eq!(next_live(&mut c).await["payload"]["agents"], json!([]));
    let mut fresh = FakeClient::connect(&hub).await;
    let backlog = fresh.subscribe(&sid, 0).await;
    assert!(
        backlog.iter().all(|e| e["type"] != "subagents.live"),
        "{backlog:?}"
    );

    w.event(live()).await;
    assert_eq!(
        next_live(&mut c).await["payload"]["agents"][0]["task_id"],
        "a1"
    );
    assert_eq!(hub.delete(&format!("/api/sessions/{sid}")).await, 204);
    assert_eq!(next_live(&mut c).await["payload"]["agents"], json!([]));
    hub.finish().await;
}

/// SPEC §5.4, §6: `session.state.background_tasks` is kept on the session, in the session object
/// and its summary; the report of a stopped agent is 0 (absent), and so is every session of a
/// restarted worker.
#[tokio::test]
async fn background_tasks_are_kept_on_the_session() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Background tasks").await;
    ack_start(&mut w, &sid).await;
    assert!(hub.session(&sid).await.get("background_tasks").is_none());
    let mut c = FakeClient::connect(&hub).await;
    c.send(json!({"type": "subscribe_summary"})).await;
    c.until("summary").await;

    let running = json!({"session_id": sid, "process_state": "running", "locked_by": "hqc",
        "background_tasks": 1});
    w.send("session.state", running.clone()).await;
    w.expect("ack").await;
    assert_eq!(hub.session(&sid).await["background_tasks"], 1);
    loop {
        let u = c.until("session.summary").await;
        if u["session"]["background_tasks"] == 1 {
            break;
        }
    }
    w.send(
        "session.state",
        json!({"session_id": sid, "process_state": "stopped", "locked_by": "none"}),
    )
    .await;
    w.expect("ack").await;
    assert!(hub.session(&sid).await.get("background_tasks").is_none());

    w.send("session.state", running).await;
    w.expect("ack").await;
    assert_eq!(hub.session(&sid).await["background_tasks"], 1);
    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let (_w, _) = FakeWorker::online_boot(&hub, &token, WS_PATH, Uuid::new_v4()).await;
    let v = hub
        .wait_session(&sid, "stopped", |v| {
            v["agent"]["process_state"] == "stopped"
        })
        .await;
    assert!(v.get("background_tasks").is_none(), "{v}");
    hub.finish().await;
}

/// SPEC §4, §6: `tokens.rate` is transport only (no `seq`, not stored); the hub keeps the rate
/// in memory for the session object and its summary, and 0 drops it (absent).
#[tokio::test]
async fn token_rate_is_kept_on_the_session() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Token rate").await;
    ack_start(&mut w, &sid).await;
    assert!(hub.session(&sid).await.get("output_per_s").is_none());
    let mut c = FakeClient::connect(&hub).await;
    c.send(json!({"type": "subscribe_summary"})).await;
    c.until("summary").await;

    let rate = agent_event(&sid, "tokens.rate", json!({"output_per_s": 42}));
    let a = w.event(rate.clone()).await;
    assert_eq!(
        a,
        json!({"event_id": rate["event_id"]}),
        "no seq for tokens.rate"
    );
    assert_eq!(hub.session(&sid).await["output_per_s"], 42);
    loop {
        let u = c.until("session.summary").await;
        if u["session"]["output_per_s"] == 42 {
            break;
        }
    }
    w.event(agent_event(&sid, "tokens.rate", json!({"output_per_s": 0})))
        .await;
    assert!(hub.session(&sid).await.get("output_per_s").is_none());
    loop {
        let u = c.until("session.summary").await;
        if u["session"].get("output_per_s").is_none() {
            break;
        }
    }
    // A rate goes with its worker: a 0 sent while it was away would be lost.
    w.event(agent_event(
        &sid,
        "tokens.rate",
        json!({"output_per_s": 42}),
    ))
    .await;
    assert_eq!(hub.session(&sid).await["output_per_s"], 42);
    w.close().await;
    // The hub drops the rates after it has taken the worker off, past a read of the database.
    hub.wait_session(&sid, "worker_offline without a rate", |v| {
        v["state"] == "worker_offline" && v.get("output_per_s").is_none()
    })
    .await;
    // A connection that replaces one the hub still holds: the rates go with the old one.
    let (mut w2, _) = FakeWorker::online_boot(&hub, &token, WS_PATH, Uuid::new_v4()).await;
    w2.event(agent_event(
        &sid,
        "tokens.rate",
        json!({"output_per_s": 42}),
    ))
    .await;
    assert_eq!(hub.session(&sid).await["output_per_s"], 42);
    let (_w3, _) = FakeWorker::online_boot(&hub, &token, WS_PATH, Uuid::new_v4()).await;
    assert!(hub.session(&sid).await.get("output_per_s").is_none());
    let events = hub.events(&sid).await;
    assert!(events.iter().all(|e| e["type"] != "tokens.rate"));
    hub.finish().await;
}

/// SPEC §5.3, §5.4, §6: `session.state.foreground_calls` is kept on the session, in the session
/// object and its summary, and goes when the agent stops and when its worker restarts;
/// `POST .../background` reaches the worker as `session.background`, a `call_id` that is empty,
/// too long or has a NUL is refused.
#[tokio::test]
async fn a_foreground_call_goes_to_the_background() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Foreground calls").await;
    ack_start(&mut w, &sid).await;
    assert!(hub.session(&sid).await.get("foreground_calls").is_none());
    let mut c = FakeClient::connect(&hub).await;
    c.send(json!({"type": "subscribe_summary"})).await;
    c.until("summary").await;

    let call = "toolu_01FoCFY4DmUbVxXAembc44PX";
    let running = json!({"session_id": sid, "process_state": "running", "locked_by": "hqc",
        "foreground_calls": [call]});
    w.send("session.state", running.clone()).await;
    w.expect("ack").await;
    assert_eq!(hub.session(&sid).await["foreground_calls"], json!([call]));
    loop {
        let u = c.until("session.summary").await;
        if u["session"]["foreground_calls"] == json!([call]) {
            break;
        }
    }

    // The worker is trusted for the list, not for its size: 64 unique ids of 1..=128 bytes
    // without whitespace or control characters are kept.
    let many: Vec<String> = (0..70).map(|i| format!("toolu_{i:02}")).collect();
    let mut flood = vec![
        call.to_string(),
        call.to_string(),
        String::new(),
        "x".repeat(129),
    ];
    flood.push("toolu_\n1".into());
    flood.extend(many.iter().cloned());
    let flooded = json!({"session_id": sid, "process_state": "running", "locked_by": "hqc",
        "foreground_calls": flood});
    w.send("session.state", flooded).await;
    w.expect("ack").await;
    let mut kept = vec![call.to_string()];
    kept.extend(many.iter().take(63).cloned());
    assert_eq!(hub.session(&sid).await["foreground_calls"], json!(kept));
    w.send("session.state", running.clone()).await;
    w.expect("ack").await;

    let path = format!("/api/sessions/{sid}/background");
    let (s, r) = hub.post(&path, json!({"call_id": call})).await;
    assert_eq!((s, r["sent"].as_bool()), (200, Some(true)));
    let f = w.expect("session.background").await;
    assert_eq!(f["payload"], json!({"session_id": sid, "call_id": call}));
    let long = "x".repeat(129);
    for bad in ["", "toolu_\u{0}", long.as_str()] {
        let (s, _) = hub.post(&path, json!({"call_id": bad})).await;
        assert_eq!(s, 400, "{bad:?}");
    }

    w.send(
        "session.state",
        json!({"session_id": sid, "process_state": "stopped", "locked_by": "none"}),
    )
    .await;
    w.expect("ack").await;
    assert!(hub.session(&sid).await.get("foreground_calls").is_none());

    w.send("session.state", running).await;
    w.expect("ack").await;
    assert_eq!(hub.session(&sid).await["foreground_calls"], json!([call]));
    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let (_w, _) = FakeWorker::online_boot(&hub, &token, WS_PATH, Uuid::new_v4()).await;
    // The list goes right after the process state, outside its statement.
    hub.wait_session(&sid, "stopped, no foreground calls", |v| {
        v["agent"]["process_state"] == "stopped" && v.get("foreground_calls").is_none()
    })
    .await;
    hub.finish().await;
}

/// SPEC §6: `GET /api/sessions` is the union of the lists of the user's projects, archived and
/// service sessions included, the newest `updated_at` first; another user's sessions are not in
/// it.
#[tokio::test]
async fn all_sessions_are_the_union_of_the_projects_lists() {
    let hub = TestHub::start().await;
    let db = &hub.app.db;
    let me = hqc_hub::db::ensure_user(db, &hub.email).await.unwrap();
    let personal = me.personal_project_id.unwrap();
    let boards = hqc_hub::db::create_project(db, me.id, "Boards", "")
        .await
        .unwrap()
        .id;
    let stranger = hqc_hub::db::ensure_user(
        db,
        &format!("stranger-{}@example.com", Uuid::new_v4().simple()),
    )
    .await
    .unwrap();
    let theirs = hqc_hub::db::create_project(db, stranger.id, "Theirs", "")
        .await
        .unwrap()
        .id;
    let new = |owner: Uuid, project: Uuid, title: &'static str| async move {
        hqc_hub::db::create_session(
            db,
            hqc_hub::db::NewSession {
                project_id: project,
                owner_id: owner,
                workspace_id: None,
                worker_id: None,
                cwd: ".",
                title: Some(title),
                model: None,
                auto_approve: false,
                permission_mode: None,
            },
        )
        .await
        .unwrap()
    };
    new(me.id, personal, "Notes").await;
    new(me.id, boards, "Power board").await;
    let archived = new(me.id, boards, "Old layout").await;
    let system = new(me.id, personal, "home-mac").await;
    let foreign = new(stranger.id, theirs, "Not mine").await;
    sqlx::query("UPDATE sessions SET archived = true, archived_at = now() WHERE id = $1")
        .bind(archived)
        .execute(db)
        .await
        .unwrap();
    sqlx::query("UPDATE sessions SET system = true WHERE id = $1")
        .bind(system)
        .execute(db)
        .await
        .unwrap();

    let (s, v) = hub.get("/api/sessions").await;
    assert_eq!(s, 200, "{v}");
    let all = v["sessions"].as_array().unwrap().clone();
    let (_, projects) = hub.get("/api/projects").await;
    let mut union = Vec::new();
    for p in projects["projects"].as_array().unwrap() {
        let (s, l) = hub
            .get(&format!(
                "/api/projects/{}/sessions",
                p["id"].as_str().unwrap()
            ))
            .await;
        assert_eq!(s, 200, "{l}");
        union.extend(l["sessions"].as_array().unwrap().iter().cloned());
    }
    let by_id = |l: &[Value]| {
        let mut l = l.to_vec();
        l.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        l
    };
    assert_eq!(by_id(&all), by_id(&union));
    assert_eq!(all.len(), 4, "{v}");
    let row = |id: Uuid| all.iter().find(|s| s["id"] == id.to_string().as_str());
    assert_eq!(row(archived).unwrap()["archived"], true);
    assert_eq!(row(system).unwrap()["system"], true);
    assert!(row(foreign).is_none(), "{v}");
    let times: Vec<chrono::DateTime<Utc>> = all
        .iter()
        .map(|s| s["updated_at"].as_str().unwrap().parse().unwrap())
        .collect();
    assert!(times.windows(2).all(|w| w[0] >= w[1]), "{times:?}");
    hub.finish().await;
}
