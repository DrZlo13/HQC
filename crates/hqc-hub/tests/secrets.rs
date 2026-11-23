//! SPEC 0.147 (iteration 6, T58): secrets and ssh keys for a session's agent. The hub passes a
//! value to the worker in `secret.put` and keeps it nowhere; `hqc_secret` waits for the owner's
//! give or decline; `hqc_ssh_keygen` asks the worker for a key; a workspace's kept keys.

mod common;

use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const WS_PATH: &str = "/Users/home-mac/work/lab/pi";
const VALUE: &str = "hunter2-wifi-value";

/// A worker whose `hello` has `session_secrets: secrets`, a session in its workspace started
/// (`session.start` acked): `(worker, session id, session token, workspace id)`.
async fn session(hub: &TestHub, secrets: bool) -> (FakeWorker, String, String, String) {
    session_with(hub, secrets, false).await
}

/// [`session`] with `capabilities.vault: vault` in the worker's `hello` (a worker that reads
/// Vaultwarden through `rbw`).
async fn session_with(
    hub: &TestHub,
    secrets: bool,
    vault: bool,
) -> (FakeWorker, String, String, String) {
    let (_, token) = hub.enroll("home-mac").await;
    let mut w = FakeWorker::connect(hub, &token)
        .await
        .expect("worker connect");
    let mut capabilities = json!({"claude_code": {"version": "2.1.307"}, "acp_agents": []});
    if vault {
        capabilities["vault"] = json!(true);
    }
    w.hello_with(
        WS_PATH,
        hub.boot_id,
        json!({ "session_secrets": secrets, "capabilities": capabilities }),
    )
    .await;
    let pid = hub.personal_project().await;
    let (s, ws) = hub.add_workspace(&mut w, &pid, WS_PATH).await;
    assert_eq!(s, 201, "{ws}");
    let (s, v) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": "pi", "workspace_id": ws["id"]}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    let start = ack_start(&mut w, &sid).await;
    let st = start["payload"]["session_token"]
        .as_str()
        .unwrap()
        .to_string();
    (w, sid, st, ws["id"].as_str().unwrap().to_string())
}

/// Nothing the hub stores carries `VALUE`: events, requests, sessions.
async fn stored_nowhere(hub: &TestHub) {
    for table in ["events", "pending_requests", "sessions", "workers"] {
        let n: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {table} t WHERE row_to_json(t)::text LIKE $1"
        ))
        .bind(format!("%{VALUE}%"))
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
        assert_eq!(n, 0, "the value is in {table}");
    }
}

fn outcome(path: bool) -> Value {
    if path {
        json!({"secret": {"note": "The secret WIFI_PASS is in the file \"$HQC_SECRETS/WIFI_PASS\"", "path": "$HQC_SECRETS/WIFI_PASS"}})
    } else {
        json!({"secret": {"note": "loaded", "fingerprint": "SHA256:abc", "remembered": true}})
    }
}

#[tokio::test]
async fn a_secret_goes_to_the_worker_and_nowhere_else() {
    let hub = TestHub::start().await;
    let (mut w, sid, _, _) = session(&hub, true).await;
    let (s, v) = hub.get("/api/workers").await;
    assert_eq!(
        (s, &v["workers"][0]["session_secrets"]),
        (200, &json!(true))
    );

    let url = format!("/api/sessions/{sid}/secrets");
    let ((s, v), f) = while_answering(
        &mut w,
        "secret.put",
        outcome(true),
        hub.post(
            &url,
            json!({"kind": "secret", "name": "WIFI_PASS", "value": VALUE}),
        ),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["path"], "$HQC_SECRETS/WIFI_PASS");
    let p = &f["payload"];
    assert_eq!(
        (&p["session_id"], &p["kind"], &p["value"]),
        (&json!(sid), &json!("secret"), &json!(VALUE))
    );
    let given = hub
        .wait_events(&sid, "secret_given", |e| {
            e.iter().any(|e| e["payload"]["code"] == "secret_given")
        })
        .await;
    let line = given
        .iter()
        .find(|e| e["payload"]["code"] == "secret_given")
        .unwrap();
    assert_eq!(line["payload"]["text"], "Secret WIFI_PASS given");

    // The worker refuses: 422 with its reason, nothing written.
    let refused = json!({"code": "invalid_frame", "retryable": false, "message": "the agent is not running: give it once it runs"});
    let (s, v) = {
        let req = hub.post(
            &url,
            json!({"kind": "ssh_key", "name": "pi-lab", "value": VALUE}),
        );
        tokio::pin!(req);
        let f = tokio::select! {
            _ = &mut req => panic!("answered before secret.put"),
            f = next_of(&mut w, "secret.put") => f,
        };
        w.reply(&f["id"], "error", refused).await;
        req.await
    };
    assert_eq!(
        (s, v["error"].as_str()),
        (422, Some("secret_refused")),
        "{v}"
    );
    assert!(v["message"].as_str().unwrap().contains("not running"));

    // Refused before the worker: a bad name, an unknown field, a remembered plain secret,
    // a made key.
    for bad in [
        json!({"kind": "secret", "name": "../x", "value": VALUE}),
        json!({"kind": "secret", "name": "A", "value": VALUE, "remeber": true}),
        json!({"kind": "secret", "name": "A", "value": VALUE, "remember": true}),
        json!({"kind": "ssh_keygen", "name": "A", "value": VALUE}),
        json!({"kind": "secret", "name": "A", "value": ""}),
    ] {
        let (s, v) = hub.post(&url, bad.clone()).await;
        assert!(s == 400 || s == 422, "{bad}: {s} {v}");
        assert!(!v.to_string().contains(VALUE), "{v}");
    }
    w.expect_silence(std::time::Duration::from_millis(200))
        .await;
    stored_nowhere(&hub).await;

    // Offline: 409, the client keeps its form.
    w.close().await;
    let (s, v) = {
        let mut r = (0, Value::Null);
        for _ in 0..50 {
            r = hub
                .post(&url, json!({"kind": "secret", "name": "A", "value": VALUE}))
                .await;
            if r.0 == 409 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        r
    };
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some("worker_offline")),
        "{v}"
    );
    hub.finish().await;
}

#[tokio::test]
async fn a_worker_without_session_secrets_gets_none() {
    let hub = TestHub::start().await;
    let (mut w, sid, token, _) = session(&hub, false).await;
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/secrets"),
            json!({"kind": "secret", "name": "A", "value": VALUE}),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (409, Some("unavailable")), "{v}");
    let id = w
        .send(
            "mcp.call",
            json!({"request_id": Uuid::now_v7().to_string(), "session_token": token,
                   "tool": "hqc_secret", "args": {"name": "A", "purpose": "the router"}}),
        )
        .await;
    let r = next_of(&mut w, "mcp.result").await;
    assert_eq!(r["id"], id.as_str());
    assert_eq!(r["payload"]["error"]["code"], "unavailable", "{r}");
    w.expect_silence(std::time::Duration::from_millis(200))
        .await;
    hub.finish().await;
}

#[tokio::test]
async fn hqc_secret_waits_for_the_owner() {
    let hub = TestHub::start().await;
    let (mut w, sid, token, _) = session(&hub, true).await;
    let call = |rid: &str, args: Value| json!({"request_id": rid, "session_token": token, "tool": "hqc_secret", "args": args});

    // Bad arguments are the call's error at once.
    let id = w
        .send(
            "mcp.call",
            call("r-bad", json!({"name": "A", "purpose": "two\nlines"})),
        )
        .await;
    let r = next_of(&mut w, "mcp.result").await;
    assert_eq!(
        (r["id"].as_str(), r["payload"]["error"]["code"].as_str()),
        (Some(id.as_str()), Some("invalid_args"))
    );

    // The card, and the call waits.
    let rid = Uuid::now_v7().to_string();
    let args =
        json!({"name": "pi-lab", "purpose": "the key of the Pi in the kitchen", "kind": "ssh_key"});
    let id = w.send("mcp.call", call(&rid, args.clone())).await;
    let ack = w.expect("ack").await;
    assert_eq!(ack["id"], id.as_str());
    let evs = hub
        .wait_events(&sid, "the card", |e| {
            e.iter().any(|e| e["type"] == "approval.request")
        })
        .await;
    let card = evs
        .iter()
        .find(|e| e["type"] == "approval.request")
        .unwrap();
    assert_eq!(card["payload"]["tool"], "hqc_secret");
    assert_eq!(
        card["payload"]["secret"],
        json!({"name": "pi-lab", "kind": "ssh_key"})
    );
    assert_eq!(
        card["payload"]["description"],
        "the key of the Pi in the kitchen"
    );
    let (s, open) = hub.get("/api/requests").await;
    assert_eq!(s, 200);
    assert!(open.to_string().contains(&rid), "{open}");
    // A repeat of the call opens no second card.
    w.send("mcp.call", call(&rid, args)).await;
    w.expect("ack").await;

    let url = format!("/api/sessions/{sid}/secrets");
    // Another name or kind than the request's: refused.
    let (s, v) = hub
        .post(
            &url,
            json!({"kind": "ssh_key", "name": "other", "value": VALUE, "request_id": rid}),
        )
        .await;
    assert_eq!(s, 400, "{v}");
    // An answer through the request: only deny.
    let (s, _) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/{rid}"),
            json!({"option_id": "once"}),
        )
        .await;
    assert_eq!(s, 400);

    // Given: the call gets its result, the card its answer.
    let ((s, v), _) = while_answering(
        &mut w,
        "secret.put",
        outcome(false),
        hub.post(
            &url,
            json!({"kind": "ssh_key", "name": "pi-lab", "value": VALUE, "passphrase": "p", "remember": true, "request_id": rid}),
        ),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let r = next_of(&mut w, "mcp.result").await;
    assert_eq!(r["payload"]["request_id"], rid.as_str());
    assert_eq!(r["payload"]["result"]["fingerprint"], "SHA256:abc", "{r}");
    assert!(!r.to_string().contains(VALUE));
    w.reply(&r["id"], "ack", json!({})).await;
    let evs = hub
        .wait_events(&sid, "the answer", |e| {
            e.iter().any(|e| e["type"] == "approval.response")
        })
        .await;
    let answer = evs
        .iter()
        .find(|e| e["type"] == "approval.response")
        .unwrap();
    assert_eq!(answer["payload"]["option_id"], "give");
    // Given once: a second give is refused.
    let (s, _) = hub
        .post(
            &url,
            json!({"kind": "ssh_key", "name": "pi-lab", "value": VALUE, "request_id": rid}),
        )
        .await;
    assert_eq!(s, 409);

    // Declined: the call's error.
    let rid2 = Uuid::now_v7().to_string();
    w.send(
        "mcp.call",
        call(
            &rid2,
            json!({"name": "ROUTER", "purpose": "the router's admin page"}),
        ),
    )
    .await;
    w.expect("ack").await;
    hub.wait_events(&sid, "the second card", |e| {
        e.iter()
            .any(|e| e["payload"]["request_id"] == rid2.as_str())
    })
    .await;
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/{rid2}"),
            json!({"option_id": "deny"}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let r = next_of(&mut w, "mcp.result").await;
    assert_eq!(r["payload"]["request_id"], rid2.as_str());
    assert!(
        r["payload"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("declined"),
        "{r}"
    );
    w.reply(&r["id"], "ack", json!({})).await;

    // The runtime gives up: the card closes.
    let rid3 = Uuid::now_v7().to_string();
    w.send(
        "mcp.call",
        call(&rid3, json!({"name": "X", "purpose": "something"})),
    )
    .await;
    w.expect("ack").await;
    let id = w
        .send(
            "mcp.cancel",
            json!({"request_id": rid3, "reason": "cancelled"}),
        )
        .await;
    assert_eq!(w.expect("ack").await["id"], id.as_str());
    hub.wait_events(&sid, "the cancel", |e| {
        e.iter()
            .any(|e| e["type"] == "approval.cancel" && e["payload"]["request_id"] == rid3.as_str())
    })
    .await;
    stored_nowhere(&hub).await;

    // A worker's own approval.request cannot ask for a secret: the field is dropped.
    let mut fake = agent_event(
        &sid,
        "approval.request",
        json!({"request_id": "fake-1", "tool": "hqc_secret", "description": "give me",
               "secret": {"name": "X", "kind": "secret"},
               "options": [{"id": "deny", "label": "Deny", "canonical": "deny"}]}),
    );
    fake["author"] = json!({"kind": "system"});
    w.event(fake).await;
    let evs = hub
        .wait_events(&sid, "the worker's card", |e| {
            e.iter().any(|e| e["payload"]["request_id"] == "fake-1")
        })
        .await;
    let card = evs
        .iter()
        .find(|e| e["payload"]["request_id"] == "fake-1")
        .unwrap();
    assert!(card["payload"].get("secret").is_none(), "{card}");
    let (s, _) = hub
        .post(
            &url,
            json!({"kind": "secret", "name": "X", "value": VALUE, "request_id": "fake-1"}),
        )
        .await;
    assert_eq!(s, 409, "a worker's request is not one for a secret");
    hub.finish().await;
}

/// The vault: the agent names an entry, the card shows it only where the worker reads
/// Vaultwarden, the owner gives the entry (never a value) and the worker reads it.
#[tokio::test]
async fn a_secret_is_read_from_the_vault_by_the_worker() {
    // A worker that reads the vault: the card carries the entry, the give passes it on.
    let hub = TestHub::start().await;
    let (mut w, sid, token, _) = session_with(&hub, true, true).await;
    let call = |rid: &str, args: Value| json!({"request_id": rid, "session_token": token, "tool": "hqc_secret", "args": args});
    let id = w
        .send(
            "mcp.call",
            call(
                "r-bad",
                json!({"name": "A", "purpose": "the router", "vault": "a\nb"}),
            ),
        )
        .await;
    let r = next_of(&mut w, "mcp.result").await;
    assert_eq!(
        (r["id"].as_str(), r["payload"]["error"]["code"].as_str()),
        (Some(id.as_str()), Some("invalid_args"))
    );
    let rid = Uuid::now_v7().to_string();
    w.send(
        "mcp.call",
        call(
            &rid,
            json!({"name": "WIFI_PASS", "purpose": "the camera's Wi-Fi", "vault": "home-wifi"}),
        ),
    )
    .await;
    w.expect("ack").await;
    let evs = hub
        .wait_events(&sid, "the card", |e| {
            e.iter().any(|e| e["type"] == "approval.request")
        })
        .await;
    let card = evs
        .iter()
        .find(|e| e["type"] == "approval.request")
        .unwrap();
    assert_eq!(
        card["payload"]["secret"],
        json!({"name": "WIFI_PASS", "kind": "secret", "vault": "home-wifi"})
    );

    let url = format!("/api/sessions/{sid}/secrets");
    // Both or neither of value and vault, a bad entry: 400 before the worker.
    for bad in [
        json!({"kind": "secret", "name": "WIFI_PASS", "value": VALUE, "vault": "home-wifi", "request_id": rid}),
        json!({"kind": "secret", "name": "WIFI_PASS", "request_id": rid}),
        json!({"kind": "secret", "name": "WIFI_PASS", "vault": "", "request_id": rid}),
        json!({"kind": "secret", "name": "WIFI_PASS", "vault": "a\u{202E}b", "request_id": rid}),
        json!({"kind": "secret", "name": "WIFI_PASS", "vault": "x".repeat(201), "request_id": rid}),
    ] {
        let (s, v) = hub.post(&url, bad.clone()).await;
        assert_eq!(s, 400, "{bad}: {v}");
    }
    w.expect_silence(std::time::Duration::from_millis(200))
        .await;

    // The worker refuses (locked vault): 422 with its reason, the request stays open.
    let locked = json!({"code": "invalid_frame", "retryable": false, "message": "Vaultwarden is locked on this machine: unlock rbw here, or type the value"});
    let (s, v) = {
        let req = hub.post(
            &url,
            json!({"kind": "secret", "name": "WIFI_PASS", "vault": "home-wifi", "request_id": rid}),
        );
        tokio::pin!(req);
        let f = tokio::select! {
            _ = &mut req => panic!("answered before secret.put"),
            f = next_of(&mut w, "secret.put") => f,
        };
        w.reply(&f["id"], "error", locked).await;
        req.await
    };
    assert_eq!(
        (s, v["error"].as_str()),
        (422, Some("secret_refused")),
        "{v}"
    );
    assert!(v["message"].as_str().unwrap().contains("locked"), "{v}");
    let (_, open) = hub.get("/api/requests").await;
    assert!(open.to_string().contains(&rid), "still open: {open}");

    // Given: the worker got the entry and no value, the feed names the entry, the call closes.
    let ((s, v), f) = while_answering(
        &mut w,
        "secret.put",
        outcome(true),
        hub.post(
            &url,
            json!({"kind": "secret", "name": "WIFI_PASS", "vault": "home-wifi", "request_id": rid}),
        ),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(f["payload"]["vault"], "home-wifi", "{f}");
    assert!(f["payload"].get("value").is_none(), "{f}");
    let r = next_of(&mut w, "mcp.result").await;
    assert_eq!(r["payload"]["request_id"], rid.as_str());
    assert_eq!(
        r["payload"]["result"]["path"], "$HQC_SECRETS/WIFI_PASS",
        "{r}"
    );
    w.reply(&r["id"], "ack", json!({})).await;
    let given = hub
        .wait_events(&sid, "secret_given", |e| {
            e.iter().any(|e| e["payload"]["code"] == "secret_given")
        })
        .await;
    let line = given
        .iter()
        .find(|e| e["payload"]["code"] == "secret_given")
        .unwrap();
    assert_eq!(
        line["payload"]["text"],
        "Secret WIFI_PASS given from Vaultwarden entry home-wifi"
    );
    let (_, open) = hub.get("/api/requests").await;
    assert!(!open.to_string().contains(&rid), "closed: {open}");
    hub.finish().await;

    // A worker that does not read the vault: the entry is dropped from the card, a give with
    // one is 409 and sends nothing, a typed value goes as before.
    let hub = TestHub::start().await;
    let (mut w, sid, token, _) = session_with(&hub, true, false).await;
    let rid = Uuid::now_v7().to_string();
    w.send(
        "mcp.call",
        json!({"request_id": rid, "session_token": token, "tool": "hqc_secret",
               "args": {"name": "WIFI_PASS", "purpose": "the camera's Wi-Fi", "vault": "home-wifi"}}),
    )
    .await;
    w.expect("ack").await;
    let evs = hub
        .wait_events(&sid, "the card", |e| {
            e.iter().any(|e| e["type"] == "approval.request")
        })
        .await;
    let card = evs
        .iter()
        .find(|e| e["type"] == "approval.request")
        .unwrap();
    assert_eq!(
        card["payload"]["secret"],
        json!({"name": "WIFI_PASS", "kind": "secret"})
    );
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/secrets"),
            json!({"kind": "secret", "name": "WIFI_PASS", "vault": "home-wifi", "request_id": rid}),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (409, Some("unavailable")), "{v}");
    assert!(
        v["message"].as_str().unwrap().contains("Vaultwarden"),
        "{v}"
    );
    w.expect_silence(std::time::Duration::from_millis(200))
        .await;
    hub.finish().await;
}

#[tokio::test]
async fn hqc_ssh_keygen_and_the_kept_keys() {
    let hub = TestHub::start().await;
    let (mut w, sid, token, ws) = session(&hub, true).await;
    let id = w
        .send(
            "mcp.call",
            json!({"request_id": "k-1", "session_token": token, "tool": "hqc_ssh_keygen",
                   "args": {"name": "pi-kitchen"}}),
        )
        .await;
    let f = next_of(&mut w, "secret.put").await;
    assert_eq!(f["payload"]["kind"], "ssh_keygen");
    assert_eq!(f["payload"]["remember"], true);
    assert!(f["payload"].get("value").is_none());
    w.reply(
        &f["id"],
        "result",
        json!({"request_id": f["payload"]["request_id"], "secret": {
            "note": "made", "fingerprint": "SHA256:k", "public_key": "ssh-ed25519 AAAA hqc pi-kitchen", "remembered": true}}),
    )
    .await;
    let r = next_of(&mut w, "mcp.result").await;
    assert_eq!(r["id"], id.as_str());
    assert_eq!(
        r["payload"]["result"]["public_key"], "ssh-ed25519 AAAA hqc pi-kitchen",
        "{r}"
    );
    let evs = hub
        .wait_events(&sid, "the line", |e| {
            e.iter().any(|e| e["payload"]["code"] == "secret_given")
        })
        .await;
    let line = evs
        .iter()
        .find(|e| e["payload"]["code"] == "secret_given")
        .unwrap();
    assert_eq!(
        line["payload"]["text"],
        "ssh key pi-kitchen (SHA256:k) made for this workspace"
    );

    let keys = format!("/api/workspaces/{ws}/keys");
    let ((s, v), f) = while_answering(
        &mut w,
        "workspace.keys",
        json!({"keys": [{"name": "pi-kitchen", "fingerprint": "SHA256:k"}]}),
        hub.get(&keys),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(f["payload"]["path"], WS_PATH);
    assert_eq!(v["keys"][0]["name"], "pi-kitchen");
    let (s, _) = while_answering(
        &mut w,
        "workspace.key_delete",
        json!({"key_deleted": false}),
        hub.delete(&format!("{keys}/pi-kitchen")),
    )
    .await;
    assert_eq!(s, 404);
    let (s, f) = while_answering(
        &mut w,
        "workspace.key_delete",
        json!({"key_deleted": true}),
        hub.delete(&format!("{keys}/pi-kitchen")),
    )
    .await;
    assert_eq!((s, &f["payload"]["name"]), (204, &json!("pi-kitchen")));
    // Another owner's workspace looks like a missing one.
    assert_eq!(
        hub.delete(&format!("/api/workspaces/{}/keys/x", Uuid::now_v7()))
            .await,
        404
    );
    hub.finish().await;
}
