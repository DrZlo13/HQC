//! T39 (SPEC §5.3, §7.1, §10.1, §16): a message cannot raise permissions. A peer message,
//! the prompt of `hqc_spawn` and the runs of an agent's schedule carry a cap in their chain:
//! the lower of the sender's reported mode and the cap of the sender's turn. `session.input`
//! carries it to the worker, and the session shows who capped its turn while the worker
//! reports it.

mod common;

use chrono::{DateTime, Utc};
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const WS_PATH: &str = "/Users/home-mac/work/hqc";

/// A started session in the personal project, created with `mode` and reported running in
/// `mode` (or in `reported`, when given); its id and token.
async fn session(
    hub: &TestHub,
    w: &mut FakeWorker,
    title: &str,
    mode: &str,
    reported: Option<&str>,
) -> (String, String) {
    let pid = hub.personal_project().await;
    let (s, v) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": title, "permission_mode": mode}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    let f = ack_start(w, &sid).await;
    if let Some(m) = reported.or(Some(mode)).filter(|m| !m.is_empty()) {
        report(w, &sid, json!({"permission_mode": m})).await;
    }
    (sid, f["payload"]["session_token"].as_str().unwrap().into())
}

/// `session.state` of a running agent with `extra` fields.
async fn report(w: &mut FakeWorker, sid: &str, extra: Value) {
    let mut p = json!({"session_id": sid, "process_state": "running", "locked_by": "hqc"});
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    let id = w.send("session.state", p).await;
    loop {
        let f = w.recv().await;
        if f["type"] == "ack" && f["id"] == id.as_str() {
            return;
        }
    }
}

/// A human's input to `sid`, delivered: the session's turn is a human's.
async fn human_turn(hub: &TestHub, w: &mut FakeWorker, sid: &str) {
    let (s, v) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": "go"}))
        .await;
    assert_eq!(s, 201, "{v}");
    let input = w.expect("session.input").await;
    assert!(
        input["payload"].get("permission_cap").is_none(),
        "a person's input has no cap: {input}"
    );
    deliver(w, &input).await;
    hub.wait_session(sid, "running", |v| v["state"] == "running")
        .await;
}

async fn deliver(w: &mut FakeWorker, input: &Value) {
    w.reply(
        &input["id"],
        "ack",
        json!({"event_id": input["payload"]["event_id"]}),
    )
    .await;
}

/// Calls `tool` as the session of `token`; the `mcp.result` payload, or `{"ack": true}` for
/// a deferred call, and the frames that came meanwhile.
async fn call(w: &mut FakeWorker, token: &str, tool: &str, args: Value) -> (Value, Vec<Value>) {
    let id = w
        .send(
            "mcp.call",
            json!({"request_id": Uuid::now_v7().to_string(), "session_token": token,
                   "tool": tool, "args": args}),
        )
        .await;
    let mut other = Vec::new();
    loop {
        let f = w.recv().await;
        if f["id"] == id.as_str() && f["type"] == "mcp.result" {
            assert!(f["payload"].get("error").is_none(), "{f}");
            return (f["payload"].clone(), other);
        }
        if f["id"] == id.as_str() && f["type"] == "ack" {
            return (json!({"ack": true}), other);
        }
        other.push(f);
    }
}

/// The `session.input` of session `sid`: among `seen`, or the next one to come.
async fn input_of(w: &mut FakeWorker, seen: Vec<Value>, sid: &str) -> Value {
    if let Some(f) = seen
        .into_iter()
        .find(|f| f["type"] == "session.input" && f["payload"]["session_id"] == sid)
    {
        return f;
    }
    loop {
        let f = w.expect("session.input").await;
        if f["payload"]["session_id"] == sid {
            return f;
        }
    }
}

/// `sid` asks `to`; the input `to` gets, delivered, so `to`'s turn runs on it.
async fn ask(w: &mut FakeWorker, token: &str, to: &str, to_sid: &str) -> Value {
    let (_, seen) = call(
        w,
        token,
        "hqc_ask",
        json!({"session": to, "text": "please run the tests"}),
    )
    .await;
    let input = input_of(w, seen, to_sid).await;
    deliver(w, &input).await;
    input
}

async fn stored_chain(hub: &TestHub, input: &Value) -> Value {
    sqlx::query_scalar("SELECT chain FROM events WHERE event_id = $1")
        .bind(
            input["payload"]["event_id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        )
        .fetch_one(&hub.app.db)
        .await
        .unwrap()
}

/// Sender × target in Auto and Ask: the cap is the sender's mode, whatever the target's;
/// the target shows who capped it while its worker says the cap holds it down.
#[tokio::test]
async fn the_cap_is_the_senders_mode_in_all_four_combinations() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    for (i, (sender, target)) in [
        ("auto", "auto"),
        ("auto", "ask"),
        ("ask", "auto"),
        ("ask", "ask"),
    ]
    .into_iter()
    .enumerate()
    {
        let (a, ta) = session(&hub, &mut w, &format!("S{i}"), sender, None).await;
        let (b, _) = session(&hub, &mut w, &format!("T{i}"), target, None).await;
        human_turn(&hub, &mut w, &a).await;
        let input = ask(&mut w, &ta, &format!("personal/t{i}"), &b).await;
        assert_eq!(
            input["payload"]["permission_cap"], sender,
            "{sender} -> {target}: {input}"
        );
        let chain = stored_chain(&hub, &input).await;
        assert_eq!(chain["permission_cap"], sender);
        if sender == "ask" {
            assert_eq!(chain["capped_by"], format!("personal/s{i}"));
        } else {
            assert!(chain.get("capped_by").is_none(), "{chain}");
        }
        if (sender, target) == ("ask", "auto") {
            // The worker runs the turn in ask and says why; the header names the sender.
            report(
                &mut w,
                &b,
                json!({"permission_mode": "ask", "capped_input": input["payload"]["event_id"]}),
            )
            .await;
            let v = hub.session(&b).await;
            assert_eq!(v["permission_capped_by"], format!("personal/s{i}"), "{v}");
            assert_eq!(v["effective_permission_mode"], "ask");
            assert_eq!(v["permission_mode"], "auto");
            // The turn is over: the session's own mode, and no cap.
            report(&mut w, &b, json!({"permission_mode": "auto"})).await;
            let v = hub.session(&b).await;
            assert!(v.get("permission_capped_by").is_none(), "{v}");
            assert_eq!(v["effective_permission_mode"], "auto");
        }
    }
    hub.finish().await;
}

/// A (Auto) -> B (Ask) -> C (Auto) -> D (Auto): from B on, every turn down the chain is capped
/// to Ask, named by B; a spawn under the cap carries it too.
#[tokio::test]
async fn ask_in_the_middle_of_a_chain_caps_everything_below_it() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = session(&hub, &mut w, "A", "auto", None).await;
    let (b, tb) = session(&hub, &mut w, "B", "ask", None).await;
    let (c, tc) = session(&hub, &mut w, "C", "auto", None).await;
    let (d, _) = session(&hub, &mut w, "D", "auto", None).await;
    human_turn(&hub, &mut w, &a).await;

    let to_b = ask(&mut w, &ta, "personal/b", &b).await;
    assert_eq!(to_b["payload"]["permission_cap"], "auto");
    let to_c = ask(&mut w, &tb, "personal/c", &c).await;
    assert_eq!(to_c["payload"]["permission_cap"], "ask");
    assert_eq!(stored_chain(&hub, &to_c).await["capped_by"], "personal/b");
    // C runs that turn in ask (its worker says so); its own mode is still auto.
    report(
        &mut w,
        &c,
        json!({"permission_mode": "ask", "capped_input": to_c["payload"]["event_id"]}),
    )
    .await;
    let to_d = ask(&mut w, &tc, "personal/d", &d).await;
    assert_eq!(to_d["payload"]["permission_cap"], "ask");
    let chain = stored_chain(&hub, &to_d).await;
    assert_eq!(
        (
            &chain["permission_cap"],
            &chain["capped_by"],
            &chain["depth"]
        ),
        (&json!("ask"), &json!("personal/b"), &json!(3))
    );
    // Even if C's worker reported auto: the cap comes from up the chain.
    report(&mut w, &c, json!({"permission_mode": "auto"})).await;
    let (r, seen) = call(
        &mut w,
        &tc,
        "hqc_spawn",
        json!({"project": "personal", "prompt": "Count the files."}),
    )
    .await;
    assert!(r["result"]["address"].is_string(), "{r}");
    let start = match seen.iter().find(|f| f["type"] == "session.start") {
        Some(f) => f.clone(),
        None => w.expect("session.start").await,
    };
    let spawned = start["payload"]["session_id"].as_str().unwrap().to_string();
    w.reply(&start["id"], "ack", json!({})).await;
    let prompt = input_of(&mut w, seen, &spawned).await;
    assert_eq!(prompt["payload"]["permission_cap"], "ask");
    hub.finish().await;
}

/// The sender's effective mode counts, not the one asked for; before any report it is ask.
#[tokio::test]
async fn the_reported_mode_counts() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    // Asked for auto, but the worker runs ask (`allow_auto = false`).
    let (a, ta) = session(&hub, &mut w, "A", "auto", Some("ask")).await;
    // Asked for auto, never reported.
    let (n, tn) = session(&hub, &mut w, "N", "auto", Some("")).await;
    let (b, _) = session(&hub, &mut w, "B", "auto", None).await;
    human_turn(&hub, &mut w, &a).await;
    let input = ask(&mut w, &ta, "personal/b", &b).await;
    assert_eq!(input["payload"]["permission_cap"], "ask");
    human_turn(&hub, &mut w, &n).await;
    let input = ask(&mut w, &tn, "personal/b", &b).await;
    assert_eq!(input["payload"]["permission_cap"], "ask");
    hub.finish().await;
}

fn utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

/// An agent's schedule stores the cap with its chain and its runs carry it; a person's
/// schedule has none.
#[tokio::test]
async fn an_agents_schedule_keeps_its_cap() {
    let hub = TestHub::start().await;
    hub.app.clock.set(utc("2026-10-25T10:00:00Z"));
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = session(&hub, &mut w, "A", "ask", None).await;
    let (b, _) = session(&hub, &mut w, "B", "auto", None).await;
    human_turn(&hub, &mut w, &a).await;
    let (r, _) = call(
        &mut w,
        &ta,
        "hqc_schedule",
        json!({"session": "personal/b", "at": "2026-10-25T10:05:00Z", "message": "nightly"}),
    )
    .await;
    let id: Uuid = r["result"]["schedule_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let chain: Value = sqlx::query_scalar("SELECT chain FROM scheduled_messages WHERE id = $1")
        .bind(id)
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
    assert_eq!(
        (&chain["permission_cap"], &chain["capped_by"]),
        (&json!("ask"), &json!("personal/a"))
    );
    // Even after A switches to auto, the schedule keeps the cap it was made with.
    report(&mut w, &a, json!({"permission_mode": "auto"})).await;
    hub.app.clock.set(utc("2026-10-25T10:05:01Z"));
    assert_eq!(hqc_hub::schedules::run_due(&hub.app).await.unwrap(), 1);
    let input = input_of(&mut w, vec![], &b).await;
    assert_eq!(input["payload"]["text"], "nightly");
    assert_eq!(input["payload"]["permission_cap"], "ask");
    deliver(&mut w, &input).await;
    report(
        &mut w,
        &b,
        json!({"permission_mode": "ask", "capped_input": input["payload"]["event_id"]}),
    )
    .await;
    assert_eq!(hub.session(&b).await["permission_capped_by"], "personal/a");
    report(&mut w, &b, json!({"permission_mode": "auto"})).await;

    // A person's schedule: the run is the person's input, no cap.
    let (s, v) = hub
        .post(
            "/api/schedules",
            json!({"session_id": b, "at": "2026-10-25T11:00:00Z", "message": "by hand"}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    hub.app.clock.set(utc("2026-10-25T11:00:01Z"));
    assert_eq!(hqc_hub::schedules::run_due(&hub.app).await.unwrap(), 1);
    let input = input_of(&mut w, vec![], &b).await;
    assert_eq!(input["payload"]["text"], "by hand");
    assert!(input["payload"].get("permission_cap").is_none(), "{input}");
    hub.finish().await;
}

/// A worker restart ends every turn, and with it every cap.
#[tokio::test]
async fn a_worker_restart_clears_the_cap() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = session(&hub, &mut w, "A", "ask", None).await;
    let (b, _) = session(&hub, &mut w, "B", "auto", None).await;
    human_turn(&hub, &mut w, &a).await;
    let input = ask(&mut w, &ta, "personal/b", &b).await;
    report(
        &mut w,
        &b,
        json!({"permission_mode": "ask", "capped_input": input["payload"]["event_id"]}),
    )
    .await;
    assert_eq!(hub.session(&b).await["permission_capped_by"], "personal/a");
    w.close().await;
    let (_w, _) = FakeWorker::online_boot(&hub, &t, WS_PATH, Uuid::now_v7()).await;
    let v = hub
        .wait_session(&b, "cap cleared", |v| {
            v.get("permission_capped_by").is_none()
        })
        .await;
    assert_eq!(v["agent"]["process_state"], "stopped");
    hub.finish().await;
}
