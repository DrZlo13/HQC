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
    assert_eq!(p["environment"], "default");
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
    let v = hub
        .wait_session(&sid, "error", |v| v["state"] == "error")
        .await;
    assert_eq!(v["queue_len"], 0);
    assert_eq!(v["agent"]["start_failures"], 3);
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
    let events = hub.events(&sid).await;
    let cancel = events
        .iter()
        .find(|e| e["type"] == "approval.cancel")
        .expect("approval.cancel");
    assert_eq!(
        cancel["payload"],
        json!({"request_id": "req-b", "reason": "worker_restart"})
    );
    assert!(events
        .iter()
        .any(|e| e["type"] == "system" && e["payload"]["code"] == "turn_lost"));
    let (s, _) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/req-b"),
            json!({"option_id": "allow"}),
        )
        .await;
    assert_eq!(s, 409, "the cancelled request is closed");
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
            json!({"endpoint": "https://push.example/sub-1", "expirationTime": null,
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
        assert_eq!(sent[0].0, "https://push.example/sub-1");
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
