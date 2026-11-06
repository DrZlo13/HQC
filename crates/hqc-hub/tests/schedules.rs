//! T35 (SPEC §16, §3 ScheduledMessage, §7): `hqc_schedule`, `/api/schedules` and the hub
//! cron, on the hub's fake clock (`app.clock`): `at`, `cron` across a DST change, catch-up
//! and skip, pause and delete, a deleted target session, limits and repeated calls.

mod common;

use chrono::{DateTime, Utc};
use common::*;
use hqc_hub::schedules;
use serde_json::{json, Value};
use uuid::Uuid;

const WS_PATH: &str = "/Users/home-mac/work/hqc";

fn utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

/// A started session of the worker's default workspace: `(id, session token)`.
async fn started(hub: &TestHub, w: &mut FakeWorker, title: &str) -> (String, String) {
    let pid = hub.personal_project().await;
    let (s, v) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": title}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    let f = ack_start(w, &sid).await;
    (sid, f["payload"]["session_token"].as_str().unwrap().into())
}

/// `hqc_schedule` with `request_id` `rid`: the `mcp.result` payload.
async fn schedule(w: &mut FakeWorker, token: &str, args: Value, rid: &str) -> Value {
    let id = w
        .send(
            "mcp.call",
            json!({"request_id": rid, "session_token": token, "tool": "hqc_schedule", "args": args}),
        )
        .await;
    let r = w.expect("mcp.result").await;
    assert_eq!(r["id"], id.as_str(), "{r}");
    r["payload"].clone()
}

fn code(r: &Value) -> &str {
    assert!(r.get("result").is_none(), "{r}");
    r["error"]["code"].as_str().unwrap()
}

/// The clock at `t`, then one pass of the cron: the number of messages sent.
async fn run_at(hub: &TestHub, t: &str) -> usize {
    hub.app.clock.set(utc(t));
    schedules::run_due(&hub.app).await.unwrap()
}

async fn scheduled_inputs(hub: &TestHub, sid: &str) -> Vec<Value> {
    hub.events(sid)
        .await
        .into_iter()
        .filter(|e| e["type"] == "user.message" && e["payload"].get("schedule_id").is_some())
        .collect()
}

async fn get_schedule(hub: &TestHub, sid: &str, id: &str) -> Value {
    let (s, v) = hub.get(&format!("/api/schedules?session_id={sid}")).await;
    assert_eq!(s, 200, "{v}");
    v["schedules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .cloned()
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn an_at_is_sent_once_as_the_owners_input() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = started(&hub, &mut w, "Reminders").await;
    hub.app.clock.set(utc("2026-03-01T10:00:00Z"));

    let rid = Uuid::now_v7().to_string();
    let args = json!({"session": "personal/reminders", "message": "Check the build",
                      "at": "2026-03-01T11:30:00+01:00"});
    let r = schedule(&mut w, &ta, args.clone(), &rid).await;
    let res = &r["result"];
    assert_eq!(res["session"], "personal/reminders", "{r}");
    assert_eq!(res["next_run"], "2026-03-01T10:30:00Z");
    assert_eq!(res["timezone"], "UTC", "the owner's zone");
    let id = res["schedule_id"].as_str().unwrap().to_string();

    // A repeat of the call (its answer was lost) creates nothing new.
    let again = schedule(&mut w, &ta, args, &rid).await;
    assert_eq!(again["result"], *res);
    let (_, l) = hub.get("/api/schedules").await;
    assert_eq!(l["schedules"].as_array().unwrap().len(), 1, "{l}");

    assert_eq!(run_at(&hub, "2026-03-01T10:29:59Z").await, 0);
    assert_eq!(run_at(&hub, "2026-03-01T10:30:04Z").await, 1);
    let input = w.expect("session.input").await;
    assert_eq!(input["payload"]["session_id"], a.as_str());
    assert_eq!(input["payload"]["text"], "Check the build");
    // The worker lets it wait for the end of a running turn (SPEC §17.2).
    assert_eq!(input["payload"]["schedule_id"], id.as_str());

    let ev = scheduled_inputs(&hub, &a).await;
    assert_eq!(ev.len(), 1);
    let me = hub.get("/api/me").await.1;
    assert_eq!(ev[0]["author"], json!({"kind": "user", "id": me["id"]}));
    assert_eq!(ev[0]["payload"]["schedule_id"], id.as_str());
    assert_eq!(ev[0]["delivery"], "pending");
    assert_eq!(ev[0]["event_id"], input["payload"]["event_id"]);

    // Done: never again.
    assert_eq!(run_at(&hub, "2026-03-02T10:30:00Z").await, 0);
    let s = get_schedule(&hub, &a, &id).await;
    assert!(s.get("next_run").is_none(), "{s}");
    assert_eq!(s["last_run"], "2026-03-01T10:30:04Z");
    assert_eq!(scheduled_inputs(&hub, &a).await.len(), 1);
    hub.finish().await;
}

#[tokio::test]
async fn a_cron_follows_the_wall_clock_across_dst() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, _) = started(&hub, &mut w, "Daily").await;
    hub.app.clock.set(utc("2026-03-27T12:00:00Z"));
    let (s, v) = hub
        .post(
            "/api/schedules",
            json!({"session_id": a, "cron": "0 9 * * *", "timezone": "Europe/Berlin",
                   "message": "Morning report"}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    // 09:00 CET.
    assert_eq!(v["next_run"], "2026-03-28T08:00:00Z");

    assert_eq!(run_at(&hub, "2026-03-28T08:00:01Z").await, 1);
    // Berlin moves to CEST on 2026-03-29: 09:00 is 07:00 UTC from then on.
    assert_eq!(
        get_schedule(&hub, &a, &id).await["next_run"],
        "2026-03-29T07:00:00Z"
    );
    assert_eq!(run_at(&hub, "2026-03-29T06:59:59Z").await, 0);
    assert_eq!(run_at(&hub, "2026-03-29T07:00:02Z").await, 1);
    assert_eq!(
        get_schedule(&hub, &a, &id).await["next_run"],
        "2026-03-30T07:00:00Z"
    );

    let ev = scheduled_inputs(&hub, &a).await;
    assert_eq!(ev.len(), 2);
    assert!(ev.iter().all(|e| e["payload"]["text"] == "Morning report"));
    hub.finish().await;
}

#[tokio::test]
async fn missed_runs_are_caught_up_once_or_skipped() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, _) = started(&hub, &mut w, "Catch").await;
    let (b, _) = started(&hub, &mut w, "Skip").await;
    hub.app.clock.set(utc("2026-05-01T10:00:00Z"));
    let mut ids = Vec::new();
    for (sid, catch_up, when) in [
        (&a, true, json!({"cron": "0 * * * *"})),
        (&b, false, json!({"cron": "0 * * * *"})),
        (&a, true, json!({"at": "2026-05-01T12:00:00Z"})),
        (&b, false, json!({"at": "2026-05-01T12:00:00Z"})),
    ] {
        let mut body = json!({"session_id": sid, "message": format!("catch_up {catch_up}"),
                              "catch_up": catch_up});
        body.as_object_mut()
            .unwrap()
            .extend(when.as_object().unwrap().clone());
        let (s, v) = hub.post("/api/schedules", body).await;
        assert_eq!(s, 201, "{v}");
        ids.push(v["id"].as_str().unwrap().to_string());
    }

    // The hub was down from 10:30 to 15:20: five hourly runs and the `at` missed.
    assert_eq!(run_at(&hub, "2026-05-01T15:20:00Z").await, 2);
    let caught = scheduled_inputs(&hub, &a).await;
    assert_eq!(caught.len(), 2, "one per catching-up schedule: {caught:?}");
    assert!(scheduled_inputs(&hub, &b).await.is_empty());
    for (sid, id) in [(&a, &ids[0]), (&b, &ids[1])] {
        assert_eq!(
            get_schedule(&hub, sid, id).await["next_run"],
            "2026-05-01T16:00:00Z"
        );
    }
    for (sid, id) in [(&a, &ids[2]), (&b, &ids[3])] {
        assert!(get_schedule(&hub, sid, id).await.get("next_run").is_none());
    }
    assert!(
        get_schedule(&hub, &b, &ids[3])
            .await
            .get("last_run")
            .is_none(),
        "skipped"
    );

    // A run found within the grace is on time, whatever `catch_up` says.
    assert_eq!(run_at(&hub, "2026-05-01T16:01:30Z").await, 2);
    assert_eq!(scheduled_inputs(&hub, &b).await.len(), 1);
    hub.finish().await;
}

#[tokio::test]
async fn paused_deleted_and_orphaned_schedules_never_fire() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, _) = started(&hub, &mut w, "Paused").await;
    let (b, _) = started(&hub, &mut w, "Doomed").await;
    hub.app.clock.set(utc("2026-06-01T08:00:00Z"));
    let new = |sid: &str| json!({"session_id": sid, "cron": "*/30 * * * *", "message": "tick"});
    let (_, p) = hub.post("/api/schedules", new(&a)).await;
    let (_, d) = hub.post("/api/schedules", new(&a)).await;
    let (_, o) = hub.post("/api/schedules", new(&b)).await;
    let (p, d, o) = (
        p["id"].as_str().unwrap().to_string(),
        d["id"].as_str().unwrap().to_string(),
        o["id"].as_str().unwrap().to_string(),
    );

    let (s, v) = hub
        .patch(&format!("/api/schedules/{p}"), json!({"enabled": false}))
        .await;
    assert_eq!((s, &v["enabled"]), (200, &json!(false)), "{v}");
    assert_eq!(hub.delete(&format!("/api/schedules/{d}")).await, 204);
    assert_eq!(hub.delete(&format!("/api/sessions/{b}")).await, 204);
    let gone: i64 = sqlx::query_scalar("SELECT count(*) FROM scheduled_messages WHERE id = $1")
        .bind(o.parse::<Uuid>().unwrap())
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
    assert_eq!(gone, 0, "the session took its schedule with it");

    assert_eq!(run_at(&hub, "2026-06-01T08:30:00Z").await, 0);
    assert_eq!(run_at(&hub, "2026-06-01T10:00:00Z").await, 0);
    assert!(scheduled_inputs(&hub, &a).await.is_empty());

    // Resumed at 10:10: the runs missed while paused are not sent; the next is 10:30.
    hub.app.clock.set(utc("2026-06-01T10:10:00Z"));
    let (_, v) = hub
        .patch(&format!("/api/schedules/{p}"), json!({"enabled": true}))
        .await;
    assert_eq!(v["next_run"], "2026-06-01T10:30:00Z", "{v}");
    assert_eq!(run_at(&hub, "2026-06-01T10:30:00Z").await, 1);
    assert_eq!(hub.delete(&format!("/api/schedules/{d}")).await, 404);
    hub.finish().await;
}

#[tokio::test]
async fn a_target_that_takes_no_input_skips_the_run() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, _) = started(&hub, &mut w, "Archived").await;
    hub.app.clock.set(utc("2026-06-01T08:00:00Z"));
    let (_, v) = hub
        .post(
            "/api/schedules",
            json!({"session_id": a, "cron": "0 9 * * *", "message": "hello"}),
        )
        .await;
    let id = v["id"].as_str().unwrap().to_string();
    let (s, _) = hub
        .post(&format!("/api/sessions/{a}/archive"), json!({}))
        .await;
    assert_eq!(s, 200);
    assert_eq!(run_at(&hub, "2026-06-01T09:00:00Z").await, 0);
    assert!(scheduled_inputs(&hub, &a).await.is_empty());
    let s = get_schedule(&hub, &a, &id).await;
    assert_eq!(s["next_run"], "2026-06-02T09:00:00Z", "{s}");
    assert!(s.get("last_run").is_none());
    hub.finish().await;
}

#[tokio::test]
async fn refusals_and_limits() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = started(&hub, &mut w, "Here").await;
    hub.app.clock.set(utc("2026-06-01T08:00:00Z"));
    let rid = || Uuid::now_v7().to_string();

    for (args, want) in [
        (
            json!({"session": "personal/here", "message": "x"}),
            "invalid_args",
        ),
        (
            json!({"session": "personal/here", "message": "x", "cron": "0 9 * * *",
                   "at": "2026-06-02T09:00:00Z"}),
            "invalid_args",
        ),
        (
            json!({"session": "personal/here", "message": "x", "cron": "* * * * *"}),
            "invalid_args",
        ),
        (
            json!({"session": "personal/here", "message": "x", "cron": "0 0 9 * * *"}),
            "invalid_args",
        ),
        (
            json!({"session": "personal/here", "message": "x", "at": "2026-06-01T07:00:00Z"}),
            "invalid_args",
        ),
        (
            json!({"session": "personal/here", "message": "x", "at": "2026-06-02 09:00"}),
            "invalid_args",
        ),
        (
            json!({"session": "personal/here", "message": " ", "at": "2026-06-02T09:00:00Z"}),
            "invalid_args",
        ),
        (
            json!({"session": "personal/here", "message": "x", "cron": "0 9 * * *",
                   "timezone": "Mars/Olympus"}),
            "invalid_args",
        ),
        (
            json!({"session": "personal/nowhere", "message": "x", "cron": "0 9 * * *"}),
            "not_found",
        ),
        (
            json!({"session": "personal/here", "text": "x", "cron": "0 9 * * *"}),
            "invalid_args",
        ),
    ] {
        let r = schedule(&mut w, &ta, args.clone(), &rid()).await;
        assert_eq!(code(&r), want, "{args}: {r}");
    }

    // The owner's zone, set by the client.
    let (s, me) = hub
        .patch("/api/me", json!({"timezone": "Asia/Tokyo"}))
        .await;
    assert_eq!((s, &me["timezone"]), (200, &json!("Asia/Tokyo")), "{me}");
    assert_eq!(
        hub.patch("/api/me", json!({"timezone": "Nowhere"})).await.0,
        400
    );
    let r = schedule(
        &mut w,
        &ta,
        json!({"session": a, "message": "x", "cron": "0 9 * * *"}),
        &rid(),
    )
    .await;
    assert_eq!(r["result"]["timezone"], "Asia/Tokyo", "{r}");
    // 09:00 in Tokyo is 00:00 UTC.
    assert_eq!(r["result"]["next_run"], "2026-06-02T00:00:00Z");

    // At most 100 that may still run per owner.
    sqlx::query(
        "INSERT INTO scheduled_messages (id, owner_id, session_id, at, timezone, message, next_run)
         SELECT gen_random_uuid(), owner_id, id, '2027-01-01Z', 'UTC', 'x', '2027-01-01Z'
         FROM sessions, generate_series(1, 99) WHERE id = $1",
    )
    .bind(a.parse::<Uuid>().unwrap())
    .execute(&hub.app.db)
    .await
    .unwrap();
    let r = schedule(
        &mut w,
        &ta,
        json!({"session": a, "message": "x", "at": "2026-06-02T09:00:00Z"}),
        &rid(),
    )
    .await;
    assert_eq!(code(&r), "limit_exceeded", "{r}");
    let (s, v) = hub
        .post(
            "/api/schedules",
            json!({"session_id": a, "at": "2026-06-02T09:00:00Z", "message": "x"}),
        )
        .await;
    assert_eq!((s, &v["error"]), (409, &json!("schedule_limit")), "{v}");

    // Another user's schedules and sessions do not exist.
    let id: Uuid = sqlx::query_scalar("SELECT id FROM scheduled_messages LIMIT 1")
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO users (id, email, display_name) VALUES ($1, 'x@example.com', 'X')")
        .bind(Uuid::now_v7())
        .execute(&hub.app.db)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE scheduled_messages SET owner_id = (SELECT id FROM users WHERE email = 'x@example.com')
         WHERE id = $1",
    )
    .bind(id)
    .execute(&hub.app.db)
    .await
    .unwrap();
    assert_eq!(hub.delete(&format!("/api/schedules/{id}")).await, 404);
    assert_eq!(
        hub.patch(&format!("/api/schedules/{id}"), json!({"enabled": false}))
            .await
            .0,
        404
    );
    let (_, l) = hub.get("/api/schedules").await;
    assert!(l["schedules"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["id"] != id.to_string()));
    let (s, _) = hub
        .post(
            "/api/schedules",
            json!({"session_id": Uuid::now_v7(), "at": "2026-06-02T09:00:00Z", "message": "x"}),
        )
        .await;
    assert_eq!(s, 404);
    hub.finish().await;
}

/// SPEC §17.2: a run of a schedule never joins a running turn; a worker that acks it `joined`
/// all the same is not believed, and the run's turn opens.
#[tokio::test]
async fn a_scheduled_run_acked_as_joined_opens_its_turn() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = started(&hub, &mut w, "Reminders").await;
    hub.app.clock.set(utc("2026-03-01T10:00:00Z"));
    let args = json!({"session": "personal/reminders", "message": "Check the build",
                      "at": "2026-03-01T10:30:00Z"});
    schedule(&mut w, &ta, args, &Uuid::now_v7().to_string()).await;
    assert_eq!(run_at(&hub, "2026-03-01T10:30:00Z").await, 1);
    let input = w.expect("session.input").await;
    let event_id = input["payload"]["event_id"].clone();
    w.reply(
        &input["id"],
        "ack",
        json!({"event_id": event_id, "joined": true}),
    )
    .await;
    hub.wait_session(&a, "running", |v| v["state"] == "running")
        .await;
    hub.finish().await;
}
