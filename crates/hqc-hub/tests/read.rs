//! SPEC §7: `hqc_read`, one session of the caller's owner read from the hub in pages.

mod common;

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
        let (_, me) = hub.get("/api/me").await;
        let owner = me["id"].as_str().unwrap().parse().unwrap();
        let project = me["personal_project_id"].as_str().unwrap().parse().unwrap();
        Rig {
            hub,
            w,
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
    let other = Uuid::now_v7();
    let other_project = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, email, display_name) VALUES ($1, $2, 'other')")
        .bind(other)
        .bind(format!("other-{other}@example.com"))
        .execute(&db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO projects (id, owner_id, name, slug) VALUES ($1, $2, 'Personal', 'personal')",
    )
    .bind(other_project)
    .bind(other)
    .execute(&db)
    .await
    .unwrap();
    let foreign = session(&db, other, other_project, "notes", "Theirs").await;
    fill(&db, foreign, 2).await;
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
