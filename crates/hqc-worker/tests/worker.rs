//! T3 acceptance (docs/ITERATION-1.md): a fake hub on tokio-tungstenite, the fake adapter,
//! no network beyond loopback and no `claude`.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use hqc_proto::event::{EventKind, LiveKind, LiveSubagent};
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
    assert_eq!(p["environments"], json!([]), "default is implied");
    assert_eq!(p["env_bindings"], json!([]));
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
    let delta = agent_event(
        sid,
        EventKind::AgentDelta {
            text: "par".into(),
            reset: false,
        },
    );
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
    assert_eq!(ob.after(0, i64::MAX).unwrap().len(), 3);
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

// SPEC §5.2: a large outbox goes out with at most `MAX_IN_FLIGHT` entries un-acked; the rest
// follow as the acks come, in order.
#[tokio::test]
async fn outbox_keeps_at_most_max_in_flight_unacked() {
    use hqc_worker::connection::MAX_IN_FLIGHT;
    let env = Env::new();
    let sid = Uuid::now_v7();
    let ob = Outbox::open(&env.outbox()).unwrap();
    let want: Vec<Uuid> = (0..2 * MAX_IN_FLIGHT + 10)
        .map(|i| {
            let ev = agent_message(sid, &format!("m{i}"));
            ob.push(
                &ev.session_id.to_string(),
                ev.event_id,
                &serde_json::to_value(&ev).unwrap(),
            )
            .unwrap();
            ev.event_id
        })
        .collect();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;

    let mut frames = vec![];
    while frames.len() < MAX_IN_FLIGHT {
        frames.push(c.expect("event").await);
    }
    assert!(
        c.try_recv(Duration::from_millis(500)).await.is_none(),
        "sent before any ack"
    );
    for (i, f) in frames.iter().enumerate() {
        c.ack_event(f, i as u64 + 1).await;
    }
    while frames.len() < want.len() {
        let f = c.expect("event").await;
        c.ack_event(&f, frames.len() as u64 + 1).await;
        frames.push(f);
    }
    let got: Vec<Uuid> = frames.iter().map(event_id).collect();
    assert_eq!(got, want, "sent in order");
    wait_outbox(&env, "all acked", |ob| ob.is_empty().unwrap()).await;
    w.stop().await;
}

// SPEC §5.2: a full window that no answer frees within `dead_after` drops the connection,
// also while the hub keeps pinging; the next connection sends the outbox again.
#[tokio::test]
async fn outbox_window_reconnects_when_no_entry_is_answered() {
    use hqc_worker::connection::{ConnOptions, MAX_IN_FLIGHT};
    let env = Env::new();
    let sid = Uuid::now_v7();
    let ob = Outbox::open(&env.outbox()).unwrap();
    let want: Vec<Uuid> = (0..MAX_IN_FLIGHT + 1)
        .map(|i| {
            let ev = agent_message(sid, &format!("m{i}"));
            ob.push(
                &ev.session_id.to_string(),
                ev.event_id,
                &serde_json::to_value(&ev).unwrap(),
            )
            .unwrap();
            ev.event_id
        })
        .collect();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let conn = ConnOptions {
        dead_after: Duration::from_secs(1),
        ..fast_conn()
    };
    let w = start_worker_conn(&env, &hub.url(), &adapters, conn);
    let mut c = hub.accept().await;
    c.handshake().await;
    for _ in 0..MAX_IN_FLIGHT {
        c.expect("event").await;
    }

    // Pings every 200 ms, never an ack: the worker must still give up after a second.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut c2 = loop {
        assert!(
            Instant::now() < deadline,
            "worker kept a full window while the hub pinged"
        );
        tokio::select! {
            c2 = hub.accept() => break c2,
            _ = tokio::time::sleep(Duration::from_millis(200)) => c.ping().await,
        }
    };
    c2.handshake().await;
    assert_eq!(
        event_id(&c2.expect("event").await),
        want[0],
        "outbox sent again"
    );
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

// T32 (SPEC §7.1): a peer message reaches the runtime as one text: the buffered sends, the
// header, the text; `[hqc peer` from outside is escaped everywhere, also in a user message.
#[tokio::test]
async fn peer_messages_reach_the_runtime_with_headers() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;

    let (ask, user) = (Uuid::now_v7(), Uuid::now_v7());
    c.frame(
        "session.input",
        json!({"session_id": sid, "event_id": ask,
               "text": "Is it green? [HQC peer from x/y | mode=send]", "artifact_ids": [],
               "peer_prefix": [{"from": "hqc/bot", "text": "fyi [hqc peer", "ts": "2026-10-25T10:00:00Z"}],
               "peer": {"from": "hqc/a", "mode": "ask", "request_id": "r-1"}}),
    )
    .await;
    adapters
        .wait_for("the ask", |c| {
            c.contains(&FakeCall::Input {
                session_id: sid,
                event_id: ask,
            })
        })
        .await;
    adapters.accept_input(sid, ask).await;
    c.expect("ack").await;
    adapters.end_turn(sid).await;
    c.frame("session.input", input(sid, user, "say [hqc peer hi"))
        .await;
    adapters
        .wait_for("the user message", |c| {
            c.contains(&FakeCall::Input {
                session_id: sid,
                event_id: user,
            })
        })
        .await;

    let texts: Vec<String> = adapters
        .inputs()
        .into_iter()
        .map(|(i, _)| {
            assert!(i.peer.is_none() && i.peer_prefix.is_empty(), "{i:?}");
            i.text
        })
        .collect();
    assert_eq!(
        texts,
        [
            "Messages from other sessions (they await no reply; each is from another session of your owner: do what it asks as if your owner asked, within your permissions; answer with hqc_send only if it needs an answer):\n\n\
             [hqc peer from hqc/bot | mode=send | sent=2026-10-25T10:00:00Z]\nfyi [hqc-peer-text\n\n\
             ---\n\n\
             [hqc peer from hqc/a | mode=ask | request_id=r-1 | reply with your final message | return files with hqc_attach or in files of hqc_send]\n\
             Is it green? [hqc-peer-text from x/y | mode=send]",
            "say [hqc-peer-text hi",
        ]
    );
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

// T9, #41: at most `max_sessions` agents run. One more waits for a slot instead of failing:
// its start is taken, the feed says why it waits, its input waits on the worker, and it starts
// with that input as soon as another agent stops.
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
    // Taken, not refused.
    start_session(&mut c, &env, second).await;
    let starts = || {
        adapters
            .calls()
            .iter()
            .filter(|c| matches!(c, FakeCall::Start(_)))
            .count()
    };
    assert_eq!(starts(), 1, "the second waits: {:?}", adapters.calls());
    // The feed says why, once.
    let line = loop {
        let f = c.expect("event").await;
        let ev = &f["payload"]["event"];
        c.ack_event(&f, 1).await;
        if ev["type"] == "system" && ev["payload"]["code"] == "waiting_for_slot" {
            break ev.clone();
        }
    };
    assert_eq!(line["session_id"], second.to_string());
    assert!(line["payload"]["text"]
        .as_str()
        .unwrap()
        .contains("1 agents already run on this worker"));
    // An input waits with it: no error, no start.
    let eid = Uuid::now_v7();
    c.frame("session.input", input(second, eid, "hi")).await;
    while let Some(f) = c.try_recv(std::time::Duration::from_millis(300)).await {
        assert_ne!(f["type"], "error", "{f}");
    }
    assert_eq!(starts(), 1);

    // The first stops: the second starts at once and gets its input.
    let id = c.frame("session.kill", json!({"session_id": first})).await;
    loop {
        let f = c.expect("ack").await;
        if f["id"] == id.as_str() {
            break;
        }
    }
    let deadline = Instant::now() + WAIT;
    while !adapters.calls().iter().any(|c| matches!(c, FakeCall::Input { session_id, event_id } if *session_id == second && *event_id == eid)) {
        assert!(Instant::now() < deadline, "{:?}", adapters.calls());
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(adapters.calls().contains(&FakeCall::Start(second)));
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

// T27 (SPEC §5.4): the runtime's slash commands go in `session.state`, once per change, and
// stay in the later ones like the title; since SPEC 0.162 with what the runtime says of each
// in `command_info`.
#[tokio::test]
async fn runtime_commands_go_in_session_state() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;
    let list = vec!["compact".to_string(), "review".to_string()];
    let compact = hqc_proto::frame::SlashCommand {
        name: "compact".into(),
        description: Some("Compact the conversation".into()),
        argument_hint: Some("<instructions>".into()),
    };
    let review = hqc_proto::frame::SlashCommand {
        name: "review".into(),
        description: None,
        argument_hint: None,
    };
    let info = serde_json::json!([
        {"name": "compact", "description": "Compact the conversation", "argument_hint": "<instructions>"},
        {"name": "review"}
    ]);
    adapters
        .emit(sid, AdapterEvent::Commands(vec![compact, review]))
        .await;
    loop {
        let st = c.expect("session.state").await;
        if st["payload"]["commands"] == serde_json::json!(list) {
            assert_eq!(st["payload"]["command_info"], info);
            break;
        }
    }
    w.shutdown.cancel();
    loop {
        let st = c.expect("session.state").await;
        if st["payload"]["process_state"] == "stopped" {
            assert_eq!(st["payload"]["commands"], serde_json::json!(list));
            assert_eq!(st["payload"]["command_info"], info);
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
        ends(&adapters.calls(), sid) == 0,
        "never idle during a turn"
    );
    adapters.end_turn(sid).await;
    adapters
        .wait_for("idle close", |c| c.contains(&FakeCall::Close(sid)))
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

// idle_timeout (SPEC §17.3): an agent whose subagent still runs after the turn is not idle;
// it is stopped once the last one ends.
#[tokio::test]
async fn idle_timeout_waits_for_subagents() {
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
    adapters.emit(sid, AdapterEvent::Subagents(1)).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        ends(&adapters.calls(), sid) == 0,
        "never idle while a subagent runs"
    );
    adapters.emit(sid, AdapterEvent::Subagents(0)).await;
    adapters.wait_for("idle kill", |c| ends(c, sid) > 0).await;
    w.stop().await;
}

// idle_timeout (SPEC §17.3): an agent while the runtime lists background tasks (a subagent's
// command moved to the background) is not idle; it is stopped once there are none.
#[tokio::test]
async fn idle_timeout_waits_for_background_tasks() {
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
    adapters.emit(sid, AdapterEvent::BackgroundTasks(1)).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        ends(&adapters.calls(), sid) == 0,
        "never idle while the runtime lists a background task"
    );
    adapters.emit(sid, AdapterEvent::BackgroundTasks(0)).await;
    adapters.wait_for("idle kill", |c| ends(c, sid) > 0).await;
    w.stop().await;
}

// idle_timeout (SPEC §17.3): a turn the runtime starts by itself (Claude Code: on the end of a
// background task) is not idleness, however long it is silent (an approval it waits for); the
// agent is stopped once that turn ends.
#[tokio::test]
async fn idle_timeout_waits_for_a_turn_the_runtime_starts_by_itself() {
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
    adapters.emit(sid, AdapterEvent::BackgroundTasks(1)).await;
    // The task ends, the runtime runs its own turn on it and then says nothing.
    adapters.set_own_turn(true);
    adapters.emit(sid, AdapterEvent::BackgroundTasks(0)).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        ends(&adapters.calls(), sid),
        0,
        "never idle during the runtime's own turn"
    );
    adapters.set_own_turn(false);
    adapters.end_turn(sid).await;
    adapters.wait_for("idle kill", |c| ends(c, sid) > 0).await;
    w.stop().await;
}

// idle_timeout (SPEC §17.3): an agent that armed a wakeup for itself is not idle before it is
// due; once it fired or was stopped, the usual timeout counts.
#[tokio::test]
async fn idle_timeout_waits_for_an_armed_wakeup() {
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
    let due = chrono::Utc::now() + chrono::Duration::seconds(60);
    adapters.emit(sid, AdapterEvent::Wakeup(Some(due))).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        ends(&adapters.calls(), sid) == 0,
        "never idle before the wakeup is due"
    );
    adapters.emit(sid, AdapterEvent::Wakeup(None)).await;
    adapters.wait_for("idle kill", |c| ends(c, sid) > 0).await;
    w.stop().await;
}

// After `/clear` the session goes on in another transcript (SPEC §8.1): the hub hears of it, and
// the worker's next start of the agent resumes that one.
#[tokio::test]
async fn a_new_runtime_session_id_is_the_one_resumed() {
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
    let cleared = Uuid::new_v4().to_string();
    adapters
        .emit(sid, AdapterEvent::RuntimeSessionId(cleared.clone()))
        .await;
    loop {
        let st = c.expect("session.state").await;
        if st["payload"]["runtime_session_id"] == cleared.as_str() {
            break;
        }
    }
    adapters.wait_for("idle kill", |c| ends(c, sid) > 0).await;
    let b = Uuid::now_v7();
    c.frame("session.input", input(sid, b, "B")).await;
    adapters
        .wait_for("resume", |c| {
            c.iter().filter(|c| **c == FakeCall::Resume(sid)).count() == 1
        })
        .await;
    let specs = adapters.start_specs();
    assert_eq!(
        specs.last().unwrap().runtime_session_id.as_deref(),
        Some(cleared.as_str())
    );
    w.stop().await;
}

// idle_timeout (SPEC §17.3): holders of other transcripts (Claude Code in VS Code or a terminal
// on the same machine) changing all the time do not keep an idle agent alive.
#[tokio::test]
async fn idle_timeout_ignores_holders_of_other_transcripts() {
    let env = Env::with_mirror();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let mut cfg = env.config(&hub.url());
    cfg.idle_timeout = Some(Duration::from_millis(500));
    let w = start_worker_with(&env, cfg, &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;
    // A new holder every 100 ms (the mirror's poll here), each of another transcript: an entry
    // of Claude Code's registry `<config>/sessions` for a live process, this test's.
    let (projects, _) = env.mirror.clone().unwrap();
    let dir = projects.with_file_name("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let started = Instant::now();
    let mut n = 0;
    while ends(&adapters.calls(), sid) == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "an idle agent must be stopped while holders of other transcripts change"
        );
        n += 1;
        let entry = json!({"pid": std::process::id(), "sessionId": Uuid::new_v4(),
            "cwd": env.workspace(), "entrypoint": "claude-vscode", "kind": "interactive",
            "status": "busy", "updatedAt": 1792779848554u64});
        std::fs::write(dir.join(format!("{n}.json")), entry.to_string()).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    w.stop().await;
}

// idle_timeout (SPEC §17.3): the background tasks of a killed process go with it; the process
// resumed after it is stopped once idle.
#[tokio::test]
async fn idle_timeout_forgets_the_background_tasks_of_a_killed_process() {
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
    // Events are handled in order: once the title is in `session.state`, so is the count.
    adapters.emit(sid, AdapterEvent::BackgroundTasks(1)).await;
    adapters
        .emit(sid, AdapterEvent::Title("Fix the build".into()))
        .await;
    state_where(&mut c, sid, |st| st["payload"]["title"] == "Fix the build").await;
    c.frame("session.kill", json!({"session_id": sid})).await;
    adapters
        .wait_for("kill", |c| c.contains(&FakeCall::Kill(sid)))
        .await;
    let e = Uuid::now_v7();
    c.frame("session.input", input(sid, e, "go")).await;
    adapters
        .wait_for("input", |c| c.contains(&input_call(sid, e)))
        .await;
    adapters.accept_input(sid, e).await;
    adapters.end_turn(sid).await;
    adapters
        .wait_for("idle kill of the resumed process", |c| ends(c, sid) == 2)
        .await;
    w.stop().await;
}

// `session.kill` closes a process with nothing running through its stdin (above) and kills
// one that works: a subagent, a background task, a turn the runtime started by itself.
#[tokio::test]
async fn session_kill_kills_a_process_that_works() {
    for case in ["subagent", "background task", "own turn"] {
        let env = Env::new();
        let adapters = FakeAdapters::new();
        let mut hub = FakeHub::start().await;
        let w = start_worker(&env, &hub.url(), &adapters);
        let sid = Uuid::now_v7();
        let mut c = hub.accept().await;
        c.handshake().await;
        start_session(&mut c, &env, sid).await;
        match case {
            "subagent" => {
                adapters.emit(sid, AdapterEvent::Subagents(1)).await;
                state_where(&mut c, sid, |st| st["payload"]["subagents"] == 1).await;
            }
            "background task" => {
                adapters.emit(sid, AdapterEvent::BackgroundTasks(1)).await;
                state_where(&mut c, sid, |st| st["payload"]["background_tasks"] == 1).await;
            }
            _ => adapters.set_own_turn(true),
        }
        c.frame("session.kill", json!({"session_id": sid})).await;
        let calls = adapters
            .wait_for("kill", |c| c.contains(&FakeCall::Kill(sid)))
            .await;
        assert!(!calls.contains(&FakeCall::Close(sid)), "{case}: {calls:?}");
        w.stop().await;
    }
}

// idle_timeout (SPEC §17.3): counts that come after the process was stopped (lines of a killed
// process read late) are dropped: no `subagents` in `session.state`, and the process resumed
// later is stopped once idle.
#[tokio::test]
async fn counts_after_the_process_stopped_are_dropped() {
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
    c.frame("session.kill", json!({"session_id": sid})).await;
    adapters
        .wait_for("close of a quiet agent", |c| {
            c.contains(&FakeCall::Close(sid))
        })
        .await;
    adapters.emit(sid, AdapterEvent::BackgroundTasks(1)).await;
    adapters.emit(sid, AdapterEvent::Subagents(2)).await;
    adapters
        .emit(sid, AdapterEvent::Title("after".into()))
        .await;
    let st = state_where(&mut c, sid, |st| st["payload"]["title"] == "after").await;
    assert!(st["payload"].get("subagents").is_none(), "{st}");
    let e = Uuid::now_v7();
    c.frame("session.input", input(sid, e, "go")).await;
    adapters
        .wait_for("input", |c| c.contains(&input_call(sid, e)))
        .await;
    adapters.accept_input(sid, e).await;
    adapters.end_turn(sid).await;
    adapters
        .wait_for("idle kill of the resumed process", |c| ends(c, sid) == 2)
        .await;
    w.stop().await;
}

// idle_timeout (SPEC §17.3): the worker's reconnects to the hub do not keep an idle agent alive.
#[tokio::test]
async fn idle_timeout_ignores_reconnects() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let mut cfg = env.config(&hub.url());
    cfg.idle_timeout = Some(Duration::from_millis(500));
    let w = start_worker_with(&env, cfg, &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;
    let started = Instant::now();
    while ends(&adapters.calls(), sid) == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "an idle agent must be stopped while the worker reconnects"
        );
        c.close().await;
        c = hub.accept().await;
        c.handshake().await;
    }
    w.stop().await;
}

/// A `session.state` of `sid` whose `permission_mode` is `mode`, skipped or next.
async fn state_with_mode(c: &mut HubConn, sid: Uuid, mode: &str) -> Value {
    let hit = |st: &Value| {
        st["type"] == "session.state"
            && st["payload"]["session_id"] == json!(sid)
            && st["payload"]["permission_mode"] == mode
    };
    if let Some(i) = c.skipped.iter().position(hit) {
        return c.skipped.remove(i);
    }
    loop {
        let st = c.expect("session.state").await;
        if hit(&st) {
            return st;
        }
    }
}

// T38: the mode of `session.start` reaches the adapter's start; `session.permission_mode`
// switches the running agent and the next start; the mode the runtime confirms goes in every
// `session.state`.
#[tokio::test]
async fn permission_mode_is_started_switched_and_reported() {
    use hqc_proto::frame::PermissionMode::{Ask, Auto};
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    let mut start = env.start_payload(sid);
    start["permission_mode"] = json!("auto");
    c.frame("session.start", start).await;
    c.expect("ack").await;
    state_with_mode(&mut c, sid, "auto").await;
    assert_eq!(adapters.start_modes(), [(sid, Auto)]);

    // A running agent switches at once; the runtime's confirmation is what is reported.
    let id = c
        .frame(
            "session.permission_mode",
            json!({"session_id": sid, "permission_mode": "ask"}),
        )
        .await;
    let ack = c.expect("ack").await;
    assert_eq!(ack["id"], id.as_str());
    adapters
        .wait_for("switch", |c| {
            c.contains(&FakeCall::PermissionMode(sid, Ask))
        })
        .await;
    adapters.emit(sid, AdapterEvent::PermissionMode(Ask)).await;
    state_with_mode(&mut c, sid, "ask").await;

    // The next process starts in the switched mode, and the mode stays in `session.state`.
    c.frame("session.kill", json!({"session_id": sid})).await;
    loop {
        let st = c.expect("session.state").await;
        if st["payload"]["process_state"] == "stopped" {
            assert_eq!(st["payload"]["permission_mode"], "ask");
            break;
        }
    }
    let e = Uuid::now_v7();
    c.frame("session.input", input(sid, e, "go")).await;
    adapters
        .wait_for("input", |c| {
            c.contains(&FakeCall::Input {
                session_id: sid,
                event_id: e,
            })
        })
        .await;
    assert_eq!(adapters.start_modes(), [(sid, Auto), (sid, Ask)]);

    // An unknown session: nothing to switch, the next `session.start` carries the mode.
    let other = Uuid::now_v7();
    let id = c
        .frame(
            "session.permission_mode",
            json!({"session_id": other, "permission_mode": "auto"}),
        )
        .await;
    loop {
        let ack = c.expect("ack").await;
        if ack["id"] == id.as_str() {
            assert_eq!(ack["payload"], json!({}));
            break;
        }
    }
    w.stop().await;
}

// T38: with `[permissions] allow_auto = false` the worker runs `ask` whatever the hub asks
// for, at the start and on a switch, and reports `ask`.
#[tokio::test]
async fn allow_auto_false_runs_ask() {
    use hqc_proto::frame::PermissionMode::Ask;
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let cfg = hqc_worker::config::Config::parse(&format!(
        "{}[permissions]\nallow_auto = false\n",
        env.config_toml(&hub.url())
    ))
    .unwrap();
    let w = start_worker_with(&env, cfg, &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    let mut start = env.start_payload(sid);
    start["permission_mode"] = json!("auto");
    c.frame("session.start", start).await;
    c.expect("ack").await;
    state_with_mode(&mut c, sid, "ask").await;
    assert_eq!(adapters.start_modes(), [(sid, Ask)]);
    c.frame(
        "session.permission_mode",
        json!({"session_id": sid, "permission_mode": "auto"}),
    )
    .await;
    c.expect("ack").await;
    assert!(
        !adapters
            .calls()
            .iter()
            .any(|c| matches!(c, FakeCall::PermissionMode(..))),
        "no switch to auto"
    );
    w.stop().await;
}

// SPEC 0.129: Allow all needs `[permissions] allow_all`, off by default: without it a session
// the hub asks to run in it runs in the highest mode the worker allows — Auto, or Ask with
// `allow_auto = false` — at the start and on a switch, and reports that mode.
#[tokio::test]
async fn allow_all_needs_its_own_setting() {
    use hqc_proto::frame::PermissionMode::{AllowAll, Ask, Auto};
    for (extra, want, reported) in [
        ("", Auto, "auto"),
        ("[permissions]\nallow_auto = false\n", Ask, "ask"),
    ] {
        let env = Env::new();
        let adapters = FakeAdapters::new();
        let mut hub = FakeHub::start().await;
        let cfg =
            hqc_worker::config::Config::parse(&format!("{}{extra}", env.config_toml(&hub.url())))
                .unwrap();
        let w = start_worker_with(&env, cfg, &adapters);
        let sid = Uuid::now_v7();
        let mut c = hub.accept().await;
        c.handshake().await;
        let mut start = env.start_payload(sid);
        start["permission_mode"] = json!("allow_all");
        c.frame("session.start", start).await;
        c.expect("ack").await;
        state_with_mode(&mut c, sid, reported).await;
        assert_eq!(adapters.start_modes(), [(sid, want)], "{extra:?}");
        c.frame(
            "session.permission_mode",
            json!({"session_id": sid, "permission_mode": "allow_all"}),
        )
        .await;
        c.expect("ack").await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !adapters
                .calls()
                .iter()
                .any(|c| matches!(c, FakeCall::PermissionMode(_, AllowAll))),
            "{extra:?}: no switch to allow_all"
        );
        w.stop().await;
    }
}

/// A worker whose owner allowed Allow all (`[permissions] allow_all = true`, SPEC 0.129).
fn allow_all_worker(env: &Env, hub_url: &str, adapters: &FakeAdapters) -> RunningWorker {
    let cfg = hqc_worker::config::Config::parse(&format!(
        "{}[permissions]\nallow_all = true\n",
        env.config_toml(hub_url)
    ))
    .unwrap();
    start_worker_with(env, cfg, adapters)
}

/// An input with `permission_cap` (T39).
fn capped_input(sid: Uuid, eid: Uuid, cap: &str) -> Value {
    let mut v = input(sid, eid, "from another session");
    v["permission_cap"] = json!(cap);
    v
}

/// The next `session.state` of `sid` that `pred` accepts.
async fn state_where(c: &mut HubConn, sid: Uuid, pred: impl Fn(&Value) -> bool) -> Value {
    let hit = |st: &Value| {
        st["type"] == "session.state" && st["payload"]["session_id"] == json!(sid) && pred(st)
    };
    if let Some(i) = c.skipped.iter().position(hit) {
        return c.skipped.remove(i);
    }
    loop {
        let st = c.expect("session.state").await;
        if hit(&st) {
            return st;
        }
    }
}

/// A started session in `mode`, its first `session.state` read.
async fn started_in(c: &mut HubConn, env: &Env, sid: Uuid, mode: &str) {
    let mut start = env.start_payload(sid);
    start["permission_mode"] = json!(mode);
    c.frame("session.start", start).await;
    c.expect("ack").await;
    state_with_mode(c, sid, mode).await;
}

fn index(calls: &[FakeCall], call: &FakeCall) -> usize {
    calls
        .iter()
        .position(|c| c == call)
        .unwrap_or_else(|| panic!("no {call:?} in {calls:?}"))
}

fn switches(calls: &[FakeCall]) -> Vec<PermissionMode> {
    calls
        .iter()
        .filter_map(|c| match c {
            FakeCall::PermissionMode(_, m) => Some(*m),
            _ => None,
        })
        .collect()
}

use hqc_proto::frame::PermissionMode;

// T39: a capped input in an auto session waits for the runtime's `ask`, runs in it, and the
// session's own mode comes back before the user's input queued after it.
#[tokio::test]
async fn a_capped_turn_runs_in_ask_and_the_mode_comes_back() {
    use PermissionMode::{Ask, Auto};
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    started_in(&mut c, &env, sid, "auto").await;

    let (e1, e2) = (Uuid::now_v7(), Uuid::now_v7());
    c.frame("session.input", capped_input(sid, e1, "ask")).await;
    adapters
        .wait_for("switch down", |c| {
            c.contains(&FakeCall::PermissionMode(sid, Ask))
        })
        .await;
    // The user's input queues meanwhile; nothing reaches the runtime before it confirms.
    c.frame("session.input", input(sid, e2, "mine")).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !adapters
            .calls()
            .iter()
            .any(|c| matches!(c, FakeCall::Input { .. })),
        "{:?}",
        adapters.calls()
    );
    adapters.emit(sid, AdapterEvent::PermissionMode(Ask)).await;
    let first = FakeCall::Input {
        session_id: sid,
        event_id: e1,
    };
    adapters
        .wait_for("capped input", |c| c.contains(&first))
        .await;
    let st = state_where(&mut c, sid, |st| st["payload"]["capped_input"] == json!(e1)).await;
    assert_eq!(st["payload"]["permission_mode"], "ask");
    adapters.accept_input(sid, e1).await;
    adapters.end_turn(sid).await;

    // Back to auto first, then the user's input.
    let second = FakeCall::Input {
        session_id: sid,
        event_id: e2,
    };
    let calls = adapters
        .wait_for("the user's input", |c| c.contains(&second))
        .await;
    assert_eq!(switches(&calls), [Ask, Auto]);
    assert!(index(&calls, &FakeCall::PermissionMode(sid, Auto)) < index(&calls, &second));
    adapters.emit(sid, AdapterEvent::PermissionMode(Auto)).await;
    let st = state_with_mode(&mut c, sid, "auto").await;
    assert!(st["payload"].get("capped_input").is_none(), "{st}");
    w.stop().await;
}

// T39: a switch the user asks for during a capped turn waits for its end; meanwhile the
// session reports the cap that holds it down.
#[tokio::test]
async fn a_switch_during_a_capped_turn_waits_for_its_end() {
    use PermissionMode::Auto;
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    started_in(&mut c, &env, sid, "ask").await;

    // In ask already: the capped input goes at once, no switch.
    let e1 = Uuid::now_v7();
    c.frame("session.input", capped_input(sid, e1, "ask")).await;
    adapters
        .wait_for("capped input", |c| {
            c.contains(&FakeCall::Input {
                session_id: sid,
                event_id: e1,
            })
        })
        .await;
    adapters.accept_input(sid, e1).await;
    let id = c
        .frame(
            "session.permission_mode",
            json!({"session_id": sid, "permission_mode": "auto"}),
        )
        .await;
    loop {
        let ack = c.expect("ack").await;
        if ack["id"] == id.as_str() {
            break;
        }
    }
    let st = state_where(&mut c, sid, |st| st["payload"]["capped_input"] == json!(e1)).await;
    assert_eq!(st["payload"]["permission_mode"], "ask");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(switches(&adapters.calls()), [], "not raised mid-turn");

    adapters.end_turn(sid).await;
    let calls = adapters
        .wait_for("the user's mode", |c| {
            c.contains(&FakeCall::PermissionMode(sid, Auto))
        })
        .await;
    assert_eq!(switches(&calls), [Auto]);
    w.stop().await;
}

// T39: the cap only lowers: `auto` on an input changes nothing, in an ask session or an auto
// one, and the worker's `allow_auto` is not touched by it.
#[tokio::test]
async fn the_cap_never_raises() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    for mode in ["ask", "auto"] {
        let sid = Uuid::now_v7();
        started_in(&mut c, &env, sid, mode).await;
        let e = Uuid::now_v7();
        c.frame("session.input", capped_input(sid, e, "auto")).await;
        adapters
            .wait_for("input", |c| {
                c.contains(&FakeCall::Input {
                    session_id: sid,
                    event_id: e,
                })
            })
            .await;
        adapters.accept_input(sid, e).await;
        adapters.end_turn(sid).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(switches(&adapters.calls()), []);
    w.stop().await;
}

// SPEC 0.129: an `auto` cap holds an Allow all session down to Auto: the runtime is switched
// to Auto and confirms it before the input, the session reports the cap meanwhile, a switch the
// user asks for goes no higher than Auto until the turn ends, and Allow all comes back after
// it. A stopped agent starts in Auto for such an input. On a worker that allows Allow all.
#[tokio::test]
async fn an_auto_cap_holds_allow_all_down_to_auto() {
    use PermissionMode::{AllowAll, Ask, Auto};
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = allow_all_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    started_in(&mut c, &env, sid, "allow_all").await;
    assert_eq!(adapters.start_modes(), [(sid, AllowAll)]);

    let e1 = Uuid::now_v7();
    c.frame("session.input", capped_input(sid, e1, "auto"))
        .await;
    adapters
        .wait_for("switch down", |c| {
            c.contains(&FakeCall::PermissionMode(sid, Auto))
        })
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let first = FakeCall::Input {
        session_id: sid,
        event_id: e1,
    };
    assert!(
        !adapters.calls().contains(&first),
        "before the runtime confirms Auto"
    );
    adapters.emit(sid, AdapterEvent::PermissionMode(Auto)).await;
    adapters
        .wait_for("capped input", |c| c.contains(&first))
        .await;
    let st = state_where(&mut c, sid, |st| st["payload"]["capped_input"] == json!(e1)).await;
    assert_eq!(st["payload"]["permission_mode"], "auto");
    adapters.accept_input(sid, e1).await;

    // A switch to Ask mid-turn goes at once; back to Allow all goes no higher than Auto.
    for mode in ["ask", "allow_all"] {
        let id = c
            .frame(
                "session.permission_mode",
                json!({"session_id": sid, "permission_mode": mode}),
            )
            .await;
        loop {
            let ack = c.expect("ack").await;
            if ack["id"] == id.as_str() {
                break;
            }
        }
    }
    adapters
        .wait_for("switches mid-turn", |c| switches(c).len() == 3)
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(switches(&adapters.calls()), [Auto, Ask, Auto]);

    adapters.end_turn(sid).await;
    adapters
        .wait_for("own mode back", |c| {
            c.contains(&FakeCall::PermissionMode(sid, AllowAll))
        })
        .await;
    assert_eq!(switches(&adapters.calls()), [Auto, Ask, Auto, AllowAll]);

    // Stopped: the next process starts in Auto for an `auto`-capped input, and an `ask` cap
    // starts it in Ask.
    for (cap, want) in [("auto", Auto), ("ask", Ask)] {
        c.frame("session.kill", json!({"session_id": sid})).await;
        state_where(&mut c, sid, |st| {
            st["payload"]["process_state"] == "stopped"
        })
        .await;
        let e = Uuid::now_v7();
        c.frame("session.input", capped_input(sid, e, cap)).await;
        adapters
            .wait_for("input", |c| {
                c.contains(&FakeCall::Input {
                    session_id: sid,
                    event_id: e,
                })
            })
            .await;
        assert_eq!(adapters.start_modes().last(), Some(&(sid, want)), "{cap}");
        adapters.accept_input(sid, e).await;
        adapters.end_turn(sid).await;
    }
    w.stop().await;
}

// SPEC 0.129: an `allow_all` cap (a chain of Allow all sessions) holds nothing down, on a
// worker that allows Allow all.
#[tokio::test]
async fn an_allow_all_cap_changes_nothing() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = allow_all_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    for mode in ["ask", "auto", "allow_all"] {
        let sid = Uuid::now_v7();
        started_in(&mut c, &env, sid, mode).await;
        let e = Uuid::now_v7();
        c.frame("session.input", capped_input(sid, e, "allow_all"))
            .await;
        adapters
            .wait_for("input", |c| {
                c.contains(&FakeCall::Input {
                    session_id: sid,
                    event_id: e,
                })
            })
            .await;
        adapters.accept_input(sid, e).await;
        adapters.end_turn(sid).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(switches(&adapters.calls()), []);
    w.stop().await;
}

// SPEC 0.129: an Auto session whose runtime lowered itself (`plan` reads as Ask) takes an
// input capped to Auto at once: nothing is asked of the runtime, so nothing would confirm it,
// and waiting would end in a restart after MODE_SWITCH_TIMEOUT.
#[tokio::test]
async fn a_runtime_below_the_cap_takes_the_capped_input_at_once() {
    use PermissionMode::{Ask, Auto};
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    started_in(&mut c, &env, sid, "auto").await;
    adapters.emit(sid, AdapterEvent::PermissionMode(Ask)).await;
    state_with_mode(&mut c, sid, "ask").await;

    let e = Uuid::now_v7();
    c.frame("session.input", capped_input(sid, e, "auto")).await;
    let calls = adapters
        .wait_for("capped input", |c| {
            c.contains(&FakeCall::Input {
                session_id: sid,
                event_id: e,
            })
        })
        .await;
    assert_eq!(switches(&calls), []);
    assert!(ends(&calls, sid) == 0, "{calls:?}");
    assert_eq!(adapters.start_modes(), [(sid, Auto)]);
    adapters.accept_input(sid, e).await;
    adapters.end_turn(sid).await;
    w.stop().await;
}

// T39: a runtime that cannot switch is restarted in ask for the capped input; a process
// started again later starts in the session's own mode.
#[tokio::test]
async fn a_runtime_that_cannot_switch_is_restarted_in_ask() {
    use PermissionMode::{Ask, Auto};
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    started_in(&mut c, &env, sid, "auto").await;
    adapters.set_refuse_modes(true);

    let e1 = Uuid::now_v7();
    c.frame("session.input", capped_input(sid, e1, "ask")).await;
    let first = FakeCall::Input {
        session_id: sid,
        event_id: e1,
    };
    let calls = adapters
        .wait_for("capped input", |c| c.contains(&first))
        .await;
    assert!(index(&calls, &FakeCall::Kill(sid)) < index(&calls, &FakeCall::Resume(sid)));
    assert_eq!(adapters.start_modes(), [(sid, Auto), (sid, Ask)]);
    adapters.accept_input(sid, e1).await;
    adapters.end_turn(sid).await;

    // The process lost later starts in the session's mode for an input without a cap.
    c.frame("session.kill", json!({"session_id": sid})).await;
    adapters.wait_for("kill", |c| ends(c, sid) == 2).await;
    let e2 = Uuid::now_v7();
    c.frame("session.input", input(sid, e2, "mine")).await;
    adapters
        .wait_for("input", |c| {
            c.contains(&FakeCall::Input {
                session_id: sid,
                event_id: e2,
            })
        })
        .await;
    assert_eq!(
        adapters.start_modes(),
        [(sid, Auto), (sid, Ask), (sid, Auto)]
    );
    w.stop().await;
}

// T39: a stopped agent starts in ask for a capped input (no switch needed); a turn lost with
// its process leaves nothing behind: the next input starts the process in the session's mode.
#[tokio::test]
async fn a_stopped_agent_starts_in_ask_for_a_capped_input() {
    use PermissionMode::{Ask, Auto};
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    started_in(&mut c, &env, sid, "auto").await;
    c.frame("session.kill", json!({"session_id": sid})).await;
    adapters.wait_for("kill", |c| ends(c, sid) > 0).await;

    let e1 = Uuid::now_v7();
    c.frame("session.input", capped_input(sid, e1, "ask")).await;
    adapters
        .wait_for("capped input", |c| {
            c.contains(&FakeCall::Input {
                session_id: sid,
                event_id: e1,
            })
        })
        .await;
    assert_eq!(adapters.start_modes(), [(sid, Auto), (sid, Ask)]);
    assert_eq!(switches(&adapters.calls()), []);
    state_where(&mut c, sid, |st| st["payload"]["capped_input"] == json!(e1)).await;
    adapters.accept_input(sid, e1).await;

    // The process dies mid-turn.
    adapters
        .emit(sid, AdapterEvent::Exited { error: None })
        .await;
    let st = state_where(&mut c, sid, |st| {
        st["payload"]["process_state"] == "stopped"
    })
    .await;
    assert!(st["payload"].get("capped_input").is_none(), "{st}");
    let e2 = Uuid::now_v7();
    c.frame("session.input", input(sid, e2, "mine")).await;
    adapters
        .wait_for("input", |c| {
            c.contains(&FakeCall::Input {
                session_id: sid,
                event_id: e2,
            })
        })
        .await;
    assert_eq!(
        adapters.start_modes(),
        [(sid, Auto), (sid, Ask), (sid, Auto)]
    );
    w.stop().await;
}

// T43: `hello` reports the environments and bindings without the values of `env`; the worker
// resolves a session's environment itself (an `environment` an older hub still sends is
// ignored), and `permissions.inspect` shows the environment's rules under their own source.
#[tokio::test]
async fn environments_in_hello_and_inspect() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let ws = env.workspace().display().to_string();
    let config = hqc_worker::config::Config::parse(&format!(
        "{}[environments.infra]\nenv = {{ GITHUB_TOKEN = \"ghp_t43_secret\" }}\n\
         [environments.infra.permissions]\nunblock = [\"Bash(ssh:*)\"]\n\
         allow = [\"Bash(ssh debian@192.168.1.20:*)\"]\n\
         [[env_bindings]]\npath = {ws:?}\nenvironment = \"infra\"\n\
         [providers.zai]\nbase_url = \"https://api.z.ai/api/anthropic\"\napi_key = \"sk-zai-secret\"\n",
        env.config_toml(&hub.url())
    ))
    .unwrap();
    let w = start_worker_with(&env, config, &adapters);
    let mut c = hub.accept().await;
    let hello = c.handshake().await;
    let p = &hello["payload"];
    assert_eq!(
        p["environments"],
        json!([{"name": "infra", "permissions": {
            "unblock": ["Bash(ssh:*)"], "allow": ["Bash(ssh debian@192.168.1.20:*)"]}}])
    );
    assert_eq!(
        p["env_bindings"],
        json!([{"path": ws, "environment": "infra"}])
    );
    assert!(!hello.to_string().contains("ghp_t43_secret"));
    assert!(!hello.to_string().contains("GITHUB_TOKEN"));
    // SPEC 0.144: a provider by name only.
    assert_eq!(p["providers"], json!(["zai"]));
    assert!(!hello.to_string().contains("sk-zai-secret"));
    assert!(!hello.to_string().contains("api.z.ai"));

    let mut start = env.start_payload(Uuid::now_v7());
    start["environment"] = json!("default");
    c.frame("session.start", start).await;
    c.expect("ack").await;
    c.frame(
        "permissions.inspect",
        json!({"request_id": "i-1", "path": ws, "command": "ssh debian@192.168.1.20 true"}),
    )
    .await;
    let p = c.expect("result").await["payload"].clone();
    assert_eq!(
        p["sources"][1]["source"],
        "worker.toml [environments.infra]"
    );
    assert_eq!(p["check"]["verdict"], "allow");
    assert_eq!(p["check"]["source"], "worker.toml [environments.infra]");
    assert!(!p.to_string().contains("ghp_t43_secret"));
    w.stop().await;
}

// T48: hqc's rules of a folder come with `session.start` and `workspace.rules`;
// `permissions.inspect` shows them with the files' rules and checks a command, only for a
// workspace the worker knows inside allowed_roots.
#[tokio::test]
async fn folder_rules_and_inspect() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let ws = env.workspace().display().to_string();

    // Not known yet: refused, though inside allowed_roots.
    let id = c
        .frame(
            "permissions.inspect",
            json!({"request_id": "i-1", "path": ws}),
        )
        .await;
    let r = c.expect("error").await;
    assert_eq!(
        (r["id"].as_str(), r["payload"]["code"].as_str()),
        (Some(id.as_str()), Some("invalid_frame"))
    );

    let sid = Uuid::now_v7();
    let mut start = env.start_payload(sid);
    start["allow_rules"] = json!(["Bash(python3 *)"]);
    c.frame("session.start", start).await;
    c.expect("ack").await;
    c.frame(
        "permissions.inspect",
        json!({"request_id": "i-2", "path": ws, "command": "python3 a.py && sudo ls"}),
    )
    .await;
    let r = c.expect("result").await;
    let p = &r["payload"];
    assert_eq!(p["request_id"], "i-2");
    assert_eq!(p["hqc"], json!(["Bash(python3 *)"]));
    assert_eq!(p["sources"][0]["source"], "worker.toml");
    assert_eq!(p["sources"][2]["source"], ".claude/settings.local.json");
    assert_eq!(p["check"]["verdict"], "deny");
    assert_eq!(p["check"]["rule"], "Bash(sudo:*)");

    // A change of the list: acked, and what the worker holds from now on.
    let id = c
        .frame(
            "workspace.rules",
            json!({"workspace_path": format!("{ws}/sub/.."), "allow_rules": ["Bash(git fetch:*)", "Bash", "Edit"]}),
        )
        .await;
    let a = c.expect("ack").await;
    assert_eq!(a["id"], id.as_str());
    c.frame(
        "permissions.inspect",
        json!({"request_id": "i-3", "path": ws, "command": "git -C sub fetch"}),
    )
    .await;
    let p = c.expect("result").await["payload"].clone();
    assert_eq!(
        p["hqc"],
        json!(["Bash(git fetch:*)", "Edit"]),
        "invalid rules dropped"
    );
    assert_eq!(p["check"]["verdict"], "allow");
    assert_eq!(p["check"]["source"], "hqc");

    // Outside allowed_roots: neither rules nor inspection.
    let outside = tempfile::tempdir().unwrap();
    let outside = outside.path().display().to_string();
    c.frame(
        "workspace.rules",
        json!({"workspace_path": outside, "allow_rules": []}),
    )
    .await;
    assert_eq!(c.expect("error").await["payload"]["code"], "invalid_frame");
    c.frame(
        "permissions.inspect",
        json!({"request_id": "i-4", "path": outside}),
    )
    .await;
    assert_eq!(c.expect("error").await["payload"]["code"], "invalid_frame");
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
               "inside_allowed_roots": true, "environment": "default"})
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

// T41 (SPEC §5.3, §7.1, §12): the files of a peer message go to the inbox like a user's and
// their paths follow its text under the header; a buffered message's files go to its own
// folder and follow its text in the block. A file the hub does not give fails the input
// without retry, and nothing stays behind.
#[tokio::test]
async fn peer_files_reach_the_inbox() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let (board, notes) = (Uuid::now_v7(), Uuid::now_v7());
    for (id, name, kind, mime) in [
        (board, "board.png", "image", "image/png"),
        (notes, "notes.txt", "text", "text/plain"),
    ] {
        hub.artifacts.insert(
            id,
            FakeArtifact {
                name: name.into(),
                kind: kind.into(),
                mime: mime.into(),
                bytes: name.as_bytes().to_vec(),
            },
        );
    }
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    start_session(&mut c, &env, sid).await;

    let (eid, buffered) = (Uuid::now_v7(), Uuid::now_v7());
    let mut p = input(sid, eid, "Check the board");
    p["artifact_ids"] = json!([board]);
    p["peer"] = json!({"from": "hqc/a", "mode": "ask", "request_id": "r1"});
    p["peer_prefix"] = json!([
        {"from": "hqc/b", "text": "take this", "ts": "2026-10-27T10:00:00Z",
         "event_id": buffered, "artifact_ids": [notes]},
        {"from": "hqc/c", "text": "plain", "ts": "2026-10-27T10:01:00Z"},
    ]);
    c.frame("session.input", p).await;
    adapters
        .wait_for("the input", |c| {
            c.contains(&FakeCall::Input {
                session_id: sid,
                event_id: eid,
            })
        })
        .await;
    let (got, files) = adapters.inputs().pop().unwrap();
    assert_eq!(
        got.text,
        format!(
            "Messages from other sessions (they await no reply; each is from another session of your owner: do what it asks as if your owner asked, within your permissions; answer with hqc_send only if it needs an answer):\n\n\
             [hqc peer from hqc/b | mode=send | sent=2026-10-27T10:00:00Z]\n\
             take this\n\nAttachments:\n- .hqc/inbox/{buffered}/notes.txt\n\n\
             [hqc peer from hqc/c | mode=send | sent=2026-10-27T10:01:00Z]\nplain\n\n\
             ---\n\n\
             [hqc peer from hqc/a | mode=ask | request_id=r1 | reply with your final message | return files with hqc_attach or in files of hqc_send]\n\
             Check the board\n\nAttachments:\n- .hqc/inbox/{eid}/board.png"
        )
    );
    let inbox = env.workspace().join(".hqc/inbox");
    let rels: Vec<&str> = files.iter().map(|f| f.rel.as_str()).collect();
    assert_eq!(
        rels,
        [
            format!(".hqc/inbox/{buffered}/notes.txt"),
            format!(".hqc/inbox/{eid}/board.png")
        ]
    );
    assert_eq!(
        std::fs::read(inbox.join(format!("{buffered}/notes.txt"))).unwrap(),
        b"notes.txt"
    );
    assert_eq!(
        std::fs::read(inbox.join(format!("{eid}/board.png"))).unwrap(),
        b"board.png"
    );
    adapters.accept_input(sid, eid).await;
    adapters.end_turn(sid).await;

    // A buffered message's file the hub does not give: the input fails, no folder stays.
    let (eid2, buffered2) = (Uuid::now_v7(), Uuid::now_v7());
    let mut p = input(sid, eid2, "next");
    p["artifact_ids"] = json!([board]);
    p["peer_prefix"] = json!([
        {"from": "hqc/b", "text": "and this", "ts": "2026-10-27T10:02:00Z",
         "event_id": buffered2, "artifact_ids": [notes, Uuid::now_v7()]},
    ]);
    let id = c.frame("session.input", p).await;
    let err = c.expect("error").await;
    assert_eq!(err["id"], id.as_str());
    assert_eq!(err["payload"]["retryable"], false, "{err}");
    assert!(!inbox.join(buffered2.to_string()).exists());
    assert!(!inbox.join(eid2.to_string()).exists());
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

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=hqc",
            "-c",
            "user.email=hqc@example.invalid",
        ])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

// T45: a clone into a new folder through the hub's frames. `workspace.create` is acked at once
// and answered with `workspace.created`; the same request again gets the same outcome without
// a second clone; a folder inside a workspace the worker runs sessions in is refused;
// `workspace.inspect` and `workspace.delete` answer like `fs.*`, and the worker deletes only
// what it created.
#[tokio::test]
async fn clone_inspect_and_delete_through_frames() {
    let env = Env::new();
    let root = env.root();
    std::fs::create_dir_all(root.join("repos/src")).unwrap();
    git(&root.join("repos/src"), &["init", "-q"]);
    std::fs::write(root.join("repos/src/a.txt"), "a").unwrap();
    git(&root.join("repos/src"), &["add", "a.txt"]);
    git(&root.join("repos/src"), &["commit", "-q", "-m", "a"]);
    git(
        &root.join("repos"),
        &["clone", "-q", "--bare", "src", "origin.git"],
    );
    let url = format!("file://{}", root.join("repos/origin.git").display());

    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    // A session makes `root/ws` a workspace the worker knows.
    c.frame("session.start", env.start_payload(Uuid::now_v7()))
        .await;
    c.expect("ack").await;

    let target = root.join("c6").display().to_string();
    let create = json!({"request_id": "ws-1", "kind": "clone", "path": target, "url": url});
    for round in 0..2 {
        let id = c.frame("workspace.create", create.clone()).await;
        let ack = c.expect("ack").await;
        assert_eq!(ack["id"], id);
        let done = c.expect("workspace.created").await;
        assert_eq!(
            done["payload"],
            json!({"request_id": "ws-1", "canonical": target}),
            "round {round}"
        );
    }
    assert!(root.join("c6/a.txt").exists());
    let created = std::fs::read_to_string(env.dir.path().join("created_workspaces.json")).unwrap();
    assert!(created.contains(&target), "{created}");

    let inside = env.workspace().join("nested").display().to_string();
    c.frame(
        "workspace.create",
        json!({"request_id": "ws-2", "kind": "clone", "path": inside, "url": url}),
    )
    .await;
    c.expect("ack").await;
    let refused = c.expect("workspace.created").await["payload"].clone();
    assert!(refused["canonical"].is_null());
    assert!(
        refused["error"].as_str().unwrap().contains("overlaps"),
        "{refused}"
    );
    assert!(!env.workspace().join("nested").exists());

    std::fs::write(root.join("c6/a.txt"), "changed").unwrap();
    c.frame(
        "workspace.inspect",
        json!({"request_id": "i-1", "path": target}),
    )
    .await;
    let r = c.expect("result").await["payload"].clone();
    assert_eq!(
        r,
        json!({"request_id": "i-1", "git": true, "dirty": ["a.txt"], "dirty_count": 1, "unpushed": []})
    );

    let ws = env.workspace().display().to_string();
    c.frame("workspace.delete", json!({"request_id": "d-1", "path": ws}))
        .await;
    let e = c.expect("error").await;
    assert_eq!(e["payload"]["code"], "invalid_frame");
    assert!(env.workspace().exists());
    c.frame(
        "workspace.delete",
        json!({"request_id": "d-2", "path": target}),
    )
    .await;
    let r = c.expect("result").await["payload"].clone();
    assert_eq!(r, json!({"request_id": "d-2", "removed": true}));
    assert!(!root.join("c6").exists());
    w.stop().await;
}

// T46: a worktree of a repository the hub told about with `workspace.rules`, through the
// frames: the origin unknown before that is refused; the inspection lists the worktree's own
// branch; `workspace.delete` refuses uncommitted files until `force`, and the branch stays.
#[tokio::test]
async fn worktree_through_frames() {
    let env = Env::new();
    let root = env.root();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    let repo_s = repo.display().to_string();
    let target = root.join("repo-hqc-fix").display().to_string();

    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let create = json!({"request_id": "wt-1", "kind": "worktree", "path": target,
                        "origin": repo_s, "branch": "hqc/fix", "new_branch": true});
    c.frame("workspace.create", create).await;
    c.expect("ack").await;
    let refused = c.expect("workspace.created").await["payload"].clone();
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("not a workspace of this worker"),
        "{refused}"
    );

    // The hub sends the origin's rules first: from then on it is a workspace the worker knows.
    c.frame(
        "workspace.rules",
        json!({"workspace_path": repo_s, "allow_rules": []}),
    )
    .await;
    c.expect("ack").await;
    let create = json!({"request_id": "wt-2", "kind": "worktree", "path": target,
                        "origin": repo_s, "branch": "hqc/fix", "new_branch": true});
    c.frame("workspace.create", create).await;
    c.expect("ack").await;
    let done = c.expect("workspace.created").await;
    assert_eq!(
        done["payload"],
        json!({"request_id": "wt-2", "canonical": target})
    );
    let wt = root.join("repo-hqc-fix");
    git(&wt, &["commit", "-q", "--allow-empty", "-m", "fix"]);
    std::fs::write(wt.join("notes.txt"), "x").unwrap();

    c.frame(
        "workspace.inspect",
        json!({"request_id": "i-1", "path": target}),
    )
    .await;
    let r = c.expect("result").await["payload"].clone();
    // No remote: both commits of the branch are on none; `main` is not the worktree's.
    assert_eq!(
        r,
        json!({"request_id": "i-1", "git": true, "dirty": ["notes.txt"], "dirty_count": 1,
               "unpushed": [{"branch": "hqc/fix", "commits": 2}]})
    );
    c.frame(
        "workspace.delete",
        json!({"request_id": "d-1", "path": target}),
    )
    .await;
    let e = c.expect("error").await;
    assert!(
        e["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("modified or untracked"),
        "{e}"
    );
    assert!(wt.exists());
    c.frame(
        "workspace.delete",
        json!({"request_id": "d-2", "path": target, "force": true}),
    )
    .await;
    let r = c.expect("result").await["payload"].clone();
    assert_eq!(r, json!({"request_id": "d-2", "removed": true}));
    assert!(!wt.exists());
    git(
        &repo,
        &["rev-parse", "--verify", "-q", "refs/heads/hqc/fix"],
    );
    w.stop().await;
}

fn has_call(call: FakeCall) -> impl Fn(&[FakeCall]) -> bool {
    move |c: &[FakeCall]| c.contains(&call)
}

fn input_call(sid: Uuid, eid: Uuid) -> FakeCall {
    FakeCall::Input {
        session_id: sid,
        event_id: eid,
    }
}

fn inject_call(sid: Uuid, eid: Uuid) -> FakeCall {
    FakeCall::Inject {
        session_id: sid,
        event_id: eid,
    }
}

/// A running session with input `a` handed and accepted, its ack read.
async fn turn_of(c: &mut HubConn, env: &Env, adapters: &FakeAdapters, sid: Uuid, a: Uuid) {
    start_session(c, env, sid).await;
    c.frame("session.input", input(sid, a, "A")).await;
    adapters
        .wait_for("input A", has_call(input_call(sid, a)))
        .await;
    adapters.accept_input(sid, a).await;
    assert_eq!(c.expect("ack").await["payload"], json!({"event_id": a}));
}

/// The ack of frame `id`, skipped or next; other acks read meanwhile are dropped.
async fn ack_of(c: &mut HubConn, id: &str) -> Value {
    if let Some(i) = c
        .skipped
        .iter()
        .position(|f| f["type"] == "ack" && f["id"] == id)
    {
        return c.skipped.remove(i);
    }
    loop {
        let f = c.expect("ack").await;
        if f["id"] == id {
            return f;
        }
    }
}

// SPEC §17.2: a person's input during a turn goes into it when the runtime can take it, as
// in the VS Code tab; its acceptance is `ack {joined: true}` and the turn's end hands nothing
// more. The next input after the turn is a turn again.
#[tokio::test]
async fn input_during_a_turn_goes_into_it() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    let (a, b, d) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    turn_of(&mut c, &env, &adapters, sid, a).await;

    let fb = c.frame("session.input", input(sid, b, "B")).await;
    adapters
        .wait_for("B into the turn", has_call(inject_call(sid, b)))
        .await;
    assert!(c.maybe("ack", Duration::from_millis(200)).await.is_none());
    adapters.accept_input(sid, b).await;
    let ack = ack_of(&mut c, &fb).await;
    assert_eq!(ack["payload"], json!({"event_id": b, "joined": true}));
    // Resent after a reconnect (the ack was lost with the link): acked from memory as joined
    // again, not written again; a plain ack would open a turn in the hub that nothing closes.
    let fb2 = c.frame("session.input", input(sid, b, "B")).await;
    assert_eq!(
        ack_of(&mut c, &fb2).await["payload"],
        json!({"event_id": b, "joined": true})
    );

    adapters.end_turn(sid).await;
    c.frame("session.input", input(sid, d, "D")).await;
    let calls = adapters
        .wait_for("D as a turn", has_call(input_call(sid, d)))
        .await;
    let written: Vec<&FakeCall> = calls
        .iter()
        .filter(|c| matches!(c, FakeCall::Input { .. } | FakeCall::Inject { .. }))
        .collect();
    assert_eq!(
        written,
        [
            &input_call(sid, a),
            &inject_call(sid, b),
            &input_call(sid, d)
        ]
    );
    w.stop().await;
}

// SPEC §17.2: a capped input does not go into a turn: it waits for its end and runs as a turn
// of its own.
#[tokio::test]
async fn a_capped_input_waits_for_the_end_of_the_turn() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    let (a, k) = (Uuid::now_v7(), Uuid::now_v7());
    turn_of(&mut c, &env, &adapters, sid, a).await;

    c.frame("session.input", capped_input(sid, k, "ask")).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        injected_calls(&adapters).is_empty(),
        "{:?}",
        adapters.calls()
    );
    adapters.end_turn(sid).await;
    adapters
        .wait_for("K as a turn", has_call(input_call(sid, k)))
        .await;
    w.stop().await;
}

/// `hqc_ask` P of another session, as a `session.input` frame.
fn ask_input(sid: Uuid, eid: Uuid) -> Value {
    let mut v = input(sid, eid, "Is the build green?");
    v["peer"] = json!({"from": "hqc/y", "mode": "ask", "request_id": "r-1"});
    v
}

/// The inputs written into a running turn so far.
fn injected_calls(adapters: &FakeAdapters) -> Vec<FakeCall> {
    adapters
        .calls()
        .into_iter()
        .filter(|c| matches!(c, FakeCall::Inject { .. }))
        .collect()
}

// SPEC §17.2: a person's input does not go into the turn of an `hqc_ask`: the ask's final
// answer would be shaped by it. It waits for the end of that turn.
#[tokio::test]
async fn a_persons_input_does_not_join_an_ask_turn() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;
    let (p, u) = (Uuid::now_v7(), Uuid::now_v7());
    c.frame("session.input", ask_input(sid, p)).await;
    adapters
        .wait_for("the ask as a turn", has_call(input_call(sid, p)))
        .await;
    adapters.accept_input(sid, p).await;
    c.frame("session.input", input(sid, u, "stop, do Z instead"))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        injected_calls(&adapters).is_empty(),
        "{:?}",
        adapters.calls()
    );
    adapters.end_turn(sid).await;
    adapters
        .wait_for("U as a turn", has_call(input_call(sid, u)))
        .await;
    assert!(
        injected_calls(&adapters).is_empty(),
        "{:?}",
        adapters.calls()
    );
    w.stop().await;
}

// SPEC §17.2: an `hqc_ask` handed while the runtime runs a turn it began by itself is carried
// into the runtime's next turn; a person's input does not go into it there either, or the
// turn's answer would name the person's input and the ask would get none.
#[tokio::test]
async fn a_persons_input_does_not_join_an_ask_carried_into_the_next_turn() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;
    let (p, u) = (Uuid::now_v7(), Uuid::now_v7());
    // The runtime is in a turn of its own: P is not taken yet.
    c.frame("session.input", ask_input(sid, p)).await;
    adapters
        .wait_for("the ask handed", has_call(input_call(sid, p)))
        .await;
    c.frame("session.input", input(sid, u, "stop, do Z instead"))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        injected_calls(&adapters).is_empty(),
        "{:?}",
        adapters.calls()
    );
    adapters.end_turn_carrying(sid, vec![p], vec![]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        injected_calls(&adapters).is_empty(),
        "{:?}",
        adapters.calls()
    );
    adapters.accept_input(sid, p).await;
    adapters.end_turn(sid).await;
    adapters
        .wait_for("U as a turn", has_call(input_call(sid, u)))
        .await;
    assert!(
        injected_calls(&adapters).is_empty(),
        "{:?}",
        adapters.calls()
    );
    w.stop().await;
}

/// A `send` of another session `from`, as a `session.input` frame.
fn send_input(sid: Uuid, eid: Uuid, text: &str) -> Value {
    let mut v = input(sid, eid, text);
    v["peer"] = json!({"from": "hqc/a", "mode": "send"});
    v
}

// SPEC §17.2: a `send` of another session goes into the running turn of a person's input as a
// person's input does, and its acceptance is `ack {joined: true}`: it opens no turn.
#[tokio::test]
async fn a_send_during_a_persons_turn_joins_it() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    let (a, s) = (Uuid::now_v7(), Uuid::now_v7());
    turn_of(&mut c, &env, &adapters, sid, a).await;

    let fs = c.frame("session.input", send_input(sid, s, "FYI")).await;
    adapters
        .wait_for("the send into the turn", has_call(inject_call(sid, s)))
        .await;
    adapters.accept_input(sid, s).await;
    assert_eq!(
        ack_of(&mut c, &fs).await["payload"],
        json!({"event_id": s, "joined": true})
    );
    adapters.end_turn(sid).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !adapters.calls().contains(&input_call(sid, s)),
        "{:?}",
        adapters.calls()
    );
    w.stop().await;
}

// SPEC §17.2: a person's input goes past the waiting inputs in front of it that are no person's
// and cannot join (an `hqc_ask`, a run of a schedule); they keep their places and run as turns
// of their own after the turn, in order.
#[tokio::test]
async fn a_persons_input_passes_a_waiting_ask_and_a_schedule_run() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    let (a, p, s, u) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    turn_of(&mut c, &env, &adapters, sid, a).await;

    c.frame("session.input", ask_input(sid, p)).await;
    let mut scheduled = input(sid, s, "Check the build");
    scheduled["schedule_id"] = json!(Uuid::now_v7());
    c.frame("session.input", scheduled).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        injected_calls(&adapters).is_empty(),
        "{:?}",
        adapters.calls()
    );
    let fu = c.frame("session.input", input(sid, u, "mine")).await;
    adapters
        .wait_for("U into the turn", has_call(inject_call(sid, u)))
        .await;
    adapters.accept_input(sid, u).await;
    assert_eq!(
        ack_of(&mut c, &fu).await["payload"],
        json!({"event_id": u, "joined": true})
    );

    adapters.end_turn(sid).await;
    adapters
        .wait_for("the ask as a turn", has_call(input_call(sid, p)))
        .await;
    adapters.accept_input(sid, p).await;
    adapters.end_turn(sid).await;
    let calls = adapters
        .wait_for("the scheduled run as a turn", has_call(input_call(sid, s)))
        .await;
    let written: Vec<&FakeCall> = calls
        .iter()
        .filter(|c| matches!(c, FakeCall::Input { .. } | FakeCall::Inject { .. }))
        .collect();
    assert_eq!(
        written,
        [
            &input_call(sid, a),
            &inject_call(sid, u),
            &input_call(sid, p),
            &input_call(sid, s)
        ]
    );
    w.stop().await;
}

// SPEC §17.2: a slash command of a person stops the search: a person's input behind it does
// not go past it.
#[tokio::test]
async fn a_persons_input_does_not_pass_a_slash_command() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    let (a, k, u) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    turn_of(&mut c, &env, &adapters, sid, a).await;

    c.frame("session.input", input(sid, k, "/compact")).await;
    c.frame("session.input", input(sid, u, "mine")).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        injected_calls(&adapters).is_empty(),
        "{:?}",
        adapters.calls()
    );
    adapters.end_turn(sid).await;
    adapters
        .wait_for("the slash command as a turn", has_call(input_call(sid, k)))
        .await;
    w.stop().await;
}

// SPEC §17.2: a turn started by a `send`, or capped to `ask`, takes a person's input; the turn
// of an `hqc_ask` takes nothing, a `send` included (its answer goes to its sender).
#[tokio::test]
async fn what_may_join_goes_into_a_send_turn_and_a_capped_turn_but_not_an_ask_turn() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;
    let (s, u, k, v, p, t) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );

    c.frame("session.input", send_input(sid, s, "do X")).await;
    adapters
        .wait_for("the send as a turn", has_call(input_call(sid, s)))
        .await;
    adapters.accept_input(sid, s).await;
    c.frame("session.input", input(sid, u, "and Y")).await;
    adapters
        .wait_for("U into the send's turn", has_call(inject_call(sid, u)))
        .await;
    adapters.accept_input(sid, u).await;
    adapters.end_turn(sid).await;

    c.frame("session.input", capped_input(sid, k, "ask")).await;
    adapters
        .wait_for("the capped input as a turn", has_call(input_call(sid, k)))
        .await;
    adapters.accept_input(sid, k).await;
    c.frame("session.input", input(sid, v, "mine")).await;
    adapters
        .wait_for("V into the capped turn", has_call(inject_call(sid, v)))
        .await;
    adapters.accept_input(sid, v).await;
    adapters.end_turn(sid).await;

    c.frame("session.input", ask_input(sid, p)).await;
    adapters
        .wait_for("the ask as a turn", has_call(input_call(sid, p)))
        .await;
    adapters.accept_input(sid, p).await;
    c.frame("session.input", send_input(sid, t, "FYI")).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        injected_calls(&adapters),
        [inject_call(sid, u), inject_call(sid, v)]
    );
    adapters.end_turn(sid).await;
    adapters
        .wait_for("the send after the ask", has_call(input_call(sid, t)))
        .await;
    w.stop().await;
}

// SPEC §17.2: a `send` whose cap is below the session's own mode does not go into a running
// turn (it would hold the turn's mode down); a person's input goes past it, and a send whose
// cap is the session's mode goes in.
#[tokio::test]
async fn a_send_capped_below_the_session_does_not_join() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    started_in(&mut c, &env, sid, "auto").await;
    let (a, s, u, m) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    c.frame("session.input", input(sid, a, "A")).await;
    adapters
        .wait_for("input A", has_call(input_call(sid, a)))
        .await;
    adapters.accept_input(sid, a).await;

    let mut low = send_input(sid, s, "from a session in ask");
    low["permission_cap"] = json!("ask");
    c.frame("session.input", low).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        injected_calls(&adapters).is_empty(),
        "{:?}",
        adapters.calls()
    );
    c.frame("session.input", input(sid, u, "mine")).await;
    adapters
        .wait_for("U past the capped send", has_call(inject_call(sid, u)))
        .await;
    let mut level = send_input(sid, m, "from a session in auto");
    level["permission_cap"] = json!("auto");
    c.frame("session.input", level).await;
    adapters
        .wait_for("the send of the same mode", has_call(inject_call(sid, m)))
        .await;
    assert_eq!(
        injected_calls(&adapters),
        [inject_call(sid, u), inject_call(sid, m)]
    );
    w.stop().await;
}

// SPEC §17.2: a `send` joins only while the runtime is confirmed at or below its cap: an owner's
// switch the runtime has not confirmed (or refused) leaves the turn above it.
#[tokio::test]
async fn a_send_does_not_join_while_the_runtime_is_above_its_cap() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    started_in(&mut c, &env, sid, "auto").await;
    let (a, s, u) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    c.frame("session.input", input(sid, a, "A")).await;
    adapters
        .wait_for("input A", has_call(input_call(sid, a)))
        .await;
    adapters.accept_input(sid, a).await;

    // The owner lowers the session to ask; the fake runtime never confirms it.
    c.frame(
        "session.permission_mode",
        json!({"session_id": sid, "permission_mode": "ask"}),
    )
    .await;
    c.expect("ack").await;
    adapters
        .wait_for(
            "the switch to ask",
            has_call(FakeCall::PermissionMode(sid, PermissionMode::Ask)),
        )
        .await;
    let mut low = send_input(sid, s, "from a session in ask");
    low["permission_cap"] = json!("ask");
    c.frame("session.input", low).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        injected_calls(&adapters).is_empty(),
        "{:?}",
        adapters.calls()
    );
    // A person's input is not held by the runtime's mode.
    c.frame("session.input", input(sid, u, "mine")).await;
    adapters
        .wait_for("U past the send", has_call(inject_call(sid, u)))
        .await;
    w.stop().await;
}

// SPEC §17.2: a `send` that becomes the next turn (the runtime ran it by itself) keeps its cap:
// the owner's raise during that turn stays at it, as for a send that waited in the queue.
#[tokio::test]
async fn a_carried_send_keeps_its_cap_on_the_turn() {
    use PermissionMode::{AllowAll, Auto};
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let cfg = hqc_worker::config::Config::parse(&format!(
        "{}[permissions]\nallow_all = true\n",
        env.config_toml(&hub.url())
    ))
    .unwrap();
    let w = start_worker_with(&env, cfg, &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    started_in(&mut c, &env, sid, "auto").await;
    let (a, s) = (Uuid::now_v7(), Uuid::now_v7());
    c.frame("session.input", input(sid, a, "A")).await;
    adapters
        .wait_for("input A", has_call(input_call(sid, a)))
        .await;
    adapters.accept_input(sid, a).await;

    let mut capped = send_input(sid, s, "from a session in auto");
    capped["permission_cap"] = json!("auto");
    c.frame("session.input", capped).await;
    adapters
        .wait_for("the send into the turn", has_call(inject_call(sid, s)))
        .await;
    // The runtime ends the turn and runs the send by itself.
    adapters.end_turn_carrying(sid, vec![s], vec![]).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    c.frame(
        "session.permission_mode",
        json!({"session_id": sid, "permission_mode": "allow_all"}),
    )
    .await;
    c.expect("ack").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let calls = adapters.calls();
    assert!(
        !calls.contains(&FakeCall::PermissionMode(sid, AllowAll)),
        "{calls:?}"
    );
    assert!(switches(&calls).iter().all(|m| *m <= Auto), "{calls:?}");
    w.stop().await;
}

// SPEC §17.2, T39: a send that joined a capped turn and becomes the next turn runs it in its own
// mode (the lower of its cap and the session's), not in the one the turn before left: the
// runtime is switched up and the hub learns the turn is no longer held by the cap.
#[tokio::test]
async fn a_carried_send_after_a_capped_turn_runs_in_its_own_mode() {
    use PermissionMode::{Ask, Auto};
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    started_in(&mut c, &env, sid, "auto").await;
    let (s1, s2) = (Uuid::now_v7(), Uuid::now_v7());

    // T0: a send from a session in ask; the runtime is lowered to ask for it.
    let mut first = send_input(sid, s1, "from a session in ask");
    first["permission_cap"] = json!("ask");
    c.frame("session.input", first).await;
    adapters
        .wait_for(
            "the switch to ask",
            has_call(FakeCall::PermissionMode(sid, Ask)),
        )
        .await;
    adapters.emit(sid, AdapterEvent::PermissionMode(Ask)).await;
    adapters
        .wait_for("S1 as a turn", has_call(input_call(sid, s1)))
        .await;
    state_where(&mut c, sid, |st| st["payload"]["capped_input"] == json!(s1)).await;
    adapters.accept_input(sid, s1).await;

    // A send from a session in auto joins it (the runtime is at ask, below the cap).
    let mut second = send_input(sid, s2, "from a session in auto");
    second["permission_cap"] = json!("auto");
    c.frame("session.input", second).await;
    adapters
        .wait_for("S2 into the turn", has_call(inject_call(sid, s2)))
        .await;
    c.skipped.clear();
    // The runtime ends the turn and runs S2 by itself.
    adapters.end_turn_carrying(sid, vec![s2], vec![]).await;
    let calls = adapters
        .wait_for(
            "the switch up to auto",
            has_call(FakeCall::PermissionMode(sid, Auto)),
        )
        .await;
    assert_eq!(switches(&calls), [Ask, Auto]);
    let st = state_where(&mut c, sid, |st| {
        st["payload"].get("capped_input").is_none()
    })
    .await;
    assert!(st["payload"].get("capped_input").is_none(), "{st}");
    w.stop().await;
}

// SPEC §17.2: a run of a schedule (`schedule_id`) and a slash command do not go into a running
// turn: each waits for its end and runs as a turn of its own.
#[tokio::test]
async fn scheduled_runs_and_slash_commands_wait_for_the_end_of_the_turn() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    let (a, s, k) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    turn_of(&mut c, &env, &adapters, sid, a).await;

    let mut scheduled = input(sid, s, "Check the build");
    scheduled["schedule_id"] = json!(Uuid::now_v7());
    c.frame("session.input", scheduled).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        injected_calls(&adapters).is_empty(),
        "{:?}",
        adapters.calls()
    );
    adapters.end_turn(sid).await;
    adapters
        .wait_for("the scheduled run as a turn", has_call(input_call(sid, s)))
        .await;
    adapters.accept_input(sid, s).await;

    c.frame("session.input", input(sid, k, "/compact")).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        injected_calls(&adapters).is_empty(),
        "{:?}",
        adapters.calls()
    );
    adapters.end_turn(sid).await;
    adapters
        .wait_for("the slash command as a turn", has_call(input_call(sid, k)))
        .await;
    w.stop().await;
}

// SPEC §17.2: while the runtime asks (an open permission request), the input waits; the
// answer lets it into the turn.
#[tokio::test]
async fn input_waiting_for_an_answer_goes_into_the_turn_after_it() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    turn_of(&mut c, &env, &adapters, sid, a).await;
    // An approval is open: the runtime takes nothing now.
    c.frame("session.input", input(sid, b, "B")).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!adapters.calls().contains(&inject_call(sid, b)));
    adapters.set_injectable(true);
    let fr = c
        .frame(
            "approval.response",
            json!({"session_id": sid, "request_id": "r1", "option_id": "once"}),
        )
        .await;
    ack_of(&mut c, &fr).await;
    adapters
        .wait_for("B into the turn", has_call(inject_call(sid, b)))
        .await;
    w.stop().await;
}

// SPEC §17.2: the turn ends without taking what was written into it. The first input it
// carries becomes the next turn (the runtime runs it by itself: not handed again, plain ack);
// one the runtime will not take (`dropped`) goes back to the queue and runs as a turn of its
// own.
#[tokio::test]
async fn carried_input_becomes_the_turn_and_the_rest_goes_back() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    let (a, b, d) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    turn_of(&mut c, &env, &adapters, sid, a).await;
    let fb = c.frame("session.input", input(sid, b, "B")).await;
    adapters
        .wait_for("B into the turn", has_call(inject_call(sid, b)))
        .await;
    let fd = c.frame("session.input", input(sid, d, "D")).await;
    adapters
        .wait_for("D into the turn", has_call(inject_call(sid, d)))
        .await;

    adapters.end_turn_carrying(sid, vec![b], vec![d]).await;
    // D is dropped: back to the queue; B's turn runs, so D goes into it.
    adapters
        .wait_for("D written again", |c| {
            c.iter().filter(|c| **c == inject_call(sid, d)).count() == 2
        })
        .await;
    adapters.accept_input(sid, b).await;
    assert_eq!(
        ack_of(&mut c, &fb).await["payload"],
        json!({"event_id": b}),
        "B is its turn's input"
    );
    // Dropped again, with no turn after it: D runs as a turn of its own.
    adapters.end_turn_carrying(sid, vec![], vec![d]).await;
    adapters
        .wait_for("D as a turn", has_call(input_call(sid, d)))
        .await;
    adapters.accept_input(sid, d).await;
    assert_eq!(ack_of(&mut c, &fd).await["payload"], json!({"event_id": d}));
    let calls = adapters.calls();
    assert!(!calls.contains(&input_call(sid, b)), "{calls:?}");
    w.stop().await;
}

// SPEC §17.2: what the runtime drops after the person's Stop (Claude Code cancels a carried
// input on a second Stop) was cancelled by them: it fails, not retryable, instead of running
// again; dropped without a Stop it goes back to the queue (above).
#[tokio::test]
async fn input_dropped_after_a_stop_fails() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    turn_of(&mut c, &env, &adapters, sid, a).await;
    let fb = c.frame("session.input", input(sid, b, "B")).await;
    adapters
        .wait_for("B into the turn", has_call(inject_call(sid, b)))
        .await;
    let fs = c.frame("session.stop", json!({"session_id": sid})).await;
    assert_eq!(ack_of(&mut c, &fs).await["id"], fs.as_str());
    adapters.end_turn_carrying(sid, vec![], vec![b]).await;
    let err = loop {
        let f = c.expect("error").await;
        if f["id"] == fb.as_str() {
            break f;
        }
    };
    assert_eq!(err["payload"]["retryable"], false, "{err}");
    let calls = adapters.calls();
    assert!(!calls.contains(&input_call(sid, b)), "{calls:?}");
    assert_eq!(
        calls.iter().filter(|c| **c == inject_call(sid, b)).count(),
        1,
        "{calls:?}"
    );
    w.stop().await;
}

// SPEC §17.2: an input written into the turn the runtime runs next before the end of the turn
// before it reached the session (Claude Code: the engine takes writes as soon as a `result`
// carries an input, the session learns of that end from `TurnEnded` still on its way) is
// written once: the end names it neither carried nor dropped, so it stays written.
#[tokio::test]
async fn input_written_before_the_turn_end_arrives_is_written_once() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    let (a, b, d) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    turn_of(&mut c, &env, &adapters, sid, a).await;
    let fb = c.frame("session.input", input(sid, b, "B")).await;
    adapters
        .wait_for("B into the turn", has_call(inject_call(sid, b)))
        .await;
    // The runtime's `result` carried B; D goes into B's turn before that end arrives.
    let fd = c.frame("session.input", input(sid, d, "D")).await;
    adapters
        .wait_for("D into the turn", has_call(inject_call(sid, d)))
        .await;
    adapters.end_turn_carrying(sid, vec![b], vec![]).await;
    adapters.accept_input(sid, b).await;
    assert_eq!(ack_of(&mut c, &fb).await["payload"], json!({"event_id": b}));
    adapters.accept_input(sid, d).await;
    assert_eq!(
        ack_of(&mut c, &fd).await["payload"],
        json!({"event_id": d, "joined": true})
    );
    adapters.end_turn(sid).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let calls = adapters.calls();
    let d_writes = calls
        .iter()
        .filter(|c| **c == inject_call(sid, d) || **c == input_call(sid, d))
        .count();
    assert_eq!(d_writes, 1, "{calls:?}");
    w.stop().await;
}

// SPEC §17.2: input handed while the runtime runs a turn it began by itself is carried to
// the runtime's next turn: it stays the turn, it is not handed again.
#[tokio::test]
async fn a_carried_turn_input_stays_the_turn() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let fa = c.frame("session.input", input(sid, a, "A")).await;
    adapters
        .wait_for("input A", has_call(input_call(sid, a)))
        .await;
    c.frame("session.input", input(sid, b, "B")).await;
    adapters.end_turn_carrying(sid, vec![a], vec![]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let calls = adapters.calls();
    assert_eq!(
        calls.iter().filter(|c| **c == input_call(sid, a)).count(),
        1,
        "A not handed again: {calls:?}"
    );
    assert!(!calls.contains(&input_call(sid, b)), "B waits: {calls:?}");
    adapters.accept_input(sid, a).await;
    assert_eq!(ack_of(&mut c, &fa).await["payload"], json!({"event_id": a}));
    adapters.end_turn(sid).await;
    adapters
        .wait_for("input B", has_call(input_call(sid, b)))
        .await;
    w.stop().await;
}

// SPEC §17.2: the process exits with an input written into its turn and not taken: it goes
// back to the queue and the next process runs it.
#[tokio::test]
async fn input_written_into_a_lost_turn_runs_again() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    turn_of(&mut c, &env, &adapters, sid, a).await;
    c.frame("session.input", input(sid, b, "B")).await;
    adapters
        .wait_for("B into the turn", has_call(inject_call(sid, b)))
        .await;
    adapters
        .emit(
            sid,
            AdapterEvent::Exited {
                error: Some("killed".into()),
            },
        )
        .await;
    let calls = adapters
        .wait_for("B as a turn", has_call(input_call(sid, b)))
        .await;
    assert!(calls.contains(&FakeCall::Resume(sid)), "{calls:?}");
    w.stop().await;
}

// The runtime's subagent count goes to the hub in `session.state`, stays in the later ones,
// and is 0 (absent) once the agent is stopped.
#[tokio::test]
async fn subagents_go_in_session_state() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;
    adapters.emit(sid, AdapterEvent::Subagents(2)).await;
    state_where(&mut c, sid, |st| st["payload"]["subagents"] == 2).await;
    adapters
        .emit(sid, AdapterEvent::Title("Fix the build".into()))
        .await;
    let st = state_where(&mut c, sid, |st| st["payload"]["title"] == "Fix the build").await;
    assert_eq!(st["payload"]["subagents"], 2, "{st}");
    let fk = c.frame("session.kill", json!({"session_id": sid})).await;
    let st = state_where(&mut c, sid, |st| {
        st["payload"]["process_state"] == "stopped"
    })
    .await;
    assert!(st["payload"].get("subagents").is_none(), "{st}");
    ack_of(&mut c, &fk).await;
    w.stop().await;
}

// The runtime's list of subagents goes as `subagents.live`; the last one sent is sent again on
// the next connect, its run times counted then; so is the runtime's empty list that followed a
// non-empty one, since the hub keeps the last non-empty list while a background task runs; the
// empty list sent when the agent is stopped leaves nothing to send again. The age of the list
// is taken off the time to the fire of a wakeup too, down to 0.
#[tokio::test]
async fn live_subagents_are_sent_again_on_connect() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;
    let agent = LiveSubagent {
        task_id: "a1".into(),
        description: "Fix the build".into(),
        activity: None,
        total_tokens: None,
        tool_uses: None,
        duration_ms: 1000,
        call_id: None,
        kind: None,
        stoppable: true,
        due_in_ms: None,
        retry: None,
    };
    let wakeup = LiveSubagent {
        task_id: "toolu_01Wake".into(),
        description: "Check the build again".into(),
        duration_ms: 1000,
        call_id: Some("toolu_01Wake".into()),
        kind: Some(LiveKind::Wakeup),
        due_in_ms: Some(1500),
        stoppable: false,
        ..agent.clone()
    };
    let due = LiveSubagent {
        task_id: "toolu_01Due".into(),
        due_in_ms: Some(10),
        ..wakeup.clone()
    };
    adapters
        .emit(
            sid,
            AdapterEvent::LiveSubagents(vec![agent.clone(), wakeup, due]),
        )
        .await;
    let first = live_agents(&mut c).await;
    assert_eq!(first[0]["duration_ms"], 1000);
    assert_eq!(first[0].get("due_in_ms"), None, "{first}");
    assert_eq!(first[1]["due_in_ms"], 1500, "{first}");
    tokio::time::sleep(Duration::from_millis(50)).await;
    c.close().await;
    c = hub.accept().await;
    c.handshake().await;
    let again = live_agents(&mut c).await;
    assert_eq!(again[0]["task_id"], "a1", "{again}");
    assert!(again[0]["duration_ms"].as_u64().unwrap() >= 1050, "{again}");
    assert_eq!(again[0].get("due_in_ms"), None, "{again}");
    assert!(again[1]["duration_ms"].as_u64().unwrap() >= 1050, "{again}");
    // 1500 ms less than 50 ms and a little: not 1500, not 0 yet.
    let left = again[1]["due_in_ms"].as_u64().unwrap();
    assert!((1..=1450).contains(&left), "{again}");
    // Due within the age: 0, not a wrapped number.
    assert_eq!(again[2]["due_in_ms"], 0, "{again}");
    // Its subagents ended, the agent runs on: the empty list is sent again, on every connect.
    adapters
        .emit(sid, AdapterEvent::LiveSubagents(vec![]))
        .await;
    assert_eq!(live_agents(&mut c).await, json!([]));
    for _ in 0..2 {
        c.close().await;
        c = hub.accept().await;
        c.handshake().await;
        assert_eq!(live_agents(&mut c).await, json!([]));
    }
    adapters
        .emit(sid, AdapterEvent::LiveSubagents(vec![agent]))
        .await;
    assert_eq!(live_agents(&mut c).await[0]["task_id"], "a1");
    let fk = c.frame("session.kill", json!({"session_id": sid})).await;
    assert_eq!(live_agents(&mut c).await, json!([]));
    ack_of(&mut c, &fk).await;
    c.close().await;
    c = hub.accept().await;
    c.handshake().await;
    while let Some(f) = c.maybe("event", Duration::from_millis(300)).await {
        assert_ne!(f["payload"]["event"]["type"], "subagents.live", "{f}");
    }
    w.stop().await;
}

/// The `agents` of the next `subagents.live`; other events read meanwhile are dropped.
async fn live_agents(c: &mut HubConn) -> Value {
    loop {
        let f = c.expect("event").await;
        if f["payload"]["event"]["type"] == "subagents.live" {
            return f["payload"]["event"]["payload"]["agents"].clone();
        }
    }
}

// SPEC 0.181: the output file of a background task that ended is uploaded for its session as
// `hqc_attach` uploads a file, its last 2 MB when it is larger, and comes as an `attachment`
// of the agent, `text`, named as the file, whose caption says it was cut. A symlink in its
// place is not followed and gives nothing.
#[tokio::test]
async fn a_task_output_becomes_an_attachment() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;
    let dir = env.dir.path().join("tasks");
    std::fs::create_dir_all(&dir).unwrap();
    let small = dir.join("bnx80r4bg.output");
    std::fs::write(&small, "make: *** [all] Error 2\n").unwrap();
    let big = dir.join("b2st7oc3z.output");
    // 10 bytes more than 2 MB: the first 10 go, the error at the end stays.
    let mut output = b"make all\n\n".to_vec();
    output.extend(vec![b'x'; 2 * 1024 * 1024 - 6]);
    output.extend(b"Error\n");
    std::fs::write(&big, &output).unwrap();
    let linked = dir.join("a52af2b436ded4cea.output");
    std::os::unix::fs::symlink(&small, &linked).unwrap();
    let empty = dir.join("bq4m1t0ys.output");
    std::fs::write(&empty, "").unwrap();
    for p in [&linked, &empty, &small, &big] {
        adapters
            .emit(sid, AdapterEvent::TaskOutput(p.clone()))
            .await;
    }
    let mut attached = Vec::new();
    while attached.len() < 2 {
        let f = c.expect("event").await;
        let e = &f["payload"]["event"];
        if e["type"] == "attachment" {
            assert_eq!(e["author"], json!({"kind": "agent", "id": sid}), "{e}");
            attached.push(e["payload"].clone());
        }
    }
    attached.sort_by_key(|p| p["name"].as_str().unwrap().to_owned());
    let mut uploads = hub.artifacts.uploads();
    uploads.sort_by_key(|u| u.headers["x-hqc-artifact-name"].clone());
    assert_eq!(uploads.len(), 2);
    for u in &uploads {
        assert_eq!(u.headers["x-hqc-session-id"], sid.to_string());
        assert_eq!(u.headers["x-hqc-artifact-kind"], "text");
    }
    assert!(uploads[0].body == output[10..], "not the last 2 MB");
    assert_eq!(uploads[1].body, b"make: *** [all] Error 2\n");
    assert_eq!(
        attached,
        [
            json!({"artifact_id": uploads[0].id, "kind": "text", "name": "b2st7oc3z.output",
                   "caption": "The last 2 MB of 2097162 bytes"}),
            json!({"artifact_id": uploads[1].id, "kind": "text", "name": "bnx80r4bg.output"}),
        ]
    );
    while let Some(f) = c.maybe("event", Duration::from_millis(300)).await {
        assert_ne!(f["payload"]["event"]["type"], "attachment", "{f}");
    }
    assert_eq!(hub.artifacts.uploads().len(), 2);
    w.stop().await;
}

// SPEC 0.181: `session.subagent_history` answers with the events of the subagent's transcript,
// in the session and by its agent, after `after` only the later ones when it holds it; a
// subagent without a transcript has none; a `task_id` that could name another file is refused.
#[tokio::test]
async fn a_subagents_history_comes_from_its_transcript() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let dir = env.dir.path().join("subagents");
    std::fs::create_dir_all(&dir).unwrap();
    adapters.set_subagents_dir(dir.clone());
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;
    let record = |kind: &str, content: Value| {
        json!({"parentUuid": Uuid::new_v4(), "isSidechain": true, "agentId": "a52af2b436ded4cea",
            "type": kind, "uuid": Uuid::new_v4(), "timestamp": "2026-11-13T10:00:00.000Z",
            "message": {"role": kind, "content": content}})
        .to_string()
    };
    let transcript = [
        record("user", json!("Fix the power nets of the import")),
        record(
            "assistant",
            json!([{"type": "tool_use", "id": "toolu_02", "name": "Bash",
                "input": {"command": "cargo test"}}]),
        ),
        record(
            "user",
            json!([{"type": "tool_result", "tool_use_id": "toolu_02", "content": "ok"}]),
        ),
    ];
    std::fs::write(
        dir.join("agent-a52af2b436ded4cea.jsonl"),
        transcript.join("\n"),
    )
    .unwrap();
    // Outside the folder, where `../` would lead.
    std::fs::write(env.dir.path().join("agent-x.jsonl"), &transcript[1]).unwrap();

    let ask = |task_id: &str| json!({"request_id": "r-1", "session_id": sid, "task_id": task_id});
    let id = c
        .frame("session.subagent_history", ask("a52af2b436ded4cea"))
        .await;
    let f = c.expect("result").await;
    assert_eq!(f["id"], id.as_str());
    let events = f["payload"]["events"].as_array().unwrap();
    assert_eq!(
        events.iter().map(|e| e["type"].clone()).collect::<Vec<_>>(),
        ["tool.call", "tool.result"]
    );
    for e in events {
        assert_eq!(e["session_id"], json!(sid));
        assert_eq!(e["author"], json!({"kind": "agent", "id": sid}));
        assert!(e.get("seq").is_none(), "{e}");
    }
    assert_eq!(f["payload"]["truncated"], false);
    assert!(f["payload"].get("continues").is_none(), "{f}");

    // After an event of the page: only the later ones, to append; after one it does not hold:
    // the whole page.
    let page = events.clone();
    let mut again = ask("a52af2b436ded4cea");
    again["after"] = page[0]["event_id"].clone();
    c.frame("session.subagent_history", again.clone()).await;
    let f = c.expect("result").await;
    assert_eq!(f["payload"]["events"], json!(page[1..]), "{f}");
    assert_eq!(f["payload"]["continues"], true);
    assert_eq!(f["payload"]["truncated"], false);
    again["after"] = json!(Uuid::new_v4());
    c.frame("session.subagent_history", again).await;
    let f = c.expect("result").await;
    assert_eq!(f["payload"]["events"], json!(page), "{f}");
    assert!(f["payload"].get("continues").is_none(), "{f}");

    // Before an event of the page: the ones ahead of it; before one it does not hold: none,
    // left out.
    let mut earlier = ask("a52af2b436ded4cea");
    earlier["before"] = page[1]["event_id"].clone();
    c.frame("session.subagent_history", earlier.clone()).await;
    let f = c.expect("result").await;
    assert_eq!(f["payload"]["events"], json!(page[..1]), "{f}");
    assert_eq!(f["payload"]["truncated"], false);
    earlier["before"] = json!(Uuid::new_v4());
    c.frame("session.subagent_history", earlier).await;
    let f = c.expect("result").await;
    assert_eq!(f["payload"]["events"], json!([]), "{f}");
    assert_eq!(f["payload"]["truncated"], true);

    c.frame("session.subagent_history", ask("a55949578b136436b"))
        .await;
    let f = c.expect("result").await;
    assert_eq!(f["payload"]["events"], json!([]));

    let long = "a".repeat(65);
    for bad in ["../agent-x", "a.b", "", long.as_str()] {
        let id = c.frame("session.subagent_history", ask(bad)).await;
        let f = reply_to(&mut c, &id).await;
        assert_eq!(f["type"], "error", "{bad:?}: {f}");
        assert_eq!(f["payload"]["code"], "invalid_frame");
    }
    w.stop().await;
}

// Of a long transcript only the last 4 MiB are read, without the line cut at their start; a
// symlink or a FIFO in its place is no transcript (a FIFO would hang the read).
#[tokio::test]
async fn a_subagents_history_is_the_tail_of_a_regular_file() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let dir = env.dir.path().join("subagents");
    std::fs::create_dir_all(&dir).unwrap();
    adapters.set_subagents_dir(dir.clone());
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;
    let record = |kind: &str, content: Value| {
        json!({"parentUuid": Uuid::new_v4(), "isSidechain": true, "agentId": "big",
            "type": kind, "uuid": Uuid::new_v4(), "timestamp": "2026-11-13T10:00:00.000Z",
            "message": {"role": kind, "content": content}})
        .to_string()
    };
    let call = |id: &str| {
        record(
            "assistant",
            json!([{"type": "tool_use", "id": id, "name": "Bash", "input": {"command": "ls"}}]),
        )
    };
    // The first call; then the last 4 MiB: a line cut right before a call (whole, it does not
    // parse; its rest does), a text (no event) and the last call.
    let tail = 4 * 1024 * 1024;
    let (cut, last) = (call("toolu_cut"), call("toolu_last"));
    let empty = record("user", json!("")).len();
    let text = "x".repeat(tail - cut.len() - last.len() - 2 - empty);
    let long = [
        call("toolu_first"),
        format!("junk {cut}"),
        record("user", json!(text)),
        last,
    ];
    let file = long.join("\n");
    assert!(file[file.len() - tail..].starts_with(&cut));
    std::fs::write(dir.join("agent-big.jsonl"), file).unwrap();
    std::fs::write(dir.join("agent-small.jsonl"), call("toolu_small")).unwrap();
    std::os::unix::fs::symlink(dir.join("agent-small.jsonl"), dir.join("agent-link.jsonl"))
        .unwrap();
    let fifo = dir.join("agent-fifo.jsonl");
    let fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

    let ask = |task_id: &str| json!({"request_id": "r-1", "session_id": sid, "task_id": task_id});
    c.frame("session.subagent_history", ask("big")).await;
    let f = c.expect("result").await;
    let events = f["payload"]["events"].as_array().unwrap();
    assert_eq!(
        events
            .iter()
            .map(|e| e["payload"]["call_id"].clone())
            .collect::<Vec<_>>(),
        ["toolu_last"],
        "{f}"
    );
    assert_eq!(f["payload"]["truncated"], true);

    c.frame("session.subagent_history", ask("small")).await;
    let f = c.expect("result").await;
    assert_eq!(f["payload"]["events"].as_array().unwrap().len(), 1, "{f}");
    c.frame("session.subagent_history", ask("link")).await;
    let f = c.expect("result").await;
    assert_eq!(f["payload"]["events"], json!([]), "{f}");
    c.frame("session.subagent_history", ask("fifo")).await;
    let f = c
        .maybe("result", Duration::from_secs(10))
        .await
        .expect("a FIFO is answered at once");
    assert_eq!(f["payload"]["events"], json!([]), "{f}");
    w.stop().await;
}

// The token rate goes as `tokens.rate` when it changes; one above 0 is sent again on the next
// connect (the hub dropped it with the link), a 0 is not; the agent stopped, it sends 0.
#[tokio::test]
async fn a_token_rate_is_sent_again_on_connect() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;
    adapters.emit(sid, AdapterEvent::TokenRate(42)).await;
    assert_eq!(token_rate(&mut c).await, 42);
    c.close().await;
    c = hub.accept().await;
    c.handshake().await;
    assert_eq!(token_rate(&mut c).await, 42);
    adapters.emit(sid, AdapterEvent::TokenRate(0)).await;
    assert_eq!(token_rate(&mut c).await, 0);
    c.close().await;
    c = hub.accept().await;
    c.handshake().await;
    while let Some(f) = c.maybe("event", Duration::from_millis(300)).await {
        assert_ne!(f["payload"]["event"]["type"], "tokens.rate", "{f}");
    }
    adapters.emit(sid, AdapterEvent::TokenRate(7)).await;
    assert_eq!(token_rate(&mut c).await, 7);
    let fk = c.frame("session.kill", json!({"session_id": sid})).await;
    assert_eq!(token_rate(&mut c).await, 0);
    ack_of(&mut c, &fk).await;
    w.stop().await;
}

/// `output_per_s` of the next `tokens.rate`; other events read meanwhile are dropped.
async fn token_rate(c: &mut HubConn) -> Value {
    loop {
        let f = c.expect("event").await;
        if f["payload"]["event"]["type"] == "tokens.rate" {
            return f["payload"]["event"]["payload"]["output_per_s"].clone();
        }
    }
}

/// The reply (`ack` or `error`) to frame `id`, skipped or next; other replies read meanwhile
/// are dropped.
async fn reply_to(c: &mut HubConn, id: &str) -> Value {
    let is = |f: &Value| (f["type"] == "ack" || f["type"] == "error") && f["id"] == id;
    if let Some(i) = c.skipped.iter().position(is) {
        return c.skipped.remove(i);
    }
    loop {
        let f = c.recv().await;
        if is(&f) {
            return f;
        }
    }
}

fn models(adapters: &FakeAdapters) -> Vec<Option<String>> {
    adapters
        .start_specs()
        .into_iter()
        .map(|s| s.model)
        .collect()
}

// The session's model: `session.start` gives it to the start; `session.model` switches a
// running agent at once, mid-turn too; `null` (the runtime's default) waits for the next
// start, as any model does for a stopped agent; a `session.start` for a running agent with
// another model switches it; a name that is no model name is refused.
#[tokio::test]
async fn model_is_started_and_switched() {
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    let mut start = env.start_payload(sid);
    start["model"] = json!("opus");
    let f = c.frame("session.start", start.clone()).await;
    ack_of(&mut c, &f).await;
    assert_eq!(models(&adapters), [Some("opus".to_string())]);
    let switched = |a: &FakeAdapters| {
        a.calls()
            .into_iter()
            .filter(|c| matches!(c, FakeCall::Model(..)))
            .collect::<Vec<_>>()
    };

    // In a turn: at once, before it ends.
    let a = Uuid::now_v7();
    c.frame("session.input", input(sid, a, "A")).await;
    adapters
        .wait_for("input A", has_call(input_call(sid, a)))
        .await;
    let f = c
        .frame(
            "session.model",
            json!({"session_id": sid, "model": "sonnet"}),
        )
        .await;
    assert_eq!(reply_to(&mut c, &f).await["payload"], json!({}));
    adapters
        .wait_for(
            "the switch",
            has_call(FakeCall::Model(sid, "sonnet".into())),
        )
        .await;
    adapters.accept_input(sid, a).await;
    adapters.end_turn(sid).await;

    // The same model again: nothing to switch. The default has no name: the next start.
    for model in [json!("sonnet"), Value::Null] {
        let f = c
            .frame("session.model", json!({"session_id": sid, "model": model}))
            .await;
        assert_eq!(reply_to(&mut c, &f).await["type"], "ack");
    }
    let f = c.frame("session.kill", json!({"session_id": sid})).await;
    reply_to(&mut c, &f).await;
    let b = Uuid::now_v7();
    c.frame("session.input", input(sid, b, "B")).await;
    adapters
        .wait_for("input B", has_call(input_call(sid, b)))
        .await;
    assert_eq!(models(&adapters), [Some("opus".to_string()), None]);
    assert_eq!(switched(&adapters), [FakeCall::Model(sid, "sonnet".into())]);

    // A switch the worker missed comes with the next `session.start`.
    start["model"] = json!("haiku");
    let f = c.frame("session.start", start).await;
    assert_eq!(reply_to(&mut c, &f).await["type"], "ack");
    adapters
        .wait_for(
            "the missed switch",
            has_call(FakeCall::Model(sid, "haiku".into())),
        )
        .await;

    // Not a model name: refused, as in `session.start`. An unknown session: the next
    // `session.start` carries the model.
    let f = c
        .frame(
            "session.model",
            json!({"session_id": sid, "model": "--resume"}),
        )
        .await;
    let err = reply_to(&mut c, &f).await;
    assert_eq!(
        (err["type"].as_str(), err["payload"]["code"].as_str()),
        (Some("error"), Some("invalid_frame"))
    );
    let f = c
        .frame(
            "session.model",
            json!({"session_id": Uuid::now_v7(), "model": "opus"}),
        )
        .await;
    assert_eq!(reply_to(&mut c, &f).await["type"], "ack");
    assert_eq!(switched(&adapters).len(), 2);
    w.stop().await;
}

// SPEC §8.1, 0.180: a tool result's whole text and images are uploaded for its session before
// the event goes, which names them, and the adapter's next event comes after it; a refused
// upload leaves its field out and the event goes as it was.
#[tokio::test]
async fn a_tool_result_goes_after_its_files() {
    use hqc_worker::adapter::{ResultFiles, ResultImage};
    let env = Env::new();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let sid = Uuid::now_v7();
    let mut c = hub.accept().await;
    c.handshake().await;
    start_session(&mut c, &env, sid).await;

    let result = |preview: &str| {
        agent_event(
            sid,
            EventKind::ToolResult {
                call_id: "t1".into(),
                result_preview: preview.into(),
                error: false,
                error_text: None,
                duration_ms: None,
                artifact_id: None,
                artifact_ids: vec![],
            },
        )
    };
    let files = ResultFiles {
        text: Some("the whole text".into()),
        images: vec![ResultImage {
            media_type: "image/png".into(),
            data: b"png".to_vec(),
        }],
    };
    let ev = result("the whole");
    adapters
        .emit(
            sid,
            AdapterEvent::ToolResult {
                event: ev.clone(),
                files: files.clone(),
            },
        )
        .await;
    let after = agent_message(sid, "after");
    adapters.emit(sid, AdapterEvent::Event(after.clone())).await;
    let f = stored_event(&mut c).await;
    assert_eq!(event_id(&f), ev.event_id);
    let ups = hub.artifacts.uploads();
    assert_eq!(ups.len(), 2);
    let p = &f["payload"]["event"]["payload"];
    assert_eq!(p["artifact_id"], json!(ups[0].id));
    assert_eq!(p["artifact_ids"], json!([ups[1].id]));
    assert_eq!(p["result_preview"], "the whole");
    assert_eq!(ups[0].body, b"the whole text");
    assert_eq!(ups[0].headers["x-hqc-session-id"], sid.to_string());
    assert_eq!(ups[0].headers["x-hqc-artifact-kind"], "text");
    assert_eq!(ups[0].headers["content-type"], "text/plain; charset=utf-8");
    assert_eq!(ups[1].body, b"png");
    assert_eq!(ups[1].headers["x-hqc-artifact-kind"], "image");
    assert_eq!(ups[1].headers["x-hqc-artifact-name"], "image.png");
    assert_eq!(ups[1].headers["content-type"], "image/png");
    assert_eq!(event_id(&stored_event(&mut c).await), after.event_id);

    hub.artifacts.fail_uploads.store(true, Ordering::SeqCst);
    let ev = result("again");
    adapters
        .emit(
            sid,
            AdapterEvent::ToolResult {
                event: ev.clone(),
                files,
            },
        )
        .await;
    let f = stored_event(&mut c).await;
    assert_eq!(event_id(&f), ev.event_id);
    let p = &f["payload"]["event"]["payload"];
    assert!(
        p.get("artifact_id").is_none() && p.get("artifact_ids").is_none(),
        "{p}"
    );
    assert_eq!(p["result_preview"], "again");
    w.stop().await;
}
