//! T42 (SPEC §7, §7.1): replies that come back as a send. While A waits on an ask to B, an
//! `hqc_send` from B to A from B's turn on the question goes with the reply (its text after
//! the final message, its files with the reply's files) instead of A's send buffer; a send
//! from B outside that turn, after the reply, or from another session is buffered as before;
//! a repeat acts once; an ask that closes without a reply buffers what was joined to it.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const WS_PATH: &str = "/Users/home-mac/work/hqc";
const OTHER_PATH: &str = "/Users/home-mac/work/other";
const SEPARATOR: &str = hqc_hub::peers::JOINED_SEPARATOR;

/// Two workers of one owner: A (`personal/asker`) on the first; B (`personal/helper`) and C
/// (`personal/third`) on the second. Returns the workers, their tokens and the sessions with
/// their session tokens.
struct Setup {
    w1: FakeWorker,
    w2: FakeWorker,
    t1: String,
    t2: String,
    a: (String, String),
    b: (String, String),
    c: (String, String),
}

async fn setup(hub: &TestHub) -> Setup {
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
    let a = started(hub, &mut w1, "Asker", &ws_of(WS_PATH)).await;
    let b = started(hub, &mut w2, "Helper", &ws_of(OTHER_PATH)).await;
    let c = started(hub, &mut w2, "Third", &ws_of(OTHER_PATH)).await;
    Setup {
        w1,
        w2,
        t1,
        t2,
        a,
        b,
        c,
    }
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

/// A asks B `text`: the call's `request_id` and B's `session.input` (not acked).
async fn ask(s: &mut Setup, args: Value) -> (String, Value) {
    let rid = Uuid::now_v7().to_string();
    let id = call(&mut s.w1, &s.a.1, "hqc_ask", args, &rid).await;
    let ack = s.w1.expect("ack").await;
    assert_eq!(ack["id"], id.as_str());
    let input = s.w2.expect("session.input").await;
    assert_eq!(input["payload"]["peer"]["mode"], "ask");
    (rid, input)
}

fn final_answer(session: &str, input: &Value, text: &str) -> Value {
    agent_event(
        session,
        "agent.message",
        json!({"text": text, "final": true, "in_reply_to": input["payload"]["event_id"]}),
    )
}

/// `peer_prefix` texts of A's next input.
async fn a_buffer(hub: &TestHub, s: &mut Setup) -> Vec<String> {
    let a = &s.a.0;
    hub.post(&format!("/api/sessions/{a}/input"), json!({"text": "next"}))
        .await;
    let input = s.w1.expect("session.input").await;
    input["payload"]["peer_prefix"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["text"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn a_send_from_the_answering_turn_joins_the_reply_with_its_files() {
    let hub = TestHub::start().await;
    let mut s = setup(&hub).await;
    hub.buffer_sends(&s.a.0).await;
    let (b, tb) = s.b.clone();
    let (_, tc) = s.c.clone();
    let (rid, input) = ask(
        &mut s,
        json!({"text": "Fix the file", "session": "personal/helper"}),
    )
    .await;
    ack_input(&mut s.w2, &input).await;

    // Another session's send to A meanwhile: buffered as before.
    let r = fast(
        &mut s.w2,
        &tc,
        "hqc_send",
        json!({"text": "from C", "session": "personal/asker"}),
        &Uuid::now_v7().to_string(),
    )
    .await;
    assert_eq!(r["result"]["delivery"], "buffered", "{r}");

    // B's turn on the question: an hqc_attach, then a send to A with a file.
    let png = upload(&hub, &s.t2, &b, "shot.png", b"\x89PNG").await;
    fast(
        &mut s.w2,
        &tb,
        "hqc_attach",
        json!({"artifact_id": png}),
        &Uuid::now_v7().to_string(),
    )
    .await;
    let fixed = upload(&hub, &s.t2, &b, "fixed.txt", b"fixed").await;
    let send_rid = Uuid::now_v7().to_string();
    let send = json!({"text": "Here is the fixed file", "session": "personal/asker",
                      "artifact_ids": [fixed]});
    let r = fast(&mut s.w2, &tb, "hqc_send", send.clone(), &send_rid).await;
    let seq_in_a = r["result"]["message_seq"].clone();
    assert!(seq_in_a.is_i64(), "{r}");
    assert_eq!(
        r["result"],
        json!({"to": "personal/asker", "delivery": "with_reply", "message_seq": seq_in_a})
    );
    // A repeat acts once and answers the same.
    let again = fast(&mut s.w2, &tb, "hqc_send", send, &send_rid).await;
    assert_eq!(again["result"], r["result"]);
    s.w1.expect_silence(Duration::from_millis(200)).await;
    let a_msgs: Vec<Value> = hub
        .events(&s.a.0)
        .await
        .into_iter()
        .filter(|e| e["type"] == "peer.message")
        .collect();
    assert_eq!(a_msgs.len(), 2, "C's and B's, once each");
    assert!(a_msgs.iter().all(|m| m.get("delivery").is_none()));

    s.w2.event(final_answer(&b, &input, "Done.")).await;
    let r = s.w1.expect("mcp.result").await;
    assert_eq!(
        r["payload"],
        json!({"request_id": rid, "result": {
            "from": "personal/helper",
            "reply": format!("Done.{SEPARATOR}Here is the fixed file"),
            "files": [
                {"artifact_id": png, "name": "shot.png", "kind": "image"},
                {"artifact_id": fixed, "name": "fixed.txt", "kind": "text"},
            ],
            "message_seq": 2,
        }})
    );
    s.w1.reply(&r["id"], "ack", json!({})).await;
    assert_eq!(
        download(&hub, &s.t1, &fixed).await,
        (200, b"fixed".to_vec())
    );

    // A's buffer has C's message only: B's went with the reply.
    assert_eq!(a_buffer(&hub, &mut s).await, ["from C"]);
}

#[tokio::test]
async fn sends_outside_the_answering_turn_are_buffered() {
    let hub = TestHub::start().await;
    let mut s = setup(&hub).await;
    hub.buffer_sends(&s.a.0).await;
    let (b, tb) = s.b.clone();

    // B is in a turn on the user's input when the question comes; a send from that turn is
    // not part of the reply, although the ask is open.
    hub.post(&format!("/api/sessions/{b}/input"), json!({"text": "work"}))
        .await;
    let user = s.w2.expect("session.input").await;
    ack_input(&mut s.w2, &user).await;
    let (_, input) = ask(
        &mut s,
        json!({"text": "Status?", "session": "personal/helper"}),
    )
    .await;
    let r = fast(
        &mut s.w2,
        &tb,
        "hqc_send",
        json!({"text": "from the user's turn", "session": "personal/asker"}),
        &Uuid::now_v7().to_string(),
    )
    .await;
    assert_eq!(r["result"]["delivery"], "buffered", "{r}");
    // The same with the turn named by the worker.
    let id =
        s.w2.send(
            "mcp.call",
            json!({"request_id": Uuid::now_v7().to_string(), "session_token": tb,
                   "tool": "hqc_send", "turn_input": user["payload"]["event_id"],
                   "args": {"text": "named turn", "session": "personal/asker"}}),
        )
        .await;
    let r = s.w2.expect("mcp.result").await;
    assert_eq!(r["id"], id.as_str());
    assert_eq!(r["payload"]["result"]["delivery"], "buffered", "{r}");

    // The answering turn ends first; a send after the reply is a normal send.
    ack_input(&mut s.w2, &input).await;
    s.w2.event(final_answer(&b, &input, "Green.")).await;
    let r = s.w1.expect("mcp.result").await;
    assert_eq!(
        r["payload"]["result"],
        json!({"from": "personal/helper", "reply": "Green.", "message_seq": 3})
    );
    s.w1.reply(&r["id"], "ack", json!({})).await;
    let r = fast(
        &mut s.w2,
        &tb,
        "hqc_send",
        json!({"text": "after the reply", "session": "personal/asker"}),
        &Uuid::now_v7().to_string(),
    )
    .await;
    assert_eq!(r["result"]["delivery"], "buffered", "{r}");

    assert_eq!(
        a_buffer(&hub, &mut s).await,
        ["from the user's turn", "named turn", "after the reply"]
    );
}

#[tokio::test]
async fn an_ask_that_gets_no_reply_buffers_its_joined_sends() {
    let hub = TestHub::start().await;
    let mut s = setup(&hub).await;
    let (_, tb) = s.b.clone();
    let (rid, input) = ask(
        &mut s,
        json!({"text": "quick?", "session": "personal/helper", "timeout_s": 1}),
    )
    .await;
    ack_input(&mut s.w2, &input).await;
    let r = fast(
        &mut s.w2,
        &tb,
        "hqc_send",
        json!({"text": "partial answer", "session": "personal/asker"}),
        &Uuid::now_v7().to_string(),
    )
    .await;
    assert_eq!(r["result"]["delivery"], "with_reply", "{r}");
    let r = s.w1.expect("mcp.result").await;
    assert_eq!(r["payload"]["request_id"], rid.as_str());
    assert_eq!(r["payload"]["error"]["code"], "timeout");
    s.w1.reply(&r["id"], "ack", json!({})).await;
    assert_eq!(a_buffer(&hub, &mut s).await, ["partial answer"]);
}
