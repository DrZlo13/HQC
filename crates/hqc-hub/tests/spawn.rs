//! T34 (SPEC §7, §13.4, §20): `hqc_spawn` from a session of a fake worker: the new session
//! and its first turn, the spawner on the session, a repeated call, the project and the
//! workspace of the owner only, a missing workspace, the chain and the spawn limits.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const WS_PATH: &str = "/Users/home-mac/work/hqc";
const OTHER_PATH: &str = "/Users/home-mac/work/other";
const MODEL: &str = "claude-haiku-5-5";

/// A started session in the personal project's default workspace.
async fn started(hub: &TestHub, w: &mut FakeWorker, title: &str) -> (String, String) {
    let pid = hub.personal_project().await;
    let (s, v) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": title, "model": MODEL}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    let f = ack_start(w, &sid).await;
    (sid, f["payload"]["session_token"].as_str().unwrap().into())
}

/// `hqc_spawn` with `request_id` `rid`: its `mcp.result` payload and the other frames that
/// came before it (the new session's `session.start`).
async fn spawn_with(
    w: &mut FakeWorker,
    token: &str,
    args: Value,
    rid: &str,
) -> (Value, Vec<Value>) {
    let id = w
        .send(
            "mcp.call",
            json!({"request_id": rid, "session_token": token, "tool": "hqc_spawn", "args": args}),
        )
        .await;
    let mut other = Vec::new();
    loop {
        let f = w.recv().await;
        if f["type"] == "mcp.result" {
            assert_eq!(f["id"], id.as_str(), "{f}");
            assert_eq!(f["payload"]["request_id"], rid);
            return (f["payload"].clone(), other);
        }
        other.push(f);
    }
}

async fn spawn(w: &mut FakeWorker, token: &str, args: Value) -> (Value, Vec<Value>) {
    spawn_with(w, token, args, &Uuid::now_v7().to_string()).await
}

fn code(r: &Value) -> &str {
    assert!(r.get("result").is_none(), "{r}");
    r["error"]["code"].as_str().unwrap()
}

fn address(r: &Value) -> &str {
    assert!(r.get("error").is_none(), "{r}");
    r["result"]["address"].as_str().unwrap()
}

/// The id of the session at `personal/<slug>`.
async fn session_at(hub: &TestHub, slug: &str) -> String {
    let id: Uuid = sqlx::query_scalar("SELECT id FROM sessions WHERE slug = $1")
        .bind(slug)
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
    id.to_string()
}

async fn sessions(hub: &TestHub) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM sessions WHERE NOT system")
        .fetch_one(&hub.app.db)
        .await
        .unwrap()
}

/// A human's input to `sid`, delivered: the session's turn is a human's.
async fn human_turn(hub: &TestHub, w: &mut FakeWorker, sid: &str) {
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "spawn some helpers"}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let input = w.expect("session.input").await;
    w.reply(
        &input["id"],
        "ack",
        json!({"event_id": input["payload"]["event_id"]}),
    )
    .await;
    hub.wait_session(sid, "running", |v| v["state"] == "running")
        .await;
}

#[tokio::test]
async fn a_spawned_session_runs_its_first_turn_on_the_prompt() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = started(&hub, &mut w, "Spawner").await;

    let rid = Uuid::now_v7().to_string();
    let (r, before) = spawn_with(
        &mut w,
        &ta,
        json!({"project": "personal", "prompt": "Count the files and tell me."}),
        &rid,
    )
    .await;
    // "Session N": the spawner is the first one.
    assert_eq!(address(&r), "personal/session-2", "{r}");
    let b = session_at(&hub, "session-2").await;

    // The new session starts on the workspace's worker; its first input is the prompt.
    let mut start = before.into_iter().find(|f| f["type"] == "session.start");
    if start.is_none() {
        start = Some(w.expect("session.start").await);
    }
    let start = start.unwrap();
    assert_eq!(start["payload"]["session_id"], b.as_str());
    assert_eq!(start["payload"]["mode"], "new");
    assert_eq!(start["payload"]["workspace_path"], WS_PATH);
    assert_eq!(start["payload"]["model"], MODEL, "the spawner's model");
    w.reply(&start["id"], "ack", json!({})).await;
    let input = w.expect("session.input").await;
    let p = &input["payload"];
    assert_eq!(p["session_id"], b.as_str());
    assert_eq!(p["text"], "Count the files and tell me.");
    assert_eq!(
        p["peer"],
        json!({"from": "personal/spawner", "mode": "send"})
    );

    let ev = hub
        .events(&b)
        .await
        .into_iter()
        .find(|e| e["type"] == "peer.message")
        .unwrap();
    assert_eq!(ev["event_id"], p["event_id"]);
    assert_eq!(ev["author"], json!({"kind": "agent", "id": a}));
    assert_eq!(ev["payload"]["from"], "personal/spawner");
    assert_eq!(ev["payload"]["mode"], "send");
    assert_eq!(ev["delivery"], "pending");
    assert_eq!(
        (&ev["chain"]["depth"], &ev["chain"]["hops"]),
        (&json!(0), &json!(1))
    );

    // The session records who started it.
    let view = hub.session(&b).await;
    assert_eq!(
        view["spawned_by"],
        json!({"session_id": a, "address": "personal/spawner"})
    );
    assert_eq!(view["owner_id"], hub.session(&a).await["owner_id"]);
    assert!(hub.session(&a).await.get("spawned_by").is_none());

    // The first turn starts with the input's ack.
    w.reply(&input["id"], "ack", json!({"event_id": p["event_id"]}))
        .await;
    hub.wait_session(&b, "the first turn", |v| v["state"] == "running")
        .await;

    // A repeat of the call: the same answer, nothing created or delivered twice.
    let n = sessions(&hub).await;
    let (again, _) = spawn_with(
        &mut w,
        &ta,
        json!({"project": "personal", "prompt": "Count the files and tell me."}),
        &rid,
    )
    .await;
    assert_eq!(again, r);
    assert_eq!(sessions(&hub).await, n);
    let messages = hub
        .events(&b)
        .await
        .into_iter()
        .filter(|e| e["type"] == "peer.message")
        .count();
    assert_eq!(messages, 1);
    w.expect_silence(Duration::from_millis(200)).await;

    // The new agent answers the spawner with hqc_send.
    let tb = start["payload"]["session_token"].as_str().unwrap();
    let id = w
        .send(
            "mcp.call",
            json!({"request_id": "b-1", "session_token": tb, "tool": "hqc_send",
                   "args": {"session": "personal/spawner", "text": "42 files"}}),
        )
        .await;
    let res = w.expect("mcp.result").await;
    assert_eq!(res["id"], id.as_str());
    assert_eq!(
        res["payload"]["result"],
        json!({"to": "personal/spawner", "delivery": "buffered"})
    );
    hub.finish().await;
}

#[tokio::test]
async fn spawn_takes_the_owners_project_and_workspace_only() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (_, ta) = started(&hub, &mut w, "Spawner").await;
    let db = &hub.app.db;

    // Another user's project, an unknown one.
    let stranger = hqc_hub::db::ensure_user(db, "stranger@example.com")
        .await
        .unwrap();
    let theirs = hqc_hub::db::create_project(db, stranger.id, "Theirs", "")
        .await
        .unwrap();
    for p in [theirs.slug.clone(), theirs.id.to_string(), "nope".into()] {
        let (r, _) = spawn(&mut w, &ta, json!({"project": p, "prompt": "hi"})).await;
        assert_eq!(code(&r), "not_found", "{p}: {r}");
    }

    // A project of the owner without a workspace: `workspace` is required, and an unknown
    // one is not found.
    let (s, p) = hub.post("/api/projects", json!({"name": "Bare"})).await;
    assert_eq!(s, 201, "{p}");
    let bare = p["id"].as_str().unwrap().to_string();
    let (r, _) = spawn(&mut w, &ta, json!({"project": "bare", "prompt": "hi"})).await;
    assert_eq!(code(&r), "invalid_args", "{r}");
    assert!(r["error"]["message"]
        .as_str()
        .unwrap()
        .contains("workspace"));
    let (r, _) = spawn(
        &mut w,
        &ta,
        json!({"project": "bare", "workspace": WS_PATH, "prompt": "hi"}),
    )
    .await;
    assert_eq!(code(&r), "not_found", "a workspace of another project: {r}");

    // A workspace of that project on another worker of the owner, by label, path and id;
    // the session starts there.
    let (_, t2) = hub.enroll("two").await;
    let (mut w2, _) = FakeWorker::online_bare(&hub, &t2, OTHER_PATH, hub.boot_id).await;
    let (s, ws) = hub.add_workspace(&mut w2, &bare, OTHER_PATH).await;
    assert_eq!(s, 201, "{ws}");
    let ws_id = ws["id"].as_str().unwrap().to_string();
    let label = ws["label"].as_str().unwrap().to_string();
    for (i, which) in [label, OTHER_PATH.to_string(), ws_id.clone()]
        .into_iter()
        .enumerate()
    {
        let (r, _) = spawn(
            &mut w,
            &ta,
            json!({"project": bare, "workspace": which, "prompt": "hi", "model": "claude-sonnet-5-5"}),
        )
        .await;
        assert_eq!(
            address(&r),
            format!("bare/session-{}", i + 1),
            "{which}: {r}"
        );
    }
    let first = session_at(&hub, "session-1").await;
    let s = hub.session(&first).await;
    assert_eq!(s["workspace_id"], ws_id.as_str());
    let start = w2.expect("session.start").await;
    assert_eq!(start["payload"]["session_id"], first.as_str());
    assert_eq!(start["payload"]["workspace_path"], OTHER_PATH);
    assert_eq!(s["agent"]["model"], "claude-sonnet-5-5");

    // Arguments.
    for bad in [
        json!({"project": "personal", "prompt": "  "}),
        json!({"project": "personal", "prompt": "hi", "runtime": "acp"}),
        json!({"project": "personal", "prompt": "hi", "model": "--danger"}),
        json!({"project": "personal", "prompt": "hi", "cwd": "x"}),
        json!({"project": "personal"}),
    ] {
        let (r, _) = spawn(&mut w, &ta, bad.clone()).await;
        assert_eq!(code(&r), "invalid_args", "{bad}: {r}");
    }
    let (r, _) = spawn(
        &mut w,
        &ta,
        json!({"project": "personal", "prompt": "hi", "runtime": "claude_code"}),
    )
    .await;
    assert_eq!(address(&r), "personal/session-2", "{r}");
    hub.finish().await;
}

#[tokio::test]
async fn spawns_are_limited_per_chain_and_per_hour() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = started(&hub, &mut w, "Spawner").await;
    let per_chain = hqc_hub::spawn::MAX_PER_CHAIN;
    let per_hour = hqc_hub::spawn::MAX_PER_HOUR;

    // One human turn is one chain: its spawns count together.
    human_turn(&hub, &mut w, &a).await;
    let mut first = Vec::new();
    for i in 0..per_chain {
        let (r, other) = spawn(&mut w, &ta, json!({"project": "personal", "prompt": "go"})).await;
        address(&r);
        if i == 0 {
            first = other;
        }
    }
    let (r, _) = spawn(&mut w, &ta, json!({"project": "personal", "prompt": "go"})).await;
    assert_eq!(code(&r), "limit_exceeded", "{r}");
    let chains: Vec<Uuid> =
        sqlx::query_scalar("SELECT DISTINCT spawn_chain_id FROM sessions WHERE spawned_by = $1")
            .bind(a.parse::<Uuid>().unwrap())
            .fetch_all(&hub.app.db)
            .await
            .unwrap();
    assert_eq!(chains.len(), 1);

    // A spawned agent's spawns continue the chain (hops + 1), so they count there too.
    let b = session_at(&hub, "session-2").await;
    let mut start = first
        .into_iter()
        .find(|f| f["type"] == "session.start" && f["payload"]["session_id"] == b.as_str());
    while start.is_none() {
        let f = w.recv().await;
        if f["type"] == "session.start" && f["payload"]["session_id"] == b.as_str() {
            start = Some(f);
        }
    }
    let start = start.unwrap();
    let tb = start["payload"]["session_token"]
        .as_str()
        .unwrap()
        .to_string();
    w.reply(&start["id"], "ack", json!({})).await;
    let input = loop {
        let f = w.recv().await;
        if f["type"] == "session.input" && f["payload"]["session_id"] == b.as_str() {
            break f;
        }
    };
    w.reply(
        &input["id"],
        "ack",
        json!({"event_id": input["payload"]["event_id"]}),
    )
    .await;
    hub.wait_session(&b, "B's turn", |v| v["state"] == "running")
        .await;
    let (r, _) = spawn(&mut w, &tb, json!({"project": "personal", "prompt": "go"})).await;
    assert_eq!(code(&r), "limit_exceeded", "{r}");

    // Per owner and hour, whatever the chains.
    sqlx::query(
        "UPDATE sessions SET spawn_chain_id = gen_random_uuid() WHERE spawn_chain_id IS NOT NULL",
    )
    .execute(&hub.app.db)
    .await
    .unwrap();
    for _ in per_chain..per_hour {
        let (r, _) = spawn(&mut w, &tb, json!({"project": "personal", "prompt": "go"})).await;
        address(&r);
        sqlx::query("UPDATE sessions SET spawn_chain_id = gen_random_uuid() WHERE spawn_chain_id IS NOT NULL")
            .execute(&hub.app.db)
            .await
            .unwrap();
    }
    let (r, _) = spawn(&mut w, &ta, json!({"project": "personal", "prompt": "go"})).await;
    assert_eq!(code(&r), "limit_exceeded", "{r}");
    assert!(r["error"]["message"].as_str().unwrap().contains("hour"));
    sqlx::query("UPDATE sessions SET created_at = now() - interval '61 minutes'")
        .execute(&hub.app.db)
        .await
        .unwrap();
    let (r, _) = spawn(&mut w, &ta, json!({"project": "personal", "prompt": "go"})).await;
    address(&r);

    // The chain's hop limit holds for spawns too.
    sqlx::query("UPDATE events SET chain = jsonb_set(chain, '{hops}', '20') WHERE event_id = $1")
        .bind(
            input["payload"]["event_id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        )
        .execute(&hub.app.db)
        .await
        .unwrap();
    let (r, _) = spawn(&mut w, &tb, json!({"project": "personal", "prompt": "go"})).await;
    assert_eq!(code(&r), "limit_exceeded", "{r}");
    assert!(r["error"]["message"].as_str().unwrap().contains("20"));
    hub.finish().await;
}
