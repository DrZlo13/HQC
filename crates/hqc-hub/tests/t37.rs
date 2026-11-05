//! T37 (SPEC §6, §10.1, §16): the user's open requests in one list (`GET /api/requests`, the
//! `requests` frame of the summary subscription), with the sessions that wait on them through
//! open `hqc_ask`s, transitively; live changes; the push of a waited-on request opens the top
//! of the chain; another owner's requests are never listed.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const WS_PATH: &str = "/Users/home-mac/work/hqc";
const OTHER_PATH: &str = "/Users/home-mac/work/other";

fn approval(request_id: &str) -> Value {
    json!({
        "request_id": request_id,
        "tool": "Bash",
        "description": "cargo build --release",
        "options": [
            {"id": "allow", "label": "Allow once", "canonical": "once"},
            {"id": "deny", "label": "Deny", "canonical": "deny"}
        ],
        "expires_at": "2099-01-01T00:00:00Z",
    })
}

/// A started session of `w` (`workspace` none: the project's default one).
async fn started(
    hub: &TestHub,
    w: &mut FakeWorker,
    title: &str,
    workspace: Option<&str>,
) -> (String, String) {
    let pid = hub.personal_project().await;
    let mut body = json!({"title": title});
    if let Some(ws) = workspace {
        body["workspace_id"] = json!(ws);
    }
    let (s, v) = hub
        .post(&format!("/api/projects/{pid}/sessions"), body)
        .await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    let f = ack_start(w, &sid).await;
    (sid, f["payload"]["session_token"].as_str().unwrap().into())
}

/// `hqc_ask` from the session of `token` on `from` to `to`: the asking worker gets the
/// call's `ack`, the target's worker the question; the question is acked and its turn runs.
async fn ask(from: &mut FakeWorker, token: &str, to: &str, target: Option<&mut FakeWorker>) {
    let rid = Uuid::now_v7().to_string();
    let id = from
        .send(
            "mcp.call",
            json!({"request_id": rid, "session_token": token, "tool": "hqc_ask",
                   "args": {"text": "How is the build?", "session": to}}),
        )
        .await;
    let (input, w) = match target {
        Some(t) => {
            let ack = from.expect("ack").await;
            assert_eq!(ack["id"], id.as_str());
            (t.expect("session.input").await, t)
        }
        None => {
            // The same worker: the call's ack and the question in either order.
            let (a, b) = (from.recv().await, from.recv().await);
            let (ack, input) = if a["type"] == "ack" { (a, b) } else { (b, a) };
            assert_eq!(ack["id"], id.as_str(), "{ack}");
            assert_eq!(input["type"], "session.input", "{input}");
            (input, from)
        }
    };
    w.reply(
        &input["id"],
        "ack",
        json!({"event_id": input["payload"]["event_id"]}),
    )
    .await;
}

async fn list(hub: &TestHub) -> Vec<Value> {
    let (s, v) = hub.get("/api/requests").await;
    assert_eq!(s, 200, "{v}");
    v["requests"].as_array().unwrap().clone()
}

/// The next `requests` frame the client gets.
async fn next_requests(c: &mut FakeClient) -> Vec<Value> {
    c.until("requests").await["requests"]
        .as_array()
        .unwrap()
        .clone()
}

fn ids(list: &[Value], field: &str) -> Vec<String> {
    list.iter()
        .map(|r| r[field].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn the_owners_requests_are_listed_and_followed_live() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (sid, _) = started(&hub, &mut w, "Build", None).await;

    let mut c = FakeClient::connect(&hub).await;
    c.send(json!({"type": "subscribe_summary"})).await;
    c.until("summary").await;
    assert!(next_requests(&mut c).await.is_empty());
    assert!(list(&hub).await.is_empty());

    // An approval: listed with where it is, its event, nobody waiting.
    w.event(agent_event(&sid, "approval.request", approval("r-1")))
        .await;
    let got = next_requests(&mut c).await;
    assert_eq!(got.len(), 1, "{got:?}");
    let r = &got[0];
    assert_eq!(r["session_id"], sid.as_str());
    assert_eq!(
        (r["request_id"].as_str(), r["kind"].as_str()),
        (Some("r-1"), Some("approval"))
    );
    assert_eq!(r["address"], "personal/build");
    assert_eq!(r["title"], "Build");
    assert_eq!(r["worker_name"], "home-mac");
    assert_eq!(r["worker_id"], w.worker_id.to_string().as_str());
    assert!(r.get("system").is_none() && r.get("waiting").is_none());
    assert_eq!(r["event"]["type"], "approval.request");
    assert_eq!(r["event"]["payload"]["request_id"], "r-1");
    assert!(r["event"]["seq"].is_u64());
    let typed: hqc_proto::api::RequestList =
        serde_json::from_value(json!({ "requests": got })).unwrap();
    assert_eq!(typed.requests.len(), 1);
    assert_eq!(list(&hub).await, got, "REST and the frame agree");

    // A clarify and a worker's own request join it, oldest first.
    w.event(agent_event(
        &sid,
        "clarify.request",
        json!({"request_id": "c-1", "questions": [{"id": "q", "text": "Which?"}]}),
    ))
    .await;
    assert_eq!(next_requests(&mut c).await.len(), 2);
    let id = w
        .send(
            "worker.approval.request",
            json!({"request_id": "ws-1", "kind": "workspace.use",
                   "description": "The hub wants to run sessions in /w/new",
                   "expires_at": chrono::Utc::now() + chrono::TimeDelta::minutes(10)}),
        )
        .await;
    assert_eq!(w.expect("ack").await["id"], id.as_str());
    let got = next_requests(&mut c).await;
    assert_eq!(ids(&got, "request_id"), ["r-1", "c-1", "ws-1"]);
    assert_eq!(
        ids(&got, "kind"),
        ["approval", "clarify", "worker_approval"]
    );
    let ws = &got[2];
    assert_eq!(
        (ws["system"].as_bool(), ws["title"].as_str()),
        (Some(true), Some("home-mac"))
    );
    assert_eq!(ws["event"]["payload"]["tool"], "workspace.use");

    // Answered or cancelled anywhere, it leaves the list at once.
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/r-1"),
            json!({"option_id": "allow"}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    w.expect("approval.response").await;
    assert_eq!(
        ids(&next_requests(&mut c).await, "request_id"),
        ["c-1", "ws-1"]
    );
    // The worker closes the clarify itself (the runtime gave up on it).
    w.event(agent_event(
        &sid,
        "clarify.cancel",
        json!({"request_id": "c-1", "reason": "stopped"}),
    ))
    .await;
    assert_eq!(ids(&next_requests(&mut c).await, "request_id"), ["ws-1"]);
    let sys = ws["session_id"].as_str().unwrap().to_string();
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sys}/requests/ws-1"),
            json!({"option_id": "deny"}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(next_requests(&mut c).await.is_empty());
    assert!(list(&hub).await.is_empty());
    hub.finish().await;
}

#[tokio::test]
async fn another_owners_requests_are_never_listed() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (mine, _) = started(&hub, &mut w, "Mine", None).await;

    // Another user's session with an open request, and an ask of theirs aimed at mine.
    let db = &hub.app.db;
    let stranger =
        hqc_hub::db::ensure_user(db, &format!("stranger-{}@example.com", Uuid::now_v7()))
            .await
            .unwrap();
    let theirs = hqc_hub::db::create_project(db, stranger.id, "Theirs", "")
        .await
        .unwrap();
    let secret = hqc_hub::db::create_session(
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
            permission_mode: None,
        },
    )
    .await
    .unwrap();
    let s = hqc_hub::db::session(db, secret).await.unwrap().unwrap();
    hqc_hub::events::store_hub_event(
        &hub.app,
        &s,
        json!({"kind": "system"}),
        "approval.request",
        approval("theirs-1"),
        None,
        None,
    )
    .await
    .unwrap();
    hqc_hub::db::open_request(
        db,
        "approval",
        secret,
        "theirs-1",
        &approval("theirs-1"),
        None,
        None,
    )
    .await
    .unwrap();
    hqc_hub::db::open_request(
        db,
        "ask",
        secret,
        "theirs-ask",
        &json!({"target_session_id": mine, "event_id": Uuid::now_v7(), "to": "personal/mine"}),
        None,
        None,
    )
    .await
    .unwrap();
    w.event(agent_event(&mine, "approval.request", approval("mine-1")))
        .await;

    let got = list(&hub).await;
    assert_eq!(ids(&got, "request_id"), ["mine-1"], "{got:?}");
    assert!(
        got[0].get("waiting").is_none(),
        "another owner's ask is not a wait: {got:?}"
    );
    let theirs_list = hqc_hub::requests::open_of(&hub.app, stranger.id)
        .await
        .unwrap();
    assert_eq!(theirs_list.len(), 1);
    assert_eq!(theirs_list[0].request_id, "theirs-1");
    // Their request cannot be answered from here either.
    let (s, _) = hub
        .post(
            &format!("/api/sessions/{secret}/requests/theirs-1"),
            json!({"option_id": "allow"}),
        )
        .await;
    assert_eq!(s, 404);

    // A change of theirs sends my clients nothing.
    let mut c = FakeClient::connect(&hub).await;
    c.send(json!({"type": "subscribe_summary"})).await;
    c.until("summary").await;
    assert_eq!(next_requests(&mut c).await.len(), 1);
    hqc_hub::requests::cancel(
        &hub.app,
        &hqc_hub::db::find_open_request(db, secret, "theirs-1")
            .await
            .unwrap()
            .unwrap(),
        hqc_proto::event::CancelReason::Timeout,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Summaries of my own sessions may come; no `requests`.
    while let Ok(f) = tokio::time::timeout(Duration::from_millis(200), c.recv()).await {
        assert_ne!(f["type"], "requests", "{f}");
    }
    hub.finish().await;
}

/// A waits on B (on another worker), B waits on C: C's request names both, the nearest
/// first; its push opens A; answered from A's view it is gone everywhere, and the answer is
/// an event of C, sent to C's worker.
#[tokio::test]
async fn requests_show_where_the_user_waits_along_the_chain() {
    let hub = TestHub::start().await;
    let (s, _) = hub
        .post(
            "/api/push/subscribe",
            json!({"endpoint": "https://fcm.googleapis.com/fcm/send/t37", "expirationTime": null,
                   "keys": {"p256dh": "k", "auth": "a"}}),
        )
        .await;
    assert_eq!(s, 201);
    let (_, t1) = hub.enroll("home-mac").await;
    let (_, t2) = hub.enroll("llm-server").await;
    let (mut w1, _) = FakeWorker::online(&hub, &t1, WS_PATH).await;
    let (mut w2, _) = FakeWorker::online(&hub, &t2, OTHER_PATH).await;
    let (_, l) = hub.get("/api/workspaces").await;
    let other_ws = l["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["path"] == OTHER_PATH)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (a, ta) = started(&hub, &mut w1, "Asker", None).await;
    let (b, tb) = started(&hub, &mut w2, "Helper", Some(&other_ws)).await;
    let (c_sid, _) = started(&hub, &mut w2, "Deep", Some(&other_ws)).await;

    let mut c = FakeClient::connect(&hub).await;
    c.send(json!({"type": "subscribe_summary"})).await;
    c.until("summary").await;
    next_requests(&mut c).await;

    // B has a request open before anyone asks it: the ask brings it to A.
    w2.event(agent_event(&b, "approval.request", approval("b-1")))
        .await;
    let got = next_requests(&mut c).await;
    assert!(got[0].get("waiting").is_none());
    ask(&mut w1, &ta, "personal/helper", Some(&mut w2)).await;
    let got = next_requests(&mut c).await;
    assert_eq!(got[0]["request_id"], "b-1");
    assert_eq!(
        ids(got[0]["waiting"].as_array().unwrap(), "session_id"),
        [a.as_str()]
    );
    assert_eq!(got[0]["waiting"][0]["address"], "personal/asker");
    assert_eq!(got[0]["worker_name"], "llm-server");

    // B asks C; C asks for permission: A and B wait on it, B the nearest.
    ask(&mut w2, &tb, "personal/deep", None).await;
    next_requests(&mut c).await;
    hub.push.sent.lock().unwrap().clear();
    w2.event(agent_event(&c_sid, "approval.request", approval("c-1")))
        .await;
    let got = next_requests(&mut c).await;
    let deep = got.iter().find(|r| r["request_id"] == "c-1").unwrap();
    assert_eq!(
        ids(deep["waiting"].as_array().unwrap(), "session_id"),
        [b.as_str(), a.as_str()]
    );
    assert_eq!(list(&hub).await, got);

    // One push for C's request; its click opens A, its tag stays C's request.
    let deadline = tokio::time::Instant::now() + WAIT;
    let sent = loop {
        let sent = hub.push.sent.lock().unwrap().clone();
        if !sent.is_empty() {
            break sent;
        }
        assert!(tokio::time::Instant::now() < deadline, "no push");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(hub.push.sent.lock().unwrap().len(), 1, "one notification");
    let m: hqc_proto::api::PushMessage = serde_json::from_value(sent[0].1.clone()).unwrap();
    assert_eq!(m.session_id.to_string(), c_sid);
    assert_eq!(m.open_session_id.map(|x| x.to_string()), Some(a.clone()));
    assert_eq!(m.tag.as_deref(), Some(format!("{c_sid}:c-1").as_str()));

    // While the user looks at A, another request of C gets no push: its card is in A.
    let mut watcher = FakeClient::connect(&hub).await;
    watcher.subscribe(&a, 0).await;
    w2.event(agent_event(&c_sid, "approval.request", approval("c-2")))
        .await;
    next_requests(&mut c).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        hub.push.sent.lock().unwrap().len(),
        1,
        "no push while watching A"
    );

    // Answered from A's view (the same endpoint, C's session): the answer is C's event and
    // goes to C's worker; nothing is written in A or B.
    let before_a = hub.events(&a).await.len();
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{c_sid}/requests/c-1"),
            json!({"option_id": "deny", "message": "Not now"}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let f = w2.expect("approval.response").await;
    assert_eq!(f["payload"]["session_id"], c_sid.as_str());
    assert_eq!(f["payload"]["message"], "Not now");
    let got = next_requests(&mut c).await;
    assert_eq!(ids(&got, "request_id"), ["b-1", "c-2"]);
    assert!(hub
        .events(&c_sid)
        .await
        .iter()
        .any(|e| e["type"] == "approval.response" && e["payload"]["request_id"] == "c-1"));
    assert_eq!(hub.events(&a).await.len(), before_a);
    // A second answer from another view is refused as before.
    let (s, _) = hub
        .post(
            &format!("/api/sessions/{c_sid}/requests/c-1"),
            json!({"option_id": "allow"}),
        )
        .await;
    assert_eq!(s, 409);

    // C replies to B: B no longer waits on C, so C's request stays C's alone; A still waits
    // on B.
    let input = hub
        .events(&c_sid)
        .await
        .into_iter()
        .find(|e| e["type"] == "peer.message")
        .unwrap();
    w2.send(
        "event",
        json!({"event": agent_event(
            &c_sid,
            "agent.message",
            json!({"text": "fine", "final": true, "in_reply_to": input["event_id"]}),
        )}),
    )
    .await;
    // The event's ack and the answer to B's ask, in either order.
    let mut types = vec![
        w2.recv().await["type"].clone(),
        w2.recv().await["type"].clone(),
    ];
    types.sort_by_key(|t| t.to_string());
    assert_eq!(types, ["ack", "mcp.result"]);
    let got = loop {
        let got = next_requests(&mut c).await;
        if got
            .iter()
            .all(|r| r["request_id"] != "c-2" || r.get("waiting").is_none())
        {
            break got;
        }
    };
    let b1 = got.iter().find(|r| r["request_id"] == "b-1").unwrap();
    assert_eq!(
        ids(b1["waiting"].as_array().unwrap(), "session_id"),
        [a.as_str()]
    );
    hub.finish().await;
}
