//! T30 (SPEC §5.2, §7): `mcp.call` from a fake worker: the session token and the worker
//! that runs the session, the error shape, the argument cap and `hqc_list_sessions`.

mod common;

use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const WS_PATH: &str = "/Users/home-mac/work/hqc";

/// `mcp.call` and its `mcp.result` payload, answered under the call's frame id.
async fn call(w: &mut FakeWorker, token: &str, tool: &str, args: Value) -> Value {
    let rid = Uuid::now_v7().to_string();
    let id = w
        .send(
            "mcp.call",
            json!({"request_id": rid, "session_token": token, "tool": tool, "args": args}),
        )
        .await;
    let r = w.expect("mcp.result").await;
    assert_eq!(r["id"], id.as_str(), "{r}");
    assert_eq!(r["payload"]["request_id"], rid.as_str());
    r["payload"].clone()
}

async fn list(w: &mut FakeWorker, token: &str, args: Value) -> Value {
    call(w, token, "hqc_list_sessions", args).await
}

fn code(r: &Value) -> &str {
    assert!(r.get("result").is_none(), "{r}");
    r["error"]["code"].as_str().unwrap()
}

fn addresses(r: &Value) -> Vec<String> {
    r["result"]["sessions"]
        .as_array()
        .unwrap_or_else(|| panic!("{r}"))
        .iter()
        .map(|s| s["address"].as_str().unwrap().to_string())
        .collect()
}

/// A session of `w`'s workspace (the personal project's default one for the first worker).
async fn started(hub: &TestHub, w: &mut FakeWorker, title: &str) -> (String, String) {
    let sid = hub.create_session(title).await;
    let f = ack_start(w, &sid).await;
    (sid, f["payload"]["session_token"].as_str().unwrap().into())
}

#[tokio::test]
async fn the_token_must_be_a_session_of_this_worker() {
    let hub = TestHub::start().await;
    let (_, t1) = hub.enroll("one").await;
    let (_, t2) = hub.enroll("two").await;
    let (mut w1, _) = FakeWorker::online(&hub, &t1, WS_PATH).await;
    let (sid, token) = started(&hub, &mut w1, "Mine").await;

    let r = list(&mut w1, &token, json!({})).await;
    assert!(addresses(&r).contains(&"personal/mine".to_string()), "{r}");

    for bad in [
        "hqcs_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        "nonsense".to_string(),
        String::new(),
    ] {
        let r = list(&mut w1, &bad, json!({})).await;
        assert_eq!(code(&r), "unauthorized");
        assert!(!r["error"]["message"].as_str().unwrap().is_empty());
    }

    // The right token from another worker: the same answer as an unknown one.
    let (mut w2, _) = FakeWorker::online(&hub, &t2, "/Users/home-mac/work/other").await;
    let r = list(&mut w2, &token, json!({})).await;
    assert_eq!(code(&r), "unauthorized");

    // A new `session.start` (after the agent stopped) brings a new token; the one before
    // stays valid for calls on their way, the one before that does not.
    let mut tokens = vec![token];
    for n in 0..2 {
        w1.send(
            "session.state",
            json!({"session_id": sid, "process_state": "stopped", "locked_by": "none"}),
        )
        .await;
        w1.expect("ack").await;
        let (s, v) = hub
            .post(
                &format!("/api/sessions/{sid}/input"),
                json!({"text": format!("again {n}")}),
            )
            .await;
        assert_eq!(s, 201, "{v}");
        let f = ack_start(&mut w1, &sid).await;
        let input = w1.expect("session.input").await;
        w1.reply(
            &input["id"],
            "ack",
            json!({"event_id": input["payload"]["event_id"]}),
        )
        .await;
        tokens.push(f["payload"]["session_token"].as_str().unwrap().into());
    }
    assert_ne!(tokens[1], tokens[2]);
    assert_eq!(
        code(&list(&mut w1, &tokens[0], json!({})).await),
        "unauthorized"
    );
    for t in &tokens[1..] {
        assert!(list(&mut w1, t, json!({})).await.get("result").is_some());
    }

    // A tool the hub does not have.
    let r = call(&mut w1, &tokens[2], "hqc_nope", json!({})).await;
    assert_eq!(code(&r), "unknown_tool");

    // `mcp.cancel` is acked; T30's tools have nothing open on the hub.
    let id = w1
        .send(
            "mcp.cancel",
            json!({"request_id": Uuid::now_v7(), "reason": "eof"}),
        )
        .await;
    let ack = w1.expect("ack").await;
    assert_eq!(
        (ack["id"].as_str(), &ack["payload"]),
        (Some(id.as_str()), &json!({}))
    );
}

#[tokio::test]
async fn arguments_are_checked() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (_, token) = started(&hub, &mut w, "Args").await;
    for bad in [
        json!({"projct": "personal"}),
        json!({"project": 7}),
        json!("personal"),
        json!({"project": "x".repeat(hqc_proto::mcp::MAX_ARGS)}),
    ] {
        let r = list(&mut w, &token, bad.clone()).await;
        assert_eq!(code(&r), "invalid_args", "{bad}");
    }
    // Absent arguments are none.
    assert!(list(&mut w, &token, Value::Null)
        .await
        .get("result")
        .is_some());
}

#[tokio::test]
async fn list_sessions_of_the_owner() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (me, token) = started(&hub, &mut w, "Me").await;
    let (other, _) = started(&hub, &mut w, "Other one").await;
    let (gone, _) = started(&hub, &mut w, "Gone").await;
    let (s, _) = hub
        .post(&format!("/api/sessions/{gone}/archive"), json!({}))
        .await;
    assert_eq!(s, 200);
    w.expect("session.kill").await;

    // A project of the owner without sessions, and another user's project with one.
    let (s, p) = hub.post("/api/projects", json!({"name": "Empty"})).await;
    assert_eq!(s, 201, "{p}");
    let empty_id = p["id"].as_str().unwrap().to_string();
    let db = &hub.app.db;
    let stranger = hqc_hub::db::ensure_user(db, "stranger@example.com")
        .await
        .unwrap();
    let theirs = hqc_hub::db::create_project(db, stranger.id, "Theirs", "")
        .await
        .unwrap();
    hqc_hub::db::create_session(
        db,
        hqc_hub::db::NewSession {
            project_id: theirs.id,
            owner_id: stranger.id,
            workspace_id: None,
            worker_id: None,
            cwd: ".",
            title: Some("Secret"),
            model: None,
            auto_approve: false,
        },
    )
    .await
    .unwrap();

    let r = list(&mut w, &token, json!({})).await;
    let all = &r["result"];
    assert_eq!(all["truncated"], false);
    let mut got = addresses(&r);
    got.sort();
    // Archived and service sessions are left out, other users' never shown.
    assert_eq!(got, ["personal/me", "personal/other-one"], "{r}");
    let mine = all["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["address"] == "personal/me")
        .unwrap();
    assert_eq!(mine["title"], "Me");
    assert_eq!(mine["current"], true);
    let view = hub.session(&me).await;
    assert_eq!(mine["status"], view["state"]);
    let theirs_view = all["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["address"] == "personal/other-one")
        .unwrap();
    assert!(theirs_view.get("current").is_none());
    let _ = other;

    // By slug and by id; an empty project; another user's project and an unknown one are
    // not found.
    let personal = hub.personal_project().await;
    for p in ["personal".to_string(), personal] {
        let r = list(&mut w, &token, json!({"project": p})).await;
        assert_eq!(addresses(&r).len(), 2, "{r}");
    }
    let r = list(&mut w, &token, json!({"project": "empty"})).await;
    assert_eq!(addresses(&r).len(), 0);
    let r = list(&mut w, &token, json!({"project": empty_id})).await;
    assert_eq!(addresses(&r).len(), 0);
    for p in [theirs.id.to_string(), "theirs".into(), "nope".into()] {
        let r = list(&mut w, &token, json!({"project": p})).await;
        assert_eq!(code(&r), "not_found", "{p}");
    }
}
