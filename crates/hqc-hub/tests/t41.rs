//! T41 (SPEC §4, §5.3, §7, §7.1, §12): files between sessions. A message of `hqc_send` /
//! `hqc_ask` carries the artifact ids the sender's worker uploaded; the target's worker may
//! download them; a reply carries the files the target attached during its answering turn,
//! which the asking worker may then download; buffered sends carry theirs in `peer_prefix`;
//! another owner's artifact is `not_found`; a repeated call acts once.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const WS_PATH: &str = "/Users/home-mac/work/hqc";
const OTHER_PATH: &str = "/Users/home-mac/work/other";

/// Two workers of one owner; session A on the first, B on the second. Returns the workers,
/// their tokens and `(a, token of a, b, token of b)`.
async fn two_workers(
    hub: &TestHub,
) -> (
    FakeWorker,
    FakeWorker,
    String,
    String,
    (String, String, String, String),
) {
    let (_, t1) = hub.enroll("one").await;
    let (_, t2) = hub.enroll("two").await;
    let (mut w1, _) = FakeWorker::online(hub, &t1, WS_PATH).await;
    let (mut w2, _) = FakeWorker::online(hub, &t2, OTHER_PATH).await;
    let (_, list) = hub.get("/api/workspaces").await;
    let ws_of = |path: &str| -> String {
        list["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["path"] == path)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (a, ta) = started(hub, &mut w1, "Asker", &ws_of(WS_PATH)).await;
    let (b, tb) = started(hub, &mut w2, "Helper", &ws_of(OTHER_PATH)).await;
    (w1, w2, t1, t2, (a, ta, b, tb))
}

async fn started(hub: &TestHub, w: &mut FakeWorker, title: &str, ws: &str) -> (String, String) {
    let pid = hub.personal_project().await;
    let (s, v) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": title, "workspace_id": ws}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    let f = ack_start(w, &sid).await;
    (sid, f["payload"]["session_token"].as_str().unwrap().into())
}

/// `PUT /worker/artifacts` for `session` with worker token `token`: the artifact's id.
async fn upload(hub: &TestHub, token: &str, session: &str, name: &str, body: &[u8]) -> String {
    let r = hub
        .http
        .put(format!("{}/worker/artifacts", hub.base))
        .bearer_auth(token)
        .header("x-hqc-session-id", session)
        .header("x-hqc-artifact-name", hqc_hub::artifacts::pct(name))
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.status());
    let v: Value = r.json().await.unwrap();
    v["id"].as_str().unwrap().to_string()
}

/// What `GET /worker/artifacts/{id}` gives worker token `token`: status and body.
async fn download(hub: &TestHub, token: &str, id: &str) -> (u16, Vec<u8>) {
    let r = hub
        .http
        .get(format!("{}/worker/artifacts/{id}", hub.base))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let s = r.status().as_u16();
    (s, r.bytes().await.unwrap().to_vec())
}

async fn call(w: &mut FakeWorker, token: &str, tool: &str, args: Value, rid: &str) -> String {
    w.send(
        "mcp.call",
        json!({"request_id": rid, "session_token": token, "tool": tool, "args": args}),
    )
    .await
}

/// A fast call's `mcp.result` payload.
async fn fast(w: &mut FakeWorker, token: &str, tool: &str, args: Value, rid: &str) -> Value {
    let id = call(w, token, tool, args, rid).await;
    let r = w.expect("mcp.result").await;
    assert_eq!(r["id"], id.as_str(), "{r}");
    r["payload"].clone()
}

async fn ack_input(w: &mut FakeWorker, input: &Value) {
    w.reply(
        &input["id"],
        "ack",
        json!({"event_id": input["payload"]["event_id"]}),
    )
    .await;
}

/// An artifact of another user, linked to `session` as if it had been given to it: only the
/// owner check keeps it out.
async fn strangers_artifact(hub: &TestHub, session: &str) -> String {
    let db = &hub.app.db;
    let stranger =
        hqc_hub::db::ensure_user(db, &format!("stranger-{}@example.com", Uuid::now_v7()))
            .await
            .unwrap();
    let id = Uuid::now_v7();
    let sha = format!("{:064x}", id.as_u128());
    sqlx::query(
        "INSERT INTO artifacts (id, owner_id, kind, mime, size, sha256, name, storage_key)
         VALUES ($1, $2, 'text', 'text/plain', 6, $3, 'secret.txt', $3)",
    )
    .bind(id)
    .bind(stranger.id)
    .bind(&sha)
    .execute(db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO artifact_sessions (artifact_id, session_id) VALUES ($1, $2)")
        .bind(id)
        .bind(session.parse::<Uuid>().unwrap())
        .execute(db)
        .await
        .unwrap();
    id.to_string()
}

fn peer_messages(evs: Vec<Value>) -> Vec<Value> {
    evs.into_iter()
        .filter(|e| e["type"] == "peer.message")
        .collect()
}

#[tokio::test]
async fn files_go_with_the_question_and_come_back_with_the_reply() {
    let hub = TestHub::start().await;
    let (mut w1, mut w2, t1, t2, (a, ta, b, tb)) = two_workers(&hub).await;

    // A's worker uploaded the agent's file for A; B's worker cannot read it yet.
    let plan = upload(&hub, &t1, &a, "plan.txt", b"the plan").await;
    assert_eq!(download(&hub, &t2, &plan).await.0, 404);

    let rid = Uuid::now_v7().to_string();
    let args =
        json!({"text": "Do the plan", "session": "personal/helper", "artifact_ids": [plan, plan]});
    let id = call(&mut w1, &ta, "hqc_ask", args.clone(), &rid).await;
    let ack = w1.expect("ack").await;
    assert_eq!(ack["id"], id.as_str());
    let input = w2.expect("session.input").await;
    assert_eq!(input["payload"]["artifact_ids"], json!([plan]), "once each");
    assert_eq!(input["payload"]["peer"]["mode"], "ask");
    let ev = peer_messages(hub.events(&b).await).remove(0);
    assert_eq!(ev["payload"]["artifact_ids"], json!([plan]));
    // Now B's worker may download it.
    assert_eq!(
        download(&hub, &t2, &plan).await,
        (200, b"the plan".to_vec())
    );
    ack_input(&mut w2, &input).await;

    // B's turn: an hqc_attach, an attachment event of B's worker naming another owner's
    // artifact, the same file attached twice; then the final answer.
    let png = upload(&hub, &t2, &b, "result.png", b"\x89PNG done").await;
    let r = fast(
        &mut w2,
        &tb,
        "hqc_attach",
        json!({"artifact_id": png}),
        &Uuid::now_v7().to_string(),
    )
    .await;
    assert_eq!(r["result"]["artifact_id"], png.as_str(), "{r}");
    let foreign = strangers_artifact(&hub, &b).await;
    w2.event(agent_event(
        &b,
        "attachment",
        json!({"artifact_id": foreign, "kind": "text", "name": "secret.txt"}),
    ))
    .await;
    fast(
        &mut w2,
        &tb,
        "hqc_attach",
        json!({"artifact_id": png, "caption": "again"}),
        &Uuid::now_v7().to_string(),
    )
    .await;
    assert_eq!(download(&hub, &t1, &png).await.0, 404, "not A's yet");
    w2.event(agent_event(
        &b,
        "agent.message",
        json!({"text": "Done.", "final": true, "in_reply_to": input["payload"]["event_id"]}),
    ))
    .await;

    let r = w1.expect("mcp.result").await;
    assert_eq!(
        r["payload"],
        json!({"request_id": rid, "result": {
            "from": "personal/helper",
            "reply": "Done.",
            "files": [{"artifact_id": png, "name": "result.png", "kind": "image"}],
        }})
    );
    w1.reply(&r["id"], "ack", json!({})).await;
    // A's worker may download the reply's file; never the other owner's.
    assert_eq!(
        download(&hub, &t1, &png).await,
        (200, b"\x89PNG done".to_vec())
    );
    assert_eq!(download(&hub, &t1, &foreign).await.0, 404);

    // The same call again: the stored answer with its files, nothing stored twice.
    let id = call(&mut w1, &ta, "hqc_ask", args, &rid).await;
    let again = w1.expect("mcp.result").await;
    assert_eq!(again["id"], id.as_str());
    assert_eq!(again["payload"], r["payload"]);
    w1.expect_silence(Duration::from_millis(200)).await;
    w2.expect_silence(Duration::from_millis(200)).await;
    assert_eq!(peer_messages(hub.events(&b).await).len(), 1);

    // The inbox folders to clean up: B's question, A's call.
    let db = &hub.app.db;
    let of = |s: &str| s.parse::<Uuid>().unwrap();
    assert_eq!(
        hqc_hub::db::inbox_events(db, of(&b)).await.unwrap(),
        [of(input["payload"]["event_id"].as_str().unwrap())]
    );
    assert_eq!(
        hqc_hub::db::inbox_events(db, of(&a)).await.unwrap(),
        [of(&rid)]
    );
}

#[tokio::test]
async fn a_reply_without_files_and_files_before_the_question() {
    let hub = TestHub::start().await;
    let (mut w1, mut w2, _t1, t2, (_a, ta, b, tb)) = two_workers(&hub).await;
    // A file B attached before the question is not part of the reply.
    let old = upload(&hub, &t2, &b, "old.txt", b"old").await;
    let r = fast(
        &mut w2,
        &tb,
        "hqc_attach",
        json!({"artifact_id": old}),
        &Uuid::now_v7().to_string(),
    )
    .await;
    assert!(r.get("result").is_some(), "{r}");
    let rid = Uuid::now_v7().to_string();
    call(
        &mut w1,
        &ta,
        "hqc_ask",
        json!({"text": "Anything?", "session": "personal/helper"}),
        &rid,
    )
    .await;
    w1.expect("ack").await;
    let input = w2.expect("session.input").await;
    assert_eq!(input["payload"]["artifact_ids"], json!([]));
    assert!(peer_messages(hub.events(&b).await)[0]["payload"]
        .get("artifact_ids")
        .is_none());
    ack_input(&mut w2, &input).await;
    w2.event(agent_event(
        &b,
        "agent.message",
        json!({"text": "No.", "final": true, "in_reply_to": input["payload"]["event_id"]}),
    ))
    .await;
    let r = w1.expect("mcp.result").await;
    assert_eq!(
        r["payload"]["result"],
        json!({"from": "personal/helper", "reply": "No."})
    );
}

#[tokio::test]
async fn files_must_be_the_owners_and_this_sessions() {
    let hub = TestHub::start().await;
    let (mut w1, mut w2, t1, t2, (a, ta, b, _tb)) = two_workers(&hub).await;
    let foreign = strangers_artifact(&hub, &a).await;
    // The owner's file, but uploaded for B, not given to A.
    let bs = upload(&hub, &t2, &b, "b.txt", b"b").await;
    let unknown = Uuid::now_v7().to_string();
    for (tool, ids) in [
        ("hqc_send", json!([foreign])),
        ("hqc_ask", json!([foreign])),
        ("hqc_send", json!([bs])),
        ("hqc_send", json!([unknown])),
    ] {
        let r = fast(
            &mut w1,
            &ta,
            tool,
            json!({"text": "here", "session": "personal/helper", "artifact_ids": ids}),
            &Uuid::now_v7().to_string(),
        )
        .await;
        assert_eq!(r["error"]["code"], "not_found", "{tool} {ids}: {r}");
    }
    let many: Vec<String> = (0..33).map(|_| Uuid::now_v7().to_string()).collect();
    let r = fast(
        &mut w1,
        &ta,
        "hqc_send",
        json!({"text": "here", "session": "personal/helper", "artifact_ids": many}),
        &Uuid::now_v7().to_string(),
    )
    .await;
    assert_eq!(r["error"]["code"], "invalid_args", "{r}");
    // Paths never reach the hub: the worker turns `files` into artifact ids.
    let r = fast(
        &mut w1,
        &ta,
        "hqc_send",
        json!({"text": "here", "session": "personal/helper", "files": ["a.txt"]}),
        &Uuid::now_v7().to_string(),
    )
    .await;
    assert_eq!(r["error"]["code"], "invalid_args", "{r}");
    assert!(peer_messages(hub.events(&b).await).is_empty());
    assert_eq!(download(&hub, &t2, &foreign).await.0, 404);
    // A's own upload is fine.
    let mine = upload(&hub, &t1, &a, "a.txt", b"a").await;
    let r = fast(
        &mut w1,
        &ta,
        "hqc_send",
        json!({"text": "here", "session": "personal/helper", "artifact_ids": [mine]}),
        &Uuid::now_v7().to_string(),
    )
    .await;
    assert_eq!(r["result"]["delivery"], "buffered", "{r}");
    w2.expect_silence(Duration::from_millis(100)).await;
}

#[tokio::test]
async fn buffered_sends_carry_their_files() {
    let hub = TestHub::start().await;
    let (mut w1, mut w2, t1, t2, (a, ta, b, _tb)) = two_workers(&hub).await;
    let f1 = upload(&hub, &t1, &a, "one.txt", b"one").await;
    let rid = Uuid::now_v7().to_string();
    let args = json!({"text": "first", "session": "personal/helper", "artifact_ids": [f1]});
    let r = fast(&mut w1, &ta, "hqc_send", args.clone(), &rid).await;
    assert_eq!(
        r["result"],
        json!({"to": "personal/helper", "delivery": "buffered"})
    );
    // A repeat acts once.
    let again = fast(&mut w1, &ta, "hqc_send", args, &rid).await;
    assert_eq!(again["result"], r["result"]);
    fast(
        &mut w1,
        &ta,
        "hqc_send",
        json!({"text": "second", "session": "personal/helper"}),
        &Uuid::now_v7().to_string(),
    )
    .await;
    let sends = peer_messages(hub.events(&b).await);
    assert_eq!(sends.len(), 2);
    assert_eq!(sends[0]["payload"]["artifact_ids"], json!([f1]));

    hub.post(
        &format!("/api/sessions/{b}/input"),
        json!({"text": "go on"}),
    )
    .await;
    let input = w2.expect("session.input").await;
    let prefix = input["payload"]["peer_prefix"].as_array().unwrap();
    assert_eq!(prefix.len(), 2);
    assert_eq!(prefix[0]["event_id"], sends[0]["event_id"]);
    assert_eq!(prefix[0]["artifact_ids"], json!([f1]));
    assert_eq!(prefix[1]["event_id"], sends[1]["event_id"]);
    assert!(prefix[1].get("artifact_ids").is_none(), "{prefix:?}");
    assert_eq!(download(&hub, &t2, &f1).await, (200, b"one".to_vec()));
    // The buffered message's folder is cleaned up with B.
    let of = |s: &Value| s.as_str().unwrap().parse::<Uuid>().unwrap();
    assert_eq!(
        hqc_hub::db::inbox_events(&hub.app.db, of(&json!(b)))
            .await
            .unwrap(),
        [of(&sends[0]["event_id"])]
    );
}
