//! T4 acceptance on the T0 fixtures (Claude Code 2.1.307), without spawning `claude`: the
//! adapter runs against [`ScriptedClaude`] playing the captured stdout, and its stdin is
//! compared with what the spike driver wrote.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use hqc_proto::event::{
    ApprovalCanonical, Author, ClarifyAnswer, ClarifyQuestion, Event, EventKind, SessionState,
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
            })
            .await
            .unwrap();
        let mut out = Vec::new();
        loop {
            let ev = self.next().await;
            out.push(ev.clone());
            match ev {
                AdapterEvent::TurnEnded => return out,
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
            AdapterEvent::TurnEnded => "turn_ended".into(),
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
    assert_eq!(h.claude.stdin(), lines(fx.stdin));
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
            detail: None
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
    // The same answer as the spike's, except for the message text.
    let mut want = lines(fx.stdin);
    let mut got = h.claude.stdin();
    assert_eq!(
        got[1]["response"]["response"]["message"],
        super::engine::DENY_MESSAGE
    );
    for v in [&mut want[1], &mut got[1]] {
        v["response"]["response"]
            .as_object_mut()
            .unwrap()
            .remove("message");
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
    assert_eq!(h.claude.stdin(), want);
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
    assert_eq!(
        questions,
        [ClarifyQuestion {
            id: "1".into(),
            text: "Which area should I focus on first?".into(),
            options: Some(vec![
                "Hub (Recommended)".into(),
                "Worker".into(),
                "Protocol".into(),
                "Client".into()
            ]),
            multi: Some(false),
        }]
    );
    assert_eq!(h.claude.stdin(), lines(fx.stdin));
}

#[tokio::test]
async fn mcp_timeout_turn_ends_with_detail() {
    let fx = fixture!("mcp-timeout");
    let (h, evs, _) = run(&fx, "once", BTreeMap::new()).await;
    assert_eq!(h.claude.stdin(), lines(fx.stdin));
    let evs = events(&evs);
    assert!(evs.iter().any(|e| matches!(
        &e.kind,
        EventKind::Status { state: SessionState::Idle, detail: Some(d) } if d == "error_max_turns"
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
    };
    let a = core.input_with_images(&input, images);
    assert!(matches!(&a[..], [Action::Write(v)] if *v == want), "{a:?}");
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
    });
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
                in_reply_to: Some(input)
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
    })
    .await
    .unwrap();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        if ev == AdapterEvent::TurnEnded {
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
