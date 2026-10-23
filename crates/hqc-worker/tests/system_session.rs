//! T17: confirmation of a new folder for someone else's session and the worker's log lines
//! in its service session (SPEC §10.2). Fake hub, fake adapters.

mod common;

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use common::*;
use hqc_worker::adapter::fake::FakeAdapters;
use hqc_worker::known_paths::KnownPaths;
use hqc_worker::worker_log::{self, LogLine};
use serde_json::{json, Value};
use uuid::Uuid;

/// `allow_foreign_sessions = true`: the folder check is what this file is about.
fn foreign_config(env: &Env, hub_url: &str) -> hqc_worker::config::Config {
    let text = env.config_toml(hub_url).replace(
        "allow_foreign_sessions = false",
        "allow_foreign_sessions = true",
    );
    hqc_worker::config::Config::parse(&text).unwrap()
}

fn foreign_start(env: &Env, sid: Uuid) -> Value {
    let mut p = env.start_payload(sid);
    p["owner_is_worker_owner"] = json!(false);
    p
}

fn known(env: &Env) -> KnownPaths {
    KnownPaths::load(&env.known_paths()).unwrap()
}

/// Someone else's session in a folder the worker has not seen: `session.start` waits for
/// `worker.approval.request` -> `once`, then goes on and the folder is remembered; the
/// next foreign session there starts without asking.
#[tokio::test]
async fn new_folder_for_a_foreign_session_needs_the_owner() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker_with(&env, foreign_config(&env, &hub.url()), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;

    let sid = Uuid::now_v7();
    let start_id = c.frame("session.start", foreign_start(&env, sid)).await;
    let req = c.expect("worker.approval.request").await;
    let p = &req["payload"];
    assert_eq!(p["kind"], "workspace.use");
    let folder = env.workspace().display().to_string();
    assert!(p["description"].as_str().unwrap().contains(&folder), "{p}");
    let expires: chrono::DateTime<Utc> = serde_json::from_value(p["expires_at"].clone()).unwrap();
    assert!(expires > Utc::now() + chrono::TimeDelta::minutes(5));
    let request_id = p["request_id"].as_str().unwrap().to_string();
    c.send(json!({"id": req["id"], "type": "ack", "payload": {}}))
        .await;
    // Nothing starts and no ack comes while the owner has not answered.
    assert!(c.maybe("ack", Duration::from_millis(200)).await.is_none());
    assert_eq!(adapters.created(), 0);

    // A second start in the same folder waits for the same request.
    let sid2 = Uuid::now_v7();
    let start2 = c.frame("session.start", foreign_start(&env, sid2)).await;
    assert!(c
        .maybe("worker.approval.request", Duration::from_millis(200))
        .await
        .is_none());

    let resp = c
        .frame(
            "worker.approval.response",
            json!({"request_id": request_id, "option_id": "once"}),
        )
        .await;
    let mut acked = vec![];
    for _ in 0..3 {
        acked.push(c.expect("ack").await["id"].as_str().unwrap().to_string());
    }
    acked.sort();
    let mut want = vec![resp, start_id, start2];
    want.sort();
    assert_eq!(acked, want);
    assert!(known(&env).contains(&env.workspace()));

    // Known now: the next foreign session starts at once.
    let sid3 = Uuid::now_v7();
    let id3 = c.frame("session.start", foreign_start(&env, sid3)).await;
    let ack = c.expect("ack").await;
    assert_eq!(ack["id"], id3.as_str());
    assert!(c
        .skipped
        .iter()
        .all(|f| f["type"] != "worker.approval.request"));
    w.stop().await;
}

/// `deny` refuses the start (`invalid_frame`, not retryable) and nothing is remembered;
/// an answer nobody waits for is acked and dropped.
#[tokio::test]
async fn refused_folder_refuses_the_start() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker_with(&env, foreign_config(&env, &hub.url()), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;

    let sid = Uuid::now_v7();
    let start_id = c.frame("session.start", foreign_start(&env, sid)).await;
    let req = c.expect("worker.approval.request").await;
    let request_id = req["payload"]["request_id"].clone();
    c.frame(
        "worker.approval.response",
        json!({"request_id": request_id, "option_id": "deny"}),
    )
    .await;
    let err = c.expect("error").await;
    assert_eq!(err["id"], start_id.as_str());
    assert_eq!(err["payload"]["code"], "invalid_frame");
    assert_eq!(err["payload"]["retryable"], false);
    assert_eq!(adapters.created(), 0);
    assert!(!known(&env).contains(&env.workspace()));

    let late = c
        .frame(
            "worker.approval.response",
            json!({"request_id": "wa-unknown", "option_id": "once"}),
        )
        .await;
    let ack = c.expect("ack").await;
    assert_eq!(ack["id"], late.as_str());
    w.stop().await;
}

/// The worker owner's own sessions, and sessions in a workspace the owner added
/// (`requested_by_owner`), need no confirmation; the folder becomes known.
#[tokio::test]
async fn owner_needs_no_confirmation() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker_with(&env, foreign_config(&env, &hub.url()), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;

    let id = c
        .frame("session.start", env.start_payload(Uuid::now_v7()))
        .await;
    assert_eq!(c.expect("ack").await["id"], id.as_str());
    assert!(known(&env).contains(&env.workspace()));

    // Another folder, a foreign session in a workspace the owner added.
    std::fs::create_dir_all(env.root().join("other")).unwrap();
    let mut p = foreign_start(&env, Uuid::now_v7());
    p["workspace_path"] = json!(env.root().join("other").display().to_string());
    p["requested_by_owner"] = json!(true);
    let id = c.frame("session.start", p).await;
    assert_eq!(c.expect("ack").await["id"], id.as_str());
    assert!(c
        .skipped
        .iter()
        .all(|f| f["type"] != "worker.approval.request"));
    w.stop().await;
}

/// A request lost with the connection is sent again, with the same `request_id`.
#[tokio::test]
async fn waiting_request_is_sent_again_after_reconnect() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker_with(&env, foreign_config(&env, &hub.url()), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    c.frame("session.start", foreign_start(&env, Uuid::now_v7()))
        .await;
    let first = c.expect("worker.approval.request").await;
    c.close().await;

    let mut c = hub.accept().await;
    c.handshake().await;
    let again = c.expect("worker.approval.request").await;
    assert_eq!(again["payload"], first["payload"]);
    w.stop().await;
}

/// Log lines go to the service session named in `hello.ack` as `system {code: worker_log,
/// level}`, through the outbox; the same text at most once a second, the rest reported as
/// «пропущено N». Lines logged before `hello.ack` wait for it.
#[tokio::test]
async fn log_lines_reach_the_service_session_rate_limited() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let (tx, rx) = worker_log::channel();
    let line = |level: &'static str, text: &str| LogLine {
        level,
        text: text.into(),
        ts: Utc::now(),
    };
    tx.send(line("error", "before hello"));
    let w = start_worker_logs(
        &env,
        env.config(&hub.url()),
        Arc::new(adapters.clone()),
        Some(rx),
    );
    let mut c = hub.accept().await;
    let sys = Uuid::now_v7();
    c.handshake_system(sys).await;

    let ev = c.expect("event").await;
    c.ack_event(&ev, 1).await;
    let e = &ev["payload"]["event"];
    assert_eq!(e["session_id"], sys.to_string());
    assert_eq!(e["type"], "system");
    assert_eq!(e["author"], json!({"kind": "system"}));
    assert_eq!(
        e["payload"],
        json!({"text": "before hello", "code": "worker_log", "level": "error"})
    );

    for _ in 0..5 {
        tx.send(line("warn", "hub connection lost"));
    }
    let ev = c.expect("event").await;
    c.ack_event(&ev, 2).await;
    assert_eq!(
        ev["payload"]["event"]["payload"]["text"],
        "hub connection lost"
    );
    let ev = c.expect("event").await;
    c.ack_event(&ev, 3).await;
    assert_eq!(
        ev["payload"]["event"]["payload"],
        json!({"text": "пропущено 4", "code": "worker_log", "level": "warn"})
    );
    assert!(c
        .maybe("event", Duration::from_millis(1500))
        .await
        .is_none());
    w.stop().await;
}
