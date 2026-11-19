//! SPEC 0.185: the owner signs a worker's own Claude Code in from hqc. The hub relays the
//! status, the sign-in page and the code; the code is stored nowhere.

mod common;

use common::*;
use serde_json::json;

const ROOT: &str = "/Users/home-mac/work";
const CODE: &str = "one-time-code-xyz#state-abc";

async fn worker(hub: &TestHub, login: bool) -> (FakeWorker, String) {
    let (_, token) = hub.enroll("home-mac").await;
    let mut w = FakeWorker::connect(hub, &token).await.expect("connect");
    let caps = if login {
        json!({"claude_code": {"version": "2.1.330", "login": true}, "acp_agents": []})
    } else {
        json!({"claude_code": {"version": "2.1.330"}, "acp_agents": []})
    };
    w.hello_with(ROOT, hub.boot_id, json!({ "capabilities": caps }))
        .await;
    let id = w.worker_id.clone();
    (w, id)
}

fn status(logged_in: bool) -> serde_json::Value {
    json!({"logged_in": logged_in, "email": "owner@example.com", "subscription_type": "max"})
}

#[tokio::test]
async fn the_owner_signs_the_workers_claude_in() {
    let hub = TestHub::start().await;
    let (mut w, id) = worker(&hub, true).await;

    let ((s, v), _) = while_answering(
        &mut w,
        "claude.auth",
        json!({"claude_auth": status(false)}),
        hub.get(&format!("/api/workers/{id}/claude")),
    )
    .await;
    assert_eq!((s, &v["logged_in"]), (200, &json!(false)), "{v}");

    let url = "https://claude.com/cai/oauth/authorize?code=true";
    let ((s, v), _) = while_answering(
        &mut w,
        "claude.login",
        json!({"login": {"login_id": "l1", "url": url}}),
        hub.post(&format!("/api/workers/{id}/claude/login"), json!({})),
    )
    .await;
    assert_eq!(
        (s, &v["login_id"], &v["url"]),
        (200, &json!("l1"), &json!(url))
    );

    let ((s, v), f) = while_answering(
        &mut w,
        "claude.login_code",
        json!({"login_done": {"ok": true, "status": status(true)}}),
        hub.post(
            &format!("/api/workers/{id}/claude/login/l1"),
            json!({ "code": CODE }),
        ),
    )
    .await;
    assert_eq!(
        (s, &v["ok"], &v["status"]["logged_in"]),
        (200, &json!(true), &json!(true))
    );
    assert_eq!(
        (&f["payload"]["login_id"], &f["payload"]["code"]),
        (&json!("l1"), &json!(CODE))
    );

    // Unknown fields are refused; cancel is relayed.
    let (s, _) = hub
        .post(
            &format!("/api/workers/{id}/claude/login/l1"),
            json!({"code": CODE, "x": 1}),
        )
        .await;
    assert!(s == 400 || s == 422, "{s}");
    let s = hub
        .delete(&format!("/api/workers/{id}/claude/login/l1"))
        .await;
    assert_eq!(s, 204);
    let f = next_of(&mut w, "claude.login_cancel").await;
    assert_eq!(f["payload"]["login_id"], "l1");

    for table in ["events", "pending_requests", "sessions", "workers"] {
        let n: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {table} t WHERE row_to_json(t)::text LIKE $1"
        ))
        .bind(format!("%{CODE}%"))
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
        assert_eq!(n, 0, "the code is in {table}");
    }
}

#[tokio::test]
async fn a_page_off_anthropic_is_refused_and_cancelled() {
    let hub = TestHub::start().await;
    let (mut w, id) = worker(&hub, true).await;
    let ((s, v), _) = while_answering(
        &mut w,
        "claude.login",
        json!({"login": {"login_id": "l2", "url": "https://claude.com.evil.io/x"}}),
        hub.post(&format!("/api/workers/{id}/claude/login"), json!({})),
    )
    .await;
    assert_eq!((s, v["error"].as_str()), (422, Some("login_failed")), "{v}");
    let f = next_of(&mut w, "claude.login_cancel").await;
    assert_eq!(f["payload"]["login_id"], "l2");
}

#[tokio::test]
async fn the_workers_refusals_and_its_absence() {
    let hub = TestHub::start().await;
    let (mut w, id) = worker(&hub, true).await;
    for (message, status, code) in [
        (
            "login_running: a sign-in is under way",
            409,
            Some("login_running"),
        ),
        ("no_login: no such sign-in", 404, None),
    ] {
        let path = format!("/api/workers/{id}/claude/login/l9");
        let req = hub.post(&path, json!({"code": "x"}));
        tokio::pin!(req);
        let f = tokio::select! {
            _ = &mut req => panic!("answered before claude.login_code"),
            f = next_of(&mut w, "claude.login_code") => f,
        };
        w.reply(
            &f["id"],
            "error",
            json!({"code": "invalid_frame", "retryable": false, "message": message}),
        )
        .await;
        let (s, v) = req.await;
        assert_eq!(s, status, "{v}");
        if let Some(c) = code {
            assert_eq!(v["error"], c);
        }
    }
    drop(w);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let (s, v) = hub.get(&format!("/api/workers/{id}/claude")).await;
        if s == 409 {
            assert_eq!(v["error"], "worker_offline");
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{s} {v}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let hub = TestHub::start().await;
    let (_w, id) = worker(&hub, false).await;
    let (s, v) = hub.get(&format!("/api/workers/{id}/claude")).await;
    assert_eq!((s, v["error"].as_str()), (422, Some("unsupported")), "{v}");
}
