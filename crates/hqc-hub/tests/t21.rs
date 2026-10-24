//! T21 with a fake worker: the mirror is on by default and switched by `PATCH`, `mirror.set`
//! lists the hub's sessions with the time of their last event, migration `0011`; the machine
//! fingerprint orders replaceable workers; titles from a model (fake Anthropic and OpenAI
//! servers) or from the first prompt, never over the user's; the daily artifact cleanup;
//! `session.cleanup` on archive and delete.

mod common;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{http::HeaderMap, routing::post, Json, Router};
use common::*;
use hqc_hub::llm::{LlmConfig, Provider};
use serde_json::{json, Value};
use uuid::Uuid;

const WS: &str = "/tmp/hqc-t21/ws";

async fn workspace_of(hub: &TestHub) -> Value {
    let (_, list) = hub.get("/api/workspaces").await;
    list["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["path"] == WS)
        .cloned()
        .unwrap()
}

/// Input `text` (with `artifact_ids`) into `sid`, delivered to the fake worker; returns the
/// stored `user.message` event.
async fn input(
    hub: &TestHub,
    w: &mut FakeWorker,
    sid: &str,
    text: &str,
    artifact_ids: Value,
) -> Value {
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": text, "artifact_ids": artifact_ids}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let mut f = w.recv().await;
    if f["type"] == "session.start" {
        w.reply(&f["id"], "ack", json!({})).await;
        f = w.expect("session.input").await;
    }
    assert_eq!(f["type"], "session.input", "{f}");
    w.reply(
        &f["id"],
        "ack",
        json!({"event_id": f["payload"]["event_id"]}),
    )
    .await;
    v["event"].clone()
}

async fn answer(w: &mut FakeWorker, sid: &str, text: &str) {
    w.event(agent_event(
        sid,
        "agent.message",
        json!({"text": text, "final": true}),
    ))
    .await;
}

async fn new_session(hub: &TestHub, body: Value) -> String {
    let pid = hub.personal_project().await;
    let (s, v) = hub
        .post(&format!("/api/projects/{pid}/sessions"), body)
        .await;
    assert_eq!(s, 201, "{v}");
    v["id"].as_str().unwrap().to_string()
}

// ---------------------------------------------------------------- mirror by default

#[tokio::test]
async fn mirror_is_on_by_default_and_switched_by_patch() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS).await;
    w.see_mirror_set = true;
    let ws = workspace_of(&hub).await;
    assert_eq!(ws["mirror_enabled"], true);
    let set = w.expect("mirror.set").await;
    assert_eq!(
        set["payload"],
        json!({"workspace_path": WS, "enabled": true})
    );

    // A session of hqc with a prompt and an answer.
    let sid = hub.create_session("built here").await;
    input(&hub, &mut w, &sid, "hello", json!([])).await;
    answer(&mut w, &sid, "hi").await;
    let runtime = hub.session(&sid).await["agent"]["runtime_session_id"].clone();
    let last: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT max(ts) FROM events WHERE session_id = $1::uuid")
            .bind(&sid)
            .fetch_one(&hub.app.db)
            .await
            .unwrap();

    let url = format!("/api/workspaces/{}", ws["id"].as_str().unwrap());
    let (s, v) = hub.patch(&url, json!({"mirror_enabled": false})).await;
    assert_eq!((s, &v["mirror_enabled"]), (200, &json!(false)), "{v}");
    let set = w.expect("mirror.set").await;
    assert_eq!(
        set["payload"],
        json!({"workspace_path": WS, "enabled": false})
    );
    // The same value again: nothing is sent.
    hub.patch(&url, json!({"mirror_enabled": false})).await;
    w.expect_silence(Duration::from_millis(200)).await;

    // On again: the hub's session with the time of its last event (no duplicates, T21).
    let (s, v) = hub
        .patch(&url, json!({"mirror_enabled": true, "label": "main"}))
        .await;
    assert_eq!(
        (s, &v["mirror_enabled"], &v["label"]),
        (200, &json!(true), &json!("main"))
    );
    let set = w.expect("mirror.set").await;
    let sessions = set["payload"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{set}");
    assert_eq!(sessions[0]["runtime_session_id"], runtime);
    let since: chrono::DateTime<chrono::Utc> =
        serde_json::from_value(sessions[0]["since_ts"].clone()).unwrap();
    assert_eq!(since, last);

    // Switched while the worker is away: it learns after `hello`.
    w.close().await;
    hub.wait_session(&sid, "worker offline", |s| s["state"] == "worker_offline")
        .await;
    hub.patch(&url, json!({"mirror_enabled": false})).await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS).await;
    w.expect_silence(Duration::from_millis(300)).await;
    assert!(w.mirror_sets.is_empty(), "{:?}", w.mirror_sets);
    w.close().await;
    hub.patch(&url, json!({"mirror_enabled": true})).await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS).await;
    w.see_mirror_set = true;
    let set = w.expect("mirror.set").await;
    assert_eq!(set["payload"]["sessions"][0]["runtime_session_id"], runtime);

    // Another user's workspace is not theirs to switch; `mirror_enabled: false` on creation.
    let (s, _) = hub
        .patch(
            &format!("/api/workspaces/{}", Uuid::now_v7()),
            json!({"mirror_enabled": true}),
        )
        .await;
    assert_eq!(s, 404);
    w.close().await;
    let (mut w, _) = FakeWorker::online_bare(&hub, &token, "/tmp/hqc-t21", hub.boot_id).await;
    let pid = hub.personal_project().await;
    let body = json!({"project_id": pid, "worker_id": w.worker_id, "path": "/tmp/hqc-t21/plain",
                      "mirror_enabled": false});
    let (s, v) = hub
        .while_worker_answers(&mut w, hub.post("/api/workspaces", body))
        .await;
    assert_eq!((s, &v["mirror_enabled"]), (201, &json!(false)), "{v}");
    w.expect_silence(Duration::from_millis(200)).await;
    hub.finish().await;
}

// Migration 0011: workspaces mirrored before are untouched, the others are switched on.
#[tokio::test]
async fn migration_turns_the_mirror_on() {
    const SEED: &str = r#"
        INSERT INTO users (id, email, display_name) VALUES
            ('0199f3a2-7c10-7000-8000-0000000000aa', '{email}', 'me');
        INSERT INTO projects (id, owner_id, name, slug, is_personal) VALUES
            ('0199f3a2-7c10-7000-8000-0000000000d1', '0199f3a2-7c10-7000-8000-0000000000aa',
             'Personal', 'personal', true);
        UPDATE users SET personal_project_id = '0199f3a2-7c10-7000-8000-0000000000d1';
        INSERT INTO workers (id, owner_id, name) VALUES
            ('0199f3a2-7c10-7000-8000-0000000000b1', '0199f3a2-7c10-7000-8000-0000000000aa',
             'home-mac');
        INSERT INTO workspaces (id, project_id, owner_id, worker_id, path, mirror_enabled) VALUES
            ('0199f3a2-7c10-7000-8000-0000000000e1', '0199f3a2-7c10-7000-8000-0000000000d1',
             '0199f3a2-7c10-7000-8000-0000000000aa', '0199f3a2-7c10-7000-8000-0000000000b1',
             '/w/a', false),
            ('0199f3a2-7c10-7000-8000-0000000000e2', '0199f3a2-7c10-7000-8000-0000000000d1',
             '0199f3a2-7c10-7000-8000-0000000000aa', '0199f3a2-7c10-7000-8000-0000000000b1',
             '/w/b', true);
    "#;
    let hub = TestHub::start_upgrading(11, SEED).await;
    let (_, list) = hub.get("/api/workspaces").await;
    let on: Vec<&Value> = list["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| &w["mirror_enabled"])
        .collect();
    assert_eq!(on, [&json!(true), &json!(true)], "{list}");
    let hash: Option<String> = sqlx::query_scalar("SELECT machine_hash FROM workers")
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
    assert_eq!(hash, None);
    hub.finish().await;
}

// ---------------------------------------------------------------- machine fingerprint

fn fingerprint(c: char) -> String {
    std::iter::repeat_n(c, 64).collect()
}

/// `POST /worker/enroll` with `machine_id`; returns `(code, poll_token)`.
async fn enroll_code(hub: &TestHub, name: &str, machine: Option<&str>) -> (String, String) {
    let mut body =
        json!({"name": name, "os": "macos", "protocol_version": hqc_proto::PROTOCOL_VERSION});
    if let Some(m) = machine {
        body["machine_id"] = json!(m);
    }
    let (s, e) = hub.post("/worker/enroll", body).await;
    assert_eq!(s, 200, "{e}");
    (
        e["code"].as_str().unwrap().to_string(),
        e["poll_token"].as_str().unwrap().to_string(),
    )
}

/// A full enrollment with `machine_id`; returns `(worker_id, token)`.
async fn enroll_machine(hub: &TestHub, name: &str, machine: Option<&str>) -> (String, String) {
    let (code, poll) = enroll_code(hub, name, machine).await;
    let (s, c) = hub
        .post("/api/workers/enroll/confirm", json!({"code": code}))
        .await;
    assert_eq!(s, 200, "{c}");
    let (_, p) = hub
        .post("/worker/enroll/poll", json!({"poll_token": poll}))
        .await;
    (
        p["worker_id"].as_str().unwrap().to_string(),
        p["token"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn replaceable_workers_of_the_same_machine_come_first() {
    let hub = TestHub::start().await;
    let (m1, m2) = (fingerprint('a'), fingerprint('b'));
    let (other, _) = enroll_machine(&hub, "llm-server", Some(&m2)).await;
    let (older, older_token) = enroll_machine(&hub, "old-mac", None).await;
    let (same, _) = enroll_machine(&hub, "home-mac", Some(&m1.to_uppercase())).await;

    let info = |code: String| {
        let hub = &hub;
        async move {
            let (s, v) = hub.get(&format!("/api/workers/enroll/{code}")).await;
            assert_eq!(s, 200, "{v}");
            v["replaceable"].as_array().unwrap().clone()
        }
    };
    let (code, _) = enroll_code(&hub, "home-mac", Some(&m1)).await;
    let r = info(code).await;
    assert_eq!(r.len(), 3, "{r:?}");
    assert_eq!(
        (&r[0]["id"], &r[0]["same_machine"]),
        (&json!(same), &json!(true))
    );
    assert!(
        r[1..].iter().all(|w| w.get("same_machine").is_none()),
        "{r:?}"
    );
    // Without a fingerprint nothing is the same machine.
    let (code, _) = enroll_code(&hub, "x", None).await;
    assert!(info(code)
        .await
        .iter()
        .all(|w| w.get("same_machine").is_none()));
    // A malformed fingerprint is ignored.
    let (code, _) = enroll_code(&hub, "x", Some("../../etc")).await;
    assert!(info(code)
        .await
        .iter()
        .all(|w| w.get("same_machine").is_none()));

    // A worker enrolled before T21 gets its fingerprint from its first `hello`.
    let mut w = FakeWorker::connect(&hub, &older_token).await.unwrap();
    w.send(
        "hello",
        json!({"protocol_version": hqc_proto::PROTOCOL_VERSION, "boot_id": Uuid::new_v4(),
               "capabilities": {"acp_agents": []}, "allowed_roots": ["/tmp"],
               "machine_id": fingerprint('c')}),
    )
    .await;
    w.expect("hello.ack").await;
    w.close().await;
    // Once the hub has seen it go offline.
    let deadline = tokio::time::Instant::now() + WAIT;
    let r = loop {
        let (code, _) = enroll_code(&hub, "old-mac", Some(&fingerprint('c'))).await;
        let r = info(code).await;
        if r.len() == 3 || tokio::time::Instant::now() > deadline {
            break r;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        (&r[0]["id"], &r[0]["same_machine"]),
        (&json!(older), &json!(true)),
        "{r:?}"
    );
    assert_ne!(r[1]["id"], json!(older));
    let _ = other;
    hub.finish().await;
}

// ---------------------------------------------------------------- titles

#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<(String, HeaderMap, Value)>>>);

/// A fake model server: `reply(body)` gives the response; returns its base URL.
async fn model_server(seen: Seen, status: u16, delay: Duration, reply: Value) -> String {
    let handler = move |path: &'static str| {
        let (seen, reply) = (seen.clone(), reply.clone());
        move |headers: HeaderMap, Json(body): Json<Value>| {
            let (seen, reply) = (seen.clone(), reply.clone());
            async move {
                seen.0
                    .lock()
                    .unwrap()
                    .push((path.to_string(), headers, body));
                tokio::time::sleep(delay).await;
                (
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    Json(reply),
                )
            }
        }
    };
    let app = Router::new()
        .route("/v1/messages", post(handler("/v1/messages")))
        .route("/chat/completions", post(handler("/chat/completions")));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

fn llm(provider: Provider, base: &str, key: Option<&str>, timeout: Duration) -> LlmConfig {
    LlmConfig {
        provider,
        model: "test-model".into(),
        base_url: base.into(),
        api_key: key.map(String::from),
        timeout,
    }
}

/// A session with the default title, a prompt and the first answer; returns it once its
/// title changed (or after `wait` without a change).
async fn titled(hub: &TestHub, w: &mut FakeWorker, prompt: &str, body: Value) -> Value {
    let sid = new_session(hub, body).await;
    let before = hub.session(&sid).await["title"].clone();
    input(hub, w, &sid, prompt, json!([])).await;
    answer(w, &sid, "Готово: сборка снова зелёная.").await;
    if hub.session(&sid).await["title_source"] == "user" {
        tokio::time::sleep(Duration::from_millis(300)).await;
        return hub.session(&sid).await;
    }
    hub.wait_session(&sid, "a title", |s| s["title"] != before)
        .await
}

#[tokio::test]
async fn title_from_an_anthropic_model() {
    let seen = Seen::default();
    let base = model_server(
        seen.clone(),
        200,
        Duration::ZERO,
        json!({"content": [{"type": "text", "text": "\"Починка сборки\""}]}),
    )
    .await;
    let cfg = llm(
        Provider::Anthropic,
        &base,
        Some("sk-test"),
        Duration::from_secs(5),
    );
    let hub = TestHub::start_config(|c| c.title_model = Some(cfg)).await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS).await;
    let long = "почини сборку ".repeat(300);
    let s = titled(&hub, &mut w, &long, json!({})).await;
    assert_eq!(
        (&s["title"], &s["title_source"]),
        (&json!("Починка сборки"), &json!("runtime"))
    );
    let calls = seen.0.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    let (path, headers, body) = &calls[0];
    assert_eq!(path, "/v1/messages");
    assert_eq!(headers["x-api-key"], "sk-test");
    assert_eq!(headers["anthropic-version"], "2023-06-01");
    assert_eq!(body["model"], "test-model");
    assert!(body["system"].as_str().unwrap().contains("60 characters"));
    let user = body["messages"][0]["content"].as_str().unwrap();
    assert!(user.contains("Готово: сборка снова зелёная."), "{user}");
    // Each part cut to 2000 characters.
    assert!(user.chars().count() < 2200, "{}", user.chars().count());
    // The next answer does not ask again.
    let sid = s["id"].as_str().unwrap();
    input(&hub, &mut w, sid, "ещё", json!([])).await;
    answer(&mut w, sid, "ok").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(seen.0.lock().unwrap().len(), 1);
    hub.finish().await;
}

#[tokio::test]
async fn title_from_an_openai_compatible_model() {
    let seen = Seen::default();
    let base = model_server(
        seen.clone(),
        200,
        Duration::ZERO,
        json!({"choices": [{"message": {"role": "assistant", "content": "«Кронштейн камеры»\n"}}]}),
    )
    .await;
    // A local model: no key.
    let cfg = llm(Provider::OpenAi, &base, None, Duration::from_secs(5));
    let hub = TestHub::start_config(|c| c.title_model = Some(cfg)).await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS).await;
    let s = titled(&hub, &mut w, "Сделай кронштейн для камеры", json!({})).await;
    assert_eq!(s["title"], "Кронштейн камеры");
    let calls = seen.0.lock().unwrap().clone();
    let (path, headers, body) = &calls[0];
    assert_eq!(path, "/chat/completions");
    assert!(!headers.contains_key("authorization"));
    assert_eq!(body["messages"][0]["role"], "system");
    assert!(body["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("Сделай кронштейн для камеры"));
    hub.finish().await;
}

#[tokio::test]
async fn title_falls_back_to_the_first_prompt() {
    // No model.
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS).await;
    let s = titled(
        &hub,
        &mut w,
        "## **Почини** сборку\nподробности ниже",
        json!({}),
    )
    .await;
    assert_eq!(
        (&s["title"], &s["title_source"]),
        (&json!("Почини сборку"), &json!("runtime"))
    );
    // The user's title stays.
    let s = titled(&hub, &mut w, "что-то", json!({"title": "Мой заголовок"})).await;
    assert_eq!(
        (&s["title"], &s["title_source"]),
        (&json!("Мой заголовок"), &json!("user"))
    );
    // So does a runtime's (`ai-title`) that came first.
    let sid = new_session(&hub, json!({})).await;
    input(&hub, &mut w, &sid, "prompt text", json!([])).await;
    w.send(
        "session.state",
        json!({"session_id": sid, "process_state": "running", "locked_by": "hqc",
               "title": "From the runtime"}),
    )
    .await;
    w.expect("ack").await;
    answer(&mut w, &sid, "done").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(hub.session(&sid).await["title"], "From the runtime");
    let long = "Сделай кронштейн для крепления камеры на трубу диаметром двадцать пять миллиметров";
    let s = titled(&hub, &mut w, long, json!({})).await;
    let t = s["title"].as_str().unwrap();
    assert!(t.ends_with('…') && t.chars().count() <= 60, "{t}");

    // An imported session is titled by its first records the same way.
    let runtime = Uuid::new_v4();
    for (ty, payload) in [
        (
            "user.message",
            json!({"text": "Импортированный вопрос", "artifact_ids": []}),
        ),
        ("agent.message", json!({"text": "ответ", "final": true})),
    ] {
        let id = w
            .send(
                "mirror.event",
                json!({"workspace_path": WS, "runtime_session_id": runtime, "cwd": WS,
                       "recent": false,
                       "event": {"session_id": Uuid::nil(), "event_id": Uuid::new_v4(),
                                 "ts": "2026-10-23T00:10:15Z",
                                 "author": {"kind": "agent", "id": Uuid::nil()},
                                 "type": ty, "payload": payload}}),
            )
            .await;
        let ack = w.expect("ack").await;
        assert_eq!(ack["id"], id);
    }
    let pid = hub.personal_project().await;
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let (_, l) = hub.get(&format!("/api/projects/{pid}/sessions")).await;
        let found = l["sessions"].as_array().unwrap().iter().any(|s| {
            s["agent"]["runtime_session_id"] == json!(runtime.to_string())
                && s["title"] == "Импортированный вопрос"
        });
        if found {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{l}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    hub.finish().await;
}

#[tokio::test]
async fn failing_or_slow_model_falls_back() {
    for (status, delay) in [(500, Duration::ZERO), (200, Duration::from_secs(3))] {
        let seen = Seen::default();
        let base = model_server(
            seen.clone(),
            status,
            delay,
            json!({"content": [{"type": "text", "text": "late title"}],
                   "error": {"message": "overloaded"}}),
        )
        .await;
        let cfg = llm(
            Provider::Anthropic,
            &base,
            Some("k"),
            Duration::from_millis(300),
        );
        let hub = TestHub::start_config(|c| c.title_model = Some(cfg)).await;
        let (_, token) = hub.enroll("home-mac").await;
        let (mut w, _) = FakeWorker::online(&hub, &token, WS).await;
        let s = titled(&hub, &mut w, "Fix the flaky test", json!({})).await;
        assert_eq!(s["title"], "Fix the flaky test", "status {status}");
        assert_eq!(seen.0.lock().unwrap().len(), 1);
        hub.finish().await;
    }
}

// ---------------------------------------------------------------- artifacts

fn blob(hub: &TestHub, sha: &str) -> std::path::PathBuf {
    hub.app.config.artifacts.dir.join(&sha[..2]).join(sha)
}

#[tokio::test]
async fn cleanup_removes_only_old_unreferenced_artifacts() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS).await;
    let up = |name: &'static str, bytes: &'static [u8]| {
        let hub = &hub;
        async move {
            let (s, a) = hub.upload(name, "text/plain", bytes.to_vec(), None).await;
            assert!(s == 201, "{a}");
            a
        }
    };
    let used = up("used.txt", b"shared content").await;
    let twin = up("twin.txt", b"shared content").await; // same file, another name
    let lone = up("lone.txt", b"nobody refers to me").await;
    let fresh = up("fresh.txt", b"uploaded just now").await;
    let shown = up("shown.txt", b"shown by an agent").await;
    assert_eq!(used["sha256"], twin["sha256"]);
    let sid = hub.create_session("files").await;
    input(&hub, &mut w, &sid, "see", json!([used["id"]])).await;
    w.event(agent_event(
        &sid,
        "attachment",
        json!({"artifact_id": shown["id"], "kind": "text", "name": "shown.txt"}),
    ))
    .await;
    sqlx::query(
        "UPDATE artifacts SET updated_at = now() - interval '40 days' WHERE id <> $1::uuid",
    )
    .bind(fresh["id"].as_str().unwrap())
    .execute(&hub.app.db)
    .await
    .unwrap();

    let keep = Duration::from_secs(30 * 86_400);
    let n = hqc_hub::artifacts::cleanup(&hub.app, keep).await.unwrap();
    assert_eq!(n, 2, "twin and lone");
    let left: Vec<String> = sqlx::query_scalar("SELECT name FROM artifacts ORDER BY name")
        .fetch_all(&hub.app.db)
        .await
        .unwrap();
    assert_eq!(left, ["fresh.txt", "shown.txt", "used.txt"]);
    let sha = |a: &Value| a["sha256"].as_str().unwrap().to_string();
    assert!(
        blob(&hub, &sha(&used)).exists(),
        "still used by another row"
    );
    assert!(!blob(&hub, &sha(&lone)).exists());
    assert!(blob(&hub, &sha(&fresh)).exists());
    assert!(blob(&hub, &sha(&shown)).exists());
    // Nothing else is touched in the store, and a second run finds nothing.
    assert_eq!(
        hqc_hub::artifacts::cleanup(&hub.app, keep).await.unwrap(),
        0
    );
    // The same content uploaded again after its file went is stored anew.
    let again = up("lone.txt", b"nobody refers to me").await;
    assert!(blob(&hub, &sha(&again)).exists());
    hub.finish().await;
}

// ---------------------------------------------------------------- session.cleanup

#[tokio::test]
async fn archive_and_delete_clean_the_inbox() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS).await;
    w.see_cleanup = true;
    let (_, a) = hub.upload("a.png", "image/png", vec![1, 2, 3], None).await;

    let sid = hub.create_session("with files").await;
    input(&hub, &mut w, &sid, "plain", json!([])).await;
    let ev = input(&hub, &mut w, &sid, "look", json!([a["id"]])).await;
    let (s, _) = hub
        .post(&format!("/api/sessions/{sid}/archive"), json!({}))
        .await;
    assert_eq!(s, 200);
    w.expect("session.kill").await;
    let c = w.expect("session.cleanup").await;
    assert_eq!(
        c["payload"],
        json!({"session_id": sid, "workspace_path": WS, "cwd": ".",
               "event_ids": [ev["event_id"]]})
    );

    // Deleting does the same; a session without files sends nothing.
    let other = hub.create_session("no files").await;
    input(&hub, &mut w, &other, "plain", json!([])).await;
    assert_eq!(hub.delete(&format!("/api/sessions/{other}")).await, 204);
    w.expect("session.kill").await;
    w.expect_silence(Duration::from_millis(200)).await;
    assert_eq!(hub.delete(&format!("/api/sessions/{sid}")).await, 204);
    w.expect("session.kill").await;
    let c = w.expect("session.cleanup").await;
    assert_eq!(c["payload"]["event_ids"], json!([ev["event_id"]]));
    hub.finish().await;
}
