//! T40 (SPEC §7.1, §16): the cap for buffered sends and old schedules. An input that carries
//! a buffered `hqc_send` from a session in Ask runs capped, even a person's own; each run of
//! an agent's schedule is capped by the lower of the stored cap and its creator's mode at run
//! time, a deleted creator counting as Ask.

mod common;

use chrono::{DateTime, Utc};
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const WS_PATH: &str = "/Users/home-mac/work/hqc";

/// A started session in the personal project, created and reported running in `mode`; its
/// id and token.
async fn session(hub: &TestHub, w: &mut FakeWorker, title: &str, mode: &str) -> (String, String) {
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
    report(w, &sid, json!({"permission_mode": mode})).await;
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

/// A person's input to `sid`; its `session.input`, delivered.
async fn human_input(hub: &TestHub, w: &mut FakeWorker, sid: &str) -> Value {
    let (s, v) = hub
        .post(&format!("/api/sessions/{sid}/input"), json!({"text": "go"}))
        .await;
    assert_eq!(s, 201, "{v}");
    let input = input_of(w, vec![], sid).await;
    deliver(w, &input).await;
    input
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

async fn send(w: &mut FakeWorker, token: &str, to: &str, text: &str) {
    let (r, _) = call(w, token, "hqc_send", json!({"session": to, "text": text})).await;
    assert_eq!(r["result"]["delivery"], "buffered", "{r}");
}

/// A buffered send from a session in Ask caps the turn of the person's input that carries
/// it, names that session in the header and caps what the turn sends on; one from a session
/// in Auto caps it to Auto (since 0.129), which holds only an Allow all session down.
#[tokio::test]
async fn a_buffered_send_caps_the_input_that_carries_it() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = session(&hub, &mut w, "A", "ask").await;
    let (c, tc) = session(&hub, &mut w, "C", "auto").await;
    let (b, tb) = session(&hub, &mut w, "B", "auto").await;
    let (d, _) = session(&hub, &mut w, "D", "auto").await;
    for s in [&a, &b, &c, &d] {
        hub.buffer_sends(s).await;
    }
    human_input(&hub, &mut w, &a).await;
    human_input(&hub, &mut w, &c).await;

    // From Auto: capped to Auto, which is B's own mode.
    send(&mut w, &tc, "personal/b", "from auto").await;
    let input = human_input(&hub, &mut w, &b).await;
    assert_eq!(input["payload"]["peer_prefix"][0]["text"], "from auto");
    assert_eq!(input["payload"]["permission_cap"], "auto", "{input}");

    // From Auto, then from Ask: the lowest cap of the two, named by the session in Ask.
    send(&mut w, &tc, "personal/b", "from auto again").await;
    send(&mut w, &ta, "personal/b", "from ask").await;
    let input = human_input(&hub, &mut w, &b).await;
    assert_eq!(
        input["payload"]["peer_prefix"].as_array().unwrap().len(),
        2,
        "{input}"
    );
    assert_eq!(input["payload"]["permission_cap"], "ask", "{input}");
    report(
        &mut w,
        &b,
        json!({"permission_mode": "ask", "capped_input": input["payload"]["event_id"]}),
    )
    .await;
    let v = hub.session(&b).await;
    assert_eq!(v["permission_capped_by"], "personal/a", "{v}");
    // The turn is capped as a whole: what it sends on is capped too, by A.
    let (_, seen) = call(
        &mut w,
        &tb,
        "hqc_ask",
        json!({"session": "personal/d", "text": "and you?"}),
    )
    .await;
    let to_d = input_of(&mut w, seen, &d).await;
    assert_eq!(to_d["payload"]["permission_cap"], "ask", "{to_d}");
    let chain: Value = sqlx::query_scalar("SELECT chain FROM events WHERE event_id = $1")
        .bind(
            to_d["payload"]["event_id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        )
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
    assert_eq!(chain["capped_by"], "personal/a", "{chain}");
    // The person's own input is stored as it came: no chain.
    let stored: Option<Value> = sqlx::query_scalar("SELECT chain FROM events WHERE event_id = $1")
        .bind(
            input["payload"]["event_id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        )
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
    assert_eq!(stored, None);
    report(&mut w, &b, json!({"permission_mode": "auto"})).await;
    assert!(hub.session(&b).await.get("permission_capped_by").is_none());

    // The buffer is empty again: the next input of the person runs uncapped.
    let input = human_input(&hub, &mut w, &b).await;
    assert!(input["payload"].get("permission_cap").is_none(), "{input}");
    hub.finish().await;
}

/// SPEC 0.129: a buffered send from a session in Auto holds the person's input to an Allow
/// all session down to Auto, named by the sender; one from an Allow all session caps nothing.
#[tokio::test]
async fn a_buffered_send_from_auto_caps_an_allow_all_session() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = session(&hub, &mut w, "A", "auto").await;
    let (c, tc) = session(&hub, &mut w, "C", "allow_all").await;
    let (b, _) = session(&hub, &mut w, "B", "allow_all").await;
    for s in [&a, &b, &c] {
        hub.buffer_sends(s).await;
    }
    human_input(&hub, &mut w, &a).await;
    human_input(&hub, &mut w, &c).await;

    send(&mut w, &tc, "personal/b", "from allow all").await;
    let input = human_input(&hub, &mut w, &b).await;
    assert!(input["payload"].get("permission_cap").is_none(), "{input}");

    send(&mut w, &ta, "personal/b", "from auto").await;
    let input = human_input(&hub, &mut w, &b).await;
    assert_eq!(input["payload"]["permission_cap"], "auto", "{input}");
    report(
        &mut w,
        &b,
        json!({"permission_mode": "auto", "capped_input": input["payload"]["event_id"]}),
    )
    .await;
    let v = hub.session(&b).await;
    assert_eq!(v["permission_capped_by"], "personal/a", "{v}");
    assert_eq!(v["permission_mode"], "allow_all");
    report(&mut w, &b, json!({"permission_mode": "allow_all"})).await;
    assert!(hub.session(&b).await.get("permission_capped_by").is_none());
    hub.finish().await;
}

fn utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

/// Makes a schedule of the session of `token` for `personal/b` at `at`; its id.
async fn schedule(w: &mut FakeWorker, token: &str, at: &str, message: &str) -> Uuid {
    let (r, _) = call(
        w,
        token,
        "hqc_schedule",
        json!({"session": "personal/b", "at": at, "message": message}),
    )
    .await;
    r["result"]["schedule_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

/// A run of an agent's schedule takes the lower of the cap it was made with and its
/// creator's mode now: a creator that moved from Auto to Ask caps it, one still in Auto
/// does not, a deleted one counts as Ask.
#[tokio::test]
async fn a_schedule_run_is_capped_by_its_creators_mode_now() {
    let hub = TestHub::start().await;
    hub.app.clock.set(utc("2026-10-26T10:00:00Z"));
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = session(&hub, &mut w, "A", "auto").await;
    let (x, tx) = session(&hub, &mut w, "X", "auto").await;
    let (b, _) = session(&hub, &mut w, "B", "auto").await;
    human_input(&hub, &mut w, &a).await;
    human_input(&hub, &mut w, &x).await;
    let first = schedule(&mut w, &ta, "2026-10-26T10:05:00Z", "while auto").await;
    schedule(&mut w, &ta, "2026-10-26T10:10:00Z", "after ask").await;
    schedule(&mut w, &tx, "2026-10-26T10:15:00Z", "creator gone").await;
    let stored: Value = sqlx::query_scalar("SELECT chain FROM scheduled_messages WHERE id = $1")
        .bind(first)
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
    assert_eq!(stored["permission_cap"], "auto", "{stored}");

    // A is still in Auto: the run is not capped below it.
    let run = |at: &'static str| {
        let app = hub.app.clone();
        async move {
            app.clock.set(utc(at));
            assert_eq!(hqc_hub::schedules::run_due(&app).await.unwrap(), 1);
        }
    };
    run("2026-10-26T10:05:01Z").await;
    let input = input_of(&mut w, vec![], &b).await;
    assert_eq!(input["payload"]["text"], "while auto");
    assert_eq!(input["payload"]["permission_cap"], "auto", "{input}");
    deliver(&mut w, &input).await;

    // A moved to Ask: its schedule's next run runs in Ask, named by A.
    report(&mut w, &a, json!({"permission_mode": "ask"})).await;
    run("2026-10-26T10:10:01Z").await;
    let input = input_of(&mut w, vec![], &b).await;
    assert_eq!(input["payload"]["text"], "after ask");
    assert_eq!(input["payload"]["permission_cap"], "ask", "{input}");
    deliver(&mut w, &input).await;
    report(
        &mut w,
        &b,
        json!({"permission_mode": "ask", "capped_input": input["payload"]["event_id"]}),
    )
    .await;
    assert_eq!(hub.session(&b).await["permission_capped_by"], "personal/a");
    report(&mut w, &b, json!({"permission_mode": "auto"})).await;

    // X was deleted: its schedule lives on, and its runs count it as Ask.
    assert_eq!(hub.delete(&format!("/api/sessions/{x}")).await, 204);
    w.expect("session.kill").await;
    run("2026-10-26T10:15:01Z").await;
    let input = input_of(&mut w, vec![], &b).await;
    assert_eq!(input["payload"]["text"], "creator gone");
    assert_eq!(input["payload"]["permission_cap"], "ask", "{input}");
    deliver(&mut w, &input).await;
    report(
        &mut w,
        &b,
        json!({"permission_mode": "ask", "capped_input": input["payload"]["event_id"]}),
    )
    .await;
    assert_eq!(
        hub.session(&b).await["permission_capped_by"],
        hqc_hub::schedules::GONE_CREATOR
    );
    hub.finish().await;
}

/// SPEC 0.129: a schedule an agent in Auto made before 0.129 stores an `auto` cap that names
/// no session; its runs hold an Allow all session down to Auto, named by the creator.
#[tokio::test]
async fn an_unnamed_auto_cap_of_an_old_schedule_is_named_by_its_creator() {
    let hub = TestHub::start().await;
    hub.app.clock.set(utc("2026-10-26T10:00:00Z"));
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = session(&hub, &mut w, "A", "auto").await;
    let (b, _) = session(&hub, &mut w, "B", "allow_all").await;
    human_input(&hub, &mut w, &a).await;
    let id = schedule(&mut w, &ta, "2026-10-26T10:05:00Z", "old schedule").await;
    sqlx::query("UPDATE scheduled_messages SET chain = chain - 'capped_by' WHERE id = $1")
        .bind(id)
        .execute(&hub.app.db)
        .await
        .unwrap();

    hub.app.clock.set(utc("2026-10-26T10:05:01Z"));
    assert_eq!(hqc_hub::schedules::run_due(&hub.app).await.unwrap(), 1);
    let input = input_of(&mut w, vec![], &b).await;
    assert_eq!(input["payload"]["permission_cap"], "auto", "{input}");
    deliver(&mut w, &input).await;
    report(
        &mut w,
        &b,
        json!({"permission_mode": "auto", "capped_input": input["payload"]["event_id"]}),
    )
    .await;
    assert_eq!(hub.session(&b).await["permission_capped_by"], "personal/a");
    hub.finish().await;
}

/// SPEC 0.45: a creator the user switched to Ask caps its schedule's runs at once, before
/// its worker confirms the switch.
#[tokio::test]
async fn a_switch_to_ask_caps_before_the_worker_confirms_it() {
    let hub = TestHub::start().await;
    hub.app.clock.set(utc("2026-10-26T10:00:00Z"));
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = session(&hub, &mut w, "A", "auto").await;
    let (b, _) = session(&hub, &mut w, "B", "auto").await;
    human_input(&hub, &mut w, &a).await;
    schedule(&mut w, &ta, "2026-10-26T10:05:00Z", "after the switch").await;

    let (s, v) = hub
        .patch(
            &format!("/api/sessions/{a}"),
            json!({"permission_mode": "ask"}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    // The worker has not confirmed: A still runs in auto as far as it reported.
    assert_eq!(v["effective_permission_mode"], "auto", "{v}");
    w.expect("session.permission_mode").await;

    hub.app.clock.set(utc("2026-10-26T10:05:01Z"));
    assert_eq!(hqc_hub::schedules::run_due(&hub.app).await.unwrap(), 1);
    let input = input_of(&mut w, vec![], &b).await;
    assert_eq!(input["payload"]["text"], "after the switch");
    assert_eq!(input["payload"]["permission_cap"], "ask", "{input}");
    hub.finish().await;
}
