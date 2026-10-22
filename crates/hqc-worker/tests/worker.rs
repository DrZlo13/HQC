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
    assert_eq!(
        p["capabilities"]["workspace_path"],
        env.workspace().display().to_string()
    );
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
        ("workspace_path", json!(env.root().display().to_string())),
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
