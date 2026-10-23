//! The mirror on the T15 fixtures (2.1.308): records of the worker's own process are never
//! another writer's (`vscode-takeover`), the outside turn after an adapter session is
//! (`adapter-then-outside`).

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
    mirror.set(PathBuf::from("/tmp/hqc-fixture"), true);
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
    s.own.begin(s.runtime);
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
    s.own.end(s.runtime);
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
    s.own.begin(s.runtime);
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
    s.own.end(s.runtime);
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
    s.mirror.set(PathBuf::from("/tmp/hqc-fixture"), false);
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
