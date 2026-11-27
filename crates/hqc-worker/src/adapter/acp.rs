//! The generic ACP adapter (SPEC §8.2, iteration 10): one agent process per session that speaks
//! the Agent Client Protocol v1 over stdio (JSON-RPC, one message per line).
//!
//! Nothing here knows an agent by name: what is particular to one is its `[acp_agents.<name>]`
//! table of `worker.toml` ([`AcpAgent`]): the command, the environments that may run it, its own
//! ids of hqc's modes, a start flag for Allow all, a warning.
//!
//! - `start`/`resume`: the process in the session's working folder with its environment (§14),
//!   `initialize` (no `fs`, no `terminal`: the agent uses its own tools), then `session/resume`
//!   (no replay: hqc has the history) or `session/load` (its replay is dropped) of the session's
//!   `runtime_session_id`, else `session/new`; hqc's MCP servers go in `mcpServers`; the mode and
//!   the model are set through the agent's modes or config options.
//! - `input`: `session/prompt`; the reply ends the turn ([`AdapterEvent::TurnEnded`]).
//! - `stop`: `session/cancel`; an agent that does not end the turn within
//!   [`AcpOptions::stop_grace`] is ended.
//! - `session/request_permission`: the worker's `deny` (a reject without a card), its allow
//!   rules and hqc's folder rules (allowed once without a card), else `approval.request` with
//!   the agent's own options. The worker sees only what the agent asks (SPEC §8.2).
//! - An auth-required error (`-32000`) is `system agent_signed_out`: the agent is signed in on the
//!   machine (RUNBOOK).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context};
use async_trait::async_trait;
use hqc_proto::event::{
    AgentTask, ApprovalCanonical, ApprovalOption, Author, CancelReason, ClarifyAction,
    ClarifyAnswer, EventKind, SessionState, TaskStatus,
};
use hqc_proto::frame::{PermissionMode, Runtime, SessionInput, SlashCommand};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use uuid::Uuid;

use super::claude_code::engine::{preview, PREVIEW_LIMIT};
use super::claude_code::process::{CommandSpec, OsSpawner, Sig, SignalFn, Spawner};
use super::claude_code::McpLaunch;
use super::jsonrpc::{read_loop, Handler, NoHandler, Rpc, RpcError, METHOD_NOT_FOUND};
use super::{new_event, Adapter, AdapterEvent, AdapterFactory, EventSink, ResultFiles};
use crate::config::{AcpAgent, Config, StartSpec};
use crate::permissions::Policy;

/// `system.code` of a turn or start the agent refused because it is not signed in on the
/// machine (SPEC §4, §8.2).
pub const AGENT_SIGNED_OUT: &str = "agent_signed_out";
/// `system.code` of a session whose agent could not continue its earlier conversation and
/// started a new one (SPEC §4, §8.2).
pub const CONVERSATION_LOST: &str = "acp_conversation_lost";
/// `system.code` of a permission request the worker's `deny` refused (SPEC §4).
pub const POLICY_DENIED: &str = "policy_denied";

/// The factory's settings.
#[derive(Clone)]
pub struct AcpOptions {
    /// How the agents start hqc's MCP servers (SPEC §7.2); `None`: they get none.
    pub mcp: Option<McpLaunch>,
    /// The key store and the agents' sockets, for an environment with `ssh_keys` (T44).
    pub ssh: crate::ssh::Store,
    /// How long `initialize` and the session's setup may take.
    pub setup_timeout: Duration,
    /// How long an agent has to end its turn after `session/cancel`.
    pub stop_grace: Duration,
    /// How long a process has after SIGTERM before SIGKILL.
    pub kill_grace: Duration,
}

impl AcpOptions {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            mcp: None,
            ssh: cfg.ssh.clone(),
            setup_timeout: Duration::from_secs(60),
            stop_grace: Duration::from_secs(10),
            kill_grace: Duration::from_secs(5),
        }
    }
}

/// The factory of [`AcpAdapter`]s, for the agents of `worker.toml`.
#[derive(Clone)]
pub struct AcpAdapters {
    opts: Arc<AcpOptions>,
    spawner: Arc<dyn Spawner>,
}

impl AcpAdapters {
    pub fn new(opts: AcpOptions, spawner: Arc<dyn Spawner>) -> Self {
        Self {
            opts: Arc::new(opts),
            spawner,
        }
    }

    pub fn from_config(cfg: &Config) -> Self {
        Self::new(AcpOptions::from_config(cfg), Arc::new(OsSpawner))
    }

    /// Give every session hqc's MCP servers (SPEC §7.2).
    pub fn with_mcp(mut self, mcp: McpLaunch) -> Self {
        Arc::make_mut(&mut self.opts).mcp = Some(mcp);
        self
    }

    pub fn supports(&self, runtime: Runtime) -> bool {
        runtime == Runtime::Acp
    }

    pub fn create(&self, spec: &StartSpec, events: EventSink) -> Box<dyn Adapter> {
        Box::new(AcpAdapter {
            session_id: spec.session_id,
            opts: self.opts.clone(),
            spawner: self.spawner.clone(),
            events,
            run: None,
        })
    }
}

/// What `initialize` told about an agent, for `hello` (SPEC §5.4).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Probe {
    pub version: Option<String>,
    pub load_session: bool,
    pub resume: bool,
    pub image: bool,
}

/// Starts `agent` in `cwd` with no session, asks `initialize` and ends it: what `hello` says
/// of the agent. `Err`: the binary is missing or does not speak ACP.
pub async fn probe(agent: &AcpAgent, cwd: PathBuf, timeout: Duration) -> anyhow::Result<Probe> {
    probe_with(&OsSpawner, agent, cwd, timeout).await
}

pub async fn probe_with(
    spawner: &dyn Spawner,
    agent: &AcpAgent,
    cwd: PathBuf,
    timeout: Duration,
) -> anyhow::Result<Probe> {
    let mut env = crate::adapter::claude_code::child_env(std::env::vars());
    for (k, v) in agent.env.iter() {
        env.retain(|(x, _)| x != k);
        env.push((k.into(), v.into()));
    }
    let cmd = CommandSpec {
        program: agent.command.clone(),
        args: agent.args.clone(),
        cwd,
        env,
    };
    let spawned = spawner
        .spawn(&cmd)
        .with_context(|| format!("spawning {}", cmd.program.display()))?;
    let signal = spawned.signal;
    let rpc = Rpc::new(spawned.stdin, true);
    let reader = tokio::spawn(read_loop(spawned.stdout, rpc.clone(), Arc::new(NoHandler)));
    let r = tokio::time::timeout(timeout, rpc.request("initialize", initialize_params())).await;
    let _ = signal(Sig::Terminate);
    reader.abort();
    let v = match r {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => bail!("initialize: {e}"),
        Err(_) => bail!("no answer to initialize in {} s", timeout.as_secs()),
    };
    let caps = Caps::of(&v);
    Ok(Probe {
        version: v["agentInfo"]["version"].as_str().map(str::to_owned),
        load_session: caps.load_session,
        resume: caps.resume,
        image: v["agentCapabilities"]["promptCapabilities"]["image"] == true,
    })
}

/// What the answer to `initialize` offers for sessions.
#[derive(Debug, Clone, Copy, Default)]
struct Caps {
    load_session: bool,
    resume: bool,
    close: bool,
}

impl Caps {
    fn of(init: &Value) -> Self {
        let a = &init["agentCapabilities"];
        let s = &a["sessionCapabilities"];
        Self {
            load_session: a["loadSession"] == true,
            resume: s["resume"].is_object(),
            close: s["close"].is_object(),
        }
    }
}

fn initialize_params() -> Value {
    json!({
        "protocolVersion": 1,
        // The worker reads and writes no file and runs no terminal for the agent (SPEC §8.2).
        "clientCapabilities": {
            "fs": {"readTextFile": false, "writeTextFile": false},
            "terminal": false,
        },
        "clientInfo": {"name": "hqc", "version": env!("CARGO_PKG_VERSION")},
    })
}

// ----------------------------------------------------------------------------------------------
// The session.

/// A permission request waiting for the person.
struct Asked {
    rpc_id: Value,
    options: Vec<Value>,
}

/// What the reader and the adapter share.
struct Shared {
    session_id: Uuid,
    agent: Arc<AcpAgent>,
    events: EventSink,
    policy: Policy,
    folder_rules: Option<crate::permissions::FolderRules>,
    workspace: PathBuf,
    /// The agent's session id.
    acp_session: Option<String>,
    /// `session/load` replays the conversation: dropped, hqc has it.
    replaying: bool,
    turn: Option<Turn>,
    asked: BTreeMap<String, Asked>,
    /// The agent's modes (`session/set_mode`) and config options as last told.
    mode_ids: Vec<String>,
    config_options: Vec<Value>,
    /// The mode the session runs in, as last set or told.
    mode: PermissionMode,
    /// The agent took the mode hqc last set and has not moved since: a switch to the same
    /// mode may be skipped only then.
    confirmed: bool,
    /// Context use as the last `usage_update` said it.
    context: Option<(u64, u64)>,
    cost: Option<f64>,
}

/// The running turn.
struct Turn {
    input: Uuid,
    /// The text since the last tool call: the final message at the end.
    text: String,
    thinking: String,
    tools: HashMap<String, std::time::Instant>,
}

impl Shared {
    fn event(&self, kind: EventKind) -> AdapterEvent {
        let author = match &kind {
            EventKind::System { .. } => Author::System,
            _ => Author::Agent {
                id: self.session_id,
            },
        };
        AdapterEvent::Event(new_event(self.session_id, author, kind))
    }

    fn system(&self, code: &str, text: String) -> AdapterEvent {
        self.event(EventKind::System {
            text,
            code: code.into(),
            call_id: None,
            status: None,
            task_id: None,
            from_session_id: None,
            usage: None,
            fork: None,
            compaction: None,
            model: None,
            goal: None,
        })
    }

    fn signed_out(&self) -> AdapterEvent {
        self.system(
            AGENT_SIGNED_OUT,
            format!(
                "The agent {} is not signed in on this machine: sign it in there (RUNBOOK)",
                self.agent.name
            ),
        )
    }

    /// The thinking written so far, as one event.
    fn flush_thinking(&mut self, out: &mut Vec<AdapterEvent>) {
        let Some(t) = self.turn.as_mut() else {
            return;
        };
        let text = std::mem::take(&mut t.thinking);
        if !text.trim().is_empty() {
            out.push(self.event(EventKind::AgentThinking { text }));
        }
    }

    /// The text written so far, as a message that is not the final one: a tool call follows.
    fn flush_text(&mut self, out: &mut Vec<AdapterEvent>) {
        self.flush_thinking(out);
        let Some(t) = self.turn.as_mut() else {
            return;
        };
        let text = std::mem::take(&mut t.text);
        let input = t.input;
        if !text.trim().is_empty() {
            out.push(self.event(EventKind::AgentMessage {
                text,
                r#final: false,
                in_reply_to: Some(input),
                runtime: false,
                interrupted: false,
                refusal: false,
            }));
        }
    }

    /// A `session/update` notification (`params.update`).
    fn update(&mut self, u: &Value) -> Vec<AdapterEvent> {
        let mut out = Vec::new();
        if self.replaying {
            return out;
        }
        match u["sessionUpdate"].as_str().unwrap_or("") {
            "agent_message_chunk" => {
                if let Some(text) = chunk_text(&u["content"]) {
                    self.flush_thinking(&mut out);
                    if let Some(t) = self.turn.as_mut() {
                        t.text.push_str(&text);
                    }
                    out.push(self.event(EventKind::AgentDelta {
                        text,
                        reset: false,
                        thinking: false,
                    }));
                }
            }
            "agent_thought_chunk" => {
                if let Some(text) = chunk_text(&u["content"]) {
                    if let Some(t) = self.turn.as_mut() {
                        t.thinking.push_str(&text);
                    }
                    out.push(self.event(EventKind::AgentDelta {
                        text,
                        reset: false,
                        thinking: true,
                    }));
                }
            }
            "tool_call" => {
                let Some(id) = u["toolCallId"].as_str().map(str::to_owned) else {
                    return out;
                };
                self.flush_text(&mut out);
                if let Some(t) = self.turn.as_mut() {
                    t.tools.insert(id.clone(), std::time::Instant::now());
                }
                let title = u["title"].as_str().unwrap_or("");
                out.push(self.event(EventKind::ToolCall {
                    call_id: id.clone(),
                    tool: tool_name(title, u["kind"].as_str()),
                    args_preview: args_preview(title, u.get("rawInput")),
                }));
                if let s @ ("completed" | "failed") = u["status"].as_str().unwrap_or("") {
                    out.push(self.tool_result(
                        &id,
                        s == "failed",
                        &u["content"],
                        u.get("rawOutput"),
                    ));
                }
            }
            "tool_call_update" => {
                let Some(id) = u["toolCallId"].as_str() else {
                    return out;
                };
                if let s @ ("completed" | "failed") = u["status"].as_str().unwrap_or("") {
                    out.push(self.tool_result(
                        id,
                        s == "failed",
                        &u["content"],
                        u.get("rawOutput"),
                    ));
                }
            }
            "plan" => {
                let items = u["entries"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .map(|e| AgentTask {
                        subject: preview(e["content"].as_str().unwrap_or(""), PREVIEW_LIMIT),
                        status: match e["status"].as_str() {
                            Some("pending") => TaskStatus::Pending,
                            Some("in_progress") => TaskStatus::InProgress,
                            Some("completed") => TaskStatus::Completed,
                            _ => TaskStatus::Unknown,
                        },
                        active_form: None,
                    })
                    .collect();
                out.push(self.event(EventKind::Tasks { items }));
            }
            "available_commands_update" => {
                let list = u["availableCommands"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                out.push(AdapterEvent::Commands(
                    list.iter()
                        .filter_map(|c| {
                            let name = c["name"].as_str()?.trim_start_matches('/');
                            Some(SlashCommand {
                                name: preview(name, 64),
                                description: Some(first_line(
                                    c["description"].as_str().unwrap_or(""),
                                ))
                                .filter(|d| !d.is_empty()),
                                argument_hint: None,
                            })
                        })
                        .collect(),
                ));
            }
            "current_mode_update" => {
                if let Some(id) = u["currentModeId"].as_str() {
                    self.agent_moved(id, &mut out);
                }
            }
            "config_option_update" => {
                if let Some(o) = u["configOptions"].as_array() {
                    self.config_options = o.clone();
                }
                if let Some(id) = mode_option_value(&self.config_options) {
                    if self.hqc_mode(&id) != Some(self.mode) {
                        self.agent_moved(&id, &mut out);
                    }
                }
            }
            "session_info_update" => {
                if let Some(title) = u["title"].as_str() {
                    let title = first_line(title);
                    if !title.is_empty() {
                        out.push(AdapterEvent::Title(title));
                    }
                }
            }
            "usage_update" => {
                if let (Some(used), Some(size)) = (u["used"].as_u64(), u["size"].as_u64()) {
                    self.context = Some((used, size));
                }
                if u["cost"]["currency"] == "USD" {
                    self.cost = u["cost"]["amount"].as_f64();
                }
            }
            _ => {}
        }
        out
    }

    fn tool_result(
        &mut self,
        id: &str,
        error: bool,
        content: &Value,
        raw_output: Option<&Value>,
    ) -> AdapterEvent {
        let started = self.turn.as_mut().and_then(|t| t.tools.remove(id));
        let mut text = String::new();
        for c in content.as_array().map(Vec::as_slice).unwrap_or_default() {
            match c["type"].as_str() {
                Some("content") => {
                    if let Some(t) = chunk_text(&c["content"]) {
                        text.push_str(&t);
                        text.push('\n');
                    }
                }
                Some("diff") => text.push_str(&diff_text(c)),
                _ => {}
            }
        }
        if text.is_empty() {
            if let Some(v) = raw_output {
                text = match v {
                    Value::String(s) => s.clone(),
                    v => v.to_string(),
                };
            }
        }
        let full = (text.len() > PREVIEW_LIMIT).then(|| preview(&text, super::FULL_RESULT_LIMIT));
        let event = new_event(
            self.session_id,
            Author::Agent {
                id: self.session_id,
            },
            EventKind::ToolResult {
                call_id: id.to_string(),
                result_preview: preview(&text, PREVIEW_LIMIT),
                error,
                error_text: None,
                duration_ms: started.map(|s| s.elapsed().as_millis() as u64),
                artifact_id: None,
                artifact_ids: vec![],
            },
        );
        AdapterEvent::ToolResult {
            event,
            files: ResultFiles {
                text: full,
                images: vec![],
            },
        }
    }

    /// hqc's mode of the agent's mode or config value `id`, by the agent's table; an id the
    /// table does not name counts as Allow all (an unknown mode is taken as the highest).
    /// `None` for an agent whose table names no mode: it is assumed to run Ask, and hqc cannot
    /// read its own modes (SPEC §8.2).
    fn hqc_mode(&self, id: &str) -> Option<PermissionMode> {
        let m = &self.agent.modes;
        if m.ask.is_none() && m.auto.is_none() && m.allow_all.is_none() {
            return None;
        }
        if m.ask_also.iter().any(|a| a == id) {
            return Some(PermissionMode::Ask);
        }
        Some(
            [
                PermissionMode::AllowAll,
                PermissionMode::Auto,
                PermissionMode::Ask,
            ]
            .into_iter()
            .find(|x| m.id(*x) == Some(id))
            .unwrap_or(PermissionMode::AllowAll),
        )
    }

    /// The mode the agent says it is in (`current_mode_update`, a mode config option's value):
    /// kept and reported; one hqc did not set ends the confirmation of the last switch.
    fn agent_moved(&mut self, id: &str, out: &mut Vec<AdapterEvent>) {
        let Some(mode) = self.hqc_mode(id) else {
            return;
        };
        if mode != self.mode {
            self.confirmed = false;
        }
        self.mode = mode;
        out.push(AdapterEvent::PermissionMode(mode));
    }

    /// The end of the turn of input `input`: the agent answered `session/prompt` with `r`.
    fn end_turn(&mut self, r: Result<Value, RpcError>) -> Vec<AdapterEvent> {
        let mut out = Vec::new();
        self.flush_thinking(&mut out);
        let Some(turn) = self.turn.take() else {
            return out;
        };
        let stop = match &r {
            Ok(v) => v["stopReason"].as_str().unwrap_or("end_turn").to_string(),
            Err(_) => "error".into(),
        };
        match &r {
            Err(e) if e.auth_required() => out.push(self.signed_out()),
            Err(e) => out.push(self.event(EventKind::AgentMessage {
                text: format!("The agent failed the turn: {}", e.message),
                r#final: true,
                in_reply_to: Some(turn.input),
                runtime: true,
                interrupted: false,
                refusal: false,
            })),
            // SPEC §8.2: a cancelled turn has no final message.
            Ok(_) if stop == "cancelled" => {
                if !turn.text.trim().is_empty() {
                    out.push(self.event(EventKind::AgentMessage {
                        text: turn.text.clone(),
                        r#final: false,
                        in_reply_to: Some(turn.input),
                        runtime: false,
                        interrupted: true,
                        refusal: false,
                    }));
                }
            }
            Ok(_) => out.push(self.event(EventKind::AgentMessage {
                text: turn.text.clone(),
                r#final: true,
                in_reply_to: Some(turn.input),
                runtime: false,
                interrupted: false,
                refusal: stop == "refusal",
            })),
        }
        // The turn's tokens where the agent reports them (`usage` of the answer, not yet part
        // of ACP v1), the context as `usage_update` last told it.
        let usage = r.as_ref().ok().map(|v| &v["usage"]);
        let tok = |k: &str| usage.and_then(|u| u[k].as_u64());
        let (input_tokens, output_tokens) = (tok("inputTokens"), tok("outputTokens"));
        if input_tokens.is_some() || output_tokens.is_some() || self.context.is_some() {
            out.push(self.event(EventKind::Usage {
                input_tokens: input_tokens.unwrap_or(0),
                output_tokens: output_tokens.unwrap_or(0),
                cache_read: tok("cachedReadTokens"),
                cache_write: tok("cachedWriteTokens"),
                cost_usd: None,
                session_cost_usd: self.cost,
                session_tokens: None,
                model: None,
                context_tokens: self.context.map(|c| c.0),
                context_limit: self.context.map(|c| c.1),
                cache_ttl_s: None,
            }));
        }
        out.extend(self.cancel_asked(CancelReason::Stopped).1);
        out.push(self.event(EventKind::Status {
            state: SessionState::Idle,
            detail: None,
            in_reply_to: Some(turn.input),
        }));
        out.push(AdapterEvent::TurnEnded {
            carried: vec![],
            dropped: vec![],
        });
        out
    }

    /// The open permission requests, closed: the JSON-RPC ids to answer `cancelled` and the
    /// `approval.cancel` events.
    fn cancel_asked(&mut self, reason: CancelReason) -> (Vec<Value>, Vec<AdapterEvent>) {
        let asked = std::mem::take(&mut self.asked);
        let mut ids = Vec::new();
        let mut out = Vec::new();
        for (request_id, a) in asked {
            ids.push(a.rpc_id);
            out.push(self.event(EventKind::ApprovalCancel { request_id, reason }));
        }
        (ids, out)
    }

    /// `session/request_permission`: answered here when the worker's rules decide it (the
    /// answer), else an `approval.request` for the person (the events).
    fn permission(&mut self, rpc_id: Value, req: &Value) -> (Option<Value>, Vec<AdapterEvent>) {
        let tc = &req["toolCall"];
        let title = tc["title"].as_str().unwrap_or("").to_string();
        let raw = tc.get("rawInput").filter(|v| !v.is_null()).cloned();
        let kind = tc["kind"].as_str();
        // An `execute` call's command; a request that names no kind is ACP's `other`: a card.
        let execute = kind == Some("execute");
        let command = raw.as_ref().filter(|_| execute).and_then(execute_command);
        let options: Vec<Value> = req["options"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|o| o["optionId"].is_string())
            .collect();
        let pick = |kinds: &[&str]| {
            kinds.iter().find_map(|k| {
                options
                    .iter()
                    .find(|o| o["kind"] == *k)
                    .and_then(|o| o["optionId"].as_str().map(str::to_owned))
            })
        };
        let mut out = Vec::new();
        // The worker's `deny` (SPEC §8.2, §14): the command of an `execute` call, as a `Bash`
        // call of Claude Code is checked.
        if let Some(cmd) = &command {
            let input = json!({ "command": cmd });
            if let Some(rule) = self.policy.denied_by("Bash", &input) {
                out.push(self.system(
                    POLICY_DENIED,
                    format!("Denied by the worker's rule {rule}: {}", preview(cmd, 200)),
                ));
                let answer = match pick(&["reject_once", "reject_always"]) {
                    Some(id) => json!({"outcome": {"outcome": "selected", "optionId": id}}),
                    None => json!({"outcome": {"outcome": "cancelled"}}),
                };
                return (Some(answer), out);
            }
            // The worker's allow rules and hqc's of the folder: once, without a card. A call
            // whose command cannot be read, or whose `rawInput` says more than the command
            // (where or how it runs: `cwd`, `env`, a shell), is never allowed so.
            let plain = raw.as_ref().is_some_and(only_harmless_keys);
            let allowed = plain && self.policy.allowed_by("Bash", &input).is_some()
                || plain
                    && self.folder_rules.as_ref().is_some_and(|f| {
                        self.policy
                            .folder_allows(f, &self.workspace, "Bash", &input)
                            .is_some()
                    });
            if allowed {
                if let Some(id) = pick(&["allow_once"]) {
                    return (
                        Some(json!({"outcome": {"outcome": "selected", "optionId": id}})),
                        out,
                    );
                }
            }
        }
        let request_id = Uuid::now_v7().to_string();
        let card_options = options
            .iter()
            .map(|o| ApprovalOption {
                id: o["optionId"].as_str().unwrap_or("").to_string(),
                label: o["name"].as_str().unwrap_or("").to_string(),
                // For the card's look only: agents disagree on what "always" keeps, so hqc
                // writes no rule from it.
                canonical: match o["kind"].as_str() {
                    Some("allow_once") => ApprovalCanonical::Once,
                    Some("allow_always") => ApprovalCanonical::Session,
                    Some("reject_once") => ApprovalCanonical::Deny,
                    Some("reject_always") => ApprovalCanonical::DenyAlways,
                    _ => ApprovalCanonical::Unknown,
                },
                grant: None,
            })
            .collect();
        let tool = tool_name(&title, kind);
        out.push(self.event(EventKind::ApprovalRequest {
            request_id: request_id.clone(),
            tool,
            description: if title.is_empty() {
                "The agent asks to run a tool".into()
            } else {
                preview(&title, PREVIEW_LIMIT)
            },
            args_preview: Some(match &command {
                Some(c) => preview(c, PREVIEW_LIMIT),
                None => args_preview(&title, raw.as_ref()),
            }),
            options: card_options,
            expires_at: None,
            policy_hint: None,
            rule_suggestions: None,
            secret: None,
            reason: None,
            display_name: None,
            human_only: false,
            subagent: None,
        }));
        self.asked.insert(request_id, Asked { rpc_id, options });
        (None, out)
    }
}

/// The text of a content block; other blocks are not shown.
fn chunk_text(c: &Value) -> Option<String> {
    match c["type"].as_str()? {
        "text" => c["text"].as_str().map(str::to_owned),
        "resource_link" => Some(format!(
            "[{}]({})",
            c["name"].as_str().unwrap_or(""),
            c["uri"].as_str().unwrap_or("")
        )),
        _ => None,
    }
}

fn first_line(s: &str) -> String {
    preview(s.lines().next().unwrap_or("").trim(), 200)
}

/// The tool's name in the feed: the agent's title, else its kind.
fn tool_name(title: &str, kind: Option<&str>) -> String {
    let t = first_line(title);
    if !t.is_empty() {
        return t;
    }
    first_line(kind.unwrap_or("tool"))
}

fn args_preview(title: &str, raw: Option<&Value>) -> String {
    match raw {
        Some(v) if !v.is_null() => preview(&v.to_string(), PREVIEW_LIMIT),
        _ => preview(title, PREVIEW_LIMIT),
    }
}

/// The command of an `execute` call from the common keys of its `rawInput`: `command` or
/// `cmd`, a string or an array of words (SPEC §8.2). `None`: none found, and the worker's
/// `deny` cannot apply.
pub fn execute_command(raw: &Value) -> Option<String> {
    for k in ["command", "cmd"] {
        match &raw[k] {
            Value::String(s) if !s.trim().is_empty() => return Some(s.clone()),
            Value::Array(a) if !a.is_empty() => {
                let words: Option<Vec<&str>> = a.iter().map(Value::as_str).collect();
                return words.map(|w| shell_join(&w));
            }
            _ => {}
        }
    }
    None
}

/// `rawInput` holds nothing but the command and keys that do not change where or how it
/// runs: only such a call may be allowed by a rule without a card.
fn only_harmless_keys(raw: &Value) -> bool {
    raw.as_object().is_some_and(|o| {
        o.keys().all(|k| {
            matches!(
                k.as_str(),
                "command" | "cmd" | "description" | "timeout" | "timeout_ms" | "timeoutMs"
            )
        })
    })
}

/// Words joined as a shell reads them back: a word with anything but plain characters quoted.
fn shell_join(words: &[&str]) -> String {
    words
        .iter()
        .map(|w| {
            if !w.is_empty()
                && w.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c))
            {
                w.to_string()
            } else {
                format!("'{}'", w.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A `diff` content of a tool call (`path`, `oldText`, `newText`) as a unified-looking text.
fn diff_text(d: &Value) -> String {
    let path = d["path"].as_str().unwrap_or("");
    let mut s = format!("--- {path}\n+++ {path}\n");
    for l in d["oldText"].as_str().unwrap_or("").lines() {
        s.push('-');
        s.push_str(l);
        s.push('\n');
    }
    for l in d["newText"].as_str().unwrap_or("").lines() {
        s.push('+');
        s.push_str(l);
        s.push('\n');
    }
    s
}

/// hqc's MCP servers (`hqc`, the hub's relays, the environment's local servers) as ACP
/// `mcpServers` entries: the same launchers the Claude Code adapter gives the CLI.
fn mcp_servers(mcp: Option<&McpLaunch>, spec: &StartSpec) -> Vec<Value> {
    let Some(mcp) = mcp else {
        return vec![];
    };
    let local: Vec<String> = spec
        .local_mcp
        .as_ref()
        .map(|l| l.names())
        .unwrap_or_default();
    let config: Value =
        serde_json::from_str(&mcp.config(&spec.session_token, false, &spec.mcp_servers, &local))
            .unwrap_or_default();
    let Some(servers) = config["mcpServers"].as_object() else {
        return vec![];
    };
    servers
        .iter()
        .map(|(name, s)| {
            let env: Vec<Value> = s["env"]
                .as_object()
                .map(|e| {
                    e.iter()
                        .map(|(k, v)| json!({"name": k, "value": v}))
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "name": name,
                "command": s["command"],
                "args": s["args"],
                "env": env,
            })
        })
        .collect()
}

/// The handler of a session's agent.
struct SessionHandler {
    shared: Arc<Mutex<Shared>>,
}

impl SessionHandler {
    async fn send(&self, events: Vec<AdapterEvent>) {
        let sink = self.shared.lock().unwrap().events.clone();
        for e in events {
            let _ = sink.send(e).await;
        }
    }
}

#[async_trait]
impl Handler for SessionHandler {
    async fn request(&self, rpc: &Arc<Rpc>, id: Value, method: &str, params: Value) {
        if method != "session/request_permission" {
            // No `fs`, no `terminal`, no elicitation were offered.
            rpc.respond_error(id, METHOD_NOT_FOUND, "not supported by this client")
                .await;
            return;
        }
        let (answer, events) = self.shared.lock().unwrap().permission(id.clone(), &params);
        self.send(events).await;
        if let Some(a) = answer {
            rpc.respond(id, a).await;
        }
    }

    async fn notification(&self, method: &str, params: Value) {
        if method != "session/update" {
            return;
        }
        let events = self.shared.lock().unwrap().update(&params["update"]);
        self.send(events).await;
    }
}

/// A running agent process.
struct Run {
    rpc: Arc<Rpc>,
    shared: Arc<Mutex<Shared>>,
    pid: Option<u32>,
    signal: Arc<SignalFn>,
    killing: Arc<AtomicBool>,
    exited: Arc<tokio::sync::Notify>,
    gone: Arc<AtomicBool>,
    /// The process's own ssh-agent; it stops with the process.
    _ssh_agent: Option<crate::ssh::SshAgent>,
    reader: tokio::task::JoinHandle<()>,
    session_caps: Caps,
}

pub struct AcpAdapter {
    session_id: Uuid,
    opts: Arc<AcpOptions>,
    spawner: Arc<dyn Spawner>,
    events: EventSink,
    run: Option<Run>,
}

impl AcpAdapter {
    async fn launch(&mut self, spec: &StartSpec) -> anyhow::Result<()> {
        if self.run.is_some() {
            return Ok(());
        }
        let agent = spec
            .acp_agent
            .clone()
            .context("a session of runtime acp without its agent")?;
        spec.recheck_workdir()?;
        let mode = agent.mode_for(spec.permission_mode);
        let key_files = self.opts.ssh.files(&spec.environment.ssh_keys);
        let ssh_agent = if key_files.is_empty() {
            None
        } else {
            Some(
                self.opts
                    .ssh
                    .start_agent(&key_files)
                    .await
                    .context("the session's ssh keys")?,
            )
        };
        let mut args = agent.args.clone();
        if mode == PermissionMode::AllowAll {
            args.extend(agent.allow_all_args.iter().cloned());
        }
        let mut env = spec.environment.process_env();
        for (k, v) in agent.env.iter() {
            env.retain(|(x, _)| x != k);
            env.push((k.into(), v.into()));
        }
        if let Some(a) = &ssh_agent {
            env.push((
                crate::ssh::SSH_AUTH_SOCK.into(),
                a.socket().display().to_string(),
            ));
        }
        let cmd = CommandSpec {
            program: agent.command.clone(),
            args,
            cwd: spec.workdir.clone(),
            env,
        };
        let spawned = self
            .spawner
            .spawn(&cmd)
            .with_context(|| format!("spawning {}", cmd.program.display()))?;
        tracing::info!(session_id = %self.session_id, agent = %agent.name, cwd = %spec.workdir.display(), environment = %spec.environment.name, ?mode, "ACP agent started");
        let rpc = Rpc::new(spawned.stdin, true);
        let shared = Arc::new(Mutex::new(Shared {
            session_id: self.session_id,
            agent: agent.clone(),
            events: self.events.clone(),
            policy: Policy::new(&spec.environment.permissions),
            folder_rules: spec.folder_rules.clone(),
            workspace: spec.workspace.clone(),
            acp_session: None,
            replaying: false,
            turn: None,
            asked: BTreeMap::new(),
            mode_ids: vec![],
            config_options: vec![],
            mode: PermissionMode::Ask,
            confirmed: false,
            context: None,
            cost: None,
        }));
        let reader = tokio::spawn(read_loop(
            spawned.stdout,
            rpc.clone(),
            Arc::new(SessionHandler {
                shared: shared.clone(),
            }),
        ));
        if let Some(stderr) = spawned.stderr {
            let sid = self.session_id;
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    tracing::debug!(session_id = %sid, "agent stderr: {}", preview(&l, 500));
                }
            });
        }
        let signal: Arc<SignalFn> = Arc::new(spawned.signal);
        let killing = Arc::new(AtomicBool::new(false));
        let exited = Arc::new(tokio::sync::Notify::new());
        let gone = Arc::new(AtomicBool::new(false));
        {
            let (events, killing, exited, gone, rpc, shared) = (
                self.events.clone(),
                killing.clone(),
                exited.clone(),
                gone.clone(),
                rpc.clone(),
                shared.clone(),
            );
            let exit = spawned.exit;
            tokio::spawn(async move {
                let st = exit.await;
                gone.store(true, Ordering::SeqCst);
                rpc.close();
                // Requests the person still sees are gone with the process.
                let cancels = shared.lock().unwrap().cancel_asked(CancelReason::Stopped).1;
                for e in cancels {
                    let _ = events.send(e).await;
                }
                if !killing.load(Ordering::SeqCst) {
                    let error = match st {
                        Ok(s) if s.success() => None,
                        Ok(s) => Some(s.to_string()),
                        Err(e) => Some(e.to_string()),
                    };
                    let _ = events.send(AdapterEvent::Exited { error }).await;
                }
                exited.notify_waiters();
            });
        }
        let run = Run {
            rpc: rpc.clone(),
            shared: shared.clone(),
            pid: spawned.pid,
            signal: signal.clone(),
            killing,
            exited,
            gone,
            _ssh_agent: ssh_agent,
            reader,
            session_caps: Caps::default(),
        };
        self.run = Some(run);
        match tokio::time::timeout(self.opts.setup_timeout, self.setup(spec, &agent, mode)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                let _ = self.kill().await;
                Err(e)
            }
            Err(_) => {
                let _ = self.kill().await;
                bail!(
                    "the agent {} did not set the session up in {} s",
                    agent.name,
                    self.opts.setup_timeout.as_secs()
                )
            }
        }
    }

    /// `initialize`, the session, its mode and model.
    async fn setup(
        &mut self,
        spec: &StartSpec,
        agent: &AcpAgent,
        mode: PermissionMode,
    ) -> anyhow::Result<()> {
        let run = self.run.as_mut().expect("launched");
        let rpc = run.rpc.clone();
        let shared = run.shared.clone();
        let init = rpc
            .request("initialize", initialize_params())
            .await
            .map_err(|e| anyhow::anyhow!("initialize: {e}"))?;
        let caps = Caps::of(&init);
        run.session_caps.close = caps.close;
        let cwd = spec.workdir.display().to_string();
        let servers = mcp_servers(self.opts.mcp.as_ref(), spec);
        let signed_out = |e: &RpcError| e.auth_required();
        let mut continued = None;
        if let Some(id) = &spec.runtime_session_id {
            let params = json!({"sessionId": id, "cwd": cwd, "mcpServers": servers});
            let r = if caps.resume {
                Some(rpc.request("session/resume", params).await)
            } else if caps.load_session {
                shared.lock().unwrap().replaying = true;
                let r = rpc.request("session/load", params).await;
                shared.lock().unwrap().replaying = false;
                Some(r)
            } else {
                None
            };
            match r {
                Some(Ok(v)) => continued = Some((id.clone(), v)),
                Some(Err(e)) if signed_out(&e) => {
                    self.signed_out_event().await;
                    bail!("the agent {} is not signed in on this machine", agent.name);
                }
                Some(Err(e)) => {
                    tracing::warn!(session_id = %self.session_id, "the agent could not continue session {id}: {e}");
                }
                None => {}
            }
            if continued.is_none() {
                let text = format!(
                    "The agent {} could not continue the earlier conversation: it starts a new one",
                    agent.name
                );
                let ev = shared.lock().unwrap().system(CONVERSATION_LOST, text);
                let _ = self.events.send(ev).await;
            }
        }
        let (session, v) = match continued {
            Some(c) => c,
            None => {
                let v = match rpc
                    .request("session/new", json!({"cwd": cwd, "mcpServers": servers}))
                    .await
                {
                    Ok(v) => v,
                    Err(e) if signed_out(&e) => {
                        self.signed_out_event().await;
                        bail!("the agent {} is not signed in on this machine", agent.name);
                    }
                    Err(e) => bail!("session/new: {e}"),
                };
                let id = v["sessionId"]
                    .as_str()
                    .context("session/new answered without sessionId")?
                    .to_string();
                let _ = self
                    .events
                    .send(AdapterEvent::RuntimeSessionId(id.clone()))
                    .await;
                (id, v)
            }
        };
        {
            let mut s = shared.lock().unwrap();
            s.acp_session = Some(session.clone());
            s.mode_ids = v["modes"]["availableModes"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|m| m["id"].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            s.config_options = v["configOptions"].as_array().cloned().unwrap_or_default();
            // The mode the agent is in now (a resumed session may restore its own), until hqc
            // sets one below.
            let current = v["modes"]["currentModeId"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| mode_option_value(&s.config_options));
            if let Some(m) = current.and_then(|id| s.hqc_mode(&id)) {
                s.mode = m;
            }
            s.confirmed = false;
        }
        self.start_mode(mode).await?;
        if let Some(m) = &spec.model {
            if let Err(e) = self.apply_model(m).await {
                tracing::warn!(session_id = %self.session_id, model = %m, "model not set: {e:#}");
            }
        }
        Ok(())
    }

    async fn signed_out_event(&self) {
        if let Some(run) = &self.run {
            let ev = run.shared.lock().unwrap().signed_out();
            let _ = self.events.send(ev).await;
        }
    }

    /// Sets the agent's own id of `mode` (its mode, else a config option of category `mode`
    /// offering that value). `Ok(true)`: the agent took it; `Ok(false)`: the table names no id
    /// for `mode`, nothing was sent; `Err`: the agent refused it or offers no such mode.
    async fn send_mode(&self, mode: PermissionMode) -> Result<bool, RpcError> {
        let Some(run) = &self.run else {
            return Ok(false);
        };
        let (agent, session, mode_ids, options) = {
            let s = run.shared.lock().unwrap();
            (
                s.agent.clone(),
                s.acp_session.clone().unwrap_or_default(),
                s.mode_ids.clone(),
                s.config_options.clone(),
            )
        };
        let Some(id) = agent.modes.id(mode) else {
            return Ok(false);
        };
        if mode_ids.iter().any(|m| m == id) {
            run.rpc
                .request(
                    "session/set_mode",
                    json!({"sessionId": session, "modeId": id}),
                )
                .await?;
        } else if let Some(opt) = config_option_with(&options, "mode", id) {
            run.rpc
                .request(
                    "session/set_config_option",
                    json!({"sessionId": session, "configId": opt, "value": id}),
                )
                .await?;
        } else {
            return Err(RpcError {
                code: 0,
                message: format!("the agent offers no mode {id}"),
            });
        }
        Ok(true)
    }

    /// The agent took `mode`: kept, confirmed and reported.
    async fn reached(&self, mode: PermissionMode) {
        if let Some(run) = &self.run {
            let mut s = run.shared.lock().unwrap();
            s.mode = mode;
            s.confirmed = true;
        }
        let _ = self.events.send(AdapterEvent::PermissionMode(mode)).await;
    }

    /// At the start: `mode` (already what the agent's table can run, [`AcpAgent::mode_for`]).
    /// Allow all by `allow_all_args` is reached by the start itself. An agent whose table names
    /// no mode is never switched and runs its own default, taken as Ask. When the agent refuses
    /// `mode`, Ask is set instead; when it refuses that too, the start fails: the worker reports
    /// only a mode it reached (SPEC §8.2).
    async fn start_mode(&mut self, mode: PermissionMode) -> anyhow::Result<()> {
        let agent = match &self.run {
            Some(run) => run.shared.lock().unwrap().agent.clone(),
            None => return Ok(()),
        };
        if mode == PermissionMode::AllowAll
            && agent.modes.allow_all.is_none()
            && !agent.allow_all_args.is_empty()
        {
            self.reached(mode).await;
            return Ok(());
        }
        match self.send_mode(mode).await {
            Ok(_) if mode == PermissionMode::Ask => {
                self.reached(PermissionMode::Ask).await;
                Ok(())
            }
            Ok(true) => {
                self.reached(mode).await;
                Ok(())
            }
            Ok(false) | Err(_) => {
                tracing::warn!(session_id = %self.session_id, agent = %agent.name, ?mode, "mode not set: trying Ask");
                match self.send_mode(PermissionMode::Ask).await {
                    Ok(true) => {
                        self.reached(PermissionMode::Ask).await;
                        Ok(())
                    }
                    Ok(false) => bail!("the agent {} has no Ask mode to fall back to", agent.name),
                    Err(e) => bail!("the agent {} refused its Ask mode: {e}", agent.name),
                }
            }
        }
    }

    async fn apply_model(&mut self, model: &str) -> anyhow::Result<()> {
        let run = self.run.as_ref().context("no process")?;
        let (session, options) = {
            let s = run.shared.lock().unwrap();
            (
                s.acp_session.clone().unwrap_or_default(),
                s.config_options.clone(),
            )
        };
        let opt = config_option_with(&options, "model", model)
            .with_context(|| format!("the agent offers no model {model}"))?;
        run.rpc
            .request(
                "session/set_config_option",
                json!({"sessionId": session, "configId": opt, "value": model}),
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(())
    }
}

/// The current value of the agent's config option of category `mode`, if it has one.
fn mode_option_value(options: &[Value]) -> Option<String> {
    options
        .iter()
        .find(|o| o["category"] == "mode")
        .and_then(|o| o["currentValue"].as_str().map(str::to_owned))
}

/// The id of the config option of `category` that offers `value`, flat or in groups.
fn config_option_with(options: &[Value], category: &str, value: &str) -> Option<String> {
    options.iter().find_map(|o| {
        if o["category"].as_str() != Some(category) {
            return None;
        }
        let offers = |list: &Value| {
            list.as_array().is_some_and(|l| {
                l.iter().any(|x| {
                    x["value"].as_str() == Some(value)
                        || x["options"]
                            .as_array()
                            .is_some_and(|g| g.iter().any(|y| y["value"].as_str() == Some(value)))
                })
            })
        };
        offers(&o["options"]).then(|| o["id"].as_str().map(str::to_owned))?
    })
}

#[async_trait]
impl Adapter for AcpAdapter {
    async fn start(&mut self, spec: &StartSpec) -> anyhow::Result<()> {
        self.launch(spec).await
    }

    async fn resume(&mut self, spec: &StartSpec) -> anyhow::Result<()> {
        self.launch(spec).await
    }

    async fn input(&mut self, input: &SessionInput) -> anyhow::Result<()> {
        let run = self.run.as_ref().context("no agent process")?;
        let session = {
            let mut s = run.shared.lock().unwrap();
            s.turn = Some(Turn {
                input: input.event_id,
                text: String::new(),
                thinking: String::new(),
                tools: HashMap::new(),
            });
            s.acp_session.clone().context("no session")?
        };
        let rx = match run
            .rpc
            .send(
                "session/prompt",
                json!({
                    "sessionId": session,
                    "prompt": [{"type": "text", "text": input.text}],
                }),
            )
            .await
        {
            Ok(rx) => rx,
            Err(e) => {
                run.shared.lock().unwrap().turn = None;
                bail!("session/prompt: {e}");
            }
        };
        let _ = self
            .events
            .send(AdapterEvent::InputAccepted {
                event_id: input.event_id,
                joined: false,
            })
            .await;
        let (shared, events, gone) = (run.shared.clone(), self.events.clone(), run.gone.clone());
        tokio::spawn(async move {
            let r = rx.await.unwrap_or_else(|_| Err(RpcError::closed()));
            // A process that died ends the turn through `Exited`.
            if r.as_ref().err() == Some(&RpcError::closed()) && gone.load(Ordering::SeqCst) {
                shared.lock().unwrap().turn = None;
                return;
            }
            let out = shared.lock().unwrap().end_turn(r);
            for e in out {
                let _ = events.send(e).await;
            }
        });
        Ok(())
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        let Some(run) = &self.run else {
            return Ok(());
        };
        let (session, in_turn, ids, cancels) = {
            let mut s = run.shared.lock().unwrap();
            let (ids, cancels) = s.cancel_asked(CancelReason::Stopped);
            (s.acp_session.clone(), s.turn.is_some(), ids, cancels)
        };
        for id in ids {
            run.rpc
                .respond(id, json!({"outcome": {"outcome": "cancelled"}}))
                .await;
        }
        for e in cancels {
            let _ = self.events.send(e).await;
        }
        if let Some(session) = session {
            let _ = run
                .rpc
                .notify("session/cancel", json!({"sessionId": session}))
                .await;
        }
        if in_turn {
            // An agent that does not end its turn is ended.
            let (shared, signal, grace, kill_grace) = (
                run.shared.clone(),
                run.signal.clone(),
                self.opts.stop_grace,
                self.opts.kill_grace,
            );
            tokio::spawn(async move {
                tokio::time::sleep(grace).await;
                if shared.lock().unwrap().turn.is_some() {
                    tracing::warn!(
                        "the ACP agent did not end its turn after session/cancel: ended"
                    );
                    let _ = signal(Sig::Terminate);
                    tokio::time::sleep(kill_grace).await;
                    let _ = signal(Sig::Kill);
                }
            });
        }
        Ok(())
    }

    async fn kill(&mut self) -> anyhow::Result<()> {
        let Some(run) = self.run.take() else {
            return Ok(());
        };
        run.killing.store(true, Ordering::SeqCst);
        if !run.gone.load(Ordering::SeqCst) {
            let notified = run.exited.notified();
            let _ = (run.signal)(Sig::Terminate);
            if tokio::time::timeout(self.opts.kill_grace, notified)
                .await
                .is_err()
            {
                let _ = (run.signal)(Sig::Kill);
                let _ = tokio::time::timeout(self.opts.kill_grace, run.exited.notified()).await;
            }
        }
        run.reader.abort();
        run.rpc.close();
        Ok(())
    }

    async fn close(&mut self) -> anyhow::Result<()> {
        if let Some(run) = &self.run {
            let session = run.shared.lock().unwrap().acp_session.clone();
            if let (true, Some(session)) = (run.session_caps.close, session) {
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    run.rpc
                        .request("session/close", json!({"sessionId": session})),
                )
                .await;
            }
        }
        self.kill().await
    }

    fn pid(&self) -> Option<u32> {
        self.run.as_ref().and_then(|r| r.pid)
    }

    async fn set_permission_mode(&mut self, mode: PermissionMode) -> anyhow::Result<()> {
        let Some(run) = &self.run else {
            return Ok(());
        };
        let (agent, now, confirmed) = {
            let s = run.shared.lock().unwrap();
            (s.agent.clone(), s.mode, s.confirmed)
        };
        let target = agent.mode_for(mode);
        // A process started with `allow_all_args` leaves Allow all only by a restart: no mode
        // set later is known to undo the flag.
        if now == PermissionMode::AllowAll
            && target != now
            && agent.modes.allow_all.is_none()
            && !agent.allow_all_args.is_empty()
        {
            bail!(
                "the agent {} leaves Allow all only by a restart",
                agent.name
            );
        }
        if target == now && confirmed {
            self.reached(now).await;
            return Ok(());
        }
        // Only a mode the agent took is reported; anything else is an error, and the
        // supervisor starts the process again in the mode asked for (T39, SPEC §8.2).
        match self.send_mode(target).await {
            Ok(true) => {
                self.reached(target).await;
                Ok(())
            }
            Ok(false) => bail!(
                "the agent {} cannot switch to {} while it runs",
                agent.name,
                target.as_str()
            ),
            Err(e) => bail!("the agent {} refused {}: {e}", agent.name, target.as_str()),
        }
    }

    async fn set_model(&mut self, model: &str) -> anyhow::Result<()> {
        if self.run.is_none() {
            return Ok(());
        }
        self.apply_model(model).await
    }

    async fn answer_approval(
        &mut self,
        request_id: &str,
        option_id: &str,
        _message: Option<&str>,
    ) -> anyhow::Result<()> {
        let run = self.run.as_ref().context("no agent process")?;
        let asked = run.shared.lock().unwrap().asked.remove(request_id);
        let Some(asked) = asked else {
            tracing::info!(session_id = %self.session_id, request_id, "approval answer for no open request");
            return Ok(());
        };
        let answer = if asked.options.iter().any(|o| o["optionId"] == option_id) {
            json!({"outcome": {"outcome": "selected", "optionId": option_id}})
        } else {
            // Not one of the agent's options (the hub's own deny): the agent's first reject,
            // else a cancel.
            match asked
                .options
                .iter()
                .find(|o| o["kind"] == "reject_once" || o["kind"] == "reject_always")
            {
                Some(o) if option_id.starts_with("deny") || option_id == "reject" => {
                    json!({"outcome": {"outcome": "selected", "optionId": o["optionId"]}})
                }
                _ => json!({"outcome": {"outcome": "cancelled"}}),
            }
        };
        run.rpc.respond(asked.rpc_id, answer).await;
        Ok(())
    }

    async fn answer_clarify(
        &mut self,
        _request_id: &str,
        _answers: &BTreeMap<String, ClarifyAnswer>,
        _action: Option<ClarifyAction>,
    ) -> anyhow::Result<()> {
        // No elicitation is offered to ACP agents.
        Ok(())
    }
}

impl AdapterFactory for AcpAdapters {
    fn supports(&self, runtime: Runtime) -> bool {
        AcpAdapters::supports(self, runtime)
    }

    fn create(&self, spec: &StartSpec, events: EventSink) -> Box<dyn Adapter> {
        AcpAdapters::create(self, spec, events)
    }

    fn mcp_program(&self) -> Option<PathBuf> {
        self.opts.mcp.as_ref().map(|m| m.program.clone())
    }
}

#[cfg(test)]
mod tests;
