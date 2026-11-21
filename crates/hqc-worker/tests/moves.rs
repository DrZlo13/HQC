//! A session moves to another folder (SPEC §5.3, §8.1): `session.start` with another
//! `workspace_path` for a session the worker knows. Fake hub, fake adapters; the transcript is
//! the `adapter-then-outside` fixture (2.1.308) with its `cwd` in the test's workspace.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use hqc_proto::event::EventKind;
use hqc_proto::frame::{Ack, HeldBy, Hello, LockedBy, ProcessState, WorkerFrame};
use hqc_worker::adapter::claude_code::paths::encode_project_dir;
use hqc_worker::adapter::fake::{agent_event, agent_message, FakeAdapters, FakeCall};
use hqc_worker::adapter::AdapterEvent;
use hqc_worker::connection::{Connection, Status, MAX_IN_FLIGHT};
use hqc_worker::inbox::ArtifactClient;
use hqc_worker::known_paths::KnownPaths;
use hqc_worker::link::HubLink;
use hqc_worker::mirror::store::Store;
use hqc_worker::outbox::Outbox;
use hqc_worker::supervisor::{wire_error, Inbound, Supervisor, LOCKED_VSCODE};
use hqc_worker::token::Secret;
use hqc_worker::workspaces::Created;
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const TRANSCRIPT: &str =
    include_str!("../../hqc-claude-jsonl/fixtures/adapter-then-outside-2.1.308.jsonl");
const FIXTURE_CWD: &str = "/tmp/hqc-fixture/adapter-then-outside";
const RUNTIME: &str = "0ac7a6fd-7974-49ec-8a6c-8c8d141d6630";

/// Transcript `RUNTIME` under the mirror's projects folder in `folder`, written in `cwd`.
fn transcript(env: &Env, folder: &str, cwd: &Path) -> PathBuf {
    let (projects, _) = env.mirror.clone().unwrap();
    let file = projects.join(folder).join(format!("{RUNTIME}.jsonl"));
    write(
        &file,
        &TRANSCRIPT.replace(FIXTURE_CWD, &cwd.display().to_string()),
    );
    file
}

/// Where Claude Code keeps transcript `RUNTIME` of working directory `dir`.
fn place(env: &Env, dir: &Path) -> PathBuf {
    let (projects, _) = env.mirror.clone().unwrap();
    projects
        .join(encode_project_dir(&dir.display().to_string()))
        .join(format!("{RUNTIME}.jsonl"))
}

/// A second folder inside `allowed_roots`.
fn other(env: &Env) -> PathBuf {
    let p = env.root().join("other");
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn store(env: &Env) -> Store {
    Store::open(&env.outbox().with_file_name("mirror")).unwrap()
}

fn start_in(env: &Env, sid: Uuid, folder: &Path) -> Value {
    let mut p = env.start_payload(sid);
    p["runtime_session_id"] = json!(RUNTIME);
    p["mode"] = json!("resume");
    p["workspace_path"] = json!(folder.display().to_string());
    p
}

fn input(sid: Uuid, eid: Uuid, text: &str) -> Value {
    json!({"session_id": sid, "event_id": eid, "text": text, "artifact_ids": [], "peer_prefix": []})
}

/// The `ack` or `error` to frame `id`; other frames read meanwhile go to `skipped`.
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
        c.skipped.push(f);
    }
}

async fn started(c: &mut HubConn, start: Value) {
    let id = c.frame("session.start", start).await;
    let r = reply_to(c, &id).await;
    assert_eq!(r["type"], "ack", "{r}");
}

async fn mirror_set(c: &mut HubConn, path: &Path) {
    let id = c
        .frame(
            "mirror.set",
            json!({"workspace_path": path.display().to_string(), "enabled": true}),
        )
        .await;
    assert_eq!(reply_to(c, &id).await["type"], "ack");
}

fn workdirs(adapters: &FakeAdapters) -> Vec<PathBuf> {
    adapters
        .start_specs()
        .into_iter()
        .map(|s| s.workdir)
        .collect()
}

/// While a turn runs the move waits; when it ends the agent is stopped and started again in
/// the new folder, its transcript and subagents' folder moved where that folder puts them, and
/// the mirror's row for the new path written: the new folder, the offset at the end.
#[tokio::test]
async fn a_running_session_moves_after_its_turn() {
    let env = Env::with_mirror();
    let (ws, other) = (env.workspace(), other(&env));
    let from = transcript(&env, &encode_project_dir(&ws.display().to_string()), &ws);
    write(
        &from.with_file_name(RUNTIME).join("subagents/agent-1.jsonl"),
        "{}\n",
    );
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    started(&mut c, start_in(&env, sid, &ws)).await;
    let a = Uuid::now_v7();
    c.frame("session.input", input(sid, a, "A")).await;
    adapters
        .wait_for("input A", |c| {
            c.contains(&FakeCall::Input {
                session_id: sid,
                event_id: a,
            })
        })
        .await;
    adapters.accept_input(sid, a).await;

    started(&mut c, start_in(&env, sid, &other)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(ends(&adapters.calls(), sid) == 0, "not during the turn");
    assert!(from.is_file());

    adapters.end_turn(sid).await;
    adapters
        .wait_for("the start in the new folder", |_| {
            adapters.start_specs().len() == 2
        })
        .await;
    assert_eq!(workdirs(&adapters), [ws.clone(), other.clone()]);
    // Nothing runs after the turn: the process is closed through its stdin, not killed.
    let calls = adapters.calls();
    assert!(calls.contains(&FakeCall::Close(sid)), "{calls:?}");
    assert!(!calls.contains(&FakeCall::Kill(sid)), "{calls:?}");
    let to = place(&env, &other);
    assert!(to.is_file() && !from.exists(), "{}", to.display());
    assert!(to
        .with_file_name(RUNTIME)
        .join("subagents/agent-1.jsonl")
        .is_file());
    assert!(!from.with_file_name(RUNTIME).exists());
    let row = store(&env).get(&to).unwrap().expect("the mirror's row");
    assert_eq!(row.cwd, Some(other.display().to_string()));
    assert_eq!(row.offset, std::fs::metadata(&to).unwrap().len());

    // Back, between turns: at once. Its records still name the first folder; the row does not.
    started(&mut c, start_in(&env, sid, &ws)).await;
    adapters
        .wait_for("the start back in the first folder", |_| {
            adapters.start_specs().len() == 3
        })
        .await;
    assert!(from.is_file() && !to.exists());
    w.stop().await;
}

/// An input that comes while a move waits is not written into the running turn (SPEC §17.2):
/// it is for the agent in the new folder (the one after `hqc_move`, §7), which gets it as its
/// first turn there.
#[tokio::test]
async fn input_during_a_waiting_move_goes_to_the_new_folder() {
    let env = Env::with_mirror();
    let (ws, other) = (env.workspace(), other(&env));
    transcript(&env, &encode_project_dir(&ws.display().to_string()), &ws);
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    started(&mut c, start_in(&env, sid, &ws)).await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    c.frame("session.input", input(sid, a, "A")).await;
    adapters
        .wait_for("input A", |c| {
            c.contains(&FakeCall::Input {
                session_id: sid,
                event_id: a,
            })
        })
        .await;
    adapters.accept_input(sid, a).await;

    started(&mut c, start_in(&env, sid, &other)).await;
    c.frame("session.input", input(sid, b, "you moved")).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let injected = FakeCall::Inject {
        session_id: sid,
        event_id: b,
    };
    assert!(
        !adapters.calls().contains(&injected),
        "{:?}",
        adapters.calls()
    );

    adapters.end_turn(sid).await;
    let b_input = FakeCall::Input {
        session_id: sid,
        event_id: b,
    };
    let calls = adapters
        .wait_for("B as a turn", |c| c.contains(&b_input))
        .await;
    assert_eq!(workdirs(&adapters), [ws, other]);
    let at = |call: &FakeCall| calls.iter().position(|c| c == call).unwrap();
    let end = calls.iter().position(|c| is_end(c, sid)).unwrap();
    assert!(end < at(&b_input), "{calls:?}");
    w.stop().await;
}

/// A move cancelled while it waits (the first folder again) lets go the input it held: into
/// the running turn, as with no move, and the process is not stopped.
#[tokio::test]
async fn a_cancelled_move_lets_the_held_input_go() {
    let env = Env::new();
    let (ws, other) = (env.workspace(), other(&env));
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    started(&mut c, start_in(&env, sid, &ws)).await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    c.frame("session.input", input(sid, a, "A")).await;
    adapters
        .wait_for("input A", |c| {
            c.contains(&FakeCall::Input {
                session_id: sid,
                event_id: a,
            })
        })
        .await;
    adapters.accept_input(sid, a).await;

    started(&mut c, start_in(&env, sid, &other)).await;
    c.frame("session.input", input(sid, b, "B")).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let injected = FakeCall::Inject {
        session_id: sid,
        event_id: b,
    };
    assert!(
        !adapters.calls().contains(&injected),
        "{:?}",
        adapters.calls()
    );

    started(&mut c, start_in(&env, sid, &ws)).await;
    adapters
        .wait_for("B into A's turn", |c| c.contains(&injected))
        .await;
    assert_eq!(ends(&adapters.calls(), sid), 0);
    assert_eq!(workdirs(&adapters), [ws]);
    w.stop().await;
}

/// A background subagent outlives the turn: the move waits for it, and no input starts a turn
/// in the old folder meanwhile. Nor does the end of the last subagent move the agent: the
/// runtime reports it before the turn it then runs by itself on the subagent's result, and the
/// end of that turn moves it. When no such turn comes, the next input does, and goes to the new
/// process.
#[tokio::test]
async fn a_move_waits_for_a_background_subagent() {
    // `before`: input B before the subagent's end, then a turn of the runtime's own; `after`: B
    // after the subagent's end, no turn of the runtime's own; `during`: B before the
    // subagent's end, which comes while a turn of the runtime's own runs.
    for case in ["before", "after", "during"] {
        let env = Env::with_mirror();
        let (ws, other) = (env.workspace(), other(&env));
        transcript(&env, &encode_project_dir(&ws.display().to_string()), &ws);
        let adapters = FakeAdapters::new();
        let mut hub = FakeHub::start().await;
        let w = start_worker(&env, &hub.url(), &adapters);
        let mut c = hub.accept().await;
        c.handshake().await;
        let sid = Uuid::now_v7();
        started(&mut c, start_in(&env, sid, &ws)).await;
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let given = |e: Uuid| FakeCall::Input {
            session_id: sid,
            event_id: e,
        };
        c.frame("session.input", input(sid, a, "A")).await;
        adapters
            .wait_for("input A", |c| c.contains(&given(a)))
            .await;
        adapters.accept_input(sid, a).await;
        adapters.emit(sid, AdapterEvent::Subagents(1)).await;

        started(&mut c, start_in(&env, sid, &other)).await;
        adapters.end_turn(sid).await;
        let mut fb = None;
        if case != "after" {
            fb = Some(c.frame("session.input", input(sid, b, "B")).await);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            ends(&adapters.calls(), sid),
            0,
            "{case}: not while the subagent runs"
        );
        assert!(
            !adapters.calls().contains(&given(b)),
            "{case}: no turn in the old folder: {:?}",
            adapters.calls()
        );

        adapters.set_own_turn(case == "during");
        adapters.emit(sid, AdapterEvent::Subagents(0)).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            ends(&adapters.calls(), sid),
            0,
            "{case}: not at the subagent's end"
        );
        assert!(
            !adapters.calls().contains(&given(b)),
            "{case}: {:?}",
            adapters.calls()
        );
        if case == "after" {
            fb = Some(c.frame("session.input", input(sid, b, "B")).await);
        } else {
            // The runtime's own turn on the subagent's result begins (`before`) and ends.
            adapters.set_own_turn(true);
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(ends(&adapters.calls(), sid), 0, "{case}: not during it");
            adapters.set_own_turn(false);
            adapters.end_turn(sid).await;
        }
        let calls = adapters
            .wait_for(&format!("{case}: B in the new folder"), |c| {
                c.contains(&given(b))
            })
            .await;
        assert_eq!(workdirs(&adapters), [ws, other]);
        let at = |x: &FakeCall| calls.iter().position(|c| c == x).unwrap();
        let end = calls.iter().position(|c| is_end(c, sid)).unwrap();
        assert!(end < at(&given(b)), "{calls:?}");
        adapters.accept_input(sid, b).await;
        let r = reply_to(&mut c, &fb.unwrap()).await;
        assert_eq!(r["type"], "ack", "{r}");
        w.stop().await;
    }
}

/// A turn the runtime starts by itself after a background task's end holds the move as a turn
/// of ours does: the agent moves at its end, not in the middle of it.
#[tokio::test]
async fn a_move_waits_for_a_turn_the_runtime_starts_by_itself() {
    let env = Env::new();
    let (ws, other) = (env.workspace(), other(&env));
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    started(&mut c, start_in(&env, sid, &ws)).await;
    adapters.emit(sid, AdapterEvent::BackgroundTasks(1)).await;
    adapters.emit(sid, AdapterEvent::BackgroundTasks(0)).await;
    adapters.set_own_turn(true);

    started(&mut c, start_in(&env, sid, &other)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        ends(&adapters.calls(), sid),
        0,
        "not during the runtime's own turn"
    );

    adapters.set_own_turn(false);
    adapters.end_turn(sid).await;
    adapters
        .wait_for("the start in the new folder", |_| {
            adapters.start_specs().len() == 2
        })
        .await;
    assert_eq!(workdirs(&adapters), [ws, other]);
    w.stop().await;
}

/// A Stop asked while no turn runs is the old process's: an input the new process drops after
/// a move goes back to the queue, as with no Stop, instead of failing as cancelled by it.
#[tokio::test]
async fn a_stop_before_a_move_cancels_nothing_after_it() {
    let env = Env::new();
    let (ws, other) = (env.workspace(), other(&env));
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    started(&mut c, start_in(&env, sid, &ws)).await;
    let stop = c.frame("session.stop", json!({"session_id": sid})).await;
    assert_eq!(reply_to(&mut c, &stop).await["type"], "ack");
    assert!(adapters.calls().contains(&FakeCall::Stop(sid)));
    started(&mut c, start_in(&env, sid, &other)).await;
    adapters
        .wait_for("the start in the new folder", |_| {
            adapters.start_specs().len() == 2
        })
        .await;

    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let given = |e: Uuid| FakeCall::Input {
        session_id: sid,
        event_id: e,
    };
    c.frame("session.input", input(sid, a, "A")).await;
    adapters
        .wait_for("input A", |c| c.contains(&given(a)))
        .await;
    adapters.accept_input(sid, a).await;
    let fb = c.frame("session.input", input(sid, b, "B")).await;
    adapters
        .wait_for("B into A's turn", |c| {
            c.contains(&FakeCall::Inject {
                session_id: sid,
                event_id: b,
            })
        })
        .await;
    adapters.end_turn_carrying(sid, vec![], vec![b]).await;
    adapters
        .wait_for("B again, as the next turn", |c| c.contains(&given(b)))
        .await;
    adapters.accept_input(sid, b).await;
    let r = reply_to(&mut c, &fb).await;
    assert_eq!(r["type"], "ack", "{r}");
    w.stop().await;
}

/// A background command of the agent itself does not hold the move: it dies with the process,
/// and the first input in the new folder that is no slash command tells the agent so; the
/// next one does not. A command started during the turn a move waits for counts too.
#[tokio::test]
async fn a_move_stops_background_commands_and_tells_the_agent() {
    let env = Env::with_mirror();
    let (ws, other) = (env.workspace(), other(&env));
    transcript(&env, &encode_project_dir(&ws.display().to_string()), &ws);
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    started(&mut c, start_in(&env, sid, &ws)).await;
    adapters.emit(sid, AdapterEvent::BackgroundTasks(2)).await;

    started(&mut c, start_in(&env, sid, &other)).await;
    adapters
        .wait_for("the start in the new folder", |_| {
            adapters.start_specs().len() == 2
        })
        .await;
    // Background commands die with the process: it is killed, not left to exit by itself.
    let calls = adapters.calls();
    assert!(calls.contains(&FakeCall::Kill(sid)), "{calls:?}");
    assert!(!calls.contains(&FakeCall::Close(sid)), "{calls:?}");
    /// Input `text` goes to the runtime, which takes it: the text it got.
    async fn given(c: &mut HubConn, adapters: &FakeAdapters, sid: Uuid, text: &str) -> String {
        let e = Uuid::now_v7();
        c.frame("session.input", input(sid, e, text)).await;
        adapters
            .wait_for("the input", |c| {
                c.contains(&FakeCall::Input {
                    session_id: sid,
                    event_id: e,
                })
            })
            .await;
        adapters.accept_input(sid, e).await;
        adapters.inputs().last().unwrap().0.text.clone()
    }
    let note = |folder: &Path, n: u32| {
        format!(
            "[hqc] Moving this session to {} stopped {n} background task(s)",
            folder.display()
        )
    };
    assert_eq!(given(&mut c, &adapters, sid, "/compact").await, "/compact");
    adapters.end_turn(sid).await;
    let a = given(&mut c, &adapters, sid, "A").await;
    assert!(
        a.starts_with(&note(&other, 2)) && a.ends_with("\n---\n\nA"),
        "{a}"
    );
    adapters.end_turn(sid).await;
    assert_eq!(given(&mut c, &adapters, sid, "B").await, "B");

    // Back during the turn of B; a command started in it dies at its end.
    started(&mut c, start_in(&env, sid, &ws)).await;
    adapters.emit(sid, AdapterEvent::BackgroundTasks(1)).await;
    adapters.end_turn(sid).await;
    adapters
        .wait_for("the start back in the first folder", |_| {
            adapters.start_specs().len() == 3
        })
        .await;
    let c2 = given(&mut c, &adapters, sid, "C").await;
    assert!(c2.starts_with(&note(&ws, 1)), "{c2}");
    w.stop().await;
}

/// The note of a move waits for the runtime to take it: an input written into the turn of the
/// one that carries it goes without it, and both go again, the note once, after the process
/// died before taking them.
#[tokio::test]
async fn the_move_note_goes_once_and_outlives_a_crash() {
    let env = Env::with_mirror();
    let (ws, other) = (env.workspace(), other(&env));
    transcript(&env, &encode_project_dir(&ws.display().to_string()), &ws);
    let adapters = FakeAdapters::new();
    adapters.set_injectable(true);
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    started(&mut c, start_in(&env, sid, &ws)).await;
    adapters.emit(sid, AdapterEvent::BackgroundTasks(1)).await;
    started(&mut c, start_in(&env, sid, &other)).await;
    adapters
        .wait_for("the start in the new folder", |_| {
            adapters.start_specs().len() == 2
        })
        .await;
    let sent = |e: Uuid| -> Vec<String> {
        adapters
            .inputs()
            .into_iter()
            .filter(|(i, _)| i.event_id == e)
            .map(|(i, _)| i.text)
            .collect()
    };
    let note = format!("[hqc] Moving this session to {} stopped 1", other.display());
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    c.frame("session.input", input(sid, a, "A")).await;
    c.frame("session.input", input(sid, b, "B")).await;
    adapters
        .wait_for("B into A's turn", |c| {
            c.contains(&FakeCall::Inject {
                session_id: sid,
                event_id: b,
            })
        })
        .await;

    adapters
        .emit(
            sid,
            AdapterEvent::Exited {
                error: Some("crashed".into()),
            },
        )
        .await;
    adapters.wait_for("B again", |_| sent(b).len() == 2).await;
    let (sa, sb) = (sent(a), sent(b));
    assert_eq!(sa.len(), 2, "{sa:?}");
    assert!(sa.iter().all(|t| t.starts_with(&note)), "{sa:?}");
    assert_eq!(sb, ["B", "B"]);
    w.stop().await;
}

/// The worker restarted after the hub moved the session: the new process has never seen the
/// old folder and finds the transcript by its uuid.
#[tokio::test]
async fn a_move_across_a_worker_restart() {
    let env = Env::with_mirror();
    let (ws, other) = (env.workspace(), other(&env));
    let from = transcript(&env, &encode_project_dir(&ws.display().to_string()), &ws);
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let sid = Uuid::now_v7();
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    started(&mut c, start_in(&env, sid, &ws)).await;
    w.stop().await;

    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    started(&mut c, start_in(&env, sid, &other)).await;
    assert_eq!(workdirs(&adapters), [ws, other.clone()]);
    assert!(place(&env, &other).is_file() && !from.exists());
    w.stop().await;
}

/// The old folder is gone (a spawned worktree deleted once the session left it): the
/// transcript still moves, its folder judged by the deepest ancestor that exists. One that
/// was outside `allowed_roots` stays refused.
#[tokio::test]
async fn a_move_out_of_a_folder_that_is_gone() {
    let env = Env::with_mirror();
    let (gone, other) = (env.root().join("gone"), other(&env));
    std::fs::create_dir_all(&gone).unwrap();
    let from = transcript(
        &env,
        &encode_project_dir(&gone.display().to_string()),
        &gone,
    );
    std::fs::remove_dir_all(&gone).unwrap();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    started(&mut c, start_in(&env, Uuid::now_v7(), &other)).await;
    assert!(place(&env, &other).is_file() && !from.exists());

    let outside = env.dir.path().canonicalize().unwrap().join("outside/gone");
    std::fs::remove_file(place(&env, &other)).unwrap();
    let elsewhere = transcript(&env, "-elsewhere", &outside);
    let id = c
        .frame("session.start", start_in(&env, Uuid::now_v7(), &other))
        .await;
    let r = reply_to(&mut c, &id).await;
    assert_eq!(r["payload"]["code"], "agent_start_failed", "{r}");
    assert!(elsewhere.is_file());
    w.stop().await;
}

/// The mirror follows both folders and has imported the transcript: after the move it reads
/// the new path from its row, at the end, and sends nothing again.
#[tokio::test]
async fn the_mirror_does_not_import_a_moved_transcript_again() {
    let env = Env::with_mirror();
    let (ws, other) = (env.workspace(), other(&env));
    let from = transcript(&env, &encode_project_dir(&ws.display().to_string()), &ws);
    let old = std::time::SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&from)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    mirror_set(&mut c, &ws).await;
    mirror_set(&mut c, &other).await;
    for _ in 0..6 {
        let f = c.expect("mirror.event").await;
        c.send(json!({"id": f["id"], "type": "ack",
            "payload": {"event_id": f["payload"]["event"]["event_id"], "seq": 1}}))
            .await;
    }

    let sid = Uuid::now_v7();
    started(&mut c, start_in(&env, sid, &ws)).await;
    started(&mut c, start_in(&env, sid, &other)).await;
    adapters
        .wait_for("the start in the new folder", |_| {
            adapters.start_specs().len() == 2
        })
        .await;
    assert!(place(&env, &other).is_file() && !from.exists());
    assert!(c
        .maybe("mirror.event", Duration::from_millis(1500))
        .await
        .is_none());
    assert_eq!(ends(&adapters.calls(), sid), 1, "only the move's");
    w.stop().await;
}

/// A transcript the worker does not move: one written outside `allowed_roots`, or one of two
/// with that uuid. The start fails (`agent_start_failed`) and the files stay.
#[tokio::test]
async fn a_transcript_that_cannot_move_stays() {
    let env = Env::with_mirror();
    let outside = env.dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let outside = outside.canonicalize().unwrap();
    let elsewhere = transcript(&env, "-elsewhere", &outside);
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;

    let sid = Uuid::now_v7();
    let id = c
        .frame("session.start", start_in(&env, sid, &env.workspace()))
        .await;
    let r = reply_to(&mut c, &id).await;
    assert_eq!(r["payload"]["code"], "agent_start_failed", "{r}");
    assert!(elsewhere.is_file());
    assert!(!place(&env, &env.workspace()).exists());

    let ws = env.workspace();
    let second = transcript(&env, "-second", &ws);
    let id = c.frame("session.start", start_in(&env, sid, &ws)).await;
    let r = reply_to(&mut c, &id).await;
    assert_eq!(r["payload"]["code"], "agent_start_failed", "{r}");
    assert!(elsewhere.is_file() && second.is_file());
    assert!(!place(&env, &ws).exists());
    assert!(adapters.calls().is_empty(), "{:?}", adapters.calls());
    w.stop().await;
}

/// Someone else's session that moves into a folder the worker has not run sessions in waits
/// for the owner's consent, as a new one does (SPEC §10.2).
#[tokio::test]
async fn a_move_into_a_new_folder_needs_the_owner() {
    let env = Env::new();
    let other = other(&env);
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let text = env.config_toml(&hub.url()).replace(
        "allow_foreign_sessions = false",
        "allow_foreign_sessions = true",
    );
    let config = hqc_worker::config::Config::parse(&text).unwrap();
    let w = start_worker_with(&env, config, &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    let mut start = env.start_payload(sid);
    start["owner_is_worker_owner"] = json!(false);
    start["requested_by_owner"] = json!(true);
    started(&mut c, start.clone()).await;

    start["requested_by_owner"] = json!(false);
    start["workspace_path"] = json!(other.display().to_string());
    let id = c.frame("session.start", start).await;
    let req = c.expect("worker.approval.request").await;
    assert!(
        req["payload"]["description"]
            .as_str()
            .unwrap()
            .contains(&other.display().to_string()),
        "{req}"
    );
    assert!(c.skipped.iter().all(|f| f["id"] != id.as_str()));
    w.stop().await;
}

/// The agent is stopped right after its turn's final answer: the hub gets that answer, which
/// goes through the outbox, before the `session.state` frames that follow it, which do not
/// (SPEC §5.2); a `stopped` before it would lose the turn. Moves back and forth, many times:
/// the order was a coin toss, the first state frame first in about one of five moves.
#[tokio::test]
async fn the_answer_goes_before_the_stop() {
    let env = Env::new();
    let folders = [env.workspace(), other(&env)];
    let adapters = FakeAdapters::new();
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    let sid = Uuid::now_v7();
    started(&mut c, start_in(&env, sid, &folders[0])).await;
    for i in 0..30 {
        let e = Uuid::now_v7();
        c.frame("session.input", input(sid, e, "go")).await;
        adapters
            .wait_for("the input", |c| {
                c.contains(&FakeCall::Input {
                    session_id: sid,
                    event_id: e,
                })
            })
            .await;
        adapters.accept_input(sid, e).await;
        started(&mut c, start_in(&env, sid, &folders[(i + 1) % 2])).await;
        let answer = agent_event(
            sid,
            EventKind::AgentMessage {
                text: "done".into(),
                r#final: true,
                in_reply_to: Some(e),
                runtime: false,
                interrupted: false,
                refusal: false,
            },
        );
        let answer_id = answer.event_id;
        adapters.emit(sid, AdapterEvent::Event(answer)).await;
        adapters.end_turn(sid).await;
        let mut answered = false;
        loop {
            let f = c.recv().await;
            if f["type"] == "event" && event_id(&f) == answer_id {
                answered = true;
                c.ack_event(&f, 1).await;
            } else if f["type"] == "session.state" {
                let state = &f["payload"]["process_state"];
                assert!(answered, "move {i}: {state} before the turn's answer");
                if state == "stopped" {
                    break;
                }
            }
        }
    }
    w.stop().await;
}

/// The same order when the worker stops: a session's `stopped` still waiting to be sent goes
/// after the events it wrote before, though the connection then sends what is left at once.
/// Written together, the stop of the connection often came first and flushed the state
/// ahead of the answer.
#[tokio::test]
async fn the_answer_goes_before_the_stop_at_shutdown() {
    let mut hub = FakeHub::start().await;
    for i in 0..20 {
        let dir = tempfile::tempdir().unwrap();
        let (link, stop, task, _inbound, mut c) = bare_connection(&mut hub, dir.path()).await;
        // The answer, the agent's stop and the worker's: no await in between.
        let sid = Uuid::now_v7();
        let answer = agent_message(sid, "done");
        let answer_id = answer.event_id;
        link.event(answer).unwrap();
        link.send(WorkerFrame::SessionState {
            session_id: sid,
            process_state: ProcessState::Stopped,
            runtime_session_id: None,
            locked_by: LockedBy::None,
            title: None,
            held_by: None,
            commands: None,
            command_info: None,
            permission_mode: None,
            capped_input: None,
            subagents: 0,
            background_tasks: 0,
            foreground_calls: vec![],
            activity: None,
        });
        stop.cancel();
        let mut answered = false;
        loop {
            let f = c.recv().await;
            if f["type"] == "event" && event_id(&f) == answer_id {
                answered = true;
                c.ack_event(&f, 1).await;
            } else if f["type"] == "session.state" {
                assert!(answered, "shutdown {i}: stopped before the answer");
                break;
            }
        }
        tokio::time::timeout(WAIT, task)
            .await
            .expect("connection stopped")
            .unwrap();
    }
}

/// The same order for an input's `ack`: the next input's goes after the final answer of the turn
/// before it, written to the outbox first, as `session.state` does: the hub closes a turn on its
/// final answer, which after the next input's `ack` would close that input's turn (SPEC §5.2,
/// §17.1). Written together, the connection picks between the outbox and a direct frame at
/// random, so many rounds.
#[tokio::test]
async fn the_answer_goes_before_the_next_inputs_ack() {
    let mut hub = FakeHub::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (link, stop, task, _inbound, mut c) = bare_connection(&mut hub, dir.path()).await;
    let sid = Uuid::now_v7();
    for i in 0..50 {
        // The answer, then the next input's ack: no await in between.
        let answer = agent_message(sid, "done");
        let answer_id = answer.event_id;
        link.event(answer).unwrap();
        let frame = format!("f-{i}");
        link.reply_input(
            sid,
            None,
            &frame,
            WorkerFrame::Ack(Ack {
                event_id: Some(Uuid::now_v7()),
                seq: None,
                joined: false,
            }),
        );
        let mut answered = false;
        loop {
            let f = c.recv().await;
            if f["type"] == "event" && event_id(&f) == answer_id {
                answered = true;
                c.ack_event(&f, 1).await;
            } else if f["type"] == "ack" && f["id"] == frame.as_str() {
                assert!(answered, "input {i}: its ack before the answer before it");
                break;
            }
        }
    }
    stop.cancel();
    tokio::time::timeout(WAIT, task)
        .await
        .expect("connection stopped")
        .unwrap();
}

/// An `error` answer to a `session.input` (`locked_vscode`, a failed delivery) keeps its place
/// like the `ack`: after the answer of the turn before it, written to the outbox first (SPEC
/// §5.2). Many rounds, as above.
#[tokio::test]
async fn an_error_answer_to_an_input_goes_after_the_answer_before_it() {
    let mut hub = FakeHub::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (link, stop, task, _inbound, mut c) = bare_connection(&mut hub, dir.path()).await;
    let sid = Uuid::now_v7();
    for i in 0..50 {
        let answer = agent_message(sid, "done");
        let answer_id = answer.event_id;
        link.event(answer).unwrap();
        let frame = format!("f-{i}");
        link.reply_input(
            sid,
            None,
            &frame,
            wire_error(LOCKED_VSCODE, "the session continues in VS Code", false),
        );
        let mut answered = false;
        loop {
            let f = c.recv().await;
            if f["type"] == "event" && event_id(&f) == answer_id {
                answered = true;
                c.ack_event(&f, 1).await;
            } else if f["type"] == "error" && f["id"] == frame.as_str() {
                assert!(answered, "input {i}: its error before the answer before it");
                break;
            }
        }
    }
    stop.cancel();
    tokio::time::timeout(WAIT, task)
        .await
        .expect("connection stopped")
        .unwrap();
}

/// Another session's outbox entries do not hold an input's `ack`: a session writes more events
/// than the window of un-acked entries, the hub acks none, and the `ack` of an input of a session
/// with no entries still reaches the hub (SPEC §5.2).
#[tokio::test]
async fn another_sessions_entries_do_not_hold_an_inputs_ack() {
    let mut hub = FakeHub::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (link, stop, task, _inbound, mut c) = bare_connection(&mut hub, dir.path()).await;
    let (busy, quiet) = (Uuid::now_v7(), Uuid::now_v7());
    let flood = MAX_IN_FLIGHT + 50;
    for _ in 0..flood {
        link.event(agent_message(busy, "x")).unwrap();
    }
    link.reply_input(
        quiet,
        None,
        "f-quiet",
        WorkerFrame::Ack(Ack {
            event_id: Some(Uuid::now_v7()),
            seq: None,
            joined: false,
        }),
    );
    // The hub acks no event: the busy session's entries all wait while the `ack` arrives.
    let mut events = 0;
    loop {
        let f = c.recv().await;
        if f["type"] == "event" {
            events += 1;
        } else if f["type"] == "ack" && f["id"] == "f-quiet" {
            break;
        }
    }
    assert!(
        events < flood,
        "the ack came after {events} of {flood} events"
    );
    // Let the connection finish: ack what it sends.
    while let Some(f) = c.try_recv(Duration::from_millis(300)).await {
        if f["type"] == "event" {
            c.ack_event(&f, 1).await;
        }
    }
    stop.cancel();
    tokio::time::timeout(WAIT, task)
        .await
        .expect("connection stopped")
        .unwrap();
}

/// The state of a session that runs a transcript goes after the holder report of that transcript
/// written before it: the hub reads `locked_by: vscode` after a holder report as the VS Code tab
/// opening the session, and before it as another writer's continuation (SPEC §9.3, §5.2).
#[tokio::test]
async fn a_sessions_state_goes_after_the_holder_report_of_its_transcript() {
    let mut hub = FakeHub::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (link, stop, task, _inbound, mut c) = bare_connection(&mut hub, dir.path()).await;
    let (sid, transcript) = (Uuid::now_v7(), Uuid::now_v7());
    link.holder_report(
        transcript,
        Some(HeldBy {
            entrypoint: "claude-vscode".into(),
            status: "idle".into(),
        }),
    )
    .unwrap();
    link.send(WorkerFrame::SessionState {
        session_id: sid,
        process_state: ProcessState::Stopped,
        runtime_session_id: Some(transcript.to_string()),
        locked_by: LockedBy::Vscode,
        title: None,
        held_by: None,
        commands: None,
        command_info: None,
        permission_mode: None,
        capped_input: None,
        subagents: 0,
        background_tasks: 0,
        foreground_calls: vec![],
        activity: None,
    });
    let mut order = vec![];
    while order.len() < 2 {
        let f = c.recv().await;
        if f["type"] == "session.state" {
            let who = f["payload"]["session_id"].as_str().unwrap().to_owned();
            order.push(who.parse::<Uuid>().unwrap());
            if f["payload"]["session_id"] == Uuid::nil().to_string() {
                c.ack_event(
                    &json!({"id": f["id"], "payload": {"event": f["payload"]}}),
                    1,
                )
                .await;
            }
        }
    }
    assert_eq!(
        order,
        [Uuid::nil(), sid],
        "the state before the holder report"
    );
    stop.cancel();
    tokio::time::timeout(WAIT, task)
        .await
        .expect("connection stopped")
        .unwrap();
}

/// A session released to VS Code answers an input with `locked_vscode` after the entries of its
/// own stream and of its transcript's: the worker's reply to a `session.input` keeps its place
/// (SPEC §5.2). Read off the frame the supervisor queues, `Direct::after`: nothing is sent.
#[tokio::test]
async fn a_refused_input_keeps_its_place_in_the_supervisor() {
    let env = Env::new();
    let cfg = Arc::new(env.config("https://h"));
    let outbox = Arc::new(Outbox::open(&env.outbox()).unwrap());
    let (link, mut direct) = HubLink::new(outbox.clone());
    let sup = Supervisor::new(
        cfg.clone(),
        Arc::new(FakeAdapters::new()),
        link.clone(),
        ArtifactClient::new(&cfg.hub_url, Secret::new(TOKEN)),
        Duration::from_secs(5),
        Arc::new(KnownPaths::load(&env.dir.path().join("known.json")).unwrap()),
        Created::load(&env.dir.path().join("created.json"), &cfg).unwrap(),
    );
    let (tx, rx) = mpsc::channel(16);
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(sup.run(rx, shutdown.clone()));

    let sid = Uuid::now_v7();
    let transcript = Uuid::now_v7();
    let mut start = env.start_payload(sid);
    start["runtime_session_id"] = json!(transcript);
    let frame = |id: &str, ty: &str, payload: Value| -> Inbound {
        Inbound::Frame(
            serde_json::from_value(json!({"id": id, "type": ty, "payload": payload})).unwrap(),
        )
    };
    tx.send(frame("start", "session.start", start))
        .await
        .unwrap();
    // The session runs; the VS Code tab takes the transcript.
    tokio::time::timeout(WAIT, async {
        loop {
            match direct.recv().await.unwrap().frame {
                WorkerFrame::SessionState {
                    process_state: ProcessState::Running,
                    ..
                } => break,
                _ => continue,
            }
        }
    })
    .await
    .expect("the session runs");
    tx.send(Inbound::Holder {
        runtime_session_id: transcript,
        held_by: Some(HeldBy {
            entrypoint: "claude-vscode".into(),
            status: "idle".into(),
        }),
    })
    .await
    .unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            match direct.recv().await.unwrap().frame {
                WorkerFrame::SessionState {
                    locked_by: LockedBy::Vscode,
                    ..
                } => break,
                _ => continue,
            }
        }
    })
    .await
    .expect("the session is released");

    // An entry of the session's stream and one of its transcript's, then the input.
    link.event(agent_message(sid, "answer")).unwrap();
    link.holder_report(transcript, None).unwrap();
    let last = outbox.last_seq().unwrap();
    assert!(last > 0);
    let input = json!({"session_id": sid, "event_id": Uuid::now_v7(), "text": "again",
        "artifact_ids": [], "peer_prefix": []});
    tx.send(frame("input", "session.input", input))
        .await
        .unwrap();
    let d = tokio::time::timeout(WAIT, async {
        loop {
            let d = direct.recv().await.unwrap();
            if d.id.as_deref() == Some("input") {
                break d;
            }
        }
    })
    .await
    .expect("the input is answered");
    assert!(matches!(&d.frame, WorkerFrame::Error(e) if e.code == LOCKED_VSCODE));
    assert_eq!(
        d.after,
        Some(last),
        "the error answer does not keep its place"
    );
    shutdown.cancel();
    tokio::time::timeout(WAIT, task)
        .await
        .expect("supervisor stopped")
        .unwrap();
}

/// A connection to `hub` alone, without a worker around it, its outbox in `dir`, connected: its
/// link, its stop, its task, the receiver of what it hands on and the hub's side.
async fn bare_connection(
    hub: &mut FakeHub,
    dir: &Path,
) -> (
    HubLink,
    CancellationToken,
    tokio::task::JoinHandle<()>,
    mpsc::Receiver<Inbound>,
    HubConn,
) {
    let outbox = Arc::new(Outbox::open(&dir.join("outbox")).unwrap());
    let (link, direct) = HubLink::new(outbox);
    let (inbound, inbound_rx) = mpsc::channel(16);
    let (status_tx, mut status) = watch::channel(Status::Connecting);
    let hello = Hello {
        protocol_version: hqc_proto::PROTOCOL_VERSION,
        boot_id: Uuid::new_v4(),
        capabilities: json!({}),
        environments: vec![],
        env_bindings: vec![],
        allowed_roots: vec![],
        allow_foreign_sessions: false,
        share_local_models: false,
        local_models: vec![],
        providers: vec![],
        session_secrets: false,
        browser: false,
        machine_id: None,
    };
    let conn = Connection::new(
        hub.url(),
        Secret::new(TOKEN),
        hello,
        link.clone(),
        direct,
        inbound,
        status_tx,
        fast_conn(),
    );
    let stop = CancellationToken::new();
    let task = tokio::spawn(conn.run(stop.clone()));
    let mut c = hub.accept().await;
    c.handshake().await;
    tokio::time::timeout(
        WAIT,
        status.wait_for(|s| matches!(s, Status::Connected { .. })),
    )
    .await
    .expect("connected")
    .unwrap();
    (link, stop, task, inbound_rx, c)
}
