//! T32 (SPEC §7.1, §5.2): `hqc_send` and `hqc_ask` between sessions of fake workers: the
//! input queue and the send buffer, reply matching, timeout, an answer kept for an offline
//! worker, a worker restart, `mcp.cancel`, chains, refusals and repeated calls.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const WS_PATH: &str = "/Users/home-mac/work/hqc";
const OTHER_PATH: &str = "/Users/home-mac/work/other";

/// A started session of `w`'s workspace (`workspace` none: the project's default one).
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

/// Sends `mcp.call`; returns `(request_id, frame id)`.
async fn call_with(w: &mut FakeWorker, token: &str, tool: &str, args: Value, rid: &str) -> String {
    w.send(
        "mcp.call",
        json!({"request_id": rid, "session_token": token, "tool": tool, "args": args}),
    )
    .await
}

/// A fast call's `mcp.result` payload (frames in between are returned in `other`).
async fn fast(w: &mut FakeWorker, token: &str, tool: &str, args: Value) -> Value {
    let rid = Uuid::now_v7().to_string();
    let id = call_with(w, token, tool, args, &rid).await;
    let r = w.expect("mcp.result").await;
    assert_eq!(r["id"], id.as_str(), "{r}");
    assert_eq!(r["payload"]["request_id"], rid.as_str());
    r["payload"].clone()
}

fn code(r: &Value) -> &str {
    assert!(r.get("result").is_none(), "{r}");
    r["error"]["code"].as_str().unwrap()
}

/// The next `n` frames.
async fn frames(w: &mut FakeWorker, n: usize) -> Vec<Value> {
    let mut v = Vec::new();
    for _ in 0..n {
        v.push(w.recv().await);
    }
    v
}

fn pick<'a>(frames: &'a [Value], ty: &str) -> &'a Value {
    frames
        .iter()
        .find(|f| f["type"] == ty)
        .unwrap_or_else(|| panic!("no {ty} in {frames:?}"))
}

/// `hqc_ask` from `token`: the hub acks the call and delivers the question to the target
/// on the same worker; returns `(request_id, the session.input)`.
async fn ask_same_worker(w: &mut FakeWorker, token: &str, args: Value) -> (String, Value) {
    let rid = Uuid::now_v7().to_string();
    let id = call_with(w, token, "hqc_ask", args, &rid).await;
    let f = frames(w, 2).await;
    let ack = pick(&f, "ack");
    assert_eq!(
        (ack["id"].as_str(), &ack["payload"]),
        (Some(id.as_str()), &json!({}))
    );
    (rid, pick(&f, "session.input").clone())
}

async fn ack_input(w: &mut FakeWorker, input: &Value) {
    w.reply(
        &input["id"],
        "ack",
        json!({"event_id": input["payload"]["event_id"]}),
    )
    .await;
}

fn final_answer(sid: &str, input: &Value, text: &str) -> Value {
    agent_event(
        sid,
        "agent.message",
        json!({"text": text, "final": true, "in_reply_to": input["payload"]["event_id"]}),
    )
}

async fn count(hub: &TestHub, sql: &str, id: &str) -> i64 {
    sqlx::query_scalar(sql)
        .bind(id.parse::<Uuid>().unwrap())
        .fetch_one(&hub.app.db)
        .await
        .unwrap()
}

async fn sql(hub: &TestHub, q: &str, id: &str) {
    sqlx::query(q)
        .bind(id.parse::<Uuid>().unwrap())
        .execute(&hub.app.db)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_ask_is_answered_by_the_targets_final_message() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = started(&hub, &mut w, "Asker", None).await;
    let (b, _) = started(&hub, &mut w, "Helper", None).await;

    let (rid, input) = ask_same_worker(
        &mut w,
        &ta,
        json!({"text": "Is the build green?", "session": "personal/helper"}),
    )
    .await;
    let p = &input["payload"];
    assert_eq!(p["session_id"], b.as_str());
    assert_eq!(p["text"], "Is the build green?");
    assert_eq!(
        p["peer"],
        json!({"from": "personal/asker", "mode": "ask", "request_id": rid})
    );
    assert_eq!(p["peer_prefix"], json!([]));

    // The stored message: authored by the asking agent, a new chain (a human's turn).
    let ev = hub
        .events(&b)
        .await
        .into_iter()
        .find(|e| e["type"] == "peer.message")
        .unwrap();
    assert_eq!(ev["event_id"], p["event_id"]);
    assert_eq!(ev["author"], json!({"kind": "agent", "id": a}));
    assert_eq!(ev["payload"]["from_session_id"], a.as_str());
    assert_eq!(ev["payload"]["from"], "personal/asker");
    assert_eq!(ev["payload"]["mode"], "ask");
    assert_eq!(ev["payload"]["request_id"], rid.as_str());
    assert_eq!(ev["delivery"], "pending");
    assert_eq!(
        (&ev["chain"]["depth"], &ev["chain"]["hops"]),
        (&json!(1), &json!(1))
    );
    assert_eq!(hub.session(&b).await["queue_len"], 1);

    ack_input(&mut w, &input).await;
    // A non-final message and another input's answer do not answer the ask.
    w.event(agent_event(
        &b,
        "agent.message",
        json!({"text": "looking", "final": false, "in_reply_to": p["event_id"]}),
    ))
    .await;
    w.event(agent_event(
        &b,
        "agent.message",
        json!({"text": "other", "final": true, "in_reply_to": Uuid::now_v7()}),
    ))
    .await;
    w.send(
        "event",
        json!({"event": final_answer(&b, &input, "Yes, green.")}),
    )
    .await;
    let f = frames(&mut w, 2).await;
    let r = pick(&f, "mcp.result");
    assert_eq!(
        r["payload"],
        json!({"request_id": rid, "result": {"from": "personal/helper", "reply": "Yes, green.",
                                              "message_seq": 2}})
    );
    w.reply(&r["id"], "ack", json!({})).await;
    let open = count(
        &hub,
        "SELECT count(*) FROM pending_requests WHERE session_id = $1 AND closed_at IS NULL",
        &a,
    )
    .await;
    assert_eq!(open, 0);
    w.expect_silence(Duration::from_millis(200)).await;

    // The same call again (its answer lost): the stored answer, nothing delivered twice.
    let id = call_with(
        &mut w,
        &ta,
        "hqc_ask",
        json!({"text": "Is the build green?", "session": "personal/helper"}),
        &rid,
    )
    .await;
    let r = w.expect("mcp.result").await;
    assert_eq!(r["id"], id.as_str());
    assert_eq!(r["payload"]["result"]["reply"], "Yes, green.");
    w.expect_silence(Duration::from_millis(200)).await;
    let n = hub
        .events(&b)
        .await
        .iter()
        .filter(|e| e["type"] == "peer.message")
        .count();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn refusals_come_at_once() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = started(&hub, &mut w, "Asker", None).await;
    let (b, _) = started(&hub, &mut w, "Helper", None).await;

    for tool in ["hqc_ask", "hqc_send"] {
        // The session itself, by address and by id.
        for me in ["personal/asker".to_string(), a.clone()] {
            let r = fast(&mut w, &ta, tool, json!({"text": "hi", "session": me})).await;
            assert_eq!(code(&r), "invalid_args", "{tool} {r}");
        }
        for nobody in [
            "personal/nobody",
            "nope/helper",
            "helper",
            &Uuid::now_v7().to_string(),
        ] {
            let r = fast(&mut w, &ta, tool, json!({"text": "hi", "session": nobody})).await;
            assert_eq!(code(&r), "not_found", "{tool} {nobody}");
        }
        let r = fast(
            &mut w,
            &ta,
            tool,
            json!({"text": "  ", "session": "personal/helper"}),
        )
        .await;
        assert_eq!(code(&r), "invalid_args");
        let r = fast(&mut w, &ta, tool, json!({"text": "hi"})).await;
        assert_eq!(code(&r), "invalid_args", "session is required");
        let r = fast(
            &mut w,
            &ta,
            tool,
            json!({"text": "hi", "session": "personal/helper", "extra": 1}),
        )
        .await;
        assert_eq!(code(&r), "invalid_args");
    }
    let r = fast(
        &mut w,
        &ta,
        "hqc_ask",
        json!({"text": "hi", "session": "personal/helper", "timeout_s": 0}),
    )
    .await;
    assert_eq!(code(&r), "invalid_args");

    // A session driven from VS Code: no silent queue.
    sql(
        &hub,
        "UPDATE sessions SET locked_by = 'vscode' WHERE id = $1",
        &b,
    )
    .await;
    for tool in ["hqc_ask", "hqc_send"] {
        let r = fast(&mut w, &ta, tool, json!({"text": "hi", "session": b})).await;
        assert_eq!(code(&r), "unavailable", "{tool}");
        assert!(r["error"]["message"].as_str().unwrap().contains("VS Code"));
    }
    // Another owner's session is not found, the same as none.
    let stranger = hqc_hub::db::ensure_user(&hub.app.db, "stranger-t32@example.com")
        .await
        .unwrap();
    let theirs = hqc_hub::db::create_project(&hub.app.db, stranger.id, "Theirs", "")
        .await
        .unwrap();
    let secret = hqc_hub::db::create_session(
        &hub.app.db,
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
    for target in [secret.to_string(), format!("{}/secret", theirs.slug)] {
        let r = fast(
            &mut w,
            &ta,
            "hqc_send",
            json!({"text": "hi", "session": target}),
        )
        .await;
        assert_eq!(code(&r), "not_found");
    }
    assert!(hub
        .events(&b)
        .await
        .iter()
        .all(|e| e["type"] != "peer.message"));
}

#[tokio::test]
async fn sends_wait_in_the_buffer_for_the_next_input() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (_, ta) = started(&hub, &mut w, "Asker", None).await;
    let (b, _) = started(&hub, &mut w, "Helper", None).await;

    for n in 1..=3 {
        let r = fast(
            &mut w,
            &ta,
            "hqc_send",
            json!({"text": format!("note {n}"), "session": "personal/helper"}),
        )
        .await;
        assert_eq!(
            r["result"],
            json!({"to": "personal/helper", "delivery": "buffered", "message_seq": n + 1})
        );
    }
    // No turn starts for them.
    w.expect_silence(Duration::from_millis(200)).await;
    let view = hub.session(&b).await;
    assert_eq!(
        (view["queue_len"].as_i64(), view["state"].as_str()),
        (Some(0), Some("idle"))
    );
    let sends: Vec<Value> = hub
        .events(&b)
        .await
        .into_iter()
        .filter(|e| e["type"] == "peer.message")
        .collect();
    assert_eq!(sends.len(), 3);
    assert!(sends
        .iter()
        .all(|e| e.get("delivery").is_none_or(Value::is_null)));
    // Each send moves the chain on.
    assert_eq!(sends[0]["chain"]["hops"], 1);
    assert_eq!(sends[0]["chain"]["depth"], 0);

    // The next input carries them, oldest first.
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{b}/input"),
            json!({"text": "go on"}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let input = w.expect("session.input").await;
    let p = &input["payload"];
    assert_eq!(p["text"], "go on");
    assert!(p.get("peer").is_none());
    let prefix = p["peer_prefix"].as_array().unwrap();
    let texts: Vec<&str> = prefix.iter().map(|x| x["text"].as_str().unwrap()).collect();
    assert_eq!(texts, ["note 1", "note 2", "note 3"]);
    assert!(prefix.iter().all(|x| x["from"] == "personal/asker"));
    // A send that comes while that input is on its way waits for the next one.
    fast(
        &mut w,
        &ta,
        "hqc_send",
        json!({"text": "late", "session": "personal/helper"}),
    )
    .await;
    ack_input(&mut w, &input).await;
    hub.wait_session(&b, "the turn", |v| v["state"] == "running")
        .await;
    let left = count(
        &hub,
        "SELECT count(*) FROM peer_buffer WHERE session_id = $1",
        &b,
    )
    .await;
    assert_eq!(left, 1, "the carried ones left the buffer");

    w.event(final_answer(&b, &input, "done")).await;
    hub.post(&format!("/api/sessions/{b}/input"), json!({"text": "next"}))
        .await;
    let input = w.expect("session.input").await;
    let prefix = input["payload"]["peer_prefix"].as_array().unwrap().clone();
    assert_eq!(prefix.len(), 1);
    assert_eq!(prefix[0]["text"], "late");
}

#[tokio::test]
async fn a_full_buffer_drops_the_oldest() {
    // More messages in a row than a session may send per minute (T36): the buffer is tested.
    let hub = TestHub::start_limited(hqc_hub::config::Limits {
        peer_messages_per_session_min: 1000,
        ..Default::default()
    })
    .await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (_, ta) = started(&hub, &mut w, "Asker", None).await;
    let (b, _) = started(&hub, &mut w, "Helper", None).await;

    for n in 0..51 {
        fast(
            &mut w,
            &ta,
            "hqc_send",
            json!({"text": format!("m{n}"), "session": "personal/helper"}),
        )
        .await;
    }
    let evicted: Vec<Value> = hub
        .events(&b)
        .await
        .into_iter()
        .filter(|e| e["type"] == "system" && e["payload"]["code"] == "peer_evicted")
        .collect();
    assert_eq!(evicted.len(), 1);
    assert!(evicted[0]["payload"]["text"]
        .as_str()
        .unwrap()
        .starts_with("1 message(s)"));
    assert_eq!(
        count(
            &hub,
            "SELECT count(*) FROM peer_buffer WHERE session_id = $1",
            &b
        )
        .await,
        50
    );
    // By size: two messages of 40 KB do not fit 64 KB together.
    let big = "x".repeat(40 * 1024);
    for _ in 0..2 {
        fast(
            &mut w,
            &ta,
            "hqc_send",
            json!({"text": big, "session": "personal/helper"}),
        )
        .await;
    }
    hub.post(&format!("/api/sessions/{b}/input"), json!({"text": "go"}))
        .await;
    let input = w.expect("session.input").await;
    let prefix = input["payload"]["peer_prefix"].as_array().unwrap();
    let bytes: usize = prefix
        .iter()
        .map(|x| x["text"].as_str().unwrap().len())
        .sum();
    assert!(bytes <= 64 * 1024, "{bytes}");
    assert_eq!(
        prefix.last().unwrap()["text"],
        big.as_str(),
        "the newest stays"
    );
    assert!(prefix.iter().all(|x| x["text"] != "m0"));
}

#[tokio::test]
async fn wake_on_send_queues_the_message() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (_, ta) = started(&hub, &mut w, "Asker", None).await;
    let (b, _) = started(&hub, &mut w, "Helper", None).await;
    sql(
        &hub,
        "UPDATE sessions SET wake_on_send = true WHERE id = $1",
        &b,
    )
    .await;

    let rid = Uuid::now_v7().to_string();
    let args = json!({"text": "wake up", "session": "personal/helper"});
    let id = call_with(&mut w, &ta, "hqc_send", args.clone(), &rid).await;
    let f = frames(&mut w, 2).await;
    let r = pick(&f, "mcp.result");
    assert_eq!(r["id"], id.as_str());
    assert_eq!(
        r["payload"]["result"],
        json!({"to": "personal/helper", "delivery": "queued", "message_seq": 2})
    );
    let input = pick(&f, "session.input");
    assert_eq!(
        input["payload"]["peer"],
        json!({"from": "personal/asker", "mode": "send"})
    );
    assert_eq!(input["payload"]["text"], "wake up");
    ack_input(&mut w, input).await;
    hub.wait_session(&b, "the turn", |v| v["state"] == "running")
        .await;

    // The same call again: the same result, no second message.
    call_with(&mut w, &ta, "hqc_send", args, &rid).await;
    let r = w.expect("mcp.result").await;
    assert_eq!(r["payload"]["result"]["delivery"], "queued");
    w.expect_silence(Duration::from_millis(200)).await;
    let n = hub
        .events(&b)
        .await
        .iter()
        .filter(|e| e["type"] == "peer.message")
        .count();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn an_ask_times_out_and_a_cancelled_one_is_closed() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = started(&hub, &mut w, "Asker", None).await;
    let (b, _) = started(&hub, &mut w, "Helper", None).await;

    let (rid, input) = ask_same_worker(
        &mut w,
        &ta,
        json!({"text": "quick?", "session": "personal/helper", "timeout_s": 1}),
    )
    .await;
    ack_input(&mut w, &input).await;
    let r = w.expect("mcp.result").await;
    assert_eq!(r["payload"]["request_id"], rid.as_str());
    assert_eq!(r["payload"]["error"]["code"], "timeout");
    assert!(r["payload"]["error"]["message"]
        .as_str()
        .unwrap()
        .contains("personal/helper"));
    w.reply(&r["id"], "ack", json!({})).await;
    // The late reply goes nowhere.
    w.event(final_answer(&b, &input, "too late")).await;
    w.expect_silence(Duration::from_millis(200)).await;

    // `mcp.cancel`: closed, `system ask_cancelled` in the asking session, nothing to B.
    let (rid, input) = ask_same_worker(
        &mut w,
        &ta,
        json!({"text": "slow?", "session": "personal/helper"}),
    )
    .await;
    ack_input(&mut w, &input).await;
    let id = w
        .send(
            "mcp.cancel",
            json!({"request_id": rid, "reason": "cancelled"}),
        )
        .await;
    let ack = w.expect("ack").await;
    assert_eq!(ack["id"], id.as_str());
    let ev = hub
        .events(&a)
        .await
        .into_iter()
        .find(|e| e["payload"]["code"] == "ask_cancelled")
        .unwrap();
    assert!(ev["payload"]["text"]
        .as_str()
        .unwrap()
        .contains("personal/helper"));
    assert_eq!(
        count(
            &hub,
            "SELECT count(*) FROM pending_requests WHERE session_id = $1 AND closed_at IS NULL",
            &a
        )
        .await,
        0
    );
    w.event(final_answer(&b, &input, "answer")).await;
    w.expect_silence(Duration::from_millis(300)).await;
}

#[tokio::test]
async fn the_answer_waits_for_an_offline_worker_and_a_restart_closes_the_ask() {
    let hub = TestHub::start().await;
    let (_, t1) = hub.enroll("one").await;
    let (_, t2) = hub.enroll("two").await;
    let (mut w1, _) = FakeWorker::online(&hub, &t1, WS_PATH).await;
    let (mut w2, _) = FakeWorker::online(&hub, &t2, OTHER_PATH).await;
    let (_, list) = hub.get("/api/workspaces").await;
    let other_ws = list["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["path"] == OTHER_PATH)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (a, ta) = started(&hub, &mut w1, "Asker", None).await;
    let (b, _) = started(&hub, &mut w2, "Helper", Some(&other_ws)).await;

    for restart in [false, true] {
        let rid = Uuid::now_v7().to_string();
        let id = call_with(
            &mut w1,
            &ta,
            "hqc_ask",
            json!({"text": "status?", "session": "personal/helper"}),
            &rid,
        )
        .await;
        let ack = w1.expect("ack").await;
        assert_eq!(ack["id"], id.as_str());
        let input = w2.expect("session.input").await;
        // The asking worker goes away; the target answers meanwhile.
        w1.close().await;
        hub.wait_session(&a, "worker offline", |v| v["state"] == "worker_offline")
            .await;
        ack_input(&mut w2, &input).await;
        w2.event(final_answer(&b, &input, "all good")).await;

        let boot = if restart { Uuid::now_v7() } else { hub.boot_id };
        let (w, _) = FakeWorker::online_boot(&hub, &t1, WS_PATH, boot).await;
        w1 = w;
        if restart {
            // The call died with the worker: the answer is dropped, never sent.
            w1.expect_silence(Duration::from_millis(300)).await;
            let (r, delivered): (Value, bool) = sqlx::query_as(
                "SELECT result, result_delivered_at IS NOT NULL FROM pending_requests
                 WHERE kind = 'ask' AND request_id = $1",
            )
            .bind(&rid)
            .fetch_one(&hub.app.db)
            .await
            .unwrap();
            assert_eq!(r["payload"]["result"]["reply"], "all good", "{r}");
            assert!(delivered, "not resent");
        } else {
            let r = w1.expect("mcp.result").await;
            assert_eq!(r["payload"]["request_id"], rid.as_str());
            assert_eq!(r["payload"]["result"]["reply"], "all good");
            w1.reply(&r["id"], "ack", json!({})).await;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while count(
                &hub,
                "SELECT count(*) FROM pending_requests WHERE session_id = $1
                   AND result_delivered_at IS NOT NULL",
                &a,
            )
            .await
                != 1
            {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the ack marks it delivered"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
}

#[tokio::test]
async fn an_ask_left_open_by_a_restart_before_the_answer_is_closed() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = started(&hub, &mut w, "Asker", None).await;
    let (b, _) = started(&hub, &mut w, "Helper", None).await;
    let (rid, input) = ask_same_worker(
        &mut w,
        &ta,
        json!({"text": "status?", "session": "personal/helper"}),
    )
    .await;
    w.close().await;
    let (mut w, _) = FakeWorker::online_boot(&hub, &t, WS_PATH, Uuid::now_v7()).await;
    // The question itself is still B's input after the restart.
    ack_start(&mut w, &b).await;
    let again = w.expect("session.input").await;
    assert_eq!(again["payload"]["event_id"], input["payload"]["event_id"]);
    let open = count(
        &hub,
        "SELECT count(*) FROM pending_requests WHERE session_id = $1 AND closed_at IS NULL",
        &a,
    )
    .await;
    assert_eq!(open, 0, "the asking call is gone with the restart");
    let r: Value = sqlx::query_scalar(
        "SELECT result FROM pending_requests WHERE kind = 'ask' AND request_id = $1",
    )
    .bind(&rid)
    .fetch_one(&hub.app.db)
    .await
    .unwrap();
    assert_eq!(r["payload"]["error"]["code"], "worker_restart", "{r}");
    ack_input(&mut w, &again).await;
    w.event(final_answer(&b, &again, "late")).await;
    w.expect_silence(Duration::from_millis(300)).await;
}

#[tokio::test]
async fn chains_are_limited() {
    let hub = TestHub::start().await;
    let (_, t) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &t, WS_PATH).await;
    let (a, ta) = started(&hub, &mut w, "Asker", None).await;
    let (b, tb) = started(&hub, &mut w, "Helper", None).await;
    let (_, _) = started(&hub, &mut w, "Third", None).await;

    // A asks B; B's turn runs on that question, so B's messages continue A's chain.
    let (_, input) = ask_same_worker(
        &mut w,
        &ta,
        json!({"text": "q", "session": "personal/helper"}),
    )
    .await;
    ack_input(&mut w, &input).await;
    hub.wait_session(&b, "B's turn", |v| v["state"] == "running")
        .await;
    let chain_of = |events: Vec<Value>| {
        events
            .into_iter()
            .rev()
            .find(|e| e["type"] == "peer.message")
            .unwrap()["chain"]
            .clone()
    };
    let first = chain_of(hub.events(&b).await);
    let r = fast(
        &mut w,
        &tb,
        "hqc_send",
        json!({"text": "fyi", "session": "personal/third"}),
    )
    .await;
    assert!(r.get("result").is_some(), "{r}");
    let sent: Vec<Value> = sqlx::query_scalar(
        "SELECT chain FROM events WHERE type = 'peer.message' AND author->>'id' = $1",
    )
    .bind(&b)
    .fetch_all(&hub.app.db)
    .await
    .unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["chain_id"], first["chain_id"]);
    assert_eq!(
        (&sent[0]["depth"], &sent[0]["hops"]),
        (&json!(1), &json!(2))
    );

    // At the depth limit an ask is refused, a send still goes; at the hops limit nothing.
    let set = |depth: u8, hops: u8| {
        let chain = json!({"chain_id": first["chain_id"], "depth": depth, "hops": hops});
        let ev = input["payload"]["event_id"].as_str().unwrap().to_string();
        let db = hub.app.db.clone();
        async move {
            sqlx::query("UPDATE events SET chain = $2 WHERE event_id = $1")
                .bind(ev.parse::<Uuid>().unwrap())
                .bind(chain)
                .execute(&db)
                .await
                .unwrap();
        }
    };
    set(3, 5).await;
    let r = fast(
        &mut w,
        &tb,
        "hqc_ask",
        json!({"text": "q", "session": "personal/third"}),
    )
    .await;
    assert_eq!(code(&r), "limit_exceeded");
    let r = fast(
        &mut w,
        &tb,
        "hqc_send",
        json!({"text": "s", "session": "personal/third"}),
    )
    .await;
    assert!(r.get("result").is_some(), "{r}");
    set(1, 20).await;
    let r = fast(
        &mut w,
        &tb,
        "hqc_send",
        json!({"text": "s", "session": "personal/third"}),
    )
    .await;
    assert_eq!(code(&r), "limit_exceeded");
    let r = fast(
        &mut w,
        &tb,
        "hqc_ask",
        json!({"text": "q", "session": "personal/asker"}),
    )
    .await;
    assert_eq!(code(&r), "limit_exceeded");

    // B answers A; then a human's turn starts a new chain.
    w.send("event", json!({"event": final_answer(&b, &input, "done")}))
        .await;
    let f = frames(&mut w, 2).await;
    let r = pick(&f, "mcp.result");
    assert_eq!(r["payload"]["result"]["reply"], "done");
    w.reply(&r["id"], "ack", json!({})).await;
    hub.post(
        &format!("/api/sessions/{b}/input"),
        json!({"text": "carry on"}),
    )
    .await;
    let human = w.expect("session.input").await;
    ack_input(&mut w, &human).await;
    hub.wait_session(&b, "B's turn", |v| v["state"] == "running")
        .await;
    let r = fast(
        &mut w,
        &tb,
        "hqc_send",
        json!({"text": "s", "session": "personal/asker"}),
    )
    .await;
    assert!(r.get("result").is_some(), "{r}");
    let last = chain_of(hub.events(&a).await);
    assert_ne!(last["chain_id"], first["chain_id"]);
    assert_eq!((&last["depth"], &last["hops"]), (&json!(0), &json!(1)));
}
