//! SPEC §7: `hqc_read`, one session of the caller's owner read from the hub in pages.

mod common;

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use common::*;
use hqc_hub::db::{self, Inserted, NewEvent};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

const WS_PATH: &str = "/Users/home-mac/work/hqc";
/// The budget of one answer, in bytes (`hqc_proto::mcp::MAX_ARGS`).
const BUDGET: usize = 64 * 1024;

fn t(seconds: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-20T10:00:00Z")
        .unwrap()
        .to_utc()
        + TimeDelta::seconds(seconds)
}

/// A session row straight in the database.
async fn session(db: &PgPool, owner: Uuid, project: Uuid, slug: &str, title: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO sessions (id, project_id, owner_id, title, slug, created_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(project)
    .bind(owner)
    .bind(title)
    .bind(slug)
    .bind(t(0))
    .execute(db)
    .await
    .unwrap();
    id
}

async fn event(db: &PgPool, session: Uuid, text: &str, n: i64) -> i64 {
    let e = NewEvent {
        event_id: Uuid::now_v7(),
        session_id: session,
        ts: t(n),
        author: json!({"kind": "system"}),
        kind: "agent.message".into(),
        payload: json!({"text": text, "final": true}),
        chain: None,
        client_op_id: None,
        delivery: None,
        delivered_at: None,
    };
    match db::insert_event(db, &e).await.unwrap() {
        Inserted::New(r) => r.seq,
        other => panic!("{other:?}"),
    }
}

/// `n` events with the text `m<i>`; the seqs run from 1.
async fn fill(db: &PgPool, session: Uuid, n: i64) {
    for i in 1..=n {
        event(db, session, &format!("m{i}"), i).await;
    }
}

struct Rig {
    hub: TestHub,
    w: FakeWorker,
    sid: String,
    token: String,
    owner: Uuid,
    project: Uuid,
}

impl Rig {
    async fn start() -> Rig {
        let hub = TestHub::start().await;
        let (_, wt) = hub.enroll("one").await;
        let (mut w, _) = FakeWorker::online(&hub, &wt, WS_PATH).await;
        let sid = hub.create_session("Reader").await;
        let f = ack_start(&mut w, &sid).await;
        let token = f["payload"]["session_token"].as_str().unwrap().to_string();
        // As the worker's report of a running agent leaves it.
        sqlx::query("UPDATE sessions SET process_state = 'running' WHERE id = $1")
            .bind(sid.parse::<Uuid>().unwrap())
            .execute(&hub.app.db)
            .await
            .unwrap();
        let (_, me) = hub.get("/api/me").await;
        let owner = me["id"].as_str().unwrap().parse().unwrap();
        let project = me["personal_project_id"].as_str().unwrap().parse().unwrap();
        Rig {
            hub,
            w,
            sid,
            token,
            owner,
            project,
        }
    }

    async fn read(&mut self, args: Value) -> Value {
        let rid = Uuid::now_v7().to_string();
        let id = self
            .w
            .send(
                "mcp.call",
                json!({"request_id": rid, "session_token": self.token,
                       "tool": "hqc_read", "args": args}),
            )
            .await;
        let r = self.w.expect("mcp.result").await;
        assert_eq!(r["id"], id.as_str(), "{r}");
        r["payload"].clone()
    }

    /// `hqc_read` of a subagent: the worker is asked, answers `answer` (a `result` payload
    /// without its `request_id`) and the call's result comes after. Returns the payload of the
    /// frame the worker got and the call's result.
    async fn read_subagent(&mut self, args: Value, answer: Value) -> (Value, Value) {
        let rid = Uuid::now_v7().to_string();
        let id = self
            .w
            .send(
                "mcp.call",
                json!({"request_id": rid, "session_token": self.token,
                       "tool": "hqc_read", "args": args}),
            )
            .await;
        let f = self.w.expect("session.subagent_history").await;
        let mut body = answer;
        body["request_id"] = f["id"].clone();
        self.w.reply(&f["id"], "result", body).await;
        let r = self.w.expect("mcp.result").await;
        assert_eq!(r["id"], id.as_str(), "{r}");
        (f["payload"].clone(), r["payload"].clone())
    }
}

const TASK: &str = "a52af2b436ded4cea";

/// A session of another owner, with two events.
async fn foreign_session(db: &PgPool) -> Uuid {
    let other = Uuid::now_v7();
    let other_project = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, email, display_name) VALUES ($1, $2, 'other')")
        .bind(other)
        .bind(format!("other-{other}@example.com"))
        .execute(db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO projects (id, owner_id, name, slug) VALUES ($1, $2, 'Personal', 'personal')",
    )
    .bind(other_project)
    .bind(other)
    .execute(db)
    .await
    .unwrap();
    let foreign = session(db, other, other_project, "notes", "Theirs").await;
    fill(db, foreign, 2).await;
    foreign
}

/// `n` `agent.message` events of a subagent of the session `sid`, texts `s<i>`.
fn sub_events(sid: &str, n: usize) -> Vec<Value> {
    (1..=n)
        .map(|i| {
            agent_event(
                sid,
                "agent.message",
                json!({"text": format!("s{i}"), "final": false}),
            )
        })
        .collect()
}

fn texts(r: &Value) -> Vec<String> {
    r["result"]["events"]
        .as_array()
        .unwrap_or_else(|| panic!("{r}"))
        .iter()
        .map(|e| e["payload"]["text"].as_str().unwrap().to_string())
        .collect()
}

/// The `seq`s of an answer's events.
fn seqs(r: &Value) -> Vec<i64> {
    r["result"]["events"]
        .as_array()
        .unwrap_or_else(|| panic!("{r}"))
        .iter()
        .map(|e| e["seq"].as_i64().unwrap())
        .collect()
}

#[tokio::test]
async fn the_last_events_and_the_pages_around_them() {
    let mut rig = Rig::start().await;
    let db = rig.hub.app.db.clone();
    let s = session(&db, rig.owner, rig.project, "notes", "Notes").await;
    fill(&db, s, 250).await;

    let r = rig.read(json!({"session": "personal/notes"})).await;
    let res = &r["result"];
    assert_eq!(seqs(&r), (151..=250).collect::<Vec<_>>(), "{r}");
    assert_eq!(res["address"], "personal/notes");
    assert_eq!(res["title"], "Notes");
    assert_eq!(res["last_seq"], 250);
    assert_eq!(res["earlier"], 151);
    assert!(res.get("later").is_none(), "{r}");
    // The events are the client's: the text is in the payload.
    assert_eq!(res["events"][99]["payload"]["text"], "m250");

    // An id works as an address does.
    let r = rig.read(json!({"session": s.to_string()})).await;
    assert_eq!(seqs(&r).len(), 100, "{r}");

    let r = rig
        .read(json!({"session": "personal/notes", "before_seq": 151}))
        .await;
    assert_eq!(seqs(&r), (51..=150).collect::<Vec<_>>(), "{r}");
    assert_eq!(r["result"]["earlier"], 51);
    assert_eq!(r["result"]["later"], 150);

    let r = rig
        .read(json!({"session": "personal/notes", "before_seq": 11}))
        .await;
    assert_eq!(seqs(&r), (1..=10).collect::<Vec<_>>(), "{r}");
    assert!(r["result"].get("earlier").is_none(), "{r}");
    assert_eq!(r["result"]["later"], 10);

    let r = rig
        .read(json!({"session": "personal/notes", "after_seq": 0}))
        .await;
    assert_eq!(seqs(&r), (1..=100).collect::<Vec<_>>(), "{r}");
    assert!(r["result"].get("earlier").is_none(), "{r}");
    assert_eq!(r["result"]["later"], 100);

    let r = rig
        .read(json!({"session": "personal/notes", "after_seq": 240}))
        .await;
    assert_eq!(seqs(&r), (241..=250).collect::<Vec<_>>(), "{r}");
    assert_eq!(r["result"]["earlier"], 241);
    assert!(r["result"].get("later").is_none(), "{r}");

    // Reading wrote nothing.
    let n: i64 = sqlx::query_scalar("SELECT last_seq FROM sessions WHERE id = $1")
        .bind(s)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(n, 250);
    rig.hub.finish().await;
}

#[tokio::test]
async fn an_archived_session_and_the_caller_itself_are_read() {
    let mut rig = Rig::start().await;
    let db = rig.hub.app.db.clone();
    let s = session(&db, rig.owner, rig.project, "old", "Old").await;
    fill(&db, s, 3).await;
    sqlx::query("UPDATE sessions SET archived = true WHERE id = $1")
        .bind(s)
        .execute(&db)
        .await
        .unwrap();
    let r = rig.read(json!({"session": "personal/old"})).await;
    assert_eq!(seqs(&r), vec![1, 2, 3], "{r}");
    let r = rig.read(json!({"session": "personal/reader"})).await;
    assert_eq!(r["result"]["address"], "personal/reader", "{r}");
    rig.hub.finish().await;
}

#[tokio::test]
async fn what_is_not_the_owners_is_not_found_and_bad_arguments_are_refused() {
    let mut rig = Rig::start().await;
    let db = rig.hub.app.db.clone();
    let s = session(&db, rig.owner, rig.project, "notes", "Notes").await;
    fill(&db, s, 2).await;
    let foreign = foreign_session(&db).await;
    let sys = session(&db, rig.owner, rig.project, "home-mac", "home-mac").await;
    sqlx::query("UPDATE sessions SET system = true WHERE id = $1")
        .bind(sys)
        .execute(&db)
        .await
        .unwrap();

    for session in [
        foreign.to_string(),
        sys.to_string(),
        "personal/home-mac".into(),
        "personal/none".into(),
        Uuid::now_v7().to_string(),
        "nope".into(),
    ] {
        let r = rig.read(json!({ "session": session })).await;
        assert_eq!(r["error"]["code"], "not_found", "{session}: {r}");
    }
    for args in [
        json!({"session": "personal/notes", "before_seq": 2, "after_seq": 1}),
        json!({"session": "personal/notes", "limit": 5}),
        json!({"before_seq": 2}),
    ] {
        let r = rig.read(args.clone()).await;
        assert_eq!(r["error"]["code"], "invalid_args", "{args}: {r}");
    }
    rig.hub.finish().await;
}

#[tokio::test]
async fn a_page_over_the_byte_budget_loses_its_far_end_but_keeps_one_event() {
    let mut rig = Rig::start().await;
    let db = rig.hub.app.db.clone();
    let s = session(&db, rig.owner, rig.project, "big", "Big").await;
    let text = "x".repeat(20_000);
    for i in 1..=6 {
        event(&db, s, &text, i).await;
    }

    let r = rig.read(json!({"session": "personal/big"})).await;
    let size = serde_json::to_string(&r["result"]["events"]).unwrap().len();
    assert!(size <= BUDGET, "{size}");
    // The oldest go; the newest stay.
    assert_eq!(seqs(&r), vec![4, 5, 6], "{r}");
    assert_eq!(r["result"]["earlier"], 4);
    assert!(r["result"].get("later").is_none(), "{r}");

    let r = rig
        .read(json!({"session": "personal/big", "after_seq": 0}))
        .await;
    assert_eq!(seqs(&r), vec![1, 2, 3], "{r}");
    assert_eq!(r["result"]["later"], 3);
    assert!(r["result"].get("earlier").is_none(), "{r}");

    // Paging on from `earlier` reaches the rest.
    let r = rig
        .read(json!({"session": "personal/big", "before_seq": 4}))
        .await;
    assert_eq!(seqs(&r), vec![1, 2, 3], "{r}");

    // One event larger than the budget is still returned.
    let huge = session(&db, rig.owner, rig.project, "huge", "Huge").await;
    event(&db, huge, "tiny", 1).await;
    event(&db, huge, &"y".repeat(BUDGET * 2), 2).await;
    let r = rig.read(json!({"session": "personal/huge"})).await;
    assert_eq!(seqs(&r), vec![2], "{r}");
    assert_eq!(r["result"]["earlier"], 2);
    let r = rig
        .read(json!({"session": "personal/huge", "after_seq": 1}))
        .await;
    assert_eq!(seqs(&r), vec![2], "{r}");
    rig.hub.finish().await;
}

#[tokio::test]
async fn a_subagent_of_the_caller_is_read_by_events_without_its_thinking_unless_asked() {
    let mut rig = Rig::start().await;
    let sid = rig.sid.clone();
    let mut evs = sub_events(&sid, 3);
    let thought = agent_event(&sid, "agent.thinking", json!({"text": "hmm"}));
    evs.insert(2, thought);

    // No `session`: the caller's own; the frame carries no cursor.
    let (frame, r) = rig
        .read_subagent(
            json!({"subagent": TASK}),
            json!({"events": evs, "truncated": false}),
        )
        .await;
    assert_eq!(
        frame,
        json!({"request_id": frame["request_id"], "session_id": sid, "task_id": TASK}),
    );
    assert_eq!(texts(&r), ["s1", "s2", "s3"], "{r}");
    assert_eq!(r["result"]["address"], "personal/reader");
    assert_eq!(r["result"]["subagent"], TASK);
    for k in ["earlier", "gap"] {
        assert!(r["result"].get(k).is_none(), "{k}: {r}");
    }
    // The page's newest event is the cursor to follow the subagent from.
    assert_eq!(r["result"]["later"], evs[3]["event_id"], "{r}");

    // The thinking comes when asked for, in its place.
    let (_, r) = rig
        .read_subagent(
            json!({"subagent": TASK, "session": "personal/reader", "thinking": true}),
            json!({"events": evs, "truncated": false}),
        )
        .await;
    let types: Vec<_> = r["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "agent.message",
            "agent.message",
            "agent.thinking",
            "agent.message"
        ],
        "{r}"
    );
    rig.hub.finish().await;
}

#[tokio::test]
async fn a_subagent_is_paged_back_from_its_newest_events_and_forward_from_a_cursor() {
    let mut rig = Rig::start().await;
    let sid = rig.sid.clone();
    let evs = sub_events(&sid, 150);
    let id = |i: usize| evs[i]["event_id"].clone();

    // The worker's 150 events: the newest 100, with the cursor to the 50 before.
    let (_, r) = rig
        .read_subagent(
            json!({"subagent": TASK}),
            json!({"events": evs, "truncated": false}),
        )
        .await;
    let want: Vec<_> = (51..=150).map(|i| format!("s{i}")).collect();
    assert_eq!(texts(&r), want, "{r}");
    assert_eq!(r["result"]["earlier"], id(50), "{r}");
    assert_eq!(r["result"]["later"], id(149), "{r}");

    // `earlier` goes to the worker as `before`; what is before it comes back, without `later`:
    // the newer ones were read already.
    let (frame, r) = rig
        .read_subagent(
            json!({"subagent": TASK, "before_event": id(50)}),
            json!({"events": evs[..50], "truncated": true}),
        )
        .await;
    assert_eq!(frame["before"], id(50), "{frame}");
    assert!(frame.get("after").is_none(), "{frame}");
    assert_eq!(texts(&r).len(), 50, "{r}");
    assert_eq!(r["result"]["earlier"], id(0), "{r}");
    assert!(r["result"].get("later").is_none(), "{r}");

    // After an event the worker holds: the events after it, `earlier` their first (reading
    // before it gives the cursor's event and older ones); `gap` only when the worker does not
    // hold it.
    let (frame, r) = rig
        .read_subagent(
            json!({"subagent": TASK, "after_event": id(147)}),
            json!({"events": evs[148..], "truncated": false, "continues": true}),
        )
        .await;
    assert_eq!(frame["after"], id(147), "{frame}");
    assert_eq!(texts(&r), ["s149", "s150"], "{r}");
    assert_eq!(r["result"]["earlier"], id(148), "{r}");
    assert_eq!(r["result"]["later"], id(149), "{r}");
    assert!(r["result"].get("gap").is_none(), "{r}");

    // Nothing new after the cursor: it comes back, for polling on.
    let (_, r) = rig
        .read_subagent(
            json!({"subagent": TASK, "after_event": id(149)}),
            json!({"events": [], "truncated": false, "continues": true}),
        )
        .await;
    assert_eq!(texts(&r), Vec::<String>::new(), "{r}");
    assert_eq!(r["result"]["later"], id(149), "{r}");
    assert!(r["result"].get("earlier").is_none(), "{r}");

    let (_, r) = rig
        .read_subagent(
            json!({"subagent": TASK, "after_event": Uuid::new_v4()}),
            json!({"events": evs[50..], "truncated": true}),
        )
        .await;
    assert_eq!(r["result"]["gap"], true, "{r}");
    assert_eq!(texts(&r).len(), 100, "{r}");
    assert_eq!(r["result"]["earlier"], id(50), "{r}");
    assert_eq!(r["result"]["later"], id(149), "{r}");

    // A page from the cursor on stops at 100 events: `later` is the 100th, the rest is read
    // from it.
    let (_, r) = rig
        .read_subagent(
            json!({"subagent": TASK, "after_event": Uuid::new_v4()}),
            json!({"events": evs, "truncated": false, "continues": true}),
        )
        .await;
    let want: Vec<_> = (1..=100).map(|i| format!("s{i}")).collect();
    assert_eq!(texts(&r), want, "{r}");
    assert_eq!(r["result"]["earlier"], id(0), "{r}");
    assert_eq!(r["result"]["later"], id(99), "{r}");
    rig.hub.finish().await;
}

#[tokio::test]
async fn skipped_thinking_costs_a_page_nothing() {
    let mut rig = Rig::start().await;
    let sid = rig.sid.clone();
    // 101 messages; after each of the newest 50 a thought.
    let mut evs = Vec::new();
    for (i, m) in sub_events(&sid, 101).into_iter().enumerate() {
        evs.push(m);
        if i >= 51 {
            evs.push(agent_event(&sid, "agent.thinking", json!({"text": "hmm"})));
        }
    }
    let (_, r) = rig
        .read_subagent(
            json!({"subagent": TASK}),
            json!({"events": evs, "truncated": false}),
        )
        .await;
    let want: Vec<_> = (2..=101).map(|i| format!("s{i}")).collect();
    assert_eq!(texts(&r), want, "{r}");
    // The oldest message left out is the first event; `earlier` is the one after it.
    assert_eq!(r["result"]["earlier"], evs[1]["event_id"], "{r}");
    rig.hub.finish().await;
}

#[tokio::test]
async fn a_subagent_page_over_the_byte_budget_loses_its_far_end() {
    let mut rig = Rig::start().await;
    let sid = rig.sid.clone();
    let big = "x".repeat(20_000);
    let evs: Vec<Value> = (0..6)
        .map(|_| agent_event(&sid, "agent.message", json!({"text": big, "final": false})))
        .collect();
    let (_, r) = rig
        .read_subagent(
            json!({"subagent": TASK}),
            json!({"events": evs, "truncated": false}),
        )
        .await;
    assert_eq!(texts(&r).len(), 3, "{r}");
    assert_eq!(r["result"]["earlier"], evs[3]["event_id"], "{r}");
    rig.hub.finish().await;
}

#[tokio::test]
async fn a_subagent_read_that_cannot_be_asked_is_refused() {
    let mut rig = Rig::start().await;
    let ev = Uuid::new_v4();
    for args in [
        json!({"subagent": TASK, "before_seq": 1}),
        json!({"subagent": TASK, "after_seq": 1}),
        json!({"session": "personal/reader", "before_event": ev}),
        json!({"session": "personal/reader", "after_event": ev}),
        json!({"session": "personal/reader", "thinking": true}),
        json!({"subagent": TASK, "before_event": ev, "after_event": ev}),
        json!({"subagent": "../x"}),
        json!({"subagent": "a".repeat(65)}),
        json!({"subagent": TASK, "before_event": "nope"}),
        json!({"thinking": true}),
    ] {
        let r = rig.read(args.clone()).await;
        assert_eq!(r["error"]["code"], "invalid_args", "{args}: {r}");
    }
    // Nothing of that reached the worker.
    rig.w.expect_silence(Duration::from_millis(200)).await;

    // A session without a worker, and one the worker does not run now, cannot be asked.
    let s = session(&rig.hub.app.db, rig.owner, rig.project, "idle", "Idle").await;
    let r = rig
        .read(json!({"subagent": TASK, "session": s.to_string()}))
        .await;
    assert_eq!(r["error"]["code"], "unavailable", "{r}");
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no worker"),
        "{r}"
    );
    let rid = Uuid::now_v7().to_string();
    rig.w
        .send(
            "mcp.call",
            json!({"request_id": rid, "session_token": rig.token, "tool": "hqc_read",
                   "args": {"subagent": TASK}}),
        )
        .await;
    let f = rig.w.expect("session.subagent_history").await;
    rig.w
        .reply(
            &f["id"],
            "error",
            json!({"code": "unknown_session", "message": "session is not loaded", "retryable": false}),
        )
        .await;
    let r = rig.w.expect("mcp.result").await["payload"].clone();
    assert_eq!(r["error"]["code"], "unavailable", "{r}");
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("is not running now"),
        "{r}"
    );

    // An idle-stopped session is still in the worker's list, but runs nothing to read.
    let stopped = session(&rig.hub.app.db, rig.owner, rig.project, "asleep", "Asleep").await;
    sqlx::query(
        "UPDATE sessions SET process_state = 'stopped',
             worker_id = (SELECT worker_id FROM sessions WHERE id = $2) WHERE id = $1",
    )
    .bind(stopped)
    .bind(rig.sid.parse::<Uuid>().unwrap())
    .execute(&rig.hub.app.db)
    .await
    .unwrap();
    let r = rig
        .read(json!({"subagent": TASK, "session": "personal/asleep"}))
        .await;
    assert_eq!(r["error"]["code"], "unavailable", "{r}");
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("is not running now"),
        "{r}"
    );
    rig.w.expect_silence(Duration::from_millis(200)).await;

    // A session that does not exist, and one of another owner, are not found.
    let foreign = foreign_session(&rig.hub.app.db).await;
    for session in ["personal/none".to_string(), foreign.to_string()] {
        let r = rig
            .read(json!({"subagent": TASK, "session": session}))
            .await;
        assert_eq!(r["error"]["code"], "not_found", "{session}: {r}");
    }
    rig.hub.finish().await;
}
