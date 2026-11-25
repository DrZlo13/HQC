//! T4 acceptance on the T0 fixtures (Claude Code 2.1.307), without spawning `claude`: the
//! adapter runs against [`ScriptedClaude`] playing the captured stdout, and its stdin is
//! compared with what the spike driver wrote.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use hqc_proto::event::{
    AgentTask, ApiRetry, ApprovalCanonical, ApprovalOption, Author, CancelReason, ClarifyAction,
    ClarifyAnswer, ClarifyQuestion, ContextCompacted, Event, EventKind, GrantPlace, LiveKind,
    LiveSubagent, OptionGrant, SessionState, TaskStatus,
};
use hqc_proto::frame::{Activity, LeftoverCommand, LongRun, Runtime, SessionInput, SlashCommand};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::engine::{
    args_preview, grant_rules, next_cron_fire, record_event_id, resurrected_cron_jobs, Action,
    Core, COMMAND_TEXT_LIMIT, CONVERSATION_CLEARED, CONVERSATION_CLEARED_TEXT,
    OPTION_PREVIEW_LIMIT, OPTION_PREVIEW_TRUNCATED, PROMPT_INJECTION_TEXT,
};
use super::process::{CommandSpec, OsSpawner, Sig};
use super::scripted::ScriptedClaude;
use super::{ClaudeCodeAdapter, ClaudeCodeAdapters, ClaudeOptions, Launch};
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
                // A person's `origin` is checked on its own (SPEC 0.219).
                v.as_object_mut().unwrap().remove("origin");
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
        exit_grace: Duration::from_millis(50),
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
        has_history: false,
        provider: None,
        provider_refused: None,
        secrets: None,
        browser: false,
        browser_refused: None,
        mcp_servers: vec![],
        skills: vec![],
        skill_dir: None,
        fork: None,
    }
}

impl Harness {
    async fn start(stream: &str, claude_session: Uuid) -> Self {
        Self::start_with(ScriptedClaude::new(stream), claude_session).await
    }

    async fn start_with(claude: ScriptedClaude, claude_session: Uuid) -> Self {
        Self::start_with_options(claude, claude_session, |_| {}).await
    }

    /// [`Harness::start_with`] over [`options`] as `tune` changes them.
    async fn start_with_options(
        claude: ScriptedClaude,
        claude_session: Uuid,
        tune: impl FnOnce(&mut ClaudeOptions),
    ) -> Self {
        let work = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let mut opts = options(config.path());
        tune(&mut opts);
        let factory = ClaudeCodeAdapters::new(opts, Arc::new(claude.clone()));
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
                human: false,
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
                        .answer_clarify(request_id, &clarify, None)
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
            AdapterEvent::Wakeup(at) => format!("wakeup:{}", at.is_some()),
            // Every `system/init` carries it; the T38 tests look at it on their own.
            AdapterEvent::PermissionMode(_) => continue,
            // Every turn's `rate_limit_event`; `limits_of_a_rate_limit_event` looks at it.
            AdapterEvent::Limits(_) => continue,
            // With every count and progress; `live_subagents_are_listed` looks at it.
            AdapterEvent::LiveSubagents(_) => continue,
            // Counted every 2 s while the agent works, not by the stream's lines.
            AdapterEvent::TokenRate(_) => continue,
            // `foreground_calls_are_reported` looks at it.
            AdapterEvent::ForegroundCalls(_) => continue,
            // `the_activity_follows_the_stream` and `a_heartbeat_is_reported` look at them.
            AdapterEvent::Activity(_) | AdapterEvent::Heartbeat => continue,
            AdapterEvent::ModelKept(m) => format!("model_kept:{m}"),
            AdapterEvent::ToolResult { .. } => "tool.result".into(),
            // `a_task_end_carries_its_usage_and_output` looks at it.
            AdapterEvent::TaskOutput(_) => continue,
            AdapterEvent::SubagentLeftovers { .. } => "subagent_leftovers".into(),
            AdapterEvent::LongRuns { .. } => "long_runs".into(),
            AdapterEvent::QuietTaken { .. } => "quiet_taken".into(),
            AdapterEvent::QuietCancelled { .. } => "quiet_cancelled".into(),
            AdapterEvent::SideAnswer { .. } => "side_answer".into(),
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
            AdapterEvent::Event(e) | AdapterEvent::ToolResult { event: e, .. } => Some(e),
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
        // A turn that ended early ends with the adapter's own `system` line.
        let author = match e.kind {
            EventKind::System { .. } => Author::System,
            _ => Author::Agent { id: session_id },
        };
        assert_eq!(e.author, author);
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

/// The `args_preview` and the options of the first `approval.request`.
fn approval_card(evs: &[AdapterEvent]) -> (Option<String>, Vec<ApprovalOption>) {
    events(evs)
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::ApprovalRequest {
                args_preview,
                options,
                ..
            } => Some((args_preview.clone(), options.clone())),
            _ => None,
        })
        .expect("approval.request")
}

fn grant_of(options: &[ApprovalOption], id: &str) -> Option<OptionGrant> {
    options.iter().find(|o| o.id == id).unwrap().grant.clone()
}

/// T13 (SPEC §8.1): when the turn ends the adapter reads the transcript and reports its
/// last `ai-title` before `TurnEnded` (a tick during the turn may read it earlier).
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
    assert_eq!(titles_in(&evs), ["Touch a file"]);
}

/// SPEC 0.218: before a `result` line the adapter reads the goal records the transcript holds,
/// so the check of the turn's last text is sent with the turn, not up to a tick later, after
/// whatever follows the turn at once. The meter's next tick is 2 s away: without the read the
/// record is not sent before `TurnEnded`.
#[tokio::test]
async fn a_goal_record_is_read_before_the_result() {
    let fx = fixture!("permission-allow");
    let mut h = Harness::start(fx.stream, fx.claude_session()).await;
    let (work, config) = &h._dirs;
    let transcript = super::paths::transcript_path(
        config.path(),
        &work.path().canonicalize().unwrap(),
        &fx.claude_session(),
    );
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    let goal = json!({"type": "attachment", "uuid": "00000000-0000-4000-8000-0000000000aa",
        "timestamp": "2099-01-01T00:00:00.000Z",
        "attachment": {"type": "goal_status", "met": false, "condition": "C", "reason": "R"}});
    std::fs::write(&transcript, format!("{}{goal}\n", fx.transcript)).unwrap();

    let evs = h
        .turn(&fx.prompt(), Uuid::now_v7(), "once", BTreeMap::new())
        .await;
    assert!(
        events(&evs).iter().any(
            |e| matches!(&e.kind, EventKind::System { goal: Some(g), .. } if g.state == "unmet")
        ),
        "{:?}",
        shape(&evs)
    );
}

/// The titles among `evs`.
fn titles_in(evs: &[AdapterEvent]) -> Vec<&str> {
    evs.iter()
        .filter_map(|e| match e {
            AdapterEvent::Title(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

/// SPEC §8.1: a title a person gave (`custom-title`) is the one reported at the end of the
/// turn, over an `ai-title` written after it.
#[tokio::test]
async fn a_custom_title_is_reported_at_turn_end() {
    let fx = fixture!("permission-allow");
    let mut h = Harness::start(fx.stream, fx.claude_session()).await;
    let (work, config) = &h._dirs;
    let transcript = super::paths::transcript_path(
        config.path(),
        &work.path().canonicalize().unwrap(),
        &fx.claude_session(),
    );
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    let titles = [
        r#"{"type":"custom-title","customTitle":"Mine","sessionId":"x"}"#,
        r#"{"type":"ai-title","aiTitle":"Touch a file","sessionId":"x"}"#,
    ]
    .join("\n");
    std::fs::write(&transcript, format!("{}{titles}\n", fx.transcript)).unwrap();

    let evs = h
        .turn(&fx.prompt(), Uuid::now_v7(), "once", BTreeMap::new())
        .await;
    assert_eq!(titles_in(&evs), ["Mine"]);
}

/// SPEC 0.181: a subagent's transcript is `<CLI session id>/subagents/agent-<task_id>.jsonl`
/// beside the session's transcript, the id from `system/init`; none before it came.
#[tokio::test]
async fn a_subagents_transcript_is_beside_the_sessions() {
    let fx = fixture!("permission-allow");
    let mut h = Harness::start(fx.stream, fx.claude_session()).await;
    assert_eq!(h.adapter.subagent_transcript("a1"), None);
    let (work, config) = &h._dirs;
    let transcript = super::paths::transcript_path(
        config.path(),
        &work.path().canonicalize().unwrap(),
        &fx.claude_session(),
    );
    h.turn(&fx.prompt(), Uuid::now_v7(), "once", BTreeMap::new())
        .await;
    assert_eq!(
        h.adapter.subagent_transcript("a52af2b436ded4cea"),
        Some(
            transcript
                .parent()
                .unwrap()
                .join(fx.claude_session().to_string())
                .join("subagents/agent-a52af2b436ded4cea.jsonl")
        )
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
    let evs_all = evs;
    let evs = events(&evs_all);
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
    // The card carries the call's arguments, as the call does (SPEC 0.207), and says what
    // `session` and `always` grant.
    let (args, options) = approval_card(&evs_all);
    assert_eq!(args.as_deref(), Some(call.2.as_str()));
    assert_eq!(grant_of(&options, "once"), None);
    assert_eq!(grant_of(&options, "deny"), None);
    assert_eq!(
        grant_of(&options, "session"),
        Some(OptionGrant {
            place: GrantPlace::Session,
            rules: vec!["Bash(touch hqc-marker-once *)".into()],
            exact: true,
        })
    );
    assert_eq!(
        grant_of(&options, "always"),
        Some(OptionGrant {
            place: GrantPlace::LocalSettings,
            rules: vec!["Bash(touch hqc-marker-once *)".into()],
            exact: false,
        })
    );
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
        EventKind::Usage { input_tokens: 4, output_tokens: 96, cache_read: Some(36251), cache_write: Some(12206), cost_usd: Some(_), session_cost_usd: Some(_), model: Some(m), .. }
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
    // What `always` says it writes is what it sends (SPEC 0.207).
    let (_, options) = approval_card(&evs);
    assert_eq!(
        grant_of(&options, "always"),
        Some(OptionGrant {
            place: GrantPlace::LocalSettings,
            rules: vec!["Bash(touch hqc-marker *)".into()],
            exact: false,
        })
    );
}

/// A rule too long for the card is cut on a char boundary and says so; a whole one is as it is.
#[test]
fn a_cut_grant_rule_ends_with_an_ellipsis() {
    let whole = "Bash(ls)".to_string();
    // 100 three-byte chars: 300 bytes, the 256-byte limit falls inside a char.
    let long = format!("Bash({})", "日".repeat(100));
    let rules = grant_rules([whole.clone(), long.clone()]);
    assert_eq!(rules[0], whole);
    assert!(rules[1].ends_with('…'));
    assert!(rules[1].len() <= 256);
    assert!(long.starts_with(rules[1].trim_end_matches('…')));
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
            option_previews: None,
            multi: Some(false),
            required: false,
            input: None,
        }]
    );
    assert_eq!(without_uuids(h.claude.stdin()), lines(fx.stdin));
}

/// A turn that hit `--max-turns` (`error_max_turns`, `terminal_reason: max_turns`) ends with
/// a line that says why, after its `usage`; its `status idle` carries no reason of its own.
#[tokio::test]
async fn mcp_timeout_turn_says_it_ended_early() {
    let fx = fixture!("mcp-timeout");
    let (h, evs, _) = run(&fx, "once", BTreeMap::new()).await;
    assert_eq!(without_uuids(h.claude.stdin()), lines(fx.stdin));
    assert_eq!(
        shape(&evs[evs.len() - 4..]),
        ["usage", "system", "status", "turn_ended"]
    );
    let evs = events(&evs);
    assert!(evs.iter().any(|e| matches!(
        &e.kind,
        EventKind::System { code, text, .. } if code == "turn_ended_early"
            && text == "Turn ended early: max_turns · Reached maximum number of turns (2)"
    )));
    assert!(evs.iter().any(|e| matches!(
        &e.kind,
        EventKind::Status {
            state: SessionState::Idle,
            detail: None,
            ..
        }
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
        .settings()
        .to_string();
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
            "--no-chrome",
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

/// SPEC §8.1: a session that had a conversation (`session.start.has_history`) whose transcript
/// is gone starts anew and says so in the feed; with its transcript, or without a
/// conversation, nothing is said.
#[tokio::test]
async fn a_missing_transcript_of_a_session_with_history_is_reported() {
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let claude = ScriptedClaude::new("");
    let factory = ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude.clone()));
    let claude_session = Uuid::new_v4();
    let mut s = spec(Uuid::now_v7(), work.path(), claude_session);
    let (tx, mut rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx);

    // No conversation yet: a new start, quietly.
    a.start(&s).await.unwrap();
    a.kill().await.unwrap();
    assert!(rx.try_recv().is_err());

    s.has_history = true;
    a.resume(&s).await.unwrap();
    assert!(claude.commands()[1]
        .args
        .contains(&"--session-id".to_string()));
    let Ok(AdapterEvent::Event(ev)) = rx.try_recv() else {
        panic!("no event for the missing transcript");
    };
    assert_eq!(ev.author, Author::System);
    let EventKind::System { code, text, .. } = &ev.kind else {
        panic!("{ev:?}");
    };
    assert_eq!(code, super::TRANSCRIPT_MISSING);
    assert!(text.contains("cleanupPeriodDays"), "{text}");
    a.kill().await.unwrap();

    // With the transcript on disk it resumes, quietly.
    let t = super::paths::transcript_path(config.path(), &s.workdir, &claude_session);
    std::fs::create_dir_all(t.parent().unwrap()).unwrap();
    std::fs::write(&t, "").unwrap();
    a.resume(&s).await.unwrap();
    assert!(claude.commands()[2].args.contains(&"--resume".to_string()));
    a.kill().await.unwrap();
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
        allow_all: false,
        allow: vec!["Bash(cargo test:*)".into(), "Read".into()],
        deny: vec!["Bash(git push:*)".into()],
        remote_unblock: Default::default(),
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
        let args = a.command(s, &Uuid::nil(), Launch::New, None, None).args;
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
        .command(&s, &Uuid::nil(), Launch::New, None, None)
        .args
        .contains(&"--settings".to_string()));
}

/// T43 (SPEC §14): a session in a bound folder gets its environment's merged rules in
/// `--settings`, its `env` on top of the child allowlist, and no `--strict-mcp-config` with
/// `allow_local_mcp`; a session elsewhere on the same worker gets none of it.
/// SPEC 0.144: a provider's endpoint, its key and its one model reach the CLI on top of the
/// environment's variables, and the worker's own Anthropic credentials do not; a session
/// without a provider gets the environment as it is, a gateway's `ANTHROPIC_BASE_URL` too.
#[tokio::test]
async fn command_line_of_a_provider() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let cfg = crate::config::Config::parse(&format!(
        r#"hub_url = "https://h"
allowed_roots = [{root:?}]
[environments.gw]
env = {{ ANTHROPIC_BASE_URL = "http://gw.local:8787", ANTHROPIC_API_KEY = "sk-ant-own" }}
[[env_bindings]]
path = "{}"
environment = "gw"
"#,
        root.display()
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
    let var = |env: &[(String, String)], k: &str| {
        env.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone())
    };
    let model = |args: &[String]| {
        let i = args.iter().position(|a| a == "--model")?;
        Some(args[i + 1].clone())
    };
    let mut s = spec(Uuid::now_v7(), &root, Uuid::new_v4());
    s.environment = cfg.environment(&s.workspace);
    s.model = Some("claude-opus-5-5".into());

    // Without a provider: the environment's gateway and key, the session's model.
    let plain = a.command(&s, &Uuid::nil(), Launch::New, None, None);
    assert_eq!(
        var(&plain.env, "ANTHROPIC_BASE_URL").as_deref(),
        Some("http://gw.local:8787")
    );
    assert_eq!(
        var(&plain.env, "ANTHROPIC_API_KEY").as_deref(),
        Some("sk-ant-own")
    );
    assert_eq!(var(&plain.env, "ANTHROPIC_MODEL"), None);
    assert_eq!(model(&plain.args).as_deref(), Some("claude-opus-5-5"));

    s.provider = Some(crate::config::SessionProvider {
        name: "qwen-local".into(),
        base_url: "http://192.168.1.229:8082".into(),
        model: "qwen3.8-27b".into(),
        context_tokens: Some(200_000),
        api_key: None,
        api_key_vault: None,
    });
    let on = a.command(&s, &Uuid::nil(), Launch::New, None, None);
    assert_eq!(
        var(&on.env, "ANTHROPIC_BASE_URL").as_deref(),
        Some("http://192.168.1.229:8082")
    );
    assert_eq!(
        var(&on.env, "ANTHROPIC_API_KEY"),
        None,
        "the worker's key stays"
    );
    assert_eq!(var(&on.env, "ANTHROPIC_AUTH_TOKEN").as_deref(), Some("hqc"));
    for k in [
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_DEFAULT_FABLE_MODEL",
        "ANTHROPIC_SMALL_FAST_MODEL",
        "CLAUDE_CODE_SUBAGENT_MODEL",
    ] {
        assert_eq!(var(&on.env, k).as_deref(), Some("qwen3.8-27b"), "{k}");
    }
    assert_eq!(
        var(&on.env, "CLAUDE_CODE_MAX_CONTEXT_TOKENS").as_deref(),
        Some("200000")
    );
    assert_eq!(model(&on.args).as_deref(), Some("qwen3.8-27b"));
    // Each variable once.
    let mut names: Vec<&str> = on.env.iter().map(|(k, _)| k.as_str()).collect();
    let n = names.len();
    names.dedup();
    assert_eq!(names.len(), n);

    s.provider.as_mut().unwrap().api_key = Some(crate::token::Secret::new("sk-zai"));
    let keyed = a.command(&s, &Uuid::nil(), Launch::New, None, None);
    assert_eq!(
        var(&keyed.env, "ANTHROPIC_AUTH_TOKEN").as_deref(),
        Some("sk-zai")
    );
}

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
        a.command(&s, &Uuid::nil(), Launch::New, None, None)
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

/// SPEC §8.1, §14: a session in a folder whose environment has `project_settings` reads the
/// project setting source too; a session elsewhere on the same worker does not, and a
/// restricted one stays `--restricted` with no `--setting-sources`.
#[tokio::test]
async fn command_line_reads_project_settings_by_the_environment() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    for d in ["mine", "other"] {
        std::fs::create_dir(root.join(d)).unwrap();
    }
    let cfg = crate::config::Config::parse(&format!(
        r#"hub_url = "https://h"
allowed_roots = [{root:?}]
[environments.mine]
project_settings = true
[[env_bindings]]
path = "{}"
environment = "mine"
"#,
        root.join("mine").display()
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
    let args = |dir: &str, restricted: bool| {
        let mut s = spec(Uuid::now_v7(), &root.join(dir), Uuid::new_v4());
        s.environment = cfg.environment(&s.workspace);
        s.restricted = restricted;
        a.command(&s, &Uuid::nil(), Launch::New, None, None).args
    };
    let sources = |args: &[String]| {
        args.iter()
            .position(|a| a == "--setting-sources")
            .map(|i| args[i + 1].clone())
    };

    assert_eq!(
        sources(&args("mine", false)).as_deref(),
        Some("user,project,local")
    );
    assert_eq!(
        sources(&args("other", false)).as_deref(),
        Some("user,local")
    );
    let restricted = args("mine", true);
    assert!(
        restricted.contains(&"--restricted".to_string()),
        "{restricted:?}"
    );
    assert_eq!(sources(&restricted), None, "{restricted:?}");
}

/// SPEC 0.129: a session in allow_all starts with `--permission-mode bypassPermissions`; with
/// `[permissions] allow_all` every process of a session that is not restricted gets
/// `--allow-dangerously-skip-permissions`, so that a switch to allow_all takes (the CLI
/// refuses the flag together with `--restricted`); without it none does, `allow_auto` or not.
#[test]
fn allow_all_starts_in_bypass_permissions() {
    use crate::permissions::{PermissionMode, Permissions};
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let claude = ScriptedClaude::new("");
    let f = ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude));
    let a = ClaudeCodeAdapter {
        session_id: Uuid::nil(),
        opts: f.opts.clone(),
        spawner: f.spawner.clone(),
        events: mpsc::channel(1).0,
        proc: None,
    };
    let mut s = spec(Uuid::now_v7(), work.path(), Uuid::new_v4());
    let args = |s: &StartSpec| a.command(s, &Uuid::nil(), Launch::New, None, None).args;
    let mode = |args: &[String]| {
        let i = args.iter().position(|a| a == "--permission-mode").unwrap();
        args[i + 1].clone()
    };
    let allows = |args: &[String]| {
        args.iter()
            .any(|a| a == "--allow-dangerously-skip-permissions")
    };
    let modes = [
        (PermissionMode::Ask, "default"),
        (PermissionMode::Auto, "auto"),
        (PermissionMode::AllowAll, "bypassPermissions"),
    ];
    // The default: allow_auto on, allow_all off.
    for (m, want) in modes {
        s.permission_mode = m;
        let line = args(&s);
        assert_eq!(mode(&line), want);
        assert!(!allows(&line), "{m:?}: not by default, allow_auto alone");
    }
    s.environment = with_permissions(Permissions {
        allow_all: true,
        ..Permissions::default()
    });
    for (m, _) in modes {
        s.permission_mode = m;
        assert!(allows(&args(&s)), "{m:?}: with allow_all");
    }
    s.restricted = true;
    assert!(!allows(&args(&s)), "never with --restricted");
    s.restricted = false;
    s.environment = with_permissions(Permissions {
        allow_auto: false,
        allow_all: true,
        ..Permissions::default()
    });
    assert!(allows(&args(&s)), "allow_all without allow_auto");
}

/// SPEC 0.129: allow_all is `bypassPermissions` both ways: in our `set_permission_mode` and in
/// what the CLI reports.
#[test]
fn allow_all_switches_as_bypass_permissions() {
    use crate::permissions::PermissionMode;
    let mut core = Core::new(Uuid::now_v7(), false);
    let Action::Write(v) = core.set_permission_mode(PermissionMode::AllowAll) else {
        panic!("not a write");
    };
    assert_eq!(
        v["request"],
        json!({"subtype": "set_permission_mode", "mode": "bypassPermissions"})
    );
    let status = r#"{"type":"system","subtype":"status","status":null,"permissionMode":"bypassPermissions"}"#;
    assert_eq!(
        core.on_line(status),
        [Action::Emit(AdapterEvent::PermissionMode(
            PermissionMode::AllowAll
        ))]
    );
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
/// The answer is no permission mode; a refusal is a line (SPEC 0.179).
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
    let ok = r#"{"type":"control_response","response":{"subtype":"success","request_id":"hqc-model-1","response":{"model":"claude-sonnet-5-5"}}}"#;
    assert!(core.on_line(ok).is_empty(), "{ok}");
    let a = core.on_line(
        r#"{"type":"control_response","response":{"subtype":"error","request_id":"hqc-model-2","error":"Unknown model"}}"#,
    );
    let [Action::Emit(AdapterEvent::Event(e))] = a.as_slice() else {
        panic!("{a:?}");
    };
    assert!(
        matches!(&e.kind, EventKind::System { code, text, model: None, .. }
            if code == "model_not_switched" && text == "Model not switched: Unknown model"),
        "{:?}",
        e.kind
    );
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
    let claude = ScriptedClaude::new(&stream).answering_interrupts();
    let mut h = Harness::start_with(claude, fx.claude_session()).await;
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
            human: false,
        })
        .await
        .unwrap();
    assert_eq!(
        h.next().await,
        AdapterEvent::InputAccepted { event_id: input_id }
    );
    assert_eq!(shape(&[h.next().await]), ["delta"]);
    h.adapter.stop().await.unwrap();
    // The scripted CLI answers the interrupt but the turn does not end; after `stop_grace` the group gets SIGINT,
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
            human: false,
        })
        .await
        .is_err());
    h.adapter.kill().await.unwrap();
    assert_eq!(h.claude.signals(), [Sig::Interrupt]);
}

/// A process stuck in a tool call that SIGINT does not reach: after another `stop_grace` the
/// group gets SIGTERM, the process dies, and Stop has stopped the session.
#[tokio::test]
async fn stop_escalates_to_sigterm_when_sigint_does_not_end_the_turn() {
    let fx = fixture!("permission-allow");
    let stream: String = fx
        .stream
        .lines()
        .filter(|l| l.contains("\"isReplay\": true") || l.contains("\"text_delta\""))
        .map(|l| format!("{l}\n"))
        .collect();
    let claude = ScriptedClaude::new(&stream)
        .answering_interrupts()
        .ignoring(&[Sig::Interrupt]);
    let mut h = Harness::start_with(claude, fx.claude_session()).await;
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
            human: false,
        })
        .await
        .unwrap();
    assert_eq!(
        h.next().await,
        AdapterEvent::InputAccepted { event_id: input_id }
    );
    assert_eq!(shape(&[h.next().await]), ["delta"]);
    h.adapter.stop().await.unwrap();
    let mut last = h.next().await;
    while matches!(last, AdapterEvent::Event(_)) {
        last = h.next().await;
    }
    assert!(
        matches!(last, AdapterEvent::Exited { error: Some(_) }),
        "{last:?}"
    );
    assert_eq!(h.claude.signals(), [Sig::Interrupt, Sig::Terminate]);
}

/// A CLI that does not answer the interrupt is hung: the group gets SIGTERM after
/// `interrupt_answer`, far before the `stop_grace` that an answering CLI gets.
#[tokio::test]
async fn stop_kills_a_cli_that_does_not_answer_the_interrupt() {
    let fx = fixture!("permission-allow");
    let mut h = Harness::start_with_options(
        ScriptedClaude::new(&running_turn(&fx)),
        fx.claude_session(),
        |o| {
            o.stop_grace = Duration::from_secs(10);
            o.interrupt_answer = Duration::from_millis(50);
        },
    )
    .await;
    start_the_turn(&mut h).await;
    h.adapter.stop().await.unwrap();
    let mut last = h.next().await;
    while matches!(last, AdapterEvent::Event(_)) {
        last = h.next().await;
    }
    assert!(
        matches!(last, AdapterEvent::Exited { error: Some(_) }),
        "{last:?}"
    );
    assert_eq!(h.claude.signals(), [Sig::Terminate]);
}

/// A CLI that answers the interrupt gets its `stop_grace` to end the turn, no signal before.
#[tokio::test]
async fn an_answered_interrupt_is_waited_for() {
    let fx = fixture!("permission-allow");
    let claude = ScriptedClaude::new(&running_turn(&fx)).answering_interrupts();
    let mut h = Harness::start_with_options(claude, fx.claude_session(), |o| {
        o.stop_grace = Duration::from_secs(10);
        o.interrupt_answer = Duration::from_millis(50);
    })
    .await;
    start_the_turn(&mut h).await;
    h.adapter.stop().await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(h.claude.signals(), []);
}

/// Input the running turn of [`running_turn`] takes, `event_id` its id.
fn another_input(h: &Harness, event_id: Uuid) -> SessionInput {
    SessionInput {
        session_id: h.session_id,
        event_id,
        text: "now".into(),
        artifact_ids: vec![],
        peer_prefix: vec![],
        peer: None,
        permission_cap: None,
        schedule_id: None,
        human: true,
    }
}

/// SPEC 0.219: `inject_now_with` writes the line with `priority: "now"`; `withdraw_input` asks
/// the CLI to cancel that very line (its `uuid`) and tells whether the CLI did, waiting for the
/// answer; a message not written is not withdrawn.
#[tokio::test]
async fn a_message_sent_now_is_withdrawn_by_its_uuid() {
    let fx = fixture!("permission-allow");
    let claude = ScriptedClaude::new(&running_turn(&fx)).answering_cancels(true);
    let mut h = Harness::start_with_options(claude, fx.claude_session(), |_| {}).await;
    start_the_turn(&mut h).await;
    let id = Uuid::now_v7();
    assert!(h
        .adapter
        .inject_now_with(&another_input(&h, id), &[])
        .await
        .unwrap());
    // The scripted CLI reads its stdin in a task of its own.
    let sent = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let line = h
                .claude
                .stdin()
                .into_iter()
                .find(|l| l["priority"] == "now");
            if let Some(line) = line {
                break line;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the line sent now is not on the CLI's stdin");
    assert_eq!(sent["priority"], "now");
    assert_eq!(sent["origin"], json!({"kind": "human"}));
    assert!(h.adapter.withdraw_input(id).await.unwrap());
    let asked = h.claude.stdin().last().cloned().unwrap();
    assert_eq!(asked["request"]["subtype"], "cancel_async_message");
    assert_eq!(asked["request"]["message_uuid"], sent["uuid"]);
    assert!(
        !h.adapter.withdraw_input(id).await.unwrap(),
        "it is gone: nothing to withdraw"
    );
    assert!(!h.adapter.withdraw_input(Uuid::now_v7()).await.unwrap());
}

/// SPEC 0.219, through the adapter: the line of an input the hub marked `human` (the owner's)
/// reaches the CLI with `origin: {kind: "human"}`, the line of one it did not (a message of
/// another session, a room's input, a note of the hub's own: all arrive with `human` false)
/// without it, and each input is a line of its own.
#[tokio::test]
async fn only_the_owners_input_reaches_the_cli_with_origin_human() {
    let fx = fixture!("permission-allow");
    let mut h = Harness::start_with_options(
        ScriptedClaude::new(&running_turn(&fx)),
        fx.claude_session(),
        |_| {},
    )
    .await;
    start_the_turn(&mut h).await;
    let owner = another_input(&h, Uuid::now_v7());
    let mut other = another_input(&h, Uuid::now_v7());
    other.text = "from another session".into();
    other.human = false;
    assert!(h.adapter.inject_with(&owner, &[]).await.unwrap());
    assert!(h.adapter.inject_with(&other, &[]).await.unwrap());
    // The scripted CLI reads its stdin in a task of its own.
    let users = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let users: Vec<Value> = h
                .claude
                .stdin()
                .into_iter()
                .filter(|l| l["type"] == "user")
                .collect();
            if users.len() == 3 {
                break users;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the lines are not on the CLI's stdin");
    let by_text = |text: &str| {
        users
            .iter()
            .find(|l| l["message"]["content"] == json!(text))
            .unwrap_or_else(|| panic!("no line {text:?}: {users:?}"))
    };
    assert_eq!(by_text("now")["origin"], json!({"kind": "human"}));
    assert!(by_text("hi").get("origin").is_none());
    assert!(by_text("from another session").get("origin").is_none());
}

/// The CLI answers `cancelled: false`: the message has been taken, `false`.
#[tokio::test]
async fn a_message_the_cli_will_not_cancel_is_not_withdrawn() {
    let fx = fixture!("permission-allow");
    let claude = ScriptedClaude::new(&running_turn(&fx)).answering_cancels(false);
    let mut h = Harness::start_with_options(claude, fx.claude_session(), |_| {}).await;
    start_the_turn(&mut h).await;
    let id = Uuid::now_v7();
    assert!(h
        .adapter
        .inject_with(&another_input(&h, id), &[])
        .await
        .unwrap());
    assert!(!h.adapter.withdraw_input(id).await.unwrap());
}

/// A CLI that does not answer the cancel is hung: an error after `interrupt_answer`, not a wait
/// for ever and not a guess.
#[tokio::test]
async fn a_cancel_the_cli_does_not_answer_is_an_error() {
    let fx = fixture!("permission-allow");
    let mut h = Harness::start_with_options(
        ScriptedClaude::new(&running_turn(&fx)),
        fx.claude_session(),
        |o| o.interrupt_answer = Duration::from_millis(100),
    )
    .await;
    start_the_turn(&mut h).await;
    let id = Uuid::now_v7();
    assert!(h
        .adapter
        .inject_with(&another_input(&h, id), &[])
        .await
        .unwrap());
    assert!(h.adapter.withdraw_input(id).await.is_err());
    // SPEC 0.219: the input is no longer being withdrawn: it is a written one that the CLI may
    // cancel by itself later, which is then its drop. A second ask is made (and not met with
    // "it is gone", which a withdrawal still marked under way would give).
    assert!(h.adapter.withdraw_input(id).await.is_err());
}

/// SPEC 0.219: a cancel the CLI does not answer in time leaves the input a written one again:
/// when the CLI then cancels it after all (its `cancelled` line comes late), that is a drop like
/// any other, which the turn's end reports as `dropped` so the input is sent again, not the
/// silence of a withdrawal the owner was told had failed.
#[test]
fn an_unanswered_cancel_leaves_a_late_cancelled_input_dropped() {
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
    play(&mut core, &lifecycle(&ub, "queued"));
    let rid = cancel_request(&mut core, b.event_id, &ub);
    // The adapter gave up waiting for the answer.
    core.withdraw_unanswered(&rid);
    // No cancel is under way any more: it is asked for again, and left unanswered again.
    let rid2 = cancel_request(&mut core, b.event_id, &ub);
    core.withdraw_unanswered(&rid2);
    let mut out = play(&mut core, &lifecycle(&ub, "cancelled"));
    out.extend(play(&mut core, &cancel_answer(&rid, true)));
    out.extend(play(
        &mut core,
        &lines_of(&[tool_result("toolu_1"), reply("DONE"), result("success")]),
    ));
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

/// The echo of the prompt and a delta, then silence: a turn that never ends by itself.
fn running_turn(fx: &Fixture) -> String {
    fx.stream
        .lines()
        .filter(|l| l.contains("\"isReplay\": true") || l.contains("\"text_delta\""))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// Sends the input that starts the turn of [`running_turn`] and reads up to its delta.
async fn start_the_turn(h: &mut Harness) {
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
            human: false,
        })
        .await
        .unwrap();
    assert_eq!(
        h.next().await,
        AdapterEvent::InputAccepted { event_id: input_id }
    );
    assert_eq!(shape(&[h.next().await]), ["delta"]);
}

const WRITES_LOST: &str = r#"{"type":"system","subtype":"session_writes_lost","error_code":"ENOSPC","lost":["transcript"],"uuid":"4f0e2d3c-1b2a-4c5d-8e9f-0a1b2c3d4e5f","session_id":"42e1f456-d428-4380-ad8a-473e21ef9ff7"}"#;

/// The system lines of what `h` emitted since, by code.
fn system_codes(h: &mut Harness) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(ev) = h.events.try_recv() {
        if let AdapterEvent::Event(e) = ev {
            if let EventKind::System { code, .. } = e.kind {
                out.push(code);
            }
        }
    }
    out
}

/// A process with nothing running is closed through its stdin: a CLI that exits on it says
/// what it could not write, and gets no signal.
#[tokio::test]
async fn an_idle_close_ends_the_cli_through_its_stdin() {
    let fx = fixture!("permission-allow");
    let claude = ScriptedClaude::new(fx.stream).exiting_on_eof(WRITES_LOST);
    let mut h = Harness::start_with(claude, fx.claude_session()).await;
    h.turn(&fx.prompt(), Uuid::now_v7(), "once", BTreeMap::new())
        .await;
    h.adapter.close().await.unwrap();
    assert_eq!(h.claude.signals(), []);
    assert_eq!(system_codes(&mut h), ["transcript_not_saved"]);
}

/// A CLI that is still there `exit_grace` after its stdin was closed is ended by SIGTERM.
#[tokio::test]
async fn a_close_the_cli_outlives_ends_in_sigterm() {
    let fx = fixture!("permission-allow");
    let mut h = Harness::start(fx.stream, fx.claude_session()).await;
    h.turn(&fx.prompt(), Uuid::now_v7(), "once", BTreeMap::new())
        .await;
    h.adapter.close().await.unwrap();
    assert_eq!(h.claude.signals(), [Sig::Terminate]);
}

/// A turn is not left to finish: a close during one ends the process at once, as `kill` does
/// (a CLI that would exit on its stdin's end is not given the time).
#[tokio::test]
async fn a_close_during_a_turn_is_a_kill() {
    let fx = fixture!("permission-allow");
    let claude = ScriptedClaude::new(&running_turn(&fx)).exiting_on_eof(WRITES_LOST);
    let mut h = Harness::start_with(claude, fx.claude_session()).await;
    start_the_turn(&mut h).await;
    h.adapter.close().await.unwrap();
    assert_eq!(h.claude.signals(), [Sig::Terminate]);
}

/// `system/session_writes_lost` (CLI 2.1.317) is a line when the transcript is among what
/// was lost, worded by the error code; the global config alone is not.
#[test]
fn a_lost_transcript_write_is_a_feed_line() {
    let line = |code: &str, lost: &str| {
        format!(
            r#"{{"type":"system","subtype":"session_writes_lost","error_code":"{code}","lost":{lost},"uuid":"4f0e2d3c-1b2a-4c5d-8e9f-0a1b2c3d4e5f"}}"#
        )
    };
    let mut core = Core::new(Uuid::now_v7(), false);
    let lines = |core: &mut Core, l: String| system_events(&core.on_line(&l));
    assert_eq!(
        lines(&mut core, line("ENOSPC", r#"["transcript","global_config"]"#)),
        [(
            "transcript_not_saved".to_owned(),
            "Transcript not saved: the disk is full (ENOSPC); resuming this session may miss part of its conversation. Free up space on the worker's disk".to_owned()
        )]
    );
    assert_eq!(
        lines(&mut core, line("EDQUOT", r#"["transcript"]"#)),
        [(
            "transcript_not_saved".to_owned(),
            "Transcript not saved: the disk quota is used up (EDQUOT); resuming this session may miss part of its conversation. Free up space or raise the quota on the worker's disk".to_owned()
        )]
    );
    assert_eq!(
        lines(&mut core, line("EIO", r#"["transcript"]"#))[0].1,
        "Transcript not saved: a write to the worker's disk failed (EIO); resuming this session may miss part of its conversation. Check the worker's disk"
    );
    assert_eq!(
        lines(
            &mut core,
            r#"{"type":"system","subtype":"session_writes_lost","lost":["transcript"]}"#.to_owned()
        )[0]
            .1,
        "Transcript not saved: a write to the worker's disk failed; resuming this session may miss part of its conversation. Check the worker's disk"
    );
    assert!(lines(&mut core, line("ENOSPC", r#"["global_config"]"#)).is_empty());
}

/// SPEC §8.1: every process, a new one and a resumed one, gets `initialize` with only
/// `perTaskStopAffordance: true` as the first line of its stdin, before the prompt: the CLI
/// takes it only before the first user message, and with it Stop spares the background
/// subagents.
#[tokio::test]
async fn every_process_gets_initialize_first() {
    let fx = fixture!("permission-allow");
    let mut h = Harness::start(fx.stream, fx.claude_session()).await;
    h.turn(&fx.prompt(), Uuid::now_v7(), "once", BTreeMap::new())
        .await;
    h.adapter.kill().await.unwrap();
    let (work, config) = &h._dirs;
    let t = super::paths::transcript_path(
        config.path(),
        &work.path().canonicalize().unwrap(),
        &fx.claude_session(),
    );
    std::fs::create_dir_all(t.parent().unwrap()).unwrap();
    std::fs::write(&t, fx.transcript).unwrap();
    let s = spec(h.session_id, work.path(), fx.claude_session());
    h.adapter.resume(&s).await.unwrap();
    assert!(h.claude.commands()[1]
        .args
        .contains(&"--resume".to_string()));
    let inits = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let i = h.claude.initialize();
            if i.len() == 2 {
                return i;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("no initialize on the stdin of both processes");
    // Before the first process's prompt, and before anything else of the second.
    assert_eq!(inits[0].0, 0);
    assert_eq!(inits[1].0, h.claude.stdin().len());
    for (_, v) in &inits {
        assert_eq!(v["type"], "control_request");
        assert_eq!(
            v["request"],
            json!({"subtype": "initialize", "perTaskStopAffordance": true})
        );
        assert!(v["request_id"].as_str().unwrap().starts_with("hqc-init-"));
    }
    assert_ne!(inits[0].1["request_id"], inits[1].1["request_id"]);
    h.adapter.kill().await.unwrap();
}

/// The CLI's answer to `initialize` (2.1.312, shape of a live answer: `{name, description,
/// argumentHint, aliases?, builtin?}`) gives the commands with the first line of each
/// description and the argument hint, before any `system/init`; an error gives nothing. Its
/// list wins over the names of `system/init`, whose `terminal_slash_commands` it loses, and
/// `commands_changed` replaces it.
#[test]
fn the_answer_to_initialize_gives_the_commands() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let init = writes(&[core.initialize()]);
    let id = init[0]["request_id"].as_str().unwrap();
    let error = json!({"type": "control_response", "response": {"subtype": "error", "request_id": id,
        "error": "initialize: skills must be an array of strings"}});
    assert!(core.on_line(&error.to_string()).is_empty());
    let long = "x".repeat(300);
    let answer = json!({"type": "control_response", "response": {"subtype": "success", "request_id": id,
        "response": {"commands": [
            {"name": "compact", "description": "Clear conversation history but keep a summary in context. Optional: /compact [instructions for summarization]", "argumentHint": "<optional custom summarization instructions>", "builtin": true},
            {"name": "claude-api", "description": format!("Build apps with the Claude API.\nTRIGGER when: {long}"), "argumentHint": ""},
            {"name": "doctor", "description": "Health-check the user's Claude Code setup", "argumentHint": "[prompt-audit [<path>]]", "aliases": ["checkup"], "builtin": true},
            {"name": "__remote-workflow", "description": "Run the workflow script", "argumentHint": "", "builtin": true},
            {"name": "long", "description": long, "argumentHint": ""}],
            "agents": [], "output_style": "default", "models": [], "account": {}, "pid": 4242},
        "pending_permission_requests": [], "pending_user_dialog_requests": []}});
    let one = |name: &str, description: Option<&str>, argument_hint: Option<&str>| SlashCommand {
        name: name.into(),
        description: description.map(Into::into),
        argument_hint: argument_hint.map(Into::into),
    };
    let reported = |actions: &[Action]| -> Vec<Vec<SlashCommand>> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Emit(AdapterEvent::Commands(c)) => Some(c.clone()),
                _ => None,
            })
            .collect()
    };
    let compact = one(
        "compact",
        Some("Clear conversation history but keep a summary in context. Optional: /compact [instructions for summarization]"),
        Some("<optional custom summarization instructions>"),
    );
    let api = one("claude-api", Some("Build apps with the Claude API."), None);
    let doctor = one(
        "doctor",
        Some("Health-check the user's Claude Code setup"),
        Some("[prompt-audit [<path>]]"),
    );
    let cut = one("long", Some(&long[..COMMAND_TEXT_LIMIT]), None);
    assert_eq!(
        reported(&core.on_line(&answer.to_string())),
        [vec![compact.clone(), api.clone(), doctor, cut.clone()]],
        "before any turn"
    );
    let init = json!({"type": "system", "subtype": "init", "session_id": CLI_SESSION,
        "model": "claude-haiku-5-5", "permissionMode": "default",
        "slash_commands": ["compact", "claude-api", "doctor", "__remote-workflow", "long", "other"],
        "terminal_slash_commands": ["doctor"]});
    assert_eq!(
        reported(&core.on_line(&init.to_string())),
        [vec![compact.clone(), api.clone(), cut.clone()]],
        "the terminal's own go; the names of init add nothing"
    );
    let changed = json!({"type": "system", "subtype": "commands_changed",
        "commands": [{"name": "compact", "description": "Compact", "argumentHint": ""},
            {"name": "new-skill", "description": "A skill found in a subfolder", "argumentHint": "<file>"}],
        "uuid": Uuid::new_v4(), "session_id": CLI_SESSION});
    let list = vec![
        one("compact", Some("Compact"), None),
        one(
            "new-skill",
            Some("A skill found in a subfolder"),
            Some("<file>"),
        ),
    ];
    assert_eq!(reported(&core.on_line(&changed.to_string())), [list]);
    assert!(
        core.on_line(&changed.to_string()).is_empty(),
        "the same list: not reported again"
    );
}

/// The commands reported are those the hub keeps (SPEC §5.4): a name longer than 100
/// characters is dropped and the list stops at 500, so a CLI with more does not make the frame
/// the hub refuses.
#[test]
fn the_commands_reported_are_what_the_hub_keeps() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let mut names = vec!["x".repeat(101), "y".repeat(100)];
    names.extend((0..600).map(|i| format!("c{i}")));
    let changed = json!({"type": "system", "subtype": "commands_changed",
        "commands": names.iter().map(|n| json!({"name": n, "description": "", "argumentHint": ""}))
            .collect::<Vec<_>>(),
        "uuid": Uuid::new_v4(), "session_id": CLI_SESSION});
    let lists: Vec<Vec<SlashCommand>> = emitted(core.on_line(&changed.to_string()))
        .into_iter()
        .filter_map(|e| match e {
            AdapterEvent::Commands(c) => Some(c),
            _ => None,
        })
        .collect();
    let [list] = lists.as_slice() else {
        panic!("{} lists", lists.len())
    };
    assert_eq!(list.len(), 500);
    assert_eq!(list[0].name, "y".repeat(100));
    assert_eq!(list[499].name, "c498");
}

/// A new conversation, whose title the ticker reads during its first model turn: a new session's
/// or the one `/clear` starts, not a resumed one or a fork; `/clear`'s own result does not end
/// that turn.
#[test]
fn a_new_conversation_lasts_until_its_first_model_turn_ends() {
    assert!(Launch::New.new_conversation());
    assert!(!Launch::Resume.new_conversation());
    assert!(!Launch::Fork { at: Uuid::nil() }.new_conversation());
    let sid = Uuid::now_v7();
    let result = |local: Option<&str>| {
        let mut v = json!({"type": "result", "subtype": "success", "is_error": false,
            "num_turns": 1, "total_cost_usd": 0.0, "session_id": "s", "uuid": Uuid::new_v4()});
        if let Some(l) = local {
            v["local_command"] = json!(l);
        }
        v.to_string()
    };
    assert!(!Core::new(sid, false).fresh(), "a resumed conversation");
    let mut core = Core::new(sid, false).with_new_conversation(true);
    assert!(core.fresh());
    core.on_line(&result(None));
    assert!(!core.fresh(), "the first model turn ended");
    core.on_line(
        &json!({"type": "conversation_reset", "trigger": "clear", "session_id": "s"}).to_string(),
    );
    assert!(core.fresh(), "/clear starts a new conversation");
    core.on_line(&result(Some("clear")));
    assert!(core.fresh(), "/clear's own result is no model turn");
    core.on_line(&result(None));
    assert!(!core.fresh());
}

/// `/clear` (the lines of the live run `w1-clear`, 2.1.312): `conversation_reset` says
/// "Conversation cleared" and starts the cost from 0, as the CLI's `total_cost_usd` does; the
/// first `system/init` after it, with the new conversation's id, moves the session to that
/// transcript, once.
#[test]
fn clear_moves_the_session_to_a_new_transcript() {
    const OLD: &str = "8ff09d1d-af14-46c9-ac99-63cd19302d2c";
    const NEW: &str = "4d4b2f4f-a1f2-47a3-9438-c1d3d9733c6f";
    let init = |session: &str| {
        json!({"type": "system", "subtype": "init", "session_id": session,
            "model": "claude-haiku-5-5", "permissionMode": "bypassPermissions",
            "slash_commands": ["clear", "compact"], "uuid": Uuid::new_v4()})
        .to_string()
    };
    let echo_in = |session: &str, text: &str| {
        json!({"type": "user", "message": {"role": "user", "content": text},
            "session_id": session, "parent_tool_use_id": null, "uuid": Uuid::new_v4(),
            "timestamp": "2026-11-11T22:34:40.295Z", "isReplay": true})
        .to_string()
    };
    let result = |session: &str, total: f64, tokens: u64, local: Option<&str>| {
        let mut v = json!({"type": "result", "subtype": "success", "is_error": false,
            "num_turns": 1, "total_cost_usd": total, "session_id": session,
            "usage": {"input_tokens": tokens, "output_tokens": tokens}, "uuid": Uuid::new_v4()});
        if let Some(l) = local {
            v["local_command"] = json!(l);
        }
        v.to_string()
    };
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let mut actions = Vec::new();
    written(&mut core, sid, "Reply with exactly: ONE");
    for l in [
        init(OLD),
        echo_in(OLD, "Reply with exactly: ONE"),
        reply("ONE"),
        result(OLD, 0.00232649, 3, None),
    ] {
        actions.extend(core.on_line(&l));
    }
    written(&mut core, sid, "/clear");
    let reset = json!({"type": "conversation_reset", "new_conversation_id": "45db54bd-8b7d-44bd-a9a9-a73c5d0f6bf4",
        "uuid": "45db54bd-8b7d-44bd-a9a9-a73c5d0f6bf4", "trigger": "clear",
        "user_message_uuid": Uuid::new_v4(), "timestamp": "2026-11-11T22:34:41.884Z", "session_id": OLD});
    for l in [
        reset.to_string(),
        init(NEW),
        echo_in(
            NEW,
            "<command-name>/clear</command-name>\n            <command-message>clear</command-message>\n            <command-args></command-args>",
        ),
        result(NEW, 0.0, 0, Some("clear")),
    ] {
        actions.extend(core.on_line(&l));
    }
    written(&mut core, sid, "What did I ask you to reply before?");
    for l in [
        init(NEW),
        echo_in(NEW, "What did I ask you to reply before?"),
        reply("NOTHING TWO"),
        result(NEW, 0.00232689, 3, None),
    ] {
        actions.extend(core.on_line(&l));
    }
    assert_eq!(
        system_events(&actions),
        [(
            CONVERSATION_CLEARED.to_string(),
            CONVERSATION_CLEARED_TEXT.to_string()
        )]
    );
    let moved: Vec<&AdapterEvent> = actions
        .iter()
        .filter_map(|a| match a {
            Action::Emit(e @ AdapterEvent::RuntimeSessionId(_)) => Some(e),
            _ => None,
        })
        .collect();
    assert_eq!(moved, [&AdapterEvent::RuntimeSessionId(NEW.into())]);
    assert_eq!(
        costs_and_commands(&actions).0,
        [(0.00232649, 0.00232649), (0.00232689, 0.00232689)],
        "the turn after /clear costs what the new total says"
    );
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
    // `always`, not offered in restricted mode, is refused and the request stays open.
    assert!(core.answer_approval("r1", "always", None).is_err());
    assert_eq!(core.session_rules(), 0);
    let w = writes(&core.answer_approval("r1", "session", None).unwrap());
    assert!(w[0]["response"]["response"]
        .get("updatedPermissions")
        .is_none());
    assert_eq!(core.session_rules(), 2);
    // Answered: a second answer is an error.
    assert!(core.answer_approval("r1", "once", None).is_err());
}

/// A request carries its call's arguments also when a subagent asks (SPEC 0.207), and an
/// option's grant is the rules the answer acts on: restricted, `session` still lists them and
/// there is no `always`.
#[test]
fn a_request_carries_the_call_and_the_grants() {
    let fx = fixture!("permission-allow");
    let mut v = lines(fx.stream)
        .into_iter()
        .find(|v| v["type"] == "control_request")
        .unwrap();
    v["request_id"] = json!("r1");
    v["request"]["agent_id"] = json!("a8b8fdc3b4a031d4f");
    let want = args_preview("Bash", &v["request"]["input"]);
    let mut core = Core::new(Uuid::now_v7(), true);
    let a = core.on_line(&v.to_string());
    let (args, options) = match &a[0] {
        Action::Emit(AdapterEvent::Event(e)) => match &e.kind {
            EventKind::ApprovalRequest {
                args_preview,
                options,
                ..
            } => (args_preview.clone(), options.clone()),
            _ => panic!(),
        },
        _ => panic!(),
    };
    assert_eq!(args, Some(want));
    assert!(options.iter().all(|o| o.id != "always"));
    assert_eq!(
        grant_of(&options, "session").unwrap().rules,
        ["Bash(touch hqc-marker-once *)"]
    );
}

fn policy(allow: &[&str], deny: &[&str]) -> Arc<crate::permissions::Policy> {
    Arc::new(crate::permissions::Policy::new(
        &crate::permissions::Permissions {
            mode: crate::permissions::PermissionMode::Ask,
            allow_auto: true,
            allow_all: false,
            allow: allow.iter().map(|r| r.to_string()).collect(),
            deny: deny.iter().map(|r| r.to_string()).collect(),
            remote_unblock: Default::default(),
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

/// An MCP tool's approval offers "all tools of this server" for the session, and as a folder
/// rule when the session has a folder and is not restricted; answering the session one allows
/// the server's other tools without a card and not another server's.
#[test]
fn an_mcp_server_is_allowed_whole() {
    let mcp = |id: &str, tool: &str| {
        json!({"type": "control_request", "request_id": id, "request": {
            "subtype": "can_use_tool", "tool_name": tool, "input": {}}})
        .to_string()
    };
    let with_folder = |restricted: bool| {
        Core::new(Uuid::now_v7(), restricted).with_folder(
            crate::permissions::FolderRules::default(),
            std::path::PathBuf::from("/w/proj"),
        )
    };
    let ids = |core: &mut Core, tool: &str| approval_of(&core.on_line(&mcp("r", tool))).0;

    let mut core = with_folder(false);
    assert_eq!(
        ids(&mut core, "mcp__srv__one"),
        ["once", "session", "server_session", "server_always", "deny"]
    );
    // Not an MCP tool, no folder, restricted: fewer options.
    assert!(!ids(&mut core, "Edit")
        .iter()
        .any(|i| i.starts_with("server_")));
    let mut core = Core::new(Uuid::now_v7(), false);
    assert_eq!(
        ids(&mut core, "mcp__srv__one"),
        ["once", "session", "server_session", "deny"]
    );
    let mut core = with_folder(true);
    assert_eq!(
        ids(&mut core, "mcp__srv__one"),
        ["once", "session", "server_session", "deny"]
    );
    // A request only a person may answer offers no standing answer (SPEC §8.1).
    let mut core = with_folder(false);
    let line = json!({"type": "control_request", "request_id": "h", "request": {
        "subtype": "can_use_tool", "tool_name": "mcp__srv__one", "input": {},
        "requires_user_interaction": true}})
    .to_string();
    assert_eq!(approval_of(&core.on_line(&line)).0, ["once", "deny"]);
    // An option the card did not offer is refused and the request stays open.
    for option in ["session", "server_session", "server_always", "always"] {
        assert!(core.answer_approval("h", option, None).is_err(), "{option}");
    }
    assert_eq!(core.session_rules(), 0);
    assert!(core.answer_approval("h", "once", None).is_ok());
    let mut core = with_folder(true);
    ids(&mut core, "mcp__srv__one");
    assert!(core.answer_approval("r", "server_always", None).is_err());
    assert!(core.answer_approval("r", "server_session", None).is_ok());
    // A name another server's tool could share (`hqc_`'s `x`, `a__b`'s `x`) offers no
    // answer for the whole server.
    let mut core = with_folder(false);
    for tool in ["mcp__hqc___x", "mcp___a__x", "mcp__a__b__x"] {
        assert_eq!(ids(&mut core, tool), ["once", "session", "deny"], "{tool}");
        assert!(core.answer_approval("r", "server_session", None).is_err());
        assert!(core.answer_approval("r", "deny", None).is_ok());
    }

    let mut core = with_folder(false);
    ids(&mut core, "mcp__srv__one");
    let a = core.answer_approval("r", "server_session", None).unwrap();
    assert_eq!(writes(&a)[0]["response"]["response"]["behavior"], "allow");
    let a = core.on_line(&mcp("r2", "mcp__srv__two"));
    assert_eq!(approvals(&a), 0);
    assert_eq!(writes(&a)[0]["response"]["response"]["behavior"], "allow");
    assert_eq!(approvals(&core.on_line(&mcp("r3", "mcp__srv2__two"))), 1);
    assert_eq!(approvals(&core.on_line(&mcp("r4", "mcp__other__one"))), 1);

    // `server_always` is allowed the same way; an answer for a tool that is not an MCP one
    // is refused and the request stays open.
    let mut core = with_folder(false);
    ids(&mut core, "mcp__srv__one");
    core.answer_approval("r", "server_always", None).unwrap();
    assert_eq!(approvals(&core.on_line(&mcp("r2", "mcp__srv__two"))), 0);
    ids(&mut core, "Edit");
    assert!(core.answer_approval("r", "server_session", None).is_err());
    assert!(core.answer_approval("r", "once", None).is_ok());
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
    let w = writes(&core.answer_clarify(rid, &BTreeMap::new(), None).unwrap());
    assert_eq!(w[0]["response"]["response"]["behavior"], "deny");
    assert!(core.answer_clarify(rid, &BTreeMap::new(), None).is_err());

    core.on_line(&req.to_string());
    let answers = BTreeMap::from([
        (
            "1".to_string(),
            ClarifyAnswer::Many(vec!["Hub (Recommended)".into(), "Worker".into()]),
        ),
        ("7".to_string(), ClarifyAnswer::One("ignored".into())),
    ]);
    let w = writes(&core.answer_clarify(rid, &answers, None).unwrap());
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
    let w = writes(&core.answer_clarify("q1", &answers, None).unwrap());
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

/// `can_use_tool_line` with `patch` applied to its `request`.
fn patched(fx: &Fixture, request_id: &str, patch: impl FnOnce(&mut Value)) -> String {
    let mut v: Value = serde_json::from_str(&can_use_tool_line(fx, request_id, None)).unwrap();
    patch(&mut v["request"]);
    v.to_string()
}

/// The `approval.request` of `actions` as on the wire.
fn approval_wire(actions: &[Action]) -> Value {
    actions
        .iter()
        .find_map(|a| match a {
            Action::Emit(AdapterEvent::Event(e))
                if matches!(e.kind, EventKind::ApprovalRequest { .. }) =>
            {
                Some(serde_json::to_value(e).unwrap()["payload"].clone())
            }
            _ => None,
        })
        .expect("approval.request")
}

/// SPEC 0.180: a request the CLI gives only to a person (`requires_user_interaction`,
/// `matched_ask_rule`, `suppress_always_allow_rule`) reaches the card although a session rule
/// or a folder rule covers the call, with `human_only`, only `once` and `deny`, and no rule
/// suggested; the others go as before.
#[test]
fn a_request_only_a_person_may_answer_reaches_the_card() {
    let fx = fixture!("permission-allow");
    let only: [(&str, Value); 3] = [
        ("requires_user_interaction", json!(true)),
        (
            "matched_ask_rule",
            json!({"source": "projectSettings", "tool_name": "Bash"}),
        ),
        ("suppress_always_allow_rule", json!(true)),
    ];

    // A session rule.
    let mut core = Core::new(Uuid::now_v7(), false);
    core.on_line(&can_use_tool_line(&fx, "r0", None));
    core.answer_approval("r0", "session", None).unwrap();
    assert_eq!(
        approvals(&core.on_line(&can_use_tool_line(&fx, "r1", None))),
        0
    );
    for (i, (k, v)) in only.iter().enumerate() {
        let a = core.on_line(&patched(&fx, &format!("s{i}"), |r| r[*k] = v.clone()));
        assert_eq!((approvals(&a), writes(&a).len()), (1, 0), "{k}");
        let p = approval_wire(&a);
        assert_eq!(p["human_only"], true, "{k}");
        let ids: Vec<&str> = p["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["once", "deny"], "{k}");
    }

    // A folder rule.
    let rules = crate::permissions::FolderRules::default();
    rules.set(&["Bash(touch *)".into()]);
    let mut core =
        Core::new(Uuid::now_v7(), false).with_folder(rules, std::path::PathBuf::from("/w/proj"));
    let a = core.on_line(&can_use_tool_line(&fx, "f0", None));
    assert_eq!((approvals(&a), writes(&a).len()), (0, 1));
    for (i, (k, v)) in only.iter().enumerate() {
        let a = core.on_line(&patched(&fx, &format!("f{}", i + 1), |r| r[*k] = v.clone()));
        assert_eq!((approvals(&a), writes(&a).len()), (1, 0), "{k}");
        let p = approval_wire(&a);
        assert_eq!(p["human_only"], true, "{k}");
        assert!(p.get("rule_suggestions").is_none(), "{k}: {p}");
    }
    // Any other request is no person's only.
    let mut plain = Core::new(Uuid::now_v7(), false);
    let p = approval_wire(&plain.on_line(&can_use_tool_line(&fx, "p1", None)));
    assert!(p.get("human_only").is_none(), "{p}");
}

/// SPEC 0.180: the card carries why the CLI asks (`decision_reason`) and its name of the tool
/// (`display_name`), cut to 1024 and 256 bytes; blank ones are left out.
#[test]
fn approval_carries_the_reason_and_the_tools_name() {
    let fx = fixture!("permission-allow");
    let mut core = Core::new(Uuid::now_v7(), false);
    let p = approval_wire(&core.on_line(&patched(&fx, "r1", |r| {
        r["decision_reason"] = json!("Permission rule 'Bash(touch:*)' requires approval");
        r["display_name"] = json!("Shell");
    })));
    assert_eq!(
        (&p["reason"], &p["display_name"]),
        (
            &json!("Permission rule 'Bash(touch:*)' requires approval"),
            &json!("Shell")
        )
    );
    let p = approval_wire(&core.on_line(&patched(&fx, "r2", |r| {
        r["decision_reason"] = json!("é".repeat(1000));
        r["display_name"] = json!("x".repeat(300));
    })));
    assert_eq!(p["reason"].as_str().unwrap().len(), 1024);
    assert_eq!(p["display_name"].as_str().unwrap().len(), 256);
    let p = approval_wire(&core.on_line(&patched(&fx, "r3", |r| {
        r["decision_reason"] = json!(" ");
        r.as_object_mut().unwrap().remove("display_name");
    })));
    assert!(
        p.get("reason").is_none() && p.get("display_name").is_none(),
        "{p}"
    );
}

/// SPEC 0.217: for a call an ask rule caught, Claude Code sends no `decision_reason`, only
/// `decision_reason_type: "rule"` and the rule's source (2.1.329, also in `bypassPermissions`),
/// so the card says that an ask rule for the tool asks and where it lives; `matched_ask_rule`
/// names the rule itself; a `decision_reason` of the CLI still wins.
#[test]
fn an_ask_rule_names_itself_on_the_card() {
    let fx = fixture!("permission-allow");
    let mut core = Core::new(Uuid::now_v7(), false);
    // The request as 2.1.329 sent it for an `Artifact` call under the ask rule `Artifact`.
    let p = approval_wire(&core.on_line(&patched(&fx, "r1", |r| {
        r["decision_reason_type"] = json!("rule");
        r["decision_reason_rule_source"] = json!("userSettings");
        r["suppress_always_allow_rule"] = json!(true);
    })));
    let tool = p["tool"].as_str().unwrap().to_owned();
    assert_eq!(
        p["reason"],
        json!(format!(
            "An ask rule for {tool} in user settings (~/.claude/settings.json) requires approval for this call"
        ))
    );
    let p = approval_wire(&core.on_line(&patched(&fx, "r2", |r| {
        r["decision_reason_type"] = json!("rule");
        r["decision_reason_rule_source"] = json!("projectSettings");
        r["matched_ask_rule"] = json!({"source": "localSettings", "tool_name": "Bash",
            "rule_content": "git push:*"});
    })));
    assert_eq!(
        p["reason"],
        json!("The ask rule Bash(git push:*) in project local settings (.claude/settings.local.json) requires approval for this call")
    );
    // A tool's own ask with the rule beside it (`subcommandResults`, `mode`), a rule on the
    // whole tool.
    let p = approval_wire(&core.on_line(&patched(&fx, "r5", |r| {
        r["decision_reason_type"] = json!("subcommandResults");
        r["matched_ask_rule"] = json!({"source": "userSettings", "tool_name": "Bash"});
    })));
    assert_eq!(
        p["reason"],
        json!("The ask rule Bash in user settings (~/.claude/settings.json) requires approval for this call")
    );
    let p = approval_wire(&core.on_line(&patched(&fx, "r3", |r| {
        r["decision_reason_type"] = json!("rule");
        r["decision_reason"] = json!("A hook asks");
    })));
    assert_eq!(p["reason"], json!("A hook asks"));
    let p = approval_wire(&core.on_line(&patched(&fx, "r4", |r| {
        r["decision_reason_type"] = json!("mode");
        r["decision_reason_rule_source"] = json!("userSettings");
    })));
    assert!(p.get("reason").is_none(), "{p}");
}

/// `approval.cancel` / `clarify.cancel` events of `actions`: kind, request id, reason.
fn cancels(actions: &[Action]) -> Vec<(&'static str, String, CancelReason)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Emit(AdapterEvent::Event(e)) => match &e.kind {
                EventKind::ApprovalCancel { request_id, reason } => {
                    Some(("approval", request_id.clone(), *reason))
                }
                EventKind::ClarifyCancel { request_id, reason } => {
                    Some(("clarify", request_id.clone(), *reason))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn withdraw(request_id: &str) -> String {
    json!({"type": "control_cancel_request", "request_id": request_id}).to_string()
}

/// SPEC 0.180: `control_cancel_request` closes the card of the request it names, `withdrawn`
/// (an approval, a question, an MCP server's request), and an answer that comes later writes
/// nothing to the CLI; an id not open here gives nothing.
#[test]
fn a_withdrawn_request_closes_its_card() {
    let fx = fixture!("permission-allow");
    let mut core = Core::new(Uuid::now_v7(), false);
    core.on_line(&can_use_tool_line(&fx, "r1", None));
    let a = core.on_line(&withdraw("r1"));
    assert_eq!(
        cancels(&a),
        [("approval", "r1".to_string(), CancelReason::Withdrawn)]
    );
    assert!(writes(&a).is_empty());
    assert!(core.answer_approval("r1", "once", None).is_err());

    let ask = json!({"type": "control_request", "request_id": "q1", "request": {
        "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "input": {"questions": [
            {"question": "Which?", "options": [{"label": "A"}, {"label": "B"}]}]}}});
    core.on_line(&ask.to_string());
    let a = core.on_line(&withdraw("q1"));
    assert_eq!(
        cancels(&a),
        [("clarify", "q1".to_string(), CancelReason::Withdrawn)]
    );
    let answers = BTreeMap::from([("1".to_string(), ClarifyAnswer::One("A".into()))]);
    assert!(core.answer_clarify("q1", &answers, None).is_err());

    core.on_line(&elicitation("e1", json!({})));
    let a = core.on_line(&withdraw("e1"));
    assert_eq!(
        cancels(&a),
        [("clarify", "e1".to_string(), CancelReason::Withdrawn)]
    );
    assert!(core
        .answer_clarify("e1", &BTreeMap::new(), Some(ClarifyAction::Accept))
        .is_err());

    assert!(core.on_line(&withdraw("e1")).is_empty());
    assert!(core.on_line(&withdraw("nope")).is_empty());
}

/// An open AskUserQuestion `request_id` of the main agent.
fn ask_line(request_id: &str) -> String {
    json!({"type": "control_request", "request_id": request_id, "request": {
        "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "input": {"questions": [
            {"question": "Which?", "options": [{"label": "A"}, {"label": "B"}]}]}}})
    .to_string()
}

/// SPEC 0.180: Stop makes the CLI withdraw every request of the turn it aborts; for an
/// approval and a question of the main agent that is the turn's end, `stopped` (a question is
/// then answered by a message, §10.1), not `withdrawn`; an MCP server's request stays open
/// until answered or withdrawn as it was.
#[test]
fn a_request_withdrawn_by_stop_is_stopped() {
    let fx = fixture!("permission-allow");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    core.on_line(&can_use_tool_line(&fx, "r1", None));
    core.on_line(&ask_line("q1"));
    core.on_line(&elicitation("e1", json!({})));
    core.interrupt().expect("a turn runs");
    let a = core.on_line(&withdraw("q1"));
    assert_eq!(
        cancels(&a),
        [("clarify", "q1".to_string(), CancelReason::Stopped)]
    );
    let a = core.on_line(&withdraw("r1"));
    assert_eq!(
        cancels(&a),
        [("approval", "r1".to_string(), CancelReason::Stopped)]
    );
    let a = core.on_line(&withdraw("e1"));
    assert_eq!(
        cancels(&a),
        [("clarify", "e1".to_string(), CancelReason::Withdrawn)]
    );
}

/// A request of a background subagent outlives the aborted turn: the CLI withdrawing it under
/// Stop is a withdrawal, as the turn's `result` would leave it open.
#[test]
fn a_background_subagents_request_withdrawn_under_stop_is_withdrawn() {
    let fx = fixture!("permission-allow");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    play(
        &mut core,
        r#"{"type":"system","subtype":"task_started","task_id":"a8b8fdc3b4a031d4f","tool_use_id":"toolu_01BR8iav6ZtPUAhpuCWdQPCt","description":"Touch hqc-live-check file","is_backgrounded":true,"task_type":"local_agent","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e13"}"#,
    );
    let mut v: Value =
        serde_json::from_str(&can_use_tool_line(&fx, "bg", Some("touch x"))).unwrap();
    v["request"]["agent_id"] = json!("a8b8fdc3b4a031d4f");
    core.on_line(&v.to_string());
    core.interrupt().expect("a turn runs");
    assert_eq!(
        cancels(&core.on_line(&withdraw("bg"))),
        [("approval", "bg".to_string(), CancelReason::Withdrawn)]
    );
}

/// "No" that interrupts the turn (the main agent's call) makes the CLI withdraw the other
/// requests of the turn too: a parallel question is `stopped`. After the turn's `result` the
/// turn is not being ended any more, and a withdrawal is `withdrawn` again.
#[test]
fn a_request_withdrawn_after_no_is_stopped_until_the_result() {
    let fx = fixture!("permission-allow");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    core.on_line(&can_use_tool_line(&fx, "r1", None));
    core.on_line(&ask_line("q1"));
    core.answer_approval("r1", "deny", None).unwrap();
    assert_eq!(
        cancels(&core.on_line(&withdraw("q1"))),
        [("clarify", "q1".to_string(), CancelReason::Stopped)]
    );
    core.on_line(
        r#"{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":1,"terminal_reason":"aborted_tools","total_cost_usd":0.001,"usage":{"input_tokens":4,"output_tokens":2}}"#,
    );
    started(&mut core, sid);
    core.on_line(&ask_line("q2"));
    assert_eq!(
        cancels(&core.on_line(&withdraw("q2"))),
        [("clarify", "q2".to_string(), CancelReason::Withdrawn)]
    );
}

/// An `elicitation` of the MCP server `deploy`, with `extra` in its request.
fn elicitation(request_id: &str, extra: Value) -> String {
    let mut r = json!({"subtype": "elicitation", "mcp_server_name": "deploy",
        "message": "Where should the release go?"});
    for (k, v) in extra.as_object().unwrap() {
        r[k] = v.clone();
    }
    json!({"type": "control_request", "request_id": request_id, "request": r}).to_string()
}

/// The `clarify.request` of `actions` as on the wire.
fn clarify_wire(actions: &[Action]) -> Value {
    actions
        .iter()
        .find_map(|a| match a {
            Action::Emit(AdapterEvent::Event(e))
                if matches!(e.kind, EventKind::ClarifyRequest { .. }) =>
            {
                Some(serde_json::to_value(e).unwrap()["payload"].clone())
            }
            _ => None,
        })
        .expect("clarify.request")
}

/// SPEC 0.180: an MCP server's form is a question card of the server: its properties in
/// `property_order` (an enum's values as options, a boolean as Yes/No, the rest free text), the
/// title as the header and the description as the text, a property the schema's `required` lists
/// `required`, an integer's or a number's free text with that `input`; the answer goes back
/// typed as `accept`, Decline as `decline`, the hub's cancel as `cancel`. The turn's end leaves
/// it open.
#[test]
fn an_mcp_servers_form_is_a_question_card() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let form = json!({
        "requested_schema": {"type": "object", "required": ["region"], "properties": {
            "keep": {"type": "boolean", "description": "Keep the old release"},
            "note": {"type": "string"},
            "ratio": {"type": "number", "title": "Ratio"},
            "region": {"type": "string", "title": "Region", "description": "The region to deploy to",
                "enum": ["eu-west", "us-east"]},
            "replicas": {"type": "integer", "title": "Replicas"}}},
        "property_order": ["region", "replicas", "keep", "note", "ratio"]});
    let a = core.on_line(&elicitation("e1", form.clone()));
    assert!(writes(&a).is_empty());
    let p = clarify_wire(&a);
    let example: Value = serde_json::from_str(include_str!(
        "../../../../hqc-proto/examples/wire/event.clarify.request.elicitation.json"
    ))
    .unwrap();
    let mut first = p.clone();
    first["questions"] = json!(p["questions"].as_array().unwrap()[..3]);
    first["request_id"] = json!("req-4");
    assert_eq!(first, example["payload"]);
    assert_eq!(
        p["questions"].as_array().unwrap()[3..],
        [
            json!({"id": "4", "text": "note"}),
            json!({"id": "5", "text": "Ratio", "input": "number"})
        ]
    );

    let answers = BTreeMap::from([
        ("1".to_string(), ClarifyAnswer::One("us-east".into())),
        ("2".to_string(), ClarifyAnswer::One(" 3 ".into())),
        ("3".to_string(), ClarifyAnswer::One("No".into())),
        ("4".to_string(), ClarifyAnswer::One("  ".into())),
        ("5".to_string(), ClarifyAnswer::One("0.5".into())),
    ]);
    let w = writes(
        &core
            .answer_clarify("e1", &answers, Some(ClarifyAction::Accept))
            .unwrap(),
    );
    assert_eq!(
        w,
        [
            json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": "e1", "response": {"action": "accept", "content":
                {"region": "us-east", "replicas": 3, "keep": false, "ratio": 0.5}}}})
        ]
    );

    core.on_line(&elicitation("e2", form.clone()));
    let w = writes(
        &core
            .answer_clarify("e2", &BTreeMap::new(), Some(ClarifyAction::Decline))
            .unwrap(),
    );
    assert_eq!(w[0]["response"]["response"], json!({"action": "decline"}));

    core.on_line(&elicitation("e3", form.clone()));
    // A turn's end does not close it: no agent asked it.
    let a = core.on_line(
        r#"{"type":"result","subtype":"success","is_error":false,"num_turns":1,"usage":{"input_tokens":1,"output_tokens":1}}"#,
    );
    assert!(cancels(&a).is_empty(), "{a:?}");
    let w = writes(&core.answer_clarify("e3", &BTreeMap::new(), None).unwrap());
    assert_eq!(w[0]["response"]["response"], json!({"action": "cancel"}));
}

/// SPEC 0.180: an MCP server's link is a card with its `url` and no questions, Done is `accept`
/// without `content`; a form the card cannot ask (a nested object, an array) and a link without
/// a URL are answered `cancel` at once, with a `system elicitation_unsupported` line.
#[test]
fn an_mcp_servers_link_and_a_form_hqc_cannot_ask() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let a = core.on_line(&elicitation(
        "e1",
        json!({"mode": "url", "url": "https://github.com/login/device", "elicitation_id": "x"}),
    ));
    let p = clarify_wire(&a);
    assert_eq!(
        (&p["questions"], &p["url"], &p["server"]),
        (
            &json!([]),
            &json!("https://github.com/login/device"),
            &json!("deploy")
        )
    );
    let w = writes(
        &core
            .answer_clarify("e1", &BTreeMap::new(), Some(ClarifyAction::Accept))
            .unwrap(),
    );
    assert_eq!(w[0]["response"]["response"], json!({"action": "accept"}));

    for (id, extra) in [
        (
            "n1",
            json!({"requested_schema": {"type": "object", "properties": {
                "name": {"type": "string"}, "address": {"type": "object"}}}}),
        ),
        (
            "n2",
            json!({"requested_schema": {"type": "object", "properties": {
                "tags": {"type": "array", "items": {"type": "string"}}}}}),
        ),
        ("n3", json!({"mode": "url"})),
    ] {
        let a = core.on_line(&elicitation(id, extra));
        assert_eq!(approvals(&a), 0);
        assert!(!a
            .iter()
            .any(|a| matches!(a, Action::Emit(AdapterEvent::Event(e))
            if matches!(e.kind, EventKind::ClarifyRequest { .. }))));
        let w = writes(&a);
        assert_eq!(w.len(), 1, "{id}");
        assert_eq!(w[0]["response"]["request_id"], id);
        assert_eq!(w[0]["response"]["response"], json!({"action": "cancel"}));
        let sys = system_events(&a);
        assert_eq!(sys.len(), 1, "{id}");
        assert_eq!(sys[0].0, "elicitation_unsupported");
        assert!(sys[0].1.contains("deploy"), "{}", sys[0].1);
        assert!(core
            .answer_clarify(id, &BTreeMap::new(), Some(ClarifyAction::Decline))
            .is_err());
    }
}

/// SPEC 0.180: what an MCP server's request only shows is cut: a 10 KB `message` arrives as its
/// first `PREVIEW_LIMIT` bytes and "…", the server's name as 256 bytes, a field's title and
/// description as 1024; the field's name, the key of the answer, is kept whole.
#[test]
fn an_mcp_servers_long_message_is_cut() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let long = "m".repeat(10 * 1024);
    let name = "n".repeat(2 * 1024);
    let a = core.on_line(&elicitation(
        "e1",
        json!({"message": long, "mcp_server_name": "s".repeat(1024),
            "requested_schema": {"type": "object", "properties": {
                name.clone(): {"type": "string", "title": "t".repeat(4096),
                    "description": "d".repeat(4096)}}}}),
    ));
    let p = clarify_wire(&a);
    assert_eq!(
        p["message"],
        json!(format!("{}…", &long[..super::engine::PREVIEW_LIMIT]))
    );
    assert_eq!(p["server"], json!("s".repeat(256)));
    assert_eq!(p["questions"][0]["text"], json!("d".repeat(1024)));
    assert_eq!(p["questions"][0]["header"], json!("t".repeat(1024)));
    // The answer goes back under the property's own name, not cut.
    let answers = BTreeMap::from([("1".to_string(), ClarifyAnswer::One("x".into()))]);
    let w = writes(
        &core
            .answer_clarify("e1", &answers, Some(ClarifyAction::Accept))
            .unwrap(),
    );
    assert_eq!(w[0]["response"]["response"]["content"], json!({name: "x"}));
}

/// SPEC 0.180: a request whose `clarify.request` would be over 256 KiB (here its enum values,
/// which go back to the server and are not cut) is answered `cancel` at once, with a `system
/// elicitation_unsupported` line, and gives no card.
#[test]
fn an_mcp_servers_request_over_256_kib_is_cancelled() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let values: Vec<String> = (0..300)
        .map(|i| format!("{i:04}{}", "v".repeat(1020)))
        .collect();
    assert!(values.iter().map(String::len).sum::<usize>() > 256 * 1024);
    let a = core.on_line(&elicitation(
        "e1",
        json!({"requested_schema": {"type": "object", "properties": {
            "region": {"type": "string", "enum": values}}}}),
    ));
    assert!(!a
        .iter()
        .any(|a| matches!(a, Action::Emit(AdapterEvent::Event(e))
        if matches!(e.kind, EventKind::ClarifyRequest { .. }))));
    let w = writes(&a);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0]["response"]["request_id"], "e1");
    assert_eq!(w[0]["response"]["response"], json!({"action": "cancel"}));
    let sys = system_events(&a);
    assert_eq!(sys.len(), 1);
    assert_eq!(sys[0].0, "elicitation_unsupported");
    assert!(sys[0].1.contains("too large"), "{}", sys[0].1);
    assert!(core
        .answer_clarify("e1", &BTreeMap::new(), Some(ClarifyAction::Decline))
        .is_err());
}

/// SPEC 0.199: each option's `preview` goes into `option_previews` by the rule of
/// `option_descriptions`: paired with the labels, `""` for an option without one, U+0000
/// replaced, absent when no option has one. A preview over `OPTION_PREVIEW_LIMIT` bytes is
/// cut on a char boundary and gets the truncation mark.
#[test]
fn clarify_option_previews() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let x = |n: usize| "x".repeat(n);
    // "é" is two bytes across the limit: the cut drops it whole.
    let long = format!("{}é tail", x(OPTION_PREVIEW_LIMIT - 1));
    let req = json!({"type": "control_request", "request_id": "q1", "request": {
        "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "input": {"questions": [
            {"question": "Which layout?", "multiSelect": false, "options": [
                {"label": "Sidebar", "preview": "| a |\u{0}| <b>b</b> |"},
                {"label": "Tabs", "description": "On top"},
                {"preview": "no label"},
                {"label": "Exact", "preview": x(OPTION_PREVIEW_LIMIT)},
                {"label": "Long", "preview": long}]},
            {"question": "Which checks?", "multiSelect": true, "options": [
                {"label": "fmt", "description": "rustfmt"}, {"label": "clippy"}]}]}}});
    let a = core.on_line(&req.to_string());
    let qs = match &a[..] {
        [Action::Emit(AdapterEvent::Event(e))] => match &e.kind {
            EventKind::ClarifyRequest { questions, .. } => questions.clone(),
            k => panic!("{k:?}"),
        },
        other => panic!("{other:?}"),
    };
    assert_eq!(
        qs[0].options,
        Some(vec![
            "Sidebar".into(),
            "Tabs".into(),
            "Exact".into(),
            "Long".into()
        ])
    );
    assert_eq!(
        qs[0].option_previews,
        Some(vec![
            "| a |\u{FFFD}| <b>b</b> |".into(),
            "".into(),
            x(OPTION_PREVIEW_LIMIT),
            format!("{}{OPTION_PREVIEW_TRUNCATED}", x(OPTION_PREVIEW_LIMIT - 1)),
        ])
    );
    assert_eq!(OPTION_PREVIEW_TRUNCATED, "\n… (truncated)");
    assert_eq!(qs[1].option_previews, None);
    let wire = serde_json::to_value(&qs[1]).unwrap();
    assert!(wire.get("option_previews").is_none());
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
        human: false,
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
    // One "Interrupted" line for the turn; `[Request interrupted by user for tool use]` is none.
    assert_eq!(
        kinds,
        [
            "tool.result",
            "usage",
            "system",
            "status:None",
            "turn_ended"
        ]
    );
    assert_eq!(
        system_events(&all),
        [("turn_interrupted".into(), "Interrupted".into())]
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
            json!({"state": "idle", "in_reply_to": input})
        );
    }
}

/// Stop during the answer (2.1.312: the text cut off comes as `assistant` with `aborted:
/// true`, then `[Request interrupted by user]`, then `result` with `terminal_reason:
/// aborted_streaming`): the text is the final `agent.message` with `interrupted: true`, and
/// the turn ends with one "Interrupted" line after its `usage`.
#[test]
fn an_interrupted_answer_is_marked_and_the_turn_says_so() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let input = started(&mut core, sid);
    let a = play(
        &mut core,
        &format!(
            r#"{{"type":"assistant","message":{{"model":"claude-haiku-5-5","content":[{{"type":"text","text":"The tests in crates/hqc-hub"}}]}},"parent_tool_use_id":null,"uuid":"{}","aborted":true}}
{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"[Request interrupted by user]"}}]}},"parent_tool_use_id":null,"uuid":"{}"}}
{{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":1,"stop_reason":null,"terminal_reason":"aborted_streaming","errors":["[ede_diagnostic] result_type=user last_content_type=text stop_reason=null"],"total_cost_usd":0.002,"usage":{{"input_tokens":4,"output_tokens":7}}}}"#,
            Uuid::new_v4(),
            Uuid::new_v4()
        ),
    );
    let evs = emitted(a.clone());
    assert_eq!(
        shape(&evs),
        [
            "agent.message:true",
            "usage",
            "system",
            "status",
            "turn_ended"
        ]
    );
    assert_eq!(
        system_events(&a),
        [("turn_interrupted".into(), "Interrupted".into())]
    );
    let wire: Vec<Value> = events(&evs)
        .into_iter()
        .map(|e| serde_json::to_value(&e.kind).unwrap())
        .collect();
    assert_eq!(
        wire[0]["payload"],
        json!({"text": "The tests in crates/hqc-hub", "final": true, "in_reply_to": input,
            "interrupted": true})
    );
    assert_eq!(
        wire[3]["payload"],
        json!({"state": "idle", "in_reply_to": input})
    );
}

/// A turn interrupted while its reply streamed can end with a `result` of no tokens though the
/// total grew (seen on the stand, 2.1.312): no `0 → 0 tokens` line, only the session's total.
#[test]
fn an_interrupted_turn_without_tokens_gives_no_usage_line() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let a = play(
        &mut core,
        &format!(
            r#"{{"type":"assistant","message":{{"model":"claude-haiku-5-5","content":[{{"type":"text","text":"Once upon"}}]}},"parent_tool_use_id":null,"uuid":"{}","aborted":true}}
{{"type":"result","subtype":"success","is_error":false,"num_turns":1,"stop_reason":null,"terminal_reason":"aborted_streaming","result":"Once upon","total_cost_usd":0.00013,"usage":{{"input_tokens":0,"output_tokens":0}}}}"#,
            Uuid::new_v4()
        ),
    );
    let evs = emitted(a.clone());
    assert_eq!(
        shape(&evs),
        [
            "agent.message:true",
            "usage",
            "system",
            "status",
            "turn_ended"
        ]
    );
    let wire: Vec<Value> = events(&evs)
        .into_iter()
        .map(|e| serde_json::to_value(&e.kind).unwrap())
        .collect();
    assert_eq!(wire[1]["payload"]["input_tokens"], 0);
    assert!(wire[1]["payload"].get("cost_usd").is_none());
    assert_eq!(wire[1]["payload"]["session_cost_usd"], 0.00013);
}

/// `session_tokens` of every `usage` the `result` line `r` makes, as the wire shows it.
fn session_tokens_of(r: Value) -> Vec<Option<u64>> {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    events(&emitted(core.on_line(&r.to_string())))
        .into_iter()
        .filter(|e| matches!(e.kind, EventKind::Usage { .. }))
        .map(|e| serde_json::to_value(&e.kind).unwrap()["payload"]["session_tokens"].as_u64())
        .collect()
}

/// The `usage` of a turn and the one with the total only carry the runtime's
/// cumulative tokens: the four `modelUsage` counts summed over all its models; none when the
/// `result` has no `modelUsage`.
#[test]
fn usage_carries_the_session_tokens_of_model_usage() {
    let model_usage = json!({
        "claude-opus-5-5": {"inputTokens": 1, "outputTokens": 2, "cacheReadInputTokens": 3, "cacheCreationInputTokens": 4},
        "claude-haiku-5-5": {"inputTokens": 10, "outputTokens": 20, "cacheReadInputTokens": 30, "cacheCreationInputTokens": 40},
    });
    let turn = json!({"type": "result", "num_turns": 1, "total_cost_usd": 0.01,
        "usage": {"input_tokens": 4, "output_tokens": 2}, "modelUsage": model_usage});
    assert_eq!(session_tokens_of(turn), [Some(110)]);
    // No tokens in `usage`, the total grew: the total-only `usage`.
    let total_only = json!({"type": "result", "num_turns": 1, "total_cost_usd": 0.01,
        "terminal_reason": "aborted_streaming",
        "usage": {"input_tokens": 0, "output_tokens": 0}, "modelUsage": model_usage});
    assert_eq!(session_tokens_of(total_only), [Some(110)]);
    let bare = json!({"type": "result", "num_turns": 1, "total_cost_usd": 0.01,
        "usage": {"input_tokens": 4, "output_tokens": 2}});
    assert_eq!(session_tokens_of(bare), [None]);
}

/// A turn the CLI ended early for any reason but an interrupt ends with one line that says
/// why, after its `usage`: the `terminal_reason` (the subtype without one), the API's status
/// and `errors`, cut to 300 bytes; not the `result` text of `is_error: true`, which the CLI's
/// own reply brought. A turn that completed, and a local command (no `terminal_reason`), have
/// none.
#[test]
fn a_turn_that_ended_early_says_why() {
    let sid = Uuid::now_v7();
    let end = |fields: Value| {
        let mut core = Core::new(sid, false);
        started(&mut core, sid);
        let mut r = json!({"type": "result", "num_turns": 1, "total_cost_usd": 0.001,
            "usage": {"input_tokens": 4, "output_tokens": 2}});
        r.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        system_events(&core.on_line(&r.to_string()))
    };
    let early = |t: &str| vec![("turn_ended_early".to_string(), t.to_string())];
    assert_eq!(
        end(
            json!({"subtype": "error_max_budget_usd", "is_error": true, "terminal_reason": "budget_exhausted",
            "errors": ["Reached maximum budget ($0.01)"]})
        ),
        early("Turn ended early: budget_exhausted · Reached maximum budget ($0.01)")
    );
    assert_eq!(
        end(
            json!({"subtype": "success", "is_error": true, "result": "API Error: 529 Overloaded",
            "terminal_reason": "api_error", "api_error_status": 529})
        ),
        early("Turn ended early: api_error · API status 529")
    );
    assert_eq!(
        end(
            json!({"subtype": "success", "is_error": false, "result": "", "terminal_reason": "hook_stopped"})
        ),
        early("Turn ended early: hook_stopped")
    );
    assert_eq!(
        end(
            json!({"subtype": "error_during_execution", "is_error": true, "errors": ["boom", " ", "again"]})
        ),
        early("Turn ended early: error_during_execution · boom; again")
    );
    let long = end(
        json!({"subtype": "error_during_execution", "is_error": true, "terminal_reason": "model_error",
        "errors": ["x".repeat(1000)]}),
    );
    assert_eq!(long[0].1.len(), 300);
    assert!(long[0].1.starts_with("Turn ended early: model_error · xxx"));
    assert!(end(
        json!({"subtype": "success", "is_error": false, "result": "OK", "terminal_reason": "completed"})
    )
    .is_empty());
    // A completed turn whose answer the API stopped over a possible prompt injection.
    assert_eq!(
        end(
            json!({"subtype": "success", "is_error": false, "result": "", "terminal_reason": "completed",
            "stop_reason": "prompt_injection_risk"})
        ),
        early(PROMPT_INJECTION_TEXT)
    );
    assert!(
        end(json!({"subtype": "success", "is_error": false, "result": "", "num_turns": 0}))
            .is_empty()
    );
    // The model moved its work to the background, or a tool call was deferred: on purpose.
    for reason in ["background_requested", "tool_deferred"] {
        assert!(end(
            json!({"subtype": "success", "is_error": false, "result": "", "terminal_reason": reason})
        )
        .is_empty());
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
    let (req, turn, _) = core.interrupt().expect("the CLI's own turn runs");
    assert_eq!(writes(&[req])[0]["request"]["subtype"], "interrupt");
    assert!(core.turn_running(turn));
    play(
        &mut core,
        r#"{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":1,"total_cost_usd":0.001,"usage":{"input_tokens":4,"output_tokens":2}}"#,
    );
    assert!(!core.turn_running(turn));
    assert!(core.interrupt().is_none());
}

/// A turn the CLI starts by itself after a background task's end (`init` with no input of
/// ours) is `status running`: no input opens it on the hub (SPEC §17.1). The `init` of a turn
/// our input starts sends none: the input's `ack` opens that turn.
#[test]
fn a_turn_the_cli_starts_by_itself_is_running() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let running = |a: Vec<Action>| -> usize {
        events(&emitted(a))
            .into_iter()
            .filter(|e| {
                matches!(
                    e.kind,
                    EventKind::Status {
                        state: SessionState::Running,
                        ..
                    }
                )
            })
            .count()
    };
    let init = r#"{"type":"system","subtype":"init","model":"claude-haiku-5-5","permissionMode":"default","uuid":"fb2b6af8-8921-4277-a09e-61e710bfcdb2"}"#;
    let result = r#"{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.004,"usage":{"input_tokens":4,"output_tokens":38}}"#;
    play(
        &mut core,
        &format!(
            r#"{result}
{{"type":"system","subtype":"task_notification","task_id":"abd6ecd27912266e5","tool_use_id":"toolu_018foiN3RMuTb1fmFn53JffP","status":"completed","summary":"done","uuid":"614bde30-70d7-46cc-9ff9-9e22f9bdc118"}}"#
        ),
    );
    assert_eq!(running(play(&mut core, init)), 1, "the CLI's own turn");
    play(&mut core, result);
    written(&mut core, sid, "x");
    assert_eq!(running(play(&mut core, init)), 0, "our input's turn");
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
    // Only `status idle` names a turn's input. Here `status running` goes the other way round:
    // the input is written before the `init` of the CLI's own turn, which then gets none, and
    // with no `command_lifecycle` lines it counts as taken at that turn's `result`, so the
    // `init` of the input's own turn looks like one the CLI starts by itself.
    let replies = |a: Vec<Action>| -> Vec<(String, Option<Uuid>)> {
        events(&emitted(a))
            .into_iter()
            .filter_map(|e| match &e.kind {
                EventKind::AgentMessage { in_reply_to, .. } => {
                    Some(("agent.message".into(), *in_reply_to))
                }
                EventKind::Status {
                    state: SessionState::Idle,
                    in_reply_to,
                    ..
                } => Some(("status".into(), *in_reply_to)),
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

/// An input written while the CLI runs a turn it started by itself, after that turn's last text
/// and before its `result` (9–100 ms apart on the stand): the text stays held, as for an input
/// written into a turn of hqc's, and that `result` ends it `final: true`; the input runs next.
#[test]
fn an_input_during_a_turn_the_cli_starts_by_itself_keeps_its_last_text() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let a = user_input(sid, "Run `sleep 5` in the background.");
    let b = user_input(sid, "Reply with the single word PONG.");
    let init = json!({"type": "system", "subtype": "init", "model": "claude-haiku-5-5",
        "permissionMode": "default", "session_id": CLI_SESSION})
    .to_string();
    let ua = line_uuid(&core.input(&a));
    play(
        &mut core,
        &lines_of(&[
            lifecycle(&ua, "queued"),
            lifecycle(&ua, "started"),
            echo(&ua, &a.text),
            reply("LAUNCHED"),
            result("success"),
            lifecycle(&ua, "completed"),
            r#"{"type":"system","subtype":"task_notification","task_id":"abd6ecd27912266e5","tool_use_id":"toolu_018foiN3RMuTb1fmFn53JffP","status":"completed","summary":"done","uuid":"614bde30-70d7-46cc-9ff9-9e22f9bdc118"}"#.into(),
            init.clone(),
            reply("The background command finished."),
        ]),
    );
    let ub = line_uuid(&core.input(&b));
    let out = play(
        &mut core,
        &lines_of(&[
            lifecycle(&ub, "queued"),
            result("success"),
            init,
            lifecycle(&ub, "started"),
            echo(&ub, &b.text),
            reply("PONG"),
            result("success"),
            lifecycle(&ub, "completed"),
        ]),
    );
    let names = [(a.event_id, "A"), (b.event_id, "B")];
    assert_eq!(
        trace(&out, &names),
        [
            "message \"The background command finished.\" to A",
            "status to A",
            "turn_ended [B]",
            "accepted B",
            "message \"PONG\" to B",
            "status to B",
            "turn_ended []"
        ]
    );
    let finals: Vec<bool> = events(&emitted(out))
        .into_iter()
        .filter_map(|e| match &e.kind {
            EventKind::AgentMessage { r#final, .. } => Some(*r#final),
            _ => None,
        })
        .collect();
    assert_eq!(finals, [true, true]);
}

/// A request whose stream broke after some text, then retried by the CLI: the main agent's
/// next `message_start` drops the text written so far (`agent.delta` with `reset: true` and an
/// empty `text`), as the VS Code tab drops the rows of an abandoned attempt. No reset before any
/// text, after text an assistant text took up, or on a subagent's `message_start`.
#[test]
fn a_new_request_drops_the_text_of_an_abandoned_attempt() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let ev = |e: Value| {
        json!({"type": "stream_event", "parent_tool_use_id": null, "event": e,
            "session_id": CLI_SESSION, "uuid": Uuid::new_v4()})
    };
    let start = ev(json!({"type": "message_start", "message": {"id": "msg_01"}})).to_string();
    let delta = |t: &str| {
        ev(json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": t}}))
        .to_string()
    };
    let deltas = |a: Vec<Action>| -> Vec<(String, bool)> {
        events(&emitted(a))
            .into_iter()
            .filter_map(|e| match &e.kind {
                EventKind::AgentDelta { text, reset, .. } => Some((text.clone(), *reset)),
                _ => None,
            })
            .collect()
    };
    assert!(
        deltas(play(&mut core, &start)).is_empty(),
        "nothing written"
    );
    assert_eq!(
        deltas(play(&mut core, &delta("Let me"))),
        [("Let me".into(), false)]
    );
    assert_eq!(deltas(play(&mut core, &start)), [(String::new(), true)]);
    assert!(deltas(play(&mut core, &start)).is_empty(), "dropped once");
    play(
        &mut core,
        &lines_of(&[
            delta("Let me look."),
            reply("Let me look."),
            bash("toolu_01", "ls"),
            tool_result("toolu_01"),
        ]),
    );
    assert!(
        deltas(play(&mut core, &start)).is_empty(),
        "an assistant text took it up"
    );
    play(&mut core, &delta("Now"));
    let mut sub: Value = serde_json::from_str(&start).unwrap();
    sub["parent_tool_use_id"] = json!("toolu_02");
    assert!(
        deltas(play(&mut core, &sub.to_string())).is_empty(),
        "a subagent's"
    );
}

/// An attempt the CLI abandoned while it was thinking: its thinking goes with it, as the VS
/// Code tab drops every row of the attempt (`retireAbandonedStreamedRows`); once saved it stays.
#[test]
fn a_new_request_drops_the_thinking_of_an_abandoned_attempt() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let ev = |e: Value| {
        json!({"type": "stream_event", "parent_tool_use_id": null, "event": e,
            "session_id": CLI_SESSION, "uuid": Uuid::new_v4()})
    };
    let start = ev(json!({"type": "message_start", "message": {"id": "msg_01"}})).to_string();
    let thought = ev(json!({"type": "content_block_delta", "index": 0,
        "delta": {"type": "thinking_delta", "thinking": "Let me think"}}))
    .to_string();
    let saved = json!({"type": "assistant", "message": {"id": "msg_01",
        "model": "claude-haiku-5-5", "role": "assistant",
        "content": [{"type": "thinking", "thinking": "Let me think"}]},
        "parent_tool_use_id": null, "session_id": CLI_SESSION, "uuid": Uuid::new_v4()})
    .to_string();
    let resets = |a: Vec<Action>| -> usize {
        events(&emitted(a))
            .iter()
            .filter(|e| matches!(e.kind, EventKind::AgentDelta { reset: true, .. }))
            .count()
    };
    play(&mut core, &start);
    play(&mut core, &thought);
    assert_eq!(
        resets(play(&mut core, &start)),
        1,
        "the abandoned attempt's"
    );
    assert_eq!(resets(play(&mut core, &start)), 0, "dropped once");
    play(&mut core, &thought);
    play(&mut core, &saved);
    assert_eq!(
        resets(play(&mut core, &start)),
        0,
        "a saved thinking took it up"
    );
    // A stream broken while thinking that the CLI retried without one: the response comes as an
    // assistant of another `message.id`, and the thinking so far is dropped before it.
    let said = |id: &str, content: Value| {
        json!({"type": "assistant", "message": {"id": id, "model": "claude-haiku-5-5",
            "role": "assistant", "content": content},
            "parent_tool_use_id": null, "session_id": CLI_SESSION, "uuid": Uuid::new_v4()})
        .to_string()
    };
    play(&mut core, &thought);
    assert_eq!(
        resets(play(
            &mut core,
            &said("msg_02", json!([{"type": "text", "text": "Here it is."}]))
        )),
        1,
        "a reply without a stream"
    );
    // An empty saved thinking sends no agent.thinking; the saved text after it ends the
    // thinking being written.
    play(&mut core, &start);
    play(&mut core, &thought);
    play(
        &mut core,
        &said(
            "msg_01",
            json!([{"type": "thinking", "thinking": ""}, {"type": "text", "text": "Here."}]),
        ),
    );
    assert_eq!(
        resets(play(&mut core, &start)),
        0,
        "a saved text took it up"
    );
}

/// A stream broken after some text that the CLI retried without one (2.1.312: "Error
/// streaming, falling back to non-streaming mode"): no `message_start` comes, the response
/// arrives as an `assistant` of another `message.id`, and the text written so far is dropped
/// before it, as the VS Code tab drops it (`web:171092`). None for the response streamed, a
/// `<synthetic>` one, one without an id, after text an assistant text took up, or a subagent's.
#[test]
fn a_reply_without_a_stream_drops_the_text_of_an_abandoned_attempt() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let ev = |e: Value| {
        json!({"type": "stream_event", "parent_tool_use_id": null, "event": e,
            "session_id": CLI_SESSION, "uuid": Uuid::new_v4()})
        .to_string()
    };
    let start = |id: &str| ev(json!({"type": "message_start", "message": {"id": id}}));
    let delta = |t: &str| {
        ev(json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": t}}))
    };
    let said = |id: &str, model: &str, t: &str| {
        json!({"type": "assistant", "message": {"id": id, "model": model, "role": "assistant",
            "content": [{"type": "text", "text": t}]},
            "parent_tool_use_id": null, "session_id": CLI_SESSION, "uuid": Uuid::new_v4()})
        .to_string()
    };
    let kinds = |a: Vec<Action>| -> Vec<String> {
        events(&emitted(a))
            .into_iter()
            .map(|e| match &e.kind {
                EventKind::AgentDelta { text, reset, .. } => format!("delta {text:?} {reset}"),
                EventKind::AgentMessage { text, .. } => format!("message {text:?}"),
                _ => "other".into(),
            })
            .collect()
    };
    play(&mut core, &lines_of(&[start("msg_01"), delta("Let me")]));
    let out = kinds(play(
        &mut core,
        &lines_of(&[
            said("msg_02", "claude-haiku-5-5", "Here it is."),
            result("success"),
        ]),
    ));
    assert_eq!(
        out[..2],
        ["delta \"\" true", "message \"Here it is.\""],
        "{out:?}"
    );
    started(&mut core, sid);
    for (lines, why) in [
        (
            vec![
                start("msg_03"),
                delta("Sure"),
                said("msg_03", "claude-haiku-5-5", "Sure."),
            ],
            "the response streamed",
        ),
        (
            vec![said("msg_04", "claude-haiku-5-5", "More.")],
            "an assistant text took it up",
        ),
        (
            vec![
                start("msg_05"),
                delta("API"),
                said("msg_06", "<synthetic>", "API Error"),
            ],
            "a synthetic one",
        ),
        (
            vec![start("msg_07"), delta("No id"), reply("No id.")],
            "one without an id",
        ),
    ] {
        let out = kinds(play(&mut core, &lines_of(&lines)));
        assert!(!out.iter().any(|k| k.ends_with("true")), "{why}: {out:?}");
    }
    play(&mut core, &lines_of(&[start("msg_08"), delta("Main")]));
    let mut sub: Value = serde_json::from_str(&said("msg_09", "claude-haiku-5-5", "Sub.")).unwrap();
    sub["parent_tool_use_id"] = json!("toolu_02");
    assert!(
        kinds(play(&mut core, &sub.to_string())).is_empty(),
        "a subagent's"
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
        human: false,
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
        human: false,
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
                interrupted: false,
                refusal: false,
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
        human: false,
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
        text: "d\u{FFFD}".into(),
        reset: false,
        thinking: false,
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

/// SPEC §8.1: with `--thinking-display summarized` the CLI streams the thinking as it is
/// written, `thinking_delta` chunks with text (2.1.312): each is an `agent.delta` with
/// `thinking`; an empty chunk (an older CLI, the recorded fixtures) gives nothing, and the
/// answer's text stays a plain delta.
#[test]
fn thinking_chunks_are_thinking_deltas() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let chunk = |delta: Value| {
        json!({"type":"stream_event","parent_tool_use_id":null,"event":{
            "type":"content_block_delta","index":0,"delta":delta},
            "thinking_display":"summarized"})
        .to_string()
    };
    let mut kinds = vec![];
    for l in [
        chunk(json!({"type":"thinking_delta","thinking":"I'm checking wh"})),
        chunk(json!({"type":"thinking_delta","thinking":""})),
        chunk(json!({"type":"thinking_delta","thinking":"ether 1001"})),
        chunk(json!({"type":"text_delta","text":"Yes."})),
    ] {
        for e in emitted(core.on_line(&l)) {
            if let AdapterEvent::Event(e) = e {
                kinds.push(e.kind);
            }
        }
    }
    let delta = |text: &str, thinking| EventKind::AgentDelta {
        text: text.into(),
        reset: false,
        thinking,
    };
    assert_eq!(
        kinds,
        [
            delta("I'm checking wh", true),
            delta("ether 1001", true),
            delta("Yes.", false),
        ]
    );
}

/// SPEC §8.1: Claude Code's own cross-session messaging by the worker config. By default the
/// command line is as without these options: `ListAgents` and `SendMessage` disallowed, no
/// `crossSessionInbound`. `allow_subagent_messages` keeps `SendMessage` (not in restricted
/// mode) and refuses other sessions' messages; `allow_native_messaging` wins over it: both
/// tools, no `crossSessionInbound`.
#[tokio::test]
async fn native_messaging_follows_the_worker_config() {
    let work = tempfile::tempdir().unwrap();
    let base = format!(
        "hub_url = \"https://h\"\nallowed_roots = [{0:?}]\n",
        work.path().display().to_string()
    );
    let args = |toml: &str, restricted: bool| -> Vec<String> {
        let cfg = crate::config::Config::parse(&format!("{base}{toml}")).unwrap();
        let f = ClaudeCodeAdapters::from_config(&cfg, "default".into());
        let a = ClaudeCodeAdapter {
            session_id: Uuid::nil(),
            opts: f.opts.clone(),
            spawner: f.spawner.clone(),
            events: mpsc::channel(1).0,
            proc: None,
        };
        let mut s = spec(Uuid::now_v7(), work.path(), Uuid::new_v4());
        s.restricted = restricted;
        a.command(&s, &Uuid::nil(), Launch::New, None, None).args
    };
    let disallowed = |args: &[String]| -> Vec<String> {
        let i = args.iter().position(|a| a == "--disallowedTools");
        i.map(|i| {
            args[i + 1..]
                .iter()
                .take_while(|a| !a.starts_with("--"))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
    };
    let settings = |args: &[String]| -> Value {
        let i = args.iter().position(|a| a == "--settings").unwrap();
        serde_json::from_str(&args[i + 1]).unwrap()
    };

    // Default: the whole command line, as without these options.
    let deny = crate::permissions::DEFAULT_DENY;
    let plain = json!({"voiceEnabled": false, "permissions": {"allow": [], "deny": deny}});
    let want: Vec<String> = [
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
        "--no-chrome",
        "--disallowedTools",
        "ListAgents",
        "SendMessage",
        "--settings",
        &plain.to_string(),
        "--setting-sources",
        "user,local",
        "--permission-mode",
        "default",
        "--session-id",
        &Uuid::nil().to_string(),
        "--verbose",
        "--model",
        "claude-haiku-5-5",
    ]
    .map(String::from)
    .into();
    assert_eq!(args("", false), want);

    let subagents = "allow_subagent_messages = true\n";
    let sub = args(subagents, false);
    assert_eq!(disallowed(&sub), ["ListAgents"]);
    assert_eq!(settings(&sub)["crossSessionInbound"], "refuse");
    // `--restricted` does not remove `SendMessage` by itself.
    let restricted = args(subagents, true);
    assert!(
        restricted.contains(&"--restricted".to_string()),
        "{restricted:?}"
    );
    assert_eq!(disallowed(&restricted), ["ListAgents", "SendMessage"]);
    assert_eq!(settings(&restricted)["crossSessionInbound"], "refuse");

    let on = args(
        "allow_native_messaging = true\nallow_subagent_messages = true\n",
        false,
    );
    assert_eq!(disallowed(&on), Vec::<String>::new());
    assert_eq!(settings(&on).get("crossSessionInbound"), None, "{on:?}");
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
        human: false,
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

/// The record the CLI writes for our `set_model`, `<command-name>/model</command-name> …
/// <command-args>name</command-args>` (seen on 2.1.312), is the adapter's: the mirror does not
/// import it as a prompt of someone else.
#[tokio::test]
async fn a_model_switch_record_is_own() {
    let runtime = Uuid::new_v4();
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let claude = ScriptedClaude::new("");
    let opts = options(config.path());
    let own = opts.own_records.clone();
    let factory = ClaudeCodeAdapters::new(opts, Arc::new(claude.clone()));
    let s = spec(Uuid::now_v7(), work.path(), runtime);
    let (tx, _rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    a.set_model("claude-fable-5-1").await.unwrap();
    assert!(own.claim(runtime, Uuid::new_v4(), Some("/model claude-fable-5-1")));
    a.kill().await.unwrap();
}

/// SPEC §8.1: a `rate_limit_event` (2.1.310) gives the 5-hour and 7-day windows of its
/// `unifiedWindows` and (since 0.179) its `status`, `rateLimitType` and `resetsAt`; one that
/// names neither window gives nothing.
#[test]
fn limits_of_a_rate_limit_event() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let line = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1794150000,"rateLimitType":"five_hour","unifiedWindows":{"five_hour":{"utilization":0.39,"resetsAt":1794150000,"status":"allowed"},"seven_day":{"utilization":0.23,"resetsAt":1794283200,"status":"allowed"}}},"uuid":"c4eef22b-3339-410d-a1d4-64ea1db4f46e","session_id":"fe2b6ce5-0e5c-46bb-afc3-9c91cd7dff97"}"#;
    let out = core.on_line(line);
    let [Action::Emit(AdapterEvent::Limits(l))] = out.as_slice() else {
        panic!("{out:?}");
    };
    assert_eq!(
        serde_json::to_value(l).unwrap(),
        json!({"five_hour": {"utilization": 0.39, "resets_at": 1794150000},
               "seven_day": {"utilization": 0.23, "resets_at": 1794283200},
               "status": "allowed", "rate_limit_type": "five_hour", "resets_at": 1794150000})
    );
    assert!(core
        .on_line(r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#)
        .is_empty());
}

/// Two turns of a source transcript: prompt, reply; prompt, reply.
fn two_turns() -> (String, [Uuid; 4]) {
    let ids = [
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    ];
    let line = |i: usize, kind: &str, content: Value| {
        let parent = (i > 0).then(|| ids[i - 1].to_string());
        json!({"type": kind, "uuid": ids[i], "parentUuid": parent, "isSidechain": false,
            "message": {"role": kind, "content": content}})
        .to_string()
    };
    let text = [
        line(0, "user", json!("Remember APPLE.")),
        json!({"type": "attachment", "uuid": Uuid::new_v4()}).to_string(),
        line(
            1,
            "assistant",
            json!([{"type": "thinking", "thinking": ""}, {"type": "text", "text": "OK"}]),
        ),
        line(2, "user", json!("Remember BANANA.")),
        line(3, "assistant", json!([{"type": "text", "text": "OK"}])),
    ]
    .join("\n");
    (text, ids)
}

/// The fork point is the record of the first event found, by any of its blocks; the records
/// up to it are copied. No event in the transcript: no fork point.
#[test]
fn a_fork_cuts_at_the_first_event_found() {
    let (text, ids) = two_turns();
    let reply = super::engine::record_event_id(&ids[1], 1);
    let missing = Uuid::new_v4();
    assert_eq!(
        super::fork_point(&text, &[missing, reply]),
        Some((ids[1], vec![ids[0], ids[1]]))
    );
    assert_eq!(super::fork_point(&text, &[missing]), None);
}

/// SPEC §8.1: a session that forks starts with `--resume <copy> --fork-session --session-id
/// <its own> --resume-session-at <record>`, the copy being the source cut right after the
/// record (the CLI resumes a transcript from its newest record back to the last compaction,
/// so a record before a later compaction would not be found) and removed with the process;
/// the copied records are its own; once its transcript is there it resumes it.
#[tokio::test]
async fn a_fork_starts_from_the_source_transcript() {
    let (text, ids) = two_turns();
    let (source, runtime) = (Uuid::new_v4(), Uuid::new_v4());
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let from = super::paths::transcript_path(config.path(), work.path(), &source);
    std::fs::create_dir_all(from.parent().unwrap()).unwrap();
    std::fs::write(&from, &text).unwrap();
    let claude = ScriptedClaude::new("");
    let opts = options(config.path());
    let own = opts.own_records.clone();
    let factory = ClaudeCodeAdapters::new(opts, Arc::new(claude.clone()));
    let mut s = spec(Uuid::now_v7(), work.path(), runtime);
    s.fork = Some(hqc_proto::frame::ForkFrom {
        runtime_session_id: source.to_string(),
        at_event_ids: vec![
            super::engine::record_event_id(&ids[3], 0),
            super::engine::record_event_id(&ids[1], 1),
        ]
        .into_iter()
        .rev()
        .collect(),
    });
    let (tx, _rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    let args = claude.commands()[0].args.clone();
    let at = args.iter().position(|a| a == "--resume").unwrap();
    let copy = std::path::PathBuf::from(&args[at + 1]);
    assert_eq!(
        args[at..at + 7],
        [
            "--resume".to_string(),
            super::fork_copy_path(&runtime).display().to_string(),
            "--fork-session".into(),
            "--session-id".into(),
            runtime.to_string(),
            "--resume-session-at".into(),
            ids[1].to_string(),
        ]
    );
    // The copy is the source up to and including the record's line, nothing after it.
    let cut: String = text.split_inclusive('\n').take(3).collect();
    assert_eq!(std::fs::read_to_string(&copy).unwrap(), cut);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&copy).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    }
    assert!(own.is_own(runtime, ids[0]) && own.is_own(runtime, ids[1]));
    assert!(!own.is_own(runtime, ids[3]));
    a.kill().await.unwrap();
    assert!(!copy.exists(), "the copy outlived the process");

    // The fork made the session's transcript: the next start resumes it.
    let to = super::paths::transcript_path(config.path(), work.path(), &runtime);
    std::fs::write(&to, "").unwrap();
    a.start(&s).await.unwrap();
    let args = claude.commands()[1].args.clone();
    assert!(!args.contains(&"--fork-session".to_string()), "{args:?}");
    assert!(args
        .windows(2)
        .any(|w| w == ["--resume".to_string(), runtime.to_string()]));
    a.kill().await.unwrap();

    // A source that does not hold the message refuses the start.
    std::fs::remove_file(&to).unwrap();
    s.fork.as_mut().unwrap().at_event_ids = vec![Uuid::new_v4()];
    let e = a.start(&s).await.unwrap_err();
    assert!(format!("{e:#}").contains("not in"), "{e:#}");
}

/// A fork takes the transcripts of the subagents its copied part names (`toolUseResult.agentId`)
/// into the new session's folder, where the CLI looks for them: the `.jsonl` up to its last
/// line break and the `.meta.json`, 0600. A subagent only the later part names, and an id that
/// is no file name, are left out.
#[tokio::test]
async fn a_fork_takes_the_transcripts_of_its_subagents() {
    let (text, ids) = two_turns();
    let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    let result = |id: &str| {
        json!({"type": "user", "uuid": Uuid::new_v4(), "isSidechain": false,
            "message": {"role": "user", "content": []},
            "toolUseResult": {"agentId": id, "status": "async_launched"}})
        .to_string()
    };
    lines.insert(1, result("a1"));
    lines.insert(2, result("../x"));
    lines.push(result("late"));
    let (source, runtime) = (Uuid::new_v4(), Uuid::new_v4());
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let from = super::paths::transcript_path(config.path(), work.path(), &source);
    std::fs::create_dir_all(from.parent().unwrap()).unwrap();
    std::fs::write(&from, lines.join("\n")).unwrap();
    let theirs = super::paths::subagents_dir(config.path(), work.path(), &source);
    std::fs::create_dir_all(&theirs).unwrap();
    std::fs::write(
        theirs.join("agent-a1.jsonl"),
        "{\"n\":1}\n{\"n\":2}\n{\"n\":3",
    )
    .unwrap();
    std::fs::write(theirs.join("agent-a1.meta.json"), "{\"agentType\":\"x\"}").unwrap();
    std::fs::write(theirs.join("agent-late.jsonl"), "{}\n").unwrap();
    let claude = ScriptedClaude::new("");
    let factory = ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude.clone()));
    let mut s = spec(Uuid::now_v7(), work.path(), runtime);
    s.fork = Some(hqc_proto::frame::ForkFrom {
        runtime_session_id: source.to_string(),
        at_event_ids: vec![super::engine::record_event_id(&ids[1], 1)],
    });
    let (tx, _rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    let mine = super::paths::subagents_dir(config.path(), work.path(), &runtime);
    let mut names: Vec<_> = std::fs::read_dir(&mine)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["agent-a1.jsonl", "agent-a1.meta.json"]);
    assert_eq!(
        std::fs::read_to_string(mine.join("agent-a1.jsonl")).unwrap(),
        "{\"n\":1}\n{\"n\":2}\n"
    );
    assert_eq!(
        std::fs::read_to_string(mine.join("agent-a1.meta.json")).unwrap(),
        "{\"agentType\":\"x\"}"
    );
    #[cfg(unix)]
    for n in &names {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(mine.join(n))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{n}: {mode:o}");
    }
    // Nothing outside the new session's folder: the project folder has the source's files and
    // the new folder only.
    let project = mine.parent().unwrap().parent().unwrap();
    let mut top: Vec<_> = std::fs::read_dir(project)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    top.sort();
    let mut want = vec![
        format!("{source}"),
        format!("{source}.jsonl"),
        runtime.to_string(),
    ];
    want.sort();
    assert_eq!(top, want);
    a.kill().await.unwrap();
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

/// `task_tools` (worker config): the child gets `CLAUDE_CODE_ENABLE_TODO_TOOLS=1` over the
/// environment's own value; without it the variable is removed, that value too.
#[tokio::test]
async fn the_task_tools_variable_follows_the_worker_config() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("ws")).unwrap();
    let cfg = crate::config::Config::parse(&format!(
        r#"hub_url = "https://h"
allowed_roots = [{root:?}]
[environments.e]
env = {{ CLAUDE_CODE_ENABLE_TODO_TOOLS = "0" }}
[[env_bindings]]
path = "{}"
environment = "e"
"#,
        root.join("ws").display()
    ))
    .unwrap();
    let config = tempfile::tempdir().unwrap();
    for (task_tools, want) in [(true, Some("1")), (false, None)] {
        let claude = ScriptedClaude::new("");
        let mut opts = options(config.path());
        opts.task_tools = task_tools;
        let factory = ClaudeCodeAdapters::new(opts, Arc::new(claude.clone()));
        let mut s = spec(Uuid::now_v7(), &root.join("ws"), Uuid::new_v4());
        s.environment = cfg.environment(&s.workspace);
        let (tx, _rx) = mpsc::channel(16);
        let mut a = factory.create(&s, tx);
        a.start(&s).await.unwrap();
        let vars: Vec<String> = claude.commands()[0]
            .env
            .iter()
            .filter(|(k, _)| k == "CLAUDE_CODE_ENABLE_TODO_TOOLS")
            .map(|(_, v)| v.clone())
            .collect();
        assert_eq!(vars, want.into_iter().collect::<Vec<_>>());
        a.kill().await.unwrap();
    }
    // The config's key reaches the options.
    let on = crate::config::Config::parse(&format!(
        "hub_url = \"https://h\"\nallowed_roots = [{root:?}]\ntask_tools = true\n"
    ))
    .unwrap();
    assert!(
        ClaudeCodeAdapters::from_config(&on, "default".into())
            .opts
            .task_tools
    );
    assert!(
        !ClaudeCodeAdapters::from_config(&cfg, "default".into())
            .opts
            .task_tools
    );
}

/// A local slash command (`/name x`): the CLI answers with a `result` and no replay echo.
/// The input still counts as taken, otherwise the supervisor resends it forever; taken before
/// the turn's `status idle`, whose `ack` after it would open a turn on the hub (SPEC §5.2).
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
    let status = s.iter().position(|e| e == "status").unwrap();
    let ended = s.iter().position(|e| e == "turn_ended").unwrap();
    assert!(accepted < status && status < ended, "{s:?}");
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

/// A `result` with no model turn and no tokens whose total grew (the CLI closes such a turn on a
/// background subagent's notification, and the growth is what the subagent spent): a `usage`
/// with the session's total only, no tokens and no `cost_usd`, which the client draws no line
/// for; the next turn's cost carries the growth.
#[test]
fn a_result_with_no_model_turn_leaves_the_background_cost_to_the_next_turn() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false).with_cost_baseline(Some(cost_state(1.0, 0, 0)));
    started(&mut core, sid);
    let usage = usages(core.on_line(
        r#"{"type":"result","subtype":"success","is_error":false,"num_turns":0,"duration_ms":3,"result":"","total_cost_usd":2.5,"usage":{"input_tokens":0,"output_tokens":0}}"#,
    ));
    assert_eq!(usage, vec![(0, 0, None, Some(2.5))]);
    started(&mut core, sid);
    let (costs, _) = costs_and_commands(&core.on_line(
        r#"{"type":"result","subtype":"success","is_error":false,"num_turns":1,"duration_ms":900,"result":"ok","total_cost_usd":2.75,"usage":{"input_tokens":4,"output_tokens":20}}"#,
    ));
    assert_eq!(costs, vec![(1.75, 2.75)]);
}

/// `/compact` (fixture `stream-resume-compact`, 2.1.308): `num_turns: 0` and no tokens, but the
/// session's total grew from the transcript's last `cost-state` (0.0032966), so it keeps its
/// `usage` with what the compaction cost, and the next turn's cost does not include it. Its
/// tokens are the growth of `modelUsage` over that `cost-state`'s (2037 - 4 in, 564 - 10 out,
/// cache 45717 - 29440 read and 20902 - 14984 written), not the cumulative numbers.
#[test]
fn compact_keeps_its_usage() {
    let fx = fixture!("resume-compact", "2.1.308");
    let sid = Uuid::now_v7();
    let before_compact = &fx.transcript[..fx.transcript.find("\"compact_boundary\"").unwrap()];
    let baseline = hqc_claude_jsonl::last_cost_state(before_compact);
    assert!(
        baseline.is_some_and(|b| (b.total_cost_usd - 0.0032966).abs() < 1e-12),
        "{baseline:?}"
    );
    let mut core = Core::new(sid, false).with_cost_baseline(baseline);
    started(&mut core, sid);
    let actions = play(&mut core, fx.stream);
    let (costs, _) = costs_and_commands(&actions);
    assert_eq!(costs.len(), 1, "{costs:?}");
    assert!((costs[0].0 - 0.00138282).abs() < 1e-9, "{costs:?}");
    assert_eq!(costs[0].1, 0.00467942);
    let tokens: Vec<_> = usage_tokens(actions)
        .into_iter()
        .map(|(i, o, r, w, _)| (i, o, r, w))
        .collect();
    assert_eq!(tokens, vec![(2033, 554, Some(16277), Some(5918))]);
    // The compaction counts for its own `result` only: a later one with no model turn whose
    // total grew gets the total-only `usage`, with no `cost_usd`.
    started(&mut core, sid);
    let usage = usages(core.on_line(
        r#"{"type":"result","subtype":"success","is_error":false,"num_turns":0,"duration_ms":3,"result":"","total_cost_usd":0.01,"usage":{"input_tokens":0,"output_tokens":0}}"#,
    ));
    assert_eq!(usage, vec![(0, 0, None, Some(0.01))]);
}

/// A main agent's `assistant` line of a response of `claude-opus-5-5` with this `usage`; with
/// `sub` it is a subagent's.
fn response_line(n: u32, usage: Value, sub: bool) -> String {
    json!({
        "type": "assistant",
        "uuid": format!("0f6c1d2e-2222-4a55-9d0e-1a2b3c4d5e{n:02}"),
        "parent_tool_use_id": if sub { json!("toolu_sub") } else { Value::Null },
        "message": {
            "id": format!("msg_{n}"),
            "model": "claude-opus-5-5",
            "role": "assistant",
            "content": [{"type": "text", "text": "x"}],
            "usage": usage,
        },
    })
    .to_string()
}

/// A turn's `result` with this `modelUsage` entry under `key`.
fn turn_result_as(key: &str, entry: Value) -> String {
    json!({
        "type": "result", "subtype": "success", "is_error": false, "num_turns": 1,
        "duration_ms": 9, "result": "x", "total_cost_usd": 0.01,
        "usage": {"input_tokens": 3, "output_tokens": 4},
        "modelUsage": {key: entry},
    })
    .to_string()
}

/// A turn's `result` with this `modelUsage` entry of `claude-opus-5-5`.
fn turn_result(entry: Value) -> String {
    turn_result_as("claude-opus-5-5", entry)
}

/// The `init` line of a CLI that runs `model`.
fn init_line(model: &str) -> String {
    json!({"type": "system", "subtype": "init", "model": model, "permissionMode": "default",
        "uuid": "fb2b6af8-8921-4277-a09e-61e710bfcdb2"})
    .to_string()
}

/// `(context_tokens, context_limit, cache_ttl_s)` of every `usage` among `actions`.
fn context_fields(actions: Vec<Action>) -> Vec<(Option<u64>, Option<u64>, Option<u64>)> {
    events(&emitted(actions))
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::Usage {
                context_tokens,
                context_limit,
                cache_ttl_s,
                ..
            } => Some((context_tokens, context_limit, cache_ttl_s)),
            _ => None,
        })
        .collect()
}

/// The size of the context is the sum of the usage of the main agent's last response (the
/// frames of one response repeat it: the last wins); a subagent's frame, even a later one and
/// a larger one, is not the main agent's context.
#[test]
fn the_context_is_the_last_main_response_not_a_subagents() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    core.on_line(&response_line(
        1,
        json!({"input_tokens": 2, "cache_read_input_tokens": 40, "cache_creation_input_tokens": 30, "output_tokens": 1}),
        false,
    ));
    core.on_line(&response_line(
        2,
        json!({"input_tokens": 2, "cache_read_input_tokens": 400, "cache_creation_input_tokens": 30, "output_tokens": 8}),
        false,
    ));
    core.on_line(&response_line(
        3,
        json!({"input_tokens": 9000, "cache_read_input_tokens": 9000, "output_tokens": 9000}),
        true,
    ));
    let got = context_fields(core.on_line(&turn_result(json!({}))));
    assert_eq!(got, vec![(Some(440), None, None)]);
}

/// A line of the CLI's own (`<synthetic>`, zero usage: an error text, a refusal) after a real
/// response is no response of the model: the context stays.
#[test]
fn a_synthetic_line_leaves_the_context_alone() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    core.on_line(&response_line(
        1,
        json!({"input_tokens": 700, "output_tokens": 5}),
        false,
    ));
    let synthetic = response_line(2, json!({"input_tokens": 0, "output_tokens": 0}), false)
        .replace("claude-opus-5-5", "<synthetic>");
    core.on_line(&synthetic);
    let got = context_fields(core.on_line(&turn_result(json!({}))));
    assert_eq!(got, vec![(Some(705), None, None)]);
}

/// The limit is `contextWindow − min(maxOutputTokens, 20000) − 13000` of the main model's entry:
/// the threshold of the model's window, not of an overridden auto-compaction window
/// (`CLAUDE_CODE_AUTO_COMPACT_WINDOW`, `autoCompactWindow`, `/autocompact`) nor of a disabled
/// one, which the stream does not show. Checked live on 2.1.329: a window of 1 000 000 gives
/// 967 000 in `get_context_usage`.
#[test]
fn the_context_limit_is_the_window_less_the_reserve_and_the_margin() {
    let usage = json!({"input_tokens": 1, "output_tokens": 1});
    let run = |result: String| {
        let sid = Uuid::now_v7();
        let mut core = Core::new(sid, false);
        started(&mut core, sid);
        core.on_line(&response_line(1, usage.clone(), false));
        context_fields(core.on_line(&result))
    };
    let got = run(turn_result(
        json!({"contextWindow": 1_000_000, "maxOutputTokens": 128_000}),
    ));
    assert_eq!(got, vec![(Some(2), Some(967_000), None)]);
    let got = run(turn_result(
        json!({"contextWindow": 200_000, "maxOutputTokens": 20_000}),
    ));
    assert_eq!(got, vec![(Some(2), Some(167_000), None)]);
    // An output limit below the reserve counts as it is.
    let got = run(turn_result(
        json!({"contextWindow": 200_000, "maxOutputTokens": 8_192}),
    ));
    assert_eq!(got, vec![(Some(2), Some(178_808), None)]);
    // The entry of a model that is neither the main one nor the response's is not used.
    let other = turn_result_as("claude-haiku-5-5", json!({"contextWindow": 200_000}));
    assert_eq!(run(other), vec![(Some(2), None, None)]);
}

/// The CLI keys `modelUsage` by the model as requested (`claude-opus-5-5[1m]`), the response
/// names it without the suffix: the main loop's model from `init` finds the entry; so does the
/// entry whose `canonicalModel` is the response's when the main model is not known; and a turn
/// with no tracked response (`/compact` first after a restart) still has the limit.
#[test]
fn the_limit_is_found_under_the_requested_model() {
    let entry = json!({"contextWindow": 1_000_000, "maxOutputTokens": 64_000, "canonicalModel": "claude-opus-5-5"});
    let usage = json!({"input_tokens": 1, "output_tokens": 1});
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    core.on_line(&init_line("claude-opus-5-5[1m]"));
    started(&mut core, sid);
    core.on_line(&response_line(1, usage.clone(), false));
    let got = context_fields(core.on_line(&turn_result_as("claude-opus-5-5[1m]", entry.clone())));
    assert_eq!(got, vec![(Some(2), Some(967_000), None)]);
    // No `init`: by `canonicalModel`.
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    core.on_line(&response_line(1, usage, false));
    let got = context_fields(core.on_line(&turn_result_as("some-provider-id", entry.clone())));
    assert_eq!(got, vec![(Some(2), Some(967_000), None)]);
    // No response of this process: the size is unknown, the limit is not.
    let mut core = Core::new(sid, false);
    core.on_line(&init_line("claude-opus-5-5[1m]"));
    started(&mut core, sid);
    let got = context_fields(core.on_line(&turn_result_as("claude-opus-5-5[1m]", entry)));
    assert_eq!(got, vec![(None, Some(967_000), None)]);
}

/// The cache lives an hour when the response (or the session's `modelUsage` of its model)
/// shows a write with the 1-hour TTL, else five minutes when it shows a 5-minute write, else
/// nothing is known.
#[test]
fn the_cache_ttl_follows_the_writes_seen() {
    let run = |usage: Value, entry: Value| {
        let sid = Uuid::now_v7();
        let mut core = Core::new(sid, false);
        started(&mut core, sid);
        core.on_line(&response_line(1, usage, false));
        context_fields(core.on_line(&turn_result(entry)))[0].2
    };
    // The test runs for far less than a minute, so a TTL read at the end is within a few
    // seconds of its full value.
    let near = |ttl: Option<u64>, full: u64| ttl.is_some_and(|t| t <= full && t + 30 >= full);
    let hour = json!({"cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 33468}});
    let five = json!({"cache_creation": {"ephemeral_5m_input_tokens": 70, "ephemeral_1h_input_tokens": 0}});
    let both = json!({"cache_creation": {"ephemeral_5m_input_tokens": 70, "ephemeral_1h_input_tokens": 5}});
    assert!(near(run(hour, json!({})), 3600));
    assert!(near(run(five.clone(), json!({})), 300));
    assert!(near(run(both, json!({})), 3600));
    assert_eq!(run(json!({}), json!({})), None);
    // A response that wrote nothing: what the session's entry of the model shows.
    let written = |k: &str| json!({k: 12});
    assert!(near(
        run(json!({}), written("cacheCreationEphemeral1hInputTokens")),
        3600
    ));
    assert!(near(
        run(json!({}), written("cacheCreationEphemeral5mInputTokens")),
        300
    ));
    assert!(near(
        run(five, written("cacheCreationEphemeral1hInputTokens")),
        3600
    ));
}

/// The cache is written when the request goes out, not when the turn ends: the TTL sent is
/// what is left since the response started, from its `message_start` when that came first,
/// else from its first line; never below 0.
#[test]
fn the_cache_ttl_counts_from_the_request() {
    let hour =
        json!({"cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 9}});
    let sid = Uuid::now_v7();
    let near = |ttl: Option<u64>, full: u64| ttl.is_some_and(|t| t <= full && t + 30 >= full);
    // A response seen from its first line, which started 100 s before the result.
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    core.on_line(&response_line(1, hour.clone(), false));
    core.rewind_response(Duration::from_secs(100));
    assert!(near(
        context_fields(core.on_line(&turn_result(json!({}))))[0].2,
        3500
    ));
    // The `message_start` came first: its time stands, the later line does not restart it.
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    core.on_line(
        r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"msg_1"}}}"#,
    );
    core.rewind_response(Duration::from_secs(100));
    core.on_line(&response_line(1, hour.clone(), false));
    assert!(near(
        context_fields(core.on_line(&turn_result(json!({}))))[0].2,
        3500
    ));
    // Past the TTL: 0.
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    core.on_line(&response_line(1, hour, false));
    core.rewind_response(Duration::from_secs(4000));
    assert_eq!(
        context_fields(core.on_line(&turn_result(json!({}))))[0].2,
        Some(0)
    );
}

/// The lines of a response carry the usage of its `message_start` (a few output tokens): the
/// context takes the response's own count from its `message_delta`, and only for that response.
#[test]
fn the_context_takes_the_output_count_of_the_message_delta() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    core.on_line(
        r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"msg_1"}}}"#,
    );
    core.on_line(&response_line(
        1,
        json!({"input_tokens": 100, "output_tokens": 4}),
        false,
    ));
    core.on_line(
        r#"{"type":"stream_event","event":{"type":"message_delta","usage":{"output_tokens":500}}}"#,
    );
    assert_eq!(
        context_fields(core.on_line(&turn_result(json!({}))))[0].0,
        Some(600)
    );
    // A delta of another response does not touch it.
    started(&mut core, sid);
    core.on_line(
        r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"msg_9"}}}"#,
    );
    core.on_line(
        r#"{"type":"stream_event","event":{"type":"message_delta","usage":{"output_tokens":900}}}"#,
    );
    assert_eq!(
        context_fields(core.on_line(&turn_result(json!({}))))[0].0,
        Some(600)
    );
}

/// After a compaction the context is the `post_tokens` of its boundary until the next
/// response, and it is in no cache yet: no TTL, though the session's `modelUsage` has writes. A
/// boundary without `post_tokens` (the runtime did not count the size) leaves no size, not the
/// one before.
#[test]
fn the_context_after_a_compaction_is_its_post_tokens() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let hour = json!({"input_tokens": 80_000, "output_tokens": 20,
        "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 80_000}});
    let before = response_line(1, hour, false);
    let entry = json!({"contextWindow": 200_000, "maxOutputTokens": 64_000,
        "cacheCreationEphemeral1hInputTokens": 80_000});
    core.on_line(&before);
    core.on_line(COMPACT_BOUNDARY);
    let got = context_fields(core.on_line(&turn_result(entry)));
    // The model of the response before the boundary is the one whose window gives the limit.
    assert_eq!(got, vec![(Some(9743), Some(167_000), None)]);
    // The next response is the size again, and in the cache.
    core.on_line(&response_line(
        2,
        json!({"input_tokens": 12_000, "output_tokens": 5}),
        false,
    ));
    let got = context_fields(core.on_line(&turn_result(
        json!({"cacheCreationEphemeral1hInputTokens": 5}),
    )));
    assert_eq!((got[0].0, got[0].2.is_some()), (Some(12_005), true));
    // No `post_tokens`: nothing to show for the compaction's `usage`.
    core.on_line(&before);
    core.on_line(&COMPACT_BOUNDARY.replace(r#","post_tokens":9743"#, ""));
    let got = context_fields(core.on_line(&turn_result(json!({}))));
    assert_eq!(got, vec![(None, None, None)]);
}

/// `/clear` starts a new conversation: a turn of it that has only a line of the CLI's own
/// carries nothing of the old conversation's context.
#[test]
fn a_clear_forgets_the_context() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    core.on_line(&response_line(
        1,
        json!({"input_tokens": 90_000, "output_tokens": 5}),
        false,
    ));
    core.on_line(
        &json!({"type": "conversation_reset", "trigger": "clear", "session_id": "s"}).to_string(),
    );
    started(&mut core, sid);
    let synthetic = response_line(2, json!({"input_tokens": 0, "output_tokens": 0}), false)
        .replace("claude-opus-5-5", "<synthetic>");
    core.on_line(&synthetic);
    let got = context_fields(core.on_line(&turn_result(json!({}))));
    assert_eq!(got, vec![(None, None, None)]);
}

/// A cost baseline with `input` and `output` tokens of one model.
fn cost_state(total_cost_usd: f64, input: u64, output: u64) -> hqc_claude_jsonl::CostState {
    hqc_claude_jsonl::CostState {
        total_cost_usd,
        tokens: hqc_claude_jsonl::TokenSums {
            input,
            output,
            ..Default::default()
        },
    }
}

/// The `usage` events among `actions`: input, output, `cache_read`, `cache_write`, `cost_usd`.
#[allow(clippy::type_complexity)]
fn usage_tokens(actions: Vec<Action>) -> Vec<(u64, u64, Option<u64>, Option<u64>, Option<f64>)> {
    events(&emitted(actions))
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::Usage {
                input_tokens,
                output_tokens,
                cache_read,
                cache_write,
                cost_usd,
                ..
            } => Some((
                input_tokens,
                output_tokens,
                cache_read,
                cache_write,
                cost_usd,
            )),
            _ => None,
        })
        .collect()
}

/// A `result` of a compaction (CLI 2.1.312, live): zeros in `usage`, the cumulative `modelUsage`
/// and total cost grown.
fn compaction_result(total: f64, input: u64, output: u64) -> String {
    format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"num_turns":0,"duration_ms":9,"result":"","total_cost_usd":{total},"usage":{{"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}},"modelUsage":{{"claude-haiku-5-5":{{"inputTokens":{input},"outputTokens":{output},"cacheReadInputTokens":0,"cacheCreationInputTokens":0,"costUSD":{total}}}}}}}"#
    )
}

const COMPACT_BOUNDARY: &str = r#"{"type":"system","subtype":"compact_boundary","compact_metadata":{"trigger":"manual","pre_tokens":17040,"post_tokens":9743},"uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e20"}"#;

/// The tokens of a `/compact` are not in its `result.usage` (zeros) but in the growth of the
/// cumulative `modelUsage` (CLI 2.1.312, live on haiku): after a turn that had 16961 in and 79
/// out, the compaction's `result` has 35298 and 1675, so the line reads 18337 in, 1596 out.
#[test]
fn a_compactions_usage_carries_the_growth_of_model_usage() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let turn = usage_tokens(core.on_line(
        r#"{"type":"result","subtype":"success","is_error":false,"num_turns":1,"duration_ms":900,"result":"ok","total_cost_usd":0.0017356,"usage":{"input_tokens":16961,"output_tokens":79},"modelUsage":{"claude-haiku-5-5":{"inputTokens":16961,"outputTokens":79,"cacheReadInputTokens":0,"cacheCreationInputTokens":0,"costUSD":0.0017356}}}"#,
    ));
    assert_eq!(turn, vec![(16961, 79, None, None, Some(0.0017356))]);
    started(&mut core, sid);
    core.on_line(COMPACT_BOUNDARY);
    let compaction = usage_tokens(core.on_line(&compaction_result(0.0043673, 35298, 1675)));
    assert_eq!(
        compaction,
        vec![(18337, 1596, Some(0), Some(0), Some(0.0026317))]
    );
}

/// After `--resume` the baseline of the compaction's tokens is the transcript's last
/// `cost-state`, not zero: the cumulative numbers are not the compaction's.
#[test]
fn a_resumed_compactions_tokens_start_from_the_last_cost_state() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false).with_cost_baseline(Some(cost_state(0.0017356, 16961, 79)));
    started(&mut core, sid);
    core.on_line(COMPACT_BOUNDARY);
    let compaction = usage_tokens(core.on_line(&compaction_result(0.0043673, 35298, 1675)));
    assert_eq!(
        compaction,
        vec![(18337, 1596, Some(0), Some(0), Some(0.0026317))]
    );
}

/// A compaction's tokens are summed over every model of `modelUsage` (a subagent on another
/// model, or the main agent's model switched during the session), and, like its cost, carry
/// what background work spent after the last line with a cost: the total-only `usage` between
/// them moves no baseline.
#[test]
fn a_compactions_tokens_sum_the_models_and_carry_the_background_spending() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    core.on_line(
        r#"{"type":"result","subtype":"success","is_error":false,"num_turns":1,"duration_ms":900,"result":"ok","total_cost_usd":0.0017356,"usage":{"input_tokens":16961,"output_tokens":79},"modelUsage":{"claude-haiku-5-5":{"inputTokens":16961,"outputTokens":79,"cacheReadInputTokens":0,"cacheCreationInputTokens":0,"costUSD":0.0017356}}}"#,
    );
    // A background subagent on another model spent between the turn and the compaction.
    started(&mut core, sid);
    let background = usage_tokens(core.on_line(
        r#"{"type":"result","subtype":"success","is_error":false,"num_turns":0,"duration_ms":3,"result":"","total_cost_usd":0.0027356,"usage":{"input_tokens":0,"output_tokens":0},"modelUsage":{"claude-haiku-5-5":{"inputTokens":16961,"outputTokens":79,"cacheReadInputTokens":0,"cacheCreationInputTokens":0,"costUSD":0.0017356},"claude-opus-5-5":{"inputTokens":100,"outputTokens":50,"cacheReadInputTokens":1000,"cacheCreationInputTokens":200,"costUSD":0.001}}}"#,
    ));
    assert_eq!(background, vec![(0, 0, None, None, None)]);
    started(&mut core, sid);
    core.on_line(COMPACT_BOUNDARY);
    let compaction = usage_tokens(core.on_line(
        r#"{"type":"result","subtype":"success","is_error":false,"num_turns":0,"duration_ms":9,"result":"","total_cost_usd":0.0053673,"usage":{"input_tokens":0,"output_tokens":0},"modelUsage":{"claude-haiku-5-5":{"inputTokens":35298,"outputTokens":1675,"cacheReadInputTokens":0,"cacheCreationInputTokens":0,"costUSD":0.0043673},"claude-opus-5-5":{"inputTokens":100,"outputTokens":50,"cacheReadInputTokens":1000,"cacheCreationInputTokens":200,"costUSD":0.001}}}"#,
    ));
    assert_eq!(
        compaction,
        vec![(18437, 1646, Some(1000), Some(200), Some(0.0036317))]
    );
}

/// `/clear` starts the CLI's `modelUsage` from 0 together with its cost (2.1.312): a compaction
/// after it counts its tokens from the new conversation's sums, not from the larger ones the
/// conversation before the reset left.
#[test]
fn a_compaction_after_clear_counts_its_tokens_from_the_new_conversation() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    core.on_line(
        r#"{"type":"result","subtype":"success","is_error":false,"num_turns":1,"duration_ms":900,"result":"ok","total_cost_usd":0.0017356,"usage":{"input_tokens":16961,"output_tokens":79},"modelUsage":{"claude-haiku-5-5":{"inputTokens":16961,"outputTokens":79,"cacheReadInputTokens":0,"cacheCreationInputTokens":0,"costUSD":0.0017356}}}"#,
    );
    written(&mut core, sid, "/clear");
    core.on_line(
        &json!({"type": "conversation_reset", "trigger": "clear", "session_id": "s"}).to_string(),
    );
    core.on_line(
        r#"{"type":"result","subtype":"success","is_error":false,"num_turns":1,"duration_ms":4,"result":"","local_command":"clear","total_cost_usd":0.0,"usage":{"input_tokens":0,"output_tokens":0}}"#,
    );
    started(&mut core, sid);
    let turn = usage_tokens(core.on_line(
        r#"{"type":"result","subtype":"success","is_error":false,"num_turns":1,"duration_ms":700,"result":"ok","total_cost_usd":0.000525,"usage":{"input_tokens":5000,"output_tokens":50},"modelUsage":{"claude-haiku-5-5":{"inputTokens":5000,"outputTokens":50,"cacheReadInputTokens":0,"cacheCreationInputTokens":0,"costUSD":0.000525}}}"#,
    ));
    assert_eq!(turn, vec![(5000, 50, None, None, Some(0.000525))]);
    started(&mut core, sid);
    core.on_line(COMPACT_BOUNDARY);
    let compaction = usage_tokens(core.on_line(&compaction_result(0.00195, 15000, 900)));
    assert_eq!(
        compaction,
        vec![(10000, 850, Some(0), Some(0), Some(0.001425))]
    );
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

/// The `usage` events among `actions`: tokens in, tokens out, `cost_usd`, `session_cost_usd`.
fn usages(actions: Vec<Action>) -> Vec<(u64, u64, Option<f64>, Option<f64>)> {
    events(&emitted(actions))
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::Usage {
                input_tokens,
                output_tokens,
                cost_usd,
                session_cost_usd,
                ..
            } => Some((input_tokens, output_tokens, cost_usd, session_cost_usd)),
            _ => None,
        })
        .collect()
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
            Action::Emit(AdapterEvent::Commands(c)) => {
                commands.push(c.iter().map(|c| c.name.clone()).collect())
            }
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

/// The main agent's output tokens are the `usage.output_tokens` of each response's
/// `message_delta`, thinking included: on this capture they add up to each turn's
/// `result.usage.output_tokens` (5, then 80, 75 of it thinking). A subagent's stream line
/// (`parent_tool_use_id` set) is not counted: its transcript is.
#[test]
fn output_tokens_are_counted_per_response() {
    let stream =
        include_str!("../../../../hqc-claude-jsonl/fixtures/stream-cost-turns-2.1.308.jsonl");
    let mut core = Core::new(Uuid::now_v7(), false);
    let mut turns = Vec::new();
    for l in stream.lines() {
        core.on_line(l);
        if serde_json::from_str::<Value>(l).is_ok_and(|v| v["type"] == "result") {
            turns.push(core.take_output_tokens());
        }
    }
    assert_eq!(turns, [5, 80]);
    let mut sub: Value = lines(stream)
        .into_iter()
        .find(|v| v["event"]["type"] == "message_delta")
        .unwrap();
    sub["parent_tool_use_id"] = json!("toolu_01");
    core.on_line(&sub.to_string());
    assert_eq!(core.take_output_tokens(), 0);
}

/// A response's tokens count while it is written: its text, thinking summary and tool input
/// at four characters a token, a redacted thinking delta at the CLI's `estimated_tokens`, then
/// its `message_delta` adds only what that missed. A response cut off before its
/// `message_delta` leaves nothing that the next one's count is reduced by.
#[test]
fn output_tokens_count_while_the_response_is_written() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let ev = |e: Value| json!({"type":"stream_event","parent_tool_use_id":null,"event":e});
    let delta = |d: Value| ev(json!({"type":"content_block_delta","index":0,"delta":d}));
    let start = ev(json!({"type":"message_start","message":{}})).to_string();
    core.on_line(&start);
    core.on_line(&delta(json!({"type":"text_delta","text":"x".repeat(4000)})).to_string());
    assert_eq!(core.take_output_tokens(), 1000);
    core.on_line(&start);
    for d in [
        json!({"type":"thinking_delta","thinking":"","estimated_tokens":50}),
        json!({"type":"thinking_delta","thinking":"","estimated_tokens":30}),
        json!({"type":"thinking_delta","thinking":"т".repeat(200)}),
        json!({"type":"text_delta","text":"а".repeat(200)}),
        json!({"type":"input_json_delta","partial_json":"b".repeat(200)}),
    ] {
        core.on_line(&delta(d).to_string());
    }
    assert_eq!(core.take_output_tokens(), 230);
    let end = ev(json!({"type":"message_delta","delta":{},"usage":{"output_tokens":260}}));
    core.on_line(&end.to_string());
    assert_eq!(core.take_output_tokens(), 30);
}

/// After `--resume` the CLI starts from the transcript's last `cost-state`: the resumed
/// turn costs 0.0043361 - 0.00398629. Without the baseline the whole total would be counted
/// again (the "$44.401" of every line in T27's report).
#[test]
fn resumed_turn_cost_starts_from_the_last_cost_state() {
    let fx = fixture!("cost-resume", "2.1.308");
    let baseline = hqc_claude_jsonl::last_cost_state(before_resume(fx.transcript));
    assert_eq!(baseline.map(|b| b.total_cost_usd), Some(0.00398629));
    let mut core = Core::new(Uuid::now_v7(), false).with_cost_baseline(baseline);
    let (costs, _) = costs_and_commands(&play(&mut core, fx.stream));
    assert_eq!(costs, [(0.00034981, 0.0043361)]);

    // A total below the baseline (a CLI that did not carry the cost over) is not negative,
    // and the next turn is counted from the higher of the two.
    let mut core = Core::new(Uuid::now_v7(), false).with_cost_baseline(Some(cost_state(1.0, 0, 0)));
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
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    written(&mut core, sid, "x");
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
        human: false,
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

/// A `task_notification` line carries the notification's `tool_use_id` as `call_id`, else that
/// of the task's `task_started`, its `status` and its `task_id` (made up after the lines of
/// `only_a_background_task_is_a_notification`).
#[test]
fn a_task_notification_carries_its_call_status_and_task() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let stream = r#"{"type":"system","subtype":"task_started","task_id":"a52af2b436ded4cea","tool_use_id":"toolu_01B2anRdzK42h65tA2gdwhb2","description":"Sleep and reply with number","subagent_type":"general-purpose","is_backgrounded":true,"spawn_depth":1,"task_type":"local_agent"}
{"type":"system","subtype":"task_notification","task_id":"a52af2b436ded4cea","status":"failed","summary":"4"}
{"type":"system","subtype":"task_notification","task_id":"bnx80r4bg","tool_use_id":"toolu_01LkxBZReUQu4HTFtoWqC3Nb","status":"stopped","summary":"Sleep for 20 seconds"}"#;
    type Line = (Option<String>, Option<String>, Option<String>);
    let lines: Vec<Line> = play(&mut core, stream)
        .into_iter()
        .filter_map(|a| match a {
            Action::Emit(AdapterEvent::Event(e)) => match e.kind {
                EventKind::System {
                    call_id,
                    status,
                    task_id,
                    ..
                } => Some((call_id, status, task_id)),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let line = |call: &str, status: &str, task: &str| -> Line {
        (Some(call.into()), Some(status.into()), Some(task.into()))
    };
    assert_eq!(
        lines,
        [
            line(
                "toolu_01B2anRdzK42h65tA2gdwhb2",
                "failed",
                "a52af2b436ded4cea"
            ),
            line("toolu_01LkxBZReUQu4HTFtoWqC3Nb", "stopped", "bnx80r4bg")
        ]
    );
}

/// The line of a task's end (SPEC 0.181; lines made up after 2.1.312's code): the
/// notification's `usage`; its `output_file`, when there is one, right after the line.
#[test]
fn a_task_end_carries_its_usage_and_output() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let stream = r#"{"type":"system","subtype":"task_notification","task_id":"a1","tool_use_id":"toolu_A1","status":"completed","output_file":"/tmp/tasks/a1.output","summary":"Fixed both nets.","usage":{"total_tokens":23176,"tool_uses":4,"duration_ms":61000}}
{"type":"system","subtype":"task_notification","task_id":"a2","tool_use_id":"toolu_A2","status":"completed","output_file":"","summary":"Done, 4."}
{"type":"system","subtype":"task_notification","task_id":"bnx80r4bg","tool_use_id":"toolu_B1","status":"failed","output_file":"/tmp/tasks/bnx80r4bg.output","summary":"Background command \"make\" failed with exit code 2"}"#;
    let mut lines = Vec::new();
    for a in play(&mut core, stream) {
        match a {
            Action::Emit(AdapterEvent::Event(Event {
                kind: EventKind::System { text, usage, .. },
                ..
            })) => lines.push(format!("{text} | {usage:?}")),
            Action::Emit(AdapterEvent::TaskOutput(p)) => lines.push(p.display().to_string()),
            _ => {}
        }
    }
    let usage = hqc_proto::event::TaskUsage {
        total_tokens: 23176,
        tool_uses: 4,
        duration_ms: 61000,
    };
    assert_eq!(
        lines,
        [
            format!("Fixed both nets. | {:?}", Some(usage)),
            "/tmp/tasks/a1.output".into(),
            "Done, 4. | None".into(),
            "Background command \"make\" failed with exit code 2 | None".into(),
            "/tmp/tasks/bnx80r4bg.output".into(),
        ]
    );
}

/// A task the CLI keeps out of the transcript (`skip_transcript: true`; 2.1.312 sends it so for
/// its own tasks, with `ambient: true`) is no activity: no line, no output.
#[test]
fn a_task_kept_out_of_the_transcript_gives_no_line() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let a = play(
        &mut core,
        r#"{"type":"system","subtype":"task_notification","task_id":"m1","status":"completed","output_file":"/tmp/tasks/m1.output","summary":"saving memories","skip_transcript":true,"ambient":true}"#,
    );
    assert!(
        !a.iter().any(|a| matches!(
            a,
            Action::Emit(AdapterEvent::Event(_) | AdapterEvent::TaskOutput(_))
        )),
        "{a:?}"
    );
}

/// An `ambient` task (2.1.312: in the CLI's list and in its `task_started`, a task the SDK says
/// is no activity) is not counted in the background tasks, is no running subagent and no row
/// above the input; the agent's command beside it is all three.
#[test]
fn an_ambient_task_is_no_activity() {
    let lines = [
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"amem","task_type":"post_turn_memory","description":"saving memories","ambient":true},{"task_id":"afork","task_type":"local_agent","description":"Fork worker","ambient":true},{"task_id":"b2st7oc3z","task_type":"local_bash","description":"Run the full pass"}]}"#,
        r#"{"type":"system","subtype":"task_started","task_id":"afork","tool_use_id":"toolu_01Fork","description":"Fork worker","is_backgrounded":true,"task_type":"local_agent","skip_transcript":true,"ambient":true}"#,
        r#"{"type":"system","subtype":"task_started","task_id":"b2st7oc3z","tool_use_id":"toolu_01Run","description":"Run the full pass","is_backgrounded":true,"task_type":"local_bash"}"#,
    ];
    let want: [&[&str]; 3] = [&["background_tasks:1"], &[], &[]];
    assert_eq!(task_counts(lines.into_iter()), want);
    let row = live_row("b2st7oc3z", "Run the full pass", None);
    assert_eq!(
        live_lists(lines.into_iter()),
        [vec![], vec![], vec![vec![row]]]
    );
}

/// The tasks of an `ambient` one (2.1.312 runs a skill with `context: fork` as an `ambient`
/// `local_agent`, and its tasks name it as their `parent_task_id`) are the agent's own: its
/// subagent counts and has its row, its command has its row, and it counts as no subagent;
/// its `task_started` or its entry in the list alone says it is `ambient`.
#[test]
fn the_tasks_of_an_ambient_one_are_the_agents() {
    let start = r#"{"type":"system","subtype":"task_started","task_id":"afork","tool_use_id":"toolu_01Fork","description":"/review","is_backgrounded":true,"task_type":"local_agent","skip_transcript":true,"ambient":true}"#;
    let fork =
        r#"{"task_id":"afork","task_type":"local_agent","description":"/review","ambient":true},"#;
    let listed = r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"afork","task_type":"local_agent","description":"/review","ambient":true},{"task_id":"achild","task_type":"local_agent","description":"Check the board","parent_task_id":"afork"},{"task_id":"bchild","task_type":"local_bash","description":"Run the full pass","parent_task_id":"afork"}]}"#;
    let unlisted = listed.replace(fork, "");
    let child = r#"{"type":"system","subtype":"task_started","task_id":"achild","tool_use_id":"toolu_01Chk","description":"Check the board","is_backgrounded":true,"task_type":"local_agent","parent_task_id":"afork"}"#;
    let command = r#"{"type":"system","subtype":"task_started","task_id":"bchild","tool_use_id":"toolu_01Run","description":"Run the full pass","is_backgrounded":true,"task_type":"local_bash"}"#;
    let counts: [&[&str]; 4] = [&[], &["background_tasks:2", "subagents:1"], &[], &[]];
    let subagent = live_row("achild", "Check the board", None);
    let lists = [
        vec![],
        vec![vec![live_row("achild", "", None)]],
        vec![vec![subagent.clone()]],
        vec![vec![
            subagent,
            live_row("bchild", "Run the full pass", None),
        ]],
    ];
    // Both say it; the list alone; the `task_started` alone.
    for (lines, from) in [
        (vec![start, listed, child, command], 0),
        (vec![listed, child, command], 1),
        (vec![start, unlisted.as_str(), child, command], 0),
    ] {
        assert_eq!(
            task_counts(lines.iter().copied()),
            counts[from..],
            "{lines:?}"
        );
        assert_eq!(
            live_lists(lines.iter().copied()),
            lists[from..],
            "{lines:?}"
        );
    }
}

/// A request of a subagent names it (SPEC 0.181): the CLI's `agent_id` is the subagent's
/// `task_id` (2.1.312), and its task is the `description` of its `task_started`. The agent's
/// own request, and one of a subagent the adapter has not seen start, name none.
#[test]
fn a_subagents_request_names_it() {
    let fx = fixture!("permission-allow");
    let mut core = Core::new(Uuid::now_v7(), false);
    play(
        &mut core,
        r#"{"type":"system","subtype":"task_started","task_id":"a8b8fdc3b4a031d4f","tool_use_id":"toolu_01BR8iav6ZtPUAhpuCWdQPCt","description":"Touch hqc-live-check file","is_backgrounded":true,"task_type":"local_agent"}"#,
    );
    let asked = |core: &mut Core, line: Value| {
        emitted(core.on_line(&line.to_string()))
            .into_iter()
            .find_map(|e| match e {
                AdapterEvent::Event(Event {
                    kind:
                        EventKind::ApprovalRequest { subagent, .. }
                        | EventKind::ClarifyRequest { subagent, .. },
                    ..
                }) => Some(subagent.map(|s| (s.task_id, s.description))),
                _ => None,
            })
            .expect("a request")
    };
    let named = Some((
        "a8b8fdc3b4a031d4f".to_string(),
        "Touch hqc-live-check file".to_string(),
    ));
    let mut v: Value = serde_json::from_str(&can_use_tool_line(&fx, "r1", None)).unwrap();
    assert_eq!(asked(&mut core, v.clone()), None);
    v["request_id"] = json!("r2");
    v["request"]["agent_id"] = json!("a8b8fdc3b4a031d4f");
    assert_eq!(asked(&mut core, v.clone()), named);
    v["request_id"] = json!("r3");
    v["request"]["agent_id"] = json!("aunknown");
    assert_eq!(asked(&mut core, v), None);
    let q = json!({"type": "control_request", "request_id": "q1", "request": {
        "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "agent_id": "a8b8fdc3b4a031d4f",
        "input": {"questions": [{"question": "Which net?", "header": "Net", "multiSelect": false,
            "options": [{"label": "VCC"}, {"label": "GND"}]}]}}});
    assert_eq!(asked(&mut core, q), named);
}

/// An `Agent` call's `args_preview` has its `prompt` last: with a prompt past the 4 KB cut the
/// sorted keys lost `subagent_type` and `run_in_background`, which the subagent's card shows.
/// The other fields go from the shortest value to the longest.
#[test]
fn an_agent_calls_prompt_goes_last_in_its_preview() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let input = json!({"description": "Fix the nets", "prompt": "x".repeat(5000),
        "subagent_type": "general-purpose", "run_in_background": true});
    let line = json!({"type": "assistant", "parent_tool_use_id": null,
        "uuid": "6f1d0c3e-3c2f-4a55-9d0e-1a2b3c4d5e70",
        "message": {"model": "claude-haiku-5-5", "role": "assistant", "content": [
            {"type": "tool_use", "id": "toolu_A1", "name": "Agent", "input": input}]}});
    let preview = emitted(core.on_line(&line.to_string()))
        .into_iter()
        .find_map(|e| match e {
            AdapterEvent::Event(Event {
                kind: EventKind::ToolCall { args_preview, .. },
                ..
            }) => Some(args_preview),
            _ => None,
        })
        .unwrap();
    assert_eq!(preview.len(), super::engine::PREVIEW_LIMIT);
    assert!(
        preview.starts_with(
            r#"{"run_in_background":true,"description":"Fix the nets","subagent_type":"general-purpose","prompt":"xxx"#
        ),
        "{}",
        &preview[..120]
    );
}

/// A call's `args_preview` past the 4 KB cut keeps its short fields whole: they go before the
/// longest value. With the keys sorted a Write's `file_path` came after its `content` and was
/// lost, so its row showed just "Write"; a Bash script took its `description` and `timeout`.
#[test]
fn a_cut_preview_keeps_the_short_fields() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let line = json!({"type": "assistant", "parent_tool_use_id": null,
        "uuid": "6f1d0c3e-3c2f-4a55-9d0e-1a2b3c4d5e71",
        "message": {"model": "claude-haiku-5-5", "role": "assistant", "content": [
            {"type": "tool_use", "id": "toolu_W1", "name": "Write",
                "input": {"content": "x".repeat(5000), "file_path": "/w/marks.txt"}},
            {"type": "tool_use", "id": "toolu_B1", "name": "Bash",
                "input": {"command": "y".repeat(5000), "description": "Check the ruler",
                    "timeout": 120000}}]}});
    let previews: Vec<String> = emitted(core.on_line(&line.to_string()))
        .into_iter()
        .filter_map(|e| match e {
            AdapterEvent::Event(Event {
                kind: EventKind::ToolCall { args_preview, .. },
                ..
            }) => Some(args_preview),
            _ => None,
        })
        .collect();
    assert_eq!(previews.len(), 2);
    assert!(
        previews[0].starts_with(r#"{"file_path":"/w/marks.txt","content":"xxx"#),
        "{}",
        &previews[0][..80]
    );
    assert!(
        previews[1]
            .starts_with(r#"{"timeout":120000,"description":"Check the ruler","command":"yyy"#),
        "{}",
        &previews[1][..80]
    );
    assert!(previews
        .iter()
        .all(|p| p.len() == super::engine::PREVIEW_LIMIT));
    // Two long values keep their sorted order: an Edit's `new_string` before its `old_string`,
    // so the start of the new text comes, not only the old one.
    let edit = super::engine::args_preview(
        "Edit",
        &json!({"file_path": "/w/a.rs", "old_string": "o".repeat(4500),
            "new_string": "n".repeat(5000)}),
    );
    assert!(
        edit.starts_with(r#"{"file_path":"/w/a.rs","new_string":"nnn"#),
        "{}",
        &edit[..60]
    );
    // An Agent's prompt goes last however short it is.
    assert_eq!(
        super::engine::args_preview(
            "Agent",
            &json!({"description": "Fix", "prompt": "Go", "subagent_type": "general-purpose"}),
        ),
        r#"{"description":"Fix","subagent_type":"general-purpose","prompt":"Go"}"#
    );
}

/// The rule of the VS Code tab's "Run in background" (extension 2.1.319; the first lines are of
/// live runs on 2.1.312, the `killed` one, the nested subagent, the task started in the
/// background, the ones with `skip_transcript` or `monitor_mcp` and the task started again are
/// made up after the CLI's code): a Bash call or a subagent of the agent itself whose
/// `task_started` says it runs in the foreground is a foreground call, by its `tool_use_id`,
/// until its `task_notification`, the CLI's move to the background or a `task_updated` that
/// ends it, or a `task_started` of it in the background; a call of a subagent (its Bash call,
/// its subagent), a task the CLI starts in the background, one without a transcript and one of
/// another type are none.
#[test]
fn foreground_calls_are_reported() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let stream = r#"{"type":"system","subtype":"task_started","task_id":"bfis28nrx","tool_use_id":"toolu_01FoCFY4DmUbVxXAembc44PX","description":"Sleep for 15 seconds","is_backgrounded":false,"task_type":"local_bash","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e01"}
{"type":"system","subtype":"task_started","task_id":"b6yusn86f","owned_by_subagent":true,"tool_use_id":"toolu_01T7e2mWErkbKbbTuuN4vYrm","description":"Sleep for 6 seconds","is_backgrounded":false,"parent_task_id":"a52af2b436ded4cea","task_type":"local_bash","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e04"}
{"type":"system","subtype":"task_started","task_id":"a7c3e9d1b2f4a6c80","tool_use_id":"toolu_01NstdAgntSpwndBySbgnt1","description":"Nested subagent","subagent_type":"general-purpose","is_backgrounded":false,"spawn_depth":2,"parent_task_id":"a52af2b436ded4cea","task_type":"local_agent","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e21"}
{"type":"system","subtype":"task_started","task_id":"bbgr0001","tool_use_id":"toolu_01BckgrndBashFromStart01","description":"Background from the start","is_backgrounded":true,"task_type":"local_bash","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e22"}
{"type":"system","subtype":"task_started","task_id":"bskp0001","tool_use_id":"toolu_01SkpTrnscrptBash000001","description":"Skipped transcript","is_backgrounded":false,"skip_transcript":true,"task_type":"local_bash","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e23"}
{"type":"system","subtype":"task_started","task_id":"mmcp0001","tool_use_id":"toolu_01MonitorMcpTask00000001","description":"A monitor","is_backgrounded":false,"task_type":"monitor_mcp","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e24"}
{"type":"system","subtype":"task_notification","task_id":"bfis28nrx","tool_use_id":"toolu_01FoCFY4DmUbVxXAembc44PX","status":"completed","output_file":"","summary":"Sleep for 15 seconds","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e02"}
{"type":"system","subtype":"task_started","task_id":"bnx80r4bg","tool_use_id":"toolu_01LkxBZReUQu4HTFtoWqC3Nb","description":"Sleep for 20 seconds","is_backgrounded":false,"task_type":"local_bash","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e09"}
{"type":"system","subtype":"task_updated","task_id":"bnx80r4bg","patch":{"is_backgrounded":true},"uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e0a"}
{"type":"system","subtype":"task_started","task_id":"brst0001","tool_use_id":"toolu_01RestartedInBackground01","description":"Started again","is_backgrounded":false,"task_type":"local_bash","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e25"}
{"type":"system","subtype":"task_started","task_id":"brst0001","tool_use_id":"toolu_01RestartedInBackground01","description":"Started again","is_backgrounded":true,"task_type":"local_bash","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e26"}
{"type":"system","subtype":"task_started","task_id":"ac5672f5fc80df8ec","tool_use_id":"toolu_01TYRCeySHmvXd7Md3Y1rgb8","description":"Run ping and reply with 4","subagent_type":"general-purpose","is_backgrounded":false,"spawn_depth":1,"task_type":"local_agent","uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e0d"}
{"type":"system","subtype":"task_updated","task_id":"ac5672f5fc80df8ec","patch":{"status":"killed","end_time":1793889489540},"uuid":"0f6c1d2e-1111-4a55-9d0e-1a2b3c4d5e10"}"#;
    let calls: Vec<Vec<String>> = play(&mut core, stream)
        .into_iter()
        .filter_map(|a| match a {
            Action::Emit(AdapterEvent::ForegroundCalls(c)) => Some(c),
            _ => None,
        })
        .collect();
    let one = |id: &str| vec![id.to_string()];
    assert_eq!(
        calls,
        [
            one("toolu_01FoCFY4DmUbVxXAembc44PX"),
            vec![],
            one("toolu_01LkxBZReUQu4HTFtoWqC3Nb"),
            vec![],
            one("toolu_01RestartedInBackground01"),
            vec![],
            one("toolu_01TYRCeySHmvXd7Md3Y1rgb8"),
            vec![],
        ]
    );
}

/// "Run in background": `control_request {subtype: background_tasks, tool_use_id}`; the CLI's
/// answer, `{backgrounded}` or an error, gives no action.
#[test]
fn background_asks_the_cli_to_move_the_call() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let Action::Write(v) = core.background("toolu_01LkxBZReUQu4HTFtoWqC3Nb") else {
        panic!("not a write");
    };
    assert_eq!(v["type"], "control_request");
    assert_eq!(
        v["request"],
        json!({"subtype": "background_tasks", "tool_use_id": "toolu_01LkxBZReUQu4HTFtoWqC3Nb"})
    );
    let id = v["request_id"].as_str().unwrap();
    assert!(id.starts_with("hqc-background-"), "{id}");
    for answer in [
        json!({"subtype": "success", "request_id": id, "response": {"backgrounded": true}}),
        json!({"subtype": "error", "request_id": id, "error": "Background tasks are disabled"}),
    ] {
        let line = json!({"type": "control_response", "response": answer});
        assert!(core.on_line(&line.to_string()).is_empty());
    }
}

/// The stop button of a row: `control_request {subtype: stop_task, task_id}`; the
/// CLI's answer, `{}` or an error, gives no action.
#[test]
fn stop_task_asks_the_cli_to_stop_the_task() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let Action::Write(v) = core.stop_task("a52af2b436ded4cea") else {
        panic!("not a write");
    };
    assert_eq!(v["type"], "control_request");
    assert_eq!(
        v["request"],
        json!({"subtype": "stop_task", "task_id": "a52af2b436ded4cea"})
    );
    let id = v["request_id"].as_str().unwrap();
    assert!(id.starts_with("hqc-stop-task-"), "{id}");
    for answer in [
        json!({"subtype": "success", "request_id": id, "response": {}}),
        json!({"subtype": "error", "request_id": id, "error": "stop_task: task_id must be a string"}),
    ] {
        let line = json!({"type": "control_response", "response": answer});
        assert!(core.on_line(&line.to_string()).is_empty());
    }
}

/// A subagent's row and the agent's background command beside it are both `stoppable`; the
/// subagent stopped alone ends as any end of it (the lines after the stop are made up after the
/// CLI's code, 2.1.312: `task_updated` `killed`, the list without it, `task_notification`
/// `stopped`): its row goes, the command's stays, and its "Background task" line comes.
#[test]
fn a_subagent_row_and_a_command_row_are_stoppable() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let lines = [
        r#"{"type":"system","subtype":"task_started","task_id":"a52af2b436ded4cea","tool_use_id":"toolu_01B2anRdzK42h65tA2gdwhb2","description":"Sleep and reply with number","subagent_type":"general-purpose","is_backgrounded":true,"spawn_depth":1,"task_type":"local_agent"}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a52af2b436ded4cea","task_type":"local_agent","description":"Sleep and reply with number"},{"task_id":"b2st7oc3z","task_type":"local_bash","description":"Run the full pass over 1000 boards on cirunner"}]}"#,
        r#"{"type":"system","subtype":"task_started","task_id":"b2st7oc3z","tool_use_id":"toolu_01Run","description":"Run the full pass over 1000 boards on cirunner","is_backgrounded":true,"task_type":"local_bash"}"#,
    ];
    let mut last = None;
    for l in lines {
        for a in core.on_line(l) {
            if let Action::Emit(AdapterEvent::LiveSubagents(list)) = a {
                last = Some(list);
            }
        }
    }
    let rows: Vec<(String, bool)> = last
        .unwrap()
        .into_iter()
        .map(|s| (s.task_id, s.stoppable))
        .collect();
    assert_eq!(
        rows,
        [
            ("a52af2b436ded4cea".to_string(), true),
            ("b2st7oc3z".to_string(), true)
        ]
    );

    let stopped = [
        r#"{"type":"system","subtype":"task_updated","task_id":"a52af2b436ded4cea","patch":{"status":"killed","end_time":1793889489540}}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"b2st7oc3z","task_type":"local_bash","description":"Run the full pass over 1000 boards on cirunner"}]}"#,
        r#"{"type":"system","subtype":"task_notification","task_id":"a52af2b436ded4cea","tool_use_id":"toolu_01B2anRdzK42h65tA2gdwhb2","status":"stopped","output_file":"","summary":"Sleep and reply with number","uuid":"0f6c1d2e-6666-4a55-9d0e-1a2b3c4d5e01"}"#,
    ];
    let mut out = Vec::new();
    for l in stopped {
        out.extend(core.on_line(l));
    }
    let lists: Vec<Vec<String>> = out
        .iter()
        .filter_map(|a| match a {
            Action::Emit(AdapterEvent::LiveSubagents(list)) => {
                Some(list.iter().map(|s| s.task_id.clone()).collect())
            }
            _ => None,
        })
        .collect();
    assert_eq!(lists.last().unwrap(), &["b2st7oc3z".to_string()]);
    assert_eq!(
        system_events(&out),
        [(
            "task_notification".into(),
            "Sleep and reply with number".into()
        )]
    );
}

/// `/compact` (lines of fixture `stream-resume-compact`, 2.1.308; the same `compact_boundary`
/// came on 2.1.312): `system context_compacted` with the summary of the synthetic `user` line
/// after it as its text, its id derived from the line's uuid, which is the transcript record's.
#[test]
fn compact_boundary_is_a_system_event() {
    let fx = fixture!("resume-compact", "2.1.308");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let a = play(&mut core, fx.stream);
    let summary = lines(fx.stream)
        .into_iter()
        .find(|v| v["isSynthetic"] == true)
        .unwrap()["message"]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(summary.contains("TWO"), "{summary}");
    assert_eq!(system_events(&a), [("context_compacted".into(), summary)]);
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

/// A compaction whose next line of the conversation is no synthetic `user` line (the lines of
/// fixture `stream-resume-compact` without the summary): "Context compacted".
#[test]
fn compact_boundary_without_a_summary_has_the_fixed_text() {
    let fx = fixture!("resume-compact", "2.1.308");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let a: Vec<Action> = lines(fx.stream)
        .into_iter()
        .filter(|v| v["isSynthetic"] != true)
        .flat_map(|v| core.on_line(&v.to_string()))
        .collect();
    assert_eq!(
        system_events(&a),
        [("context_compacted".into(), "Context compacted".into())]
    );
}

/// A process that ends between the boundary and the summary (the lines of fixture
/// `stream-resume-compact` up to the summary, then the exit): the held compaction comes out
/// with "Context compacted".
#[test]
fn a_compaction_held_at_the_process_end_has_the_fixed_text() {
    let fx = fixture!("resume-compact", "2.1.308");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let mut a: Vec<Action> = lines(fx.stream)
        .into_iter()
        .take_while(|v| v["isSynthetic"] != true)
        .flat_map(|v| core.on_line(&v.to_string()))
        .collect();
    assert!(system_events(&a).is_empty());
    a.extend(core.on_exit());
    assert_eq!(
        system_events(&a),
        [("context_compacted".into(), "Context compacted".into())]
    );
}

/// A subagent's `user` line and a `system` line between the boundary and the summary keep the
/// compaction held: its text is still the summary.
#[test]
fn a_subagent_line_does_not_release_a_held_compaction() {
    let fx = fixture!("resume-compact", "2.1.308");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let all = lines(fx.stream);
    let at = all.iter().position(|v| v["isSynthetic"] == true).unwrap();
    let between = [
        json!({"type": "user", "parent_tool_use_id": "toolu_x",
            "message": {"role": "user", "content": "x"}, "uuid": Uuid::new_v4().to_string()}),
        json!({"type": "system", "subtype": "status", "status": null}),
    ];
    let a: Vec<Action> = all[..at]
        .iter()
        .chain(&between)
        .chain(&all[at..])
        .flat_map(|v| core.on_line(&v.to_string()))
        .collect();
    let summary = all[at]["message"]["content"].as_str().unwrap().to_owned();
    assert_eq!(system_events(&a), [("context_compacted".into(), summary)]);
}

/// `/compact` (fixture `stream-resume-compact`, 2.1.308): `system context_compacted` carries
/// the `trigger`, `pre_tokens` and `post_tokens` of the boundary's `compact_metadata`.
#[test]
fn a_compaction_carries_its_trigger_and_tokens() {
    let fx = fixture!("resume-compact", "2.1.308");
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let compaction = play(&mut core, fx.stream)
        .into_iter()
        .find_map(|a| match a {
            Action::Emit(AdapterEvent::Event(e)) => match e.kind {
                EventKind::System {
                    code, compaction, ..
                } if code == "context_compacted" => Some(compaction),
                _ => None,
            },
            _ => None,
        })
        .unwrap();
    assert_eq!(
        compaction,
        Some(ContextCompacted {
            trigger: "manual".into(),
            pre_tokens: 22240,
            post_tokens: Some(4507),
        })
    );
}

/// SPEC §4, §8.1: a compaction that failed (`status` with `compact_result: failed`; live on
/// 2.1.312 at 165 % of the window it came with `compact_error: too_few_groups`) is `system
/// compaction_failed` "Compaction failed: <error>", "Compaction failed" without the error; one
/// that went through gives no line of its own.
#[test]
fn a_failed_compaction_is_a_line() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let a = play(
        &mut core,
        concat!(
            r#"{"type":"system","subtype":"status","status":"compacting"}"#,
            "\n",
            r#"{"type":"system","subtype":"status","status":null,"compact_result":"failed","compact_error":"too_few_groups"}"#,
            "\n",
            r#"{"type":"system","subtype":"status","status":null,"compact_result":"failed"}"#,
            "\n",
            r#"{"type":"system","subtype":"status","status":null,"compact_result":"success"}"#,
        ),
    );
    assert_eq!(
        system_events(&a),
        [
            (
                "compaction_failed".into(),
                "Compaction failed: too_few_groups".into()
            ),
            ("compaction_failed".into(), "Compaction failed".into())
        ]
    );
}

/// SPEC §8.1: what the agent waits on, line by line (lines after the CLI's code, 2.1.312:
/// `api_retry` cli:768796, `thinking_tokens` before each thinking delta cli:768702, `status`
/// cli:768731): set by `api_retry`, `status compacting` and `thinking_tokens`, ended by a
/// response that starts, a block that starts or ends, another `status` that is no report of
/// the permission mode, the agent's `assistant` line and `result`. A thinking delta, `ping`, a
/// subagent's line and a report of the mode leave it.
#[test]
fn the_activity_follows_the_stream() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let retry = |attempt, error_status| {
        Some(Activity::Retrying(ApiRetry {
            attempt,
            max_retries: 10,
            retry_delay_ms: 8000,
            error_status,
        }))
    };
    let thinking = |estimated_tokens| Some(Activity::Thinking { estimated_tokens });
    let main =
        |event: Value| json!({"type": "stream_event", "parent_tool_use_id": null, "event": event});
    let steps: Vec<(Value, Vec<Option<Activity>>)> = vec![
        (
            json!({"type": "system", "subtype": "api_retry", "attempt": 3, "max_retries": 10,
                "retry_delay_ms": 8000, "error_status": 529, "error": "overloaded"}),
            vec![retry(3, Some(529))],
        ),
        (
            json!({"type": "system", "subtype": "api_retry", "attempt": 4, "max_retries": 10,
                "retry_delay_ms": 8000, "error_status": null, "error": "unknown"}),
            vec![retry(4, None)],
        ),
        (
            json!({"type": "stream_event", "parent_tool_use_id": "toolu_sub",
                "event": {"type": "message_start", "message": {}}}),
            vec![],
        ),
        (
            main(json!({"type": "message_start", "message": {}})),
            vec![None],
        ),
        (
            main(json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "thinking", "thinking": ""}})),
            vec![],
        ),
        (
            json!({"type": "system", "subtype": "thinking_tokens", "estimated_tokens": 12,
                "estimated_tokens_delta": 12}),
            vec![thinking(12)],
        ),
        (
            main(json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "thinking_delta", "thinking": "Let me look"}})),
            vec![],
        ),
        (main(json!({"type": "ping"})), vec![]),
        // A new estimate within 2 s of the last one reported waits.
        (
            json!({"type": "system", "subtype": "thinking_tokens", "estimated_tokens": 40,
                "estimated_tokens_delta": 28}),
            vec![],
        ),
        (
            main(json!({"type": "content_block_stop", "index": 0})),
            vec![None],
        ),
        (
            json!({"type": "system", "subtype": "thinking_tokens", "estimated_tokens": 5,
                "estimated_tokens_delta": 5}),
            vec![thinking(5)],
        ),
        (
            json!({"type": "assistant", "parent_tool_use_id": null,
                "uuid": "0f6c1d2e-9999-4a55-9d0e-1a2b3c4d5e01",
                "message": {"model": "claude-opus-5-5", "role": "assistant",
                    "content": [{"type": "thinking", "thinking": "Done.", "signature": "s"}]}}),
            vec![None],
        ),
        (
            json!({"type": "system", "subtype": "status", "status": "compacting"}),
            vec![Some(Activity::Compacting)],
        ),
        (
            json!({"type": "system", "subtype": "status", "status": null,
                "permissionMode": "default"}),
            vec![],
        ),
        (
            json!({"type": "system", "subtype": "status", "status": null,
                "compact_result": "success"}),
            vec![None],
        ),
        (
            json!({"type": "system", "subtype": "status", "status": "compacting"}),
            vec![Some(Activity::Compacting)],
        ),
        (
            json!({"type": "system", "subtype": "status", "status": "requesting"}),
            vec![None],
        ),
        (
            json!({"type": "system", "subtype": "api_retry", "attempt": 1, "max_retries": 10,
                "retry_delay_ms": 8000, "error_status": 529, "error": "overloaded"}),
            vec![retry(1, Some(529))],
        ),
        (
            json!({"type": "result", "subtype": "success", "is_error": false, "num_turns": 1,
                "result": "", "total_cost_usd": 0.01,
                "usage": {"input_tokens": 1, "output_tokens": 1}}),
            vec![None],
        ),
    ];
    for (line, want) in steps {
        let got: Vec<Option<Activity>> = core
            .on_line(&line.to_string())
            .into_iter()
            .filter_map(|a| match a {
                Action::Emit(AdapterEvent::Activity(a)) => Some(a),
                _ => None,
            })
            .collect();
        assert_eq!(got, want, "{line}");
    }
}

/// SPEC §4, §8.1: the CLI's and its hooks' messages (`system/informational`, fields after
/// cli:108274, levels `info`, `notice`, `suggestion`, `warning`; `system/notification`,
/// cli:768716) are `system runtime_notice` lines with their text; `info` (shown only in the
/// transcript view of the terminal) and an empty text give none.
#[test]
fn cli_notices_are_lines() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let a = play(
        &mut core,
        concat!(
            r#"{"type":"system","subtype":"informational","content":"Stop hook says: run the tests first","level":"warning","prevent_continuation":true}"#,
            "\n",
            r#"{"type":"system","subtype":"informational","content":"Loaded 3 skills","level":"info"}"#,
            "\n",
            r#"{"type":"system","subtype":"informational","content":"  ","level":"notice"}"#,
            "\n",
            r#"{"type":"system","subtype":"informational","content":"Settings applied in part","level":"notice"}"#,
            "\n",
            r#"{"type":"system","subtype":"notification","key":"account-plugins-not-reloaded","text":"Couldn't finish reloading your plugins","priority":"medium","color":"warning"}"#,
        ),
    );
    assert_eq!(
        system_events(&a),
        [
            (
                "runtime_notice".into(),
                "Stop hook says: run the tests first".into()
            ),
            ("runtime_notice".into(), "Settings applied in part".into()),
            (
                "runtime_notice".into(),
                "Couldn't finish reloading your plugins".into()
            ),
        ]
    );
}

/// SPEC §8.1: the CLI's Stop-hook notices give no line, as in the VS Code tab and the desktop
/// app — neither `stop-hook-error` (cli:756279, cli:756784: a Stop hook blocked the end of the
/// turn or failed) nor `stop-hook-block-cap`, which repeats the `informational` line of the same cap
/// (cli:762106), the one line that stays; nor, as in the desktop app, "Stop hook failed: " with
/// no reason (cli:756301), while one with a reason stays.
#[test]
fn stop_hook_notices_are_not_lines() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let cap = "A hook blocked the turn from ending 9 consecutive times \u{2014} overriding and ending turn.";
    let a = play(
        &mut core,
        &[
            r#"{"type":"system","subtype":"notification","key":"stop-hook-error","text":"Stop hook error occurred · ctrl+o to see","priority":"immediate"}"#.to_owned(),
            format!(r#"{{"type":"system","subtype":"informational","content":"{cap}","level":"warning"}}"#),
            format!(r#"{{"type":"system","subtype":"notification","key":"stop-hook-block-cap","text":"{cap}","priority":"high","color":"warning"}}"#),
            r#"{"type":"system","subtype":"informational","content":"Stop hook failed: ","level":"warning"}"#.to_owned(),
            r#"{"type":"system","subtype":"informational","content":"Stop hook failed: spawn ENOENT","level":"warning"}"#.to_owned(),
        ]
        .join("\n"),
    );
    assert_eq!(
        system_events(&a),
        [
            ("runtime_notice".into(), cap.into()),
            (
                "runtime_notice".into(),
                "Stop hook failed: spawn ENOENT".into()
            ),
        ]
    );
}

/// SPEC §4, §8.1: the CLI went on with another model (`model_fallback`, `model_consent_fallback`,
/// fields after cli:108527): a line "<model> → <fallback>: <the CLI's words>", and the turn's
/// `usage` names the model that answered; a refusal with no other model
/// (`model_refusal_no_fallback`, sent with an empty `content`, cli:760131) is a line too.
#[test]
fn a_model_fallback_is_a_line_and_names_the_usage() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let a = play(
        &mut core,
        concat!(
            r#"{"type":"system","subtype":"init","model":"claude-opus-5-5","permissionMode":"default","session_id":"s"}"#,
            "\n",
            r#"{"type":"system","subtype":"model_fallback","trigger":"overloaded","original_model":"claude-opus-5-5","fallback_model":"claude-sonnet-5","content":"Switched to Sonnet 5 due to high demand for Opus 5.5"}"#,
            "\n",
            r#"{"type":"system","subtype":"model_refusal_no_fallback","original_model":"claude-sonnet-5","request_id":null,"api_refusal_category":null,"api_refusal_explanation":"policy","refused_user_message_uuid":null,"content":""}"#,
            "\n",
            r#"{"type":"system","subtype":"model_refusal_no_fallback","original_model":"claude-sonnet-5","content":"Sonnet 5 declined this request."}"#,
            "\n",
            r#"{"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"","total_cost_usd":0.01,"usage":{"input_tokens":3,"output_tokens":4}}"#,
        ),
    );
    assert_eq!(
        system_events(&a),
        [
            (
                "model_fallback".into(),
                "claude-opus-5-5 → claude-sonnet-5: Switched to Sonnet 5 due to high demand for Opus 5.5".into()
            ),
            (
                "model_refusal".into(),
                "claude-sonnet-5 refused to answer: policy".into()
            ),
            ("model_refusal".into(), "Sonnet 5 declined this request.".into()),
        ]
    );
    let model = a.iter().find_map(|a| match a {
        Action::Emit(AdapterEvent::Event(e)) => match &e.kind {
            EventKind::Usage { model, .. } => Some(model.clone()),
            _ => None,
        },
        _ => None,
    });
    assert_eq!(model, Some(Some("claude-sonnet-5".into())));
}

/// SPEC §4, §8.1: a model refused and another answered (2.1.312: the new answer's `assistant`
/// names in `supersedes` the wire uuids of what it replaces, cli:107172; the turn's
/// `model_refusal_fallback` repeats them in `retracted_message_uuids`, cli:760380): one
/// `retraction` of the events of those records, before the new answer, a held text among them
/// never sent; the notice retracts nothing again and is a line.
#[test]
fn a_refused_answer_is_retracted() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let [t, c, r, h, n] = std::array::from_fn::<Uuid, 5, _>(|_| Uuid::new_v4());
    let assistant = |u: Uuid, content: Value, supersedes: &[Uuid]| {
        let mut v = json!({"type": "assistant", "parent_tool_use_id": null, "uuid": u,
            "message": {"model": "claude-opus-5-5", "role": "assistant", "content": content}});
        if !supersedes.is_empty() {
            v["supersedes"] = json!(supersedes);
        }
        v.to_string()
    };
    let mut a = play(
        &mut core,
        &[
            assistant(t, json!([{"type": "text", "text": "Let me check."}]), &[]),
            assistant(
                c,
                json!([{"type": "tool_use", "id": "toolu_r", "name": "Bash", "input": {}}]),
                &[],
            ),
            json!({"type": "user", "parent_tool_use_id": null, "uuid": r,
                "message": {"role": "user", "content": [{"type": "tool_result",
                    "tool_use_id": "toolu_r", "content": "ok"}]}})
            .to_string(),
            assistant(
                h,
                json!([{"type": "text", "text": "I can't help with that."}]),
                &[],
            ),
        ]
        .join("\n"),
    );
    let refused: Vec<Uuid> = a
        .iter()
        .filter_map(|a| match a {
            Action::Emit(AdapterEvent::Event(e)) => Some(e.event_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        refused,
        [
            record_event_id(&t, 0),
            record_event_id(&c, 0),
            record_event_id(&r, 0)
        ]
    );
    a = play(
        &mut core,
        &[
            assistant(n, json!([{"type": "text", "text": "Here it is."}]), &[t, c, r, h]),
            json!({"type": "system", "subtype": "model_refusal_fallback", "trigger": "refusal",
                "direction": "retry", "original_model": "claude-opus-5-5",
                "fallback_model": "claude-sonnet-5", "request_id": null,
                "retracted_message_uuids": [t, c, r, h],
                "content": "Opus 5.5 declined. This response was generated by Sonnet 5 instead."})
            .to_string(),
            r#"{"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"","total_cost_usd":0.01,"usage":{"input_tokens":3,"output_tokens":4}}"#.into(),
        ]
        .join("\n"),
    );
    let got: Vec<String> = a
        .iter()
        .filter_map(|a| match a {
            Action::Emit(AdapterEvent::Event(e)) => Some(match &e.kind {
                EventKind::Retraction { event_ids } => format!("retraction {event_ids:?}"),
                EventKind::AgentMessage { text, r#final, .. } => format!("{text} {final}"),
                EventKind::System { code, text, .. } => format!("{code}: {text}"),
                k => serde_json::to_value(k).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .into(),
            }),
            _ => None,
        })
        .collect();
    let ids = [
        record_event_id(&t, 0),
        record_event_id(&c, 0),
        record_event_id(&r, 0),
        record_event_id(&h, 0),
    ];
    assert_eq!(
        got,
        [
            format!("retraction {ids:?}"),
            "model_refusal_fallback: claude-opus-5-5 → claude-sonnet-5: Opus 5.5 declined. This response was generated by Sonnet 5 instead.".into(),
            "Here it is. true".into(),
            "usage".into(),
            "status".into(),
        ]
    );
}

/// SPEC §8.1 (0.195): the message that asks the model again after a response whose tool call
/// the CLI could not parse (2.1.325) names that response's records in `retracted_message_uuids`:
/// their events are retracted at once, before any next answer.
#[test]
fn an_answer_the_cli_dropped_is_retracted_when_it_asks_again() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let [t, c, again] = std::array::from_fn::<Uuid, 3, _>(|_| Uuid::new_v4());
    let assistant = |u: Uuid, content: Value| {
        json!({"type": "assistant", "parent_tool_use_id": null, "uuid": u,
            "message": {"model": "claude-opus-5-5", "role": "assistant", "content": content}})
        .to_string()
    };
    play(
        &mut core,
        &[
            assistant(t, json!([{"type": "text", "text": "Writing it."}])),
            assistant(
                c,
                json!([{"type": "tool_use", "id": "toolu_x", "name": "Write", "input": {}}]),
            ),
        ]
        .join("\n"),
    );
    let a = core.on_line(
        &json!({"type": "user", "parent_tool_use_id": null, "uuid": again, "isSynthetic": true,
            "retracted_message_uuids": [t, c],
            "message": {"role": "user", "content": [{"type": "text",
                "text": "Your tool call could not be parsed. Try again."}]}})
        .to_string(),
    );
    let retracted: Vec<Vec<Uuid>> = a
        .iter()
        .filter_map(|a| match a {
            Action::Emit(AdapterEvent::Event(e)) => match &e.kind {
                EventKind::Retraction { event_ids } => Some(event_ids.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        retracted,
        [vec![record_event_id(&t, 0), record_event_id(&c, 0)]]
    );
}

/// SPEC §4, §8.1 (0.195): the CLI refused the model asked for and runs on another
/// (`system/informational` with `model_restriction`, 2.1.325): a `system model_not_switched`
/// line with the model it runs on, kept as the session's, not the notice's own text.
#[test]
fn a_restricted_model_is_a_line_and_the_kept_model() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let line = json!({"type": "system", "subtype": "informational", "level": "warning",
        "content": "claude-opus-4-1 is not available on your plan; using claude-sonnet-5-5",
        "model_restriction": {"requested_model": "claude-opus-4-1",
            "effective_model": "claude-sonnet-5-5", "at": "start"},
        "uuid": Uuid::new_v4(), "session_id": "s"});
    let a = core.on_line(&line.to_string());
    let [Action::Emit(AdapterEvent::Event(e)), Action::Emit(AdapterEvent::ModelKept(kept))] =
        a.as_slice()
    else {
        panic!("{a:?}");
    };
    assert_eq!(kept, "claude-sonnet-5-5");
    assert!(
        matches!(&e.kind, EventKind::System { code, text, model, .. }
            if code == "model_not_switched"
                && text == "Model not switched: claude-opus-4-1 is not available. The agent runs on claude-sonnet-5-5"
                && model.as_deref() == Some("claude-sonnet-5-5")),
        "{e:?}"
    );
}

/// SPEC §8.1 (0.195): a pick of a model the organization does not allow (2.1.325): the CLI sends
/// the `model_restriction` line and then the `restricted_by_org` error of the `set_model`; one
/// line in all.
#[test]
fn a_refused_pick_of_a_restricted_model_is_one_line() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let id = match core.set_model("claude-opus-4-1") {
        Action::Write(v) => v["request_id"].as_str().unwrap().to_owned(),
        a => panic!("{a:?}"),
    };
    let notice = json!({"type": "system", "subtype": "informational", "level": "warning",
        "content": "claude-opus-4-1 is not available; using claude-sonnet-5-5",
        "model_restriction": {"requested_model": "claude-opus-4-1",
            "effective_model": "claude-sonnet-5-5", "at": "switch"},
        "uuid": Uuid::new_v4(), "session_id": "s"});
    let a = core.on_line(&notice.to_string());
    assert!(
        matches!(a.as_slice(), [
            Action::Emit(AdapterEvent::Event(_)),
            Action::Emit(AdapterEvent::ModelKept(m)),
        ] if m == "claude-sonnet-5-5"),
        "{a:?}"
    );
    let error = |id: &str| {
        json!({"type": "control_response", "response": {"subtype": "error",
            "request_id": id, "error": "Model claude-opus-4-1 is restricted",
            "error_code": "restricted_by_org", "model": "claude-sonnet-5-5"}})
        .to_string()
    };
    assert!(core.on_line(&error(&id)).is_empty());
    // The same pick again: the CLI notices a model once until a switch applies, so only the
    // error comes, and it gives the line.
    let again = match core.set_model("claude-opus-4-1") {
        Action::Write(v) => v["request_id"].as_str().unwrap().to_owned(),
        a => panic!("{a:?}"),
    };
    let a = core.on_line(&error(&again));
    assert!(
        matches!(a.as_slice(), [
            Action::Emit(AdapterEvent::Event(e)),
            Action::Emit(AdapterEvent::ModelKept(m)),
        ] if m == "claude-sonnet-5-5"
            && matches!(&e.kind, EventKind::System { code, .. } if code == "model_not_switched")),
        "{a:?}"
    );
}

/// SPEC §8.1 (0.195): a `model_restriction` line of an earlier pick, after the user picked
/// another model, gives neither a line nor its model.
#[test]
fn a_restriction_of_an_earlier_pick_is_only_logged() {
    let mut core = Core::new(Uuid::now_v7(), false);
    core.set_model("claude-opus-4-1");
    core.set_model("claude-sonnet-5");
    let notice = json!({"type": "system", "subtype": "informational", "level": "warning",
        "content": "claude-opus-4-1 is not available; using claude-opus-5-5",
        "model_restriction": {"requested_model": "claude-opus-4-1",
            "effective_model": "claude-opus-5-5", "at": "switch"},
        "uuid": Uuid::new_v4(), "session_id": "s"});
    assert!(core.on_line(&notice.to_string()).is_empty());
}

/// SPEC §4, §8.1: the CLI refused a model switch of ours (2.1.312: `error` with `error_code`
/// and `model`, the model it kept, cli:1509084): a `system model_not_switched` line with the
/// reason and that `model`, and the supervisor keeps it as the session's model.
#[test]
fn a_refused_model_switch_is_a_line_and_the_kept_model() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let line = json!({"type": "control_response", "response": {"subtype": "error",
        "request_id": "hqc-model-0199f3a2-7c10-7000-8000-000000000001",
        "error": "Model claude-nope is not available", "error_code": "not_found",
        "model": "claude-opus-5-5"}});
    let a = core.on_line(&line.to_string());
    let [Action::Emit(AdapterEvent::Event(e)), Action::Emit(AdapterEvent::ModelKept(kept))] =
        a.as_slice()
    else {
        panic!("{a:?}");
    };
    assert_eq!(kept, "claude-opus-5-5");
    assert_eq!(
        e.kind,
        EventKind::System {
            text: "Model not switched: Model claude-nope is not available. The agent stays on claude-opus-5-5".into(),
            code: "model_not_switched".into(),
            call_id: None,
            status: None,
            task_id: None,
            from_session_id: None,
            usage: None,
            fork: None,
            compaction: None,
            model: Some("claude-opus-5-5".into()),
            goal: None,
        }
    );
}

/// SPEC §8.1: the CLI refused a model switch the user has made again since (the hub sent
/// another `session.model` before the answer): its refusal names the model kept before the
/// newer switch, and gives neither a line nor that model; one of the latest switch does.
#[test]
fn a_refusal_of_an_older_model_switch_is_dropped() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let mut request = |model: &str| match core.set_model(model) {
        Action::Write(v) => v["request_id"].as_str().unwrap().to_owned(),
        a => panic!("{a:?}"),
    };
    let (older, latest) = (request("claude-nope"), request("claude-sonnet-5"));
    let refusal = |id: &str| {
        json!({"type": "control_response", "response": {"subtype": "error", "request_id": id,
            "error": "Model claude-nope is not available", "model": "claude-opus-5-5"}})
        .to_string()
    };
    assert!(core.on_line(&refusal(&older)).is_empty());
    let a = core.on_line(&refusal(&latest));
    assert!(
        matches!(a.as_slice(), [
            Action::Emit(AdapterEvent::Event(_)),
            Action::Emit(AdapterEvent::ModelKept(m)),
        ] if m == "claude-opus-5-5"),
        "{a:?}"
    );
}

/// SPEC §8.1: the CLI's fixed "set_model failed" can follow a switch that applied; one naming
/// the model asked for gives no line.
#[test]
fn a_failed_answer_naming_the_model_asked_for_is_no_line() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let id = match core.set_model("claude-sonnet-5") {
        Action::Write(v) => v["request_id"].as_str().unwrap().to_owned(),
        a => panic!("{a:?}"),
    };
    let answer = json!({"type": "control_response", "response": {"subtype": "error",
        "request_id": id, "error": "set_model failed", "model": "claude-sonnet-5"}});
    assert!(core.on_line(&answer.to_string()).is_empty());
}

/// SPEC §4, §8.1: a refusal answered by another model for the session (`model_refusal_fallback`
/// with `scope` `session` or none, cli:108465) makes the turn's `usage` name the model that
/// answered; with `scope` `local` (a subagent's or a side question's answer) it does not.
#[test]
fn a_refusal_answered_by_another_model_names_it_in_the_usage() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let mut turn = |scope: Option<&str>| {
        started(&mut core, sid);
        let mut fallback = json!({"type": "system", "subtype": "model_refusal_fallback",
            "trigger": "refusal", "direction": "retry", "original_model": "claude-opus-5-5",
            "fallback_model": "claude-sonnet-5", "request_id": null,
            "content": "Opus 5.5 declined. This response was generated by Sonnet 5 instead."});
        if let Some(s) = scope {
            fallback["scope"] = json!(s);
        }
        let a = play(
            &mut core,
            &[
                r#"{"type":"system","subtype":"init","model":"claude-opus-5-5","permissionMode":"default","session_id":"s"}"#.into(),
                fallback.to_string(),
                r#"{"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"","total_cost_usd":0.01,"usage":{"input_tokens":3,"output_tokens":4}}"#.into(),
            ]
            .join("\n"),
        );
        a.iter().find_map(|a| match a {
            Action::Emit(AdapterEvent::Event(e)) => match &e.kind {
                EventKind::Usage { model, .. } => model.clone(),
                _ => None,
            },
            _ => None,
        })
    };
    assert_eq!(turn(None).as_deref(), Some("claude-sonnet-5"));
    assert_eq!(turn(Some("session")).as_deref(), Some("claude-sonnet-5"));
    assert_eq!(turn(Some("local")).as_deref(), Some("claude-opus-5-5"));
}

/// SPEC 0.232, §8.1: a side question is a `control_request` named after the hub's event, with
/// the earlier ones as `history` when there are any; the CLI's progress line is no event, and
/// its answer is a `SideAnswer`: the text, a `synthetic` notice and a `null` answer as an
/// error, an error answer with its text. It opens no turn and writes no event of its own.
#[test]
fn a_side_question_is_asked_and_answered_outside_the_conversation() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let event_id = Uuid::now_v7();
    let Action::Write(v) = core.side_question(event_id, "What now?", &[]) else {
        panic!("not a write");
    };
    assert_eq!(v["type"], "control_request");
    assert_eq!(v["request_id"], format!("hqc-side-{event_id}"));
    assert_eq!(
        v["request"],
        json!({"subtype": "side_question", "question": "What now?"})
    );
    let history = [hqc_proto::frame::SideExchange {
        question: "Before?".into(),
        response: "Earlier.".into(),
    }];
    let Action::Write(v) = core.side_question(event_id, "And now?", &history) else {
        panic!("not a write");
    };
    assert_eq!(
        v["request"]["history"],
        json!([{"question": "Before?", "response": "Earlier."}])
    );
    let progress = json!({"type": "system", "subtype": "control_request_progress",
        "request_id": format!("hqc-side-{event_id}"), "status": "started"});
    assert!(core.on_line(&progress.to_string()).is_empty());
    let answer = |core: &mut Core, response: Value| {
        let a = core.on_line(&response.to_string());
        match a.as_slice() {
            [Action::Emit(AdapterEvent::SideAnswer {
                event_id: e,
                text,
                error,
            })] => {
                assert_eq!(*e, event_id);
                (text.clone(), error.clone())
            }
            a => panic!("{a:?}"),
        }
    };
    let success = |response: Value| {
        json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": format!("hqc-side-{event_id}"), "response": response}})
    };
    assert_eq!(
        answer(
            &mut core,
            success(json!({"response": "It reads.", "synthetic": false}))
        ),
        (Some("It reads.".into()), None)
    );
    assert_eq!(
        answer(
            &mut core,
            success(json!({"response": "Try later.", "synthetic": true}))
        ),
        (None, Some("Try later.".into()))
    );
    assert_eq!(
        answer(
            &mut core,
            success(json!({"response": null, "synthetic": false}))
        ),
        (None, Some("No answer".into()))
    );
    assert_eq!(
        answer(
            &mut core,
            json!({"type": "control_response", "response": {"subtype": "error",
                "request_id": format!("hqc-side-{event_id}"), "error": "Side question cancelled"}})
        ),
        (None, Some("Side question cancelled".into()))
    );
    // Nothing of it reached the turn machinery.
    assert!(!core.turn_open());
}

/// SPEC 0.232, §8.1: a `model_refusal_fallback` of `scope` `local` (a side question's or a
/// subagent's answer came from the other model) takes back no record of the conversation, though
/// it names uuids; one for the session still does.
#[test]
fn a_local_refusal_fallback_retracts_nothing() {
    let retractions = |scope: Option<&str>| {
        let sid = Uuid::now_v7();
        let mut core = Core::new(sid, false);
        started(&mut core, sid);
        let t = Uuid::new_v4();
        play(
            &mut core,
            &json!({"type": "assistant", "parent_tool_use_id": null, "uuid": t,
                "message": {"model": "claude-opus-5-5", "role": "assistant",
                    "content": [{"type": "text", "text": "Working."}]}})
            .to_string(),
        );
        let mut fallback = json!({"type": "system", "subtype": "model_refusal_fallback",
            "trigger": "refusal", "direction": "retry", "original_model": "claude-opus-5-5",
            "fallback_model": "claude-sonnet-5", "request_id": null,
            "retracted_message_uuids": [t], "content": "Switched."});
        if let Some(s) = scope {
            fallback["scope"] = json!(s);
        }
        play(&mut core, &fallback.to_string())
            .iter()
            .filter(|a| matches!(a, Action::Emit(AdapterEvent::Event(e)) if matches!(e.kind, EventKind::Retraction { .. })))
            .count()
    };
    assert_eq!(retractions(Some("local")), 0);
    assert_eq!(retractions(Some("session")), 1);
}

/// SPEC §4, §8.1: `system/init`'s MCP servers that are not connected (live on 2.1.312, a server
/// that did not connect in time: `status: failed`) and its `plugin_errors` are a line each; the
/// CLI sends `init` before every turn, and the same set again gives none.
#[test]
fn init_problems_are_lines_when_they_change() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let init = |servers: Value, errors: Option<Value>| {
        let mut v = json!({"type": "system", "subtype": "init", "model": "claude-opus-5-5",
            "permissionMode": "default", "session_id": "s", "mcp_servers": servers});
        if let Some(e) = errors {
            v["plugin_errors"] = e;
        }
        v.to_string()
    };
    // `disabled`: turned off by the user (cli:817600), not a problem.
    let failed = json!([{"name": "hqc", "status": "connected"},
        {"name": "slow", "status": "failed"}, {"name": "docs", "status": "needs-auth"},
        {"name": "off", "status": "disabled"}]);
    let errors = json!([{"plugin": "lint@acme", "type": "hook-load-failed",
        "message": "hooks.json is not valid JSON"}]);
    let lines = |core: &mut Core, line: String| system_events(&core.on_line(&line));
    assert_eq!(
        lines(&mut core, init(failed.clone(), Some(errors.clone()))),
        [
            (
                "mcp_not_connected".into(),
                "MCP servers not connected: slow (failed), docs (needs-auth)".into()
            ),
            (
                "plugin_errors".into(),
                "Plugin errors: lint@acme: hooks.json is not valid JSON".into()
            ),
        ]
    );
    assert_eq!(lines(&mut core, init(failed.clone(), Some(errors))), []);
    assert_eq!(
        lines(
            &mut core,
            init(json!([{"name": "hqc", "status": "connected"}]), None)
        ),
        []
    );
    assert_eq!(
        lines(&mut core, init(failed, None)),
        [(
            "mcp_not_connected".into(),
            "MCP servers not connected: slow (failed), docs (needs-auth)".into()
        )]
    );
}

/// SPEC §8.1: the CLI sends a thinking estimate with every piece of a thinking, about ten a
/// second; a new estimate goes at most once per 2 s, the end of the thinking at once.
#[test]
fn a_thinking_estimate_goes_at_most_once_per_two_seconds() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let activities = |core: &mut Core, line: Value| -> Vec<Option<Activity>> {
        core.on_line(&line.to_string())
            .into_iter()
            .filter_map(|a| match a {
                Action::Emit(AdapterEvent::Activity(a)) => Some(a),
                _ => None,
            })
            .collect()
    };
    let mut got = Vec::new();
    for n in 1..=10u64 {
        got.extend(activities(
            &mut core,
            json!({"type": "system", "subtype": "thinking_tokens", "estimated_tokens": n * 10,
                "estimated_tokens_delta": 10}),
        ));
    }
    assert_eq!(
        got,
        [Some(Activity::Thinking {
            estimated_tokens: 10
        })]
    );
    let stop = json!({"type": "stream_event", "parent_tool_use_id": null,
        "event": {"type": "content_block_stop", "index": 0}});
    assert_eq!(activities(&mut core, stop), [None]);
}

/// SPEC §8.1, §17.3: the CLI's beat of a long call of the main agent (`tool_progress` with
/// `heartbeat: true` every 30 s; live on 2.1.312 its `tool_use_id` is `<call>-heartbeat-N` and
/// its `parent_tool_use_id` the call's own) is a heartbeat, not a subagent's line.
#[test]
fn a_heartbeat_is_reported() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let line = json!({"type": "tool_progress", "tool_use_id": "toolu_01Hb-heartbeat-0",
        "tool_name": "Bash", "parent_tool_use_id": "toolu_01Hb", "elapsed_time_seconds": 30,
        "heartbeat": true, "session_id": "s", "uuid": "0f6c1d2e-7777-4a55-9d0e-1a2b3c4d5e01"});
    assert_eq!(
        core.on_line(&line.to_string()),
        [Action::Emit(AdapterEvent::Heartbeat)]
    );
}

/// SPEC §4, §8.1: a subagent's API retry (`tool_progress` with `subagent_retry`, after the
/// CLI's code, cli:723779: its `agent_id` is the subagent's `task_id`, its `parent_tool_use_id`
/// the subagent's `Agent` call) is on the subagent's row until a line of the same call without
/// it, which says the retry resolved; one of a subagent not in the list changes nothing.
#[test]
fn a_subagent_retry_is_on_its_row() {
    let mut core = Core::new(Uuid::now_v7(), false);
    // Subagent a55949578b136436b of call toolu_01Ti9wrYYVp7kqsruhoHEsLf.
    core.on_line(TWO_SUBAGENTS.lines().next().unwrap());
    let progress = |retry: Option<&str>| {
        let mut v = json!({"type": "tool_progress", "tool_use_id": "agent_msg_01",
            "tool_name": "Agent", "parent_tool_use_id": "toolu_01Ti9wrYYVp7kqsruhoHEsLf",
            "elapsed_time_seconds": 0, "subagent_type": "general-purpose", "session_id": "s",
            "uuid": Uuid::new_v4().to_string()});
        if let Some(agent) = retry {
            v["subagent_retry"] = json!({"agent_id": agent, "attempt": 2, "max_retries": 10,
                "retry_delay_ms": 4000, "error_status": 529, "error_category": "overloaded"});
        }
        v.to_string()
    };
    let mut rows = |line: String| -> Vec<Vec<Option<ApiRetry>>> {
        core.on_line(&line)
            .into_iter()
            .filter_map(|a| match a {
                Action::Emit(AdapterEvent::LiveSubagents(list)) => {
                    Some(list.into_iter().map(|s| s.retry).collect())
                }
                _ => None,
            })
            .collect()
    };
    let retrying = ApiRetry {
        attempt: 2,
        max_retries: 10,
        retry_delay_ms: 4000,
        error_status: Some(529),
    };
    assert_eq!(
        rows(progress(Some("a55949578b136436b"))),
        [vec![Some(retrying)]]
    );
    assert_eq!(rows(progress(None)), [vec![None]]);
    assert!(rows(progress(Some("a0000000000000000"))).is_empty());
    assert!(rows(progress(None)).is_empty());
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
    // No tokens and no cost: no empty `0 → 0 tokens` line.
    assert_eq!(
        shape(&evs),
        [
            "agent.message:false",
            "agent.message:true",
            "system",
            "status",
            "turn_ended"
        ]
    );
    // The reason, not the reply's text again.
    assert_eq!(
        system_events(&a),
        [(
            "turn_ended_early".into(),
            "Turn ended early: api_error".into()
        )]
    );
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
                interrupted: false,
                refusal: false,
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

/// The CLI's own reply to a model refusal (`<synthetic>`, `stop_reason` `refusal` with its
/// `stop_details`, as 2.1.312 builds it; the text shortened) is the turn's final
/// `agent.message` with `runtime` and `refusal`; another synthetic reply is no refusal
/// (`synthetic_reply_is_a_runtime_agent_message`).
#[test]
fn synthetic_refusal_is_a_refusal_agent_message() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let input = started(&mut core, sid);
    let text = "API Error: Opus 5.5's safeguards flagged this session. Claude Code can't respond \
                to your last message with Opus 5.5. Try rephrasing the request in a new session \
                or change your model.";
    let stream = format!(
        r#"{{"type":"assistant","message":{{"id":"0b6f3d2a-4c1e-4f8a-9e7d-2a5c8b1f3e60","model":"<synthetic>","role":"assistant","stop_reason":"refusal","stop_details":{{"type":"refusal","category":"cyber"}},"type":"message","content":[{{"type":"text","text":"{text}"}}]}},"parent_tool_use_id":null,"uuid":"5d1f0c4e-8a2b-4f6e-9c3d-7b1a2e4f6a80","error":"invalid_request","is_api_error_message":true}}
{{"type":"result","subtype":"success","is_error":true,"result":"{text}","total_cost_usd":0,"usage":{{"input_tokens":0,"output_tokens":0}}}}"#
    );
    let evs = emitted(play(&mut core, &stream));
    let messages: Vec<&EventKind> = events(&evs)
        .into_iter()
        .map(|e| &e.kind)
        .filter(|k| matches!(k, EventKind::AgentMessage { .. }))
        .collect();
    assert_eq!(
        messages,
        [&EventKind::AgentMessage {
            text: text.into(),
            r#final: true,
            in_reply_to: Some(input),
            runtime: true,
            interrupted: false,
            refusal: true,
        }]
    );
}

/// SPEC 0.218: a slash command's output (`<synthetic>` with `local_command_source`, as 2.1.312
/// writes the reply to `/goal`, with `local_command_run` or, as for "/config opens an
/// interactive panel…", without) is `system command_output` with the record's id, after the
/// text held before it; a `<synthetic>` message without `local_command_source` (an API error)
/// stays a runtime `agent.message`. Cut at 4096 bytes.
#[test]
fn a_slash_commands_output_is_a_system_line() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let u: Uuid = "1d2f3a4b-5c6d-4e7f-8a9b-0c1d2e3f4a5b".parse().unwrap();
    let line = |text: &str, extra: &str| {
        format!(
            r#"{{"type":"assistant","message":{{"model":"<synthetic>","stop_reason":"end_turn","content":[{{"type":"text","text":"{text}"}}]}},"uuid":"{u}"{extra}}}"#
        )
    };
    let source = |text: &str| {
        format!(r#","local_command_source":"<local-command-stdout>{text}</local-command-stdout>""#)
    };
    let held = r#"{"type":"assistant","message":{"model":"claude-haiku-5-5","content":[{"type":"text","text":"Before"}]},"parent_tool_use_id":null,"uuid":"6a0f4a45-3f09-4c09-b2b7-7d3a0d7e1f10"}"#;
    let run = r#","local_command_run":{"command":"goal","args":"clear"}"#;
    let mut evs = emitted(core.on_line(held));
    evs.extend(emitted(core.on_line(&line(
        "Goal cleared: C",
        &(source("Goal cleared: C") + run),
    ))));
    let shown: Vec<(&str, Option<&str>)> = events(&evs)
        .into_iter()
        .map(|e| match &e.kind {
            EventKind::AgentMessage { text, runtime, .. } => {
                (text.as_str(), runtime.then_some("runtime"))
            }
            EventKind::System { code, text, .. } => (text.as_str(), Some(code.as_str())),
            k => panic!("{k:?}"),
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("Before", None),
            ("Goal cleared: C", Some("command_output"))
        ]
    );
    assert_eq!(events(&evs)[1].event_id, record_event_id(&u, 0));
    assert_eq!(events(&evs)[1].author, Author::System);

    // Without `local_command_run` it is still a command's output.
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let panel = "/config opens an interactive panel and isn't available in this environment";
    let evs = emitted(core.on_line(&line(panel, &source(panel))));
    let shown = events(&evs);
    assert_eq!(shown.len(), 1);
    assert!(matches!(&shown[0].kind, EventKind::System { code, .. } if code == "command_output"));

    // Without `local_command_source` it is the CLI's reply to the turn, held until the turn ends.
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    assert!(emitted(core.on_line(&line("API Error", ""))).is_empty());
    let evs = emitted(core.on_line(r#"{"type":"result","subtype":"success","is_error":true,"result":"API Error","total_cost_usd":0,"usage":{"input_tokens":0,"output_tokens":0}}"#));
    assert!(events(&evs).iter().any(|e| matches!(
        &e.kind,
        EventKind::AgentMessage { text, runtime: true, r#final: true, .. } if text == "API Error"
    )));

    // A long output is cut.
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let long = "x".repeat(5000);
    let evs = emitted(core.on_line(&line(&long, &source("x"))));
    let EventKind::System { text, .. } = &events(&evs)[0].kind else {
        panic!("{evs:?}");
    };
    assert_eq!(text.len(), 4096);
}

/// SPEC 0.218: `system goal` for a goal record of the transcript: the text in Claude Code's
/// words, the goal as a nested object, the id from the record's `uuid`, the condition and
/// the reason cut at 4096 bytes.
#[test]
fn a_goal_record_is_a_goal_line() {
    use hqc_claude_jsonl::{GoalRecord, GoalState};
    let uuid: Uuid = "36c72c19-b4ad-4d95-a482-ce4e281952cb".parse().unwrap();
    let record = |state, reason: Option<&str>| GoalRecord {
        uuid,
        timestamp: None,
        condition: "C".into(),
        state,
        reason: reason.map(str::to_owned),
        checks: Some(2),
        duration_ms: Some(10),
        tokens: Some(3),
    };
    let sid = Uuid::now_v7();
    let line = |g: &GoalRecord| {
        let e = super::engine::goal_event(sid, g);
        assert_eq!(e.event_id, Event::id_from_claude_record(&uuid));
        assert_eq!(e.author, Author::System);
        let EventKind::System {
            text, code, goal, ..
        } = e.kind
        else {
            panic!("not system");
        };
        assert_eq!(code, "goal");
        (text, goal.unwrap())
    };
    let texts: Vec<String> = [
        GoalState::Set,
        GoalState::Unmet,
        GoalState::Met,
        GoalState::Failed,
        GoalState::Cleared,
    ]
    .into_iter()
    .map(|s| line(&record(s, None)).0)
    .collect();
    assert_eq!(
        texts,
        [
            "Goal set: C",
            "Goal not yet met… continuing",
            "Goal achieved",
            "Goal could not be achieved",
            "Goal cleared: C"
        ]
    );
    let (_, goal) = line(&record(GoalState::Met, Some("R")));
    assert_eq!(
        goal,
        hqc_proto::event::GoalStatus {
            condition: "C".into(),
            state: "met".into(),
            reason: Some("R".into()),
            checks: Some(2),
            duration_ms: Some(10),
            tokens: Some(3),
        }
    );
    let mut long = record(GoalState::Unmet, Some(&"r".repeat(5000)));
    long.condition = "é".repeat(3000);
    let (_, goal) = line(&long);
    assert_eq!(goal.condition.len(), 4096);
    assert_eq!(goal.reason.unwrap().len(), 4096);
}

/// A goal record of the transcript written at `ts`.
fn goal_at(ts: Option<&str>, state: hqc_claude_jsonl::GoalState) -> hqc_claude_jsonl::GoalRecord {
    hqc_claude_jsonl::GoalRecord {
        uuid: Uuid::new_v4(),
        timestamp: ts.map(str::to_owned),
        condition: "C".into(),
        state,
        reason: Some("R".into()),
        checks: Some(1),
        duration_ms: None,
        tokens: None,
    }
}

/// An assistant text record of the main chain.
fn text_line(u: Uuid, text: &str) -> String {
    json!({"type": "assistant", "parent_tool_use_id": null, "uuid": u,
        "message": {"model": "claude-opus-5-5", "role": "assistant",
            "content": [{"type": "text", "text": text}]}})
    .to_string()
}

/// What the actions emit, one word each: `text:<t>:<final>` or `goal:<state>`.
fn goal_shape(actions: Vec<Action>) -> Vec<String> {
    emitted(actions)
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::Event(e) => Some(match &e.kind {
                EventKind::AgentMessage { text, r#final, .. } => format!("text:{text}:{final}"),
                EventKind::System { goal: Some(g), .. } => format!("goal:{}", g.state),
                EventKind::ToolCall { tool, .. } => format!("call:{tool}"),
                EventKind::Retraction { .. } => "retraction".into(),
                EventKind::Usage { .. } => "usage".into(),
                _ => return None,
            }),
            _ => None,
        })
        .collect()
}

const LATER: &str = "2099-01-01T00:00:00.000Z";
const EARLIER: &str = "2000-01-01T00:00:00.000Z";

/// SPEC 0.218: a goal record read while the text it judged is still held (the meter reads the
/// transcript on its own clock, stdout waits for the next element) goes out behind that text,
/// not above it; the text is not flushed for it.
#[test]
fn a_goal_record_read_before_the_text_it_judged_waits_behind_it() {
    use hqc_claude_jsonl::GoalState;
    let sid = Uuid::now_v7();
    let tool = json!({"type": "assistant", "parent_tool_use_id": null, "uuid": Uuid::new_v4(),
        "message": {"model": "claude-opus-5-5", "role": "assistant", "content":
            [{"type": "tool_use", "id": "toolu_g", "name": "Bash", "input": {}}]}})
    .to_string();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    assert!(emitted(core.on_line(&text_line(Uuid::new_v4(), "Done."))).is_empty());
    assert!(core
        .on_goal(&goal_at(Some(LATER), GoalState::Unmet))
        .is_empty());
    assert_eq!(
        goal_shape(core.on_line(&tool)),
        ["text:Done.:false", "goal:unmet", "call:Bash"]
    );

    // The turn's last text keeps `final`; the goal line follows the turn's `usage`, which the
    // feed draws with that text.
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    core.on_line(&text_line(Uuid::new_v4(), "Done."));
    assert!(core.on_goal(&goal_at(None, GoalState::Unmet)).is_empty());
    let result = r#"{"type":"result","subtype":"success","is_error":false,"result":"Done.","total_cost_usd":0.01,"usage":{"input_tokens":10,"output_tokens":5}}"#;
    assert_eq!(
        goal_shape(core.on_line(result)),
        ["text:Done.:true", "usage", "goal:unmet"]
    );

    // Nothing held: sent at once. One written before the held text: sent at once, which is
    // above it.
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    assert_eq!(
        goal_shape(core.on_goal(&goal_at(Some(LATER), GoalState::Set))),
        ["goal:set"]
    );
    core.on_line(&text_line(Uuid::new_v4(), "Done."));
    assert_eq!(
        goal_shape(core.on_goal(&goal_at(Some(EARLIER), GoalState::Unmet))),
        ["goal:unmet"]
    );
    assert_eq!(goal_shape(core.on_exit()), ["text:Done.:false"]);
}

/// SPEC 0.218: where the held text is dropped, not sent, the goal events behind it are not
/// lost: a retraction of the text, an input written into a turn the CLI began by itself, the
/// process ending with nothing held.
#[test]
fn a_goal_behind_a_dropped_text_is_still_sent() {
    use hqc_claude_jsonl::GoalState;
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let h = Uuid::new_v4();
    core.on_line(&text_line(h, "I can't help with that."));
    assert!(core
        .on_goal(&goal_at(Some(LATER), GoalState::Unmet))
        .is_empty());
    let fallback = json!({"type": "system", "subtype": "model_refusal_fallback",
        "trigger": "refusal", "direction": "retry", "original_model": "claude-opus-5-5",
        "fallback_model": "claude-sonnet-5", "request_id": null,
        "retracted_message_uuids": [h],
        "content": "Opus 5.5 declined. This response was generated by Sonnet 5 instead."})
    .to_string();
    let got = goal_shape(core.on_line(&fallback));
    assert_eq!(&got[..2], ["goal:unmet", "retraction"], "{got:?}");
    assert!(!got.iter().any(|g| g.starts_with("text:")), "{got:?}");

    // An input written while a text is held drops it; the goal comes out with that call.
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    core.on_line(&text_line(Uuid::new_v4(), "Done."));
    assert!(core
        .on_goal(&goal_at(Some(LATER), GoalState::Met))
        .is_empty());
    let input = SessionInput {
        session_id: sid,
        event_id: Uuid::now_v7(),
        text: "next".into(),
        artifact_ids: vec![],
        peer_prefix: vec![],
        peer: None,
        permission_cap: None,
        schedule_id: None,
        human: false,
    };
    assert_eq!(goal_shape(core.input(&input)), ["goal:met"]);
    assert!(core.on_exit().is_empty(), "nothing left behind");
}

/// SPEC 0.218: a resume tells the transcript's last goal record, whatever its state, with the
/// record's id; a transcript with none tells nothing.
#[tokio::test]
async fn a_resume_tells_the_goal_the_transcript_holds() {
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let claude = ScriptedClaude::new("");
    let factory = ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude.clone()));
    let claude_session = Uuid::new_v4();
    let s = spec(Uuid::now_v7(), work.path(), claude_session);
    let (tx, mut rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx);
    let t = super::paths::transcript_path(config.path(), &s.workdir, &claude_session);
    std::fs::create_dir_all(t.parent().unwrap()).unwrap();
    let at = |u: &str, a: Value| {
        json!({"type": "attachment", "uuid": u, "timestamp": "2026-11-22T19:33:57.895Z", "attachment": a})
            .to_string()
    };
    let set = at(
        "00000000-0000-4000-8000-000000000001",
        json!({"type": "goal_status", "met": false, "sentinel": true, "condition": "C"}),
    );
    let met = at(
        "00000000-0000-4000-8000-000000000002",
        json!({"type": "goal_status", "met": true, "condition": "C", "reason": "R", "iterations": 1}),
    );
    std::fs::write(&t, format!("{set}\n{met}\n")).unwrap();
    a.resume(&s).await.unwrap();
    let Ok(AdapterEvent::Event(ev)) = rx.try_recv() else {
        panic!("no goal line");
    };
    let EventKind::System {
        code, text, goal, ..
    } = &ev.kind
    else {
        panic!("{ev:?}");
    };
    assert_eq!((code.as_str(), text.as_str()), ("goal", "Goal achieved"));
    assert_eq!(goal.as_ref().unwrap().state, "met");
    assert_eq!(
        ev.event_id,
        Event::id_from_claude_record(&"00000000-0000-4000-8000-000000000002".parse().unwrap())
    );
    assert!(rx.try_recv().is_err());
    a.kill().await.unwrap();

    std::fs::write(&t, "").unwrap();
    a.resume(&s).await.unwrap();
    assert!(rx.try_recv().is_err());
    a.kill().await.unwrap();
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
        human: true,
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
/// and its own `result`. That turn is the input's, not the CLI's own: its `init` sends no
/// `status running` (SPEC §8.1).
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
            json!({"type": "system", "subtype": "init", "model": "claude-haiku-5-5",
                "permissionMode": "default", "session_id": CLI_SESSION})
            .to_string(),
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
    let (_, interrupted, _) = core.interrupt().expect("a turn runs");
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
    let (_, carried_turn, _) = core.interrupt().expect("the carried inputs' turn runs");
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
    // The second Stop's turn ended without a `result`: the next turn is not being stopped, and
    // a request the CLI withdraws in it is withdrawn.
    started(&mut core, sid);
    core.on_line(&ask_line("q1"));
    assert_eq!(
        cancels(&core.on_line(&withdraw("q1"))),
        [("clarify", "q1".to_string(), CancelReason::Withdrawn)]
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
            "accepted B",
            "status to -",
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

/// The CLI's answer to our `cancel_async_message` `request_id`.
fn cancel_answer(request_id: &str, cancelled: bool) -> String {
    json!({"type": "control_response", "response": {"subtype": "success",
        "request_id": request_id, "response": {"cancelled": cancelled}}})
    .to_string()
}

/// The `request_id` of the `cancel_async_message` asked for the message with `uuid`, after
/// checking what it asks.
fn cancel_request(core: &mut Core, event_id: Uuid, uuid: &str) -> String {
    let (actions, request_id) = core.withdraw(event_id).expect("the CLI has not taken it");
    let [Action::Write(v)] = &actions[..] else {
        panic!("not one write");
    };
    assert_eq!(v["type"], "control_request");
    assert_eq!(v["request_id"], request_id);
    assert_eq!(
        v["request"],
        json!({"subtype": "cancel_async_message", "message_uuid": uuid})
    );
    request_id
}

/// SPEC 0.219: an input the hub marks `human` (what the owner typed) goes to the CLI with
/// `origin: {kind: "human"}`, which its strict `isHuman()` gates ask for; one it does not mark
/// (a message of another session, a run of a schedule, a room's input, a note of the hub's own)
/// has none. Send now adds `priority: "now"`, which nothing else carries.
#[test]
fn a_persons_input_is_human_and_send_now_has_priority_now() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let a = user_input(sid, "first");
    let first = writes(&core.input(&a));
    assert_eq!(first[0]["origin"], json!({"kind": "human"}));
    assert!(first[0].get("priority").is_none(), "{first:?}");
    let b = user_input(sid, "now");
    let now = writes(
        &core
            .inject_now_with_images(&b, vec![])
            .expect("a turn runs"),
    );
    assert_eq!(now[0]["origin"], json!({"kind": "human"}));
    assert_eq!(now[0]["priority"], "now");
    assert_eq!(now[0]["message"]["content"], "now");
    // The mark is the hub's word alone: `peer` and `schedule_id` set no origin of their own, and
    // an input without them, which the hub did not mark, has none (the runtime's `peer` is
    // consumed into the text before this, so no input of another session shows it).
    let mut unmarked = user_input(sid, "from a session");
    unmarked.human = false;
    let from_peer = writes(
        &core
            .inject_with_images(&unmarked, vec![])
            .expect("a turn runs"),
    );
    assert!(from_peer[0].get("origin").is_none(), "{from_peer:?}");
    // Send now only goes into a turn that runs.
    let mut idle = Core::new(sid, false);
    assert!(idle.inject_now_with_images(&b, vec![]).is_none());
}

/// SPEC 0.219: a message the CLI holds in its queue is taken back with `cancel_async_message`.
/// The CLI's `cancelled` for it is no drop of a turn's end and its answer `cancelled: true` is
/// told once; the message is never accepted, and the turn goes on as if it had not been written.
#[test]
fn a_withdrawn_message_is_never_accepted_or_dropped() {
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
    play(&mut core, &lifecycle(&ub, "queued"));
    let rid = cancel_request(&mut core, b.event_id, &ub);
    assert!(
        core.withdraw(b.event_id).is_none(),
        "the CLI is asked already"
    );
    let mut out = play(&mut core, &lifecycle(&ub, "cancelled"));
    out.extend(play(&mut core, &cancel_answer(&rid, true)));
    assert_eq!(core.withdraw_answer(&rid), Some(true));
    assert_eq!(core.withdraw_answer(&rid), None, "told once");
    assert!(core.withdraw(b.event_id).is_none(), "it is gone");
    out.extend(play(
        &mut core,
        &lines_of(&[tool_result("toolu_1"), reply("DONE"), result("success")]),
    ));
    let names = [(a.event_id, "A"), (b.event_id, "B")];
    assert_eq!(
        trace(&out, &names),
        ["message \"DONE\" to A", "status to A", "turn_ended []"]
    );
}

/// A message the CLI took (`started`, or echoed) is not taken back: no request is written.
/// One it did not cancel (`cancelled: false`) stays, and its echo is its acceptance.
#[test]
fn a_message_the_cli_took_is_not_withdrawn() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let (a, b, d) = (
        user_input(sid, "first"),
        user_input(sid, "second"),
        user_input(sid, "third"),
    );
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
    assert!(core.withdraw(a.event_id).is_none(), "echoed");
    let ub = line_uuid(&core.inject_with_images(&b, vec![]).expect("a turn runs"));
    let ud = line_uuid(&core.inject_with_images(&d, vec![]).expect("a turn runs"));
    play(
        &mut core,
        &lines_of(&[
            lifecycle(&ub, "queued"),
            lifecycle(&ud, "queued"),
            lifecycle(&ub, "started"),
        ]),
    );
    assert!(core.withdraw(b.event_id).is_none(), "started");
    // The CLI took D between the request and its answer: `cancelled: false`, D stays written.
    let rid = cancel_request(&mut core, d.event_id, &ud);
    let mut out = play(&mut core, &cancel_answer(&rid, false));
    assert_eq!(core.withdraw_answer(&rid), Some(false));
    out.extend(play(
        &mut core,
        &lines_of(&[echo(&ub, "second"), echo(&ud, "third")]),
    ));
    let names = [(a.event_id, "A"), (b.event_id, "B"), (d.event_id, "D")];
    assert_eq!(trace(&out, &names), ["accepted B", "accepted D"]);
}

/// A message withdrawn after the `result` carried it: the CLI's own `cancelled` for it is no
/// drop, and its answer ends the turn it kept running, with nothing dropped and no `result` to
/// come (the supervisor has it out of its books and goes on with its queue).
#[test]
fn withdrawing_a_carried_message_ends_the_turn_it_kept_running() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let (a, b, d) = (
        user_input(sid, "first"),
        user_input(sid, "second"),
        user_input(sid, "third"),
    );
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
    let ud = line_uuid(&core.inject_with_images(&d, vec![]).expect("a turn runs"));
    core.interrupt().expect("a turn runs");
    let mut out = play(
        &mut core,
        &lines_of(&[
            lifecycle(&ub, "queued"),
            lifecycle(&ud, "queued"),
            tool_result("toolu_1"),
            result("error_during_execution"),
            lifecycle(&ua, "cancelled"),
        ]),
    );
    assert!(core.can_inject(), "the carried inputs' turn runs");
    let rid = cancel_request(&mut core, b.event_id, &ub);
    out.extend(play(&mut core, &lifecycle(&ub, "cancelled")));
    assert!(core.can_inject(), "D is still carried");
    out.extend(play(&mut core, &cancel_answer(&rid, true)));
    assert_eq!(core.withdraw_answer(&rid), Some(true));
    assert!(core.can_inject(), "D is still carried");
    let rid = cancel_request(&mut core, d.event_id, &ud);
    out.extend(play(&mut core, &lifecycle(&ud, "cancelled")));
    out.extend(play(&mut core, &cancel_answer(&rid, true)));
    assert!(!core.can_inject(), "nothing runs");
    assert!(core.interrupt().is_none(), "nothing to interrupt");
    let names = [(a.event_id, "A"), (b.event_id, "B"), (d.event_id, "D")];
    assert_eq!(
        trace(&out, &names),
        [
            "status to A",
            "turn_ended [B, D]",
            "turn_ended [D]",
            "turn_ended []"
        ]
    );
}

/// A message that begins the turn, withdrawn before the CLI starts it: no turn runs after, and
/// a `result` that ends the CLI's own turn first does not drop it.
#[test]
fn withdrawing_the_message_that_began_the_turn_ends_it() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let a = user_input(sid, "first");
    let ua = line_uuid(&core.input(&a));
    play(&mut core, &lifecycle(&ua, "queued"));
    assert!(core.can_inject());
    let rid = cancel_request(&mut core, a.event_id, &ua);
    let mut out = play(&mut core, &lifecycle(&ua, "cancelled"));
    out.extend(play(&mut core, &cancel_answer(&rid, true)));
    assert_eq!(core.withdraw_answer(&rid), Some(true));
    assert!(!core.can_inject(), "no turn runs");
    assert_eq!(trace(&out, &[(a.event_id, "A")]), ["turn_ended []"]);
}

/// The CLI's `cancelled` for a message being withdrawn, then the `result` of the turn it was
/// written in before the answer: nothing is dropped.
#[test]
fn a_result_between_the_cancel_and_its_answer_drops_nothing() {
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
    play(&mut core, &lifecycle(&ub, "queued"));
    let rid = cancel_request(&mut core, b.event_id, &ub);
    let mut out = play(
        &mut core,
        &lines_of(&[
            lifecycle(&ub, "cancelled"),
            tool_result("toolu_1"),
            reply("DONE"),
            result("success"),
        ]),
    );
    out.extend(play(&mut core, &cancel_answer(&rid, true)));
    assert_eq!(core.withdraw_answer(&rid), Some(true));
    let names = [(a.event_id, "A"), (b.event_id, "B")];
    assert_eq!(
        trace(&out, &names),
        ["message \"DONE\" to A", "status to A", "turn_ended []"]
    );
}

/// A message the CLI took without an echo (`started`, no `isReplay` line: a local command)
/// is taken at the `result`, also one written during the turn, before the events of that
/// `result` (SPEC §5.2).
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
            "accepted B",
            "message \"DONE\" to A",
            "status to A",
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
    let stream = TWO_SUBAGENTS;
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

/// The subagents shown above the input, those of `running_subagents_are_counted`: in the
/// order they started, each with the title of its `task_started` and what its latest
/// `task_progress` said; the list goes when it changes and on each progress, empty at the end.
#[test]
fn live_subagents_are_listed() {
    let fg = live_row("a55949578b136436b", "Reply with number", None);
    let bg = live_row("a52af2b436ded4cea", "Sleep and reply with number", None);
    let bg_running = live_row(
        "a52af2b436ded4cea",
        "Sleep and reply with number",
        Some(("Running Sleep for 6 seconds", 23176, 1)),
    );
    assert_eq!(
        live_lists(TWO_SUBAGENTS.lines()),
        [
            vec![vec![fg.clone()]],
            vec![vec![fg.clone(), bg]],
            vec![],
            vec![vec![fg, bg_running.clone()]],
            vec![],
            vec![],
            vec![vec![bg_running]],
            vec![],
            vec![vec![]]
        ]
    );
}

/// The rows above the input carry the `tool_use_id` of each subagent's `task_started` as
/// `call_id`, also a subagent the CLI's list counted before that start (the last two lines
/// made up after [`MOVED_SUBAGENT`]).
#[test]
fn live_subagents_carry_their_call() {
    let calls = |lines: &[&str]| -> Vec<Vec<Vec<Option<String>>>> {
        let mut core = Core::new(Uuid::now_v7(), false);
        lines
            .iter()
            .map(|l| {
                core.on_line(l)
                    .into_iter()
                    .filter_map(|a| match a {
                        Action::Emit(AdapterEvent::LiveSubagents(list)) => {
                            Some(list.into_iter().map(|s| s.call_id).collect())
                        }
                        _ => None,
                    })
                    .collect()
            })
            .collect()
    };
    let fg = || Some("toolu_01Ti9wrYYVp7kqsruhoHEsLf".to_string());
    let bg = || Some("toolu_01B2anRdzK42h65tA2gdwhb2".to_string());
    let lines: Vec<&str> = TWO_SUBAGENTS.lines().collect();
    assert_eq!(
        calls(&lines[..2]),
        [vec![vec![fg()]], vec![vec![fg(), bg()]]]
    );
    let listed_first = [
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a1","task_type":"local_agent"}]}"#,
        r#"{"type":"system","subtype":"task_started","task_id":"a1","tool_use_id":"toolu_A1","description":"Fix the build","is_backgrounded":true,"task_type":"local_agent"}"#,
    ];
    assert_eq!(
        calls(&listed_first),
        [vec![vec![None]], vec![vec![Some("toolu_A1".to_string())]]]
    );
}

/// (task_id, title, activity, total_tokens, tool_uses) of a subagent shown above the input.
type LiveRow = (String, String, Option<String>, Option<u64>, Option<u64>);

/// The row of subagent `id`; `progress`: (activity, total_tokens, tool_uses).
fn live_row(id: &str, title: &str, progress: Option<(&str, u64, u64)>) -> LiveRow {
    (
        id.into(),
        title.into(),
        progress.map(|p| p.0.into()),
        progress.map(|p| p.1),
        progress.map(|p| p.2),
    )
}

/// The lists of subagents each line sends, in order.
fn live_lists<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<Vec<Vec<LiveRow>>> {
    let mut core = Core::new(Uuid::now_v7(), false);
    lines
        .map(|l| {
            core.on_line(l)
                .into_iter()
                .filter_map(|a| match a {
                    Action::Emit(AdapterEvent::LiveSubagents(list)) => Some(
                        list.into_iter()
                            .map(|s| {
                                (
                                    s.task_id,
                                    s.description,
                                    s.activity,
                                    s.total_tokens,
                                    s.tool_uses,
                                )
                            })
                            .collect(),
                    ),
                    _ => None,
                })
                .collect()
        })
        .collect()
}

/// A foreground and a background subagent (lines of live runs on 2.1.312), a Bash call of the
/// background one and its progress, then their ends.
const TWO_SUBAGENTS: &str = r#"{"type":"system","subtype":"task_started","task_id":"a55949578b136436b","run_id":"0mw3mr55z-1b9c337c","tool_use_id":"toolu_01Ti9wrYYVp7kqsruhoHEsLf","description":"Reply with number","subagent_type":"general-purpose","is_backgrounded":false,"spawn_depth":1,"task_type":"local_agent","prompt":"Reply with the number 4.","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e01"}
{"type":"system","subtype":"task_started","task_id":"a52af2b436ded4cea","run_id":"0mw3mxs60-f1597f03","tool_use_id":"toolu_01B2anRdzK42h65tA2gdwhb2","description":"Sleep and reply with number","subagent_type":"general-purpose","is_backgrounded":true,"spawn_depth":1,"task_type":"local_agent","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e02"}
{"type":"system","subtype":"task_started","task_id":"b6yusn86f","run_id":"0mw3mxvyu-6c7dc659","owned_by_subagent":true,"tool_use_id":"toolu_01T7e2mWErkbKbbTuuN4vYrm","description":"Sleep for 6 seconds","is_backgrounded":false,"parent_task_id":"a52af2b436ded4cea","task_type":"local_bash","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e03"}
{"type":"system","subtype":"task_progress","task_id":"a52af2b436ded4cea","run_id":"0mw3mxs60-f1597f03","tool_use_id":"toolu_01B2anRdzK42h65tA2gdwhb2","description":"Running Sleep for 6 seconds","subagent_type":"general-purpose","usage":{"total_tokens":23176,"tool_uses":1,"duration_ms":1310},"last_tool_name":"Bash","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e04"}
{"type":"system","subtype":"task_notification","task_id":"b6yusn86f","run_id":"0mw3mxvyu-6c7dc659","tool_use_id":"toolu_01T7e2mWErkbKbbTuuN4vYrm","status":"completed","output_file":"","summary":"Sleep for 6 seconds","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e05"}
{"type":"system","subtype":"task_updated","task_id":"a52af2b436ded4cea","patch":{"is_backgrounded":true},"uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e06"}
{"type":"system","subtype":"task_updated","task_id":"a55949578b136436b","run_id":"0mw3mr55z-1b9c337c","patch":{"status":"completed","end_time":1793888262670},"uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e07"}
{"type":"system","subtype":"task_notification","task_id":"a55949578b136436b","run_id":"0mw3mr55z-1b9c337c","tool_use_id":"toolu_01Ti9wrYYVp7kqsruhoHEsLf","status":"completed","output_file":"/tmp/tasks/a55949578b136436b.output","summary":"4","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e08"}
{"type":"system","subtype":"task_notification","task_id":"a52af2b436ded4cea","run_id":"0mw3mxs60-f1597f03","tool_use_id":"toolu_01B2anRdzK42h65tA2gdwhb2","status":"completed","output_file":"/tmp/tasks/a52af2b436ded4cea.output","summary":"4","uuid":"0f6c1d2e-5555-4a55-9d0e-1a2b3c4d5e09"}"#;

/// The agent's own background shell command (`local_bash` without `parent_task_id`, with the
/// `description` of its Bash call) is a row with that description and no counts from its
/// `task_started`, which comes after the CLI's list holds it, until the list drops it.
#[test]
fn a_background_command_of_the_agent_is_listed() {
    let lines = [
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"b2st7oc3z","task_type":"local_bash","description":"Run the full pass over 1000 boards on cirunner"}]}"#,
        r#"{"type":"system","subtype":"task_started","task_id":"b2st7oc3z","tool_use_id":"toolu_01Run","description":"Run the full pass over 1000 boards on cirunner","is_backgrounded":true,"task_type":"local_bash"}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[]}"#,
    ];
    let row = live_row(
        "b2st7oc3z",
        "Run the full pass over 1000 boards on cirunner",
        None,
    );
    assert_eq!(
        live_lists(lines.into_iter()),
        [vec![], vec![vec![row]], vec![vec![]]]
    );
}

/// A command without a description is listed under the command itself, which may run to any
/// length: its row has the first 4096 bytes, and the same list again sends nothing.
#[test]
fn a_long_command_is_cut_in_its_row() {
    let command = "x".repeat(5000);
    let list = json!({"type": "system", "subtype": "background_tasks_changed",
        "tasks": [{"task_id": "blong", "task_type": "local_bash", "description": command}]})
    .to_string();
    let started = json!({"type": "system", "subtype": "task_started", "task_id": "blong",
        "tool_use_id": "toolu_01Long", "description": command, "is_backgrounded": true,
        "task_type": "local_bash"})
    .to_string();
    let lists = live_lists([list.as_str(), started.as_str(), list.as_str()].into_iter());
    assert_eq!(lists[1][0][0].1.len(), 4096);
    assert!(lists[2].is_empty(), "{:?}", lists[2].len());
}

/// An `ambient` entry of the CLI's list is no activity (the SDK): no row, though its
/// `task_started` came, while the agent's command beside it has one.
#[test]
fn an_ambient_task_is_not_listed() {
    let lines = [
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"bamb","task_type":"local_bash","description":"Watch the build log","ambient":true},{"task_id":"b2st7oc3z","task_type":"local_bash","description":"Run the full pass over 1000 boards on cirunner"}]}"#,
        r#"{"type":"system","subtype":"task_started","task_id":"bamb","tool_use_id":"toolu_01Log","description":"Watch the build log","is_backgrounded":true,"task_type":"local_bash"}"#,
        r#"{"type":"system","subtype":"task_started","task_id":"b2st7oc3z","tool_use_id":"toolu_01Run","description":"Run the full pass over 1000 boards on cirunner","is_backgrounded":true,"task_type":"local_bash"}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[]}"#,
    ];
    let row = live_row(
        "b2st7oc3z",
        "Run the full pass over 1000 boards on cirunner",
        None,
    );
    assert_eq!(
        live_lists(lines.into_iter()),
        [vec![], vec![], vec![vec![row]], vec![vec![]]]
    );
}

/// The rows of the lists `actions` send, in order; no line of the tests below that bring a
/// wakeup, a job or a Monitor changes the subagent count.
fn live_rows(actions: Vec<Action>) -> Vec<Vec<LiveSubagent>> {
    let mut lists = Vec::new();
    for e in emitted(actions) {
        match e {
            AdapterEvent::LiveSubagents(list) => lists.push(list),
            AdapterEvent::Subagents(n) => panic!("the subagent count changed to {n}"),
            _ => {}
        }
    }
    lists
}

/// A background Bash call and a Monitor of the agent: 2.1.312 lists both as `local_bash`, the
/// Monitor without `ambient`, each before its `task_started`. A row for each from its
/// `task_started`, the Bash call's `command` and the Monitor's `monitor`, told apart by the call
/// that started it; each goes when the CLI's list drops it. Both are `stoppable`: a Monitor's
/// task is a `local_bash` one, which `stop_task` stops as a command's.
#[test]
fn a_monitor_of_the_agent_is_listed() {
    type MonitorRow = (LiveKind, String, Option<u64>, bool);
    let monitor = json!({"type": "assistant", "message": {"model": "claude-haiku-5-5",
        "role": "assistant", "content": [{"type": "tool_use", "id": "toolu_01Watch",
            "name": "Monitor", "input": {"command": "tail -f run.log",
                "description": "Watch the run's log"}}]},
        "parent_tool_use_id": null, "session_id": CLI_SESSION, "uuid": Uuid::new_v4()})
    .to_string();
    let lines: [String; 8] = [
        bash("toolu_01Run", "make all-boards"),
        monitor,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"b2st7oc3z","task_type":"local_bash","description":"Run the full pass over 1000 boards on cirunner"}]}"#.into(),
        r#"{"type":"system","subtype":"task_started","task_id":"b2st7oc3z","tool_use_id":"toolu_01Run","description":"Run the full pass over 1000 boards on cirunner","is_backgrounded":true,"task_type":"local_bash"}"#.into(),
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"b2st7oc3z","task_type":"local_bash","description":"Run the full pass over 1000 boards on cirunner"},{"task_id":"bwatch1","task_type":"local_bash","description":"Watch the run's log"}]}"#.into(),
        r#"{"type":"system","subtype":"task_started","task_id":"bwatch1","tool_use_id":"toolu_01Watch","description":"Watch the run's log","is_backgrounded":true,"task_type":"local_bash"}"#.into(),
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"bwatch1","task_type":"local_bash","description":"Watch the run's log"}]}"#.into(),
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[]}"#.into(),
    ];
    let mut core = Core::new(Uuid::now_v7(), false);
    let lists: Vec<Vec<Vec<MonitorRow>>> = lines
        .iter()
        .map(|l| {
            live_rows(core.on_line(l))
                .into_iter()
                .map(|rows| {
                    rows.into_iter()
                        .map(|r| (r.kind.unwrap(), r.task_id, r.due_in_ms, r.stoppable))
                        .collect()
                })
                .collect()
        })
        .collect();
    let run = (LiveKind::Command, "b2st7oc3z".to_string(), None, true);
    let watch = (LiveKind::Monitor, "bwatch1".to_string(), None, true);
    assert_eq!(
        lists,
        [
            vec![],
            vec![],
            vec![],
            vec![vec![run.clone()]],
            vec![],
            vec![vec![run, watch.clone()]],
            vec![vec![watch]],
            vec![vec![]]
        ]
    );
}

/// An `assistant` line of the main agent (`parent`: of a subagent) with a call of `tool`.
fn tool_call(id: &str, tool: &str, input: Value, parent: Option<&str>) -> String {
    json!({"type": "assistant", "message": {"model": "claude-haiku-5-5", "role": "assistant",
        "content": [{"type": "tool_use", "id": id, "name": tool, "input": input}]},
        "parent_tool_use_id": parent, "session_id": CLI_SESSION, "uuid": Uuid::new_v4()})
    .to_string()
}

/// The `user` line with the result of call `id`, `data` its `tool_use_result`.
fn tool_done(id: &str, error: bool, data: Value, parent: Option<&str>) -> String {
    json!({"type": "user", "message": {"role": "user", "content": [{"tool_use_id": id,
        "type": "tool_result", "is_error": error, "content": "ok"}]},
        "parent_tool_use_id": parent, "session_id": CLI_SESSION, "uuid": Uuid::new_v4(),
        "tool_use_result": data})
    .to_string()
}

/// A `command_lifecycle` `started` of an input the CLI made itself (2.1.312: a wakeup names the
/// one it fires by `scheduled_for`, a cron fire names none).
fn fire(source: &str, scheduled_for: Option<i64>) -> String {
    json!({"type": "command_lifecycle", "command_uuid": Uuid::new_v4(), "state": "started",
        "wakeup_source": source, "scheduled_for": scheduled_for,
        "uuid": Uuid::new_v4(), "session_id": CLI_SESSION})
    .to_string()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// A wakeup the main agent armed with `ScheduleWakeup` (lines of the live run `x6-wake-control`,
/// 2.1.312) is a row from its result: its call as `task_id` and `call_id`, its `reason`, the
/// time to its `scheduledFor`. A newer one replaces it (the CLI keeps one); the fire of an older
/// one, a failed call and a subagent's call change nothing; its own fire and a stop
/// (`scheduledFor: 0`) take it off. None of it changes the subagent count. It is no task of the
/// CLI: not `stoppable`.
#[test]
fn a_wakeup_is_a_row_until_it_fires() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let arm =
        |core: &mut Core, id: &str, reason: &str, at: i64, error: bool, parent: Option<&str>| {
            core.on_line(&tool_call(
                id,
                "ScheduleWakeup",
                json!({"delaySeconds": 70, "reason": reason, "prompt": "WAKEUP-FIRED"}),
                parent,
            ));
            live_rows(core.on_line(&tool_done(
                id,
                error,
                json!({"scheduledFor": at, "clampedDelaySeconds": 70, "wasClamped": false}),
                parent,
            )))
        };
    let due = now_ms() + 60_000;
    let lists = arm(&mut core, "toolu_wake1", "probe", due, false, None);
    assert_eq!(lists.len(), 1, "{lists:?}");
    let [row] = &lists[0][..] else {
        panic!("{lists:?}")
    };
    assert_eq!(row.kind, Some(LiveKind::Wakeup));
    assert_eq!(row.task_id, "toolu_wake1");
    assert_eq!(row.call_id.as_deref(), Some("toolu_wake1"));
    assert_eq!(row.description, "probe");
    assert!(!row.stoppable, "{row:?}");
    assert!(row.duration_ms < 5_000, "{row:?}");
    assert!(
        (55_000..=60_000).contains(&row.due_in_ms.unwrap()),
        "{row:?}"
    );
    // A failed call and a subagent's change nothing.
    assert!(arm(&mut core, "toolu_failed", "no", due + 5, true, None).is_empty());
    assert!(arm(
        &mut core,
        "toolu_sub",
        "no",
        due + 6,
        false,
        Some("toolu_agent")
    )
    .is_empty());
    // A later one replaces it.
    let later = due + 120_000;
    let lists = arm(&mut core, "toolu_wake2", "again", later, false, None);
    let [row] = &lists[0][..] else {
        panic!("{lists:?}")
    };
    assert_eq!(
        (row.task_id.as_str(), row.description.as_str()),
        ("toolu_wake2", "again")
    );
    assert!(
        (175_000..=180_000).contains(&row.due_in_ms.unwrap()),
        "{row:?}"
    );
    // The fire of the first, older one: this one is still due. Another source fires none.
    assert!(live_rows(core.on_line(&fire("loop_wakeup", Some(due)))).is_empty());
    assert!(live_rows(core.on_line(&fire("schedule_wakeup", None))).is_empty());
    // Its own fire.
    assert_eq!(
        live_rows(core.on_line(&fire("loop_wakeup", Some(later)))),
        [vec![]]
    );
    assert!(live_rows(core.on_line(&fire("loop_wakeup", Some(later)))).is_empty());
    // Stopped.
    arm(&mut core, "toolu_wake3", "third", later, false, None);
    assert_eq!(arm(&mut core, "toolu_stop", "x", 0, false, None), [vec![]]);
    assert!(arm(&mut core, "toolu_stop2", "x", 0, false, None).is_empty());
}

/// A cron job of the agent (`CronCreate`, result `{id, humanSchedule, recurring, nextFireAt}`)
/// is a row from its result: the job's id as `task_id`, its `prompt`, its schedule in words as
/// `activity`, its call, the time to `nextFireAt` (none without one). A failed call and a
/// subagent's change nothing; a successful `CronDelete` takes the job off, a failed one and one
/// of an id no job has do not. It is no task of the CLI: not `stoppable`.
#[test]
fn a_cron_job_is_a_row_until_it_is_deleted() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let create = |core: &mut Core, id: &str, error: bool, data: Value, parent: Option<&str>| {
        core.on_line(&tool_call(
            id,
            "CronCreate",
            json!({"cron": "*/5 * * * *", "prompt": "Pull the CI status", "recurring": true}),
            parent,
        ));
        live_rows(core.on_line(&tool_done(id, error, data, parent)))
    };
    let delete = |core: &mut Core, id: &str, job: &str, error: bool| {
        core.on_line(&tool_call(id, "CronDelete", json!({"id": job}), None));
        live_rows(core.on_line(&tool_done(id, error, json!({"id": job}), None)))
    };
    let data = |id: &str, next: Option<i64>| {
        let mut d = json!({"id": id, "humanSchedule": "Every 5 minutes", "recurring": true,
            "durable": false});
        // The CLI's `nextFireAt` has a fraction of a millisecond, as live on 2.1.312.
        if let Some(n) = next {
            d["nextFireAt"] = json!(n as f64 + 0.2852);
        }
        d
    };
    let lists = create(
        &mut core,
        "toolu_c1",
        false,
        data("1a2b3c4d", Some(now_ms() + 120_000)),
        None,
    );
    let [row] = &lists[0][..] else {
        panic!("{lists:?}")
    };
    assert_eq!(row.kind, Some(LiveKind::Cron));
    assert_eq!(row.task_id, "1a2b3c4d");
    assert_eq!(row.description, "Pull the CI status");
    assert_eq!(row.activity.as_deref(), Some("Every 5 minutes"));
    assert_eq!(row.call_id.as_deref(), Some("toolu_c1"));
    assert!(!row.stoppable, "{row:?}");
    assert!(
        (115_000..=120_000).contains(&row.due_in_ms.unwrap()),
        "{row:?}"
    );
    // Without `nextFireAt` there is no time to show; a failed call and a subagent's add nothing.
    let lists = create(&mut core, "toolu_c2", false, data("5e6f7a8b", None), None);
    assert_eq!(lists[0].len(), 2);
    assert_eq!(lists[0][1].due_in_ms, None);
    assert!(create(&mut core, "toolu_c3", true, data("badbadba", None), None).is_empty());
    assert!(create(
        &mut core,
        "toolu_c4",
        false,
        data("cafecafe", None),
        Some("toolu_agent")
    )
    .is_empty());
    assert!(
        delete(&mut core, "toolu_d1", "1a2b3c4d", true).is_empty(),
        "failed"
    );
    assert!(
        delete(&mut core, "toolu_d2", "ffffffff", false).is_empty(),
        "unknown"
    );
    let lists = delete(&mut core, "toolu_d3", "1a2b3c4d", false);
    assert_eq!(lists.len(), 1);
    let ids: Vec<_> = lists[0].iter().map(|r| r.task_id.as_str()).collect();
    assert_eq!(ids, ["5e6f7a8b"]);
}

/// The whole lists of the `tasks` events in `actions`.
fn task_lists(actions: Vec<Action>) -> Vec<Vec<AgentTask>> {
    emitted(actions)
        .into_iter()
        .filter_map(|e| match e {
            AdapterEvent::Event(Event {
                kind: EventKind::Tasks { items },
                ..
            }) => Some(items),
            _ => None,
        })
        .collect()
}

fn task(subject: &str, status: TaskStatus, active_form: Option<&str>) -> AgentTask {
    AgentTask {
        subject: subject.into(),
        status,
        active_form: active_form.map(str::to_owned),
    }
}

/// The main agent's `TodoWrite` replaces the task list, as the CLI keeps it (every task done:
/// none). Nothing is sent when the list does not change, for a failed call, a subagent's call
/// and a task with a status this build does not know (left out).
#[test]
fn a_todo_write_replaces_the_task_list() {
    use TaskStatus::*;
    let mut core = Core::new(Uuid::now_v7(), false);
    let write = |core: &mut Core, id: &str, todos: Value, error: bool, parent: Option<&str>| {
        core.on_line(&tool_call(
            id,
            "TodoWrite",
            json!({ "todos": todos }),
            parent,
        ));
        task_lists(core.on_line(&tool_done(id, error, json!({}), parent)))
    };
    let todos = json!([
        {"content": "Read the test", "status": "completed", "activeForm": "Reading the test"},
        {"content": "Fix the parser", "status": "in_progress", "activeForm": "Fixing the parser"},
        {"content": "Run the suite", "status": "pending", "activeForm": "Running the suite"},
        {"content": "Odd", "status": "blocked", "activeForm": "Oddly"},
    ]);
    let want = vec![
        task("Read the test", Completed, Some("Reading the test")),
        task("Fix the parser", InProgress, Some("Fixing the parser")),
        task("Run the suite", Pending, Some("Running the suite")),
    ];
    assert_eq!(
        write(&mut core, "toolu_t1", todos.clone(), false, None),
        [want]
    );
    assert!(write(&mut core, "toolu_t2", todos.clone(), false, None).is_empty());
    assert!(
        write(&mut core, "toolu_t3", json!([]), true, None).is_empty(),
        "failed"
    );
    assert!(
        write(&mut core, "toolu_t4", json!([]), false, Some("toolu_agent")).is_empty(),
        "a subagent's"
    );
    let done = json!([{"content": "Read the test", "status": "completed", "activeForm": "x"}]);
    assert_eq!(
        write(&mut core, "toolu_t5", done.clone(), false, None),
        [vec![]],
        "every task done"
    );
    assert!(write(&mut core, "toolu_t6", done, false, None).is_empty());
}

/// A call of the task tool `tool` of the main agent and its result block with `text`, `data`
/// the record's `tool_use_result`: the lists of the `tasks` events it gives.
fn task_call(
    core: &mut Core,
    tool: &str,
    input: Value,
    error: bool,
    text: &str,
    data: Value,
) -> Vec<Vec<AgentTask>> {
    let id = format!("toolu_{}", Uuid::new_v4());
    core.on_line(&tool_call(&id, tool, input, None));
    task_lists(core.on_line(&tool_done_text(&id, error, text, data)))
}

/// The `user` line with the result of call `id`: the block's own `text`, `data` its
/// `tool_use_result`.
fn tool_done_text(id: &str, error: bool, text: &str, data: Value) -> String {
    json!({"type": "user", "message": {"role": "user", "content": [{"tool_use_id": id,
        "type": "tool_result", "is_error": error, "content": text}]},
        "parent_tool_use_id": null, "session_id": CLI_SESSION, "uuid": Uuid::new_v4(),
        "tool_use_result": data})
    .to_string()
}

fn create_input(subject: &str) -> Value {
    json!({"subject": subject, "description": "d", "activeForm": format!("Doing {subject}")})
}

fn task_create(core: &mut Core, id: &str, subject: &str) -> Vec<Vec<AgentTask>> {
    let text = format!("Task #{id} created successfully: {subject}");
    let data = json!({"task": {"id": id}});
    task_call(
        core,
        "TaskCreate",
        create_input(subject),
        false,
        &text,
        data,
    )
}

/// A `TaskUpdate` whose result block says `Updated task #…` (`updated`) or the error the CLI
/// writes for a task it did not find; the record's `tool_use_result` says `success: true` either
/// way, as one that belongs to another block of the record would.
fn task_update(core: &mut Core, input: Value, updated: bool) -> Vec<Vec<AgentTask>> {
    let id = input["taskId"].as_str().unwrap().to_owned();
    let text = if updated {
        format!("Updated task #{id} status")
    } else {
        format!("Task #{id} not found")
    };
    task_call(
        core,
        "TaskUpdate",
        input,
        false,
        &text,
        json!({"success": true}),
    )
}

/// `TaskCreate` adds a task under the id its result block names, `TaskUpdate` changes it or, with
/// `deleted`, removes it, `TaskList` replaces the list and a known task keeps its `activeForm`.
/// A `TaskUpdate` whose block says it found no task, an update of an unknown id and a failed
/// call change nothing.
#[test]
fn the_task_tools_keep_the_task_list() {
    use TaskStatus::*;
    let mut core = Core::new(Uuid::now_v7(), false);
    assert_eq!(
        task_create(&mut core, "1", "Fix"),
        [vec![task("Fix", Pending, Some("Doing Fix"))]]
    );
    assert_eq!(task_create(&mut core, "2", "Test").len(), 1);
    let failed = task_call(
        &mut core,
        "TaskCreate",
        json!({"subject": "Lost"}),
        true,
        "Task #9 created successfully: Lost",
        json!({"task": {"id": "9"}}),
    );
    assert!(failed.is_empty(), "failed");
    let started = json!({"taskId": "1", "status": "in_progress"});
    assert_eq!(
        task_update(&mut core, started.clone(), true),
        [vec![
            task("Fix", InProgress, Some("Doing Fix")),
            task("Test", Pending, Some("Doing Test")),
        ]]
    );
    let refused = json!({"taskId": "2", "status": "completed"});
    assert!(
        task_update(&mut core, refused, false).is_empty(),
        "the block says it found no task"
    );
    let unknown = json!({"taskId": "7", "status": "completed"});
    assert!(
        task_update(&mut core, unknown, true).is_empty(),
        "unknown id"
    );
    assert!(
        task_update(&mut core, started, true).is_empty(),
        "unchanged"
    );
    let done = json!({"taskId": "1", "status": "completed", "subject": "Fix it",
        "activeForm": "Fixing it"});
    let lists = task_update(&mut core, done, true);
    assert_eq!(lists[0][0], task("Fix it", Completed, Some("Fixing it")));
    // The list the agent asks for replaces ours; a task it knew keeps its active form.
    let listed = json!({"tasks": [
        {"id": "1", "subject": "Fix it", "status": "completed"},
        {"id": "3", "subject": "Ship", "status": "pending"},
        {"id": "4", "subject": "Odd", "status": "blocked"},
    ]});
    let list =
        |core: &mut Core| task_call(core, "TaskList", json!({}), false, "tasks", listed.clone());
    assert_eq!(
        list(&mut core),
        [vec![
            task("Fix it", Completed, Some("Fixing it")),
            task("Ship", Pending, None),
        ]]
    );
    assert!(list(&mut core).is_empty());
    let deleted = json!({"taskId": "3", "status": "deleted"});
    assert_eq!(
        task_update(&mut core, deleted, true),
        [vec![task("Fix it", Completed, Some("Fixing it"))]]
    );
}

/// Parallel `TaskCreate` calls come back in one `user` record, the `tool_use_result` of which
/// is of one of them (or none): each task takes its id from its own block's text. Without the
/// text the result's `task.id` is the id.
#[test]
fn parallel_task_creates_in_one_record_lose_nothing() {
    use TaskStatus::*;
    let mut core = Core::new(Uuid::now_v7(), false);
    let calls: Vec<Value> = ["A", "B", "C"]
        .iter()
        .enumerate()
        .map(|(i, s)| {
            json!({"type": "tool_use", "id": format!("toolu_p{i}"), "name": "TaskCreate",
                "input": create_input(s)})
        })
        .collect();
    core.on_line(
        &json!({"type": "assistant", "message": {"model": "claude-haiku-5-5", "role": "assistant",
            "content": calls}, "parent_tool_use_id": null, "session_id": CLI_SESSION,
            "uuid": Uuid::new_v4()})
        .to_string(),
    );
    let results: Vec<Value> = ["A", "B", "C"]
        .iter()
        .enumerate()
        .map(|(i, s)| {
            json!({"tool_use_id": format!("toolu_p{i}"), "type": "tool_result",
                "content": format!("Task #{} created successfully: {s}", i + 1)})
        })
        .collect();
    let record = json!({"type": "user", "message": {"role": "user", "content": results},
        "parent_tool_use_id": null, "session_id": CLI_SESSION, "uuid": Uuid::new_v4(),
        "tool_use_result": {"task": {"id": "1", "subject": "A"}}});
    let lists = task_lists(core.on_line(&record.to_string()));
    let Some(last) = lists.last() else {
        panic!("no list")
    };
    assert_eq!(
        last,
        &vec![
            task("A", Pending, Some("Doing A")),
            task("B", Pending, Some("Doing B")),
            task("C", Pending, Some("Doing C")),
        ]
    );
    // The ids are the texts': an update of the third finds it.
    let lists = task_update(
        &mut core,
        json!({"taskId": "3", "status": "completed"}),
        true,
    );
    assert_eq!(lists[0][2].status, Completed);
    // Text of no known form: the result's id.
    let lists = task_call(
        &mut core,
        "TaskCreate",
        create_input("D"),
        false,
        "ok",
        json!({"task": {"id": "4"}}),
    );
    assert_eq!(lists[0][3], task("D", Pending, Some("Doing D")));
}

/// A call the CLI corrects before it runs (SPEC §8.1): `TaskCreate` with `content` and the `task`
/// wrapper, `active_form`, no `subject`: the task is the one its result text names, with the
/// call's `active_form`; `TaskUpdate` with `id` for `taskId`: the task of the id the text names.
#[test]
fn a_call_the_cli_corrected_is_read_from_its_result_text() {
    use TaskStatus::*;
    let mut core = Core::new(Uuid::now_v7(), false);
    let lists = task_call(
        &mut core,
        "TaskCreate",
        json!({"task": {"content": "Wrapped one", "active_form": "Wrapping"}}),
        false,
        "Task #7 created successfully: Wrapped one",
        json!({}),
    );
    assert_eq!(
        lists,
        [vec![task("Wrapped one", Pending, Some("Wrapping"))]]
    );
    let lists = task_call(
        &mut core,
        "TaskUpdate",
        json!({"id": "7", "status": "in_progress"}),
        false,
        "Updated task #7 status",
        json!({"success": true}),
    );
    assert_eq!(
        lists,
        [vec![task("Wrapped one", InProgress, Some("Wrapping"))]]
    );
    // A `task` wrapper next to a top-level `subject` is not unwrapped, as in the CLI.
    let lists = task_call(
        &mut core,
        "TaskCreate",
        json!({"subject": "Top", "task": {"active_form": "Wrapped"}}),
        false,
        "ok",
        json!({"task": {"id": "9"}}),
    );
    assert_eq!(lists[0][1], task("Top", Pending, None));
    // And a `TaskUpdate` takes no `title` (the CLI refuses such a call; nothing reaches us).
    let lists = task_call(
        &mut core,
        "TaskUpdate",
        json!({"taskId": "9", "title": "Renamed"}),
        false,
        "Updated task #9",
        json!({"success": true}),
    );
    assert!(lists.is_empty());
    // `title` for `subject` when the text names none.
    let lists = task_call(
        &mut core,
        "TaskCreate",
        json!({"title": "Titled", "activeForm": "Titling"}),
        false,
        "Task #8 created",
        json!({"task": {"id": "8"}}),
    );
    assert_eq!(lists[0][2], task("Titled", Pending, Some("Titling")));
}

/// A record of two results, `TaskList`'s and `TaskGet`'s, carries the `tool_use_result` of one of
/// them: `TaskList` changes nothing from it, and nor does a `TaskList` result with no `tasks`
/// array (never an empty list); nor does a `TaskCreate` that names no id in its text take the
/// `task.id` of such a record.
#[test]
fn a_task_list_result_of_a_record_of_several_changes_nothing() {
    use TaskStatus::*;
    let mut core = Core::new(Uuid::now_v7(), false);
    task_create(&mut core, "1", "Keep");
    let call =
        |id: &str, tool: &str| json!({"type": "tool_use", "id": id, "name": tool, "input": {}});
    core.on_line(
        &json!({"type": "assistant", "message": {"model": "claude-haiku-5-5", "role": "assistant",
            "content": [call("toolu_l", "TaskList"), call("toolu_g", "TaskGet")]},
            "parent_tool_use_id": null, "session_id": CLI_SESSION, "uuid": Uuid::new_v4()})
        .to_string(),
    );
    let results = json!([
        {"tool_use_id": "toolu_l", "type": "tool_result", "content": "#1 [pending] Keep"},
        {"tool_use_id": "toolu_g", "type": "tool_result", "content": "Task #1: Keep"}
    ]);
    // The record's `tool_use_result` is the `TaskGet`'s, or another call's with a `tasks` array.
    for data in [json!({"task": {"id": "1"}}), json!({"tasks": []})] {
        let record = json!({"type": "user", "message": {"role": "user", "content": results},
            "parent_tool_use_id": null, "session_id": CLI_SESSION, "uuid": Uuid::new_v4(),
            "tool_use_result": data});
        assert!(task_lists(core.on_line(&record.to_string())).is_empty());
        core.on_line(
            &json!({"type": "assistant", "message": {"model": "claude-haiku-5-5",
                "role": "assistant", "content": [call("toolu_l", "TaskList"),
                call("toolu_g", "TaskGet")]}, "parent_tool_use_id": null,
                "session_id": CLI_SESSION, "uuid": Uuid::new_v4()})
            .to_string(),
        );
    }
    // Two `TaskCreate` results whose texts name no id: the record's `task.id` is of one of them,
    // so neither is added.
    let create = |id: &str| json!({"type": "tool_use", "id": id, "name": "TaskCreate", "input": create_input("New")});
    core.on_line(
        &json!({"type": "assistant", "message": {"model": "claude-haiku-5-5", "role": "assistant",
            "content": [create("toolu_c1"), create("toolu_c2")]},
            "parent_tool_use_id": null, "session_id": CLI_SESSION, "uuid": Uuid::new_v4()})
        .to_string(),
    );
    let record = json!({"type": "user", "message": {"role": "user", "content": [
        {"tool_use_id": "toolu_c1", "type": "tool_result", "content": "ok"},
        {"tool_use_id": "toolu_c2", "type": "tool_result", "content": "ok"}]},
        "parent_tool_use_id": null, "session_id": CLI_SESSION, "uuid": Uuid::new_v4(),
        "tool_use_result": {"task": {"id": "5"}}});
    assert!(task_lists(core.on_line(&record.to_string())).is_empty());
    // Alone, with no `tasks` array: nothing either.
    assert!(task_call(
        &mut core,
        "TaskList",
        json!({}),
        false,
        "No tasks found",
        json!({})
    )
    .is_empty());
    // Alone, with the array: the list.
    let lists = task_call(
        &mut core,
        "TaskList",
        json!({}),
        false,
        "#1 [completed] Keep",
        json!({"tasks": [{"id": "1", "subject": "Keep", "status": "completed"}]}),
    );
    assert_eq!(lists, [vec![task("Keep", Completed, Some("Doing Keep"))]]);
}

/// The same in the replay of a transcript: a `TaskList` result sharing its record with another
/// result does not replace the list, and a `TaskCreate` of such a record takes its id from the
/// text.
#[test]
fn the_replay_of_a_task_list_result_of_a_record_of_several_changes_nothing() {
    let line = |v: Value| v.to_string();
    let transcript = [
        line(json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "c1", "name": "TaskCreate", "input": create_input("Keep")}]}})),
        line(json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "c1",
             "content": "Task #1 created successfully: Keep"}]},
            "toolUseResult": {"task": {"id": "1"}}})),
        line(json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "l1", "name": "TaskList", "input": {}},
            {"type": "tool_use", "id": "g1", "name": "TaskGet", "input": {}}]}})),
        line(json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "l1", "content": "#1 [pending] Keep"},
            {"type": "tool_result", "tool_use_id": "g1", "content": "Task #1: Keep"}]},
            "toolUseResult": {"tasks": []}})),
    ]
    .join("\n");
    let list = super::engine::resurrected_tasks(&transcript);
    assert_eq!(list.len(), 1);
}

/// A subject and an `active_form` are cut at 512 bytes, the list at 200 tasks.
#[test]
fn the_task_list_is_cut_to_its_limits() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let long = "я".repeat(300); // 600 bytes
    let todos: Vec<Value> = (0..250)
        .map(|i| {
            json!({"content": if i == 0 { long.clone() } else { format!("T{i}") },
                "status": "pending", "activeForm": long})
        })
        .collect();
    let lists = task_call(
        &mut core,
        "TodoWrite",
        json!({"todos": todos}),
        false,
        "ok",
        json!({}),
    );
    let [list] = &lists[..] else {
        panic!("{lists:?}")
    };
    assert_eq!(list.len(), 200);
    assert_eq!(list[0].subject.len(), 512);
    assert_eq!(list[0].active_form.as_deref().map(str::len), Some(512));
    // A created task over the limit is not added either.
    assert!(task_create(&mut core, "9", "Over").is_empty());
}

/// A transcript of the main agent's tasks: three created (the last two by parallel calls whose
/// results share a record), the first one completed, a subagent's create and an update that
/// found no task not counted.
fn tasks_transcript() -> String {
    let call = |side: bool, calls: Vec<(&str, &str, Value)>| {
        let blocks: Vec<Value> = calls
            .into_iter()
            .map(|(id, tool, input)| json!({"type": "tool_use", "id": id, "name": tool, "input": input}))
            .collect();
        json!({"type": "assistant", "isSidechain": side, "message": {"content": blocks}})
            .to_string()
    };
    let result = |side: bool, results: Vec<(&str, &str)>, data: Value| {
        let blocks: Vec<Value> = results
            .into_iter()
            .map(|(id, text)| json!({"type": "tool_result", "tool_use_id": id, "content": text}))
            .collect();
        json!({"type": "user", "isSidechain": side, "message": {"content": blocks},
            "toolUseResult": data})
        .to_string()
    };
    [
        call(false, vec![("toolu_r1", "TaskCreate", create_input("One"))]),
        result(
            false,
            vec![("toolu_r1", "Task #1 created successfully: One")],
            json!({"task": {"id": "1"}}),
        ),
        call(
            false,
            vec![
                ("toolu_r2", "TaskCreate", create_input("Two")),
                ("toolu_r3", "TaskCreate", create_input("Three")),
            ],
        ),
        result(
            false,
            vec![
                ("toolu_r2", "Task #2 created successfully: Two"),
                ("toolu_r3", "Task #3 created successfully: Three"),
            ],
            json!({"task": {"id": "2"}}),
        ),
        call(true, vec![("toolu_s1", "TaskCreate", create_input("Sub"))]),
        result(
            true,
            vec![("toolu_s1", "Task #4 created successfully: Sub")],
            json!({"task": {"id": "4"}}),
        ),
        call(
            false,
            vec![(
                "toolu_r4",
                "TaskUpdate",
                json!({"taskId": "1", "status": "completed"}),
            )],
        ),
        result(
            false,
            vec![("toolu_r4", "Updated task #1 status")],
            json!({"success": true}),
        ),
        call(
            false,
            vec![(
                "toolu_r5",
                "TaskUpdate",
                json!({"taskId": "9", "status": "completed"}),
            )],
        ),
        result(
            false,
            vec![("toolu_r5", "Task #9 not found")],
            json!({"success": false}),
        ),
    ]
    .join("\n")
}

/// A resumed process starts with the task list its transcript makes, as the CLI does: a live
/// `TaskUpdate` then sends the whole list, the three tasks with their states. Seeding sends no
/// event (the hub has the latest one).
#[test]
fn a_resumed_core_has_the_tasks_of_its_transcript() {
    use TaskStatus::*;
    let seeded = super::engine::resurrected_tasks(&tasks_transcript());
    let mut core = Core::new(Uuid::now_v7(), false).with_resumed_tasks(seeded);
    assert!(core.resumed_live().is_empty(), "no event for the seed");
    let lists = task_update(
        &mut core,
        json!({"taskId": "2", "status": "in_progress"}),
        true,
    );
    assert_eq!(
        lists,
        [vec![
            task("One", Completed, Some("Doing One")),
            task("Two", InProgress, Some("Doing Two")),
            task("Three", Pending, Some("Doing Three")),
        ]]
    );
}

/// Sends one input to a started adapter (the scripted CLI plays its stream after the first
/// one) and returns the task lists the process sends.
async fn task_lists_after_input(
    a: &mut Box<dyn Adapter>,
    rx: &mut mpsc::Receiver<AdapterEvent>,
    session_id: Uuid,
) -> Vec<Vec<AgentTask>> {
    a.input(&SessionInput {
        session_id,
        event_id: Uuid::now_v7(),
        text: "go".into(),
        artifact_ids: vec![],
        peer_prefix: vec![],
        peer: None,
        permission_cap: None,
        schedule_id: None,
        human: false,
    })
    .await
    .unwrap();
    let mut lists = Vec::new();
    let mut quiet = false;
    while !quiet {
        // Up to 5 s for the first list, then 200 ms of quiet for any more.
        let wait = if lists.is_empty() { 5000 } else { 200 };
        match tokio::time::timeout(Duration::from_millis(wait), rx.recv()).await {
            Ok(Some(AdapterEvent::Event(Event {
                kind: EventKind::Tasks { items },
                ..
            }))) => lists.push(items),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => quiet = true,
        }
    }
    lists
}

/// The same through the adapter: a `--resume` of a transcript starts its process with its tasks,
/// which a `TaskUpdate` the process makes then completes into the whole list; a fork of one (a
/// copy cut at the fork point) starts with none, as the CLI's, which keeps the list per session
/// id: the task its process creates is the whole list.
#[tokio::test]
async fn a_resume_keeps_the_tasks_of_the_transcript_and_a_fork_starts_without() {
    use TaskStatus::*;
    let stream = [
        tool_call(
            "toolu_l1",
            "TaskUpdate",
            json!({"taskId": "3", "status": "completed"}),
            None,
        ),
        tool_done_text(
            "toolu_l1",
            false,
            "Updated task #3 status",
            json!({"success": true}),
        ),
    ]
    .join("\n");
    let want = vec![
        task("One", Completed, Some("Doing One")),
        task("Two", Pending, Some("Doing Two")),
        task("Three", Completed, Some("Doing Three")),
    ];
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let transcript = tasks_transcript();
    // Resume.
    let claude = ScriptedClaude::new(&stream);
    let factory = ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude.clone()));
    let runtime = Uuid::new_v4();
    let mut s = spec(Uuid::now_v7(), work.path(), runtime);
    s.has_history = true;
    let t = super::paths::transcript_path(config.path(), &s.workdir, &runtime);
    std::fs::create_dir_all(t.parent().unwrap()).unwrap();
    std::fs::write(&t, &transcript).unwrap();
    let (tx, mut rx) = mpsc::channel(64);
    let mut a = factory.create(&s, tx);
    a.resume(&s).await.unwrap();
    let lists = task_lists_after_input(&mut a, &mut rx, s.session_id).await;
    assert_eq!(lists, std::slice::from_ref(&want), "resume");
    a.kill().await.unwrap();
    // Fork: the transcript up to the point, then a task made after it.
    let (turns, ids) = two_turns();
    let (source, copy) = (Uuid::new_v4(), Uuid::new_v4());
    let from = super::paths::transcript_path(config.path(), work.path(), &source);
    std::fs::create_dir_all(from.parent().unwrap()).unwrap();
    let after = transcript
        .replace("Three", "Later")
        .replace("toolu_r", "toolu_x");
    std::fs::write(&from, format!("{transcript}\n{turns}\n{after}")).unwrap();
    let fork_stream = [
        tool_call("toolu_f1", "TaskCreate", create_input("Fresh"), None),
        tool_done_text(
            "toolu_f1",
            false,
            "Task #4 created successfully: Fresh",
            json!({"task": {"id": "4"}}),
        ),
    ]
    .join("\n");
    let claude = ScriptedClaude::new(&fork_stream);
    let factory = ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude.clone()));
    let mut s = spec(Uuid::now_v7(), work.path(), copy);
    s.fork = Some(hqc_proto::frame::ForkFrom {
        runtime_session_id: source.to_string(),
        at_event_ids: vec![super::engine::record_event_id(&ids[1], 1)],
    });
    let (tx, mut rx) = mpsc::channel(64);
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    let lists = task_lists_after_input(&mut a, &mut rx, s.session_id).await;
    assert_eq!(
        lists,
        [vec![task("Fresh", Pending, Some("Doing Fresh"))]],
        "fork"
    );
    a.kill().await.unwrap();
}

/// A copy made by a fork has the source's records up to the cut in its own transcript, with their
/// uuids, and the CLI's list of the copy only the tasks made after the fork (numbered from #1
/// again). On the copy's second start (its transcript exists, `fork` is still sent) the replay
/// takes only what follows the cut.
#[tokio::test]
async fn a_copys_resume_replays_only_what_follows_its_cut() {
    use TaskStatus::*;
    let (turns, ids) = two_turns();
    let own = [
        json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "toolu_own", "name": "TaskCreate",
             "input": create_input("Fresh")}]}}),
        json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "toolu_own",
             "content": "Task #1 created successfully: Fresh"}]}}),
    ]
    .map(|v| v.to_string())
    .join("\n");
    // The source's three tasks, the turns up to the cut, then the copy's own task.
    let text = format!("{}\n{turns}\n{own}\n", tasks_transcript());
    let at = vec![super::engine::record_event_id(&ids[1], 1)];
    let fork = hqc_proto::frame::ForkFrom {
        runtime_session_id: Uuid::new_v4().to_string(),
        at_event_ids: at,
    };
    // What a live update of task #1 (the copy's own) sends on top of a replay.
    let shown = |tasks| {
        let mut core = Core::new(Uuid::now_v7(), false).with_resumed_tasks(tasks);
        task_update(
            &mut core,
            json!({"taskId": "1", "status": "completed"}),
            true,
        )
    };
    let own_only = vec![vec![task("Fresh", Completed, Some("Doing Fresh"))]];
    assert_eq!(shown(super::resumed_tasks(&text, Some(&fork))), own_only);
    // A cut that is not in the transcript: all of it, as a plain resume.
    let lost = hqc_proto::frame::ForkFrom {
        runtime_session_id: fork.runtime_session_id.clone(),
        at_event_ids: vec![Uuid::new_v4()],
    };
    let all = shown(super::resumed_tasks(&text, None));
    assert_ne!(all, own_only);
    assert_eq!(shown(super::resumed_tasks(&text, Some(&lost))), all);
}

/// The same through the adapter, which is what passes the fork on: a copy's second start (its
/// transcript exists, holding the source's records up to the cut and then the copy's own task;
/// `fork` is still in `StartSpec`) starts its process with the copy's own task only.
#[tokio::test]
async fn a_copys_second_start_resumes_with_its_own_tasks_only() {
    use TaskStatus::*;
    let (turns, ids) = two_turns();
    let own = [
        json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "toolu_own", "name": "TaskCreate",
             "input": create_input("Fresh")}]}}),
        json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "toolu_own",
             "content": "Task #1 created successfully: Fresh"}]}}),
    ]
    .map(|v| v.to_string())
    .join("\n");
    let text = format!("{}\n{turns}\n{own}\n", tasks_transcript());
    // The process then completes task #1, the copy's own, which sends the whole list.
    let stream = [
        tool_call(
            "toolu_u1",
            "TaskUpdate",
            json!({"taskId": "1", "status": "completed"}),
            None,
        ),
        tool_done_text(
            "toolu_u1",
            false,
            "Updated task #1 status",
            json!({"success": true}),
        ),
    ]
    .join("\n");
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let claude = ScriptedClaude::new(&stream);
    let factory = ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude.clone()));
    let copy = Uuid::new_v4();
    let mut s = spec(Uuid::now_v7(), work.path(), copy);
    s.has_history = true;
    s.fork = Some(hqc_proto::frame::ForkFrom {
        runtime_session_id: Uuid::new_v4().to_string(),
        at_event_ids: vec![super::engine::record_event_id(&ids[1], 1)],
    });
    let t = super::paths::transcript_path(config.path(), &s.workdir, &copy);
    std::fs::create_dir_all(t.parent().unwrap()).unwrap();
    std::fs::write(&t, &text).unwrap();
    let (tx, mut rx) = mpsc::channel(64);
    let mut a = factory.create(&s, tx);
    a.resume(&s).await.unwrap();
    let lists = task_lists_after_input(&mut a, &mut rx, s.session_id).await;
    assert_eq!(lists, [vec![task("Fresh", Completed, Some("Doing Fresh"))]]);
    a.kill().await.unwrap();
}

/// `/clear` starts a new conversation: its task list is empty, said once.
#[test]
fn a_clear_empties_the_task_list() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let todos = json!({"todos": [{"content": "A", "status": "pending", "activeForm": "Aing"}]});
    core.on_line(&tool_call("toolu_c", "TodoWrite", todos, None));
    assert_eq!(
        task_lists(core.on_line(&tool_done("toolu_c", false, json!({}), None))).len(),
        1
    );
    let reset =
        json!({"type": "conversation_reset", "trigger": "clear", "session_id": "s"}).to_string();
    assert_eq!(task_lists(core.on_line(&reset)), [vec![]]);
    assert!(task_lists(core.on_line(&reset)).is_empty());
}

/// The CLI fires a cron job with a `command_lifecycle` `started` with `wakeup_source:
/// schedule_wakeup` that names no job: the one due next goes, a one-shot job off the list, a
/// recurring job to its next fire (`*/5 * * * *`: 285 s from the fire). One not due within 90 s
/// is the CLI's notice of tasks it missed: nothing changes. A recurring job 7 days old at its
/// fire goes too.
#[test]
fn a_cron_fire_takes_the_job_due_next() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let create = |core: &mut Core, call: &str, id: &str, cron: &str, recurring: bool, next: i64| {
        core.on_line(&tool_call(
            call,
            "CronCreate",
            json!({"cron": cron, "prompt": id, "recurring": recurring}),
            None,
        ));
        core.on_line(&tool_done(
            call,
            false,
            json!({"id": id, "humanSchedule": cron, "recurring": recurring, "durable": false, "nextFireAt": next}),
            None,
        ));
    };
    let now = now_ms();
    create(
        &mut core,
        "toolu_1",
        "aaaaaaaa",
        "30 14 28 2 *",
        false,
        now + 3_000,
    );
    create(
        &mut core,
        "toolu_2",
        "80000000",
        "*/5 * * * *",
        true,
        now + 1_000,
    );
    create(
        &mut core,
        "toolu_3",
        "bbbbbbbb",
        "0 9 * * *",
        true,
        now + 3_600_000,
    );
    let due_of = |lists: &[Vec<LiveSubagent>], id: &str| {
        lists[0]
            .iter()
            .find(|r| r.task_id == id)
            .map(|r| r.due_in_ms)
    };
    // The recurring one is due first; its next fire is 285 s on.
    let lists = live_rows(core.on_line(&fire("schedule_wakeup", None)));
    assert_eq!(lists[0].len(), 3);
    assert!(
        (284_000..=285_000).contains(&due_of(&lists, "80000000").unwrap().unwrap()),
        "{lists:?}"
    );
    // The one-shot one is next, and goes.
    let lists = live_rows(core.on_line(&fire("schedule_wakeup", None)));
    let ids: Vec<_> = lists[0].iter().map(|r| r.task_id.as_str()).collect();
    assert_eq!(ids, ["80000000", "bbbbbbbb"]);
    // The rest is due in more than 90 s: a notice of missed tasks.
    assert!(live_rows(core.on_line(&fire("schedule_wakeup", None))).is_empty());
    // A recurring job a week old at its fire is not put off again.
    let lists = live_rows(core.cron_fired(now + 604_800_000 + 3_600_000));
    let ids: Vec<_> = lists[0].iter().map(|r| r.task_id.as_str()).collect();
    assert_eq!(ids, ["bbbbbbbb"], "the job due next was the 5-minute one");
}

/// A transcript of cron jobs as of `now` (epoch ms): see
/// [`a_resume_brings_back_the_cron_jobs_of_the_transcript`] for what each of them is. The
/// deletion of the second job has no timestamp: the CLI counts it all the same.
fn cron_transcript(now: i64) -> String {
    let stamp = |ago_ms: i64| {
        chrono::DateTime::from_timestamp_millis(now - ago_ms)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    };
    let mut lines = Vec::new();
    // A job: its `CronCreate` call, `ago_ms` before the resume, and the result of it.
    let mut create = |id: &str,
                      ago_ms: i64,
                      recurring: bool,
                      durable: bool,
                      next: i64,
                      side: bool| {
        let call = format!("toolu_{id}");
        lines.push(
            json!({"type": "assistant", "isSidechain": side, "timestamp": stamp(ago_ms), "message": {"content": [
                {"type": "tool_use", "id": call, "name": "CronCreate",
                 "input": {"cron": "*/2 * * * *", "prompt": format!("prompt {id}"), "recurring": recurring}}]}})
            .to_string(),
        );
        lines.push(
            json!({"type": "user", "isSidechain": side, "timestamp": stamp(ago_ms), "message": {"content": [
                {"type": "tool_result", "tool_use_id": call, "content": "ok"}]},
                "toolUseResult": {"id": id, "humanSchedule": "Every 2 minutes", "recurring": recurring,
                                  "durable": durable, "nextFireAt": next}})
            .to_string(),
        );
    };
    create("11111111", 3_600_000, true, false, now + 1, false);
    create("22222222", 3_600_000, true, false, now + 1, false);
    create("33333333", 8 * 86_400_000, true, false, now + 1, false);
    create("44444444", 3_600_000, true, true, now + 1, false);
    create("55555555", 3_600_000, false, false, now - 60_000, false);
    create("66666666", 3_600_000, false, false, now + 3_600_000, false);
    create("77777777", 3_600_000, true, false, now + 1, true);
    lines.push(
        json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "toolu_del", "name": "CronDelete", "input": {"id": "22222222"}}]}})
        .to_string(),
    );
    lines.join("\n")
}

/// A resume brings back the session's cron jobs as the CLI resurrects them from the
/// transcript (checked live on 2.1.312: a `*/2` job kept firing after a resume): a recurring
/// job made less than 7 days ago, due from `next_cron_fire` at the resume, and a one-shot job
/// not yet due. Not a deleted one, a one 8 days old, a `durable` one (it lives in a file), a
/// one-shot one in the past, nor a subagent's call, and a deletion without a timestamp counts.
/// The row's run time counts from its call.
#[test]
fn a_resume_brings_back_the_cron_jobs_of_the_transcript() {
    let now = now_ms();
    let transcript = cron_transcript(now);

    let core =
        Core::new(Uuid::now_v7(), false).with_resumed_cron(resurrected_cron_jobs(&transcript, now));
    let before = now_ms();
    let lists = live_rows(core.resumed_live());
    let after = now_ms();
    let [rows] = &lists[..] else {
        panic!("{lists:?}")
    };
    let ids: Vec<_> = rows.iter().map(|r| r.task_id.as_str()).collect();
    assert_eq!(ids, ["11111111", "66666666"]);
    assert!(rows.iter().all(|r| r.kind == Some(LiveKind::Cron)));
    let first = &rows[0];
    assert_eq!(first.description, "prompt 11111111");
    assert_eq!(first.activity.as_deref(), Some("Every 2 minutes"));
    assert_eq!(first.call_id.as_deref(), Some("toolu_11111111"));
    assert!(first.duration_ms >= 3_600_000, "{first:?}");
    let due = next_cron_fire("*/2 * * * *", now, "11111111").unwrap();
    let due_in = first.due_in_ms.unwrap() as i64;
    assert!(
        (due - after..=due - before).contains(&due_in),
        "{due_in} against {}",
        due - now
    );
    let one_shot = rows[1].due_in_ms.unwrap();
    assert!((3_599_000..=3_600_000).contains(&one_shot), "{one_shot}");

    // Without a job there is no list.
    assert!(Core::new(Uuid::now_v7(), false)
        .with_resumed_cron(vec![])
        .resumed_live()
        .is_empty());
}

/// The ids of the cron rows in the first event an adapter sent: it must be the list of rows.
fn first_cron_ids(rx: &mut mpsc::Receiver<AdapterEvent>) -> Vec<String> {
    let Ok(AdapterEvent::LiveSubagents(rows)) = rx.try_recv() else {
        panic!("the first event is not the list of rows");
    };
    assert!(rows.iter().all(|r| r.kind == Some(LiveKind::Cron)));
    rows.into_iter().map(|r| r.task_id).collect()
}

/// SPEC §8.1: the first event of a resumed process is the list of the cron jobs the CLI
/// resurrects from the transcript, as the adapter reads it.
#[tokio::test]
async fn a_resumed_process_starts_with_the_cron_jobs_of_its_transcript() {
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let claude = ScriptedClaude::new("");
    let factory = ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude.clone()));
    let runtime = Uuid::new_v4();
    let mut s = spec(Uuid::now_v7(), work.path(), runtime);
    s.has_history = true;
    let t = super::paths::transcript_path(config.path(), &s.workdir, &runtime);
    std::fs::create_dir_all(t.parent().unwrap()).unwrap();
    std::fs::write(&t, cron_transcript(now_ms())).unwrap();
    let (tx, mut rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx);
    a.resume(&s).await.unwrap();
    assert!(claude.commands()[0].args.contains(&"--resume".to_string()));
    assert_eq!(first_cron_ids(&mut rx), ["11111111", "66666666"]);
    a.kill().await.unwrap();
}

/// SPEC §8.1: a fork's copy resurrects the cron jobs of the source up to the fork point, so
/// the first event of its process lists them; a job made after the point is not in it.
#[tokio::test]
async fn a_fork_starts_with_the_cron_jobs_of_the_copied_conversation() {
    let (turns, ids) = two_turns();
    let (source, runtime) = (Uuid::new_v4(), Uuid::new_v4());
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let from = super::paths::transcript_path(config.path(), work.path(), &source);
    std::fs::create_dir_all(from.parent().unwrap()).unwrap();
    let now = now_ms();
    // The jobs, the conversation, then a job after the fork point.
    let after = cron_transcript(now).replace("11111111", "99999999");
    std::fs::write(&from, format!("{}\n{turns}\n{after}", cron_transcript(now))).unwrap();
    let claude = ScriptedClaude::new("");
    let factory = ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude.clone()));
    let mut s = spec(Uuid::now_v7(), work.path(), runtime);
    s.fork = Some(hqc_proto::frame::ForkFrom {
        runtime_session_id: source.to_string(),
        at_event_ids: vec![super::engine::record_event_id(&ids[1], 1)],
    });
    let (tx, mut rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    assert!(claude.commands()[0]
        .args
        .contains(&"--fork-session".to_string()));
    assert_eq!(first_cron_ids(&mut rx), ["11111111", "66666666"]);
    a.kill().await.unwrap();
}

/// The time to the next fire of a recurring job, [`next_cron_fire`], against the CLI's formula
/// (`Hmn`): the next match of the expression in local time, put off by (first 8 hex digits of
/// the id / 2^32) * half the interval to the match after it, at most 30 minutes; `*/5` fires
/// 15 s short of 5 minutes from the fire. The matches are found here minute by minute.
#[test]
fn the_next_fire_of_a_recurring_job_is_the_clis() {
    use chrono::{Datelike, Local, TimeZone, Timelike};
    let at = |ms: i64| Local.timestamp_millis_opt(ms).unwrap();
    // The first minute after `ms` that `matches`, in epoch ms.
    let next = |ms: i64, matches: &dyn Fn(chrono::DateTime<Local>) -> bool| {
        let mut t = (ms / 60_000 + 1) * 60_000;
        while !matches(at(t)) {
            t += 60_000;
        }
        t
    };
    let fire = 1_794_000_030_000_i64;
    type Matcher<'a> = &'a dyn Fn(chrono::DateTime<Local>) -> bool;
    let hourly = |t: chrono::DateTime<Local>| t.minute() == 17;
    let every_ten = |t: chrono::DateTime<Local>| t.minute().is_multiple_of(10);
    let weekdays = |t: chrono::DateTime<Local>| {
        t.hour() == 9 && t.minute() == 0 && (1..=5).contains(&t.weekday().num_days_from_sunday())
    };
    let cases: [(&str, Matcher, &str, f64); 5] = [
        ("17 * * * *", &hourly, "80000000", 0.5),
        ("17 * * * *", &hourly, "00000000", 0.0),
        // Half an hour at most: 0.99999 * 0.5 * 1 h is capped.
        ("17 * * * *", &hourly, "ffffffff", 1.0),
        ("*/10 * * * *", &every_ten, "40000000", 0.25),
        ("0 9 * * 1-5", &weekdays, "c0000000", 0.75),
    ];
    for (cron, matches, id, frac) in cases {
        let first = next(fire, matches);
        let interval = next(first, matches) - first;
        let offset = (frac * 0.5 * interval as f64).min(1_800_000.0);
        let got = next_cron_fire(cron, fire, id).unwrap();
        assert!(
            (got as f64 - (first as f64 + offset)).abs() <= 1.0,
            "{cron} {id}: {got} for {first} + {offset}"
        );
    }
    // An id that is not hex puts nothing off.
    assert_eq!(
        next_cron_fire("17 * * * *", fire, "zzzz"),
        Some(next(fire, &hourly))
    );
    // `*/5 * * * *`: an interval of 5 minutes, the early shift, whatever the id.
    for id in ["00000000", "80000000", "ffffffff"] {
        assert_eq!(
            next_cron_fire("*/5 * * * *", fire, id),
            Some(fire + 285_000)
        );
    }
    // `*/7` (gaps of 7 and 4 minutes) is no case of the early shift: the offset is that of any
    // job, from the gap after the next match.
    let seven = |t: chrono::DateTime<Local>| t.minute().is_multiple_of(7);
    let first = next(fire, &seven);
    let interval = next(first, &seven) - first;
    let got = next_cron_fire("*/7 * * * *", fire, "80000000").unwrap();
    assert_eq!(got, first + (0.25 * interval as f64).round() as i64);
    // Not a cron expression: none.
    assert_eq!(next_cron_fire("often", fire, "80000000"), None);
}

/// Subagent `A` with a command `c` of its own, and the agent's own command `m`, each listed
/// before its `task_started`; `A` hands its report over while `c` still runs, the CLI's list
/// drops `A`, and the `result` reports the drop.
const COMMANDS_BESIDE_A_SUBAGENT: [&str; 8] = [
    r#"{"type":"system","subtype":"task_started","task_id":"A","description":"Build the boards","is_backgrounded":true,"task_type":"local_agent"}"#,
    r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"A","task_type":"local_agent","description":"Build the boards"},{"task_id":"c","task_type":"local_bash","description":"make boards","parent_task_id":"A"},{"task_id":"m","task_type":"local_bash","description":"Run the full pass"}]}"#,
    r#"{"type":"system","subtype":"task_started","task_id":"c","owned_by_subagent":true,"tool_use_id":"toolu_01Make","description":"make boards","is_backgrounded":true,"parent_task_id":"A","task_type":"local_bash"}"#,
    r#"{"type":"system","subtype":"task_started","task_id":"m","tool_use_id":"toolu_01Pass","description":"Run the full pass","is_backgrounded":true,"task_type":"local_bash"}"#,
    r#"{"type":"system","subtype":"task_updated","task_id":"A","patch":{"status":"completed"}}"#,
    r#"{"type":"system","subtype":"task_notification","task_id":"A","status":"completed","handback":"send","summary":"reported"}"#,
    r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"c","task_type":"local_bash","description":"make boards","parent_task_id":"A"},{"task_id":"m","task_type":"local_bash","description":"Run the full pass"}]}"#,
    r#"{"type":"result","subtype":"success","num_turns":1}"#,
];

/// [`COMMANDS_BESIDE_A_SUBAGENT`] before the report reaches the list: the command of the subagent
/// that counts has no row of its own, the subagent's row stands for it; the agent's own command
/// follows the subagent from its `task_started`. No command changes the subagent count.
#[test]
fn a_command_of_a_counted_subagent_is_not_listed() {
    let a = live_row("A", "Build the boards", None);
    let m = live_row("m", "Run the full pass", None);
    assert_eq!(
        live_lists(COMMANDS_BESIDE_A_SUBAGENT[..7].iter().copied()),
        [
            vec![vec![a.clone()]],
            vec![],
            vec![],
            vec![vec![a, m]],
            vec![],
            vec![],
            vec![]
        ]
    );
    let subagents = |lines: &[String]| -> Vec<Vec<String>> {
        task_counts(lines.iter().map(String::as_str))
            .into_iter()
            .map(|e| {
                e.into_iter()
                    .filter(|e| e.starts_with("subagents"))
                    .collect()
            })
            .collect()
    };
    let with: Vec<String> = COMMANDS_BESIDE_A_SUBAGENT.map(str::to_owned).to_vec();
    let without: Vec<String> = with
        .iter()
        .map(|l| {
            let mut v: Value = serde_json::from_str(l).unwrap();
            if let Some(tasks) = v["tasks"].as_array_mut() {
                tasks.retain(|t| t["task_type"] != "local_bash");
            }
            v.to_string()
        })
        .collect();
    assert_eq!(subagents(&with), subagents(&without));
    assert_eq!(
        subagents(&with),
        [
            vec!["subagents:1".to_string()],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec!["subagents:0".to_string()]
        ]
    );
}

/// [`COMMANDS_BESIDE_A_SUBAGENT`]: once the subagent handed its report over, the command it left
/// running is the session's background work: it gets a row of its own, after the agent's command
/// listed before it, when the subagent's row goes.
#[test]
fn a_command_a_subagent_left_running_is_listed_after_its_report() {
    let lists = live_lists(COMMANDS_BESIDE_A_SUBAGENT.into_iter());
    assert_eq!(
        lists[7],
        [vec![
            live_row("m", "Run the full pass", None),
            live_row("c", "make boards", None)
        ]]
    );
}

/// [`COMMANDS_BESIDE_A_SUBAGENT`]: the run time of the command the subagent left counts from its
/// `task_started`, not from the report that made it a row.
#[test]
fn a_command_a_subagent_left_running_counts_from_its_start() {
    let mut core = Core::new(Uuid::now_v7(), false);
    for l in COMMANDS_BESIDE_A_SUBAGENT[..3].iter().copied() {
        core.on_line(l);
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
    let mut last = None;
    for l in COMMANDS_BESIDE_A_SUBAGENT[3..].iter().copied() {
        for a in core.on_line(l) {
            if let Action::Emit(AdapterEvent::LiveSubagents(list)) = a {
                last = Some(list);
            }
        }
    }
    let c = last
        .unwrap()
        .into_iter()
        .find(|s| s.task_id == "c")
        .unwrap();
    assert!(c.duration_ms >= 50, "{}", c.duration_ms);
}

/// [`COMMANDS_BESIDE_A_SUBAGENT`]: each row says whether it is a subagent or a command (SPEC
/// 0.141), so that a client can tell a command from a subagent that has not reported yet.
#[test]
fn a_row_says_whether_it_is_a_subagent_or_a_command() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let mut last = None;
    for l in COMMANDS_BESIDE_A_SUBAGENT[..4].iter().copied() {
        for a in core.on_line(l) {
            if let Action::Emit(AdapterEvent::LiveSubagents(list)) = a {
                last = Some(list);
            }
        }
    }
    let kinds: Vec<_> = last
        .unwrap()
        .into_iter()
        .map(|s| (s.task_id, s.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("A".to_owned(), Some(LiveKind::Agent)),
            ("m".to_owned(), Some(LiveKind::Command))
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

    // The new count goes before the end of the turn: a waiting move checks it there.
    let mut core = Core::new(Uuid::now_v7(), false);
    let mut lines = stream.lines();
    core.on_line(lines.next().unwrap());
    core.on_line(lines.next().unwrap());
    let order: Vec<&str> = core
        .on_line(lines.next().unwrap())
        .into_iter()
        .filter_map(|a| match a {
            Action::Emit(AdapterEvent::Subagents(_)) => Some("subagents"),
            Action::Emit(AdapterEvent::TurnEnded { .. }) => Some("turn ended"),
            _ => None,
        })
        .collect();
    assert_eq!(order, ["subagents", "turn ended"]);
}

/// A background subagent whose Bash call the CLI moves to the background (2.1.312 with
/// `CLAUDE_CODE_ENTRYPOINT=claude-vscode`, a subagent's 240 s `ping` and its Monitor
/// `blm1ph76i` waiting for it; the `system` task lines, `background_tasks_changed` and `result`
/// of the capture, in order, with the fields the adapter reads; the lines between them change
/// no count).
const MOVED_SUBAGENT: &str = r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a739e2460001cfdc1","task_type":"local_agent"}]}
{"type":"system","subtype":"task_started","task_id":"a739e2460001cfdc1","description":"Run ping and report done","is_backgrounded":true,"task_type":"local_agent"}
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
{"type":"system","subtype":"task_started","task_id":"a739e2460001cfdc1","description":"Run ping and report done","is_backgrounded":true,"task_type":"local_agent"}
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

/// The CLI's own watcher of the comments on a page the agent published (`ambient: true`, it
/// stays for the whole process) is no background work: the count leaves it out, also when it
/// reconnects and leaves and rejoins the list.
#[test]
fn an_ambient_task_is_no_background_work() {
    const LINES: &str = r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"w1","task_type":"monitor_ws","description":"Comments on the page","ambient":true}]}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"w1","task_type":"monitor_ws","description":"Comments on the page","ambient":true},{"task_id":"b1","task_type":"local_bash","description":"Run the checks"}]}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"w1","task_type":"monitor_ws","description":"Comments on the page","ambient":true}]}
{"type":"system","subtype":"background_tasks_changed","tasks":[]}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"w2","task_type":"monitor_ws","description":"Comments on the page","ambient":true}]}"#;
    let want: [&[&str]; 5] = [
        &[],
        &["background_tasks:1"],
        &["background_tasks:0"],
        &[],
        &[],
    ];
    assert_eq!(task_counts(LINES.lines()), want);
}

/// A subagent publishes a page and ends without a report (`handback`): 2.1.312 leaves the
/// page's comment watcher in the list with the subagent as its `parent_task_id`. The watcher
/// does not keep the subagent counted: it ends at the next line of another kind.
#[test]
fn an_ambient_task_of_a_subagent_does_not_keep_it_running() {
    const LINES: &str = r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"A","task_type":"local_agent"}]}
{"type":"system","subtype":"task_started","task_id":"A","description":"Publish the page","is_backgrounded":true,"task_type":"local_agent"}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"A","task_type":"local_agent"},{"task_id":"w1","task_type":"monitor_ws","description":"Comments on the page","parent_task_id":"A","ambient":true}]}
{"type":"system","subtype":"task_updated","task_id":"A","patch":{"status":"completed"}}
{"type":"system","subtype":"task_notification","task_id":"A","status":"completed","summary":"Published the page."}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"w1","task_type":"monitor_ws","description":"Comments on the page","parent_task_id":"A","ambient":true}]}
{"type":"result","subtype":"success","num_turns":1}"#;
    let want: [&[&str]; 7] = [
        &["background_tasks:1", "subagents:1"],
        &[],
        &[],
        &[],
        &[],
        &["background_tasks:0"],
        &["subagents:0", "turn_ended"],
    ];
    assert_eq!(task_counts(LINES.lines()), want);
}

/// A task whose work runs outside the process (`off_host: true`, 2.1.325 in a cloud session) is
/// no background work: the count leaves it out.
#[test]
fn an_off_host_task_is_no_background_work() {
    const LINES: &str = r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"w1","task_type":"mcp_task","description":"Work in the cloud","off_host":true}]}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"w1","task_type":"mcp_task","description":"Work in the cloud","off_host":true},{"task_id":"b1","task_type":"local_bash","description":"Run the checks"}]}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"w1","task_type":"mcp_task","description":"Work in the cloud","off_host":true}]}
{"type":"system","subtype":"background_tasks_changed","tasks":[]}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"w2","task_type":"mcp_task","description":"Work in the cloud","off_host":true}]}"#;
    let want: [&[&str]; 5] = [
        &[],
        &["background_tasks:1"],
        &["background_tasks:0"],
        &[],
        &[],
    ];
    assert_eq!(task_counts(LINES.lines()), want);
}

/// A subagent ends without a report (`handback`) and an `off_host` task of it stays in the list
/// with the subagent as its `parent_task_id`. The task does not keep the subagent counted: it
/// ends at the next line of another kind.
#[test]
fn an_off_host_task_of_a_subagent_does_not_keep_it_running() {
    const LINES: &str = r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"A","task_type":"local_agent"}]}
{"type":"system","subtype":"task_started","task_id":"A","description":"Start the work","is_backgrounded":true,"task_type":"local_agent"}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"A","task_type":"local_agent"},{"task_id":"w1","task_type":"mcp_task","description":"Work in the cloud","parent_task_id":"A","off_host":true}]}
{"type":"system","subtype":"task_updated","task_id":"A","patch":{"status":"completed"}}
{"type":"system","subtype":"task_notification","task_id":"A","status":"completed","summary":"Started the work."}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"w1","task_type":"mcp_task","description":"Work in the cloud","parent_task_id":"A","off_host":true}]}
{"type":"result","subtype":"success","num_turns":1}"#;
    let want: [&[&str]; 7] = [
        &["background_tasks:1", "subagents:1"],
        &[],
        &[],
        &[],
        &[],
        &["background_tasks:0"],
        &["subagents:0", "turn_ended"],
    ];
    assert_eq!(task_counts(LINES.lines()), want);
}

/// [`MOVED_SUBAGENT`]: the CLI lists the subagent before its `task_started`, so it is shown
/// with no title until that start gives it one; its start again when the `ping` ends sends
/// nothing; it goes when it stops counting. That start keeps its run time from the first one
/// and its place before a subagent that started after it.
#[test]
fn a_subagent_waiting_on_its_background_command_stays_listed() {
    let id = "a739e2460001cfdc1";
    let mut want: Vec<Vec<Vec<LiveRow>>> = vec![vec![]; 25];
    want[0] = vec![vec![live_row(id, "", None)]];
    want[1] = vec![vec![live_row(id, "Run ping and report done", None)]];
    want[23] = vec![vec![]];
    assert_eq!(live_lists(MOVED_SUBAGENT.lines()), want);

    let mut core = Core::new(Uuid::now_v7(), false);
    let lines: Vec<&str> = MOVED_SUBAGENT.lines().collect();
    for l in &lines[..2] {
        core.on_line(l);
    }
    core.on_line(r#"{"type":"system","subtype":"task_started","task_id":"b","description":"Other","is_backgrounded":true,"task_type":"local_agent"}"#);
    for l in &lines[2..16] {
        core.on_line(l);
    }
    std::thread::sleep(Duration::from_millis(30));
    core.on_line(lines[16]);
    let list = core
        .on_line(r#"{"type":"system","subtype":"task_progress","task_id":"a739e2460001cfdc1","description":"Running Ping localhost","usage":{"total_tokens":100,"tool_uses":2}}"#)
        .into_iter()
        .find_map(|a| match a {
            Action::Emit(AdapterEvent::LiveSubagents(list)) => Some(list),
            _ => None,
        })
        .unwrap();
    let ids: Vec<&str> = list.iter().map(|s| s.task_id.as_str()).collect();
    assert_eq!(ids, [id, "b"]);
    assert_eq!(list[0].description, "Run ping and report done");
    assert!(list[0].duration_ms >= 30, "{list:?}");
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

/// [`MOVED_SUBAGENT`] with the subagent's report as 2.1.312 sends it in Auto: the real end's
/// `summary` is a note for the model, the report is `handback_report`, which the line carries
/// instead, its `warning` (`handback: flagged`) above its text; an empty report leaves the
/// `summary`.
#[test]
fn a_subagents_end_with_its_report_carries_the_report() {
    let with = |report: &str| {
        let lines = MOVED_SUBAGENT.replace(
            r#""handback":"send","#,
            &format!(r#""handback":"send","handback_report":{report},"#),
        );
        system_lines(lines.lines())
            .into_iter()
            .map(|(_, _, t)| t)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        with(r#"{"text":"PING-DONE: 240 packets\n"}"#),
        ["PING-DONE: 240 packets"]
    );
    assert_eq!(
        with(r#"{"warning":"The report may be incomplete.","text":"PING-DONE"}"#),
        ["The report may be incomplete.\n\nPING-DONE"]
    );
    assert_eq!(
        with(r#"{"text":"  "}"#),
        ["This agent's report was delivered to you as a message from \"a739e2460001cfdc1\" (its SubagentHandback call). Read it there; it is not repeated here."]
    );
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
/// end, the placeholder held while that task is listed, gives no second line.
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
        r#"{"type":"system","subtype":"task_notification","task_id":"A","status":"completed","summary":"This agent has not reported yet: it is waiting on its own background work and will deliver its report through SubagentHandback when that finishes.\n"}"#,
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
/// does not count `A` out at once: the next list holds `A` again, and its end waits. Its held
/// end, the later of its two placeholders, comes when a list drops it, at the next line of
/// another kind.
#[test]
fn a_held_subagent_listed_again_with_another_keeps_its_end() {
    let waits = |n: &str| {
        format!(
            r#"{{"type":"system","subtype":"task_notification","task_id":"A","status":"completed","summary":"This agent has not reported yet ({n})"}}"#
        )
    };
    let (first, second) = (waits("first"), waits("second"));
    let lines = [
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"A","task_type":"local_agent"}]}"#,
        r#"{"type":"system","subtype":"task_started","task_id":"A","is_backgrounded":true,"task_type":"local_agent"}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"A","task_type":"local_agent"},{"task_id":"c","task_type":"local_bash","parent_task_id":"A"}]}"#,
        r#"{"type":"system","subtype":"task_updated","task_id":"A","patch":{"status":"completed"}}"#,
        first.as_str(),
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"B","task_type":"local_agent"}]}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"A","task_type":"local_agent"},{"task_id":"B","task_type":"local_agent"}]}"#,
        r#"{"type":"result","subtype":"success","num_turns":1}"#,
        second.as_str(),
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"B","task_type":"local_agent"}]}"#,
        r#"{"type":"result","subtype":"success","num_turns":0}"#,
    ];
    assert_eq!(
        system_lines(lines.into_iter()),
        [(
            10,
            "task_notification".to_string(),
            "This agent has not reported yet (second)".to_string()
        )]
    );
}

/// A background subagent that hands its report over while its 200 s `ping`, moved to the
/// background, still runs; when the `ping` ends the CLI lists and starts it again, and it
/// reports again (2.1.312 with `CLAUDE_CODE_ENTRYPOINT=claude-vscode`; the task lines, `init`
/// and `result` of the capture, in order, with the fields the adapter reads).
const REPORTED_SUBAGENT: &str = r#"{"type":"system","subtype":"init"}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a7b736ac9a06b8816","task_type":"local_agent"}]}
{"type":"system","subtype":"task_started","task_id":"a7b736ac9a06b8816","description":"Run ping command once","is_backgrounded":true,"task_type":"local_agent"}
{"type":"result","subtype":"success","num_turns":2}
{"type":"system","subtype":"task_started","task_id":"baydyre83","owned_by_subagent":true,"is_backgrounded":false,"parent_task_id":"a7b736ac9a06b8816","task_type":"local_bash"}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a7b736ac9a06b8816","task_type":"local_agent"},{"task_id":"baydyre83","task_type":"local_bash","description":"Ping localhost 200 times quietly","parent_task_id":"a7b736ac9a06b8816"}]}
{"type":"system","subtype":"task_updated","task_id":"baydyre83","patch":{"is_backgrounded":true}}
{"type":"system","subtype":"task_updated","task_id":"a7b736ac9a06b8816","patch":{"status":"completed"}}
{"type":"system","subtype":"task_notification","task_id":"a7b736ac9a06b8816","status":"completed","handback":"send","summary":"This agent's report was delivered to you as a message from \"a7b736ac9a06b8816\" (its SubagentHandback call). Read it there; it is not repeated here.\n"}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"baydyre83","task_type":"local_bash","description":"Ping localhost 200 times quietly","parent_task_id":"a7b736ac9a06b8816"}]}
{"type":"system","subtype":"init"}
{"type":"result","subtype":"success","num_turns":1}
{"type":"system","subtype":"init"}
{"type":"result","subtype":"success","num_turns":1}
{"type":"system","subtype":"task_updated","task_id":"baydyre83","patch":{"status":"completed"}}
{"type":"system","subtype":"task_notification","task_id":"baydyre83","status":"completed","summary":"Background command \"Ping localhost 200 times quietly\" completed (exit code 0)"}
{"type":"system","subtype":"background_tasks_changed","tasks":[]}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a7b736ac9a06b8816","task_type":"local_agent"}]}
{"type":"system","subtype":"task_started","task_id":"a7b736ac9a06b8816","description":"Run ping command once","is_backgrounded":true,"task_type":"local_agent"}
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

/// [`REPORTED_SUBAGENT`]: shown with no title from the CLI's list until its `task_started`,
/// gone once it reported with only its `ping` left, which is then the session's background
/// command with a row of its own until it leaves the list, then shown again the same way when
/// the CLI lists and starts it again, and gone at the end.
#[test]
fn a_subagent_that_reports_twice_is_listed_twice() {
    let id = "a7b736ac9a06b8816";
    let untitled = vec![vec![live_row(id, "", None)]];
    let titled = vec![vec![live_row(id, "Run ping command once", None)]];
    let mut want: Vec<Vec<Vec<LiveRow>>> = vec![vec![]; 24];
    want[1] = untitled.clone();
    want[2] = titled.clone();
    want[10] = vec![vec![live_row(
        "baydyre83",
        "Ping localhost 200 times quietly",
        None,
    )]];
    want[16] = vec![vec![]];
    want[17] = untitled;
    want[18] = titled;
    want[22] = vec![vec![]];
    assert_eq!(live_lists(REPORTED_SUBAGENT.lines()), want);
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

/// A background subagent starts a background wait loop of its own that never ends and stops
/// with its report (2.1.329: the subagent's `task_started`, its Bash call's with
/// `owned_by_subagent` and `parent_task_id`, the CLI's list of both, `task_updated` `completed`,
/// `task_notification` `completed` with the report as `summary` and no `handback`, the list with
/// the loop alone, then the turn the CLI starts by itself).
const LEFT_RUNNING: [&str; 9] = [
    r#"{"type":"system","subtype":"task_started","task_id":"a6996d99c74dd1835","tool_use_id":"toolu_01Agent","description":"Heightmap: 25 frames, summed match","subagent_type":"general-purpose","is_backgrounded":true,"task_type":"local_agent"}"#,
    r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a6996d99c74dd1835","task_type":"local_agent","description":"Heightmap: 25 frames, summed match"}]}"#,
    r#"{"type":"system","subtype":"task_started","task_id":"bq4wq8f2x","tool_use_id":"toolu_01Wait","description":"Wait for the suite","owned_by_subagent":true,"is_backgrounded":true,"parent_task_id":"a6996d99c74dd1835","task_type":"local_bash"}"#,
    r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a6996d99c74dd1835","task_type":"local_agent","description":"Heightmap: 25 frames, summed match"},{"task_id":"bq4wq8f2x","task_type":"local_bash","description":"Wait for the suite","owner_agent_id":"a6996d99c74dd1835","parent_task_id":"a6996d99c74dd1835"}]}"#,
    r#"{"type":"system","subtype":"task_updated","task_id":"a6996d99c74dd1835","patch":{"status":"completed"}}"#,
    r#"{"type":"system","subtype":"task_notification","task_id":"a6996d99c74dd1835","tool_use_id":"toolu_01Agent","status":"completed","summary":"Всё сделано: 457 passed","usage":{"total_tokens":257800,"tool_uses":152,"duration_ms":11139000}}"#,
    r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"bq4wq8f2x","task_type":"local_bash","description":"Wait for the suite","owner_agent_id":"a6996d99c74dd1835","parent_task_id":"a6996d99c74dd1835"}]}"#,
    r#"{"type":"system","subtype":"init"}"#,
    r#"{"type":"result","subtype":"success","num_turns":1}"#,
];

/// [`LEFT_RUNNING`]: the report is the subagent's end at once, a line with it at the
/// notification; the subagent stops counting at the next line of another kind, where its row
/// goes and the loop it left gets a row of its own, a stoppable command.
#[test]
fn a_subagent_that_reports_with_its_command_running_ends_at_once() {
    let got: Vec<(usize, String)> = system_lines(LEFT_RUNNING.into_iter())
        .into_iter()
        .map(|(i, _, t)| (i, t))
        .collect();
    assert_eq!(got, [(5, "Всё сделано: 457 passed".to_string())]);
    let subagents: Vec<(usize, String)> = task_counts(LEFT_RUNNING.into_iter())
        .into_iter()
        .enumerate()
        .flat_map(|(i, e)| e.into_iter().map(move |e| (i, e)))
        .filter(|(_, e)| e.starts_with("subagents"))
        .collect();
    assert_eq!(
        subagents,
        [
            (0, "subagents:1".to_string()),
            (7, "subagents:0".to_string())
        ]
    );
    let mut core = Core::new(Uuid::now_v7(), false);
    let mut last = None;
    for l in LEFT_RUNNING {
        for a in core.on_line(l) {
            if let Action::Emit(AdapterEvent::LiveSubagents(list)) = a {
                last = Some(list);
            }
        }
    }
    let rows: Vec<_> = last
        .unwrap()
        .into_iter()
        .map(|r| (r.task_id, r.description, r.kind, r.stoppable))
        .collect();
    assert_eq!(
        rows,
        [(
            "bq4wq8f2x".to_string(),
            "Wait for the suite".to_string(),
            Some(LiveKind::Command),
            true
        )]
    );
}

/// [`LEFT_RUNNING`] up to the report, then the loop ends, the CLI lists and starts the subagent
/// again and it ends once more: a text that differs from the one shown gives a second line (the
/// first was interim), the same text none.
#[test]
fn a_later_end_of_a_subagent_gives_a_line_only_with_a_new_text() {
    let again = |summary: &str| {
        let mut lines: Vec<String> = LEFT_RUNNING[..7].iter().map(|l| l.to_string()).collect();
        lines.extend([
            r#"{"type":"system","subtype":"task_notification","task_id":"bq4wq8f2x","status":"completed","summary":"Background command \"Wait for the suite\" completed (exit code 0)"}"#.to_string(),
            r#"{"type":"system","subtype":"background_tasks_changed","tasks":[]}"#.to_string(),
            r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a6996d99c74dd1835","task_type":"local_agent"}]}"#.to_string(),
            LEFT_RUNNING[0].to_string(),
            LEFT_RUNNING[4].to_string(),
            LEFT_RUNNING[5].replace("Всё сделано: 457 passed", summary),
            r#"{"type":"system","subtype":"background_tasks_changed","tasks":[]}"#.to_string(),
            LEFT_RUNNING[7].to_string(),
        ]);
        system_lines(lines.iter().map(String::as_str))
            .into_iter()
            .map(|(i, _, t)| (i, t))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        again("Всё сделано: 507 subtests passed"),
        [
            (5, "Всё сделано: 457 passed".to_string()),
            (12, "Всё сделано: 507 subtests passed".to_string())
        ]
    );
    assert_eq!(
        again("Всё сделано: 457 passed"),
        [(5, "Всё сделано: 457 passed".to_string())]
    );
}

/// One [`AdapterEvent::SubagentLeftovers`]: the subagent's task, the seconds since its end and
/// its commands.
type Told = (String, u64, Vec<LeftoverCommand>);

/// The [`AdapterEvent::SubagentLeftovers`] of `actions`.
fn leftovers_of(actions: Vec<Action>) -> Vec<Told> {
    actions
        .into_iter()
        .filter_map(|a| match a {
            Action::Emit(AdapterEvent::SubagentLeftovers {
                subagent,
                ended_s,
                commands,
            }) => Some((subagent, ended_s, commands)),
            _ => None,
        })
        .collect()
}

/// [`LEFT_RUNNING`]: 30 min after the report, with the loop still listed, the main agent is
/// told once, with how long ago the subagent reported, the loop and how long it has run; not
/// before, not again. Nothing when the
/// loop left the list meanwhile, or the CLI started the subagent again.
#[test]
fn a_command_a_subagent_left_running_is_told_once_after_30_min() {
    let fed = |extra: &[&str]| {
        let mut core = Core::new(Uuid::now_v7(), false);
        for l in LEFT_RUNNING.iter().chain(extra) {
            core.on_line(l);
        }
        (core, std::time::Instant::now())
    };
    let half_hour = Duration::from_secs(30 * 60);
    let (mut core, at) = fed(&[]);
    assert_eq!(leftovers_of(core.leftovers(at)), []);
    assert_eq!(
        leftovers_of(core.leftovers(at + half_hour - Duration::from_secs(5))),
        []
    );
    let told = leftovers_of(core.leftovers(at + half_hour));
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0].0, "Heightmap: 25 frames, summed match");
    assert!((1800..1810).contains(&told[0].1), "{}", told[0].1);
    assert_eq!(told[0].2.len(), 1);
    let c = &told[0].2[0];
    assert_eq!(
        (c.task_id.as_str(), c.description.as_str()),
        ("bq4wq8f2x", "Wait for the suite")
    );
    assert!((1800..1810).contains(&c.running_s), "{}", c.running_s);
    assert_eq!(leftovers_of(core.leftovers(at + 2 * half_hour)), []);

    let (mut core, at) =
        fed(&[r#"{"type":"system","subtype":"background_tasks_changed","tasks":[]}"#]);
    assert_eq!(leftovers_of(core.leftovers(at + half_hour)), []);
    let (mut core, at) = fed(&[LEFT_RUNNING[0]]);
    assert_eq!(leftovers_of(core.leftovers(at + half_hour)), []);
}

/// The tasks of the [`AdapterEvent::LongRuns`] of `actions`, one list per event.
fn long_runs_of(actions: Vec<Action>) -> Vec<Vec<LongRun>> {
    actions
        .into_iter()
        .filter_map(|a| match a {
            Action::Emit(AdapterEvent::LongRuns { tasks }) => Some(tasks),
            _ => None,
        })
        .collect()
}

/// The lines of a background command: the CLI's list, then its `task_started`.
const BACKGROUND_COMMAND: [&str; 2] = [
    r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"b2st7oc3z","task_type":"local_bash","description":"Run the full pass over 1000 boards on cirunner"}]}"#,
    r#"{"type":"system","subtype":"task_started","task_id":"b2st7oc3z","tool_use_id":"toolu_01Run","description":"Run the full pass over 1000 boards on cirunner","is_backgrounded":true,"task_type":"local_bash"}"#,
];

/// `core` after it has read `lines`, and the instant after them (the rows count from before it).
fn fed_into(mut core: Core, lines: &[&str]) -> (Core, std::time::Instant) {
    for l in lines {
        core.on_line(l);
    }
    (core, std::time::Instant::now())
}

/// A core of the default period that has read `lines`.
fn fed(lines: &[&str]) -> (Core, std::time::Instant) {
    fed_into(Core::new(Uuid::now_v7(), false), lines)
}

/// A background command that runs on is told at 55 min and again at each 55 min after, with its
/// task, description and run time; not a second before, and not twice for one mark.
#[test]
fn a_background_command_is_told_every_55_min() {
    let (mut core, at) = fed(&BACKGROUND_COMMAND);
    let every = Duration::from_secs(55 * 60);
    let short = Duration::from_secs(5);
    assert!(long_runs_of(core.long_runs(at)).is_empty());
    assert!(long_runs_of(core.long_runs(at + every - short)).is_empty());
    let told = long_runs_of(core.long_runs(at + every));
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0].len(), 1);
    let t = &told[0][0];
    assert_eq!(
        (t.task_id.as_str(), t.kind, t.description.as_str()),
        (
            "b2st7oc3z",
            LiveKind::Command,
            "Run the full pass over 1000 boards on cirunner"
        )
    );
    assert!((3300..3310).contains(&t.running_s), "{}", t.running_s);
    assert!(long_runs_of(core.long_runs(at + every + short)).is_empty());
    assert!(long_runs_of(core.long_runs(at + 2 * every - short)).is_empty());
    let told = long_runs_of(core.long_runs(at + 2 * every));
    assert_eq!(told.len(), 1, "{told:?}");
    assert!((6600..6610).contains(&told[0][0].running_s));
}

/// A background subagent is told as a task of kind agent.
#[test]
fn a_background_subagent_is_told_every_55_min() {
    let (mut core, at) = fed(&LEFT_RUNNING[..2]);
    assert!(long_runs_of(core.long_runs(at)).is_empty());
    let told = long_runs_of(core.long_runs(at + Duration::from_secs(55 * 60)));
    assert_eq!(told.len(), 1, "{told:?}");
    let t = &told[0][0];
    assert_eq!(
        (t.task_id.as_str(), t.kind, t.description.as_str()),
        (
            "a6996d99c74dd1835",
            LiveKind::Agent,
            "Heightmap: 25 frames, summed match"
        )
    );
}

/// A Monitor of the agent and a command or subagent held on the foreground of its turn are
/// listed, but never told of: a note would wait for the end of the turn they hold.
#[test]
fn a_monitor_and_a_foreground_call_are_not_told() {
    let monitor = json!({"type": "assistant", "message": {"model": "claude-haiku-5-5",
        "role": "assistant", "content": [{"type": "tool_use", "id": "toolu_01Watch",
            "name": "Monitor", "input": {"command": "tail -f run.log",
                "description": "Watch the run's log"}}]},
        "parent_tool_use_id": null, "session_id": CLI_SESSION, "uuid": Uuid::new_v4()})
    .to_string();
    let lines = [
        monitor.as_str(),
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"bwatch1","task_type":"local_bash","description":"Watch the run's log"},{"task_id":"bfis28nrx","task_type":"local_bash","description":"Sleep for 15 seconds"},{"task_id":"ac5672f5fc80df8ec","task_type":"local_agent","description":"Run ping and reply with 4"}]}"#,
        r#"{"type":"system","subtype":"task_started","task_id":"bwatch1","tool_use_id":"toolu_01Watch","description":"Watch the run's log","is_backgrounded":true,"task_type":"local_bash"}"#,
        r#"{"type":"system","subtype":"task_started","task_id":"bfis28nrx","tool_use_id":"toolu_01FoCFY4DmUbVxXAembc44PX","description":"Sleep for 15 seconds","is_backgrounded":false,"task_type":"local_bash"}"#,
        r#"{"type":"system","subtype":"task_started","task_id":"ac5672f5fc80df8ec","tool_use_id":"toolu_01TYRCeySHmvXd7Md3Y1rgb8","description":"Run ping and reply with 4","subagent_type":"general-purpose","is_backgrounded":false,"spawn_depth":1,"task_type":"local_agent"}"#,
    ];
    let mut core = Core::new(Uuid::now_v7(), false);
    let mut rows = Vec::new();
    for l in lines {
        for a in core.on_line(l) {
            if let Action::Emit(AdapterEvent::LiveSubagents(list)) = a {
                rows = list;
            }
        }
    }
    let mut kinds: Vec<_> = rows.iter().map(|r| (r.task_id.as_str(), r.kind)).collect();
    kinds.sort_by_key(|k| k.0);
    // The three are rows, so that nothing but the rule keeps them from being told.
    assert_eq!(
        kinds,
        [
            ("ac5672f5fc80df8ec", Some(LiveKind::Agent)),
            ("bfis28nrx", Some(LiveKind::Command)),
            ("bwatch1", Some(LiveKind::Monitor)),
        ]
    );
    let at = std::time::Instant::now();
    for hours in [1, 2, 5] {
        let late = at + Duration::from_secs(hours * 3600);
        assert!(long_runs_of(core.long_runs(late)).is_empty(), "{hours} h");
    }
}

/// Two tasks that reach their mark on one tick are told in one event.
#[test]
fn tasks_due_together_are_told_in_one_event() {
    let lines: Vec<&str> = LEFT_RUNNING[..2]
        .iter()
        .chain(&BACKGROUND_COMMAND)
        .copied()
        .collect();
    let (mut core, at) = fed(&lines);
    assert!(long_runs_of(core.long_runs(at)).is_empty());
    let told = long_runs_of(core.long_runs(at + Duration::from_secs(55 * 60)));
    assert_eq!(told.len(), 1, "{told:?}");
    let mut ids: Vec<_> = told[0]
        .iter()
        .map(|t| (t.task_id.as_str(), t.kind))
        .collect();
    ids.sort_by_key(|k| k.0);
    assert_eq!(
        ids,
        [
            ("a6996d99c74dd1835", LiveKind::Agent),
            ("b2st7oc3z", LiveKind::Command)
        ]
    );
}

/// [`LEFT_RUNNING`]: the loop the subagent left becomes a row when the subagent reports, and
/// the first tick that sees it is past the 55 min it has run: the marks it has passed are not
/// told, the next one is.
#[test]
fn a_command_first_seen_late_is_told_at_its_next_mark() {
    let (mut core, at) = fed(&LEFT_RUNNING);
    let every = Duration::from_secs(55 * 60);
    assert!(long_runs_of(core.long_runs(at + every + Duration::from_secs(60))).is_empty());
    assert!(long_runs_of(core.long_runs(at + 2 * every - Duration::from_secs(5))).is_empty());
    let told = long_runs_of(core.long_runs(at + 2 * every));
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0][0].task_id, "bq4wq8f2x");
}

/// `long_run_note` sets the period: a task is told at each multiple of it, and with `None`
/// never.
#[test]
fn the_period_is_the_workers_setting() {
    let ten_min = Duration::from_secs(600);
    let core = Core::new(Uuid::now_v7(), false).with_long_run_note(Some(ten_min));
    let (mut core, at) = fed_into(core, &BACKGROUND_COMMAND);
    assert!(long_runs_of(core.long_runs(at)).is_empty());
    assert!(long_runs_of(core.long_runs(at + ten_min - Duration::from_secs(5))).is_empty());
    let told = long_runs_of(core.long_runs(at + ten_min));
    assert_eq!(told.len(), 1, "{told:?}");
    assert!((600..610).contains(&told[0][0].running_s));
    assert_eq!(long_runs_of(core.long_runs(at + 2 * ten_min)).len(), 1);

    let core = Core::new(Uuid::now_v7(), false).with_long_run_note(None);
    let (mut core, at) = fed_into(core, &BACKGROUND_COMMAND);
    for hours in [0, 1, 5, 24] {
        let late = at + Duration::from_secs(hours * 3600);
        assert!(long_runs_of(core.long_runs(late)).is_empty(), "{hours} h");
    }
}

/// A background subagent a running subagent started is not told of while its parent runs (the
/// parent waits for it); once the parent reports, it is the main agent's to stop, and it is told
/// at its next mark, not of the one it passed.
#[test]
fn a_subagent_of_a_running_subagent_is_not_told() {
    let parent = "a6996d99c74dd1835";
    let nested = r#"{"type":"system","subtype":"task_started","task_id":"anest001","tool_use_id":"toolu_01Nest","description":"Nested check","subagent_type":"general-purpose","is_backgrounded":true,"spawn_depth":2,"parent_task_id":"a6996d99c74dd1835","task_type":"local_agent"}"#;
    let both = r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a6996d99c74dd1835","task_type":"local_agent","description":"Heightmap: 25 frames, summed match"},{"task_id":"anest001","task_type":"local_agent","description":"Nested check","parent_task_id":"a6996d99c74dd1835"}]}"#;
    let (mut core, at) = fed(&[LEFT_RUNNING[0], nested, both]);
    let every = Duration::from_secs(55 * 60);
    assert!(long_runs_of(core.long_runs(at)).is_empty());
    let told = long_runs_of(core.long_runs(at + every));
    let ids: Vec<_> = told.iter().flatten().map(|t| t.task_id.as_str()).collect();
    assert_eq!(ids, [parent]);
    // The parent reports; the nested one runs on, alone in the list; the turn the CLI then
    // starts by itself is the next line of another kind, which drops the parent's row.
    for l in [
        LEFT_RUNNING[4],
        LEFT_RUNNING[5],
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"anest001","task_type":"local_agent","description":"Nested check","parent_task_id":"a6996d99c74dd1835"}]}"#,
        LEFT_RUNNING[7],
        LEFT_RUNNING[8],
    ] {
        core.on_line(l);
    }
    let after = at + every + Duration::from_secs(60);
    assert!(long_runs_of(core.long_runs(after)).is_empty());
    let told = long_runs_of(core.long_runs(at + 2 * every));
    let ids: Vec<_> = told.iter().flatten().map(|t| t.task_id.as_str()).collect();
    assert_eq!(ids, ["anest001"]);
}

/// A task that ended is not told of, however long it ran.
#[test]
fn an_ended_task_is_not_told() {
    let (mut core, at) = fed(&BACKGROUND_COMMAND);
    assert!(long_runs_of(core.long_runs(at)).is_empty());
    core.on_line(r#"{"type":"system","subtype":"background_tasks_changed","tasks":[]}"#);
    assert!(long_runs_of(core.long_runs(at + Duration::from_secs(55 * 60))).is_empty());
}

/// SPEC 0.148: a session given the browser runs with `--chrome` and without the tools that
/// upload files; every other one with `--no-chrome`.
#[test]
fn the_browser_only_where_it_is_given() {
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let f = ClaudeCodeAdapters::new(options(config.path()), Arc::new(ScriptedClaude::new("")));
    let a = ClaudeCodeAdapter {
        session_id: Uuid::nil(),
        opts: f.opts.clone(),
        spawner: f.spawner.clone(),
        events: mpsc::channel(1).0,
        proc: None,
    };
    let mut s = spec(Uuid::now_v7(), work.path(), Uuid::new_v4());
    let args = |s: &StartSpec| a.command(s, &Uuid::nil(), Launch::New, None, None).args;
    let off = args(&s);
    assert!(off.contains(&"--no-chrome".to_string()) && !off.contains(&"--chrome".to_string()));
    s.browser = true;
    let on = args(&s);
    assert!(on.contains(&"--chrome".to_string()) && !on.contains(&"--no-chrome".to_string()));
    let pos = on.iter().position(|x| x == "--disallowedTools").unwrap();
    for t in super::BROWSER_DISALLOWED {
        assert!(on[pos..].iter().any(|x| x == t), "{t} in {on:?}");
    }
}

/// After `/clear` the process writes the new transcript (SPEC §8.1, §9.3): its records and the
/// inputs written from then on are registered under the new id, which is live, and the old one
/// is not; the title is read from the new transcript.
#[tokio::test]
async fn after_clear_the_process_writes_the_new_transcript() {
    let (old, new) = (Uuid::new_v4(), Uuid::new_v4());
    let answer = Uuid::new_v4();
    let init = |session: Uuid| {
        json!({"type": "system", "subtype": "init", "session_id": session,
            "model": "claude-haiku-5-5", "permissionMode": "default"})
        .to_string()
    };
    let done = |session: Uuid| {
        json!({"type": "result", "subtype": "success", "is_error": false, "num_turns": 1,
            "total_cost_usd": 0.001, "session_id": session,
            "usage": {"input_tokens": 4, "output_tokens": 2}})
        .to_string()
    };
    let stream = [
        init(old),
        reply("ONE"),
        done(old),
        json!({"type": "conversation_reset", "new_conversation_id": Uuid::new_v4(),
            "trigger": "clear", "session_id": old})
        .to_string(),
        init(new),
        json!({"type": "assistant", "message": {"model": "claude-haiku-5-5", "role": "assistant",
            "content": [{"type": "text", "text": "TWO"}]},
            "parent_tool_use_id": null, "session_id": new, "uuid": answer})
        .to_string(),
        done(new),
    ]
    .join("\n");
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let workdir = work.path().canonicalize().unwrap();
    for (session, title) in [(old, "Before"), (new, "After")] {
        let t = super::paths::transcript_path(config.path(), &workdir, &session);
        std::fs::create_dir_all(t.parent().unwrap()).unwrap();
        std::fs::write(
            &t,
            format!(
                "{{\"type\":\"ai-title\",\"aiTitle\":\"{title}\",\"sessionId\":\"{session}\"}}\n"
            ),
        )
        .unwrap();
    }
    let opts = options(config.path());
    let own = opts.own_records.clone();
    let factory = ClaudeCodeAdapters::new(opts, Arc::new(ScriptedClaude::new(&stream)));
    let s = spec(Uuid::now_v7(), work.path(), old);
    let (tx, mut events) = mpsc::channel(1024);
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    let input = |text: &str| SessionInput {
        session_id: s.session_id,
        event_id: Uuid::now_v7(),
        text: text.into(),
        artifact_ids: vec![],
        peer_prefix: vec![],
        peer: None,
        permission_cap: None,
        schedule_id: None,
        human: false,
    };
    a.input(&input("x")).await.unwrap();
    let mut titles = Vec::new();
    while titles.len() < 2 {
        let ev = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        if let AdapterEvent::Title(t) = ev {
            titles.push(t);
        }
    }
    assert_eq!(titles, ["Before", "After"]);
    assert!(own.is_live(new) && !own.is_live(old));
    assert!(own.is_own(new, answer) && !own.is_own(old, answer));
    a.input(&input("next")).await.unwrap();
    assert!(own.claim(new, Uuid::new_v4(), Some("next")));
    a.kill().await.unwrap();
    assert!(!own.is_live(new));
}

/// From `conversation_reset` until the `init` that names the new transcript the folder of the
/// session's transcript is marked in the registry, as the CLI begins the new one before it
/// names it (SPEC §9.3); the end of the process unmarks it too. A question the agent asks
/// holds the scripted CLI at each point.
#[tokio::test]
async fn a_cleared_folder_is_marked_until_init() {
    let (old, new) = (Uuid::new_v4(), Uuid::new_v4());
    let init = |session: Uuid| {
        json!({"type": "system", "subtype": "init", "session_id": session,
            "model": "claude-haiku-5-5", "permissionMode": "default"})
        .to_string()
    };
    let reset = |session: Uuid| {
        json!({"type": "conversation_reset", "trigger": "clear", "session_id": session}).to_string()
    };
    let ask = |id: &str| {
        json!({"type": "control_request", "request_id": id, "request": {
            "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "input": {"questions": [
                {"question": "Go on?", "header": "Go", "multiSelect": false,
                    "options": [{"label": "Yes"}]}]}}})
        .to_string()
    };
    let stream = [
        init(old),
        reset(old),
        ask("q1"),
        init(new),
        ask("q2"),
        reset(new),
        ask("q3"),
    ]
    .join("\n");
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let folder =
        super::paths::transcript_path(config.path(), &work.path().canonicalize().unwrap(), &old)
            .parent()
            .unwrap()
            .to_path_buf();
    let opts = options(config.path());
    let own = opts.own_records.clone();
    let factory = ClaudeCodeAdapters::new(opts, Arc::new(ScriptedClaude::new(&stream)));
    let s = spec(Uuid::now_v7(), work.path(), old);
    let (tx, mut events) = mpsc::channel(1024);
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    let question = |ev: &AdapterEvent| match ev {
        AdapterEvent::Event(e) => match &e.kind {
            EventKind::ClarifyRequest { request_id, .. } => Some(request_id.clone()),
            _ => None,
        },
        _ => None,
    };
    assert!(!own.is_clearing(&folder));
    a.input(&SessionInput {
        session_id: s.session_id,
        event_id: Uuid::now_v7(),
        text: "/clear".into(),
        artifact_ids: vec![],
        peer_prefix: vec![],
        peer: None,
        permission_cap: None,
        schedule_id: None,
        human: false,
    })
    .await
    .unwrap();
    let q1 = first(&mut events, question).await;
    assert!(own.is_clearing(&folder), "after conversation_reset");
    a.answer_clarify(&q1, &BTreeMap::new(), None).await.unwrap();
    let q2 = first(&mut events, question).await;
    assert!(!own.is_clearing(&folder), "the init named it");
    assert!(own.is_live(new));
    a.answer_clarify(&q2, &BTreeMap::new(), None).await.unwrap();
    first(&mut events, question).await;
    assert!(own.is_clearing(&folder), "cleared again");
    a.kill().await.unwrap();
    assert!(!own.is_clearing(&folder), "the process is gone");
}

/// A `/clear` whose turn ends with no `init` naming a new transcript unmarks the folder at that
/// turn's `result` (SPEC §9.2): the mark does not hold the mirror back until the process ends.
/// A question holds the scripted CLI after `conversation_reset`.
#[tokio::test]
async fn a_cleared_folder_is_unmarked_when_the_turn_of_clear_ends() {
    let old = Uuid::new_v4();
    let stream = [
        json!({"type": "system", "subtype": "init", "session_id": old,
            "model": "claude-haiku-5-5", "permissionMode": "default"})
        .to_string(),
        json!({"type": "conversation_reset", "trigger": "clear", "session_id": old}).to_string(),
        json!({"type": "control_request", "request_id": "q1", "request": {
            "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "input": {"questions": [
                {"question": "Go on?", "header": "Go", "multiSelect": false,
                    "options": [{"label": "Yes"}]}]}}})
        .to_string(),
        json!({"type": "result", "subtype": "success", "is_error": false, "num_turns": 1,
            "total_cost_usd": 0.0, "session_id": old, "local_command": "clear",
            "usage": {"input_tokens": 0, "output_tokens": 0}})
        .to_string(),
    ]
    .join("\n");
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let folder =
        super::paths::transcript_path(config.path(), &work.path().canonicalize().unwrap(), &old)
            .parent()
            .unwrap()
            .to_path_buf();
    let opts = options(config.path());
    let own = opts.own_records.clone();
    let factory = ClaudeCodeAdapters::new(opts, Arc::new(ScriptedClaude::new(&stream)));
    let s = spec(Uuid::now_v7(), work.path(), old);
    let (tx, mut events) = mpsc::channel(1024);
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    a.input(&user_input(s.session_id, "/clear")).await.unwrap();
    let q1 = first(&mut events, |ev| match ev {
        AdapterEvent::Event(e) => match &e.kind {
            EventKind::ClarifyRequest { request_id, .. } => Some(request_id.clone()),
            _ => None,
        },
        _ => None,
    })
    .await;
    assert!(own.is_clearing(&folder), "after conversation_reset");
    a.answer_clarify(&q1, &BTreeMap::new(), None).await.unwrap();
    first(&mut events, |ev| {
        matches!(ev, AdapterEvent::TurnEnded { .. }).then_some(())
    })
    .await;
    assert!(!own.is_clearing(&folder), "the turn of /clear ended");
    a.kill().await.unwrap();
}

/// What the process wrote to its transcript right before `/clear` and the mirror reads only
/// after the switch stays the process's (SPEC §9.2): the output of `/cost` there is no
/// `mirror.event`, which the hub, with the session on the new transcript, would import as a
/// session of someone else's. A record another writer adds there after the switch is a
/// `mirror.event`. A question holds the scripted CLI after the switch.
#[tokio::test]
async fn a_command_output_before_clear_is_the_processs_after_the_switch() {
    let (old, new) = (Uuid::new_v4(), Uuid::new_v4());
    let init = |session: Uuid| {
        json!({"type": "system", "subtype": "init", "session_id": session,
            "model": "claude-haiku-5-5", "permissionMode": "default"})
        .to_string()
    };
    let stream = [
        init(old),
        json!({"type": "conversation_reset", "trigger": "clear", "session_id": old}).to_string(),
        init(new),
        json!({"type": "control_request", "request_id": "q1", "request": {
            "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "input": {"questions": [
                {"question": "Go on?", "header": "Go", "multiSelect": false,
                    "options": [{"label": "Yes"}]}]}}})
        .to_string(),
    ]
    .join("\n");
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let workdir = work.path().canonicalize().unwrap();
    let transcript = super::paths::transcript_path(config.path(), &workdir, &old);
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    // The shape of 2.1.312.
    let first_uuid = Uuid::new_v4();
    let output = json!({"parentUuid": null, "isSidechain": false, "uuid": first_uuid,
        "timestamp": "2026-11-16T10:00:00.000Z", "cwd": workdir.display().to_string(),
        "sessionId": old, "type": "system", "subtype": "local_command",
        "content": "<local-command-stdout>Total cost: $0.0123</local-command-stdout>",
        "level": "info", "isMeta": false});
    std::fs::write(&transcript, format!("{output}\n")).unwrap();
    let opts = options(config.path());
    let own = opts.own_records.clone();
    let factory = ClaudeCodeAdapters::new(opts, Arc::new(ScriptedClaude::new(&stream)));
    let s = spec(Uuid::now_v7(), work.path(), old);
    let (tx, mut events) = mpsc::channel(1024);
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    a.input(&user_input(s.session_id, "/clear")).await.unwrap();
    first(&mut events, |ev| match ev {
        AdapterEvent::Event(e) => matches!(e.kind, EventKind::ClarifyRequest { .. }).then_some(()),
        _ => None,
    })
    .await;
    assert!(own.is_live(new) && !own.is_live(old), "switched");
    let mut mirror = crate::mirror::Mirror::new(
        config.path().join("projects"),
        Arc::new(crate::mirror::store::Store::memory()),
        own.clone(),
    );
    mirror.set(workdir.clone(), true, vec![]);
    let out = mirror.poll(std::time::Instant::now(), std::time::SystemTime::now());
    assert!(
        !out.iter()
            .any(|o| matches!(o, crate::mirror::Output::Event(_))),
        "{out:?}"
    );
    // Another writer of the old transcript after the switch (`--resume` of the old id,
    // `w2-resume-old`): what it adds is not the process's.
    let later = json!({"parentUuid": first_uuid, "isSidechain": false, "uuid": Uuid::new_v4(),
        "timestamp": "2026-11-16T10:01:00.000Z", "cwd": workdir.display().to_string(),
        "sessionId": old, "type": "system", "subtype": "local_command",
        "content": "<local-command-stdout>Total cost: $0.0456</local-command-stdout>",
        "level": "info", "isMeta": false});
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    std::io::Write::write_all(&mut f, format!("{later}\n").as_bytes()).unwrap();
    let out = mirror.poll(std::time::Instant::now(), std::time::SystemTime::now());
    assert!(
        out.iter()
            .any(|o| matches!(o, crate::mirror::Output::Event(_))),
        "{out:?}"
    );
    a.kill().await.unwrap();
}

/// What `want` makes of the first adapter event it takes, within 5 s each.
async fn first<T>(
    events: &mut mpsc::Receiver<AdapterEvent>,
    want: impl Fn(&AdapterEvent) -> Option<T>,
) -> T {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("no adapter event in 5 s")
            .expect("event channel closed");
        if let Some(t) = want(&ev) {
            return t;
        }
    }
}

/// The CLI's own title, which it writes into the transcript during the first turn and does not
/// stream (2.1.312), is reported while the turn still runs (SPEC §8.1): a question holds the
/// scripted turn open.
#[tokio::test]
async fn a_title_written_during_a_turn_is_reported_before_its_end() {
    let claude_session = Uuid::new_v4();
    let stream = [
        json!({"type": "system", "subtype": "init", "session_id": CLI_SESSION,
            "model": "claude-haiku-5-5", "permissionMode": "default"})
        .to_string(),
        json!({"type": "control_request", "request_id": "q1", "request": {
            "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "input": {"questions": [
                {"question": "Go on?", "header": "Go", "multiSelect": false,
                    "options": [{"label": "Yes"}]}]}}})
        .to_string(),
    ]
    .join("\n");
    let mut h = Harness::start(&stream, claude_session).await;
    let transcript = super::paths::transcript_path(
        h._dirs.1.path(),
        &h._dirs.0.path().canonicalize().unwrap(),
        &claude_session,
    );
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    h.adapter
        .input(&SessionInput {
            session_id: h.session_id,
            event_id: Uuid::now_v7(),
            text: "Fix the build".into(),
            artifact_ids: vec![],
            peer_prefix: vec![],
            peer: None,
            permission_cap: None,
            schedule_id: None,
            human: false,
        })
        .await
        .unwrap();
    first(&mut h.events, |ev| match ev {
        AdapterEvent::Event(e) => matches!(e.kind, EventKind::ClarifyRequest { .. }).then_some(()),
        _ => None,
    })
    .await;
    std::fs::write(
        &transcript,
        format!("{{\"type\":\"ai-title\",\"aiTitle\":\"Build fix\",\"sessionId\":\"{claude_session}\"}}\n"),
    )
    .unwrap();
    let title = first(&mut h.events, |ev| match ev {
        AdapterEvent::Title(t) => Some(t.clone()),
        AdapterEvent::TurnEnded { .. } => panic!("the turn ended"),
        _ => None,
    })
    .await;
    assert_eq!(title, "Build fix");
}

/// After `/clear` in a process that has sent a title, the title the CLI writes into the new
/// transcript during the new conversation's first model turn is reported while that turn still
/// runs (SPEC §8.1): the title sent before is forgotten at the switch. A question holds
/// the turn.
#[tokio::test]
async fn after_clear_the_new_title_is_reported_during_the_first_turn() {
    let (old, new) = (Uuid::new_v4(), Uuid::new_v4());
    let init = |session: Uuid| {
        json!({"type": "system", "subtype": "init", "session_id": session,
            "model": "claude-haiku-5-5", "permissionMode": "default"})
        .to_string()
    };
    let done = |session: Uuid, local: Option<&str>| {
        let mut v = json!({"type": "result", "subtype": "success", "is_error": false,
            "num_turns": 1, "total_cost_usd": 0.001, "session_id": session,
            "usage": {"input_tokens": 4, "output_tokens": 2}});
        if let Some(l) = local {
            v["local_command"] = json!(l);
        }
        v.to_string()
    };
    let stream = [
        init(old),
        reply("ONE"),
        done(old, None),
        json!({"type": "conversation_reset", "trigger": "clear", "session_id": old}).to_string(),
        init(new),
        done(new, Some("clear")),
        init(new),
        json!({"type": "control_request", "request_id": "q1", "request": {
            "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "input": {"questions": [
                {"question": "Go on?", "header": "Go", "multiSelect": false,
                    "options": [{"label": "Yes"}]}]}}})
        .to_string(),
    ]
    .join("\n");
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let workdir = work.path().canonicalize().unwrap();
    let path = |session: &Uuid| super::paths::transcript_path(config.path(), &workdir, session);
    let title_line = |session: Uuid, title: &str| {
        format!("{{\"type\":\"ai-title\",\"aiTitle\":\"{title}\",\"sessionId\":\"{session}\"}}\n")
    };
    std::fs::create_dir_all(path(&old).parent().unwrap()).unwrap();
    std::fs::write(path(&old), title_line(old, "Before")).unwrap();
    let factory = ClaudeCodeAdapters::new(
        options(config.path()),
        Arc::new(ScriptedClaude::new(&stream)),
    );
    let s = spec(Uuid::now_v7(), work.path(), old);
    let (tx, mut events) = mpsc::channel(1024);
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    a.input(&user_input(s.session_id, "x")).await.unwrap();
    let title = |ev: &AdapterEvent| match ev {
        AdapterEvent::Title(t) => Some(t.clone()),
        _ => None,
    };
    assert_eq!(first(&mut events, title).await, "Before");
    first(&mut events, |ev| match ev {
        AdapterEvent::Event(e) => matches!(e.kind, EventKind::ClarifyRequest { .. }).then_some(()),
        _ => None,
    })
    .await;
    std::fs::write(path(&new), title_line(new, "After")).unwrap();
    let after = first(&mut events, |ev| match ev {
        AdapterEvent::TurnEnded { .. } => panic!("the turn ended"),
        ev => title(ev),
    })
    .await;
    assert_eq!(after, "After");
    a.kill().await.unwrap();
}

/// A tick between `conversation_reset` and the `init` that names the new transcript (~70 ms,
/// `w1-clear`) reads the old transcript and stores its title; the switch forgets it, so the ticks
/// of the new conversation's first turn still read its title (SPEC §8.1).
#[tokio::test]
async fn a_title_read_before_the_switch_does_not_stop_the_new_ones() {
    let (old, new) = (Uuid::new_v4(), Uuid::new_v4());
    let dir = tempfile::tempdir().unwrap();
    let old_path = dir.path().join(format!("{old}.jsonl"));
    std::fs::write(
        &old_path,
        format!("{{\"type\":\"ai-title\",\"aiTitle\":\"Before\",\"sessionId\":\"{old}\"}}\n"),
    )
    .unwrap();
    let (tx, mut events) = mpsc::channel(16);
    let stdin: std::pin::Pin<Box<dyn tokio::io::AsyncWrite + Send>> = Box::pin(tokio::io::sink());
    let io = super::Io {
        session_id: Uuid::now_v7(),
        stdin: tokio::sync::Mutex::new(Some(stdin)),
        events: tx,
        signal: Box::new(|_: Sig| -> std::io::Result<()> { Ok(()) }),
        transcript: std::sync::Mutex::new(Some(old_path)),
        title: std::sync::Mutex::new(None),
        runtime: std::sync::Mutex::new(old),
        own: options(dir.path()).own_records,
        goals: tokio::sync::Mutex::new(super::rate::GoalTail::start(None).await),
    };
    io.report_title(None).await;
    assert!(matches!(events.try_recv(), Ok(AdapterEvent::Title(t)) if t == "Before"));
    io.moved_to(new);
    assert!(
        super::lock(&io.title).is_none(),
        "the old transcript's title would keep the ticks from reading the new one"
    );
}

/// A transcript over 4 MiB is not read for its title on the ticks of a first turn, only when
/// the turn ends (SPEC §8.1): without a title of the CLI's (a first message under 10
/// characters) the reads would go on for the whole turn. A question holds the turn.
#[tokio::test]
async fn a_large_transcript_is_read_for_its_title_only_at_the_turns_end() {
    let claude_session = Uuid::new_v4();
    let stream = [
        json!({"type": "system", "subtype": "init", "session_id": CLI_SESSION,
            "model": "claude-haiku-5-5", "permissionMode": "default"})
        .to_string(),
        json!({"type": "control_request", "request_id": "q1", "request": {
            "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "input": {"questions": [
                {"question": "Go on?", "header": "Go", "multiSelect": false,
                    "options": [{"label": "Yes"}]}]}}})
        .to_string(),
        json!({"type": "result", "subtype": "success", "is_error": false, "num_turns": 1,
            "total_cost_usd": 0.001, "session_id": CLI_SESSION,
            "usage": {"input_tokens": 4, "output_tokens": 2}})
        .to_string(),
    ]
    .join("\n");
    let mut h = Harness::start(&stream, claude_session).await;
    let transcript = super::paths::transcript_path(
        h._dirs.1.path(),
        &h._dirs.0.path().canonicalize().unwrap(),
        &claude_session,
    );
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    h.adapter
        .input(&user_input(h.session_id, "Hi"))
        .await
        .unwrap();
    let q1 = first(&mut h.events, |ev| match ev {
        AdapterEvent::Event(e) => match &e.kind {
            EventKind::ClarifyRequest { request_id, .. } => Some(request_id.clone()),
            _ => None,
        },
        _ => None,
    })
    .await;
    let filler = json!({"type": "user", "message": {"role": "user",
        "content": "x".repeat(4 * 1024 * 1024)}});
    std::fs::write(
        &transcript,
        format!("{filler}\n{{\"type\":\"ai-title\",\"aiTitle\":\"Big\",\"sessionId\":\"{claude_session}\"}}\n"),
    )
    .unwrap();
    // A tick at least: every 2 s.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    while let Ok(ev) = h.events.try_recv() {
        assert!(
            !matches!(ev, AdapterEvent::Title(_)),
            "read on a tick: {ev:?}"
        );
    }
    h.adapter
        .answer_clarify(&q1, &BTreeMap::new(), None)
        .await
        .unwrap();
    let title = first(&mut h.events, |ev| match ev {
        AdapterEvent::Title(t) => Some(t.clone()),
        _ => None,
    })
    .await;
    assert_eq!(title, "Big");
}

/// `system/session_title_changed` (2.1.312: a title a person gave, `/rename`) is the session's
/// title at once, not at the end of the turn.
#[test]
fn a_title_from_the_stream_is_reported_at_once() {
    let mut core = Core::new(Uuid::now_v7(), false);
    let line = |title: &str| {
        json!({"type": "system", "subtype": "session_title_changed", "title": title,
            "uuid": Uuid::new_v4(), "session_id": CLI_SESSION})
        .to_string()
    };
    assert_eq!(
        emitted(core.on_line(&line(" Fix the build "))),
        [AdapterEvent::Title("Fix the build".into())]
    );
    assert!(core.on_line(&line("  ")).is_empty(), "no title");
}

/// A wakeup the main agent armed with `ScheduleWakeup` (lines of the live run
/// `x6-wake-control`, 2.1.312) is reported from its result until the CLI starts it: a
/// `command_lifecycle` `started` with `wakeup_source: loop_wakeup` and its `scheduled_for`.
/// The fire of an older one, a failed call and a subagent's call change nothing; a stopped one
/// (`scheduledFor: 0`) is none.
#[test]
fn a_wakeup_is_reported_until_it_fires() {
    const DUE: i64 = 1794439200000;
    let mut core = Core::new(Uuid::now_v7(), false);
    let call = |id: &str, parent: Option<&str>| {
        json!({"type": "assistant", "message": {"model": "claude-haiku-5-5", "role": "assistant",
            "content": [{"type": "tool_use", "id": id, "name": "ScheduleWakeup",
                "input": {"delaySeconds": 70, "reason": "probe", "prompt": "WAKEUP-FIRED", "noop": false}}]},
            "parent_tool_use_id": parent, "session_id": CLI_SESSION, "uuid": Uuid::new_v4()})
        .to_string()
    };
    let done = |id: &str, at: i64, error: bool, parent: Option<&str>| {
        json!({"type": "user", "message": {"role": "user", "content": [{"tool_use_id": id,
            "type": "tool_result", "is_error": error, "content": "Next wakeup scheduled for 02:20:00 (in 72s)."}]},
            "parent_tool_use_id": parent, "session_id": CLI_SESSION, "uuid": Uuid::new_v4(),
            "tool_use_result": {"scheduledFor": at, "clampedDelaySeconds": 70, "wasClamped": false}})
        .to_string()
    };
    let fired = |at: i64| {
        json!({"type": "command_lifecycle", "command_uuid": Uuid::new_v4(), "state": "started",
            "wakeup_source": "loop_wakeup", "scheduled_for": at,
            "uuid": Uuid::new_v4(), "session_id": CLI_SESSION})
        .to_string()
    };
    let wakeups = |actions: Vec<Action>| -> Vec<Option<i64>> {
        emitted(actions)
            .into_iter()
            .filter_map(|e| match e {
                AdapterEvent::Wakeup(at) => Some(at.map(|t| t.timestamp_millis())),
                _ => None,
            })
            .collect()
    };
    core.on_line(&call("toolu_01TWrwmcrfbR9Z62ePaNnJTw", None));
    assert_eq!(
        wakeups(core.on_line(&done("toolu_01TWrwmcrfbR9Z62ePaNnJTw", DUE, false, None))),
        [Some(DUE)]
    );
    assert!(
        wakeups(core.on_line(&fired(DUE - 60_000))).is_empty(),
        "an older one fired"
    );
    core.on_line(&call("toolu_failed", None));
    assert!(wakeups(core.on_line(&done("toolu_failed", 0, true, None))).is_empty());
    core.on_line(&call("toolu_sub", Some("toolu_agent")));
    assert!(
        wakeups(core.on_line(&done("toolu_sub", DUE + 1, false, Some("toolu_agent")))).is_empty(),
        "a subagent's"
    );
    assert_eq!(wakeups(core.on_line(&fired(DUE))), [None]);
    core.on_line(&call("toolu_again", None));
    assert_eq!(
        wakeups(core.on_line(&done("toolu_again", DUE + 70_000, false, None))),
        [Some(DUE + 70_000)]
    );
    core.on_line(&call("toolu_stop", None));
    assert_eq!(
        wakeups(core.on_line(&done("toolu_stop", 0, false, None))),
        [None],
        "stopped"
    );
}

/// The `user` line of a tool result with `content`, and `tool_use_result` when given.
fn tool_result_line(content: Value, tool_use_result: Option<Value>) -> String {
    let mut v = json!({"type":"user","uuid":Uuid::new_v4(),"parent_tool_use_id":null,
        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":content}]}});
    if let Some(r) = tool_use_result {
        v["tool_use_result"] = r;
    }
    v.to_string()
}

/// SPEC §8.1, 0.180: a result longer than its 4 KB preview goes with its whole text, at most
/// its first 2 MiB, to be uploaded; a short one is an event as before.
#[test]
fn a_long_result_goes_with_its_whole_text() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let long = format!("{}é", "x".repeat(5000));
    let got = emitted(core.on_line(&tool_result_line(json!(long), None)));
    let [AdapterEvent::ToolResult { event, files }] = &got[..] else {
        panic!("{got:?}");
    };
    let EventKind::ToolResult { result_preview, .. } = &event.kind else {
        panic!("{event:?}");
    };
    assert_eq!(result_preview.len(), super::engine::PREVIEW_LIMIT);
    assert_eq!(files.text.as_deref(), Some(long.as_str()));
    assert!(files.images.is_empty());

    let huge = "y".repeat(crate::adapter::FULL_RESULT_LIMIT + 10);
    let got = emitted(core.on_line(&tool_result_line(json!(huge), None)));
    let [AdapterEvent::ToolResult { files, .. }] = &got[..] else {
        panic!("{got:?}");
    };
    assert_eq!(
        files.text.as_deref(),
        Some(&huge[..crate::adapter::FULL_RESULT_LIMIT])
    );

    let got = emitted(core.on_line(&tool_result_line(json!("short"), None)));
    assert!(
        matches!(&got[..], [AdapterEvent::Event(e)] if matches!(e.kind, EventKind::ToolResult { .. })),
        "{got:?}"
    );
}

/// SPEC §8.1, 0.180: an image of a result (a screenshot) goes with it decoded, with its media
/// type; the preview keeps its `[image]`, and one that does not decode is left out.
#[test]
fn a_result_image_goes_with_it() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let content = json!([
        {"type":"text","text":"Screenshot"},
        {"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0K"}},
        {"type":"image","source":{"type":"base64","media_type":"image/png","data":"not base64!"}},
    ]);
    let got = emitted(core.on_line(&tool_result_line(content, None)));
    let [AdapterEvent::ToolResult { event, files }] = &got[..] else {
        panic!("{got:?}");
    };
    let EventKind::ToolResult { result_preview, .. } = &event.kind else {
        panic!("{event:?}");
    };
    assert_eq!(result_preview, "Screenshot\n[image]\n[image]");
    assert_eq!(files.text, None);
    assert_eq!(
        files.images,
        [crate::adapter::ResultImage {
            media_type: "image/png".into(),
            data: b"\x89PNG\r\n".to_vec(),
        }]
    );
}

/// SPEC §8.1, 0.180: an `Agent` call's result is the subagent's report from `tool_use_result`
/// (2.1.312), without the trailer the CLI adds to the `tool_result` for the model.
#[test]
fn an_agent_result_is_its_report() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let content = json!([
        {"type":"text","text":"The nets are fine."},
        {"type":"text","text":"agentId: a60dff5983122596e (use SendMessage with to: 'a60dff5983122596e', summary: '<5-10 word recap>' to continue this agent)\n<usage>subagent_tokens: 1200\ntool_uses: 3\nduration_ms: 4100</usage>"},
    ]);
    let report = json!({"status":"completed","agentId":"a60dff5983122596e","agentType":"Explore",
        "content":[{"type":"text","text":"The nets are fine."}],"totalToolUseCount":3,
        "totalDurationMs":4100,"totalTokens":1200,"prompt":"Check the nets"});
    let got = events(&emitted(
        core.on_line(&tool_result_line(content.clone(), Some(report.clone()))),
    ))
    .into_iter()
    .cloned()
    .collect::<Vec<_>>();
    let [Event {
        kind: EventKind::ToolResult { result_preview, .. },
        ..
    }] = &got[..]
    else {
        panic!("{got:?}");
    };
    assert_eq!(result_preview, "The nets are fine.");
    // A background launch has no report: its text stays.
    let launched = json!({"status":"async_launched","agentId":"a1","description":"d","prompt":"p","outputFile":"/tmp/o"});
    let got = events(&emitted(core.on_line(&tool_result_line(
        json!("Async agent launched successfully."),
        Some(launched),
    ))))
    .into_iter()
    .cloned()
    .collect::<Vec<_>>();
    let [Event {
        kind: EventKind::ToolResult { result_preview, .. },
        ..
    }] = &got[..]
    else {
        panic!("{got:?}");
    };
    assert_eq!(result_preview, "Async agent launched successfully.");
    // A record with two results: `tool_use_result` is neither's, both keep their own text.
    let mut two: Value =
        serde_json::from_str(&tool_result_line(json!("first"), Some(report))).unwrap();
    two["message"]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"tool_result","tool_use_id":"t2","content":"second"}));
    let got = events(&emitted(core.on_line(&two.to_string())))
        .into_iter()
        .filter_map(|e| match &e.kind {
            EventKind::ToolResult { result_preview, .. } => Some(result_preview.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(got, ["first", "second"]);
}

/// SPEC §8.1, 0.180: the CLI's "(no content)" for a reply without text is no `agent.message`.
#[test]
fn no_content_is_no_message() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let mut got = vec![];
    for l in [
        json!({"type":"assistant","uuid":Uuid::new_v4(),"parent_tool_use_id":null,
            "message":{"model":"claude-haiku-5-5","content":[{"type":"text","text":"(no content)"}]}}),
        json!({"type":"result","subtype":"success","usage":{}}),
    ] {
        got.extend(emitted(core.on_line(&l.to_string())));
    }
    let messages: Vec<_> = events(&got)
        .into_iter()
        .filter(|e| matches!(e.kind, EventKind::AgentMessage { .. }))
        .collect();
    assert!(messages.is_empty(), "{messages:?}");
}

#[test]
fn grant_rules_past_the_cap_say_how_many_more() {
    let ten: Vec<String> = (0..10).map(|i| format!("Bash(cmd{i}:*)")).collect();
    assert_eq!(grant_rules(ten.clone()), ten);
    let twelve: Vec<String> = (0..12).map(|i| format!("Bash(cmd{i}:*)")).collect();
    let shown = grant_rules(twelve.clone());
    assert_eq!(shown.len(), 10);
    assert_eq!(shown[..9], twelve[..9]);
    assert_eq!(shown[9], "… 3 more");
}

/// SPEC 0.231: Stop's `interrupt` is plain: `cancel_queued` would also dequeue the CLI's own
/// lines that carry no `uuid` (task notifications, cron, `/loop`), which hqc cannot write back.
#[test]
fn an_interrupt_does_not_cancel_the_queue_wholesale() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    started(&mut core, sid);
    let (req, _, _) = core.interrupt().expect("a turn runs");
    assert_eq!(
        writes(&[req])[0]["request"],
        json!({"subtype": "interrupt"})
    );
}

/// SPEC 0.231: a Stop cancels, one by one, the lines hqc wrote that the CLI has not taken: the
/// queued one and the one it has not answered yet, not the one it started, one being withdrawn
/// or a line it already cancelled. The CLI's `cancelled` for them comes out as `dropped` of the
/// turn's end, unlike a withdrawn input's.
#[test]
fn a_stop_cancels_the_lines_the_cli_has_not_taken() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let (a, b, c, d) = (
        user_input(sid, "first"),
        user_input(sid, "second"),
        user_input(sid, "third"),
        user_input(sid, "fourth"),
    );
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
    play(&mut core, &lifecycle(&ub, "queued"));
    let uc = line_uuid(&core.inject_with_images(&c, vec![]).expect("a turn runs"));
    // `d` is queued and being withdrawn: a Stop leaves it alone.
    let ud = line_uuid(&core.inject_with_images(&d, vec![]).expect("a turn runs"));
    play(&mut core, &lifecycle(&ud, "queued"));
    let _ = cancel_request(&mut core, d.event_id, &ud);
    let asked: Vec<String> = writes(&core.cancel_unstarted())
        .iter()
        .map(|v| {
            assert_eq!(v["type"], "control_request");
            assert!(v["request_id"].as_str().unwrap().starts_with("hqc-cancel-"));
            assert_eq!(v["request"]["subtype"], "cancel_async_message");
            v["request"]["message_uuid"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(asked, [ub.clone(), uc.clone()]);
    // What the CLI answers with `cancelled` ends up in `dropped`.
    let mut out = play(&mut core, &lifecycle(&ub, "cancelled"));
    out.extend(play(&mut core, &lifecycle(&uc, "cancelled")));
    out.extend(play(&mut core, &lifecycle(&ud, "cancelled")));
    out.extend(play(
        &mut core,
        &lines_of(&[tool_result("toolu_1"), reply("DONE"), result("success")]),
    ));
    let names = [
        (a.event_id, "A"),
        (b.event_id, "B"),
        (c.event_id, "C"),
        (d.event_id, "D"),
    ];
    let t = trace(&out, &names);
    assert_eq!(
        t.iter()
            .filter(|l| l.starts_with("turn_ended"))
            .collect::<Vec<_>>(),
        ["turn_ended [] dropped [B, C]"],
        "{t:?}"
    );
    // Nothing runs: nothing to cancel.
    let mut idle = Core::new(sid, false);
    assert!(idle.cancel_unstarted().is_empty());
}

/// SPEC 0.231: a Stop's `cancel_async_message` can reach the turn's own head line before the CLI
/// started it (cold start): the CLI answers `cancelled` and then no `result` comes for it. That
/// line comes back as dropped, so the supervisor writes it quiet; in the turn the runtime
/// started by itself the head line is not what keeps the turn open and stays as it is.
#[test]
fn a_head_line_the_cli_cancels_before_it_started_comes_back_dropped() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let a = user_input(sid, "first");
    let ua = line_uuid(&core.input(&a));
    let asked = writes(&core.cancel_unstarted());
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0]["request"]["message_uuid"], json!(ua));
    let out = play(&mut core, &lifecycle(&ua, "cancelled"));
    let t = trace(&out, &[(a.event_id, "A")]);
    assert_eq!(t, ["turn_ended [] dropped [A]"], "{t:?}");
    assert!(!core.turn_open());

    // In a turn the runtime started by itself the head line is not what keeps the turn open:
    // its `cancelled` ends nothing, and the turn's own `result` brings it back as dropped.
    let mut core = Core::new(sid, false);
    play(&mut core, &sonnet_init_line());
    assert!(core.runs_own_turn());
    let ua = line_uuid(&core.input(&a));
    assert_eq!(writes(&core.cancel_unstarted()).len(), 1);
    let out = play(&mut core, &lifecycle(&ua, "cancelled"));
    let t = trace(&out, &[(a.event_id, "A")]);
    assert!(!t.iter().any(|l| l.starts_with("turn_ended")), "{t:?}");
    assert!(core.turn_open());
    let out = play(&mut core, &result("success"));
    let t = trace(&out, &[(a.event_id, "A")]);
    assert!(t.iter().any(|l| l == "turn_ended [] dropped [A]"), "{t:?}");
}

/// SPEC 0.231: a take-back of a line the CLI already cancelled (a Stop's) is answered at once,
/// without asking the CLI again, and the turn's end leaves it out of `dropped`; after the
/// turn's end it is no longer here to withdraw.
#[test]
fn a_line_a_stop_cancelled_is_withdrawn_at_once() {
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
    play(&mut core, &lifecycle(&ub, "queued"));
    assert_eq!(writes(&core.cancel_unstarted()).len(), 1);
    play(&mut core, &lifecycle(&ub, "cancelled"));
    let (actions, request_id) = core.withdraw(b.event_id).expect("it is still here");
    assert!(
        actions.is_empty(),
        "nothing is asked of the CLI: {actions:?}"
    );
    assert_eq!(core.withdraw_answer(&request_id), Some(true));
    let out = play(
        &mut core,
        &lines_of(&[tool_result("toolu_1"), reply("DONE"), result("success")]),
    );
    let t = trace(&out, &[(a.event_id, "A"), (b.event_id, "B")]);
    assert_eq!(
        t.iter()
            .filter(|l| l.starts_with("turn_ended"))
            .collect::<Vec<_>>(),
        ["turn_ended []"],
        "{t:?}"
    );
    // The other order: the turn ended first, B came back dropped, there is nothing to withdraw.
    let mut core = Core::new(sid, false);
    let ua = line_uuid(&core.input(&a));
    play(
        &mut core,
        &lines_of(&[
            lifecycle(&ua, "queued"),
            lifecycle(&ua, "started"),
            echo(&ua, "first"),
        ]),
    );
    let ub = line_uuid(&core.inject_with_images(&b, vec![]).expect("a turn runs"));
    play(&mut core, &lifecycle(&ub, "queued"));
    core.cancel_unstarted();
    play(&mut core, &lifecycle(&ub, "cancelled"));
    let out = play(&mut core, &lines_of(&[reply("DONE"), result("success")]));
    let t = trace(&out, &[(a.event_id, "A"), (b.event_id, "B")]);
    assert!(t.iter().any(|l| l == "turn_ended [] dropped [B]"), "{t:?}");
    assert!(core.withdraw(b.event_id).is_none());
}

/// SPEC 0.231: a quiet line is the record of an earlier Stop, so a Stop never cancels it, also
/// when it waits in the CLI's queue behind a turn.
#[test]
fn a_stop_never_cancels_a_quiet_line() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let a = user_input(sid, "first");
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
    let q = user_input(sid, "stopped earlier");
    let uq = line_uuid(&[core.input_quiet_with_images(&q, vec![])]);
    play(&mut core, &lifecycle(&uq, "queued"));
    let b = user_input(sid, "second");
    let ub = line_uuid(&core.inject_with_images(&b, vec![]).expect("a turn runs"));
    play(&mut core, &lifecycle(&ub, "queued"));
    let asked: Vec<String> = writes(&core.cancel_unstarted())
        .iter()
        .map(|v| v["request"]["message_uuid"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(asked, [ub]);
}

fn quiet_result(taken: &[&str]) -> String {
    json!({"type": "result", "subtype": "success", "is_error": false, "duration_api_ms": 0,
        "num_turns": 0, "stop_reason": null, "total_cost_usd": 0.153686,
        "usage": {"input_tokens": 0, "output_tokens": 0},
        "session_id": CLI_SESSION, "user_message_uuids": taken})
    .to_string()
}

fn sonnet_init_line() -> String {
    json!({"type": "system", "subtype": "init", "model": "claude-sonnet-5-5",
        "permissionMode": "default", "session_id": CLI_SESSION, "uuid": Uuid::new_v4()})
    .to_string()
}

/// SPEC 0.231, the capture after Stop (2.1.312): a line written with `shouldQuery: false` gets
/// `queued`, `started`, an `init`, its echo, a `result` with `num_turns: 0` and `completed`. The
/// engine sends one event for it, naming the input, and nothing a turn would: no `status
/// running` for that `init`, no accepted input at the echo, no `usage`, `status idle` or end of
/// turn at the `result`; no turn of the CLI's own runs meanwhile.
#[test]
fn a_quiet_line_is_taken_by_its_result_and_makes_no_turn() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let input = user_input(sid, "Also remember: the code word is OTTER-42.");
    let action = core.input_quiet_with_images(&input, vec![]);
    let line = &writes(&[action])[0];
    assert_eq!(line["shouldQuery"], false);
    // "later": the CLI folds only "next" and above into a running turn, a turn it started by
    // itself too (SPEC 0.231); a line without `priority` is "next".
    assert_eq!(line["priority"], "later");
    assert_eq!(line["origin"], json!({"kind": "human"}));
    assert_eq!(line["message"]["content"], input.text);
    let u = line["uuid"].as_str().unwrap().to_owned();
    assert!(!core.turn_open());

    let mut got = vec![];
    for l in [
        lifecycle(&u, "queued"),
        lifecycle(&u, "started"),
        sonnet_init_line(),
    ] {
        got.extend(emitted(core.on_line(&l)));
        assert!(!core.turn_open() && !core.runs_own_turn(), "after {l}");
    }
    got.extend(emitted(core.on_line(&echo(&u, &input.text))));
    got.extend(emitted(core.on_line(&quiet_result(&[&u]))));
    got.extend(emitted(core.on_line(&lifecycle(&u, "completed"))));
    assert!(!core.turn_open());
    assert_eq!(
        got.iter()
            .filter(|e| !matches!(
                e,
                AdapterEvent::Commands(_) | AdapterEvent::PermissionMode(_)
            ))
            .collect::<Vec<_>>(),
        [&AdapterEvent::QuietTaken {
            event_ids: vec![input.event_id]
        }],
        "{got:?}"
    );

    // A quiet line of a message that is no person's has no `origin`.
    let mut other = user_input(sid, "FYI");
    other.human = false;
    let action = core.input_quiet_with_images(&other, vec![]);
    assert!(writes(&[action])[0].get("origin").is_none());
}

/// SPEC 0.231, the second capture: three quiet lines written back to back. The CLI takes the
/// first alone, the second and third together (one `init`, one echo with the third's `uuid` and
/// both texts, one `result` listing both): each input is named exactly once.
#[test]
fn quiet_lines_taken_together_are_each_named_once() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let inputs: Vec<SessionInput> = ["first", "second", "third"]
        .into_iter()
        .map(|t| user_input(sid, &format!("Remember the {t} code word.")))
        .collect();
    let u: Vec<String> = inputs
        .iter()
        .map(|i| line_uuid(&[core.input_quiet_with_images(i, vec![])]))
        .collect();
    let both = format!("{}\n{}", inputs[1].text, inputs[2].text);
    let mut got = vec![];
    for l in [
        lifecycle(&u[0], "queued"),
        lifecycle(&u[1], "queued"),
        lifecycle(&u[2], "queued"),
        lifecycle(&u[0], "started"),
        sonnet_init_line(),
        echo(&u[0], &inputs[0].text),
        quiet_result(&[&u[0]]),
        lifecycle(&u[0], "completed"),
        // The second is echoed on its own before the `init` of the pair.
        echo(&u[1], &inputs[1].text),
        lifecycle(&u[1], "started"),
        lifecycle(&u[2], "started"),
        sonnet_init_line(),
        echo(&u[2], &both),
        quiet_result(&[&u[1], &u[2]]),
        lifecycle(&u[1], "completed"),
        lifecycle(&u[2], "completed"),
    ] {
        got.extend(emitted(core.on_line(&l)));
        assert!(!core.turn_open(), "after {l}");
    }
    assert_eq!(
        got.iter()
            .filter(|e| !matches!(
                e,
                AdapterEvent::Commands(_) | AdapterEvent::PermissionMode(_)
            ))
            .collect::<Vec<_>>(),
        [
            &AdapterEvent::QuietTaken {
                event_ids: vec![inputs[0].event_id]
            },
            &AdapterEvent::QuietTaken {
                event_ids: vec![inputs[1].event_id, inputs[2].event_id]
            },
        ],
        "{got:?}"
    );
}

/// SPEC 0.231: a quiet line the CLI drops (`command_lifecycle` `cancelled`: it failed to process
/// the line) is told once as `QuietCancelled`, naming the input; it is not taken, and a `result`
/// listing none of it names nothing.
#[test]
fn a_quiet_line_the_cli_drops_is_told_cancelled() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let input = user_input(sid, "Remember OTTER-42.");
    let q = line_uuid(&[core.input_quiet_with_images(&input, vec![])]);
    let mut got = emitted(core.on_line(&lifecycle(&q, "queued")));
    got.extend(emitted(core.on_line(&lifecycle(&q, "cancelled"))));
    got.extend(emitted(core.on_line(&lifecycle(&q, "cancelled"))));
    assert_eq!(
        got.iter()
            .filter(|e| !matches!(e, AdapterEvent::Commands(_)))
            .collect::<Vec<_>>(),
        [&AdapterEvent::QuietCancelled {
            event_id: input.event_id
        }],
        "{got:?}"
    );
    let after = emitted(core.on_line(&quiet_result(&[])));
    assert!(
        !after
            .iter()
            .any(|e| matches!(e, AdapterEvent::QuietTaken { .. })),
        "{after:?}"
    );
}

/// After quiet lines the next input is an ordinary turn: its `init` is `status running` of no
/// one's, its echo accepts it and its `result` ends the turn, and a `result` that lists none of
/// the quiet lines is that turn's.
#[test]
fn a_turn_after_a_quiet_line_is_an_ordinary_turn() {
    let sid = Uuid::now_v7();
    let mut core = Core::new(sid, false);
    let quiet = user_input(sid, "Remember OTTER-42.");
    let q = line_uuid(&[core.input_quiet_with_images(&quiet, vec![])]);
    for l in [
        lifecycle(&q, "started"),
        sonnet_init_line(),
        echo(&q, &quiet.text),
        quiet_result(&[&q]),
    ] {
        core.on_line(&l);
    }
    let next = user_input(sid, "What code word did I give you?");
    let u = line_uuid(&core.input(&next));
    let out = emitted(play(
        &mut core,
        &lines_of(&[
            lifecycle(&u, "started"),
            sonnet_init_line(),
            echo(&u, &next.text),
            reply("OTTER-42"),
            result("success"),
        ]),
    ));
    let names = [(next.event_id, "N")];
    assert_eq!(
        trace(
            &out.into_iter().map(Action::Emit).collect::<Vec<_>>(),
            &names
        ),
        [
            "accepted N",
            "message \"OTTER-42\" to N",
            "status to N",
            "turn_ended []"
        ]
    );
}

/// SPEC 0.235: each of the hub's MCP servers goes in `--mcp-config` under its own name as a relay
/// `hqc-worker mcp --upstream <name>`, with no address and no header: those stay on the hub. Its
/// tools are not allowed outright.
#[tokio::test]
async fn the_hubs_mcp_servers_are_relays_under_their_own_names() {
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
    s.mcp_servers = vec!["github".into(), "wiki".into()];
    let (tx, _rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    let cmd = claude.commands()[0].clone();
    let i = cmd.args.iter().position(|a| a == "--mcp-config").unwrap();
    let cfg: serde_json::Value = serde_json::from_str(&cmd.args[i + 1]).unwrap();
    assert_eq!(
        cfg["mcpServers"]["github"],
        json!({
            "type": "stdio",
            "command": "/opt/hqc/hqc-worker",
            "args": ["mcp", "--socket", "/home/u/.hqc/mcp.sock", "--upstream", "github"],
            "env": {"HQC_SESSION_TOKEN": "hqcs_the_token"},
        })
    );
    assert_eq!(cfg["mcpServers"]["wiki"]["args"][4], "wiki");
    assert!(cfg["mcpServers"]["hqc"].is_object());
    assert!(
        !cmd.args.iter().any(|a| a.contains("mcp__github")),
        "not allowed outright"
    );
}

/// SPEC 0.245: the hub's skills come as the plugin folder the supervisor built, with a
/// same-named skill of the machine hidden; without a folder, neither flag.
#[tokio::test]
async fn the_hubs_skills_are_a_plugin_dir_and_hide_the_machines_copy() {
    let work = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let claude = ScriptedClaude::new("");
    let factory = ClaudeCodeAdapters::new(options(config.path()), Arc::new(claude.clone()));
    let mut s = spec(Uuid::now_v7(), work.path(), Uuid::new_v4());
    s.skills = vec![
        hqc_proto::skills::SkillRef {
            name: "bench".into(),
            sha256: "a".repeat(64),
        },
        hqc_proto::skills::SkillRef {
            name: "wiki".into(),
            sha256: "b".repeat(64),
        },
    ];
    let (tx, _rx) = mpsc::channel(16);
    let mut a = factory.create(&s, tx.clone());
    a.start(&s).await.unwrap();
    let cmd = claude.commands()[0].clone();
    assert!(
        !cmd.args.iter().any(|a| a == "--plugin-dir"),
        "no folder, no plugin"
    );
    let i = cmd.args.iter().position(|a| a == "--settings").unwrap();
    let settings: serde_json::Value = serde_json::from_str(&cmd.args[i + 1]).unwrap();
    assert!(settings.get("skillOverrides").is_none());

    s.skill_dir = Some("/home/u/.hqc/hub-skills/sets/abc".into());
    let mut a = factory.create(&s, tx);
    a.start(&s).await.unwrap();
    let cmd = claude.commands()[1].clone();
    let i = cmd.args.iter().position(|a| a == "--plugin-dir").unwrap();
    assert_eq!(cmd.args[i + 1], "/home/u/.hqc/hub-skills/sets/abc");
    let i = cmd.args.iter().position(|a| a == "--settings").unwrap();
    let settings: serde_json::Value = serde_json::from_str(&cmd.args[i + 1]).unwrap();
    assert_eq!(
        settings["skillOverrides"],
        json!({"bench": "off", "wiki": "off"})
    );
    assert!(settings["permissions"].is_object(), "the rules stay");
}
