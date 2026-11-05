//! T4 acceptance on the T0 fixtures (Claude Code 2.1.307), without spawning `claude`: the
//! adapter runs against [`ScriptedClaude`] playing the captured stdout, and its stdin is
//! compared with what the spike driver wrote.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use hqc_proto::event::{
    ApprovalCanonical, Author, CancelReason, ClarifyAnswer, ClarifyQuestion, Event, EventKind,
    SessionState,
};
use hqc_proto::frame::{Runtime, SessionInput};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::engine::{Action, Core};
use super::process::{CommandSpec, OsSpawner, Sig};
use super::scripted::ScriptedClaude;
use super::{ClaudeCodeAdapter, ClaudeCodeAdapters, ClaudeOptions};
use crate::adapter::{Adapter, AdapterEvent, AdapterFactory};
use crate::config::StartSpec;

struct Fixture {
    transcript: &'static str,
    stream: &'static str,
    stdin: &'static str,
}

macro_rules! fixture {
    ($name:literal) => {
        fixture!($name, "2.1.307")
    };
    ($name:literal, $ver:literal) => {
        Fixture {
            transcript: include_str!(concat!(
                "../../../../hqc-claude-jsonl/fixtures/",
                $name,
                "-",
                $ver,
                ".jsonl"
            )),
            stream: include_str!(concat!(
                "../../../../hqc-claude-jsonl/fixtures/stream-",
                $name,
                "-",
                $ver,
                ".jsonl"
            )),
            stdin: include_str!(concat!(
                "../../../../hqc-claude-jsonl/fixtures/stdin-",
                $name,
                "-",
                $ver,
                ".jsonl"
            )),
        }
    };
}

fn lines(s: &str) -> Vec<Value> {
    s.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// What the adapter wrote to stdin without the `uuid` of each user message, checked to be a
/// fresh v4 (SPEC §8.1): the T0 spike wrote none.
fn without_uuids(lines: Vec<Value>) -> Vec<Value> {
    let mut seen = HashSet::new();
    lines
        .into_iter()
        .map(|mut v| {
            if v["type"] == "user" {
                let u: Uuid = v["uuid"].as_str().expect("no uuid").parse().unwrap();
                assert_eq!(u.get_version_num(), 4, "{v}");
                assert!(seen.insert(u), "uuid written twice: {v}");
                v.as_object_mut().unwrap().remove("uuid");
            }
            v
        })
        .collect()
}

impl Fixture {
    fn prompt(&self) -> String {
        lines(self.stdin)[0]["message"]["content"]
            .as_str()
            .unwrap()
            .into()
    }

    fn claude_session(&self) -> Uuid {
        lines(self.stream)
            .iter()
            .find(|v| v["type"] == "system" && v["subtype"] == "init")
            .and_then(|v| v["session_id"].as_str())
            .unwrap()
            .parse()
            .unwrap()
    }

    /// Event ids of transcript records that must produce an event: tool calls, tool
    /// results and non-empty text or thinking.
    fn expected_ids(&self) -> HashSet<Uuid> {
        lines(self.transcript)
            .iter()
            .filter(|r| {
                (r["type"] == "assistant" || r["type"] == "user") && r["isSidechain"] != true
            })
            .filter(|r| {
                r["message"]["content"].as_array().is_some_and(|c| {
                    c.iter().any(|b| match b["type"].as_str() {
                        Some("tool_use") | Some("tool_result") => true,
                        Some("text") => b["text"].as_str().is_some_and(|t| !t.is_empty()),
                        Some("thinking") => b["thinking"].as_str().is_some_and(|t| !t.is_empty()),
                        _ => false,
                    })
                })
            })
            .map(|r| Event::id_from_claude_record(&r["uuid"].as_str().unwrap().parse().unwrap()))
            .collect()
    }

    fn all_record_ids(&self) -> HashSet<Uuid> {
        lines(self.transcript)
            .iter()
            .filter_map(|r| r["uuid"].as_str())
            .map(|u| Event::id_from_claude_record(&u.parse().unwrap()))
            .collect()
    }
}

struct Harness {
    adapter: Box<dyn Adapter>,
    events: mpsc::Receiver<AdapterEvent>,
    claude: ScriptedClaude,
    session_id: Uuid,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

fn options(config_dir: &std::path::Path) -> ClaudeOptions {
    ClaudeOptions {
        claude_bin: Some("/opt/test/claude".into()),
        config_dir: Some(config_dir.to_path_buf()),
        early_exit: Duration::from_millis(20),
        stop_grace: Duration::from_millis(100),
        ..ClaudeOptions::default()
    }
}

/// The `default` environment over these `[permissions]`.
fn with_permissions(p: crate::permissions::Permissions) -> Arc<crate::config::Environment> {
    Arc::new(crate::config::Environment::default_of(&p))
}

fn spec(session_id: Uuid, workdir: &std::path::Path, claude_session: Uuid) -> StartSpec {
    // As `Config::check_start` gives it: canonical (/var -> /private/var on macOS).
    let workdir = &workdir.canonicalize().unwrap();
    StartSpec {
        session_id,
        runtime: Runtime::ClaudeCode,
        model: Some("claude-haiku-5-5".into()),
        workspace: workdir.to_path_buf(),
        workdir: workdir.to_path_buf(),
        environment: with_permissions(crate::permissions::Permissions::default()),
        runtime_session_id: Some(claude_session.to_string()),
        system_prompt: None,
        disallowed_tools: vec![],
        restricted: false,
        auto_approve: false,
        session_token: crate::token::Secret::new("st"),
        ask_timeout_s: 600,
        permission_mode: crate::permissions::PermissionMode::Ask,
        folder_rules: None,
    }
}

impl Harness {
    async fn start(stream: &str, claude_session: Uuid) -> Self {
        let work = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let claude = ScriptedClaude::new(stream);
        let factory = ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude.clone()));
        let session_id = Uuid::now_v7();
        let spec = spec(session_id, work.path(), claude_session);
        let (tx, events) = mpsc::channel(1024);
        let mut adapter = factory.create(&spec, tx);
        adapter.start(&spec).await.unwrap();
        Self {
            adapter,
            events,
            claude,
            session_id,
            _dirs: (work, config),
        }
    }

    async fn next(&mut self) -> AdapterEvent {
        tokio::time::timeout(Duration::from_secs(5), self.events.recv())
            .await
            .expect("no adapter event in 5 s")
            .expect("event channel closed")
    }

    /// Runs one turn; `approval` picks the option for each `approval.request`, `clarify`
    /// answers each `clarify.request`.
    async fn turn(
        &mut self,
        text: &str,
        input_id: Uuid,
        approval: &str,
        clarify: BTreeMap<String, ClarifyAnswer>,
    ) -> Vec<AdapterEvent> {
        self.adapter
            .input(&SessionInput {
                session_id: self.session_id,
                event_id: input_id,
                text: text.into(),
                artifact_ids: vec![],
                peer_prefix: vec![],
                peer: None,
                permission_cap: None,
                schedule_id: None,
            })
            .await
            .unwrap();
        let mut out = Vec::new();
        loop {
            let ev = self.next().await;
            out.push(ev.clone());
            match ev {
                AdapterEvent::TurnEnded { .. } => return out,
                AdapterEvent::Event(e) => match &e.kind {
                    EventKind::ApprovalRequest { request_id, .. } => self
                        .adapter
                        .answer_approval(request_id, approval, None)
                        .await
                        .unwrap(),
                    EventKind::ClarifyRequest { request_id, .. } => self
                        .adapter
                        .answer_clarify(request_id, &clarify)
                        .await
                        .unwrap(),
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

/// `accepted`, `delta`, `tool.call`, `agent.message:final`, ... with runs of deltas folded.
fn shape(events: &[AdapterEvent]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for ev in events {
        let s = match ev {
            AdapterEvent::InputAccepted { .. } => "accepted".into(),
            AdapterEvent::TurnEnded { .. } => "turn_ended".into(),
            AdapterEvent::Subagents(n) => format!("subagents:{n}"),
            AdapterEvent::BackgroundTasks(n) => format!("background_tasks:{n}"),
            AdapterEvent::Exited { .. } => "exited".into(),
            AdapterEvent::RuntimeSessionId(_) => "runtime_session_id".into(),
            AdapterEvent::Title(_) => "title".into(),
            AdapterEvent::Commands(_) => "commands".into(),
            // Every `system/init` carries it; the T38 tests look at it on their own.
            AdapterEvent::PermissionMode(_) => continue,
            AdapterEvent::Event(e) => match &e.kind {
                EventKind::AgentDelta { .. } => "delta".into(),
                EventKind::AgentMessage { r#final, .. } => format!("agent.message:{final}"),
                k => serde_json::to_value(k).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            },
        };
        if s == "delta" && out.last().is_some_and(|l| l == "delta") {
            continue;
        }
        out.push(s);
    }
    out
}

fn events(evs: &[AdapterEvent]) -> Vec<&Event> {
    evs.iter()
        .filter_map(|e| match e {
            AdapterEvent::Event(e) => Some(e),
            _ => None,
        })
        .collect()
}

/// Every record-derived event id matches a transcript record, and every record that
/// should produce an event did.
fn check_ids(fx: &Fixture, evs: &[AdapterEvent], session_id: Uuid, input_id: Uuid) {
    let all = fx.all_record_ids();
    let mut from_records = HashSet::new();
    for e in events(evs) {
        assert_eq!(e.session_id, session_id);
        assert_eq!(e.author, Author::Agent { id: session_id });
        assert!(e.seq.is_none());
        match &e.kind {
            EventKind::ToolCall { .. }
            | EventKind::ToolResult { .. }
            | EventKind::AgentMessage { .. }
            | EventKind::AgentThinking { .. } => {
                assert!(
                    all.contains(&e.event_id),
                    "{:?} has no transcript record",
                    e.kind
                );
                from_records.insert(e.event_id);
            }
            _ => assert!(!all.contains(&e.event_id)),
        }
        if let EventKind::AgentMessage { in_reply_to, .. } = &e.kind {
            assert_eq!(*in_reply_to, Some(input_id));
        }
    }
    assert_eq!(from_records, fx.expected_ids());
    assert!(evs.contains(&AdapterEvent::InputAccepted { event_id: input_id }));
}

async fn run(
    fx: &Fixture,
    approval: &str,
    clarify: BTreeMap<String, ClarifyAnswer>,
) -> (Harness, Vec<AdapterEvent>, Uuid) {
    let mut h = Harness::start(fx.stream, fx.claude_session()).await;
    let input_id = Uuid::now_v7();
    let evs = h.turn(&fx.prompt(), input_id, approval, clarify).await;
    check_ids(fx, &evs, h.session_id, input_id);
    (h, evs, input_id)
}

fn approval_options(evs: &[AdapterEvent]) -> Vec<(String, ApprovalCanonical)> {
    events(evs)
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::ApprovalRequest { options, .. } => Some(
                options
                    .iter()
                    .map(|o| (o.id.clone(), o.canonical))
                    .collect(),
            ),
            _ => None,
        })
        .expect("approval.request")
}

/// T13 (SPEC §8.1): when the turn ends the adapter reads the transcript and reports its
/// last `ai-title` before `TurnEnded`.
#[tokio::test]
async fn ai_title_is_reported_at_turn_end() {
    let fx = fixture!("permission-allow");
    let mut h = Harness::start(fx.stream, fx.claude_session()).await;
    let (work, config) = &h._dirs;
    let transcript = super::paths::transcript_path(
        config.path(),
        &work.path().canonicalize().unwrap(),
        &fx.claude_session(),
    );
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    let title = r#"{"type":"ai-title","aiTitle":"Touch a file","sessionId":"x"}"#;
    std::fs::write(&transcript, format!("{}{title}\n", fx.transcript)).unwrap();

    let evs = h
        .turn(&fx.prompt(), Uuid::now_v7(), "once", BTreeMap::new())
        .await;
    assert_eq!(shape(&evs[evs.len() - 2..]), ["title", "turn_ended"]);
    assert_eq!(
        evs[evs.len() - 2],
        AdapterEvent::Title("Touch a file".into())
    );
}

#[tokio::test]
async fn permission_allow_once() {
    let fx = fixture!("permission-allow");
    let (h, evs, _) = run(&fx, "once", BTreeMap::new()).await;
    assert_eq!(
        shape(&evs),
        [
            "accepted",
            "tool.call",
            "approval.request",
            "tool.result",
            "delta",
            "agent.message:true",
            "usage",
            "status",
            "turn_ended"
        ]
    );
    assert_eq!(without_uuids(h.claude.stdin()), lines(fx.stdin));
    assert_eq!(
        approval_options(&evs),
        [
            ("once".into(), ApprovalCanonical::Once),
            ("session".into(), ApprovalCanonical::Session),
            ("always".into(), ApprovalCanonical::Always),
            ("deny".into(), ApprovalCanonical::Deny),
        ]
    );
    let evs = events(&evs);
    let call = evs
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::ToolCall {
                call_id,
                tool,
                args_preview,
            } => Some((call_id.clone(), tool.clone(), args_preview.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(call.0, "toolu_01VkowPqKRFd3dYT6tKPSUwo");
    assert_eq!(call.1, "Bash");
    assert!(call.2.contains("touch hqc-marker-once"));
    let req = evs
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::ApprovalRequest {
                request_id,
                tool,
                description,
                ..
            } => Some((request_id.clone(), tool.clone(), description.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(req.0, "277e1a0e-1e92-4891-b337-a8d7b97fe1ec");
    assert_eq!(req.1, "Bash");
    assert_eq!(
        req.2,
        "Create empty marker file hqc-marker-once\ntouch hqc-marker-once"
    );
    assert!(evs.iter().any(|e| matches!(
        &e.kind,
        EventKind::ToolResult { call_id, error: false, error_text: None, .. }
            if call_id == "toolu_01VkowPqKRFd3dYT6tKPSUwo"
    )));
    assert!(evs.iter().any(|e| matches!(
        &e.kind,
        EventKind::AgentMessage { text, r#final: true, .. } if text == "DONE"
    )));
    assert!(evs.iter().any(|e| matches!(
        &e.kind,
        EventKind::Usage { input_tokens: 4, output_tokens: 96, cache_read: Some(36251), cache_write: Some(12206), cost_usd: Some(_), session_cost_usd: Some(_), model: Some(m) }
            if m == "claude-haiku-5-5"
    )));
    assert!(evs.iter().any(|e| matches!(
        &e.kind,
        EventKind::Status {
            state: SessionState::Idle,
            detail: None,
            ..
        }
    )));
}

#[tokio::test]
async fn permission_deny() {
    let fx = fixture!("permission-deny");
    let (h, evs, _) = run(&fx, "deny", BTreeMap::new()).await;
    assert_eq!(
        shape(&evs),
        [
            "accepted",
            "tool.call",
            "approval.request",
            "tool.result",
            "delta",
            "agent.message:true",
            "usage",
            "status",
            "turn_ended"
        ]
    );
    // The same answer as the spike's, except for the message text and `interrupt`: the
    // VS Code tab's "No" (cc_remote docs/SPEC.md §2.12).
    let mut want = lines(fx.stdin);
    let mut got = without_uuids(h.claude.stdin());
    assert_eq!(
        got[1]["response"]["response"]["message"],
        "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. \
         if it was a file edit, the new_string was NOT written to the file). STOP what you are \
         doing and wait for the user to tell you how to proceed."
    );
    assert_eq!(got[1]["response"]["response"]["interrupt"], true);
    for v in [&mut want[1], &mut got[1]] {
        let r = v["response"]["response"].as_object_mut().unwrap();
        r.remove("message");
        r.remove("interrupt");
    }
    assert_eq!(got, want);
    assert!(events(&evs).iter().any(|e| matches!(
        &e.kind,
        EventKind::ToolResult { error: true, error_text: Some(t), .. } if t == "denied by hqc spike"
    )));
}

#[tokio::test]
async fn permission_always_sends_only_local_add_rules() {
    let fx = fixture!("permission-always");
    let (h, evs, _) = run(&fx, "always", BTreeMap::new()).await;
    assert_eq!(shape(&evs).last().unwrap(), "turn_ended");
    // The spike passed every suggestion; the adapter keeps only addRules → localSettings.
    let mut want = lines(fx.stdin);
    let perms = want[1]["response"]["response"]["updatedPermissions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["type"] == "addRules" && p["destination"] == "localSettings")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(perms.len(), 1);
    want[1]["response"]["response"]["updatedPermissions"] = Value::Array(perms);
    assert_eq!(without_uuids(h.claude.stdin()), want);
}

#[tokio::test]
async fn ask_user_question_becomes_clarify() {
    let fx = fixture!("ask-user-question");
    let answers = BTreeMap::from([(
        "1".to_string(),
        ClarifyAnswer::One("Hub (Recommended)".into()),
    )]);
    let (h, evs, _) = run(&fx, "deny", answers).await;
    assert_eq!(
        shape(&evs),
        [
            "accepted",
            "tool.call",
            "clarify.request",
            "tool.result",
            "delta",
            "agent.message:true",
            "usage",
            "status",
            "turn_ended"
        ]
    );
    let questions = events(&evs)
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::ClarifyRequest { questions, .. } => Some(questions.clone()),
            _ => None,
        })
        .unwrap();
    // The question in full, as the VS Code tab shows it: the header and what each option
    // means.
    assert_eq!(
        questions,
        [ClarifyQuestion {
            id: "1".into(),
            text: "Which area should I focus on first?".into(),
            header: Some("Focus".into()),
            options: Some(vec![
                "Hub (Recommended)".into(),
                "Worker".into(),
                "Protocol".into(),
                "Client".into()
            ]),
            option_descriptions: Some(vec![
                "crates/hqc-hub: axum + Postgres hub work".into(),
                "crates/hqc-worker: daemon, adapters, MCP server".into(),
                "crates/hqc-proto: event and frame schema".into(),
                "client/: TypeScript PWA".into()
            ]),
            multi: Some(false),
        }]
    );
    assert_eq!(without_uuids(h.claude.stdin()), lines(fx.stdin));
}

#[tokio::test]
async fn mcp_timeout_turn_ends_with_detail() {
    let fx = fixture!("mcp-timeout");
    let (h, evs, _) = run(&fx, "once", BTreeMap::new()).await;
    assert_eq!(without_uuids(h.claude.stdin()), lines(fx.stdin));
    let evs = events(&evs);
    assert!(evs.iter().any(|e| matches!(
        &e.kind,
        EventKind::Status { state: SessionState::Idle, detail: Some(d), .. } if d == "error_max_turns"
    )));
    assert!(evs.iter().any(|e| matches!(
        &e.kind,
        EventKind::ToolResult { error: true, error_text: Some(t), .. } if t.contains("timed out after 3s")
    )));
    // No text in the turn: no agent.message.
    assert!(!evs
        .iter()
        .any(|e| matches!(e.kind, EventKind::AgentMessage { .. })));
}

#[tokio::test]
async fn command_line_new_then_resume() {
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let claude = ScriptedClaude::new("");
    let factory = ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude.clone()));
    let claude_session = Uuid::new_v4();
    let s = spec(Uuid::now_v7(), work.path(), claude_session);
    let (tx, mut rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx);

    a.start(&s).await.unwrap();
    let cmd = &claude.commands()[0];
    assert_eq!(cmd.program, std::path::PathBuf::from("/opt/test/claude"));
    assert_eq!(cmd.cwd, s.workdir);
    let settings = crate::permissions::Permissions::default()
        .settings_json()
        .unwrap();
    let expect = |mode: &str| -> Vec<String> {
        [
            "-p",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--include-partial-messages",
            "--replay-user-messages",
            "--permission-prompts",
            "host",
            "--permission-prompt-tool",
            "stdio",
            "--thinking-display",
            "summarized",
            "--strict-mcp-config",
            "--allowedTools",
            "mcp__hqc",
            "--disallowedTools",
            "ListAgents",
            "SendMessage",
            "--settings",
            &settings,
            "--setting-sources",
            "user,local",
            "--permission-mode",
            "default",
            mode,
            &claude_session.to_string(),
            "--verbose",
            "--model",
            "claude-haiku-5-5",
        ]
        .map(String::from)
        .into()
    };
    assert_eq!(cmd.args, expect("--session-id"));
    a.kill().await.unwrap();
    assert_eq!(claude.signals(), [Sig::Terminate]);

    // `resume` without a transcript (the process never got a prompt) starts it new.
    a.resume(&s).await.unwrap();
    assert_eq!(claude.commands()[1].args, expect("--session-id"));
    a.kill().await.unwrap();

    // `start` with a transcript on disk (worker restart) resumes.
    let t = super::paths::transcript_path(config.path(), &s.workdir, &claude_session);
    std::fs::create_dir_all(t.parent().unwrap()).unwrap();
    std::fs::write(&t, "").unwrap();
    a.start(&s).await.unwrap();
    assert_eq!(claude.commands()[2].args, expect("--resume"));
    a.kill().await.unwrap();

    // Killed processes are not reported as exited.
    assert!(rx.try_recv().is_err());
}

/// T30 (SPEC §7.1, §7.2, §8.1): the hqc MCP server goes in `--mcp-config` with the session
/// token in its own `env`, never in the CLI's environment; the CLI gets `MCP_TOOL_TIMEOUT`.
#[tokio::test]
async fn command_line_carries_the_hqc_mcp_server() {
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let claude = ScriptedClaude::new("");
    let launch = super::McpLaunch {
        program: "/opt/hqc/hqc-worker".into(),
        socket: "/home/u/.hqc/mcp.sock".into(),
    };
    let factory =
        ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude.clone())).with_mcp(launch);
    let mut s = spec(Uuid::now_v7(), work.path(), Uuid::new_v4());
    s.session_token = crate::token::Secret::new("hqcs_the_token");
    s.ask_timeout_s = 600;
    let (tx, _rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    let cmd = claude.commands()[0].clone();
    let i = cmd.args.iter().position(|a| a == "--mcp-config").unwrap();
    let cfg: serde_json::Value = serde_json::from_str(&cmd.args[i + 1]).unwrap();
    assert_eq!(
        cfg,
        json!({"mcpServers": {"hqc": {
            "type": "stdio",
            "command": "/opt/hqc/hqc-worker",
            "args": ["mcp", "--socket", "/home/u/.hqc/mcp.sock"],
            "env": {"HQC_SESSION_TOKEN": "hqcs_the_token"},
        }}})
    );
    assert!(cmd.args.contains(&"--strict-mcp-config".to_string()));
    assert!(cmd
        .args
        .windows(2)
        .any(|w| w == ["--allowedTools", "mcp__hqc"]));
    assert!(cmd
        .env
        .contains(&("MCP_TOOL_TIMEOUT".to_string(), "660000".to_string())));
    assert!(
        !cmd.env
            .iter()
            .any(|(k, v)| k.starts_with("HQC_") || v.contains("hqcs_")),
        "{:?}",
        cmd.env
    );
    a.kill().await.unwrap();

    // The token of a later `session.start` goes to the next process.
    s.session_token = crate::token::Secret::new("hqcs_next");
    a.resume(&s).await.unwrap();
    assert!(claude.commands()[1].args[i + 1].contains("hqcs_next"));
    a.kill().await.unwrap();
}

/// SPEC §8.1, §8.4: session restrictions reach the CLI; nothing is dropped silently.
#[tokio::test]
async fn command_line_carries_session_restrictions() {
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let claude = ScriptedClaude::new("");
    let factory = ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude.clone()));
    let claude_session = Uuid::new_v4();
    let mut s = spec(Uuid::now_v7(), work.path(), claude_session);
    s.disallowed_tools = vec!["WebFetch".into(), "Bash(git push:*)".into()];
    s.system_prompt = Some("Answer in Russian.".into());
    let (tx, _rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    let args = claude.commands()[0].args.clone();
    let after = |flag: &str| -> Vec<String> {
        let i = args
            .iter()
            .position(|a| a == flag)
            .unwrap_or_else(|| panic!("{flag} missing: {args:?}"));
        args[i + 1..]
            .iter()
            .take_while(|a| !a.starts_with("--"))
            .cloned()
            .collect()
    };
    // The session's own restrictions first, then Claude Code's cross-session tools, which
    // bypass the hub (NATIVE_MESSAGING_TOOLS).
    assert_eq!(
        after("--disallowedTools"),
        ["WebFetch", "Bash(git push:*)", "ListAgents", "SendMessage"]
    );
    assert_eq!(after("--append-system-prompt"), ["Answer in Russian."]);
    assert_eq!(after("--setting-sources"), ["user,local"]);
    assert_eq!(after("--permission-mode"), ["default"]);
    assert!(!args.contains(&"--restricted".to_string()));
    a.kill().await.unwrap();

    // Restricted: `--restricted` instead of the settings files.
    s.restricted = true;
    s.disallowed_tools.clear();
    s.system_prompt = None;
    a.start(&s).await.unwrap();
    let args = claude.commands()[1].args.clone();
    assert!(args.contains(&"--restricted".to_string()), "{args:?}");
    for absent in ["--setting-sources", "--append-system-prompt"] {
        assert!(!args.contains(&absent.to_string()), "{absent}: {args:?}");
    }
    // Restricted or not, the native messaging tools stay off.
    let i = args.iter().position(|a| a == "--disallowedTools").unwrap();
    assert_eq!(args[i + 1..i + 3], ["ListAgents", "SendMessage"]);
    assert!(args
        .windows(2)
        .any(|w| w == ["--permission-mode", "default"]));
    a.kill().await.unwrap();
}

/// T12 (SPEC §8.1, §14): `[permissions]` reach the CLI as `--settings` on every start,
/// restricted included; a session in auto gets `--permission-mode auto` (T38), the others the
/// CLI's ask mode.
#[tokio::test]
async fn command_line_carries_permissions() {
    use crate::permissions::{PermissionMode, Permissions};
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let claude = ScriptedClaude::new("");
    let factory = ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude.clone()));
    let mut s = spec(Uuid::now_v7(), work.path(), Uuid::new_v4());
    s.environment = with_permissions(Permissions {
        mode: PermissionMode::Ask,
        allow_auto: true,
        allow: vec!["Bash(cargo test:*)".into(), "Read".into()],
        deny: vec!["Bash(git push:*)".into()],
    });
    let (tx, _rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx);
    let settings = |n: usize| -> Value {
        let args = claude.commands()[n].args.clone();
        let i = args
            .iter()
            .position(|a| a == "--settings")
            .unwrap_or_else(|| panic!("--settings missing: {args:?}"));
        serde_json::from_str(&args[i + 1]).unwrap()
    };
    let want = json!({"voiceEnabled": false, "permissions": {
        "allow": ["Bash(cargo test:*)", "Read"],
        "deny": ["Bash(git push:*)"],
    }});
    a.start(&s).await.unwrap();
    assert_eq!(settings(0), want);
    a.kill().await.unwrap();
    s.restricted = true;
    a.start(&s).await.unwrap();
    assert_eq!(settings(1), want, "restricted keeps --settings");
    a.kill().await.unwrap();

    // Both lists empty: only voice off.
    s.environment = with_permissions(Permissions {
        deny: vec![],
        ..Permissions::default()
    });
    a.start(&s).await.unwrap();
    assert_eq!(settings(2), json!({"voiceEnabled": false}));
    a.kill().await.unwrap();

    // The worker config gives the CLI's ask mode; the session's mode picks the flag (T38).
    let base = format!(
        "hub_url = \"https://h\"\nallowed_roots = [{0:?}]\n",
        work.path().display().to_string()
    );
    let cfg = crate::config::Config::parse(&base).unwrap();
    let f = ClaudeCodeAdapters::from_config(&cfg, "manual".into());
    assert_eq!(f.opts.ask_mode, "manual");
    let cfg =
        crate::config::Config::parse(&format!("{base}[permissions]\nmode = \"auto\"\n")).unwrap();
    let f = ClaudeCodeAdapters::from_config(&cfg, "manual".into());
    assert_eq!(f.opts.ask_mode, "manual");
    let mode = |a: &ClaudeCodeAdapter, s: &StartSpec| -> String {
        let args = a.command(s, &Uuid::nil(), false, None).args;
        let i = args.iter().position(|a| a == "--permission-mode").unwrap();
        args[i + 1].clone()
    };
    let a = ClaudeCodeAdapter {
        session_id: s.session_id,
        opts: f.opts.clone(),
        spawner: f.spawner.clone(),
        events: mpsc::channel(1).0,
        proc: None,
    };
    s.permission_mode = PermissionMode::Ask;
    assert_eq!(mode(&a, &s), "manual");
    s.permission_mode = PermissionMode::Auto;
    assert_eq!(mode(&a, &s), "auto", "restricted too");
    s.restricted = false;
    assert_eq!(mode(&a, &s), "auto");
    // The deny list goes with auto as with ask.
    assert!(a
        .command(&s, &Uuid::nil(), false, None)
        .args
        .contains(&"--settings".to_string()));
}

/// T43 (SPEC §14): a session in a bound folder gets its environment's merged rules in
/// `--settings`, its `env` on top of the child allowlist, and no `--strict-mcp-config` with
/// `allow_local_mcp`; a session elsewhere on the same worker gets none of it.
#[tokio::test]
async fn command_line_follows_the_environment() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    for d in ["infra", "other"] {
        std::fs::create_dir(root.join(d)).unwrap();
    }
    let cfg = crate::config::Config::parse(&format!(
        r#"hub_url = "https://h"
allowed_roots = [{root:?}]
[permissions]
allow = ["Read"]
deny = ["Bash(ssh:*)", "Bash(sudo:*)"]
[environments.infra]
env = {{ GITHUB_TOKEN = "ghp_t43", PATH = "/opt/infra/bin", MCP_TOOL_TIMEOUT = "1" }}
allow_local_mcp = true
[environments.infra.permissions]
unblock = ["Bash(ssh:*)"]
allow = ["Bash(ssh debian@192.168.1.20:*)"]
deny = ["Bash(git push:*)"]
[[env_bindings]]
path = "{}"
environment = "infra"
"#,
        root.join("infra").display()
    ))
    .unwrap();
    let config = tempfile::tempdir().unwrap();
    let claude = ScriptedClaude::new("");
    let factory = ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude.clone()));
    let a = ClaudeCodeAdapter {
        session_id: Uuid::nil(),
        opts: factory.opts.clone(),
        spawner: factory.spawner.clone(),
        events: mpsc::channel(1).0,
        proc: None,
    };
    let command = |dir: &str| {
        let mut s = spec(Uuid::now_v7(), &root.join(dir), Uuid::new_v4());
        s.environment = cfg.environment(&s.workspace);
        a.command(&s, &Uuid::nil(), false, None)
    };
    let settings = |args: &[String]| -> Value {
        let i = args.iter().position(|a| a == "--settings").unwrap();
        serde_json::from_str(&args[i + 1]).unwrap()
    };
    let var = |env: &[(String, String)], k: &str| {
        env.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone())
    };

    let infra = command("infra");
    assert_eq!(
        settings(&infra.args)["permissions"],
        json!({
            "allow": ["Read", "Bash(ssh debian@192.168.1.20:*)"],
            "deny": ["Bash(sudo:*)", "Bash(git push:*)"],
        })
    );
    assert!(!infra.args.contains(&"--strict-mcp-config".to_string()));
    assert_eq!(var(&infra.env, "GITHUB_TOKEN").as_deref(), Some("ghp_t43"));
    assert_eq!(var(&infra.env, "PATH").as_deref(), Some("/opt/infra/bin"));
    assert_eq!(
        var(&infra.env, "MCP_TOOL_TIMEOUT").as_deref(),
        Some("660000"),
        "the worker's own variables win"
    );
    assert_eq!(
        infra.env.iter().filter(|(k, _)| k == "PATH").count(),
        1,
        "one PATH"
    );

    let other = command("other");
    assert_eq!(
        settings(&other.args)["permissions"],
        json!({"allow": ["Read"], "deny": ["Bash(ssh:*)", "Bash(sudo:*)"]})
    );
    assert!(other.args.contains(&"--strict-mcp-config".to_string()));
    assert_eq!(var(&other.env, "GITHUB_TOKEN"), None);
}

/// T38: a running process switches with `set_permission_mode` (`default` for ask, the
/// name 2.1.308 takes), and what the CLI then reports in `system/status` and
/// `system/init` becomes [`AdapterEvent::PermissionMode`] when it changes. Fixture
/// `permission-mode-switch`: ask → auto → ask in one process.
#[test]
fn permission_mode_switch_in_the_stream() {
    use crate::permissions::PermissionMode;
    let fx = fixture!("permission-mode-switch", "2.1.308");
    let stdin: Vec<Value> = fx
        .stdin
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let sent: Vec<&str> = stdin
        .iter()
        .filter(|v| v["request"]["subtype"] == "set_permission_mode")
        .map(|v| v["request"]["mode"].as_str().unwrap())
        .collect();
    assert_eq!(sent, ["auto", "default"]);

    let mut core = Core::new(Uuid::now_v7(), false);
    for (mode, want) in [
        (PermissionMode::Auto, "auto"),
        (PermissionMode::Ask, "default"),
    ] {
        let Action::Write(v) = core.set_permission_mode(mode) else {
            panic!("not a write");
        };
        assert_eq!(v["type"], "control_request");
        assert_eq!(
            v["request"],
            json!({"subtype": "set_permission_mode", "mode": want})
        );
        assert!(v["request_id"].as_str().unwrap().starts_with("hqc-mode-"));
    }
    let mut modes = Vec::new();
    let mut requests = 0;
    for line in fx.stream.lines() {
        for a in core.on_line(line) {
            match a {
                Action::Emit(AdapterEvent::PermissionMode(m)) => modes.push(m),
                Action::Emit(AdapterEvent::Event(e))
                    if matches!(e.kind, EventKind::ApprovalRequest { .. }) =>
                {
                    requests += 1
                }
                _ => {}
            }
        }
    }
    // Reported once per change: init (default), the switch to auto, the switch back.
    assert_eq!(
        modes,
        [
            PermissionMode::Ask,
            PermissionMode::Auto,
            PermissionMode::Ask
        ]
    );
    // Only the turn in ask asked.
    assert_eq!(requests, 1);

    // The CLI's answer to our request reports the mode too; a refusal changes nothing.
    let mut core = Core::new(Uuid::now_v7(), false);
    let ok = r#"{"type":"control_response","response":{"subtype":"success","request_id":"hqc-mode-1","response":{"mode":"auto"}}}"#;
    assert_eq!(
        core.on_line(ok),
        [Action::Emit(AdapterEvent::PermissionMode(
            PermissionMode::Auto
        ))]
    );
    // Every confirmation of ours is reported, also of the mode it already had: a capped
    // input waits for it (T39).
    assert_eq!(
        core.on_line(ok),
        [Action::Emit(AdapterEvent::PermissionMode(
            PermissionMode::Auto
        ))]
    );
    let status = r#"{"type":"system","subtype":"status","status":null,"permissionMode":"auto"}"#;
    assert!(
        core.on_line(status).is_empty(),
        "the CLI's own report: on change only"
    );
    let refused = r#"{"type":"control_response","response":{"subtype":"error","request_id":"hqc-mode-2","error":"Cannot set permission mode"}}"#;
    assert!(core.on_line(refused).is_empty());
    // Our echo of an approval answer is not a mode.
    let echo = r#"{"type":"control_response","response":{"subtype":"success","request_id":"277e","response":{"behavior":"allow","mode":"auto"}}}"#;
    assert!(core.on_line(echo).is_empty());
}

/// T38: `set_permission_mode` on the adapter writes the control request to a running
/// process and does nothing without one.
#[tokio::test]
async fn adapter_switches_a_running_process() {
    use crate::permissions::PermissionMode;
    let mut h = Harness::start("", Uuid::new_v4()).await;
    h.adapter
        .set_permission_mode(PermissionMode::Auto)
        .await
        .unwrap();
    let req = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(v) = h
                .claude
                .stdin()
                .into_iter()
                .find(|v| v["request"]["subtype"] == "set_permission_mode")
            {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("no set_permission_mode on stdin");
    assert_eq!(req["request"]["mode"], "auto");
    h.adapter.kill().await.unwrap();
    h.adapter
        .set_permission_mode(PermissionMode::Ask)
        .await
        .unwrap();
}

/// `set_model` on the adapter writes the control request to a running process (2.1.312
/// answers `success` with the model it resolved the name to) and does nothing without one.
/// The answer is no permission mode.
#[tokio::test]
async fn adapter_switches_the_model_of_a_running_process() {
    let mut h = Harness::start("", Uuid::new_v4()).await;
    h.adapter.set_model("sonnet").await.unwrap();
    let req = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(v) = h
                .claude
                .stdin()
                .into_iter()
                .find(|v| v["request"]["subtype"] == "set_model")
            {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("no set_model on stdin");
    assert_eq!(req["type"], "control_request");
    assert_eq!(
        req["request"],
        json!({"subtype": "set_model", "model": "sonnet"})
    );
    assert!(req["request_id"]
        .as_str()
        .unwrap()
        .starts_with("hqc-model-"));
    h.adapter.kill().await.unwrap();
    h.adapter.set_model("haiku").await.unwrap();

    let mut core = Core::new(Uuid::now_v7(), false);
    for answer in [
        r#"{"type":"control_response","response":{"subtype":"success","request_id":"hqc-model-1","response":{"model":"claude-sonnet-5-5"}}}"#,
        r#"{"type":"control_response","response":{"subtype":"error","request_id":"hqc-model-2","error":"Unknown model"}}"#,
    ] {
        assert!(core.on_line(answer).is_empty(), "{answer}");
    }
}

/// The agent gets a cleared environment with only the allowed variables (SPEC §14).
#[tokio::test]
async fn child_environment_is_an_allowlist() {
    let vars = [
        ("PATH", "/usr/bin"),
        ("HOME", "/Users/me"),
        ("USER", "me"),
        ("LANG", "en_US.UTF-8"),
        ("LC_ALL", "C"),
        ("TERM", "xterm"),
        ("TMPDIR", "/tmp/x"),
        ("SSH_AUTH_SOCK", "/tmp/agent"),
        ("CLAUDE_CONFIG_DIR", "/c"),
        ("ANTHROPIC_MODEL", "m"),
        ("HQC_HOME", "/Users/me/.hqc"),
        ("HQC_TOKEN_STORE", "file"),
        ("AWS_SECRET_ACCESS_KEY", "s"),
        ("GITHUB_TOKEN", "t"),
        ("RUST_LOG", "trace"),
    ]
    .map(|(k, v)| (k.to_string(), v.to_string()));
    let env = super::child_env(vars.into_iter());
    let names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        names,
        [
            "ANTHROPIC_MODEL",
            "CLAUDE_CONFIG_DIR",
            "HOME",
            "LANG",
            "LC_ALL",
            "PATH",
            "TERM",
            "TMPDIR",
            "USER"
        ]
    );

    // And the command carries it: no HQC_* of the worker's own environment.
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let claude = ScriptedClaude::new("");
    let factory = ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude.clone()));
    let s = spec(Uuid::now_v7(), work.path(), Uuid::new_v4());
    let (tx, _rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    let env = claude.commands()[0].env.clone();
    assert!(env.iter().any(|(k, _)| k == "PATH"));
    assert!(env.iter().all(|(k, _)| !k.starts_with("HQC_")), "{env:?}");
    a.kill().await.unwrap();
}

/// T44: a worker config with `infra` (key `infra_ed25519`) bound to `<root>/infra`, its
/// store in `<root>/home`, and an adapter factory over the scripted CLI. Under `/tmp`: the
/// agent sockets need a short path.
fn with_keys() -> (
    tempfile::TempDir,
    crate::config::Config,
    ClaudeCodeAdapters,
    ScriptedClaude,
) {
    let dir = tempfile::Builder::new()
        .prefix("hqc-t44-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = dir.path().canonicalize().unwrap();
    for d in ["infra", "other", "config"] {
        std::fs::create_dir(root.join(d)).unwrap();
    }
    let mut cfg = crate::config::Config::parse(&format!(
        "hub_url = \"https://h\"\nallowed_roots = [{root:?}]\n[environments.infra]\n\
         ssh_keys = [\"infra_ed25519\"]\n[[env_bindings]]\npath = \"{}\"\nenvironment = \"infra\"\n",
        root.join("infra").display()
    ))
    .unwrap();
    cfg.ssh = crate::ssh::Store::in_home(&root.join("home"));
    let key = crate::ssh::tests::keygen(&root, "t44-key");
    cfg.ssh.add_key("infra_ed25519", &key).unwrap();
    cfg.check_ssh().unwrap();
    let claude = ScriptedClaude::new("");
    let factory = ClaudeCodeAdapters::new(
        ClaudeOptions {
            ssh: cfg.ssh.clone(),
            ..options(&root.join("config"))
        },
        Arc::new(claude.clone()),
    );
    (dir, cfg, factory, claude)
}

fn auth_sock(cmd: &CommandSpec) -> Option<std::path::PathBuf> {
    cmd.env
        .iter()
        .find(|(k, _)| k == "SSH_AUTH_SOCK")
        .map(|(_, v)| v.into())
}

/// T44: a session whose environment has keys gets `SSH_AUTH_SOCK` of an ssh-agent with
/// them; the agent and its socket go when the process is killed. Two sessions of one
/// environment never share an agent.
#[tokio::test]
async fn ssh_agent_lives_and_dies_with_the_process() {
    let (dir, cfg, factory, claude) = with_keys();
    let root = dir.path().canonicalize().unwrap();
    let infra = |id| {
        let mut s = spec(id, &root.join("infra"), Uuid::new_v4());
        s.environment = cfg.environment(&s.workspace);
        s
    };
    let (s1, s2) = (infra(Uuid::now_v7()), infra(Uuid::now_v7()));
    let (tx, _rx) = mpsc::channel(16);
    let mut a1 = factory.create(&s1, tx.clone());
    let mut a2 = factory.create(&s2, tx);
    a1.start(&s1).await.unwrap();
    a2.start(&s2).await.unwrap();
    let cmds = claude.commands();
    let (sock1, sock2) = (
        auth_sock(&cmds[0]).expect("SSH_AUTH_SOCK"),
        auth_sock(&cmds[1]).expect("SSH_AUTH_SOCK"),
    );
    assert_ne!(sock1, sock2, "one agent per session");
    assert!(sock1.starts_with(&cfg.ssh.agents), "{}", sock1.display());
    for s in [&sock1, &sock2] {
        let keys = crate::ssh::tests::listed(s).expect("an agent listens");
        assert!(keys.contains("t44-key"), "{keys}");
    }

    a1.kill().await.unwrap();
    assert!(!sock1.exists(), "the agent went with the process");
    assert_eq!(crate::ssh::tests::listed(&sock1), None);
    assert!(
        crate::ssh::tests::listed(&sock2).is_some(),
        "the other's lives on"
    );
    a2.kill().await.unwrap();
    assert!(!sock2.exists());
    assert_eq!(std::fs::read_dir(&cfg.ssh.agents).unwrap().count(), 0);
}

/// T44: without keys there is no `SSH_AUTH_SOCK` at all, whatever the worker runs with; a
/// spawn that fails leaves no agent behind.
#[tokio::test]
async fn no_keys_no_ssh_auth_sock() {
    let (dir, cfg, factory, claude) = with_keys();
    let root = dir.path().canonicalize().unwrap();
    let mut s = spec(Uuid::now_v7(), &root.join("other"), Uuid::new_v4());
    s.environment = cfg.environment(&s.workspace);
    assert_eq!(s.environment.name, "default");
    let (tx, _rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx.clone());
    a.start(&s).await.unwrap();
    assert_eq!(auth_sock(&claude.commands()[0]), None);
    a.kill().await.unwrap();

    // The worker's own SSH_AUTH_SOCK (the user's agent) is not in the allowlist.
    let env = super::child_env(
        [("SSH_AUTH_SOCK".to_string(), "/tmp/user-agent".to_string())].into_iter(),
    );
    assert!(env.is_empty(), "{env:?}");

    let missing = ClaudeCodeAdapters::new(
        ClaudeOptions {
            ssh: cfg.ssh.clone(),
            claude_bin: Some(root.join("no-such-claude")),
            ..options(&root.join("config"))
        },
        Arc::new(OsSpawner),
    );
    let mut s = spec(Uuid::now_v7(), &root.join("infra"), Uuid::new_v4());
    s.environment = cfg.environment(&s.workspace);
    let mut a = missing.create(&s, tx);
    assert!(a.start(&s).await.is_err());
    assert_eq!(std::fs::read_dir(&cfg.ssh.agents).unwrap().count(), 0);
}

/// The working directory is checked again before each spawn.
#[cfg(unix)]
#[tokio::test]
async fn spawn_rechecks_the_working_directory() {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().canonicalize().unwrap().join("ws");
    std::fs::create_dir_all(ws.join("sub")).unwrap();
    let config = tempfile::tempdir().unwrap();
    let claude = ScriptedClaude::new("");
    let factory = ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude.clone()));
    let mut s = spec(Uuid::now_v7(), &ws.join("sub"), Uuid::new_v4());
    s.workspace = ws.clone();
    let (tx, _rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    a.kill().await.unwrap();
    std::fs::remove_dir(ws.join("sub")).unwrap();
    std::os::unix::fs::symlink(root.path(), ws.join("sub")).unwrap();
    assert!(a.start(&s).await.is_err());
    assert_eq!(claude.commands().len(), 1, "nothing spawned");
}

#[tokio::test]
async fn stop_interrupts_then_falls_back_to_sigint() {
    let fx = fixture!("permission-allow");
    // Echo and a delta, then silence: the turn never ends by itself.
    let stream: String = fx
        .stream
        .lines()
        .filter(|l| l.contains("\"isReplay\": true") || l.contains("\"text_delta\""))
        .map(|l| format!("{l}\n"))
        .collect();
    let mut h = Harness::start(&stream, fx.claude_session()).await;
    let input_id = Uuid::now_v7();
    h.adapter
        .input(&SessionInput {
            session_id: h.session_id,
            event_id: input_id,
            text: "hi".into(),
            artifact_ids: vec![],
            peer_prefix: vec![],
            peer: None,
            permission_cap: None,
            schedule_id: None,
        })
        .await
        .unwrap();
    assert_eq!(
        h.next().await,
        AdapterEvent::InputAccepted { event_id: input_id }
    );
    assert_eq!(shape(&[h.next().await]), ["delta"]);
    h.adapter.stop().await.unwrap();
    // The scripted CLI ignores the interrupt; after `stop_grace` the group gets SIGINT,
    // the process dies, and the exit is reported.
    let mut last = h.next().await;
    while matches!(last, AdapterEvent::Event(_)) {
        last = h.next().await;
    }
    assert!(
        matches!(last, AdapterEvent::Exited { error: Some(_) }),
        "{last:?}"
    );
    let stdin = h.claude.stdin();
    assert_eq!(stdin[1]["type"], "control_request");
    assert_eq!(stdin[1]["request"], json!({"subtype": "interrupt"}));
    assert_eq!(h.claude.signals(), [Sig::Interrupt]);
    // The process is gone: input fails, kill is a no-op.
    assert!(h
        .adapter
        .input(&SessionInput {
            session_id: h.session_id,
            event_id: Uuid::now_v7(),
            text: "again".into(),
            artifact_ids: vec![],
            peer_prefix: vec![],
            peer: None,
            permission_cap: None,
            schedule_id: None,
        })
        .await
        .is_err());
    h.adapter.kill().await.unwrap();
    assert_eq!(h.claude.signals(), [Sig::Interrupt]);
}

#[tokio::test]
async fn early_exit_fails_start() {
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let mut opts = options(config.path());
    // Exits at once with status 1, like a CLI rejecting its flags.
    opts.claude_bin = Some("/usr/bin/false".into());
    opts.early_exit = Duration::from_secs(5);
    let factory = ClaudeCodeAdapters::new(opts, Arc::new(super::process::OsSpawner));
    let s = spec(Uuid::now_v7(), work.path(), Uuid::new_v4());
    let (tx, mut rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx);
    let e = a.start(&s).await.unwrap_err();
    assert!(
        format!("{e:#}").contains("exited right after start (exit code 1)"),
        "{e:#}"
    );
    // Reported through `start`, not as `Exited`.
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn missing_binary_fails_start() {
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let mut opts = options(config.path());
    opts.claude_bin = Some("/nonexistent/claude".into());
    let factory = ClaudeCodeAdapters::new(opts, Arc::new(super::process::OsSpawner));
    let s = spec(Uuid::now_v7(), work.path(), Uuid::new_v4());
    let (tx, _rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx);
    let e = a.start(&s).await.unwrap_err();
    assert!(format!("{e:#}").contains("/nonexistent/claude"), "{e:#}");
}

// ---- engine-level ----------------------------------------------------------------------

fn can_use_tool_line(fx: &Fixture, request_id: &str, command: Option<&str>) -> String {
    let mut v = lines(fx.stream)
        .into_iter()
        .find(|v| v["type"] == "control_request")
        .unwrap();
    v["request_id"] = json!(request_id);
    if let Some(c) = command {
        v["request"]["input"]["command"] = json!(c);
    }
    v.to_string()
}

fn approvals(actions: &[Action]) -> usize {
    actions
        .iter()
        .filter(|a| {
            matches!(a, Action::Emit(AdapterEvent::Event(e)) if matches!(e.kind, EventKind::ApprovalRequest { .. }))
        })
        .count()
}

fn writes(actions: &[Action]) -> Vec<Value> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Write(v) => Some(v.clone()),
            _ => None,
        })
        .collect()
}

/// Task 2: after `session`, the same request is allowed without `approval.request`.
#[test]
fn session_rule_answers_repeats_itself() {
    let fx = fixture!("permission-allow");
    let mut core = Core::new(Uuid::now_v7(), false);

    let a = core.on_line(&can_use_tool_line(&fx, "r1", None));
    assert_eq!(approvals(&a), 1);
    let w = writes(&core.answer_approval("r1", "session", None).unwrap());
    assert_eq!(w[0]["response"]["response"]["behavior"], "allow");
    assert!(w[0]["response"]["response"]
        .get("updatedPermissions")
        .is_none());

    let a = core.on_line(&can_use_tool_line(&fx, "r2", None));
    assert_eq!(approvals(&a), 0);
    let w = writes(&a);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0]["response"]["request_id"], "r2");
    assert_eq!(w[0]["response"]["response"]["behavior"], "allow");
    assert_eq!(
        w[0]["response"]["response"]["updatedInput"]["command"],
        "touch hqc-marker-once"
    );

    // The suggested rule `touch hqc-marker-once *` covers arguments, not other commands
    // or chained ones.
    let a = core.on_line(&can_use_tool_line(
        &fx,
        "r3",
        Some("touch hqc-marker-once -c"),
    ));
    assert_eq!((approvals(&a), writes(&a).len()), (0, 1));
    let a = core.on_line(&can_use_tool_line(&fx, "r4", Some("touch other")));
    assert_eq!((approvals(&a), writes(&a).len()), (1, 0));
    let a = core.on_line(&can_use_tool_line(
        &fx,
        "r5",
        Some("touch hqc-marker-once && rm -rf ~"),
    ));
    assert_eq!((approvals(&a), writes(&a).len()), (1, 0));

    // A new process starts with no rules.
    let mut fresh = Core::new(Uuid::now_v7(), false);
    assert_eq!(
        approvals(&fresh.on_line(&can_use_tool_line(&fx, "r6", None))),
        1
    );
}

#[test]
fn approval_errors_and_restricted_always() {
    let fx = fixture!("permission-allow");
    let mut core = Core::new(Uuid::now_v7(), true);
    let a = core.on_line(&can_use_tool_line(&fx, "r1", None));
    // Restricted: no `always` offered.
    let ids: Vec<String> = match &a[0] {
        Action::Emit(AdapterEvent::Event(e)) => match &e.kind {
            EventKind::ApprovalRequest { options, .. } => {
                options.iter().map(|o| o.id.clone()).collect()
            }
            _ => panic!(),
        },
        _ => panic!(),
    };
    assert_eq!(ids, ["once", "session", "deny"]);
    assert!(core.answer_approval("r1", "bogus", None).is_err());
    assert!(core.answer_approval("nope", "once", None).is_err());
    // `always` in restricted mode behaves as `session`: no updatedPermissions.
    let w = writes(&core.answer_approval("r1", "always", None).unwrap());
    assert!(w[0]["response"]["response"]
        .get("updatedPermissions")
        .is_none());
    assert_eq!(core.session_rules(), 2);
    // Answered: a second answer is an error.
    assert!(core.answer_approval("r1", "once", None).is_err());
}

fn policy(allow: &[&str], deny: &[&str]) -> Arc<crate::permissions::Policy> {
    Arc::new(crate::permissions::Policy::new(
        &crate::permissions::Permissions {
            mode: crate::permissions::PermissionMode::Ask,
            allow_auto: true,
            allow: allow.iter().map(|r| r.to_string()).collect(),
            deny: deny.iter().map(|r| r.to_string()).collect(),
        },
    ))
}

fn system_events(actions: &[Action]) -> Vec<(String, String)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Emit(AdapterEvent::Event(e)) => match &e.kind {
                EventKind::System { code, text, .. } => {
                    assert_eq!(e.author, Author::System);
                    Some((code.clone(), text.clone()))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn approval_of(actions: &[Action]) -> (Vec<String>, Option<hqc_proto::event::PolicyHint>) {
    actions
        .iter()
        .find_map(|a| match a {
            Action::Emit(AdapterEvent::Event(e)) => match &e.kind {
                EventKind::ApprovalRequest {
                    options,
                    policy_hint,
                    ..
                } => Some((options.iter().map(|o| o.id.clone()).collect(), *policy_hint)),
                _ => None,
            },
            _ => None,
        })
        .expect("approval.request")
}

/// T12: a `can_use_tool` that a deny rule of `[permissions]` matches never becomes a card
/// with `always` (or any option): the adapter denies it and says so with `system
/// policy_denied`. The CLI itself does not ask about such calls (2.1.308), this is the
/// second line.
#[test]
fn always_is_not_offered_for_denied_calls() {
    let fx = fixture!("always-git-push", "2.1.308");
    let line = can_use_tool_line(&fx, "r1", None);
    assert!(line.contains("git push origin master"), "{line}");

    // Without a policy the same request offers `always`.
    let mut plain = Core::new(Uuid::now_v7(), false);
    let (ids, hint) = approval_of(&plain.on_line(&line));
    assert_eq!(ids, ["once", "session", "always", "deny"]);
    assert_eq!(hint, None);

    let mut core = Core::new(Uuid::now_v7(), false).with_policy(policy(&[], &["Bash(git push:*)"]));
    let a = core.on_line(&line);
    assert_eq!(approvals(&a), 0);
    let w = writes(&a);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0]["response"]["request_id"], "r1");
    assert_eq!(w[0]["response"]["response"]["behavior"], "deny");
    let msg = w[0]["response"]["response"]["message"].as_str().unwrap();
    assert!(msg.contains("Bash(git push:*)"), "{msg}");
    let sys = system_events(&a);
    assert_eq!(sys.len(), 1);
    assert_eq!(sys[0].0, "policy_denied");
    assert!(sys[0].1.contains("git push origin master"), "{}", sys[0].1);
    // Nothing is left open to answer with `always` later.
    assert!(core.answer_approval("r1", "always", None).is_err());

    // A chained command with a denied part is denied too.
    let a = core.on_line(&can_use_tool_line(&fx, "r2", Some("git add . && git push")));
    assert_eq!((approvals(&a), writes(&a).len()), (0, 1));
}

/// T48: the folder's hqc rules answer `can_use_tool` with allow, after the deny check and
/// only when every part of a compound command is covered; a change of the shared list
/// reaches the running process at its next request; a request that is asked carries the
/// suggested rules.
#[test]
fn folder_rules_in_can_use_tool() {
    let fx = fixture!("always-git-push", "2.1.308");
    let rules = crate::permissions::FolderRules::default();
    rules.set(&["Bash(python3 *)".into(), "Bash(git push *)".into()]);
    let root = std::path::PathBuf::from("/w/proj");
    let mut core = Core::new(Uuid::now_v7(), false)
        .with_policy(policy(&[], &["Bash(git push:*)"]))
        .with_folder(rules.clone(), root);
    let allowed = |a: &[Action]| {
        approvals(a) == 0
            && writes(a)
                .first()
                .is_some_and(|w| w["response"]["response"]["behavior"] == "allow")
    };

    assert!(allowed(&core.on_line(&can_use_tool_line(
        &fx,
        "r1",
        Some("python3 tools/conv.py x")
    ))));
    // `deny` of the worker wins over an hqc rule.
    let a = core.on_line(&can_use_tool_line(
        &fx,
        "r2",
        Some("git push origin master"),
    ));
    assert_eq!(approvals(&a), 0);
    assert_eq!(writes(&a)[0]["response"]["response"]["behavior"], "deny");
    // A part no rule covers: asked, with a suggestion per part.
    let a = core.on_line(&can_use_tool_line(
        &fx,
        "r3",
        Some("python3 x && git -C sub fetch"),
    ));
    assert_eq!(approvals(&a), 1);
    let sugg = a
        .iter()
        .find_map(|a| match a {
            Action::Emit(AdapterEvent::Event(e)) => match &e.kind {
                EventKind::ApprovalRequest {
                    rule_suggestions, ..
                } => rule_suggestions.clone(),
                _ => None,
            },
            _ => None,
        })
        .expect("suggestions");
    let rules_suggested: Vec<_> = sugg.iter().map(|s| s.rule.as_deref()).collect();
    assert_eq!(
        rules_suggested,
        [Some("Bash(python3 *)"), Some("Bash(git fetch:*)")]
    );
    // The list changes while the process runs: the next request sees it.
    rules.set(&["Bash(python3 *)".into(), "Bash(git fetch:*)".into()]);
    assert!(allowed(&core.on_line(&can_use_tool_line(
        &fx,
        "r4",
        Some("python3 x && git -C sub fetch")
    ))));
    rules.set(&[]);
    let a = core.on_line(&can_use_tool_line(&fx, "r5", Some("python3 x")));
    assert_eq!(approvals(&a), 1);
}

/// T12: `approval.request.policy_hint` is `allow` when an allow rule matches a call the CLI
/// still asked about, absent otherwise.
#[test]
fn approval_carries_policy_hint() {
    let fx = fixture!("always-git-push", "2.1.308");
    let mut core = Core::new(Uuid::now_v7(), false)
        .with_policy(policy(&["Bash(cargo test:*)"], &["Bash(sudo:*)"]));
    let a = core.on_line(&can_use_tool_line(&fx, "r1", Some("cargo test -p x")));
    assert_eq!(approval_of(&a).1, Some(hqc_proto::event::PolicyHint::Allow));
    let a = core.on_line(&can_use_tool_line(&fx, "r2", Some("touch x")));
    let (ids, hint) = approval_of(&a);
    assert_eq!(hint, None);
    assert!(ids.contains(&"always".to_string()));
    let ev = a
        .iter()
        .find_map(|a| match a {
            Action::Emit(AdapterEvent::Event(e)) => Some(serde_json::to_value(e).unwrap()),
            _ => None,
        })
        .unwrap();
    assert!(ev["payload"].get("policy_hint").is_none(), "{ev}");
}

/// T12 fixture `settings-deny` (2.1.308): the CLI refused `git push` by a `--settings` deny
/// rule without `can_use_tool`; the adapter turns `system/permission_denied` into `system
/// policy_denied`, and the call outside both lists is asked as usual.
#[test]
fn cli_denial_becomes_a_system_event() {
    let fx = fixture!("settings-deny", "2.1.308");
    let mut core = Core::new(Uuid::now_v7(), false)
        .with_policy(policy(&["Bash(git status:*)"], &["Bash(git push:*)"]));
    let mut all = Vec::new();
    for l in fx.stream.lines() {
        all.extend(core.on_line(l));
    }
    let sys = system_events(&all);
    assert_eq!(sys.len(), 1, "{sys:?}");
    assert_eq!(sys[0].0, "policy_denied");
    assert!(sys[0].1.contains("git push origin master"), "{}", sys[0].1);
    let (_, hint) = approval_of(&all);
    assert_eq!(hint, None, "touch other.txt is in neither list");
    assert_eq!(approvals(&all), 1);
    // The denied call still gets its `tool.result` with the CLI's message.
    assert!(all.iter().any(|a| matches!(a,
        Action::Emit(AdapterEvent::Event(e)) if matches!(&e.kind,
            EventKind::ToolResult { error: true, result_preview, .. } if result_preview.contains("has been denied")))));

    // A classifier refusal (mode auto) is reported, but not as the worker's policy.
    let mut core = Core::new(Uuid::now_v7(), false);
    let a = core.on_line(
        r#"{"type":"system","subtype":"permission_denied","tool_name":"Bash","tool_use_id":"t","decision_reason_type":"classifier","message":"Blocked."}"#,
    );
    assert_eq!(
        system_events(&a),
        [("permission_denied".into(), "Blocked.".into())]
    );
}

#[test]
fn clarify_cancel_and_multi() {
    let fx = fixture!("ask-user-question");
    let req = lines(fx.stream)
        .into_iter()
        .find(|v| v["type"] == "control_request")
        .unwrap();
    let mut core = Core::new(Uuid::now_v7(), false);
    core.on_line(&req.to_string());
    let rid = req["request_id"].as_str().unwrap();
    // The hub cancelled: empty answers → deny.
    let w = writes(&core.answer_clarify(rid, &BTreeMap::new()).unwrap());
    assert_eq!(w[0]["response"]["response"]["behavior"], "deny");
    assert!(core.answer_clarify(rid, &BTreeMap::new()).is_err());

    core.on_line(&req.to_string());
    let answers = BTreeMap::from([
        (
            "1".to_string(),
            ClarifyAnswer::Many(vec!["Hub (Recommended)".into(), "Worker".into()]),
        ),
        ("7".to_string(), ClarifyAnswer::One("ignored".into())),
    ]);
    let w = writes(&core.answer_clarify(rid, &answers).unwrap());
    assert_eq!(
        w[0]["response"]["response"]["updatedInput"]["answers"],
        json!({"Which area should I focus on first?": "Hub (Recommended), Worker"})
    );
}

/// `AskUserQuestion` in full (SPEC §4 `clarify.request`): the header, and descriptions paired
/// with the labels (an option without a label goes with its description), `""` where an
/// option has none; neither field when empty. The answer is the label, the typed text of
/// "Other", or the labels joined with ", " (cc_remote docs/SPEC.md §2.11).
#[test]
fn clarify_question_in_full_and_free_text() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let req = json!({"type": "control_request", "request_id": "q1", "request": {
        "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "input": {"questions": [
            {"question": "Pick a color", "header": "Color", "multiSelect": false, "options": [
                {"label": "Red", "description": "Warm color"},
                {"description": "no label"},
                {"label": "Blue"}]},
            {"question": "Which checks?", "header": " ", "multiSelect": true, "options": [
                {"label": "fmt"}, {"label": "clippy"}]}]}}});
    let a = core.on_line(&req.to_string());
    let qs = match &a[..] {
        [Action::Emit(AdapterEvent::Event(e))] => match &e.kind {
            EventKind::ClarifyRequest { questions, .. } => questions.clone(),
            k => panic!("{k:?}"),
        },
        other => panic!("{other:?}"),
    };
    assert_eq!(qs[0].header.as_deref(), Some("Color"));
    assert_eq!(qs[0].options, Some(vec!["Red".into(), "Blue".into()]));
    assert_eq!(
        qs[0].option_descriptions,
        Some(vec!["Warm color".into(), "".into()])
    );
    assert_eq!((&qs[1].header, &qs[1].option_descriptions), (&None, &None));
    let wire = serde_json::to_value(&qs[1]).unwrap();
    assert!(wire.get("header").is_none() && wire.get("option_descriptions").is_none());

    let answers = BTreeMap::from([
        (
            "1".to_string(),
            ClarifyAnswer::One("Teal, as in the logo".into()),
        ),
        (
            "2".to_string(),
            ClarifyAnswer::Many(vec!["fmt".into(), "clippy".into()]),
        ),
    ]);
    let w = writes(&core.answer_clarify("q1", &answers).unwrap());
    let r = &w[0]["response"]["response"];
    assert_eq!(r["behavior"], "allow");
    assert_eq!(
        r["updatedInput"]["answers"],
        json!({"Pick a color": "Teal, as in the logo", "Which checks?": "fmt, clippy"})
    );
    assert_eq!(
        r["updatedInput"]["questions"],
        req["request"]["input"]["questions"]
    );
}

/// A denial is the VS Code tab's (cc_remote docs/SPEC.md §2.12): "No" stops the turn
/// (`interrupt: true`), a text answer gives the reason after two spaces and lets the turn go
/// on. After "No" the CLI ends the turn by itself (lines of a live run on 2.1.312): the turn
/// ends as after a stop, no `agent.message`, the interrupt marker maps to nothing.
#[test]
fn deny_is_the_tabs_no() {
    let fx = fixture!("permission-allow");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    core.input(&SessionInput {
        session_id: sid,
        event_id: Uuid::now_v7(),
        text: "x".into(),
        artifact_ids: vec![],
        peer_prefix: vec![],
        peer: None,
        permission_cap: None,
        schedule_id: None,
    });
    core.on_line(&can_use_tool_line(&fx, "r1", None));
    let w = writes(&core.answer_approval("r1", "deny", None).unwrap());
    assert_eq!(
        w[0]["response"]["response"],
        json!({"behavior": "deny", "interrupt": true, "message": "The user doesn't want to \
            proceed with this tool use. The tool use was rejected (eg. if it was a file edit, \
            the new_string was NOT written to the file). STOP what you are doing and wait for \
            the user to tell you how to proceed."})
    );
    core.on_line(&can_use_tool_line(&fx, "r2", None));
    let w = writes(
        &core
            .answer_approval("r2", "deny", Some("Explain instead"))
            .unwrap(),
    );
    assert_eq!(
        w[0]["response"]["response"],
        json!({"behavior": "deny", "interrupt": false, "message": "The user doesn't want to \
            proceed with this tool use. The tool use was rejected (eg. if it was a file edit, \
            the new_string was NOT written to the file). The user provided the following \
            reason for the rejection:  Explain instead"})
    );

    let mut all = Vec::new();
    for l in [
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"The user doesn't want to proceed with this tool use.","is_error":true,"tool_use_id":"toolu_01"}]},"parent_tool_use_id":null,"uuid":"e9fde0d1-1e44-4e17-ae45-df0c71e1522e","tool_use_result":"User rejected tool use"}"#,
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]},"parent_tool_use_id":null,"uuid":"a8ce3d78-0607-46d6-b845-90c476bf4429"}"#,
        r#"{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":3,"terminal_reason":"aborted_tools","total_cost_usd":0.001,"usage":{"input_tokens":4,"output_tokens":2}}"#,
    ] {
        all.extend(core.on_line(l));
    }
    let kinds: Vec<String> = all
        .iter()
        .filter_map(|a| match a {
            Action::Emit(AdapterEvent::Event(e)) => Some(match &e.kind {
                EventKind::Status { detail, .. } => format!("status:{detail:?}"),
                k => serde_json::to_value(k).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            }),
            Action::Emit(AdapterEvent::TurnEnded { .. }) => Some("turn_ended".into()),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "tool.result",
            "usage",
            "status:Some(\"error_during_execution\")",
            "turn_ended"
        ]
    );
}

/// "No" to one of two parallel calls ends the turn, and the CLI drops the other request: it
/// is cancelled with `stopped` at the turn's end, so the hub closes its card.
#[test]
fn no_cancels_the_other_open_request() {
    let fx = fixture!("permission-allow");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    core.on_line(&can_use_tool_line(&fx, "r1", None));
    core.on_line(&can_use_tool_line(&fx, "r2", Some("touch other")));
    core.answer_approval("r1", "deny", None).unwrap();
    let a = core.on_line(
        r#"{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":2,"terminal_reason":"aborted_tools","total_cost_usd":0.001,"usage":{"input_tokens":4,"output_tokens":2}}"#,
    );
    let cancels: Vec<(String, CancelReason)> = events(&emitted(a))
        .into_iter()
        .filter_map(|e| match &e.kind {
            EventKind::ApprovalCancel { request_id, reason } => Some((request_id.clone(), *reason)),
            _ => None,
        })
        .collect();
    assert_eq!(cancels, [("r2".to_string(), CancelReason::Stopped)]);
    assert!(core.answer_approval("r2", "once", None).is_err());
}

/// A background subagent's request (`agent_id`; a live run on 2.1.312) outlives the main turn's
/// `result`, and the CLI still waits for its answer: it stays open and can be answered, while
/// the main agent's request is cancelled.
#[test]
fn turn_end_cancels_only_the_main_agents_requests() {
    let fx = fixture!("permission-allow");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    core.on_line(&can_use_tool_line(&fx, "main", None));
    let mut sub: Value = serde_json::from_str(&can_use_tool_line(
        &fx,
        "sub",
        Some("touch hqc-live-check.txt"),
    ))
    .unwrap();
    sub["request"]["agent_id"] = json!("a8b8fdc3b4a031d4f");
    assert_eq!(approvals(&core.on_line(&sub.to_string())), 1);
    let a = core.on_line(
        r#"{"type":"result","subtype":"success","is_error":false,"num_turns":2,"result":"LAUNCHED","total_cost_usd":0.004,"usage":{"input_tokens":4,"output_tokens":165}}"#,
    );
    let cancels: Vec<(String, CancelReason)> = events(&emitted(a))
        .into_iter()
        .filter_map(|e| match &e.kind {
            EventKind::ApprovalCancel { request_id, reason } => Some((request_id.clone(), *reason)),
            _ => None,
        })
        .collect();
    assert_eq!(cancels, [("main".to_string(), CancelReason::Stopped)]);
    let w = writes(&core.answer_approval("sub", "once", None).unwrap());
    assert_eq!(w[0]["response"]["request_id"], "sub");
    assert_eq!(w[0]["response"]["response"]["behavior"], "allow");
}

/// A foreground subagent (its `task_started` said `is_backgrounded: false`) cannot outlive the
/// main turn: after "No" the CLI drops its request, which is cancelled at `result` like the main
/// agent's; a background subagent's stays open.
#[test]
fn turn_end_cancels_a_foreground_subagents_request() {
    let fx = fixture!("permission-allow");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    play(
        &mut core,
        r#"{"type":"system","subtype":"task_started","task_id":"ac5672f5fc80df8ec","tool_use_id":"toolu_01TYRCeySHmvXd7Md3Y1rgb8","description":"Run ping and reply with 4","subagent_type":"general-purpose","is_backgrounded":false,"spawn_depth":1,"task_type":"local_agent","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e0d"}
{"type":"system","subtype":"task_started","task_id":"a8b8fdc3b4a031d4f","tool_use_id":"toolu_01BR8iav6ZtPUAhpuCWdQPCt","description":"Touch hqc-live-check file","is_backgrounded":true,"task_type":"local_agent","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e13"}"#,
    );
    for (request_id, agent) in [("fg", "ac5672f5fc80df8ec"), ("bg", "a8b8fdc3b4a031d4f")] {
        let mut v: Value =
            serde_json::from_str(&can_use_tool_line(&fx, request_id, Some("touch x"))).unwrap();
        v["request"]["agent_id"] = json!(agent);
        assert_eq!(approvals(&core.on_line(&v.to_string())), 1);
    }
    let a = core.on_line(
        r#"{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":2,"terminal_reason":"aborted_tools","total_cost_usd":0.001,"usage":{"input_tokens":4,"output_tokens":2}}"#,
    );
    let cancels: Vec<(String, CancelReason)> = events(&emitted(a))
        .into_iter()
        .filter_map(|e| match &e.kind {
            EventKind::ApprovalCancel { request_id, reason } => Some((request_id.clone(), *reason)),
            _ => None,
        })
        .collect();
    assert_eq!(cancels, [("fg".to_string(), CancelReason::Stopped)]);
    assert!(core.answer_approval("fg", "once", None).is_err());
    let w = writes(&core.answer_approval("bg", "once", None).unwrap());
    assert_eq!(w[0]["response"]["request_id"], "bg");
}

/// "No" interrupts the turn for a request of the main agent or a foreground subagent; for a
/// background subagent's, which may outlive the turn it came in, it only denies the call: the
/// turn that runs by then is another one.
#[test]
fn no_to_a_background_subagent_does_not_interrupt() {
    let fx = fixture!("permission-allow");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    play(
        &mut core,
        r#"{"type":"system","subtype":"task_started","task_id":"ac5672f5fc80df8ec","tool_use_id":"toolu_01TYRCeySHmvXd7Md3Y1rgb8","description":"Run ping and reply with 4","subagent_type":"general-purpose","is_backgrounded":false,"spawn_depth":1,"task_type":"local_agent","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e0d"}
{"type":"system","subtype":"task_started","task_id":"a8b8fdc3b4a031d4f","tool_use_id":"toolu_01BR8iav6ZtPUAhpuCWdQPCt","description":"Touch hqc-live-check file","is_backgrounded":true,"task_type":"local_agent","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e13"}"#,
    );
    for (request_id, agent) in [
        ("main", None),
        ("fg", Some("ac5672f5fc80df8ec")),
        ("bg", Some("a8b8fdc3b4a031d4f")),
    ] {
        let mut v: Value =
            serde_json::from_str(&can_use_tool_line(&fx, request_id, Some("touch x"))).unwrap();
        if let Some(agent) = agent {
            v["request"]["agent_id"] = json!(agent);
        }
        core.on_line(&v.to_string());
    }
    let interrupt = |core: &mut Core, id: &str| {
        writes(&core.answer_approval(id, "deny", None).unwrap())[0]["response"]["response"]
            ["interrupt"]
            .clone()
    };
    assert_eq!(interrupt(&mut core, "main"), true);
    assert_eq!(interrupt(&mut core, "fg"), true);
    assert_eq!(interrupt(&mut core, "bg"), false);
}

/// The `status idle` that ends a turn names the turn's input (`in_reply_to`): the hub fails an
/// `hqc_ask` only for the input whose turn ended without a final message, even when the next
/// input's ack reaches it first (SPEC §7.1).
#[test]
fn end_of_turn_status_names_its_input() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    for _ in 0..2 {
        let input = started(&mut core, sid);
        let evs = emitted(core.on_line(
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":1,"terminal_reason":"aborted_tools","total_cost_usd":0.001,"usage":{"input_tokens":4,"output_tokens":2}}"#,
        ));
        let status = events(&evs)
            .into_iter()
            .find(|e| matches!(e.kind, EventKind::Status { .. }))
            .map(|e| serde_json::to_value(&e.kind).unwrap())
            .unwrap();
        assert_eq!(
            status["payload"],
            json!({"state": "idle", "detail": "error_during_execution", "in_reply_to": input})
        );
    }
}

/// Stop reaches a turn the CLI starts by itself after a background task's end (`init` with no
/// input of ours): it is interrupted like ours, and after its `result` nothing runs.
#[test]
fn a_turn_the_cli_starts_by_itself_can_be_stopped() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    play(
        &mut core,
        r#"{"type":"result","subtype":"success","is_error":false,"num_turns":2,"result":"LAUNCHED","total_cost_usd":0.004,"usage":{"input_tokens":4,"output_tokens":165}}
{"type":"system","subtype":"task_notification","task_id":"abd6ecd27912266e5","tool_use_id":"toolu_018foiN3RMuTb1fmFn53JffP","status":"completed","summary":"done","uuid":"614bde30-70d7-46cc-9ff9-9e22f9bdc118"}"#,
    );
    assert!(core.interrupt().is_none(), "no turn runs");
    play(
        &mut core,
        r#"{"type":"system","subtype":"init","model":"claude-haiku-5-5","permissionMode":"default","uuid":"fb2b6af8-8921-4277-a09e-61e710bfcdb2"}"#,
    );
    let (req, turn) = core.interrupt().expect("the CLI's own turn runs");
    assert_eq!(writes(&[req])[0]["request"]["subtype"], "interrupt");
    assert!(core.turn_running(turn));
    play(
        &mut core,
        r#"{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":1,"total_cost_usd":0.001,"usage":{"input_tokens":4,"output_tokens":2}}"#,
    );
    assert!(!core.turn_running(turn));
    assert!(core.interrupt().is_none());
}

/// An input written while the CLI runs a turn it started by itself after a background task is
/// only queued: its echo comes after that turn's `result` (lines of a live run on 2.1.312). That
/// turn's final message and `status idle` name no input; the input's own turn, after the echo,
/// is named by it.
#[test]
fn a_turn_the_cli_starts_by_itself_names_no_input() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    play(
        &mut core,
        r#"{"type":"assistant","message":{"model":"claude-haiku-5-5","role":"assistant","content":[{"type":"text","text":"LAUNCHED"}]},"parent_tool_use_id":null,"uuid":"facbaf1b-a768-4ef9-bb9a-e4d10fbbad9f"}
{"type":"result","subtype":"success","is_error":false,"num_turns":2,"result":"LAUNCHED","total_cost_usd":0.00426204,"usage":{"input_tokens":4,"output_tokens":165}}
{"type":"system","subtype":"task_notification","task_id":"abd6ecd27912266e5","tool_use_id":"toolu_018foiN3RMuTb1fmFn53JffP","status":"completed","summary":"The command ran and printed `hqc-live-check`.","uuid":"614bde30-70d7-46cc-9ff9-9e22f9bdc118"}"#,
    );
    let queued = written(&mut core, sid, "Reply with the single word PONG.");
    let replies = |a: Vec<Action>| -> Vec<(String, Option<Uuid>)> {
        events(&emitted(a))
            .into_iter()
            .filter_map(|e| match &e.kind {
                EventKind::AgentMessage { in_reply_to, .. } => {
                    Some(("agent.message".into(), *in_reply_to))
                }
                EventKind::Status { in_reply_to, .. } => Some(("status".into(), *in_reply_to)),
                _ => None,
            })
            .collect()
    };
    let by_itself = play(
        &mut core,
        r#"{"type":"system","subtype":"init","model":"claude-haiku-5-5","permissionMode":"default","uuid":"fb2b6af8-8921-4277-a09e-61e710bfcdb2"}
{"type":"assistant","message":{"model":"claude-haiku-5-5","role":"assistant","content":[{"type":"text","text":"The background agent finished. It ran `echo hqc-live-check` and reported the output `hqc-live-check`."}]},"parent_tool_use_id":null,"uuid":"6943d433-0cb8-475e-aeae-4c90b14e4380"}
{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.00609755,"usage":{"input_tokens":4,"output_tokens":38}}"#,
    );
    assert_eq!(
        replies(by_itself),
        [("agent.message".into(), None), ("status".into(), None)]
    );
    let next = play(
        &mut core,
        &format!(
            r#"{{"type":"system","subtype":"init","model":"claude-haiku-5-5","permissionMode":"default","uuid":"4c2af2ed-8f49-404b-be85-fbf9f4ebdd58"}}
{}
{{"type":"assistant","message":{{"model":"claude-haiku-5-5","role":"assistant","content":[{{"type":"text","text":"PONG"}}]}},"parent_tool_use_id":null,"uuid":"cd9aae03-1988-42bd-a973-f068747cca40"}}
{{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.00631608,"usage":{{"input_tokens":4,"output_tokens":5}}}}"#,
            echo(
                &Uuid::new_v4().to_string(),
                "Reply with the single word PONG."
            )
        ),
    );
    assert_eq!(
        replies(next),
        [
            ("agent.message".into(), Some(queued)),
            ("status".into(), Some(queued))
        ]
    );
}

#[test]
fn control_response_echo_and_unknown_requests() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let echo = r#"{"type":"control_response","response":{"subtype":"success","request_id":"x","response":{"behavior":"allow"}}}"#;
    assert!(core.on_line(echo).is_empty());
    assert!(core.on_line("not json").is_empty());
    let w = writes(&core.on_line(
        r#"{"type":"control_request","request_id":"h1","request":{"subtype":"hook_callback"}}"#,
    ));
    assert_eq!(w[0]["response"]["subtype"], "error");
    assert_eq!(w[0]["response"]["request_id"], "h1");
    // A replay echo with no input in flight is ignored.
    assert!(core
        .on_line(r#"{"type":"user","isReplay":true,"uuid":"adbf4a08-b683-4a10-af11-6bd389a5a73e","message":{"role":"user","content":"x"}}"#)
        .is_empty());
}

/// T16 (SPEC §8.1, §12, fixtures/inbox): an input with an image attachment is a content
/// array of the text (with the block of paths) and a base64 image block; other kinds, types
/// the API does not take and oversized images reach the agent by path only.
#[tokio::test]
async fn image_attachments_are_image_blocks() {
    use crate::inbox::InboxFile;
    use hqc_proto::event::ArtifactKind;
    let fx = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/inbox");
    let text = std::fs::read_to_string(fx.join("expected-text.txt")).unwrap();
    let want: Value =
        serde_json::from_str(&std::fs::read_to_string(fx.join("expected-stdin.json")).unwrap())
            .unwrap();
    let file = |name: &str, kind, mime: &str, size| InboxFile {
        artifact_id: Uuid::nil(),
        path: fx.join("artifacts").join(name),
        rel: name.into(),
        kind,
        mime: mime.into(),
        size,
    };
    let files = [
        file("screen.png", ArtifactKind::Image, "image/png", 69),
        file("fix.patch", ArtifactKind::Diff, "text/plain", 93),
        file("screen.png", ArtifactKind::Image, "image/svg+xml", 69),
        file(
            "screen.png",
            ArtifactKind::Image,
            "image/png",
            super::MAX_INLINE_IMAGE + 1,
        ),
    ];
    let images = super::image_blocks(&files).await;
    assert_eq!(images.len(), 1);

    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let input = SessionInput {
        session_id: sid,
        event_id: Uuid::now_v7(),
        text: text.clone(),
        artifact_ids: vec![Uuid::nil()],
        peer_prefix: vec![],
        peer: None,
        permission_cap: None,
        schedule_id: None,
    };
    let a = core.input_with_images(&input, images);
    assert!(
        matches!(&a[..], [Action::Write(v)] if without_uuids(vec![v.clone()]) == [want.clone()]),
        "{a:?}"
    );
    // Without images the content stays a string.
    let a = core.input(&input);
    assert!(
        matches!(&a[..], [Action::Write(v)] if v["message"]["content"] == json!(text)),
        "{a:?}"
    );
}

#[test]
fn held_text_is_not_final_before_a_tool_call() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let input = Uuid::now_v7();
    core.input(&SessionInput {
        session_id: sid,
        event_id: input,
        text: "x".into(),
        artifact_ids: vec![],
        peer_prefix: vec![],
        peer: None,
        permission_cap: None,
        schedule_id: None,
    });
    core.on_line(&echo(&Uuid::new_v4().to_string(), "x"));
    let u1 = Uuid::new_v4();
    let u2 = Uuid::new_v4();
    let text = json!({"type":"assistant","uuid":u1,"parent_tool_use_id":null,
        "message":{"content":[{"type":"text","text":"Let me look."}]}});
    let tool = json!({"type":"assistant","uuid":u2,"parent_tool_use_id":null,
        "message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/a"}}]}});
    assert!(core.on_line(&text.to_string()).is_empty());
    let a = core.on_line(&tool.to_string());
    let kinds: Vec<(Uuid, EventKind)> = a
        .into_iter()
        .map(|a| match a {
            Action::Emit(AdapterEvent::Event(e)) => (e.event_id, e.kind),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        kinds[0],
        (
            Event::id_from_claude_record(&u1),
            EventKind::AgentMessage {
                text: "Let me look.".into(),
                r#final: false,
                in_reply_to: Some(input),
                runtime: false,
            }
        )
    );
    assert_eq!(kinds[1].0, Event::id_from_claude_record(&u2));
    // Subagent traffic is not mapped.
    let sub = json!({"type":"assistant","uuid":Uuid::new_v4(),"parent_tool_use_id":"t1",
        "message":{"content":[{"type":"text","text":"inner"}]}});
    assert!(core.on_line(&sub.to_string()).is_empty());
}

/// T10: U+0000 in anything the CLI prints (binary tool output) becomes U+FFFD in events:
/// the hub's Postgres refuses it (SPEC §5.2).
#[test]
fn nul_never_reaches_an_event() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    core.input(&SessionInput {
        session_id: sid,
        event_id: Uuid::now_v7(),
        text: "x".into(),
        artifact_ids: vec![],
        peer_prefix: vec![],
        peer: None,
        permission_cap: None,
        schedule_id: None,
    });
    let lines = [
        json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta",
            "delta":{"type":"text_delta","text":"d\u{0}"}}}),
        json!({"type":"assistant","uuid":Uuid::new_v4(),"parent_tool_use_id":null,
            "message":{"content":[{"type":"text","text":"m\u{0}"}]}}),
        json!({"type":"assistant","uuid":Uuid::new_v4(),"parent_tool_use_id":null,
            "message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cat a.bin\u{0}"}}]}}),
        json!({"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool",
            "tool_name":"Bash","input":{"command":"cat a.bin\u{0}"},"description":"Read\u{0}"}}),
        json!({"type":"user","uuid":Uuid::new_v4(),"parent_tool_use_id":null,
            "message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,
                "content":"ELF\u{0}\u{0}\u{1}"}]}}),
        json!({"type":"result","subtype":"success","usage":{}}),
    ];
    let mut kinds = vec![];
    for l in lines {
        for a in core.on_line(&l.to_string()) {
            if let Action::Emit(AdapterEvent::Event(e)) = a {
                let v = serde_json::to_value(&e).unwrap();
                let mut clean = v.clone();
                hqc_proto::replace_nul_json(&mut clean);
                assert_eq!(clean, v);
                kinds.push(e.kind);
            }
        }
    }
    assert!(kinds.contains(&EventKind::AgentDelta {
        text: "d\u{FFFD}".into()
    }));
    let mut seen = 0;
    for k in &kinds {
        match k {
            EventKind::AgentMessage { text, .. } => assert_eq!(text, "m\u{FFFD}"),
            EventKind::ApprovalRequest { description, .. } => {
                assert_eq!(description, "Read\u{FFFD}\ncat a.bin\u{FFFD}")
            }
            EventKind::ToolResult {
                result_preview,
                error_text,
                ..
            } => {
                assert_eq!(result_preview, "ELF\u{FFFD}\u{FFFD}\u{1}");
                assert_eq!(error_text.as_deref(), Some(result_preview.as_str()));
            }
            _ => continue,
        }
        seen += 1;
    }
    assert_eq!(seen, 3, "{kinds:?}");
}

/// `allow_native_messaging` in the worker config keeps Claude Code's ListAgents/SendMessage.
#[tokio::test]
async fn native_messaging_tools_can_be_kept() {
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let claude = ScriptedClaude::new("");
    let mut opts = options(config.path());
    opts.always_disallowed.clear();
    let factory = ClaudeCodeAdapters::new(opts, Arc::new(claude.clone()));
    let s = spec(Uuid::now_v7(), work.path(), Uuid::new_v4());
    let (tx, _rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    let args = claude.commands()[0].args.clone();
    assert!(!args.contains(&"--disallowedTools".to_string()), "{args:?}");
    a.kill().await.unwrap();
}

/// T15 (SPEC §9.3), fixture `vscode-takeover` (2.1.308): the takeover turn registers the
/// process as live and every record it produced, the prompt's echo included, as its own;
/// the records `claude -p` wrote before are not. After `kill` the process is not live.
#[tokio::test]
async fn takeover_records_are_registered_as_own() {
    const TRANSCRIPT: &str =
        include_str!("../../../../hqc-claude-jsonl/fixtures/vscode-takeover-2.1.308.jsonl");
    const STREAM: &str =
        include_str!("../../../../hqc-claude-jsonl/fixtures/stream-vscode-takeover-2.1.308.jsonl");
    let runtime: Uuid = "8dd26c03-4e07-4685-b0d2-b99924e96975".parse().unwrap();
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let claude = ScriptedClaude::new(STREAM);
    let opts = options(config.path());
    let own = opts.own_records.clone();
    let factory = ClaudeCodeAdapters::new(opts, Arc::new(claude.clone()));
    let s = spec(Uuid::now_v7(), work.path(), runtime);
    let (tx, mut events) = mpsc::channel(1024);
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    assert!(own.is_live(runtime));
    a.input(&SessionInput {
        session_id: s.session_id,
        event_id: Uuid::now_v7(),
        text: "Reply with exactly: HQC-TURN".into(),
        artifact_ids: vec![],
        peer_prefix: vec![],
        peer: None,
        permission_cap: None,
        schedule_id: None,
    })
    .await
    .unwrap();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        if matches!(ev, AdapterEvent::TurnEnded { .. }) {
            break;
        }
    }
    let (records, _) = hqc_claude_jsonl::parse_str(TRANSCRIPT);
    let main: Vec<Uuid> = records
        .iter()
        .filter_map(|r| match r {
            hqc_claude_jsonl::Record::User(m) | hqc_claude_jsonl::Record::Assistant(m)
                if m.is_main() =>
            {
                Some(m.uuid)
            }
            _ => None,
        })
        .collect();
    let owned: Vec<bool> = main.iter().map(|u| own.is_own(runtime, *u)).collect();
    // prompt, tool_use, tool_result, thinking, text of `claude -p`; then the adapter's.
    assert_eq!(owned, [false, false, false, false, false, true, true]);
    // T25: the input's text too, for a prompt record the stream does not name.
    let unnamed = Uuid::new_v4();
    assert!(own.claim(runtime, unnamed, Some("Reply with exactly: HQC-TURN")));
    a.kill().await.unwrap();
    assert!(!own.is_live(runtime));
}

/// `show_in_vscode` (the default from the worker config): the child gets
/// `CLAUDE_CODE_ENTRYPOINT=claude-vscode`, whatever the worker's own environment says;
/// without it the variable is removed and the CLI writes its own `sdk-cli`.
#[tokio::test]
async fn entrypoint_for_the_vscode_history() {
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    for (entrypoint, want) in [(Some("claude-vscode"), Some("claude-vscode")), (None, None)] {
        let claude = ScriptedClaude::new("");
        let mut opts = options(config.path());
        opts.entrypoint = entrypoint.map(str::to_string);
        let factory = ClaudeCodeAdapters::new(opts, Arc::new(claude.clone()));
        let s = spec(Uuid::now_v7(), work.path(), Uuid::new_v4());
        let (tx, _rx) = mpsc::channel(16);
        let mut a = factory.create(&s, tx);
        a.start(&s).await.unwrap();
        let env = claude.commands()[0].env.clone();
        let got = env
            .iter()
            .find(|(k, _)| k == "CLAUDE_CODE_ENTRYPOINT")
            .map(|(_, v)| v.as_str());
        assert_eq!(got, want);
        a.kill().await.unwrap();
    }
}

/// A local slash command (`/name x`): the CLI answers with a `result` and no replay echo.
/// The input still counts as taken, otherwise the supervisor resends it forever.
#[tokio::test]
async fn local_command_without_echo_is_taken() {
    let stream = r#"{"type":"result","subtype":"success","is_error":false,"num_turns":0,"duration_ms":15,"result":"Session renamed to: x","total_cost_usd":0,"usage":{"input_tokens":0,"output_tokens":0}}"#;
    let mut h = Harness::start(stream, Uuid::new_v4()).await;
    let id = Uuid::now_v7();
    let events = h.turn("/name x", id, "once", BTreeMap::new()).await;
    let s = shape(&events);
    let accepted = s
        .iter()
        .position(|e| e == "accepted")
        .expect("input not taken");
    let ended = s.iter().position(|e| e == "turn_ended").unwrap();
    assert!(accepted < ended, "{s:?}");
    assert!(events
        .iter()
        .any(|e| matches!(e, AdapterEvent::InputAccepted { event_id } if *event_id == id)));
}

/// A `result` with no model turn and no tokens (a local slash command, the empty second
/// `result` after a turn the CLI starts by itself, #6) ends the turn without a `usage`: the
/// feed would show an empty `0 → 0 tokens` line. One that spent tokens keeps its `usage`.
#[test]
fn a_result_with_no_model_turn_and_no_tokens_has_no_usage() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    for (tokens, want) in [(0, 0), (12, 1)] {
        started(&mut core, sid);
        let evs = emitted(core.on_line(&format!(
            r#"{{"type":"result","subtype":"success","is_error":false,"num_turns":0,"duration_ms":15,"result":"Session renamed to: x","total_cost_usd":0,"usage":{{"input_tokens":{tokens},"output_tokens":0}}}}"#
        )));
        let s = shape(&evs);
        assert!(s.iter().any(|e| e == "turn_ended"), "{s:?}");
        assert_eq!(s.iter().filter(|e| *e == "usage").count(), want, "{s:?}");
    }
}

/// `/compact` (fixture `stream-resume-compact`, 2.1.308): `num_turns: 0` and no tokens, but the
/// session's total grew from the transcript's last `cost-state` (0.0032966), so it keeps its
/// `usage` with what the compaction cost, and the next turn's cost does not include it.
#[test]
fn compact_keeps_its_usage() {
    let fx = fixture!("resume-compact", "2.1.308");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false).with_cost_baseline(Some(0.0032966));
    started(&mut core, sid);
    let (costs, _) = costs_and_commands(&play(&mut core, fx.stream));
    assert_eq!(costs.len(), 1, "{costs:?}");
    assert!((costs[0].0 - 0.00138282).abs() < 1e-9, "{costs:?}");
    assert_eq!(costs[0].1, 0.00467942);
}

/// A subagent that ends asks nothing any more: its request still open when its
/// `task_notification` comes is cancelled there, even before the main turn's `result`.
#[test]
fn a_subagents_request_is_cancelled_when_it_ends() {
    let fx = fixture!("permission-allow");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    play(
        &mut core,
        r#"{"type":"system","subtype":"task_started","task_id":"ac5672f5fc80df8ec","tool_use_id":"toolu_01TYRCeySHmvXd7Md3Y1rgb8","description":"Run ping and reply with 4","subagent_type":"general-purpose","is_backgrounded":false,"spawn_depth":1,"task_type":"local_agent","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e0d"}
{"type":"system","subtype":"task_started","task_id":"a8b8fdc3b4a031d4f","tool_use_id":"toolu_01BR8iav6ZtPUAhpuCWdQPCt","description":"Touch hqc-live-check file","is_backgrounded":true,"task_type":"local_agent","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e13"}"#,
    );
    for (request_id, agent) in [("fg", "ac5672f5fc80df8ec"), ("bg", "a8b8fdc3b4a031d4f")] {
        let mut v: Value =
            serde_json::from_str(&can_use_tool_line(&fx, request_id, Some("touch x"))).unwrap();
        v["request"]["agent_id"] = json!(agent);
        assert_eq!(approvals(&core.on_line(&v.to_string())), 1);
    }
    for (request_id, agent) in [("fg", "ac5672f5fc80df8ec"), ("bg", "a8b8fdc3b4a031d4f")] {
        let a = core.on_line(&format!(
            r#"{{"type":"system","subtype":"task_notification","task_id":"{agent}","status":"stopped","summary":"Stopped","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e14"}}"#
        ));
        let cancels: Vec<(String, CancelReason)> = events(&emitted(a))
            .into_iter()
            .filter_map(|e| match &e.kind {
                EventKind::ApprovalCancel { request_id, reason } => {
                    Some((request_id.clone(), *reason))
                }
                _ => None,
            })
            .collect();
        assert_eq!(cancels, [(request_id.to_string(), CancelReason::Stopped)]);
        assert!(core.answer_approval(request_id, "once", None).is_err());
    }
}

/// `(cost_usd, session_cost_usd)` of every `usage` and every command list, in order.
fn costs_and_commands(actions: &[Action]) -> (Vec<(f64, f64)>, Vec<Vec<String>>) {
    let (mut costs, mut commands) = (vec![], vec![]);
    for a in actions {
        match a {
            Action::Emit(AdapterEvent::Event(e)) => {
                if let EventKind::Usage {
                    cost_usd: Some(c),
                    session_cost_usd: Some(t),
                    ..
                } = e.kind
                {
                    costs.push((c, t));
                }
            }
            Action::Emit(AdapterEvent::Commands(c)) => commands.push(c.clone()),
            _ => {}
        }
    }
    (costs, commands)
}

fn play(core: &mut Core, stream: &str) -> Vec<Action> {
    stream.lines().flat_map(|l| core.on_line(l)).collect()
}

/// The transcript as it was when the first process ended: up to its `cost-state`.
fn before_resume(transcript: &str) -> &str {
    let at = transcript.find("\"cost-state\"").unwrap();
    &transcript[..at + transcript[at..].find('\n').unwrap() + 1]
}

/// T27 (SPEC §4, §8.1): `result.total_cost_usd` is the session's running total (checked
/// live on 2.1.308): the second turn of a process reports 0.00398629, of which 0.0018188
/// is its own. `system/init` comes before every turn; the command list goes out once,
/// without the terminal-only and internal commands.
#[test]
fn turn_cost_is_the_growth_of_the_session_total() {
    // The set has no transcript of its own: it is `cost-resume`'s first process.
    let stream =
        include_str!("../../../../hqc-claude-jsonl/fixtures/stream-cost-turns-2.1.308.jsonl");
    let mut core = Core::new(Uuid::now_v7(), false);
    let (costs, commands) = costs_and_commands(&play(&mut core, stream));
    assert_eq!(costs, [(0.00216749, 0.00216749), (0.0018188, 0.00398629)]);
    assert_eq!(commands.len(), 1, "init twice, one report");
    let list = &commands[0];
    for c in ["compact", "context", "code-review", "anthropic-skills:pdf"] {
        assert!(list.contains(&c.to_string()), "{c} in {list:?}");
    }
    for c in [
        "doctor",
        "color",
        "focus",
        "reload-plugins",
        "__remote-workflow",
    ] {
        assert!(!list.contains(&c.to_string()), "{c} in {list:?}");
    }
}

/// After `--resume` the CLI starts from the transcript's last `cost-state`: the resumed
/// turn costs 0.0043361 - 0.00398629. Without the baseline the whole total would be counted
/// again (the "$44.401" of every line in T27's report).
#[test]
fn resumed_turn_cost_starts_from_the_last_cost_state() {
    let fx = fixture!("cost-resume", "2.1.308");
    let baseline = hqc_claude_jsonl::last_cost_total(before_resume(fx.transcript));
    assert_eq!(baseline, Some(0.00398629));
    let mut core = Core::new(Uuid::now_v7(), false).with_cost_baseline(baseline);
    let (costs, _) = costs_and_commands(&play(&mut core, fx.stream));
    assert_eq!(costs, [(0.00034981, 0.0043361)]);

    // A total below the baseline (a CLI that did not carry the cost over) is not negative,
    // and the next turn is counted from the higher of the two.
    let mut core = Core::new(Uuid::now_v7(), false).with_cost_baseline(Some(1.0));
    let (costs, _) = costs_and_commands(&play(&mut core, fx.stream));
    assert_eq!(costs, [(0.0, 0.0043361)]);
    let result = fx
        .stream
        .lines()
        .find(|l| l.contains("\"total_cost_usd\""))
        .unwrap();
    let mut v: Value = serde_json::from_str(result).unwrap();
    v["total_cost_usd"] = json!(1.25);
    let (costs, _) = costs_and_commands(&core.on_line(&v.to_string()));
    assert_eq!(costs, [(0.25, 1.25)]);
}

/// The adapter takes the baseline from the transcript on disk when it resumes.
#[tokio::test]
async fn adapter_resumes_with_the_transcript_cost() {
    let fx = fixture!("cost-resume", "2.1.308");
    let mut h = Harness::start(fx.stream, fx.claude_session()).await;
    h.adapter.kill().await.unwrap();
    let spec = spec(h.session_id, h._dirs.0.path(), fx.claude_session());
    let t = super::paths::transcript_path(h._dirs.1.path(), &spec.workdir, &fx.claude_session());
    std::fs::create_dir_all(t.parent().unwrap()).unwrap();
    std::fs::write(&t, before_resume(fx.transcript)).unwrap();
    h.adapter.start(&spec).await.unwrap();
    assert!(h.claude.commands()[1]
        .args
        .contains(&"--resume".to_string()));
    let evs = h
        .turn(&fx.prompt(), Uuid::now_v7(), "once", BTreeMap::new())
        .await;
    let usage: Vec<_> = events(&evs)
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::Usage {
                cost_usd,
                session_cost_usd,
                ..
            } => Some((cost_usd, session_cost_usd)),
            _ => None,
        })
        .collect();
    assert_eq!(usage, [(Some(0.00034981), Some(0.0043361))]);
    assert_eq!(shape(&evs)[..2], ["commands", "accepted"]);
}

/// The list is reported again only when it changes (`/reload-plugins`, a new skill); an
/// `init` without `slash_commands` (an older CLI) reports nothing.
#[test]
fn command_list_is_reported_when_it_changes() {
    let fx = fixture!("cost-resume", "2.1.308");
    let init = fx.stream.lines().find(|l| l.contains("\"init\"")).unwrap();
    let mut core = Core::new(Uuid::now_v7(), false);
    assert_eq!(costs_and_commands(&core.on_line(init)).1.len(), 1);
    assert!(
        core.on_line(init).is_empty(),
        "same list: not reported again"
    );
    let mut v: Value = serde_json::from_str(init).unwrap();
    v["slash_commands"]
        .as_array_mut()
        .unwrap()
        .push(json!("/new-skill"));
    let (_, commands) = costs_and_commands(&core.on_line(&v.to_string()));
    assert_eq!(commands[0].last().map(String::as_str), Some("new-skill"));
    v.as_object_mut().unwrap().remove("slash_commands");
    assert!(core.on_line(&v.to_string()).is_empty(), "no list: nothing");
}

fn emitted(actions: Vec<Action>) -> Vec<AdapterEvent> {
    actions
        .into_iter()
        .filter_map(|a| match a {
            Action::Emit(e) => Some(e),
            Action::Write(_) => None,
        })
        .collect()
}

/// An input written and echoed: the CLI runs its turn.
fn started(core: &mut Core, sid: Uuid) -> Uuid {
    let input = written(core, sid, "x");
    core.on_line(&echo(&Uuid::new_v4().to_string(), "x"));
    input
}

/// An input written to the CLI, not echoed yet.
fn written(core: &mut Core, sid: Uuid, text: &str) -> Uuid {
    let input = Uuid::now_v7();
    core.input(&SessionInput {
        session_id: sid,
        event_id: input,
        text: text.into(),
        artifact_ids: vec![],
        peer_prefix: vec![],
        peer: None,
        permission_cap: None,
        schedule_id: None,
    });
    input
}

/// A background task's end (lines of a live run on 2.1.312: `run_in_background` Bash, then
/// the turn the CLI starts by itself) is `system task_notification` the moment its line comes,
/// before that turn's answer, with its summary.
#[test]
fn background_task_notification_is_a_system_event() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let stream = r#"{"type":"assistant","message":{"model":"claude-haiku-5-5","role":"assistant","content":[{"type":"text","text":"WAITING"}]},"parent_tool_use_id":null,"uuid":"6f1d0c3e-3c2f-4a55-9d0e-1a2b3c4d5e6f"}
{"type":"result","subtype":"success","is_error":false,"total_cost_usd":0.0073724,"usage":{"input_tokens":4,"output_tokens":283}}
{"type":"system","subtype":"task_updated","task_id":"b1b20u6dl","patch":{"status":"completed"},"uuid":"c5680b81-f430-4931-af98-c609a02f6fbf"}
{"type":"system","subtype":"task_notification","task_id":"b1b20u6dl","tool_use_id":"toolu_01DNPTCZTgycgC11D8VMjfaD","status":"completed","output_file":"/tmp/tasks/b1b20u6dl.output","summary":"Background command \"Run background sleep and echo marker\" completed (exit code 0)","uuid":"e6a01eed-f829-40d1-a1f6-95882055c9f1"}
{"type":"assistant","message":{"model":"claude-haiku-5-5","role":"assistant","content":[{"type":"text","text":"PONG"}]},"parent_tool_use_id":null,"uuid":"7a2e1d4f-4d3a-4b66-8e1f-2b3c4d5e6f70"}
{"type":"result","subtype":"success","is_error":false,"total_cost_usd":0.00780105,"usage":{"input_tokens":4,"output_tokens":5}}"#;
    let evs = emitted(play(&mut core, stream));
    assert_eq!(
        shape(&evs),
        [
            "agent.message:true",
            "usage",
            "status",
            "turn_ended",
            "system",
            "agent.message:true",
            "usage",
            "status",
            "turn_ended"
        ]
    );
    let sys = system_events(&evs.into_iter().map(Action::Emit).collect::<Vec<_>>());
    assert_eq!(
        sys,
        [(
            "task_notification".into(),
            "Background command \"Run background sleep and echo marker\" completed (exit code 0)"
                .into()
        )]
    );
}

/// The CLI sends `task_started` and `task_notification` for a foreground Bash call that runs
/// for more than about 2 s and for the tasks of a subagent too, its background Bash call
/// included (lines of live runs on 2.1.312): only the ends of the background subagent, of
/// the foreground Bash call the CLI moved to the background (`background_tasks` control
/// request) and of the foreground subagent moved with it are a `system task_notification`;
/// that subagent's Bash call, moved too, stays quiet.
#[test]
fn only_a_background_task_is_a_notification() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let stream = r#"{"type":"system","subtype":"task_started","task_id":"bfis28nrx","tool_use_id":"toolu_01FoCFY4DmUbVxXAembc44PX","description":"Sleep for 15 seconds","is_backgrounded":false,"task_type":"local_bash","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e01"}
{"type":"system","subtype":"task_notification","task_id":"bfis28nrx","tool_use_id":"toolu_01FoCFY4DmUbVxXAembc44PX","status":"completed","output_file":"","summary":"Sleep for 15 seconds","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e02"}
{"type":"system","subtype":"task_started","task_id":"a52af2b436ded4cea","tool_use_id":"toolu_01B2anRdzK42h65tA2gdwhb2","description":"Sleep and reply with number","subagent_type":"general-purpose","is_backgrounded":true,"spawn_depth":1,"task_type":"local_agent","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e03"}
{"type":"system","subtype":"task_started","task_id":"b6yusn86f","owned_by_subagent":true,"tool_use_id":"toolu_01T7e2mWErkbKbbTuuN4vYrm","description":"Sleep for 6 seconds","is_backgrounded":false,"parent_task_id":"a52af2b436ded4cea","task_type":"local_bash","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e04"}
{"type":"system","subtype":"task_notification","task_id":"b6yusn86f","tool_use_id":"toolu_01T7e2mWErkbKbbTuuN4vYrm","status":"completed","output_file":"","summary":"Sleep for 6 seconds","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e05"}
{"type":"system","subtype":"task_started","task_id":"b77j6ge9s","owned_by_subagent":true,"tool_use_id":"toolu_01UAxvZqF9hbY2GJ1GSt7Pch","description":"Run sleep 4 in the background","is_backgrounded":true,"parent_task_id":"a52af2b436ded4cea","task_type":"local_bash","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e07"}
{"type":"system","subtype":"task_notification","task_id":"b77j6ge9s","tool_use_id":"toolu_01UAxvZqF9hbY2GJ1GSt7Pch","status":"completed","output_file":"/tmp/tasks/b77j6ge9s.output","summary":"Background command \"Run sleep 4 in the background\" completed (exit code 0)","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e08"}
{"type":"system","subtype":"task_notification","task_id":"a52af2b436ded4cea","tool_use_id":"toolu_01B2anRdzK42h65tA2gdwhb2","status":"completed","output_file":"/tmp/tasks/a52af2b436ded4cea.output","summary":"4","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e06"}
{"type":"system","subtype":"task_started","task_id":"bnx80r4bg","tool_use_id":"toolu_01LkxBZReUQu4HTFtoWqC3Nb","description":"Sleep for 20 seconds","is_backgrounded":false,"task_type":"local_bash","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e09"}
{"type":"system","subtype":"task_updated","task_id":"bnx80r4bg","patch":{"is_backgrounded":true},"uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e0a"}
{"type":"system","subtype":"task_updated","task_id":"bnx80r4bg","patch":{"status":"completed","end_time":1793889489540},"uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e0b"}
{"type":"system","subtype":"task_notification","task_id":"bnx80r4bg","tool_use_id":"toolu_01LkxBZReUQu4HTFtoWqC3Nb","status":"completed","output_file":"/tmp/tasks/bnx80r4bg.output","summary":"Background command \"Sleep for 20 seconds\" completed (exit code 0)","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e0c"}
{"type":"system","subtype":"task_started","task_id":"ac5672f5fc80df8ec","tool_use_id":"toolu_01TYRCeySHmvXd7Md3Y1rgb8","description":"Run ping and reply with 4","subagent_type":"general-purpose","is_backgrounded":false,"spawn_depth":1,"task_type":"local_agent","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e0d"}
{"type":"system","subtype":"task_started","task_id":"bdcvcjogg","owned_by_subagent":true,"tool_use_id":"toolu_01S54MBuJffoXWdPz6MLU7aG","description":"Ping localhost 25 times in foreground","is_backgrounded":false,"parent_task_id":"ac5672f5fc80df8ec","task_type":"local_bash","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e0e"}
{"type":"system","subtype":"task_updated","task_id":"bdcvcjogg","patch":{"is_backgrounded":true},"uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e0f"}
{"type":"system","subtype":"task_updated","task_id":"ac5672f5fc80df8ec","patch":{"is_backgrounded":true},"uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e10"}
{"type":"system","subtype":"task_notification","task_id":"bdcvcjogg","tool_use_id":"toolu_01S54MBuJffoXWdPz6MLU7aG","status":"completed","output_file":"/tmp/tasks/bdcvcjogg.output","summary":"Background command \"Ping localhost 25 times in foreground\" completed (exit code 0)","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e11"}
{"type":"system","subtype":"task_notification","task_id":"ac5672f5fc80df8ec","tool_use_id":"toolu_01TYRCeySHmvXd7Md3Y1rgb8","status":"completed","output_file":"/tmp/tasks/ac5672f5fc80df8ec.output","summary":"4","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e12"}"#;
    let a = play(&mut core, stream);
    assert_eq!(
        system_events(&a),
        [
            ("task_notification".into(), "4".into()),
            (
                "task_notification".into(),
                "Background command \"Sleep for 20 seconds\" completed (exit code 0)".into()
            ),
            ("task_notification".into(), "4".into())
        ]
    );
}

/// `/compact` (lines of fixture `stream-resume-compact`, 2.1.308; the same `compact_boundary`
/// came on 2.1.312): `system context_compacted`, its id derived from the line's uuid, which is
/// the transcript record's.
#[test]
fn compact_boundary_is_a_system_event() {
    let fx = fixture!("resume-compact", "2.1.308");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let a = play(&mut core, fx.stream);
    assert_eq!(
        system_events(&a),
        [("context_compacted".into(), "Context compacted".into())]
    );
    let id = a
        .iter()
        .find_map(|a| match a {
            Action::Emit(AdapterEvent::Event(e)) if matches!(e.kind, EventKind::System { .. }) => {
                Some(e.event_id)
            }
            _ => None,
        })
        .unwrap();
    let boundary: Uuid = "31c3d952-eaeb-4d10-9930-8b35ca152b4b".parse().unwrap();
    assert_eq!(id, Event::id_from_claude_record(&boundary));
    assert!(fx
        .transcript
        .contains("31c3d952-eaeb-4d10-9930-8b35ca152b4b"));
}

/// The CLI's own reply (`model` `<synthetic>`; the lines of a live run on 2.1.312 with a model
/// the API does not know, a model text put before them) is `agent.message` with `runtime: true`
/// and the record's id, held like any text: the text before it is not final and not runtime.
#[test]
fn synthetic_reply_is_a_runtime_agent_message() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let input = started(&mut core, sid);
    let u: Uuid = "c294c442-b0d3-4724-94a4-17d06e405b0a".parse().unwrap();
    let text = "There's an issue with the selected model (claude-bogus-model-9-9). It may not \
                exist or you may not have access to it.";
    let stream = format!(
        r#"{{"type":"assistant","message":{{"model":"claude-haiku-5-5","content":[{{"type":"text","text":"Partial answer"}}]}},"parent_tool_use_id":null,"uuid":"{}"}}
{{"type":"assistant","message":{{"id":"75495e5d-148c-41a6-8b21-25d0efd759a8","model":"<synthetic>","role":"assistant","stop_reason":"stop_sequence","type":"message","content":[{{"type":"text","text":"{text}"}}]}},"parent_tool_use_id":null,"uuid":"{u}","error":"model_not_found","is_api_error_message":true,"api_error_status":404}}
{{"type":"result","subtype":"success","is_error":true,"result":"{text}","total_cost_usd":0,"terminal_reason":"api_error","usage":{{"input_tokens":0,"output_tokens":0}}}}"#,
        Uuid::new_v4()
    );
    let a = play(&mut core, &stream);
    let evs = emitted(a.clone());
    assert_eq!(
        shape(&evs),
        [
            "agent.message:false",
            "agent.message:true",
            "usage",
            "status",
            "turn_ended"
        ]
    );
    assert!(system_events(&a).is_empty());
    let messages: Vec<(Uuid, &EventKind)> = events(&evs)
        .into_iter()
        .filter(|e| matches!(e.kind, EventKind::AgentMessage { .. }))
        .map(|e| (e.event_id, &e.kind))
        .collect();
    assert!(matches!(
        messages[0].1,
        EventKind::AgentMessage { text, r#final: false, runtime: false, .. } if text == "Partial answer"
    ));
    assert_eq!(
        messages[1],
        (
            Event::id_from_claude_record(&u),
            &EventKind::AgentMessage {
                text: text.into(),
                r#final: true,
                in_reply_to: Some(input),
                runtime: true,
            }
        )
    );
}

/// A turn the CLI's own reply ends ("Credit balance is too low", an API error) ends with a
/// final `agent.message`, `runtime: true` on the wire: the hub's push, the answer to an
/// `hqc_ask` and the title wait for that message (SPEC §4, §7.1). The lines of a live run on
/// 2.1.312 with a model the API does not know.
#[test]
fn turn_ended_by_a_synthetic_reply_has_a_final_runtime_message() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let input = started(&mut core, sid);
    let text = "There's an issue with the selected model (claude-bogus-model-9-9). It may not \
                exist or you may not have access to it.";
    let stream = format!(
        r#"{{"type":"assistant","message":{{"id":"75495e5d-148c-41a6-8b21-25d0efd759a8","model":"<synthetic>","role":"assistant","stop_reason":"stop_sequence","type":"message","content":[{{"type":"text","text":"{text}"}}]}},"parent_tool_use_id":null,"uuid":"c294c442-b0d3-4724-94a4-17d06e405b0a","error":"model_not_found","is_api_error_message":true,"api_error_status":404}}
{{"type":"result","subtype":"success","is_error":true,"result":"{text}","total_cost_usd":0,"terminal_reason":"api_error","usage":{{"input_tokens":0,"output_tokens":0}}}}"#
    );
    let evs = emitted(play(&mut core, &stream));
    let wire: Vec<Value> = events(&evs)
        .into_iter()
        .map(|e| serde_json::to_value(&e.kind).unwrap())
        .collect();
    let last_message = wire
        .iter()
        .rev()
        .find(|k| k["type"] == "agent.message")
        .unwrap_or_else(|| panic!("no agent.message in {wire:?}"));
    assert_eq!(
        last_message["payload"],
        json!({"text": text, "final": true, "in_reply_to": input, "runtime": true})
    );
}

// ---- SPEC §17.2: a message during a turn, and the subagents running ------------------------
// The lines are those of live runs on 2.1.312 (haiku, `--replay-user-messages`, 2026-11-05),
// with the message uuids the engine gave.

const CLI_SESSION: &str = "42e1f456-d428-4380-ad8a-473e21ef9ff7";

fn user_input(sid: Uuid, text: &str) -> SessionInput {
    SessionInput {
        session_id: sid,
        event_id: Uuid::now_v7(),
        text: text.into(),
        artifact_ids: vec![],
        peer_prefix: vec![],
        peer: None,
        permission_cap: None,
        schedule_id: None,
    }
}

/// The `uuid` of the one user message written.
fn line_uuid(actions: &[Action]) -> String {
    let w = writes(actions);
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0]["type"], "user");
    w[0]["uuid"].as_str().expect("no uuid").to_owned()
}

fn lifecycle(uuid: &str, state: &str) -> String {
    json!({"type": "command_lifecycle", "command_uuid": uuid, "state": state,
        "uuid": Uuid::new_v4(), "session_id": CLI_SESSION})
    .to_string()
}

fn echo(uuid: &str, text: &str) -> String {
    json!({"type": "user", "message": {"role": "user", "content": text},
        "session_id": CLI_SESSION, "parent_tool_use_id": null, "uuid": uuid,
        "timestamp": "2026-11-05T14:25:00.000Z", "isReplay": true})
    .to_string()
}

fn reply(text: &str) -> String {
    json!({"type": "assistant", "message": {"model": "claude-haiku-5-5", "role": "assistant",
        "content": [{"type": "text", "text": text}]},
        "parent_tool_use_id": null, "session_id": CLI_SESSION, "uuid": Uuid::new_v4()})
    .to_string()
}

fn bash(id: &str, command: &str) -> String {
    json!({"type": "assistant", "message": {"model": "claude-haiku-5-5", "role": "assistant",
        "content": [{"type": "tool_use", "id": id, "name": "Bash",
            "input": {"command": command, "description": "Sleep"}}]},
        "parent_tool_use_id": null, "session_id": CLI_SESSION, "uuid": Uuid::new_v4()})
    .to_string()
}

fn tool_result(id: &str) -> String {
    json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result",
        "tool_use_id": id, "content": "", "is_error": false}]},
        "parent_tool_use_id": null, "session_id": CLI_SESSION, "uuid": Uuid::new_v4()})
    .to_string()
}

fn result(subtype: &str) -> String {
    json!({"type": "result", "subtype": subtype, "is_error": subtype != "success",
        "num_turns": 2, "total_cost_usd": 0.001, "session_id": CLI_SESSION,
        "usage": {"input_tokens": 4, "output_tokens": 2}})
    .to_string()
}

fn lines_of(lines: &[String]) -> String {
    lines.join("\n")
}

/// `accepted X`, `message "text" to X`, `status to X`, `turn_ended [X, ...]` (carried; with
/// ` dropped [X, ...]` when any is), the inputs named as in `names`.
fn trace(actions: &[Action], names: &[(Uuid, &str)]) -> Vec<String> {
    let name = |u: &Uuid| {
        names
            .iter()
            .find(|(id, _)| id == u)
            .map_or("?", |(_, n)| *n)
            .to_owned()
    };
    let to = |u: &Option<Uuid>| u.as_ref().map_or("-".to_owned(), name);
    let list = |l: &[Uuid]| l.iter().map(name).collect::<Vec<_>>().join(", ");
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Emit(AdapterEvent::InputAccepted { event_id }) => {
                Some(format!("accepted {}", name(event_id)))
            }
            Action::Emit(AdapterEvent::TurnEnded { carried, dropped }) if dropped.is_empty() => {
                Some(format!("turn_ended [{}]", list(carried)))
            }
            Action::Emit(AdapterEvent::TurnEnded { carried, dropped }) => Some(format!(
                "turn_ended [{}] dropped [{}]",
                list(carried),
                list(dropped)
            )),
            Action::Emit(AdapterEvent::Event(e)) => match &e.kind {
                EventKind::AgentMessage {
                    text, in_reply_to, ..
                } => Some(format!("message {text:?} to {}", to(in_reply_to))),
                EventKind::Status { in_reply_to, .. } => {
                    Some(format!("status to {}", to(in_reply_to)))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Each message written gets a fresh v4 `uuid`, not its `event_id`: a line with a uuid the
/// CLI has seen is dropped without a turn. The echo is matched by it, not by order.
#[test]
fn every_message_has_a_fresh_uuid_and_its_echo_is_matched_by_it() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let (a, b) = (user_input(sid, "first"), user_input(sid, "second"));
    let ua = line_uuid(&core.input(&a));
    play(
        &mut core,
        &lines_of(&[lifecycle(&ua, "queued"), lifecycle(&ua, "started")]),
    );
    let ub = line_uuid(&core.inject_with_images(&b, vec![]).expect("a turn runs"));
    for (u, input) in [(&ua, &a), (&ub, &b)] {
        let u: Uuid = u.parse().unwrap();
        assert_eq!(u.get_version_num(), 4);
        assert_ne!(u, input.event_id);
    }
    assert_ne!(ua, ub);
    let out = play(
        &mut core,
        &lines_of(&[
            lifecycle(&ub, "queued"),
            echo(&ub, "second"),
            echo(&ua, "first"),
        ]),
    );
    let names = [(a.event_id, "A"), (b.event_id, "B")];
    assert_eq!(trace(&out, &names), ["accepted B", "accepted A"]);
}

/// A message written while the turn runs a command (`sleep 8`, the message 5 s in): the CLI
/// takes it after the command's result, its echo then `started`, and answers both in the
/// same turn, one `result`. The answer replies to the turn's input.
#[test]
fn a_message_during_a_command_joins_the_turn() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let a = user_input(
        sid,
        "Run the bash command `sleep 8` in the foreground and then reply with the single word DONE.",
    );
    let b = user_input(sid, "Also add the word BANANA to your reply.");
    let ua = line_uuid(&core.input(&a));
    let call = "toolu_01NpAht5sbviUoPqP96JbyXm";
    let mut out = play(
        &mut core,
        &lines_of(&[
            lifecycle(&ua, "queued"),
            lifecycle(&ua, "started"),
            echo(&ua, &a.text),
            bash(call, "sleep 8"),
        ]),
    );
    let ub = line_uuid(&core.inject_with_images(&b, vec![]).expect("a turn runs"));
    out.extend(play(&mut core, &lines_of(&[
        lifecycle(&ub, "queued"),
        r#"{"type":"system","subtype":"task_started","task_id":"b5w66jgbe","run_id":"0mw3n0e3u-695e122c","tool_use_id":"toolu_01NpAht5sbviUoPqP96JbyXm","description":"Sleep for 8 seconds","is_backgrounded":false,"task_type":"local_bash","uuid":"0f6c1d2e-2222-4a55-9d0e-1a2b3c4d5e01","session_id":"42e1f456-d428-4380-ad8a-473e21ef9ff7"}"#.into(),
        r#"{"type":"system","subtype":"task_notification","task_id":"b5w66jgbe","run_id":"0mw3n0e3u-695e122c","tool_use_id":"toolu_01NpAht5sbviUoPqP96JbyXm","status":"completed","output_file":"","summary":"Sleep for 8 seconds","uuid":"0f6c1d2e-2222-4a55-9d0e-1a2b3c4d5e02","session_id":"42e1f456-d428-4380-ad8a-473e21ef9ff7"}"#.into(),
        tool_result(call),
        echo(&ub, &b.text),
        lifecycle(&ub, "started"),
        reply("DONE BANANA"),
        lifecycle(&ub, "completed"),
        result("success"),
        lifecycle(&ua, "completed"),
    ])));
    let names = [(a.event_id, "A"), (b.event_id, "B")];
    assert_eq!(
        trace(&out, &names),
        [
            "accepted A",
            "accepted B",
            "message \"DONE BANANA\" to A",
            "status to A",
            "turn_ended []"
        ]
    );
    assert!(!core.can_inject(), "no turn runs");
}

/// A message written during the final answer (a turn without tools) is not taken into it:
/// after the `result` the CLI runs it by itself, `started` first, then its echo, its answer
/// and its own `result`.
#[test]
fn a_message_during_the_final_answer_is_carried() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let a = user_input(
        sid,
        "Without using any tools, write a 700-word story about a lighthouse keeper.",
    );
    let b = user_input(sid, "Also: end your reply with the word BANANA.");
    let ua = line_uuid(&core.input(&a));
    let mut out = play(
        &mut core,
        &lines_of(&[lifecycle(&ua, "queued"), lifecycle(&ua, "started")]),
    );
    let ub = line_uuid(&core.inject_with_images(&b, vec![]).expect("a turn runs"));
    out.extend(play(
        &mut core,
        &lines_of(&[
            lifecycle(&ub, "queued"),
            echo(&ua, &a.text),
            reply("A Tuesday of Salt and Iron"),
            result("success"),
            lifecycle(&ua, "completed"),
            lifecycle(&ub, "started"),
            echo(&ub, &b.text),
            reply("Understood. BANANA"),
            result("success"),
            lifecycle(&ub, "completed"),
        ]),
    ));
    let names = [(a.event_id, "A"), (b.event_id, "B")];
    assert_eq!(
        trace(&out, &names),
        [
            "accepted A",
            "message \"A Tuesday of Salt and Iron\" to A",
            "status to A",
            "turn_ended [B]",
            "accepted B",
            "message \"Understood. BANANA\" to B",
            "status to B",
            "turn_ended []"
        ]
    );
}

/// A message written before `interrupt`: the interrupted turn ends with
/// `error_during_execution` (its input `cancelled` after the `result`), the CLI runs the
/// message next. That turn runs for the engine at once: it can be stopped and joined.
#[test]
fn a_message_before_an_interrupt_is_carried() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let a = user_input(
        sid,
        "Run the bash command `sleep 15` in the foreground and then reply with the single word DONE.",
    );
    let b = user_input(sid, "Also add the word BANANA to your reply.");
    let ua = line_uuid(&core.input(&a));
    let call = "toolu_01FoCFY4DmUbVxXAembc44PX";
    let mut out = play(
        &mut core,
        &lines_of(&[
            lifecycle(&ua, "queued"),
            lifecycle(&ua, "started"),
            echo(&ua, &a.text),
            bash(call, "sleep 15"),
        ]),
    );
    let ub = line_uuid(&core.inject_with_images(&b, vec![]).expect("a turn runs"));
    let (_, interrupted) = core.interrupt().expect("a turn runs");
    out.extend(play(&mut core, &lines_of(&[
        lifecycle(&ub, "queued"),
        r#"{"type":"control_response","response":{"subtype":"success","request_id":"hqc-interrupt-1"}}"#.into(),
        r#"{"type":"system","subtype":"task_notification","task_id":"bfis28nrx","run_id":"0mw3mw8rx-13c7c4ef","tool_use_id":"toolu_01FoCFY4DmUbVxXAembc44PX","status":"stopped","output_file":"","summary":"Sleep for 15 seconds","uuid":"0f6c1d2e-3333-4a55-9d0e-1a2b3c4d5e01","session_id":"42e1f456-d428-4380-ad8a-473e21ef9ff7"}"#.into(),
        tool_result(call),
        result("error_during_execution"),
    ])));
    assert!(!core.turn_running(interrupted));
    assert!(core.interrupt().is_some(), "the carried input's turn runs");
    assert!(core.can_inject());
    out.extend(play(
        &mut core,
        &lines_of(&[
            lifecycle(&ua, "cancelled"),
            lifecycle(&ub, "started"),
            echo(&ub, &b.text),
            reply("BANANA. The `sleep 15` command was declined"),
            result("success"),
            lifecycle(&ub, "completed"),
        ]),
    ));
    let names = [(a.event_id, "A"), (b.event_id, "B")];
    assert_eq!(
        trace(&out, &names),
        [
            "accepted A",
            "status to A",
            "turn_ended [B]",
            "accepted B",
            "message \"BANANA. The `sleep 15` command was declined\" to B",
            "status to B",
            "turn_ended []"
        ]
    );
}

/// The interrupt of `a_message_before_an_interrupt_is_carried` with two messages written: the
/// `result` carries both, then the CLI cancels them before either is `started` (another Stop).
/// No `result` comes for that turn: each `cancelled` ends it, the first with the other still
/// carried, the last with nothing left to run.
#[test]
fn carried_messages_cancelled_after_the_result_end_their_turn() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let a = user_input(
        sid,
        "Run the bash command `sleep 15` in the foreground and then reply with the single word DONE.",
    );
    let (b, d) = (
        user_input(sid, "Also add the word BANANA to your reply."),
        user_input(sid, "And the word CHERRY."),
    );
    let ua = line_uuid(&core.input(&a));
    let call = "toolu_01FoCFY4DmUbVxXAembc44PX";
    let mut out = play(
        &mut core,
        &lines_of(&[
            lifecycle(&ua, "queued"),
            lifecycle(&ua, "started"),
            echo(&ua, &a.text),
            bash(call, "sleep 15"),
        ]),
    );
    let ub = line_uuid(&core.inject_with_images(&b, vec![]).expect("a turn runs"));
    let ud = line_uuid(&core.inject_with_images(&d, vec![]).expect("a turn runs"));
    core.interrupt().expect("a turn runs");
    out.extend(play(&mut core, &lines_of(&[
        lifecycle(&ub, "queued"),
        lifecycle(&ud, "queued"),
        r#"{"type":"control_response","response":{"subtype":"success","request_id":"hqc-interrupt-1"}}"#.into(),
        r#"{"type":"system","subtype":"task_notification","task_id":"bfis28nrx","run_id":"0mw3mw8rx-13c7c4ef","tool_use_id":"toolu_01FoCFY4DmUbVxXAembc44PX","status":"stopped","output_file":"","summary":"Sleep for 15 seconds","uuid":"0f6c1d2e-3333-4a55-9d0e-1a2b3c4d5e01","session_id":"42e1f456-d428-4380-ad8a-473e21ef9ff7"}"#.into(),
        tool_result(call),
        result("error_during_execution"),
        lifecycle(&ua, "cancelled"),
    ])));
    let (_, carried_turn) = core.interrupt().expect("the carried inputs' turn runs");
    out.extend(play(&mut core, &lifecycle(&ub, "cancelled")));
    assert!(core.can_inject(), "D is still carried");
    assert!(!core.turn_running(carried_turn));
    out.extend(play(&mut core, &lifecycle(&ud, "cancelled")));
    assert!(!core.can_inject(), "no turn runs");
    assert!(core.interrupt().is_none(), "nothing to interrupt");
    let names = [(a.event_id, "A"), (b.event_id, "B"), (d.event_id, "D")];
    assert_eq!(
        trace(&out, &names),
        [
            "accepted A",
            "status to A",
            "turn_ended [B, D]",
            "turn_ended [D] dropped [B]",
            "turn_ended [] dropped [D]"
        ]
    );
}

/// A message carried past an interrupt that the CLI runs as a local command (`started` and
/// `completed`, no echo): its own `result` (`/name`, `/cost` on 2.1.312) takes it and ends its
/// turn, once; `completed` alone ends nothing.
#[test]
fn a_carried_local_command_is_taken_by_its_own_result() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let (a, b) = (user_input(sid, "first"), user_input(sid, "/name x"));
    let ua = line_uuid(&core.input(&a));
    play(
        &mut core,
        &lines_of(&[
            lifecycle(&ua, "queued"),
            lifecycle(&ua, "started"),
            echo(&ua, "first"),
            bash("toolu_1", "sleep 15"),
        ]),
    );
    let ub = line_uuid(&core.inject_with_images(&b, vec![]).expect("a turn runs"));
    core.interrupt().expect("a turn runs");
    let out = play(
        &mut core,
        &lines_of(&[
            lifecycle(&ub, "queued"),
            tool_result("toolu_1"),
            result("error_during_execution"),
            lifecycle(&ua, "cancelled"),
            lifecycle(&ub, "started"),
            lifecycle(&ub, "completed"),
            r#"{"type":"result","subtype":"success","is_error":false,"num_turns":0,"duration_ms":15,"result":"Session renamed to: x","total_cost_usd":0,"usage":{"input_tokens":0,"output_tokens":0}}"#.to_owned(),
        ]),
    );
    let names = [(a.event_id, "A"), (b.event_id, "B")];
    assert_eq!(
        trace(&out, &names),
        [
            "status to A",
            "turn_ended [B]",
            "status to -",
            "accepted B",
            "turn_ended []"
        ]
    );
    assert!(!core.can_inject(), "no turn runs");
}

/// A message the CLI cancelled before the turn's `result` is not taken: neither accepted nor
/// carried, but dropped (the supervisor queues it again).
#[test]
fn a_cancelled_message_is_not_taken() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let (a, b) = (user_input(sid, "first"), user_input(sid, "second"));
    let ua = line_uuid(&core.input(&a));
    play(
        &mut core,
        &lines_of(&[
            lifecycle(&ua, "queued"),
            lifecycle(&ua, "started"),
            echo(&ua, "first"),
            bash("toolu_1", "sleep 8"),
        ]),
    );
    let ub = line_uuid(&core.inject_with_images(&b, vec![]).expect("a turn runs"));
    let out = play(
        &mut core,
        &lines_of(&[
            lifecycle(&ub, "queued"),
            lifecycle(&ub, "cancelled"),
            tool_result("toolu_1"),
            reply("DONE"),
            result("success"),
        ]),
    );
    let names = [(a.event_id, "A"), (b.event_id, "B")];
    assert_eq!(
        trace(&out, &names),
        [
            "message \"DONE\" to A",
            "status to A",
            "turn_ended [] dropped [B]"
        ]
    );
}

/// A message the CLI took without an echo (`started`, no `isReplay` line: a local command)
/// is taken at the `result`, also one written during the turn.
#[test]
fn a_message_started_without_an_echo_is_taken() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let (a, b) = (user_input(sid, "first"), user_input(sid, "/name x"));
    let ua = line_uuid(&core.input(&a));
    play(
        &mut core,
        &lines_of(&[
            lifecycle(&ua, "queued"),
            lifecycle(&ua, "started"),
            echo(&ua, "first"),
            bash("toolu_1", "sleep 8"),
        ]),
    );
    let ub = line_uuid(&core.inject_with_images(&b, vec![]).expect("a turn runs"));
    let out = play(
        &mut core,
        &lines_of(&[
            lifecycle(&ub, "queued"),
            lifecycle(&ub, "started"),
            tool_result("toolu_1"),
            reply("DONE"),
            result("success"),
            lifecycle(&ub, "completed"),
        ]),
    );
    let names = [(a.event_id, "A"), (b.event_id, "B")];
    assert_eq!(
        trace(&out, &names),
        [
            "message \"DONE\" to A",
            "status to A",
            "accepted B",
            "turn_ended []"
        ]
    );
}

/// Without `command_lifecycle` (an older CLI; the T0 fixtures, 2.1.307): the moment of
/// writing decides. A message written during the turn and not echoed by its `result` is
/// carried; an echo that names no uuid of ours is the oldest input's.
#[test]
fn without_lifecycle_lines_a_message_written_during_the_turn_is_carried() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let (a, b) = (user_input(sid, "first"), user_input(sid, "second"));
    core.input(&a);
    let mut out = play(
        &mut core,
        &lines_of(&[
            echo("adbf4a08-b683-4a10-af11-6bd389a5a73e", "first"),
            bash("toolu_1", "sleep 8"),
        ]),
    );
    core.inject_with_images(&b, vec![]).expect("a turn runs");
    out.extend(play(
        &mut core,
        &lines_of(&[tool_result("toolu_1"), reply("DONE"), result("success")]),
    ));
    let names = [(a.event_id, "A"), (b.event_id, "B")];
    assert_eq!(
        trace(&out, &names),
        [
            "accepted A",
            "message \"DONE\" to A",
            "status to A",
            "turn_ended [B]"
        ]
    );
    let next = play(
        &mut core,
        &lines_of(&[
            echo("bdbf4a08-b683-4a10-af11-6bd389a5a73e", "second"),
            result("success"),
        ]),
    );
    let next = trace(&next, &names);
    assert_eq!(next.first().map(String::as_str), Some("accepted B"));
    assert_eq!(next.last().map(String::as_str), Some("turn_ended []"));
}

/// After a background subagent the CLI starts a turn by itself, without `command_lifecycle`.
/// A message written meanwhile (no turn runs for the supervisor) waits for that turn: the
/// turn's answer still replies to the earlier input, the message is carried, and its
/// `started` makes it the input of the next turn.
#[test]
fn a_message_during_a_turn_the_cli_began_waits_for_it() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let a = user_input(sid, "Use the Agent tool once with subagent_type general-purpose and run_in_background set to true");
    let b = user_input(sid, "Reply with the single word PONG.");
    let ua = line_uuid(&core.input(&a));
    let mut out = play(&mut core, &lines_of(&[
        lifecycle(&ua, "queued"),
        lifecycle(&ua, "started"),
        echo(&ua, &a.text),
        r#"{"type":"system","subtype":"task_started","task_id":"a52af2b436ded4cea","run_id":"0mw3mxs60-f1597f03","tool_use_id":"toolu_01B2anRdzK42h65tA2gdwhb2","description":"Sleep and reply with number","subagent_type":"general-purpose","is_backgrounded":true,"spawn_depth":1,"task_type":"local_agent","prompt":"Run the bash command sleep 6, then reply with the number 4.","uuid":"0f6c1d2e-4444-4a55-9d0e-1a2b3c4d5e01","session_id":"42e1f456-d428-4380-ad8a-473e21ef9ff7"}"#.into(),
        tool_result("toolu_01B2anRdzK42h65tA2gdwhb2"),
        reply("WAITING"),
        result("success"),
        lifecycle(&ua, "completed"),
        r#"{"type":"system","subtype":"task_updated","task_id":"a52af2b436ded4cea","run_id":"0mw3mxs60-f1597f03","patch":{"status":"completed","end_time":1793888580322},"uuid":"0f6c1d2e-4444-4a55-9d0e-1a2b3c4d5e02","session_id":"42e1f456-d428-4380-ad8a-473e21ef9ff7"}"#.into(),
        r#"{"type":"system","subtype":"task_notification","task_id":"a52af2b436ded4cea","run_id":"0mw3mxs60-f1597f03","tool_use_id":"toolu_01B2anRdzK42h65tA2gdwhb2","status":"completed","output_file":"/tmp/tasks/a52af2b436ded4cea.output","summary":"4","uuid":"0f6c1d2e-4444-4a55-9d0e-1a2b3c4d5e03","session_id":"42e1f456-d428-4380-ad8a-473e21ef9ff7"}"#.into(),
    ]));
    let ub = line_uuid(&core.input(&b));
    out.extend(play(
        &mut core,
        &lines_of(&[
            lifecycle(&ub, "queued"),
            reply("The background agent finished and replied: **4**"),
            result("success"),
            lifecycle(&ub, "started"),
            echo(&ub, &b.text),
            reply("PONG"),
            result("success"),
            lifecycle(&ub, "completed"),
        ]),
    ));
    let names = [(a.event_id, "A"), (b.event_id, "B")];
    assert_eq!(
        trace(&out, &names),
        [
            "accepted A",
            "message \"WAITING\" to A",
            "status to A",
            "turn_ended []",
            "message \"The background agent finished and replied: **4**\" to A",
            "status to A",
            "turn_ended [B]",
            "accepted B",
            "message \"PONG\" to B",
            "status to B",
            "turn_ended []"
        ]
    );
}

/// A message handed while the CLI runs a turn it began by itself after a background command,
/// and taken into it during a command of that turn: its echo comes first, `started` after it.
/// The rest of the turn replies to it.
#[test]
fn a_message_taken_into_a_turn_the_cli_began_is_its_input() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let a = user_input(
        sid,
        "Run the bash command `sleep 6` in the background and reply with the single word WAITING.",
    );
    let b = user_input(sid, "Reply with the single word PONG.");
    let ua = line_uuid(&core.input(&a));
    let mut out = play(&mut core, &lines_of(&[
        lifecycle(&ua, "queued"),
        lifecycle(&ua, "started"),
        echo(&ua, &a.text),
        bash("toolu_01Bg", "sleep 6"),
        r#"{"type":"system","subtype":"task_started","task_id":"b2k5bg0e1","tool_use_id":"toolu_01Bg","description":"Sleep for 6 seconds","is_backgrounded":true,"task_type":"local_bash","uuid":"0f6c1d2e-6666-4a55-9d0e-1a2b3c4d5e01","session_id":"42e1f456-d428-4380-ad8a-473e21ef9ff7"}"#.into(),
        tool_result("toolu_01Bg"),
        reply("WAITING"),
        result("success"),
        lifecycle(&ua, "completed"),
        r#"{"type":"system","subtype":"task_notification","task_id":"b2k5bg0e1","tool_use_id":"toolu_01Bg","status":"completed","output_file":"/tmp/tasks/b2k5bg0e1.output","summary":"Background command \"Sleep for 6 seconds\" completed (exit code 0)","uuid":"0f6c1d2e-6666-4a55-9d0e-1a2b3c4d5e02","session_id":"42e1f456-d428-4380-ad8a-473e21ef9ff7"}"#.into(),
        bash("toolu_02Fg", "sleep 8"),
    ]));
    let ub = line_uuid(&core.input(&b));
    out.extend(play(
        &mut core,
        &lines_of(&[
            lifecycle(&ub, "queued"),
            tool_result("toolu_02Fg"),
            echo(&ub, &b.text),
            lifecycle(&ub, "started"),
            reply("PONG"),
            result("success"),
            lifecycle(&ub, "completed"),
        ]),
    ));
    let names = [(a.event_id, "A"), (b.event_id, "B")];
    assert_eq!(
        trace(&out, &names),
        [
            "accepted A",
            "message \"WAITING\" to A",
            "status to A",
            "turn_ended []",
            "accepted B",
            "message \"PONG\" to B",
            "status to B",
            "turn_ended []"
        ]
    );
}

/// While a permission request is open nothing goes into the turn; the answer opens it again.
#[test]
fn no_message_goes_into_a_turn_that_asks() {
    let fx = fixture!("permission-allow");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    assert!(!core.can_inject(), "no turn");
    let (a, b) = (user_input(sid, "first"), user_input(sid, "second"));
    core.input(&a);
    assert!(core.can_inject());
    assert_eq!(
        approvals(&core.on_line(&can_use_tool_line(&fx, "r1", None))),
        1
    );
    assert!(!core.can_inject());
    assert!(core.inject_with_images(&b, vec![]).is_none());
    core.answer_approval("r1", "once", None).unwrap();
    assert!(core.can_inject());
}

/// The subagents running (lines of live runs on 2.1.312): a foreground and a background one
/// at once; a command of the subagent is no subagent; the foreground one ends by
/// `task_updated`, the background one by `task_notification`; a `task_updated` without a
/// status changes nothing.
#[test]
fn running_subagents_are_counted() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let stream = r#"{"type":"system","subtype":"task_started","task_id":"a55949578b136436b","run_id":"0mw3mr55z-1b9c337c","tool_use_id":"toolu_01Ti9wrYYVp7kqsruhoHEsLf","description":"Reply with number","subagent_type":"general-purpose","is_backgrounded":false,"spawn_depth":1,"task_type":"local_agent","prompt":"Reply with the number 4.","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e01"}
{"type":"system","subtype":"task_started","task_id":"a52af2b436ded4cea","run_id":"0mw3mxs60-f1597f03","tool_use_id":"toolu_01B2anRdzK42h65tA2gdwhb2","description":"Sleep and reply with number","subagent_type":"general-purpose","is_backgrounded":true,"spawn_depth":1,"task_type":"local_agent","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e02"}
{"type":"system","subtype":"task_started","task_id":"b6yusn86f","run_id":"0mw3mxvyu-6c7dc659","owned_by_subagent":true,"tool_use_id":"toolu_01T7e2mWErkbKbbTuuN4vYrm","description":"Sleep for 6 seconds","is_backgrounded":false,"parent_task_id":"a52af2b436ded4cea","task_type":"local_bash","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e03"}
{"type":"system","subtype":"task_progress","task_id":"a52af2b436ded4cea","run_id":"0mw3mxs60-f1597f03","tool_use_id":"toolu_01B2anRdzK42h65tA2gdwhb2","description":"Running Sleep for 6 seconds","subagent_type":"general-purpose","usage":{"total_tokens":23176,"tool_uses":1,"duration_ms":1310},"last_tool_name":"Bash","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e04"}
{"type":"system","subtype":"task_notification","task_id":"b6yusn86f","run_id":"0mw3mxvyu-6c7dc659","tool_use_id":"toolu_01T7e2mWErkbKbbTuuN4vYrm","status":"completed","output_file":"","summary":"Sleep for 6 seconds","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e05"}
{"type":"system","subtype":"task_updated","task_id":"a52af2b436ded4cea","patch":{"is_backgrounded":true},"uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e06"}
{"type":"system","subtype":"task_updated","task_id":"a55949578b136436b","run_id":"0mw3mr55z-1b9c337c","patch":{"status":"completed","end_time":1793888262670},"uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e07"}
{"type":"system","subtype":"task_notification","task_id":"a55949578b136436b","run_id":"0mw3mr55z-1b9c337c","tool_use_id":"toolu_01Ti9wrYYVp7kqsruhoHEsLf","status":"completed","output_file":"/tmp/tasks/a55949578b136436b.output","summary":"4","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e08"}
{"type":"system","subtype":"task_notification","task_id":"a52af2b436ded4cea","run_id":"0mw3mxs60-f1597f03","tool_use_id":"toolu_01B2anRdzK42h65tA2gdwhb2","status":"completed","output_file":"/tmp/tasks/a52af2b436ded4cea.output","summary":"4","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e09"}"#;
    // The count reported by each line, in order.
    let counts: Vec<Vec<u32>> = stream
        .lines()
        .map(|l| {
            core.on_line(l)
                .into_iter()
                .filter_map(|a| match a {
                    Action::Emit(AdapterEvent::Subagents(n)) => Some(n),
                    _ => None,
                })
                .collect()
        })
        .collect();
    assert_eq!(
        counts,
        [
            vec![1],
            vec![2],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![1],
            vec![],
            vec![0]
        ]
    );
}

/// A foreground subagent the CLI sent no end for (an interrupt, a denied call,
/// `error_max_turns`) stops counting at the `result` of the main turn; a background one goes
/// on until its `task_notification`.
#[test]
fn a_foreground_subagent_stops_counting_at_the_result() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let stream = r#"{"type":"system","subtype":"task_started","task_id":"a55949578b136436b","run_id":"0mw3mr55z-1b9c337c","tool_use_id":"toolu_01Ti9wrYYVp7kqsruhoHEsLf","description":"Reply with number","subagent_type":"general-purpose","is_backgrounded":false,"spawn_depth":1,"task_type":"local_agent","prompt":"Reply with the number 4.","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e01"}
{"type":"system","subtype":"task_started","task_id":"a52af2b436ded4cea","run_id":"0mw3mxs60-f1597f03","tool_use_id":"toolu_01B2anRdzK42h65tA2gdwhb2","description":"Sleep and reply with number","subagent_type":"general-purpose","is_backgrounded":true,"spawn_depth":1,"task_type":"local_agent","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e02"}
{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":2,"terminal_reason":"aborted_tools","total_cost_usd":0.001,"usage":{"input_tokens":4,"output_tokens":2}}
{"type":"result","subtype":"success","is_error":false,"num_turns":0,"total_cost_usd":0.001,"usage":{"input_tokens":0,"output_tokens":0}}
{"type":"system","subtype":"task_notification","task_id":"a55949578b136436b","run_id":"0mw3mr55z-1b9c337c","tool_use_id":"toolu_01Ti9wrYYVp7kqsruhoHEsLf","status":"stopped","output_file":"","summary":"4","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e08"}
{"type":"system","subtype":"task_notification","task_id":"a52af2b436ded4cea","run_id":"0mw3mxs60-f1597f03","tool_use_id":"toolu_01B2anRdzK42h65tA2gdwhb2","status":"completed","output_file":"/tmp/tasks/a52af2b436ded4cea.output","summary":"4","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e09"}"#;
    // The count reported by each line, in order.
    let counts: Vec<Vec<u32>> = stream
        .lines()
        .map(|l| {
            core.on_line(l)
                .into_iter()
                .filter_map(|a| match a {
                    Action::Emit(AdapterEvent::Subagents(n)) => Some(n),
                    _ => None,
                })
                .collect()
        })
        .collect();
    assert_eq!(counts, [vec![1], vec![2], vec![1], vec![], vec![], vec![0]]);
}

/// A background subagent whose Bash call the CLI moves to the background (2.1.312 with
/// `CLAUDE_CODE_ENTRYPOINT=claude-vscode`, a subagent's 240 s `ping` and its Monitor
/// `blm1ph76i` waiting for it; the `system` task lines, `background_tasks_changed` and `result`
/// of the capture, in order, with the fields the adapter reads; the lines between them change
/// no count).
const MOVED_SUBAGENT: &str = r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a739e2460001cfdc1","task_type":"local_agent"}]}
{"type":"system","subtype":"task_started","task_id":"a739e2460001cfdc1","is_backgrounded":true,"task_type":"local_agent"}
{"type":"result","subtype":"success","num_turns":2}
{"type":"system","subtype":"task_started","task_id":"b0h1rmks3","owned_by_subagent":true,"is_backgrounded":false,"parent_task_id":"a739e2460001cfdc1","task_type":"local_bash"}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a739e2460001cfdc1","task_type":"local_agent"},{"task_id":"b0h1rmks3","task_type":"local_bash","parent_task_id":"a739e2460001cfdc1"}]}
{"type":"system","subtype":"task_updated","task_id":"b0h1rmks3","patch":{"is_backgrounded":true}}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a739e2460001cfdc1","task_type":"local_agent"},{"task_id":"b0h1rmks3","task_type":"local_bash","parent_task_id":"a739e2460001cfdc1"},{"task_id":"blm1ph76i","task_type":"local_bash","parent_task_id":"a739e2460001cfdc1"}]}
{"type":"system","subtype":"task_started","task_id":"blm1ph76i","owned_by_subagent":true,"is_backgrounded":true,"parent_task_id":"a739e2460001cfdc1","task_type":"local_bash"}
{"type":"system","subtype":"task_updated","task_id":"a739e2460001cfdc1","patch":{"status":"completed"}}
{"type":"system","subtype":"task_notification","task_id":"a739e2460001cfdc1","status":"completed","summary":"This agent has not reported yet: it is waiting on its own background work and will deliver its report through SubagentHandback when that finishes.\n"}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"b0h1rmks3","task_type":"local_bash","parent_task_id":"a739e2460001cfdc1"},{"task_id":"blm1ph76i","task_type":"local_bash","parent_task_id":"a739e2460001cfdc1"}]}
{"type":"result","subtype":"success","num_turns":1}
{"type":"system","subtype":"task_updated","task_id":"b0h1rmks3","patch":{"status":"completed"}}
{"type":"system","subtype":"task_notification","task_id":"b0h1rmks3","status":"completed","summary":"Background command \"Ping localhost 240 times quietly\" completed (exit code 0)"}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"blm1ph76i","task_type":"local_bash","parent_task_id":"a739e2460001cfdc1"}]}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a739e2460001cfdc1","task_type":"local_agent"},{"task_id":"blm1ph76i","task_type":"local_bash","parent_task_id":"a739e2460001cfdc1"}]}
{"type":"system","subtype":"task_started","task_id":"a739e2460001cfdc1","is_backgrounded":true,"task_type":"local_agent"}
{"type":"system","subtype":"task_updated","task_id":"blm1ph76i","patch":{"status":"completed"}}
{"type":"system","subtype":"task_notification","task_id":"blm1ph76i","status":"completed","summary":"Monitor \"wait for background ping to finish\" stream ended"}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a739e2460001cfdc1","task_type":"local_agent"}]}
{"type":"system","subtype":"task_updated","task_id":"a739e2460001cfdc1","patch":{"status":"completed"}}
{"type":"system","subtype":"task_notification","task_id":"a739e2460001cfdc1","status":"completed","handback":"send","summary":"This agent's report was delivered to you as a message from \"a739e2460001cfdc1\" (its SubagentHandback call). Read it there; it is not repeated here.\n"}
{"type":"system","subtype":"background_tasks_changed","tasks":[]}
{"type":"result","subtype":"success","num_turns":1}
{"type":"result","subtype":"success","num_turns":0}"#;

/// The subagent and background task counts and the ends of turns each line reports, in order.
fn task_counts<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<Vec<String>> {
    let mut core = Core::new(Uuid::now_v7(), false);
    lines
        .map(|l| {
            core.on_line(l)
                .into_iter()
                .filter_map(|a| match a {
                    Action::Emit(AdapterEvent::Subagents(n)) => Some(format!("subagents:{n}")),
                    Action::Emit(AdapterEvent::BackgroundTasks(n)) => {
                        Some(format!("background_tasks:{n}"))
                    }
                    Action::Emit(AdapterEvent::TurnEnded { .. }) => Some("turn_ended".into()),
                    _ => None,
                })
                .collect()
        })
        .collect()
}

/// [`MOVED_SUBAGENT`]: the CLI reports the subagent ended while it waits for its commands and
/// starts it again when the `ping` ends; it counts all along, until it ends for real, and that
/// end comes before the end of the turn. The number of listed background tasks is reported as
/// it changes.
#[test]
fn a_subagent_waiting_on_its_background_command_still_counts() {
    let want: [&[&str]; 25] = [
        &["background_tasks:1", "subagents:1"],
        &[],
        &["turn_ended"],
        &[],
        &["background_tasks:2"],
        &[],
        &["background_tasks:3"],
        &[],
        // The CLI says the subagent completed; its commands still run.
        &[],
        &[],
        &["background_tasks:2"],
        &["turn_ended"],
        &[],
        &[],
        &["background_tasks:1"],
        &["background_tasks:2"],
        // Started again by the CLI: it never stopped counting.
        &[],
        &[],
        &[],
        &["background_tasks:1"],
        // The real end; still listed until `[]`, reported at the next line.
        &[],
        &[],
        &["background_tasks:0"],
        &["subagents:0", "turn_ended"],
        &["turn_ended"],
    ];
    assert_eq!(task_counts(MOVED_SUBAGENT.lines()), want);
}

/// [`MOVED_SUBAGENT`] without the Monitor `blm1ph76i`, one long command only (a list that comes
/// out the same as the one before changes nothing): when the command ends the CLI lists none,
/// `[]`, and only on the next line the subagent again, `[a739…]`, then starts it. That drop on a
/// `background_tasks_changed` alone is not reported.
#[test]
fn a_subagent_with_one_background_command_still_counts() {
    let lines = one_background_command();
    let want: [&[&str]; 22] = [
        &["background_tasks:1", "subagents:1"],
        &[],
        &["turn_ended"],
        &[],
        &["background_tasks:2"],
        &[],
        &[],
        &[],
        &[],
        &["background_tasks:1"],
        &["turn_ended"],
        &[],
        &[],
        // The `ping` ended: `[]`, then the subagent listed again and started.
        &["background_tasks:0"],
        &["background_tasks:1"],
        &[],
        &[],
        &[],
        &[],
        &["background_tasks:0"],
        &["subagents:0", "turn_ended"],
        &["turn_ended"],
    ];
    assert_eq!(task_counts(lines.iter().map(String::as_str)), want);
}

/// The lines of [`MOVED_SUBAGENT`] without the Monitor `blm1ph76i`.
fn one_background_command() -> Vec<String> {
    MOVED_SUBAGENT
        .lines()
        .filter_map(|l| {
            let mut v: Value = serde_json::from_str(l).unwrap();
            if v["subtype"] == "background_tasks_changed" {
                let tasks = v["tasks"].as_array_mut().unwrap();
                tasks.retain(|t| t["task_id"] != "blm1ph76i");
                return Some(v.to_string());
            }
            (!l.contains("blm1ph76i")).then(|| l.to_owned())
        })
        .collect()
}

/// The `system` lines each line reports, by its index.
fn system_lines<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<(usize, String, String)> {
    let mut core = Core::new(Uuid::now_v7(), false);
    lines
        .enumerate()
        .flat_map(|(i, l)| {
            system_events(&core.on_line(l))
                .into_iter()
                .map(move |(code, text)| (i, code, text))
        })
        .collect()
}

/// [`MOVED_SUBAGENT`], and its variant with one long command: the subagent's first
/// `task_notification` ("has not reported yet", no `handback`) comes while the list still
/// holds it and its commands, the second, its real end with the report, while the list still
/// holds it alone. One "Background task" line, with the real end's summary, at the real end.
/// Its commands' ends give none.
#[test]
fn a_subagent_waiting_on_its_background_command_ends_in_one_line() {
    let end = |i: usize| {
        vec![(
            i,
            "task_notification".to_string(),
            "This agent's report was delivered to you as a message from \"a739e2460001cfdc1\" (its SubagentHandback call). Read it there; it is not repeated here.".to_string(),
        )]
    };
    assert_eq!(system_lines(MOVED_SUBAGENT.lines()), end(21));
    let lines = one_background_command();
    assert_eq!(system_lines(lines.iter().map(String::as_str)), end(18));
}

/// [`MOVED_SUBAGENT`] up to the list after the "has not reported yet" end, then the commands
/// leave the list with no restart and no other end: the held end gives its line at the next
/// line of another kind, the `init`, and the `result` gives none.
#[test]
fn a_held_subagent_end_comes_at_the_next_line_of_another_kind() {
    let mut lines: Vec<&str> = MOVED_SUBAGENT.lines().take(11).collect();
    lines.extend([
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[]}"#,
        r#"{"type":"system","subtype":"init"}"#,
        r#"{"type":"result","subtype":"success","num_turns":1}"#,
    ]);
    let got: Vec<(usize, String)> = system_lines(lines.into_iter())
        .into_iter()
        .map(|(i, code, _)| (i, code))
        .collect();
    assert_eq!(got, [(12, "task_notification".to_string())]);
}

/// A subagent hands its report over while a command it moved to the background still runs
/// (live on 2.1.312, a wait loop that never ended): its end, with `handback`, is the line at
/// once, and the command left in the list no longer counts it. When the command ends the CLI
/// lists and starts the subagent again: it counts by a task of its own again, and its next
/// end, held while that task is listed, gives no second line.
#[test]
fn a_subagent_that_reported_with_a_command_left_running_ends_at_once() {
    let lines = [
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"A","task_type":"local_agent"}]}"#,
        r#"{"type":"system","subtype":"task_started","task_id":"A","is_backgrounded":true,"task_type":"local_agent"}"#,
        r#"{"type":"system","subtype":"task_started","task_id":"c","owned_by_subagent":true,"is_backgrounded":false,"parent_task_id":"A","task_type":"local_bash"}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"A","task_type":"local_agent"},{"task_id":"c","task_type":"local_bash","parent_task_id":"A"}]}"#,
        r#"{"type":"system","subtype":"task_updated","task_id":"c","patch":{"is_backgrounded":true}}"#,
        r#"{"type":"system","subtype":"task_updated","task_id":"A","patch":{"status":"completed"}}"#,
        r#"{"type":"system","subtype":"task_notification","task_id":"A","status":"completed","handback":"send","summary":"reported"}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"c","task_type":"local_bash","parent_task_id":"A"}]}"#,
        r#"{"type":"result","subtype":"success","num_turns":1}"#,
        r#"{"type":"system","subtype":"task_updated","task_id":"c","patch":{"status":"completed"}}"#,
        r#"{"type":"system","subtype":"task_notification","task_id":"c","status":"completed","summary":"Background command ended"}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[]}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"A","task_type":"local_agent"}]}"#,
        r#"{"type":"system","subtype":"task_started","task_id":"A","is_backgrounded":true,"task_type":"local_agent"}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"d","task_type":"local_bash","parent_task_id":"A"}]}"#,
        r#"{"type":"system","subtype":"task_updated","task_id":"A","patch":{"status":"completed"}}"#,
        r#"{"type":"system","subtype":"task_notification","task_id":"A","status":"completed","summary":"waiting again"}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[]}"#,
        r#"{"type":"result","subtype":"success","num_turns":1}"#,
    ];
    assert_eq!(
        system_lines(lines.into_iter()),
        [(6, "task_notification".to_string(), "reported".to_string())]
    );
    let subagents: Vec<(usize, String)> = task_counts(lines.into_iter())
        .into_iter()
        .enumerate()
        .flat_map(|(i, e)| e.into_iter().map(move |e| (i, e)))
        .filter(|(_, e)| e.starts_with("subagents"))
        .collect();
    let want: Vec<(usize, String)> = [(0, 1), (8, 0), (12, 1), (18, 0)]
        .into_iter()
        .map(|(i, n)| (i, format!("subagents:{n}")))
        .collect();
    assert_eq!(subagents, want);
}

/// Subagent `A` held while a task of it is listed; a list that drops `A` and adds `B` at once
/// does not count `A` out at once: the next list holds `A` again, and its end waits. Its real
/// end comes when a list drops it, at the next line of another kind.
#[test]
fn a_held_subagent_listed_again_with_another_keeps_its_end() {
    let lines = [
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"A","task_type":"local_agent"}]}"#,
        r#"{"type":"system","subtype":"task_started","task_id":"A","is_backgrounded":true,"task_type":"local_agent"}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"A","task_type":"local_agent"},{"task_id":"c","task_type":"local_bash","parent_task_id":"A"}]}"#,
        r#"{"type":"system","subtype":"task_updated","task_id":"A","patch":{"status":"completed"}}"#,
        r#"{"type":"system","subtype":"task_notification","task_id":"A","status":"completed","summary":"not yet"}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"B","task_type":"local_agent"}]}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"A","task_type":"local_agent"},{"task_id":"B","task_type":"local_agent"}]}"#,
        r#"{"type":"result","subtype":"success","num_turns":1}"#,
        r#"{"type":"system","subtype":"task_notification","task_id":"A","status":"completed","summary":"done"}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"B","task_type":"local_agent"}]}"#,
        r#"{"type":"result","subtype":"success","num_turns":0}"#,
    ];
    assert_eq!(
        system_lines(lines.into_iter()),
        [(10, "task_notification".to_string(), "done".to_string())]
    );
}

/// A background subagent that hands its report over while its 200 s `ping`, moved to the
/// background, still runs; when the `ping` ends the CLI lists and starts it again, and it
/// reports again (2.1.312 with `CLAUDE_CODE_ENTRYPOINT=claude-vscode`; the task lines, `init`
/// and `result` of the capture, in order, with the fields the adapter reads).
const REPORTED_SUBAGENT: &str = r#"{"type":"system","subtype":"init"}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a7b736ac9a06b8816","task_type":"local_agent"}]}
{"type":"system","subtype":"task_started","task_id":"a7b736ac9a06b8816","is_backgrounded":true,"task_type":"local_agent"}
{"type":"result","subtype":"success","num_turns":2}
{"type":"system","subtype":"task_started","task_id":"baydyre83","owned_by_subagent":true,"is_backgrounded":false,"parent_task_id":"a7b736ac9a06b8816","task_type":"local_bash"}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a7b736ac9a06b8816","task_type":"local_agent"},{"task_id":"baydyre83","task_type":"local_bash","parent_task_id":"a7b736ac9a06b8816"}]}
{"type":"system","subtype":"task_updated","task_id":"baydyre83","patch":{"is_backgrounded":true}}
{"type":"system","subtype":"task_updated","task_id":"a7b736ac9a06b8816","patch":{"status":"completed"}}
{"type":"system","subtype":"task_notification","task_id":"a7b736ac9a06b8816","status":"completed","handback":"send","summary":"This agent's report was delivered to you as a message from \"a7b736ac9a06b8816\" (its SubagentHandback call). Read it there; it is not repeated here.\n"}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"baydyre83","task_type":"local_bash","parent_task_id":"a7b736ac9a06b8816"}]}
{"type":"system","subtype":"init"}
{"type":"result","subtype":"success","num_turns":1}
{"type":"system","subtype":"init"}
{"type":"result","subtype":"success","num_turns":1}
{"type":"system","subtype":"task_updated","task_id":"baydyre83","patch":{"status":"completed"}}
{"type":"system","subtype":"task_notification","task_id":"baydyre83","status":"completed","summary":"Background command \"Ping localhost 200 times quietly\" completed (exit code 0)"}
{"type":"system","subtype":"background_tasks_changed","tasks":[]}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a7b736ac9a06b8816","task_type":"local_agent"}]}
{"type":"system","subtype":"task_started","task_id":"a7b736ac9a06b8816","is_backgrounded":true,"task_type":"local_agent"}
{"type":"system","subtype":"task_updated","task_id":"a7b736ac9a06b8816","patch":{"status":"completed"}}
{"type":"system","subtype":"task_notification","task_id":"a7b736ac9a06b8816","status":"completed","handback":"send","summary":"This agent's report was delivered to you as a message from \"a7b736ac9a06b8816\" (its SubagentHandback call). Read it there; it is not repeated here.\n"}
{"type":"system","subtype":"background_tasks_changed","tasks":[]}
{"type":"system","subtype":"init"}
{"type":"result","subtype":"success","num_turns":1}"#;

/// [`REPORTED_SUBAGENT`]: one line, at the first report; the second report gives none. The
/// `ping` left running does not count the subagent; the restart does, until `[]`.
#[test]
fn a_subagent_that_reports_twice_gives_one_line() {
    assert_eq!(
        system_lines(REPORTED_SUBAGENT.lines()),
        [(
            8,
            "task_notification".to_string(),
            "This agent's report was delivered to you as a message from \"a7b736ac9a06b8816\" (its SubagentHandback call). Read it there; it is not repeated here.".to_string(),
        )]
    );
    let subagents: Vec<(usize, String)> = task_counts(REPORTED_SUBAGENT.lines())
        .into_iter()
        .enumerate()
        .flat_map(|(i, e)| e.into_iter().map(move |e| (i, e)))
        .filter(|(_, e)| e.starts_with("subagents"))
        .collect();
    let want: Vec<(usize, String)> = [(1, 1), (10, 0), (17, 1), (22, 0)]
        .into_iter()
        .map(|(i, n)| (i, format!("subagents:{n}")))
        .collect();
    assert_eq!(subagents, want);
}

/// [`MOVED_SUBAGENT`] up to the subagent's first `task_notification`, then the process exits:
/// the held end is not lost, it gives its line at the exit.
#[test]
fn a_held_subagent_end_comes_at_exit() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let mut got = vec![];
    for l in MOVED_SUBAGENT.lines().take(10) {
        got.extend(system_events(&core.on_line(l)));
    }
    assert_eq!(got, vec![]);
    got.extend(system_events(&core.on_exit()));
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, "task_notification");
    assert!(
        got[0].1.starts_with("This agent has not reported yet"),
        "{got:?}"
    );
}

/// [`MOVED_SUBAGENT`] with a request of the subagent open from its start: its first
/// `task_notification` is no end and cancels nothing; the request is cancelled at its real end.
#[test]
fn a_subagent_waiting_on_its_background_command_keeps_its_request() {
    let fx = fixture!("permission-allow");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let mut req: Value =
        serde_json::from_str(&can_use_tool_line(&fx, "sub", Some("touch x"))).unwrap();
    req["request"]["agent_id"] = json!("a739e2460001cfdc1");
    let mut cancels: Vec<(usize, String)> = vec![];
    for (i, l) in MOVED_SUBAGENT.lines().enumerate() {
        for e in events(&emitted(core.on_line(l))) {
            if let EventKind::ApprovalCancel { request_id, .. } = &e.kind {
                cancels.push((i, request_id.clone()));
            }
        }
        if i == 1 {
            assert_eq!(approvals(&core.on_line(&req.to_string())), 1);
        }
    }
    assert_eq!(cancels, [(21, "sub".to_string())]);
}
