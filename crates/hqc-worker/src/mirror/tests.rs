//! The mirror on the T15 fixtures (2.1.308): records of the worker's own process are never
//! another writer's (`vscode-takeover`), the outside turn after an adapter session is
//! (`adapter-then-outside`); T20: forks (`fork`) and holders from the registry of live sessions.

use std::io::Write;

use hqc_claude_jsonl::{parse_str, Record};
use hqc_proto::event::EventKind;

use super::*;

const VSCODE: &str =
    include_str!("../../../hqc-claude-jsonl/fixtures/vscode-takeover-2.1.308.jsonl");
const VSCODE_STREAM: &str =
    include_str!("../../../hqc-claude-jsonl/fixtures/stream-vscode-takeover-2.1.308.jsonl");
const OUTSIDE: &str =
    include_str!("../../../hqc-claude-jsonl/fixtures/adapter-then-outside-2.1.308.jsonl");
const OUTSIDE_STREAM: &str =
    include_str!("../../../hqc-claude-jsonl/fixtures/stream-adapter-then-outside-2.1.308.jsonl");

struct Setup {
    _dir: tempfile::TempDir,
    file: PathBuf,
    runtime: Uuid,
    own: Arc<OwnRecords>,
    mirror: Mirror,
}

/// A config dir with one transcript of `name` (cwd `/tmp/hqc-fixture/<name>`), the
/// workspace `/tmp/hqc-fixture` mirrored.
fn setup(name: &str, transcript: &str) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let runtime: Uuid = transcript
        .lines()
        .find_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            v["sessionId"].as_str()?.parse().ok()
        })
        .unwrap();
    let folder = dir
        .path()
        .join("projects")
        .join(format!("-tmp-hqc-fixture-{name}"));
    std::fs::create_dir_all(&folder).unwrap();
    // A subagent folder and a foreign file: ignored.
    std::fs::create_dir_all(folder.join(runtime.to_string()).join("subagents")).unwrap();
    std::fs::write(folder.join("notes.jsonl"), "{}\n").unwrap();
    let file = folder.join(format!("{runtime}.jsonl"));
    let own = Arc::new(OwnRecords::default());
    let mut mirror = Mirror::new(
        dir.path().join("projects"),
        Arc::new(Store::memory()),
        own.clone(),
    );
    mirror.set(PathBuf::from("/tmp/hqc-fixture"), true, vec![]);
    Setup {
        _dir: dir,
        file,
        runtime,
        own,
        mirror,
    }
}

fn append(path: &Path, s: &str) {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap()
        .write_all(s.as_bytes())
        .unwrap();
}

fn stream_uuids(stream: &str) -> Vec<Uuid> {
    parse_str(stream)
        .0
        .iter()
        .filter_map(|r| match r {
            Record::User(m) | Record::Assistant(m) => Some(m.uuid),
            _ => None,
        })
        .collect()
}

fn labels(out: &[Output]) -> Vec<String> {
    out.iter()
        .map(|o| match o {
            Output::Event(e) => match &e.event.kind {
                EventKind::UserMessage { text, .. } => format!("user:{text}"),
                EventKind::AgentMessage { text, .. } => format!("agent:{text}"),
                EventKind::ToolCall { .. } => "call".into(),
                EventKind::ToolResult { .. } => "result".into(),
                other => format!("{other:?}"),
            },
            Output::ForeignWriter { .. } => "foreign".into(),
            Output::Holder { .. } => "holder".into(),
        })
        .collect()
}

/// Fixture a: the `claude -p` part is imported; then the adapter takes over and its records
/// are neither emitted nor taken for another writer, however long they wait.
#[test]
fn own_records_are_not_foreign() {
    let mut s = setup("vscode-takeover", VSCODE);
    let split = VSCODE.lines().position(|l| l.contains("HQC-TURN")).unwrap();
    let lines: Vec<&str> = VSCODE.lines().collect();
    append(&s.file, &(lines[..split].join("\n") + "\n"));
    let t0 = Instant::now();
    let out = s.mirror.poll(t0, SystemTime::now());
    assert_eq!(
        labels(&out),
        [
            "user:Run the shell command: echo hqc-vscode. Then reply with exactly: VSCODE-DONE",
            "call",
            "result",
            "agent:VSCODE-DONE"
        ]
    );
    let Output::Event(first) = &out[0] else {
        unreachable!()
    };
    assert_eq!(first.runtime_session_id, s.runtime.to_string());
    assert_eq!(first.cwd, "/tmp/hqc-fixture/vscode-takeover");
    assert_eq!(first.workspace_path, "/tmp/hqc-fixture");
    assert!(first.recent, "just written");

    // Takeover: the adapter's process runs and reads its stream.
    s.own.begin(s.runtime, None);
    for u in stream_uuids(VSCODE_STREAM) {
        s.own.add(s.runtime, u);
    }
    append(&s.file, &(lines[split..].join("\n") + "\n"));
    for later in [0, 10, 60] {
        let out = s
            .mirror
            .poll(t0 + Duration::from_secs(later), SystemTime::now());
        assert!(out.is_empty(), "{later} s: {:?}", labels(&out));
    }
    // The process ends: nothing left to send either.
    s.own.end(s.runtime, None);
    assert!(s.mirror.poll(t0, SystemTime::now()).is_empty());
    let row = s.mirror.store.get(&s.file).unwrap().unwrap();
    assert_eq!(row.offset, VSCODE.len() as u64);
}

/// Fixture b: an adapter session continued by `claude -p --resume` from outside. While the
/// adapter's process runs, the outside records are held and reported once after the grace
/// period; when the process is gone they are emitted.
#[test]
fn outside_writer_is_detected() {
    let mut s = setup("adapter-then-outside", OUTSIDE);
    s.own.begin(s.runtime, None);
    for u in stream_uuids(OUTSIDE_STREAM) {
        s.own.add(s.runtime, u);
    }
    append(&s.file, OUTSIDE);
    let t0 = Instant::now();
    assert!(
        s.mirror.poll(t0, SystemTime::now()).is_empty(),
        "held, not yet foreign"
    );
    let out = s
        .mirror
        .poll(t0 + Duration::from_secs(6), SystemTime::now());
    assert_eq!(
        out,
        [Output::ForeignWriter {
            runtime_session_id: s.runtime
        }]
    );
    assert!(
        s.mirror
            .poll(t0 + Duration::from_secs(9), SystemTime::now())
            .is_empty(),
        "reported once"
    );
    // The supervisor stopped the agent: the held records go out, answers point at the
    // outside prompt, the empty thinking is skipped.
    s.own.end(s.runtime, None);
    let out = s
        .mirror
        .poll(t0 + Duration::from_secs(10), SystemTime::now());
    assert_eq!(
        labels(&out),
        [
            "user:Reply with exactly: OUTSIDE-TURN",
            "agent:OUTSIDE-TURN"
        ]
    );
    let (Output::Event(p), Output::Event(a)) = (&out[0], &out[1]) else {
        unreachable!()
    };
    let EventKind::AgentMessage {
        r#final,
        in_reply_to,
        ..
    } = &a.event.kind
    else {
        unreachable!()
    };
    assert!(*r#final);
    assert_eq!(*in_reply_to, Some(p.event.event_id));
}

/// Without a process of this worker any unknown record is emitted at once; an old file is an
/// import (`recent: false`), a half-written line waits for its end.
#[test]
fn import_and_partial_lines() {
    let mut s = setup("adapter-then-outside", OUTSIDE);
    let (head, tail) = OUTSIDE.split_at(OUTSIDE.len() - 40);
    append(&s.file, head);
    let later = SystemTime::now() + Duration::from_secs(120);
    let out = s.mirror.poll(Instant::now(), later);
    let n = out.len();
    assert!(n >= 6, "{:?}", labels(&out));
    assert!(out
        .iter()
        .all(|o| matches!(o, Output::Event(e) if !e.recent)));
    append(&s.file, tail);
    let out = s.mirror.poll(Instant::now(), SystemTime::now());
    assert!(
        out.len() <= 1,
        "only what the last line completes: {:?}",
        labels(&out)
    );
    // Not mirrored: nothing.
    s.mirror
        .set(PathBuf::from("/tmp/hqc-fixture"), false, vec![]);
    append(&s.file, OUTSIDE);
    assert!(s.mirror.poll(Instant::now(), SystemTime::now()).is_empty());
}

#[test]
fn candidates_group_by_cwd() {
    let s = setup("vscode-takeover", VSCODE);
    append(&s.file, VSCODE);
    let other = s.file.with_file_name(format!("{}.jsonl", Uuid::new_v4()));
    append(&other, VSCODE);
    let projects = s.file.parent().unwrap().parent().unwrap();
    let store = Store::memory();
    let all = candidates(projects, &store, |_| true);
    assert_eq!(all.candidates.len(), 1);
    assert_eq!(all.candidates[0].path, "/tmp/hqc-fixture/vscode-takeover");
    assert_eq!(all.candidates[0].sessions_count, 2);
    assert!(
        candidates(projects, &store, |p| p.starts_with("/elsewhere"))
            .candidates
            .is_empty()
    );
    // The cwd is cached.
    assert!(store.get(&other).unwrap().unwrap().cwd.is_some());
}

const FORK: &str = include_str!("../../../hqc-claude-jsonl/fixtures/fork-2.1.308.jsonl");
const FORK_STREAM: &str =
    include_str!("../../../hqc-claude-jsonl/fixtures/stream-fork-2.1.308.jsonl");
/// The `ONE` answer both later prompts hang off.
const FORK_AT: &str = "d6e6a091-03a1-463f-bfbf-d19775f04f96";

fn forks(out: &[Output]) -> Vec<(Uuid, String, u32, String)> {
    out.iter()
        .filter_map(|o| match o {
            Output::Event(e) => match &e.event.kind {
                EventKind::System {
                    text,
                    code,
                    fork: Some(f),
                } if code == hqc_proto::event::SESSION_FORKED => Some((
                    f.at_uuid,
                    f.branch_from.clone(),
                    f.orphaned_turns,
                    text.clone(),
                )),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// T20, fixture `fork` (2.1.308): the adapter's turn, then a prompt resumed from the earlier
/// `ONE` answer. The mirror reports the fork once, before the later prompt, with the
/// adapter's turn as orphaned; reading the file again in a new process reports nothing.
#[test]
fn fork_is_reported_once() {
    let mut s = setup("fork", FORK);
    for u in stream_uuids(FORK_STREAM) {
        s.own.add(s.runtime, u);
    }
    append(&s.file, FORK);
    let out = s.mirror.poll(Instant::now(), SystemTime::now());
    let at: Uuid = FORK_AT.parse().unwrap();
    assert_eq!(
        forks(&out),
        [(
            at,
            "vscode".to_string(),
            1,
            "VS Code continued from an earlier point: 1 turn from hqc is not in its context"
                .to_string()
        )]
    );
    let l = labels(&out);
    let i = l
        .iter()
        .position(|x| x.contains("session_forked") || x.starts_with("System"))
        .unwrap();
    assert_eq!(l[i + 1], "user:Reply with exactly: VSCODE-TURN", "{l:?}");
    let Output::Event(e) = &out[i] else {
        unreachable!()
    };
    assert_eq!(e.event.event_id, Event::id_of_fork(&at));
    assert_eq!(e.event.author, Author::System);
    assert_eq!(e.held_by, None, "no registry configured");

    // A new worker process: the tree is rebuilt from what was read, nothing is reported
    // again, and a later prompt on the new line is no fork.
    let mut again = Mirror::new(
        s.file.parent().unwrap().parent().unwrap().to_path_buf(),
        s.mirror.store.clone(),
        s.own.clone(),
    );
    again.set(PathBuf::from("/tmp/hqc-fixture"), true, vec![]);
    let answer = "120fb1ee-3831-484d-aef2-a9505079a043";
    append(
        &s.file,
        &(serde_json::json!({
            "parentUuid": answer, "isSidechain": false, "type": "user",
            "message": {"role": "user", "content": "Reply with exactly: MORE"},
            "uuid": Uuid::new_v4(), "timestamp": "2026-10-23T19:00:00.000Z",
            "cwd": "/tmp/hqc-fixture/fork", "sessionId": s.runtime
        })
        .to_string()
            + "\n"),
    );
    let out = again.poll(Instant::now(), SystemTime::now());
    assert_eq!(labels(&out), ["user:Reply with exactly: MORE"]);
}

/// With the registry, a live process holding the transcript is reported once, rides on
/// every `mirror.event`, and its going away is reported as `None` (not sooner than a poll
/// period after the last report).
#[test]
fn holder_in_registry() {
    let mut s = setup("vscode-takeover", VSCODE);
    let config = s.file.parent().unwrap().parent().unwrap().parent().unwrap();
    let sessions = config.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let pid = std::process::id();
    let entry = sessions.join(format!("{pid}.json"));
    std::fs::write(
        &entry,
        serde_json::json!({"pid": pid, "sessionId": s.runtime, "entrypoint": "claude-vscode",
            "status": "busy", "updatedAt": 1792779848554u64})
        .to_string(),
    )
    .unwrap();
    s.mirror = Mirror::new(
        config.join("projects"),
        Arc::new(Store::memory()),
        s.own.clone(),
    )
    .with_registry(sessions.clone(), Box::new(holders::System));
    s.mirror
        .set(PathBuf::from("/tmp/hqc-fixture"), true, vec![]);
    append(&s.file, VSCODE);
    let held = HeldBy {
        entrypoint: "claude-vscode".into(),
        status: "busy".into(),
    };
    let t0 = Instant::now();
    let out = s.mirror.poll(t0, SystemTime::now());
    assert_eq!(
        out[0],
        Output::Holder {
            runtime_session_id: s.runtime,
            held_by: Some(held.clone())
        }
    );
    assert!(out[1..]
        .iter()
        .all(|o| matches!(o, Output::Event(e) if e.held_by == Some(Some(held.clone())))));
    assert!(s.mirror.poll(t0, SystemTime::now()).is_empty(), "no change");

    // Our own process of the same transcript is not a holder.
    s.own.begin(s.runtime, Some(pid));
    assert!(
        s.mirror.poll(t0, SystemTime::now()).is_empty(),
        "too soon after the last report"
    );
    let out = s
        .mirror
        .poll(t0 + Duration::from_secs(3), SystemTime::now());
    assert_eq!(
        out,
        [Output::Holder {
            runtime_session_id: s.runtime,
            held_by: None
        }]
    );
    s.own.end(s.runtime, Some(pid));
    std::fs::remove_file(&entry).unwrap();
    assert!(s
        .mirror
        .poll(t0 + Duration::from_secs(6), SystemTime::now())
        .is_empty());
}

/// T21: the mirror is enabled on a workspace where hqc already ran a session (fixture b: the
/// adapter's turn, then an outside turn). The hub lists that transcript with the time of its
/// last event; the first pass skips what the hub has (no duplicate prompt with another
/// event id) and sends only the outside turn. A transcript the hub does not list is imported
/// whole, and a later `mirror.set` (after the next `hello`) changes nothing for files already
/// read.
#[test]
fn enabling_on_a_workspace_with_hqc_sessions_brings_no_duplicates() {
    let since: DateTime<Utc> = "2026-10-23T00:10:58Z".parse().unwrap();
    let mut s = setup("adapter-then-outside", OUTSIDE);
    s.mirror.set(
        PathBuf::from("/tmp/hqc-fixture"),
        true,
        vec![(s.runtime, since)],
    );
    append(&s.file, OUTSIDE);
    let out = s.mirror.poll(Instant::now(), SystemTime::now());
    assert_eq!(
        labels(&out),
        [
            "user:Reply with exactly: OUTSIDE-TURN",
            "agent:OUTSIDE-TURN"
        ]
    );
    // A later turn is not limited any more.
    s.mirror.set(
        PathBuf::from("/tmp/hqc-fixture"),
        true,
        vec![(s.runtime, Utc::now())],
    );
    let last_prompt = OUTSIDE
        .lines()
        .find(|l| {
            l.contains("Reply with exactly: OUTSIDE-TURN") && l.contains("\"type\": \"user\"")
        })
        .unwrap();
    let mut v: serde_json::Value = serde_json::from_str(last_prompt).unwrap();
    v["uuid"] = serde_json::json!(Uuid::new_v4());
    // After the last record: a continuation, not a fork.
    v["parentUuid"] = serde_json::json!("e965b24b-6355-4c01-8f1e-d1a303611946");
    v["timestamp"] = serde_json::json!("2026-10-23T00:12:00Z");
    v["message"]["content"] = serde_json::json!("one more");
    append(&s.file, &format!("{v}\n"));
    let out = s.mirror.poll(Instant::now(), SystemTime::now());
    assert_eq!(labels(&out), ["user:one more"]);

    // Not listed: the whole history.
    let mut s = setup("adapter-then-outside", OUTSIDE);
    s.mirror.set(
        PathBuf::from("/tmp/hqc-fixture"),
        true,
        vec![(Uuid::new_v4(), since)],
    );
    append(&s.file, OUTSIDE);
    let out = s.mirror.poll(Instant::now(), SystemTime::now());
    assert_eq!(
        labels(&out),
        [
            "user:Run the shell command: echo hqc-adapter. Then reply with exactly: ADAPTER-DONE",
            "call",
            "result",
            "agent:ADAPTER-DONE",
            "user:Reply with exactly: OUTSIDE-TURN",
            "agent:OUTSIDE-TURN"
        ]
    );
}

/// Merge of T21 and T22: a transcript the hub refused with `mirror_quota` (its daily limit of
/// imported sessions) is read again from the start, but not before `quota_retry` has
/// passed; the refusals of all its records in flight rewind it once.
#[test]
fn quota_refusal_reads_the_transcript_again_later() {
    let mut s = setup("adapter-then-outside", OUTSIDE);
    append(&s.file, OUTSIDE);
    let t0 = Instant::now();
    let all = labels(&s.mirror.poll(t0, SystemTime::now()));
    assert_eq!(all.len(), 6, "{all:?}");
    assert!(s.mirror.poll(t0, SystemTime::now()).is_empty());

    for _ in 0..all.len() {
        s.mirror.defer(s.runtime, t0).unwrap();
    }
    let early = t0 + QUOTA_RETRY - Duration::from_secs(1);
    assert!(s.mirror.poll(early, SystemTime::now()).is_empty());
    // A record written meanwhile waits as well.
    let last_prompt = OUTSIDE
        .lines()
        .find(|l| {
            l.contains("Reply with exactly: OUTSIDE-TURN") && l.contains("\"type\": \"user\"")
        })
        .unwrap();
    let mut v: serde_json::Value = serde_json::from_str(last_prompt).unwrap();
    v["uuid"] = serde_json::json!(Uuid::new_v4());
    v["parentUuid"] = serde_json::json!("e965b24b-6355-4c01-8f1e-d1a303611946");
    v["message"]["content"] = serde_json::json!("one more");
    append(&s.file, &format!("{v}\n"));
    assert!(s.mirror.poll(early, SystemTime::now()).is_empty());

    let later = t0 + QUOTA_RETRY;
    let again = labels(&s.mirror.poll(later, SystemTime::now()));
    assert_eq!(again[..all.len()], all[..], "the whole history again");
    assert_eq!(again[all.len()..], ["user:one more"]);
    assert!(s.mirror.poll(later, SystemTime::now()).is_empty());

    // Refused again: another hour.
    s.mirror.defer(s.runtime, later).unwrap();
    assert!(s
        .mirror
        .poll(later + Duration::from_secs(60), SystemTime::now())
        .is_empty());
    let third = labels(&s.mirror.poll(later + QUOTA_RETRY, SystemTime::now()));
    assert_eq!(third, again);
}

const RESUME: &str =
    include_str!("../../../hqc-claude-jsonl/fixtures/resume-compact-2.1.308.jsonl");
const RESUME_PROMPT_STREAM: &str =
    include_str!("../../../hqc-claude-jsonl/fixtures/stream-resume-prompt-2.1.308.jsonl");
const RESUME_COMPACT_STREAM: &str =
    include_str!("../../../hqc-claude-jsonl/fixtures/stream-resume-compact-2.1.308.jsonl");

/// Fixture `resume-compact` in three parts: the first session's turn, the `--resume` turn
/// `TWO`, the `--resume` with `/compact`.
fn resume_parts() -> [String; 3] {
    let lines: Vec<&str> = RESUME.lines().collect();
    let at = |s: &str| lines.iter().position(|l| l.contains(s)).unwrap();
    let (two, compact) = (
        at(r#""content": "Reply with exactly: TWO""#),
        at(r#""content": "/compact""#),
    );
    [
        lines[..two].join("\n") + "\n",
        lines[two..compact].join("\n") + "\n",
        lines[compact..].join("\n") + "\n",
    ]
}

/// [`setup`] of `resume-compact` with the first part read; `registry`: Claude Code's
/// `sessions` folder exists (empty).
fn resume_setup(registry: bool) -> (Setup, PathBuf) {
    let mut s = setup("resume-compact", RESUME);
    let config = s.file.parent().unwrap().parent().unwrap().parent().unwrap();
    let sessions = config.join("sessions");
    if registry {
        std::fs::create_dir_all(&sessions).unwrap();
    }
    s.mirror = Mirror::new(
        config.join("projects"),
        Arc::new(Store::memory()),
        s.own.clone(),
    )
    .with_registry(sessions.clone(), Box::new(holders::System));
    s.mirror
        .set(PathBuf::from("/tmp/hqc-fixture"), true, vec![]);
    append(&s.file, &resume_parts()[0]);
    let out = s.mirror.poll(Instant::now(), SystemTime::now());
    assert_eq!(
        labels(&out),
        ["user:Reply with exactly: ONE", "agent:ONE"],
        "the session before hqc took it over"
    );
    (s, sessions)
}

/// The adapter's process of the session runs and has read `stream`; `inputs`: what it
/// wrote to stdin.
fn run_hqc(s: &Setup, stream: &str, inputs: &[&str]) {
    s.own.begin(s.runtime, None);
    for u in stream_uuids(stream) {
        s.own.add(s.runtime, u);
    }
    for i in inputs {
        s.own.sent(s.runtime, i);
    }
}

/// T25, the field case of 2026-10-24 (fixture `resume-compact`, 2.1.308): `/compact` sent
/// from hqc writes a command record stream-json does not repeat. With the registry and no
/// holder that is no other writer, however long it stays unknown: the record goes out with
/// `held_by: null` (the hub does not lock on it). A holder appearing then is reported, and
/// that releases the session.
#[test]
fn registry_unknown_record_without_holder_keeps_the_session() {
    let (mut s, sessions) = resume_setup(true);
    run_hqc(&s, RESUME_PROMPT_STREAM, &[]);
    append(&s.file, &resume_parts()[1]);
    let t0 = Instant::now();
    assert!(s.mirror.poll(t0, SystemTime::now()).is_empty(), "own turn");
    s.own.end(s.runtime, None);

    // `/compact` from hqc; the input's text is not known here (as with an older adapter).
    run_hqc(&s, RESUME_COMPACT_STREAM, &[]);
    append(&s.file, &resume_parts()[2]);
    assert!(
        s.mirror.poll(t0, SystemTime::now()).is_empty(),
        "held while it may still be ours"
    );
    let out = s
        .mirror
        .poll(t0 + Duration::from_secs(6), SystemTime::now());
    assert_eq!(labels(&out).len(), 1, "{:?}", labels(&out));
    let Output::Event(e) = &out[0] else {
        panic!("{:?}", labels(&out))
    };
    assert!(labels(&out)[0].starts_with("user:<command-name>/compact</command-name>"));
    assert_eq!(e.held_by, Some(None), "registry data: no holder");
    assert!(s
        .mirror
        .poll(t0 + Duration::from_secs(60), SystemTime::now())
        .is_empty());

    // VS Code opens the transcript while hqc still runs it: that is the release.
    let pid = std::process::id();
    std::fs::write(
        sessions.join(format!("{pid}.json")),
        serde_json::json!({"pid": pid, "sessionId": s.runtime, "entrypoint": "claude-vscode",
            "status": "idle", "updatedAt": 1792779848554u64})
        .to_string(),
    )
    .unwrap();
    let out = s
        .mirror
        .poll(t0 + Duration::from_secs(62), SystemTime::now());
    assert_eq!(
        out,
        [Output::Holder {
            runtime_session_id: s.runtime,
            held_by: Some(HeldBy {
                entrypoint: "claude-vscode".into(),
                status: "idle".into(),
            })
        }]
    );
}

/// T25, point 1 of the field case: the prompt of the first input after `--resume` whose
/// echo did not name the record's uuid (not reproduced on 2.1.308: there the uuids match).
/// The adapter's input text claims it, with or without the registry; with the registry an
/// unclaimed one is not another writer either.
#[test]
fn resume_prompt_without_its_echo_uuid() {
    let echo = stream_uuids(RESUME_PROMPT_STREAM)[0];
    let without_echo: String = RESUME_PROMPT_STREAM
        .lines()
        .filter(|l| !l.contains(&echo.to_string()))
        .map(|l| format!("{l}\n"))
        .collect();
    for registry in [false, true] {
        let (mut s, _) = resume_setup(registry);
        run_hqc(&s, &without_echo, &["Reply with exactly: TWO"]);
        append(&s.file, &resume_parts()[1]);
        let t0 = Instant::now();
        for later in [0, 6, 60] {
            let out = s
                .mirror
                .poll(t0 + Duration::from_secs(later), SystemTime::now());
            assert!(out.is_empty(), "registry {registry}, {later} s: {out:?}");
        }
        assert!(s.own.is_own(s.runtime, echo));

        // Not even the text: with the registry the prompt goes out, and only that.
        let (mut s, _) = resume_setup(registry);
        run_hqc(&s, &without_echo, &[]);
        append(&s.file, &resume_parts()[1]);
        assert!(s.mirror.poll(t0, SystemTime::now()).is_empty());
        let out = s
            .mirror
            .poll(t0 + Duration::from_secs(6), SystemTime::now());
        let want: &[&str] = if registry {
            &["user:Reply with exactly: TWO"]
        } else {
            &["foreign"]
        };
        assert_eq!(labels(&out), want, "registry {registry}");
    }
}

/// Without the registry (no `sessions` folder) the uuid check stays (SPEC §9.3): the
/// `/compact` command record the adapter did not report is another writer after the grace
/// period, unless the adapter's input `/compact` claims it.
#[test]
fn no_registry_keeps_the_uuid_check() {
    for inputs in [&[][..], &["/compact"][..]] {
        let (mut s, _) = resume_setup(false);
        for u in stream_uuids(RESUME_PROMPT_STREAM) {
            s.own.add(s.runtime, u);
        }
        append(&s.file, &resume_parts()[1]);
        let t0 = Instant::now();
        assert!(s.mirror.poll(t0, SystemTime::now()).is_empty(), "own turn");
        run_hqc(&s, RESUME_COMPACT_STREAM, inputs);
        append(&s.file, &resume_parts()[2]);
        assert!(s.mirror.poll(t0, SystemTime::now()).is_empty());
        let out = s
            .mirror
            .poll(t0 + Duration::from_secs(6), SystemTime::now());
        if inputs.is_empty() {
            assert_eq!(
                out,
                [Output::ForeignWriter {
                    runtime_session_id: s.runtime
                }]
            );
        } else {
            assert!(out.is_empty(), "{:?}", labels(&out));
        }
    }
}

/// The `user` record of a background task's notification, as 2.1.312 wrote it in a live
/// run (`run_in_background` Bash; the output path and `cwd` shortened).
pub(crate) const NOTIFICATION: &str = r#"{"parentUuid":"d65d6dce-cab4-4296-a1cb-bf92667e507b","isSidechain":false,"promptId":"cf6b1bdd-53ad-4674-80e3-793e00dfadca","type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>b1b20u6dl</task-id>\n<tool-use-id>toolu_01DNPTCZTgycgC11D8VMjfaD</tool-use-id>\n<output-file>/tmp/tasks/b1b20u6dl.output</output-file>\n<status>completed</status>\n<summary>Background command \"Run background sleep and echo marker\" completed (exit code 0)</summary>\n</task-notification>"},"uuid":"640983ce-6012-48c6-b945-0b45eead2f1b","timestamp":"2026-11-05T02:23:28.612Z","permissionMode":"default","origin":{"kind":"task-notification","producer":"session-task","runId":"0mw2x8jvm-af387e8f"},"promptSource":"system","turnOrigin":"task_notification","turnPosition":{"promptIndex":0,"turnIndex":2},"queueSkipAttachments":true,"cwd":"/tmp/hqc-fixture/bg","sessionId":"dad7ce16-3a69-4fbc-a701-96eb301797af","version":"2.1.312"}"#;

/// A transcript of a turn with a background task (2.1.312 shapes): the prompt, the answer
/// `WAITING`, the task's notification and the turn the CLI then started (`PONG`).
fn background_task_transcript() -> (String, [Uuid; 3]) {
    let prompt = Uuid::new_v4();
    let waiting = Uuid::new_v4();
    let pong = Uuid::new_v4();
    let at = |u: Uuid, rest: &str| {
        format!(
            r#"{{"parentUuid":null,"isSidechain":false,"uuid":"{u}","timestamp":"2026-11-05T02:23:17.674Z","cwd":"/tmp/hqc-fixture/bg","sessionId":"dad7ce16-3a69-4fbc-a701-96eb301797af",{rest}}}"#
        )
    };
    let lines = [
        at(
            prompt,
            r#""type":"user","message":{"role":"user","content":"Start it, then reply WAITING"}"#,
        ),
        at(
            waiting,
            r#""type":"assistant","message":{"model":"claude-haiku-5-5","role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"WAITING"}]}"#,
        ),
        NOTIFICATION.to_string(),
        at(
            pong,
            r#""type":"assistant","message":{"model":"claude-haiku-5-5","role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"PONG"}]}"#,
        ),
    ];
    (lines.join("\n") + "\n", [prompt, waiting, pong])
}

/// A background task's notification: while the worker's own process runs the session the
/// adapter reports it (stream-json `system task_notification`, under another uuid), so the
/// mirror neither emits it nor waits on it nor takes it for another writer, also without the
/// registry; in a session the worker does not run it is `system task_notification`, never a
/// prompt.
#[test]
fn task_notification_is_the_adapters_while_it_runs() {
    let (transcript, own) = background_task_transcript();
    let mut s = setup("bg", &transcript);
    s.own.begin(s.runtime, None);
    for u in own {
        s.own.add(s.runtime, u);
    }
    append(&s.file, &transcript);
    let t0 = Instant::now();
    for later in [0, 10] {
        let out = s
            .mirror
            .poll(t0 + Duration::from_secs(later), SystemTime::now());
        assert!(out.is_empty(), "{later} s: {:?}", labels(&out));
    }
    let row = s.mirror.store.get(&s.file).unwrap().unwrap();
    assert_eq!(row.offset, transcript.len() as u64, "read past it at once");

    let mut s = setup("bg", &transcript);
    append(&s.file, &transcript);
    let out = s.mirror.poll(Instant::now(), SystemTime::now());
    let kinds: Vec<EventKind> = out
        .iter()
        .map(|o| match o {
            Output::Event(e) => e.event.kind.clone(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert!(matches!(&kinds[0], EventKind::UserMessage { .. }));
    assert_eq!(
        kinds[2],
        EventKind::System {
            text: "Background command \"Run background sleep and echo marker\" completed (exit \
                   code 0)"
                .into(),
            code: "task_notification".into(),
            fork: None,
        }
    );
    // `PONG` answers the person's prompt, not the notification.
    let EventKind::AgentMessage { in_reply_to, .. } = &kinds[3] else {
        panic!("{:?}", kinds[3])
    };
    let Output::Event(p) = &out[0] else {
        unreachable!()
    };
    assert_eq!(*in_reply_to, Some(p.event.event_id));
}
