//! T3 acceptance (docs/ITERATION-1.md): a fake hub on tokio-tungstenite, the fake adapter,
//! no network beyond loopback and no `claude`.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use hqc_proto::event::EventKind;
use hqc_worker::adapter::fake::{agent_event, agent_message, FakeAdapters, FakeCall};
use hqc_worker::adapter::AdapterEvent;
use hqc_worker::connection::Status;
use hqc_worker::outbox::Outbox;
use serde_json::{json, Value};
use uuid::Uuid;

/// Next stored event frame (not `agent.delta`); `session.state`, acks etc. are skipped.
async fn stored_event(c: &mut HubConn) -> Value {
    loop {
        let f = c.expect("event").await;
        if f["payload"]["event"]["type"] != "agent.delta" {
            return f;
        }
        c.skipped.push(f);
    }
}

async fn start_session(c: &mut HubConn, env: &Env, sid: Uuid) {
    let id = c.frame("session.start", env.start_payload(sid)).await;
    let ack = c.expect("ack").await;
    assert_eq!(ack["id"], id.as_str(), "session.start must be acked: {ack}");
    assert_eq!(ack["payload"], json!({}));
}

async fn wait_outbox(env: &Env, what: &str, pred: impl Fn(&Outbox) -> bool) {
    let ob = Outbox::open(&env.outbox()).unwrap();
    let deadline = Instant::now() + WAIT;
    while !pred(&ob) {
        assert!(
            Instant::now() < deadline,
            "outbox: timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn input(sid: Uuid, eid: Uuid, text: &str) -> Value {
    json!({"session_id": sid, "event_id": eid, "text": text, "artifact_ids": [], "peer_prefix": []})
}

// 1. enroll stores the token; run sends hello with the bearer token and gets hello.ack;
//    SIGTERM stops it cleanly; the token never shows up in the output.
#[tokio::test]
async fn enroll_then_run_says_hello() {
    let env = Env::new();
    let polls = Arc::new(AtomicUsize::new(0));
    let p = polls.clone();
    let http = http_server(move |path, body| match path {
        "/worker/enroll" => {
            assert_eq!(body["protocol_version"], 1);
            assert!(body["name"].is_string());
            (
                200,
                json!({"code": "ABCD-1234", "poll_token": "pt-1", "poll_interval_s": 0,
                       "expires_at": chrono::Utc::now() + chrono::Duration::minutes(10)}),
            )
        }
        "/worker/enroll/poll" => {
            assert_eq!(body, json!({"poll_token": "pt-1"}));
            if p.fetch_add(1, Ordering::SeqCst) == 0 {
                (200, json!({"status": "pending"}))
            } else {
                (
                    200,
                    json!({"status": "confirmed", "token": TOKEN, "worker_id": Uuid::nil()}),
                )
            }
        }
        _ => (404, json!({})),
    })
    .await;

    let home = env.dir.path().join("home");
    let cfg_path = home.join("worker.toml");
    let cmd = |args: &[&str]| {
        let mut c = tokio::process::Command::new(binary());
        c.args(args)
            .env("HQC_HOME", &home)
            .env("HQC_TOKEN_STORE", "file")
            .env("HQC_WORKER_CONFIG", &cfg_path)
            .env("RUST_LOG", "trace")
            .kill_on_drop(true);
        c
    };

    let out = cmd(&["enroll", &format!("http://{http}")])
        .output()
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "enroll failed: {stdout}\n{stderr}");
    assert!(stdout.contains("ABCD-1234"), "code is printed: {stdout}");
    assert!(
        stdout.contains(&format!("http://{http}/#/enroll?code=ABCD-1234")),
        "link to the enroll screen is printed: {stdout}"
    );
    assert!(
        !stdout.contains(TOKEN) && !stderr.contains(TOKEN),
        "token leaked"
    );
    assert_eq!(polls.load(Ordering::SeqCst), 2);
    let token_file = home.join("token");
    assert_eq!(std::fs::read_to_string(&token_file).unwrap(), TOKEN);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&token_file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let mut hub = FakeHub::start().await;
    write(&cfg_path, &env.config_toml(&hub.url()));
    let child = cmd(&["run"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut c = hub.accept().await;
    assert_eq!(c.auth.as_deref(), Some(format!("Bearer {TOKEN}").as_str()));
    assert_eq!(c.path, "/worker/ws");
    let hello = c.handshake().await;
    let p = &hello["payload"];
    assert_eq!(p["protocol_version"], 1);
    assert_eq!(p["environments"], json!(["default"]));
    assert_eq!(
        p["allowed_roots"],
        json!([env.root().display().to_string()])
    );
    assert_eq!(p["allow_foreign_sessions"], false);
    assert!(
        p["capabilities"].get("claude_code").is_none(),
        "no claude binary configured: {p}"
    );
    // Workspaces are hub objects since T14: the worker announces only allowed_roots.
    assert!(p["capabilities"].get("workspace_path").is_none(), "{p}");
    assert!(p["boot_id"].as_str().unwrap().parse::<Uuid>().is_ok());
    // Give the worker a moment to log the ack, then stop it like launchd would.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let pid = child.id().unwrap().to_string();
    let st = std::process::Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .unwrap();
    assert!(st.success());
    let out = tokio::time::timeout(WAIT, child.wait_with_output())
        .await
        .expect("worker did not exit on SIGTERM")
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "run exited with {}: {stderr}",
        out.status
    );
    assert!(stderr.contains("connected to hub"), "{stderr}");
    let leaks: Vec<&str> = stderr.lines().filter(|l| l.contains(TOKEN)).collect();
    assert!(leaks.is_empty(), "token leaked into logs: {leaks:#?}");
}

// 1. An incompatible protocol version is logged and retried after the version interval;
//    the worker keeps running and connects once the hub accepts it.
#[tokio::test]
async fn unsupported_version_is_retried_not_fatal() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let mut w = start_worker(&env, &hub.url(), &adapters);

    let mut at = vec![];
    for _ in 0..2 {
        let mut c = hub.accept().await;
        at.push(Instant::now());
        let hello = c.recv().await;
        assert_eq!(hello["type"], "hello");
        c.send(json!({
            "id": hello["id"], "type": "error",
            "payload": {"code": "protocol_version_unsupported", "message": "too old",
                        "retryable": false, "details": {"min_version": 2}}
        }))
        .await;
        let s = w
            .wait_status(|s| matches!(s, Status::UpdateNeeded { .. }))
            .await;
        assert_eq!(
            s,
            Status::UpdateNeeded {
                min_version: Some(2)
            }
        );
        assert!(!w.task.is_finished(), "worker must not exit");
    }
    let gap = at[1] - at[0];
    assert!(
        gap >= fast_conn().version_retry,
        "retried after {gap:?}, before the version interval"
    );

    let mut c = hub.accept().await;
    c.handshake().await;
    w.wait_status(|s| matches!(s, Status::Connected { .. }))
        .await;
    w.stop().await;
}

// 2. Adapter events reach the hub; unacked ones are resent in order after a reconnect and
//    after a worker restart; `agent.delta` never enters the outbox.
#[tokio::test]
async fn outbox_survives_reconnect_and_restart() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();

    let mut c = hub.accept().await;
    let boot = c.handshake().await["payload"]["boot_id"].clone();
    start_session(&mut c, &env, sid).await;

    let evs: Vec<_> = (0..4)
        .map(|i| agent_message(sid, &format!("m{i}")))
        .collect();
    let delta = agent_event(sid, EventKind::AgentDelta { text: "par".into() });
    adapters.emit(sid, AdapterEvent::Event(delta.clone())).await;
    for e in &evs[..3] {
        adapters.emit(sid, AdapterEvent::Event(e.clone())).await;
    }
    let mut frames = vec![];
    for _ in 0..3 {
        frames.push(stored_event(&mut c).await);
    }
    let got: Vec<Uuid> = frames.iter().map(event_id).collect();
    let want: Vec<Uuid> = evs[..3].iter().map(|e| e.event_id).collect();
    assert_eq!(got, want);
    // A live delta is sent directly; it is not ordered with stored events.
    let is_delta = |f: &Value| f["payload"]["event"]["type"] == "agent.delta";
    if !c.skipped.iter().any(is_delta) {
        let f = c.expect("event").await;
        assert!(is_delta(&f), "{f}");
    }
    c.ack_event(&frames[0], 1).await;
    let e0 = evs[0].event_id;
    wait_outbox(&env, "ack of m0", |ob| !ob.contains(e0).unwrap()).await;

    // Drop the socket; an event emitted meanwhile is queued behind the unacked ones.
    c.close().await;
    adapters
        .emit(sid, AdapterEvent::Event(evs[3].clone()))
        .await;
    let e3 = evs[3].event_id;
    wait_outbox(&env, "m3", |ob| ob.contains(e3).unwrap()).await;
    let ob = Outbox::open(&env.outbox()).unwrap();
    assert!(
        !ob.contains(delta.event_id).unwrap(),
        "agent.delta is not stored"
    );

    let mut c2 = hub.accept().await;
    let hello = c2.handshake().await;
    assert_eq!(
        hello["payload"]["boot_id"], boot,
        "a reconnect keeps boot_id"
    );
    let want: Vec<Uuid> = evs[1..].iter().map(|e| e.event_id).collect();
    let mut got = vec![];
    for _ in 0..3 {
        got.push(event_id(&stored_event(&mut c2).await));
    }
    assert_eq!(got, want, "resent in the same order after reconnect");

    // Restart the worker process without acking: the outbox is on disk.
    w.stop().await;
    assert_eq!(ob.after(0).unwrap().len(), 3);
    let adapters2 = FakeAdapters::new();
    let w2 = start_worker(&env, &hub.url(), &adapters2);
    let mut c3 = hub.accept().await;
    let hello = c3.handshake().await;
    assert_ne!(
        hello["payload"]["boot_id"], boot,
        "a restart has a new boot_id"
    );
    let mut got = vec![];
    for i in 0..3 {
        let f = stored_event(&mut c3).await;
        c3.ack_event(&f, 2 + i).await;
        got.push(event_id(&f));
    }
    assert_eq!(got, want, "resent in the same order after restart");
    wait_outbox(&env, "all acked", |ob| ob.is_empty().unwrap()).await;
    w2.stop().await;
}

// T10. An event the hub answers with a retryable error is resent after each reconnect, but
//      only until the tenth such error: then it is dropped from the outbox.
#[tokio::test]
async fn event_failing_retryably_is_dropped_after_ten_attempts() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();

    let mut first = hub.accept().await;
    first.handshake().await;
    start_session(&mut first, &env, sid).await;
    let ev = agent_message(sid, "binary");
    adapters.emit(sid, AdapterEvent::Event(ev.clone())).await;
    let mut first = Some(first);
    for attempt in 1..=hqc_worker::connection::MAX_EVENT_ATTEMPTS {
        let mut c = match first.take() {
            Some(c) => c,
            None => {
                let mut c = hub.accept().await;
                c.handshake().await;
                c
            }
        };
        let f = stored_event(&mut c).await;
        assert_eq!(event_id(&f), ev.event_id, "attempt {attempt}");
        c.send(json!({"id": f["id"], "type": "error", "payload": {
            "code": "internal", "message": "event not stored", "retryable": true}}))
            .await;
        if attempt < hqc_worker::connection::MAX_EVENT_ATTEMPTS {
            // Still waiting in the outbox after this error.
            let ob = Outbox::open(&env.outbox()).unwrap();
            assert!(ob.contains(ev.event_id).unwrap(), "attempt {attempt}");
        }
        c.close().await;
    }
    wait_outbox(&env, "the event dropped", |ob| {
        !ob.contains(ev.event_id).unwrap()
    })
    .await;
    let mut c = hub.accept().await;
    c.handshake().await;
    assert!(c.maybe("event", Duration::from_millis(300)).await.is_none());
    w.stop().await;
}

// 3. An input arriving during a turn waits for the turn to end and then goes to the
//    adapter; each ack comes only after the adapter confirms the input.
#[tokio::test]
async fn input_queue_and_ack_after_adapter() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;
    let states: Vec<&Value> = c
        .skipped
        .iter()
        .filter(|f| f["type"] == "session.state")
        .map(|f| &f["payload"]["process_state"])
        .collect();
    assert_eq!(states, [&json!("starting"), &json!("running")]);

    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let in_a = |c: &[FakeCall]| {
        c.contains(&FakeCall::Input {
            session_id: sid,
            event_id: a,
        })
    };
    let in_b = |c: &[FakeCall]| {
        c.contains(&FakeCall::Input {
            session_id: sid,
            event_id: b,
        })
    };

    let fa = c.frame("session.input", input(sid, a, "A")).await;
    adapters.wait_for("input A", in_a).await;
    assert!(
        c.maybe("ack", Duration::from_millis(300)).await.is_none(),
        "no ack before the adapter confirms"
    );

    let fb = c.frame("session.input", input(sid, b, "B")).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!in_b(&adapters.calls()), "B must wait for the turn of A");

    adapters.accept_input(sid, a).await;
    let ack = c.expect("ack").await;
    assert_eq!(ack["id"], fa.as_str());
    assert_eq!(ack["payload"], json!({"event_id": a}));
    assert!(c.maybe("ack", Duration::from_millis(200)).await.is_none());

    // The hub resends A (as after a reconnect): acked again, not run again.
    let fa2 = c.frame("session.input", input(sid, a, "A")).await;
    let ack = c.expect("ack").await;
    assert_eq!(ack["id"], fa2.as_str());
    assert_eq!(ack["payload"], json!({"event_id": a}));

    adapters.end_turn(sid).await;
    let calls = adapters.wait_for("input B", in_b).await;
    let inputs: Vec<_> = calls
        .iter()
        .filter(|c| matches!(c, FakeCall::Input { .. }))
        .collect();
    assert_eq!(inputs.len(), 2, "A once, then B: {calls:?}");
    assert!(c.maybe("ack", Duration::from_millis(200)).await.is_none());
    adapters.accept_input(sid, b).await;
    let ack = c.expect("ack").await;
    assert_eq!(ack["id"], fb.as_str());
    assert_eq!(ack["payload"], json!({"event_id": b}));

    // stop interrupts the turn; kill ends the process.
    let fs = c.frame("session.stop", json!({"session_id": sid})).await;
    assert_eq!(c.expect("ack").await["id"], fs.as_str());
    let fk = c.frame("session.kill", json!({"session_id": sid})).await;
    assert_eq!(c.expect("ack").await["id"], fk.as_str());
    let calls = adapters.calls();
    assert!(calls.contains(&FakeCall::Stop(sid)) && calls.contains(&FakeCall::Kill(sid)));
    w.stop().await;
}

// 4. session.start that disagrees with the local config is rejected and nothing starts.
#[tokio::test]
async fn start_outside_config_is_rejected() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();

    let outside = tempfile::tempdir().unwrap();
    let bad: Vec<(&str, Value)> = vec![
        (
            "workspace_path",
            json!(outside.path().display().to_string()),
        ),
        (
            "workspace_path",
            json!(env.root().join("missing").display().to_string()),
        ),
        ("workspace_path", json!("root/ws")),
        ("workspace_path", Value::Null),
        ("cwd", json!("../..")),
        ("environment", json!("infra")),
        ("owner_is_worker_owner", json!(false)),
    ];
    for (field, value) in bad {
        let mut p = env.start_payload(sid);
        p[field] = value.clone();
        let id = c.frame("session.start", p).await;
        let err = c.expect("error").await;
        assert_eq!(err["id"], id.as_str());
        assert_eq!(err["payload"]["code"], "invalid_frame", "{field}={value}");
        assert_eq!(err["payload"]["retryable"], false);
    }
    // Inputs for the rejected session find nothing to run.
    let id = c
        .frame("session.input", input(sid, Uuid::now_v7(), "hi"))
        .await;
    let err = c.expect("error").await;
    assert_eq!(err["id"], id.as_str());
    assert_eq!(err["payload"]["code"], "unknown_session");
    assert_eq!(adapters.created(), 0, "no adapter was created");
    assert!(adapters.calls().is_empty());

    // An unknown frame type is an invalid frame too (SPEC §5.2).
    c.send(json!({"id": "x-1", "type": "session.steer", "payload": {}}))
        .await;
    let err = c.expect("error").await;
    assert_eq!(err["id"], "x-1");
    assert_eq!(err["payload"]["code"], "invalid_frame");
    w.stop().await;
}

// SPEC §5.3: `mode: new` starts the adapter, `mode: resume` resumes it; `session.start` for
// a live process is acked without a new spawn.
#[tokio::test]
async fn start_mode_selects_start_or_resume() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;

    let new = Uuid::now_v7();
    start_session(&mut c, &env, new).await;
    let resumed = Uuid::now_v7();
    let mut p = env.start_payload(resumed);
    p["mode"] = json!("resume");
    let id = c.frame("session.start", p.clone()).await;
    assert_eq!(c.expect("ack").await["id"], id.as_str());
    let id = c.frame("session.start", p).await;
    assert_eq!(c.expect("ack").await["id"], id.as_str());
    assert_eq!(
        adapters.calls(),
        vec![FakeCall::Start(new), FakeCall::Resume(resumed)]
    );
    w.stop().await;
}

// T13 (deploy findings 1, 2, 4): a store that cannot keep the token stops enroll before the
// hub is asked; `--name` names the worker; without RUST_LOG the log is `info`, no HTTP debug.
#[tokio::test]
async fn enroll_probes_the_store_and_takes_a_name() {
    let dir = tempfile::tempdir().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let names = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (h, n) = (hits.clone(), names.clone());
    let http = http_server(move |path, body| {
        h.fetch_add(1, Ordering::SeqCst);
        match path {
            "/worker/enroll" => {
                n.lock().unwrap().push(body["name"].clone());
                (
                    200,
                    json!({"code": "ABCD-1234", "poll_token": "pt-1", "poll_interval_s": 0,
                           "expires_at": chrono::Utc::now() + chrono::Duration::minutes(10)}),
                )
            }
            "/worker/enroll/poll" => (
                200,
                json!({"status": "confirmed", "token": TOKEN, "worker_id": Uuid::nil()}),
            ),
            _ => (404, json!({})),
        }
    })
    .await;
    let enroll = |home: std::path::PathBuf, extra: &[&str]| {
        let mut c = tokio::process::Command::new(binary());
        c.arg("enroll")
            .arg(format!("http://{http}"))
            .args(extra)
            .env("HQC_HOME", home)
            .env("HQC_TOKEN_STORE", "file")
            .env_remove("RUST_LOG")
            .kill_on_drop(true);
        c
    };

    let blocked = dir.path().join("blocked");
    write(&blocked, "");
    let out = enroll(blocked, &[]).output().await.unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("not writable"), "{stderr}");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "hub not asked");

    let home = dir.path().join("home");
    let out = enroll(home.clone(), &["--name", "bench-1"])
        .output()
        .await
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert_eq!(*names.lock().unwrap(), vec![json!("bench-1")]);
    assert!(
        !stderr.contains("DEBUG") && !stderr.contains("TRACE"),
        "{stderr}"
    );
    assert_eq!(std::fs::read_to_string(home.join("token")).unwrap(), TOKEN);
}

// T9: enrollment talks https (plain http only to loopback or with --insecure-http) and does
// not follow redirects, which would re-send the poll secret elsewhere.
#[tokio::test]
async fn enroll_refuses_plain_http_and_redirects() {
    let dir = tempfile::tempdir().unwrap();
    let store = hqc_worker::token::TokenStore::File(dir.path().join("token"));
    let mut out = Vec::new();
    let e = hqc_worker::enroll::enroll("http://hub.example.com", "w", false, &store, &mut out)
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("plain http"), "{e:#}");

    let elsewhere_hit = Arc::new(AtomicUsize::new(0));
    let hit = elsewhere_hit.clone();
    let elsewhere = http_server(move |_, _| {
        hit.fetch_add(1, Ordering::SeqCst);
        (200, json!({}))
    })
    .await;
    let hub = http_server(move |path, _| {
        (
            307,
            json!({"location": format!("http://{elsewhere}{path}")}),
        )
    })
    .await;
    let e = hqc_worker::enroll::enroll(&format!("http://{hub}"), "w", false, &store, &mut out)
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("307"), "{e:#}");
    assert_eq!(
        elsewhere_hit.load(Ordering::SeqCst),
        0,
        "redirect not followed"
    );
    assert!(store.load().unwrap().is_none());
}

// T19: confirmed as the same machine as an existing worker, enroll says which one it
// replaced; the worker id is the old one's.
#[tokio::test]
async fn enroll_reports_the_replaced_worker() {
    let dir = tempfile::tempdir().unwrap();
    let store = hqc_worker::token::TokenStore::File(dir.path().join("token"));
    let old = Uuid::now_v7();
    let sent = Arc::new(std::sync::Mutex::new(Value::Null));
    let seen = sent.clone();
    let hub = http_server(move |path, body| match path {
        "/worker/enroll" => (200, {
            *seen.lock().unwrap() = body;
            json!({"code": "ABCD-1234", "poll_token": "pt-1", "poll_interval_s": 0,
                       "expires_at": chrono::Utc::now() + chrono::Duration::minutes(10)})
        }),
        "/worker/enroll/poll" => (
            200,
            json!({"status": "confirmed", "token": TOKEN, "worker_id": old,
                   "replaced": "home-mac-old"}),
        ),
        _ => (404, json!({})),
    })
    .await;
    let mut out = Vec::new();
    let id = hqc_worker::enroll::enroll(&format!("http://{hub}"), "w", false, &store, &mut out)
        .await
        .unwrap();
    assert_eq!(id, old);
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains(&format!("Enrolled as worker {old} (replaced home-mac-old)")),
        "{out}"
    );
    assert!(!out.contains(TOKEN));
    // T21: the machine fingerprint, a sha256 in hex, never the id itself.
    let m = sent.lock().unwrap()["machine_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        m.len() == 64 && m.bytes().all(|b| b.is_ascii_hexdigit()),
        "{m}"
    );
}

// T9: at most `max_sessions` agents run; one more start is retryable `agent_start_failed`.
#[tokio::test]
async fn max_sessions_limits_running_agents() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let mut config = env.config(&hub.url());
    config.max_sessions = 1;
    let w = start_worker_with(&env, config, &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;

    let first = Uuid::now_v7();
    start_session(&mut c, &env, first).await;
    let second = Uuid::now_v7();
    let id = c.frame("session.start", env.start_payload(second)).await;
    let err = c.expect("error").await;
    assert_eq!(err["id"], id.as_str());
    assert_eq!(err["payload"]["code"], "agent_start_failed");
    assert_eq!(err["payload"]["retryable"], true);

    let id = c.frame("session.kill", json!({"session_id": first})).await;
    loop {
        let f = c.expect("ack").await;
        if f["id"] == id.as_str() {
            break;
        }
    }
    start_session(&mut c, &env, second).await;
    assert_eq!(
        adapters
            .calls()
            .iter()
            .filter(|c| matches!(c, FakeCall::Start(_)))
            .count(),
        2
    );
    w.stop().await;
}

// A failed spawn is `agent_start_failed`, retryable, for the start and for queued inputs.
#[tokio::test]
async fn failed_start_is_retryable() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_fail_start(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();

    let id = c.frame("session.start", env.start_payload(sid)).await;
    let err = c.expect("error").await;
    assert_eq!(err["id"], id.as_str());
    assert_eq!(err["payload"]["code"], "agent_start_failed");
    assert_eq!(err["payload"]["retryable"], true);

    let id = c
        .frame("session.input", input(sid, Uuid::now_v7(), "hi"))
        .await;
    let err = c.expect("error").await;
    assert_eq!(err["id"], id.as_str());
    assert_eq!(err["payload"]["code"], "agent_start_failed");
    assert_eq!(
        adapters.calls(),
        [FakeCall::Start(sid), FakeCall::Start(sid)],
        "each attempt starts afresh"
    );

    adapters.set_fail_start(false);
    let eid = Uuid::now_v7();
    c.frame("session.input", input(sid, eid, "hi")).await;
    adapters
        .wait_for("input", |c| {
            c.contains(&FakeCall::Input {
                session_id: sid,
                event_id: eid,
            })
        })
        .await;
    w.stop().await;
}

// T13 (SPEC §5.4): the runtime's title goes to the hub in `session.state` and stays in the
// later ones.
#[tokio::test]
async fn runtime_title_goes_in_session_state() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;
    adapters
        .emit(sid, AdapterEvent::Title("Fix the build".into()))
        .await;
    loop {
        let st = c.expect("session.state").await;
        if st["payload"]["title"] == "Fix the build" {
            break;
        }
    }
    w.shutdown.cancel();
    loop {
        let st = c.expect("session.state").await;
        if st["payload"]["process_state"] == "stopped" {
            assert_eq!(st["payload"]["title"], "Fix the build");
            break;
        }
    }
}

// Graceful shutdown: agents are killed, `session.state stopped` is sent, the outbox is
// flushed while the hub acks.
#[tokio::test]
async fn shutdown_kills_agents_and_flushes() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;
    adapters
        .emit(sid, AdapterEvent::Event(agent_message(sid, "bye")))
        .await;
    let ev = stored_event(&mut c).await;

    w.shutdown.cancel();
    loop {
        let st = c.expect("session.state").await;
        if st["payload"]["process_state"] == "stopped" {
            assert_eq!(st["payload"]["locked_by"], "none");
            break;
        }
    }
    c.ack_event(&ev, 7).await;
    tokio::time::timeout(WAIT, w.task)
        .await
        .expect("worker did not stop")
        .unwrap()
        .unwrap();
    assert!(adapters.calls().contains(&FakeCall::Kill(sid)));
    assert!(Outbox::open(&env.outbox()).unwrap().is_empty().unwrap());
}

// idle_timeout (SPEC §17.3): an idle agent is killed, the next input resumes it.
#[tokio::test]
async fn idle_agent_is_stopped_and_resumed_by_input() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let mut cfg = env.config(&hub.url());
    cfg.idle_timeout = Some(Duration::from_millis(300));
    let w = start_worker_with(&env, cfg, &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;
    let has_input = |eid: Uuid| {
        move |c: &[FakeCall]| {
            c.contains(&FakeCall::Input {
                session_id: sid,
                event_id: eid,
            })
        }
    };

    let a = Uuid::now_v7();
    c.frame("session.input", input(sid, a, "A")).await;
    adapters.wait_for("input A", has_input(a)).await;
    adapters.accept_input(sid, a).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !adapters.calls().contains(&FakeCall::Kill(sid)),
        "never idle during a turn"
    );
    adapters.end_turn(sid).await;
    adapters
        .wait_for("idle kill", |c| c.contains(&FakeCall::Kill(sid)))
        .await;
    loop {
        let st = c.expect("session.state").await;
        if st["payload"]["process_state"] == "stopped" {
            break;
        }
    }

    let b = Uuid::now_v7();
    c.frame("session.input", input(sid, b, "B")).await;
    let calls = adapters.wait_for("input B", has_input(b)).await;
    assert_eq!(calls[calls.len() - 2], FakeCall::Resume(sid), "{calls:?}");
    w.stop().await;
}

// T14: the hub asks the worker about folders (SPEC §5.3): `fs.stat` before a workspace is
// added, `fs.list_dirs` for the folder picker; answers stay inside allowed_roots.
#[tokio::test]
async fn fs_requests_answer_inside_allowed_roots() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let root = env.root().display().to_string();
    let ws = env.workspace().display().to_string();

    let path = format!("{root}/ws/sub/..");
    let id = c
        .frame("fs.stat", json!({"request_id": "rq-1", "path": path}))
        .await;
    let r = c.expect("result").await;
    assert_eq!(r["id"], id.as_str());
    assert_eq!(
        r["payload"],
        json!({"request_id": "rq-1", "exists": true, "is_dir": true, "canonical": ws,
               "inside_allowed_roots": true})
    );
    let outside = tempfile::tempdir().unwrap();
    let outside = outside.path().display().to_string();
    c.frame("fs.stat", json!({"request_id": "rq-2", "path": outside}))
        .await;
    let r = c.expect("result").await;
    assert_eq!(r["payload"]["inside_allowed_roots"], false);
    assert_eq!(r["payload"]["exists"], false, "nothing about paths outside");

    c.frame("fs.list_dirs", json!({"request_id": "rq-3", "path": null}))
        .await;
    let r = c.expect("result").await;
    assert_eq!(
        r["payload"]["entries"],
        json!([{"name": "root", "path": root}])
    );
    c.frame("fs.list_dirs", json!({"request_id": "rq-4", "path": root}))
        .await;
    let r = c.expect("result").await;
    assert_eq!(r["payload"]["entries"], json!([{"name": "ws", "path": ws}]));
    let id = c
        .frame(
            "fs.list_dirs",
            json!({"request_id": "rq-5", "path": outside}),
        )
        .await;
    let e = c.expect("error").await;
    assert_eq!(e["id"], id.as_str());
    assert_eq!(e["payload"]["code"], "invalid_frame");
    assert!(adapters.calls().is_empty(), "no agent involved");
    w.stop().await;
}

// T16 (SPEC §5.3, §12): the artifacts of an input are downloaded with the worker token into
// `.hqc/inbox/<event_id>/` under sanitized, unique names, `.hqc/` goes to the git exclude,
// and the adapter gets the text with the block of paths (fixtures/inbox).
#[tokio::test]
async fn inbox_scenario() {
    let fx = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/inbox");
    let read = |n: &str| std::fs::read_to_string(fx.join(n)).unwrap();
    let env = Env::new();
    std::fs::create_dir_all(env.workspace().join(".git/info")).unwrap();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let arts: Vec<Value> = serde_json::from_str(&read("artifacts.json")).unwrap();
    for a in &arts {
        hub.artifacts.insert(
            a["id"].as_str().unwrap().parse().unwrap(),
            FakeArtifact {
                name: a["name"].as_str().unwrap().into(),
                kind: a["kind"].as_str().unwrap().into(),
                mime: a["mime"].as_str().unwrap().into(),
                bytes: std::fs::read(fx.join("artifacts").join(a["file"].as_str().unwrap()))
                    .unwrap(),
            },
        );
    }
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    start_session(&mut c, &env, sid).await;

    let mut payload: Value = serde_json::from_str(&read("session.input.json")).unwrap();
    payload["session_id"] = json!(sid);
    let eid: Uuid = payload["event_id"].as_str().unwrap().parse().unwrap();
    c.frame("session.input", payload).await;
    adapters
        .wait_for("the input", |c| {
            c.contains(&FakeCall::Input {
                session_id: sid,
                event_id: eid,
            })
        })
        .await;

    let (got, files) = adapters.inputs().pop().unwrap();
    assert_eq!(got.text, read("expected-text.txt"));
    assert_eq!(got.artifact_ids.len(), 4);
    let inbox = env.workspace().join(".hqc/inbox").join(eid.to_string());
    let names: Vec<_> = files.iter().map(|f| f.rel.clone()).collect();
    assert_eq!(
        names,
        ["screen.png", "fix_1.patch", "notes.txt", "notes-2.txt"]
            .map(|n| format!(".hqc/inbox/{eid}/{n}"))
    );
    for (f, a) in files.iter().zip(&arts) {
        assert_eq!(f.path, inbox.join(f.path.file_name().unwrap()));
        let want = std::fs::read(fx.join("artifacts").join(a["file"].as_str().unwrap())).unwrap();
        assert_eq!(std::fs::read(&f.path).unwrap(), want, "{}", f.rel);
        assert_eq!(f.size, want.len() as u64);
    }
    assert_eq!(files[0].kind, hqc_proto::event::ArtifactKind::Image);
    assert_eq!(files[0].mime, "image/png");
    // Only the files: no temporary leftovers.
    assert_eq!(std::fs::read_dir(&inbox).unwrap().count(), 4);
    let exclude = std::fs::read_to_string(env.workspace().join(".git/info/exclude")).unwrap();
    assert!(exclude.lines().any(|l| l == ".hqc/"), "{exclude}");
    // Every download carried the worker token.
    let reqs = hub.artifacts.requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 4);
    let bearer = format!("Bearer {TOKEN}");
    assert!(reqs
        .iter()
        .all(|(p, a)| p.starts_with("/worker/artifacts/") && a.as_deref() == Some(&bearer[..])));

    // Without artifacts nothing is touched and the text is as sent.
    adapters.accept_input(sid, eid).await;
    adapters.end_turn(sid).await;
    let plain = Uuid::now_v7();
    c.frame("session.input", input(sid, plain, "just text"))
        .await;
    adapters
        .wait_for("plain input", |c| {
            c.contains(&FakeCall::Input {
                session_id: sid,
                event_id: plain,
            })
        })
        .await;
    let (got, files) = adapters.inputs().pop().unwrap();
    assert_eq!((got.text.as_str(), files.len()), ("just text", 0));
    assert!(!env
        .workspace()
        .join(".hqc/inbox")
        .join(plain.to_string())
        .exists());

    // T21: the working directory is remembered for the sweep at start, and archiving the
    // session removes its inbox folder, nothing else.
    let dirs =
        std::fs::read_to_string(env.known_paths().with_file_name("inbox_dirs.json")).unwrap();
    assert!(dirs.contains(&*env.workspace().to_string_lossy()), "{dirs}");
    let id = c
        .frame(
            "session.cleanup",
            json!({"session_id": sid, "workspace_path": env.workspace(), "cwd": ".",
                   "event_ids": [eid, plain]}),
        )
        .await;
    while c.expect("ack").await["id"] != id.as_str() {}
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while inbox.exists() {
        assert!(tokio::time::Instant::now() < deadline, "inbox not removed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(env.workspace().join(".hqc/inbox").is_dir());
    assert!(env.workspace().join(".git/info/exclude").exists());
    w.stop().await;
}

// An artifact the hub does not give fails the input without retry; a `.hqc` that points
// outside the workspace is refused and nothing is written there.
#[tokio::test]
async fn inbox_refusals() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    start_session(&mut c, &env, sid).await;

    let missing = Uuid::now_v7();
    let mut p = input(sid, Uuid::now_v7(), "see");
    p["artifact_ids"] = json!([missing]);
    let id = c.frame("session.input", p).await;
    let err = c.expect("error").await;
    assert_eq!(err["id"], id.as_str());
    assert_eq!(err["payload"]["retryable"], false, "{err}");
    assert!(err["payload"]["message"]
        .as_str()
        .unwrap()
        .contains(&missing.to_string()));

    let aid = Uuid::now_v7();
    hub.artifacts.insert(
        aid,
        FakeArtifact {
            name: "x.txt".into(),
            kind: "text".into(),
            mime: "text/plain".into(),
            bytes: b"x".to_vec(),
        },
    );
    let outside = env.dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    // The failed input left no files behind.
    let ws_hqc = env.workspace().join(".hqc");
    assert_eq!(std::fs::read_dir(ws_hqc.join("inbox")).unwrap().count(), 0);
    std::fs::remove_dir_all(&ws_hqc).unwrap();
    std::os::unix::fs::symlink(&outside, env.workspace().join(".hqc")).unwrap();
    let mut p = input(sid, Uuid::now_v7(), "see");
    p["artifact_ids"] = json!([aid]);
    let id = c.frame("session.input", p).await;
    let err = c.expect("error").await;
    assert_eq!(err["id"], id.as_str());
    assert!(
        err["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("is a symlink"),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    assert!(!adapters
        .calls()
        .iter()
        .any(|c| matches!(c, FakeCall::Input { .. })));
    w.stop().await;
}

// T24: `title.request` runs a one-off `claude -p` outside every session and answers with
// `result {title}`; a failing run is an `error`, and nothing reaches the session adapters.
#[tokio::test]
async fn title_request_runs_claude_once() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let bin = env.dir.path().join("claude");
    let result = json!({"type": "result", "subtype": "success", "is_error": false,
                        "result": "«Починка\nсборки»."});
    write(
        &bin,
        &format!("#!/bin/sh\ncat > /dev/null\nprintf '%s' '{result}'\n"),
    );
    let exec = std::os::unix::fs::PermissionsExt::from_mode(0o755);
    std::fs::set_permissions(&bin, exec).unwrap();
    let mut config = env.config(&hub.url());
    config.claude_bin = Some(bin.clone());
    config.scratch_root = env.dir.path().join("home/scratch");
    let w = start_worker_with(&env, config, &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    let id = c
        .frame(
            "title.request",
            json!({"request_id": "h-5", "session_id": sid, "user_text": "почини сборку",
                   "agent_text": "готово"}),
        )
        .await;
    let r = c.expect("result").await;
    assert_eq!(r["id"], id.as_str());
    assert_eq!(
        r["payload"],
        json!({"request_id": "h-5", "title": "Починка сборки"})
    );
    let titles = env.dir.path().join("home/titles");
    assert_eq!(
        std::fs::read_dir(&titles).unwrap().count(),
        0,
        "run folder removed"
    );

    write(&bin, "#!/bin/sh\necho 'Not logged in' >&2\nexit 1\n");
    let id = c
        .frame(
            "title.request",
            json!({"request_id": "h-6", "session_id": sid, "user_text": "a", "agent_text": ""}),
        )
        .await;
    let e = c.expect("error").await;
    assert_eq!(e["id"], id.as_str());
    let msg = e["payload"]["message"].as_str().unwrap();
    assert!(msg.contains("Not logged in"), "{e}");
    assert!(adapters.calls().is_empty(), "{:?}", adapters.calls());
    w.stop().await;
}
