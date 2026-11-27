//! OpenAI's Codex through its App Server (SPEC §8.6, iteration 10): `codex app-server`, one
//! process per session, JSON-RPC over stdio (no `"jsonrpc"` field), read as plain JSON.
//!
//! The protocol is experimental and changes between Codex releases: the worker supports
//! [`SUPPORTED`] only, checked against `codex --version` at its start (the schema of that
//! version: `docs/notes/codex-app-server.md`).
//!
//! - `start`/`resume`: the process in the session's working folder with its environment (§14),
//!   `initialize` and `initialized`, `account/read` (signed in on the machine?), then
//!   `thread/resume` of the session's `runtime_session_id` or `thread/start`, with the mode's
//!   approval policy and sandbox, the model and hqc's MCP servers in `config.mcp_servers`.
//! - `input`: `turn/start` (with the approval policy and sandbox policy only when they changed,
//!   which keeps the user's `network_access`); `turn/completed` ends it. An input written during
//!   a turn goes in by `turn/steer`.
//! - `stop`: `turn/interrupt`; a process that does not end its turn is ended.
//! - Approvals: the worker's `deny` and allow rules on a command's explicit text, else a card
//!   with Codex's own decisions.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context};
use async_trait::async_trait;
use hqc_proto::event::{
    AgentTask, ApprovalCanonical, ApprovalOption, Author, CancelReason, ClarifyAction,
    ClarifyAnswer, ClarifyQuestion, EventKind, SessionState, TaskStatus,
};
use hqc_proto::frame::{PermissionMode, Runtime, SessionInput};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use uuid::Uuid;

use super::acp::{AGENT_SIGNED_OUT, CONVERSATION_LOST, POLICY_DENIED};
use super::claude_code::engine::{preview, PREVIEW_LIMIT};
use super::claude_code::process::{CommandSpec, OsSpawner, Sig, SignalFn, Spawner};
use super::claude_code::McpLaunch;
use super::jsonrpc::{read_loop, Handler, NoHandler, Rpc, RpcError, METHOD_NOT_FOUND};
use super::{new_event, Adapter, AdapterEvent, AdapterFactory, EventSink, ResultFiles};
use crate::config::{CodexConfig, Config, StartSpec};
use crate::permissions::Policy;

/// The Codex versions this worker speaks the App Server protocol of: `[from, to)`.
pub const SUPPORTED: ((u32, u32, u32), (u32, u32, u32)) = ((0, 172, 0), (0, 173, 0));

/// Whether `version` (`0.172.0`) is one [`SUPPORTED`].
pub fn supported(version: &str) -> bool {
    let n: Vec<u32> = version.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    let v = (
        *n.first().unwrap_or(&0),
        *n.get(1).unwrap_or(&0),
        *n.get(2).unwrap_or(&0),
    );
    v >= SUPPORTED.0 && v < SUPPORTED.1
}

/// Codex's approval policy and sandbox mode of hqc's mode (SPEC §8.6).
fn policy_of(mode: PermissionMode) -> (&'static str, &'static str, Value) {
    match mode {
        PermissionMode::Ask => ("on-request", "read-only", json!({"type": "readOnly"})),
        PermissionMode::Auto => (
            "on-request",
            "workspace-write",
            json!({"type": "workspaceWrite"}),
        ),
        PermissionMode::AllowAll => (
            "never",
            "danger-full-access",
            json!({"type": "dangerFullAccess"}),
        ),
    }
}

#[derive(Clone)]
pub struct CodexOptions {
    pub codex: Option<Arc<CodexConfig>>,
    pub mcp: Option<McpLaunch>,
    pub ssh: crate::ssh::Store,
    pub setup_timeout: Duration,
    /// How long `turn/steer` and `turn/interrupt` may go unanswered before the process is
    /// taken for wedged and ended (T3 Code's lesson).
    pub answer_timeout: Duration,
    pub stop_grace: Duration,
    pub kill_grace: Duration,
}

impl CodexOptions {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            codex: cfg.codex.clone(),
            mcp: None,
            ssh: cfg.ssh.clone(),
            setup_timeout: Duration::from_secs(60),
            answer_timeout: Duration::from_secs(10),
            stop_grace: Duration::from_secs(10),
            kill_grace: Duration::from_secs(5),
        }
    }
}

#[derive(Clone)]
pub struct CodexAdapters {
    opts: Arc<CodexOptions>,
    spawner: Arc<dyn Spawner>,
}

impl CodexAdapters {
    pub fn new(opts: CodexOptions, spawner: Arc<dyn Spawner>) -> Self {
        Self {
            opts: Arc::new(opts),
            spawner,
        }
    }

    pub fn from_config(cfg: &Config) -> Self {
        Self::new(CodexOptions::from_config(cfg), Arc::new(OsSpawner))
    }

    pub fn with_mcp(mut self, mcp: McpLaunch) -> Self {
        Arc::make_mut(&mut self.opts).mcp = Some(mcp);
        self
    }
}

impl AdapterFactory for CodexAdapters {
    fn supports(&self, runtime: Runtime) -> bool {
        runtime == Runtime::Codex && self.opts.codex.is_some()
    }

    fn create(&self, spec: &StartSpec, events: EventSink) -> Box<dyn Adapter> {
        Box::new(CodexAdapter {
            session_id: spec.session_id,
            opts: self.opts.clone(),
            spawner: self.spawner.clone(),
            events,
            run: None,
        })
    }

    fn mcp_program(&self) -> Option<PathBuf> {
        self.opts.mcp.as_ref().map(|m| m.program.clone())
    }
}

/// What the worker learns of Codex at its start, for `hello` (SPEC §5.4).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Probe {
    pub version: String,
    pub signed_in: bool,
    /// `model/list`: `{id, name, default, efforts}` each.
    pub models: Vec<Value>,
}

/// `codex --version`, then a short-lived App Server for `account/read` and `model/list`.
/// `Err`: no binary, a version not [`SUPPORTED`], or no answer.
pub async fn probe(codex: &CodexConfig, cwd: PathBuf, timeout: Duration) -> anyhow::Result<Probe> {
    let out = tokio::time::timeout(
        timeout,
        tokio::process::Command::new(&codex.command)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("`codex --version` timed out")?
    .with_context(|| format!("running {}", codex.command.display()))?;
    let version = crate::capabilities::parse_version(&String::from_utf8_lossy(&out.stdout))
        .context("unrecognised `codex --version` output")?;
    if !supported(&version) {
        bail!(
            "Codex {version} is not one this worker speaks the App Server of ({}.{}.x); install that version",
            SUPPORTED.0 .0,
            SUPPORTED.0 .1
        );
    }
    let env = crate::adapter::claude_code::child_env(std::env::vars());
    let spawned = OsSpawner.spawn(&CommandSpec {
        program: codex.command.clone(),
        args: vec!["app-server".into()],
        cwd,
        env,
    })?;
    let signal = spawned.signal;
    let rpc = Rpc::new(spawned.stdin, false);
    let reader = tokio::spawn(read_loop(spawned.stdout, rpc.clone(), Arc::new(NoHandler)));
    let asked = async {
        rpc.request("initialize", initialize_params()).await?;
        rpc.notify("initialized", json!({})).await?;
        let account = rpc.request("account/read", json!({})).await?;
        let models = rpc.request("model/list", json!({})).await?;
        Ok::<_, RpcError>((account, models))
    };
    let r = tokio::time::timeout(timeout, asked).await;
    let _ = signal(Sig::Terminate);
    reader.abort();
    let (account, models) = r
        .context("the App Server did not answer")?
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(Probe {
        version,
        signed_in: signed_in(&account),
        models: model_list(&models),
    })
}

fn initialize_params() -> Value {
    json!({"clientInfo": {"name": "hqc", "version": env!("CARGO_PKG_VERSION")}})
}

/// `account/read`: signed in unless Codex needs OpenAI's sign-in and has no account.
fn signed_in(account: &Value) -> bool {
    !(account["requiresOpenaiAuth"] == true && account["account"].is_null())
}

/// `model/list` as `hello` gives it to the client: the visible models.
fn model_list(models: &Value) -> Vec<Value> {
    models["data"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|m| m["hidden"] != true)
        .filter_map(|m| {
            Some(json!({
                "id": m["model"].as_str().or(m["id"].as_str())?,
                "name": m["displayName"].as_str().unwrap_or(""),
                "default": m["isDefault"] == true,
                "efforts": m["supportedReasoningEfforts"]
                    .as_array()
                    .map(|e| e.iter().filter_map(|x| x["reasoningEffort"].as_str().map(str::to_owned)).collect::<Vec<_>>())
                    .unwrap_or_default(),
            }))
        })
        .collect()
}

/// A request Codex waits on the person for.
enum AskedKind {
    /// A command or file change: answered with one of Codex's decisions.
    Approval,
    /// `item/tool/requestUserInput`.
    Input,
}

struct Asked {
    rpc_id: Value,
    kind: AskedKind,
}

struct Turn {
    input: Uuid,
    /// Codex's id of the turn, from `turn/start`'s answer or `turn/started`.
    id: Option<String>,
    text: String,
    thinking: String,
    tools: HashMap<String, std::time::Instant>,
    /// The changes of each file-change item, for its approval card.
    changes: HashMap<String, Value>,
}

struct Shared {
    session_id: Uuid,
    events: EventSink,
    policy: Policy,
    folder_rules: Option<crate::permissions::FolderRules>,
    workspace: PathBuf,
    thread: Option<String>,
    turn: Option<Turn>,
    asked: BTreeMap<String, Asked>,
    usage: Value,
    /// The mode the next `turn/start` runs in, and the one the last turn ran in.
    mode: PermissionMode,
    sent_mode: Option<PermissionMode>,
    model: Option<String>,
    sent_model: Option<String>,
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
            "Codex is not signed in on this machine: sign it in there with `codex login --device-auth` (RUNBOOK)".into(),
        )
    }

    fn flush_thinking(&mut self, out: &mut Vec<AdapterEvent>) {
        let Some(t) = self.turn.as_mut() else {
            return;
        };
        let text = std::mem::take(&mut t.thinking);
        if !text.trim().is_empty() {
            out.push(self.event(EventKind::AgentThinking { text }));
        }
    }

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

    fn tool_call(&mut self, id: &str, tool: String, args: String, out: &mut Vec<AdapterEvent>) {
        self.flush_text(out);
        if let Some(t) = self.turn.as_mut() {
            t.tools.insert(id.to_string(), std::time::Instant::now());
        }
        out.push(self.event(EventKind::ToolCall {
            call_id: id.to_string(),
            tool,
            args_preview: preview(&args, PREVIEW_LIMIT),
        }));
    }

    fn tool_result(&mut self, id: &str, error: bool, text: String) -> AdapterEvent {
        let started = self.turn.as_mut().and_then(|t| t.tools.remove(id));
        let full = (text.len() > PREVIEW_LIMIT).then(|| preview(&text, super::FULL_RESULT_LIMIT));
        AdapterEvent::ToolResult {
            event: new_event(
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
            ),
            files: ResultFiles {
                text: full,
                images: vec![],
            },
        }
    }

    fn notification(&mut self, method: &str, p: &Value) -> Vec<AdapterEvent> {
        let mut out = Vec::new();
        if p["threadId"]
            .as_str()
            .is_some_and(|t| Some(t) != self.thread.as_deref())
        {
            // A subagent's thread, or another one the process holds: not this session's.
            return out;
        }
        match method {
            "turn/started" => {
                if let (Some(t), Some(id)) = (self.turn.as_mut(), p["turn"]["id"].as_str()) {
                    t.id = Some(id.to_string());
                }
            }
            "item/agentMessage/delta" => {
                if let Some(text) = p["delta"].as_str().map(str::to_owned) {
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
            "item/reasoning/summaryTextDelta" => {
                if let Some(text) = p["delta"].as_str().map(str::to_owned) {
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
            "item/started" => self.item_started(&p["item"], &mut out),
            "item/completed" => self.item_completed(&p["item"], &mut out),
            "turn/plan/updated" => {
                let items = p["plan"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .map(|s| AgentTask {
                        subject: preview(s["step"].as_str().unwrap_or(""), PREVIEW_LIMIT),
                        status: match s["status"].as_str() {
                            Some("pending") => TaskStatus::Pending,
                            Some("inProgress") => TaskStatus::InProgress,
                            Some("completed") => TaskStatus::Completed,
                            _ => TaskStatus::Unknown,
                        },
                        active_form: None,
                    })
                    .collect();
                out.push(self.event(EventKind::Tasks { items }));
            }
            "thread/tokenUsage/updated" => self.usage = p["tokenUsage"].clone(),
            "thread/name/updated" => {
                if let Some(t) = p["threadName"].as_str() {
                    let t = preview(t.lines().next().unwrap_or("").trim(), 200);
                    if !t.is_empty() {
                        out.push(AdapterEvent::Title(t));
                    }
                }
            }
            "serverRequest/resolved" => {
                // Codex settled a request itself (an interrupt, its reviewer): the card goes.
                let rid = &p["requestId"];
                let ids: Vec<String> = self
                    .asked
                    .iter()
                    .filter(|(_, a)| &a.rpc_id == rid)
                    .map(|(k, _)| k.clone())
                    .collect();
                for request_id in ids {
                    if let Some(a) = self.asked.remove(&request_id) {
                        out.push(self.cancel_event(request_id, &a, CancelReason::Withdrawn));
                    }
                }
            }
            "turn/completed" => out.extend(self.end_turn(&p["turn"])),
            _ => {}
        }
        out
    }

    fn item_started(&mut self, item: &Value, out: &mut Vec<AdapterEvent>) {
        let Some(id) = item["id"].as_str() else {
            return;
        };
        match item["type"].as_str() {
            Some("commandExecution") => {
                let cmd = item["command"].as_str().unwrap_or("").to_string();
                self.tool_call(id, "Bash".into(), cmd, out)
            }
            Some("fileChange") => {
                if let Some(t) = self.turn.as_mut() {
                    t.changes.insert(id.to_string(), item["changes"].clone());
                }
                let paths = change_paths(&item["changes"]);
                self.tool_call(id, "Edit".into(), paths, out)
            }
            Some("mcpToolCall") => {
                let tool = format!(
                    "mcp__{}__{}",
                    item["server"].as_str().unwrap_or(""),
                    item["tool"].as_str().unwrap_or("")
                );
                let args = item["arguments"].to_string();
                self.tool_call(id, tool, args, out)
            }
            Some("webSearch") => {
                let q = item["query"].as_str().unwrap_or("").to_string();
                self.tool_call(id, "WebSearch".into(), q, out)
            }
            _ => {}
        }
    }

    fn item_completed(&mut self, item: &Value, out: &mut Vec<AdapterEvent>) {
        let Some(id) = item["id"].as_str() else {
            return;
        };
        let status = item["status"].as_str().unwrap_or("completed");
        let failed = matches!(status, "failed" | "declined");
        match item["type"].as_str() {
            Some("reasoning") => self.flush_thinking(out),
            Some("commandExecution") => {
                let mut text = item["aggregatedOutput"].as_str().unwrap_or("").to_string();
                if let Some(code) = item["exitCode"].as_i64().filter(|c| *c != 0) {
                    text.push_str(&format!("\n(exit code {code})"));
                }
                if status == "declined" {
                    text = "Declined".into();
                }
                let error = failed || item["exitCode"].as_i64().is_some_and(|c| c != 0);
                out.push(self.tool_result(id, error, text));
            }
            Some("fileChange") => {
                let text = item["changes"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .map(|c| {
                        format!(
                            "{}\n{}",
                            c["path"].as_str().unwrap_or(""),
                            c["diff"].as_str().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if let Some(t) = self.turn.as_mut() {
                    t.changes.remove(id);
                }
                out.push(self.tool_result(id, failed, text));
            }
            Some("mcpToolCall") => {
                let text = match item["error"]["message"].as_str() {
                    Some(e) => e.to_string(),
                    None => item["result"]["content"]
                        .as_array()
                        .map(Vec::as_slice)
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|c| c["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                let error = failed || !item["error"].is_null();
                out.push(self.tool_result(id, error, text));
            }
            Some("webSearch") => out.push(self.tool_result(id, false, String::new())),
            _ => {}
        }
    }

    /// `turn/completed`: the final message, usage, `status idle`, the end.
    fn end_turn(&mut self, t: &Value) -> Vec<AdapterEvent> {
        let mut out = Vec::new();
        self.flush_thinking(&mut out);
        let Some(turn) = self.turn.take() else {
            return out;
        };
        let status = t["status"].as_str().unwrap_or("completed");
        match status {
            "interrupted" => {
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
            "failed" => {
                if t["error"]["codexErrorInfo"] == "unauthorized" {
                    out.push(self.signed_out());
                } else {
                    out.push(self.event(EventKind::AgentMessage {
                        text: format!(
                            "Codex failed the turn: {}",
                            t["error"]["message"].as_str().unwrap_or("an error")
                        ),
                        r#final: true,
                        in_reply_to: Some(turn.input),
                        runtime: true,
                        interrupted: false,
                        refusal: false,
                    }));
                }
            }
            _ => out.push(self.event(EventKind::AgentMessage {
                text: turn.text.clone(),
                r#final: true,
                in_reply_to: Some(turn.input),
                runtime: false,
                interrupted: false,
                refusal: false,
            })),
        }
        let last = &self.usage["last"];
        if last.is_object() {
            let n = |v: &Value, k: &str| v[k].as_u64();
            out.push(self.event(EventKind::Usage {
                input_tokens: n(last, "inputTokens").unwrap_or(0),
                output_tokens: n(last, "outputTokens").unwrap_or(0),
                cache_read: n(last, "cachedInputTokens"),
                cache_write: n(last, "cacheWriteInputTokens"),
                cost_usd: None,
                session_cost_usd: None,
                session_tokens: n(&self.usage["total"], "totalTokens"),
                model: self.sent_model.clone(),
                context_tokens: n(last, "totalTokens"),
                context_limit: self.usage["modelContextWindow"].as_u64(),
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

    fn cancel_event(&self, request_id: String, a: &Asked, reason: CancelReason) -> AdapterEvent {
        match a.kind {
            AskedKind::Approval => self.event(EventKind::ApprovalCancel { request_id, reason }),
            AskedKind::Input => self.event(EventKind::ClarifyCancel { request_id, reason }),
        }
    }

    /// The open requests closed: Codex's ids with the answer each takes, and the events.
    fn cancel_asked(&mut self, reason: CancelReason) -> (Vec<(Value, Value)>, Vec<AdapterEvent>) {
        let asked = std::mem::take(&mut self.asked);
        let mut answers = Vec::new();
        let mut out = Vec::new();
        for (request_id, a) in asked {
            let answer = match a.kind {
                AskedKind::Approval => json!({"decision": "cancel"}),
                AskedKind::Input => json!({"answers": {}}),
            };
            out.push(self.cancel_event(request_id, &a, reason));
            answers.push((a.rpc_id, answer));
        }
        (answers, out)
    }

    /// `item/commandExecution/requestApproval` and `item/fileChange/requestApproval`.
    fn approval(
        &mut self,
        rpc_id: Value,
        method: &str,
        p: &Value,
    ) -> (Option<Value>, Vec<AdapterEvent>) {
        let mut out = Vec::new();
        let command = (method == "item/commandExecution/requestApproval")
            .then(|| p["command"].as_str().map(str::to_owned))
            .flatten();
        if let Some(cmd) = &command {
            let input = json!({ "command": cmd });
            if let Some(rule) = self.policy.denied_by("Bash", &input) {
                out.push(self.system(
                    POLICY_DENIED,
                    format!("Denied by the worker's rule {rule}: {}", preview(cmd, 200)),
                ));
                return (Some(json!({"decision": "decline"})), out);
            }
            // Allowed by a rule only in the session's own folder (SPEC §8.6).
            let here = p["cwd"]
                .as_str()
                .is_none_or(|c| std::path::Path::new(c).starts_with(&self.workspace));
            let allowed = here
                && (self.policy.allowed_by("Bash", &input).is_some()
                    || self.folder_rules.as_ref().is_some_and(|f| {
                        self.policy
                            .folder_allows(f, &self.workspace, "Bash", &input)
                            .is_some()
                    }));
            if allowed {
                return (Some(json!({"decision": "accept"})), out);
            }
        }
        let (tool, description, args) = match &command {
            Some(c) => (
                "Bash".to_string(),
                p["reason"].as_str().unwrap_or("Run a command").to_string(),
                c.clone(),
            ),
            None if method == "item/fileChange/requestApproval" => {
                let changes = self
                    .turn
                    .as_ref()
                    .and_then(|t| p["itemId"].as_str().and_then(|i| t.changes.get(i)))
                    .cloned()
                    .unwrap_or(Value::Null);
                let diffs = changes
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .map(|c| {
                        format!(
                            "{}\n{}",
                            c["path"].as_str().unwrap_or(""),
                            c["diff"].as_str().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (
                    "Edit".to_string(),
                    p["reason"].as_str().unwrap_or("Change files").to_string(),
                    diffs,
                )
            }
            None => (
                "Bash".to_string(),
                p["reason"].as_str().unwrap_or("Run a command").to_string(),
                String::new(),
            ),
        };
        let request_id = Uuid::now_v7().to_string();
        let opt = |id: &str, label: &str, canonical| ApprovalOption {
            id: id.into(),
            label: label.into(),
            canonical,
            grant: None,
        };
        out.push(self.event(EventKind::ApprovalRequest {
            request_id: request_id.clone(),
            tool,
            description: preview(&description, PREVIEW_LIMIT),
            args_preview: Some(preview(&args, PREVIEW_LIMIT)),
            options: vec![
                opt("accept", "Allow", ApprovalCanonical::Once),
                opt(
                    "acceptForSession",
                    "Allow for this session",
                    ApprovalCanonical::Session,
                ),
                opt("decline", "Deny", ApprovalCanonical::Deny),
                opt("cancel", "Deny and stop", ApprovalCanonical::Deny),
            ],
            expires_at: None,
            policy_hint: None,
            rule_suggestions: None,
            secret: None,
            reason: None,
            display_name: None,
            human_only: false,
            subagent: None,
        }));
        self.asked.insert(
            request_id,
            Asked {
                rpc_id,
                kind: AskedKind::Approval,
            },
        );
        (None, out)
    }

    /// `item/tool/requestUserInput`: Codex's questions to the person.
    fn user_input(&mut self, rpc_id: Value, p: &Value) -> Vec<AdapterEvent> {
        let questions = p["questions"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|q| {
                let opts = q["options"].as_array();
                Some(ClarifyQuestion {
                    id: q["id"].as_str()?.to_string(),
                    text: q["question"].as_str().unwrap_or("").to_string(),
                    header: q["header"].as_str().map(str::to_owned),
                    options: opts.map(|o| {
                        o.iter()
                            .map(|x| x["label"].as_str().unwrap_or("").to_string())
                            .collect()
                    }),
                    option_descriptions: opts.map(|o| {
                        o.iter()
                            .map(|x| x["description"].as_str().unwrap_or("").to_string())
                            .collect()
                    }),
                    option_previews: None,
                    multi: None,
                    required: false,
                    input: None,
                })
            })
            .collect();
        let request_id = Uuid::now_v7().to_string();
        let ev = self.event(EventKind::ClarifyRequest {
            request_id: request_id.clone(),
            questions,
            server: None,
            message: None,
            url: None,
            subagent: None,
        });
        self.asked.insert(
            request_id,
            Asked {
                rpc_id,
                kind: AskedKind::Input,
            },
        );
        vec![ev]
    }
}

fn change_paths(changes: &Value) -> String {
    changes
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|c| c["path"].as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

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
        match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                let (answer, events) =
                    self.shared
                        .lock()
                        .unwrap()
                        .approval(id.clone(), method, &params);
                self.send(events).await;
                if let Some(a) = answer {
                    rpc.respond(id, a).await;
                }
            }
            "item/tool/requestUserInput" => {
                let events = self.shared.lock().unwrap().user_input(id, &params);
                self.send(events).await;
            }
            // An MCP server's form: not offered to Codex sessions yet.
            "mcpServer/elicitation/request" => rpc.respond(id, json!({"action": "decline"})).await,
            _ => {
                rpc.respond_error(id, METHOD_NOT_FOUND, "not supported by this client")
                    .await
            }
        }
    }

    async fn notification(&self, method: &str, params: Value) {
        let events = self.shared.lock().unwrap().notification(method, &params);
        self.send(events).await;
    }
}

struct Run {
    rpc: Arc<Rpc>,
    shared: Arc<Mutex<Shared>>,
    pid: Option<u32>,
    signal: Arc<SignalFn>,
    killing: Arc<AtomicBool>,
    exited: Arc<tokio::sync::Notify>,
    gone: Arc<AtomicBool>,
    _ssh_agent: Option<crate::ssh::SshAgent>,
    reader: tokio::task::JoinHandle<()>,
}

pub struct CodexAdapter {
    session_id: Uuid,
    opts: Arc<CodexOptions>,
    spawner: Arc<dyn Spawner>,
    events: EventSink,
    run: Option<Run>,
}

impl CodexAdapter {
    async fn launch(&mut self, spec: &StartSpec) -> anyhow::Result<()> {
        if self.run.is_some() {
            return Ok(());
        }
        let codex = self.opts.codex.clone().context("codex is not configured")?;
        spec.recheck_workdir()?;
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
        let mut env = spec.environment.process_env();
        if let Some(a) = &ssh_agent {
            env.push((
                crate::ssh::SSH_AUTH_SOCK.into(),
                a.socket().display().to_string(),
            ));
        }
        let cmd = CommandSpec {
            program: codex.command.clone(),
            args: vec!["app-server".into()],
            cwd: spec.workdir.clone(),
            env,
        };
        let spawned = self
            .spawner
            .spawn(&cmd)
            .with_context(|| format!("spawning {}", cmd.program.display()))?;
        tracing::info!(session_id = %self.session_id, cwd = %spec.workdir.display(), environment = %spec.environment.name, mode = spec.permission_mode.as_str(), "Codex App Server started");
        let rpc = Rpc::new(spawned.stdin, false);
        let shared = Arc::new(Mutex::new(Shared {
            session_id: self.session_id,
            events: self.events.clone(),
            policy: Policy::new(&spec.environment.permissions),
            folder_rules: spec.folder_rules.clone(),
            workspace: spec.workspace.clone(),
            thread: None,
            turn: None,
            asked: BTreeMap::new(),
            usage: Value::Null,
            mode: spec.permission_mode,
            sent_mode: None,
            model: spec.model.clone(),
            sent_model: None,
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
                    tracing::debug!(session_id = %sid, "codex stderr: {}", preview(&l, 500));
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
        self.run = Some(Run {
            rpc,
            shared,
            pid: spawned.pid,
            signal,
            killing,
            exited,
            gone,
            _ssh_agent: ssh_agent,
            reader,
        });
        match tokio::time::timeout(self.opts.setup_timeout, self.setup(spec)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                let _ = self.kill().await;
                Err(e)
            }
            Err(_) => {
                let _ = self.kill().await;
                bail!(
                    "Codex did not set the session up in {} s",
                    self.opts.setup_timeout.as_secs()
                )
            }
        }
    }

    async fn setup(&mut self, spec: &StartSpec) -> anyhow::Result<()> {
        let run = self.run.as_ref().expect("launched");
        let (rpc, shared) = (run.rpc.clone(), run.shared.clone());
        let e = |e: RpcError| anyhow::anyhow!("{e}");
        rpc.request("initialize", initialize_params())
            .await
            .map_err(e)?;
        rpc.notify("initialized", json!({})).await.map_err(e)?;
        let account = rpc.request("account/read", json!({})).await.map_err(e)?;
        if !signed_in(&account) {
            let ev = shared.lock().unwrap().signed_out();
            let _ = self.events.send(ev).await;
            bail!("Codex is not signed in on this machine");
        }
        let mode = spec.permission_mode;
        let (approval, sandbox, _) = policy_of(mode);
        let config = json!({ "mcp_servers": mcp_servers(self.opts.mcp.as_ref(), spec) });
        let cwd = spec.workdir.display().to_string();
        let mut thread = None;
        if let Some(id) = &spec.runtime_session_id {
            let mut params = json!({"threadId": id, "cwd": cwd, "approvalPolicy": approval,
                "sandbox": sandbox, "config": config, "excludeTurns": true});
            if let Some(m) = &spec.model {
                params["model"] = json!(m);
            }
            match rpc.request("thread/resume", params).await {
                Ok(v) => thread = v["thread"]["id"].as_str().map(str::to_owned),
                Err(err) => {
                    tracing::warn!(session_id = %self.session_id, "Codex could not resume thread {id}: {err}");
                }
            }
            if thread.as_deref() != Some(id.as_str()) {
                thread = None;
                let ev = shared.lock().unwrap().system(
                    CONVERSATION_LOST,
                    "Codex could not continue the earlier conversation: it starts a new one".into(),
                );
                let _ = self.events.send(ev).await;
            }
        }
        let thread = match thread {
            Some(t) => t,
            None => {
                let mut params = json!({"cwd": cwd, "approvalPolicy": approval,
                    "sandbox": sandbox, "config": config});
                if let Some(m) = &spec.model {
                    params["model"] = json!(m);
                }
                let v = rpc.request("thread/start", params).await.map_err(e)?;
                let id = v["thread"]["id"]
                    .as_str()
                    .context("thread/start answered without a thread id")?
                    .to_string();
                let _ = self
                    .events
                    .send(AdapterEvent::RuntimeSessionId(id.clone()))
                    .await;
                id
            }
        };
        {
            let mut s = shared.lock().unwrap();
            s.thread = Some(thread);
            // The thread runs in this mode and model from its start.
            s.sent_mode = Some(mode);
            s.sent_model = spec.model.clone();
        }
        let _ = self.events.send(AdapterEvent::PermissionMode(mode)).await;
        Ok(())
    }

    /// A request that must be answered soon: no answer in time means a wedged process, which
    /// is ended (T3 Code's lesson); the session starts again on its next input.
    async fn timed(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        let run = self.run.as_ref().ok_or_else(RpcError::closed)?;
        match tokio::time::timeout(self.opts.answer_timeout, run.rpc.request(method, params)).await
        {
            Ok(r) => r,
            Err(_) => {
                tracing::warn!(session_id = %self.session_id, method, "Codex did not answer: ending the process");
                let (signal, kill_grace) = (run.signal.clone(), self.opts.kill_grace);
                let _ = signal(Sig::Terminate);
                tokio::spawn(async move {
                    tokio::time::sleep(kill_grace).await;
                    let _ = signal(Sig::Kill);
                });
                Err(RpcError::closed())
            }
        }
    }
}

/// hqc's MCP servers as Codex's `mcp_servers` config: the launchers of §8.1.
fn mcp_servers(mcp: Option<&McpLaunch>, spec: &StartSpec) -> Value {
    let Some(mcp) = mcp else {
        return json!({});
    };
    let local: Vec<String> = spec
        .local_mcp
        .as_ref()
        .map(|l| l.names())
        .unwrap_or_default();
    let config: Value =
        serde_json::from_str(&mcp.config(&spec.session_token, false, &spec.mcp_servers, &local))
            .unwrap_or_default();
    let mut out = serde_json::Map::new();
    if let Some(servers) = config["mcpServers"].as_object() {
        for (name, s) in servers {
            out.insert(
                name.clone(),
                json!({"command": s["command"], "args": s["args"], "env": s["env"]}),
            );
        }
    }
    Value::Object(out)
}

#[async_trait]
impl Adapter for CodexAdapter {
    async fn start(&mut self, spec: &StartSpec) -> anyhow::Result<()> {
        self.launch(spec).await
    }

    async fn resume(&mut self, spec: &StartSpec) -> anyhow::Result<()> {
        self.launch(spec).await
    }

    async fn input(&mut self, input: &SessionInput) -> anyhow::Result<()> {
        let run = self.run.as_ref().context("no Codex process")?;
        let params = {
            let mut s = run.shared.lock().unwrap();
            let thread = s.thread.clone().context("no thread")?;
            let mut params = json!({"threadId": thread,
                "input": [{"type": "text", "text": input.text, "text_elements": []}]});
            // Only what changed: a sandbox policy sent on every turn would reset the user's
            // network access (T3 Code's lesson).
            if s.sent_mode != Some(s.mode) {
                let (approval, _, sandbox) = policy_of(s.mode);
                params["approvalPolicy"] = json!(approval);
                params["sandboxPolicy"] = sandbox;
                s.sent_mode = Some(s.mode);
            }
            if s.model.is_some() && s.model != s.sent_model {
                params["model"] = json!(s.model);
                s.sent_model = s.model.clone();
            }
            s.turn = Some(Turn {
                input: input.event_id,
                id: None,
                text: String::new(),
                thinking: String::new(),
                tools: HashMap::new(),
                changes: HashMap::new(),
            });
            params
        };
        let rx = match run.rpc.send("turn/start", params).await {
            Ok(rx) => rx,
            Err(e) => {
                run.shared.lock().unwrap().turn = None;
                bail!("turn/start: {e}");
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
            match rx.await.unwrap_or_else(|_| Err(RpcError::closed())) {
                Ok(v) => {
                    if let (Some(t), Some(id)) = (
                        shared.lock().unwrap().turn.as_mut(),
                        v["turn"]["id"].as_str(),
                    ) {
                        t.id.get_or_insert_with(|| id.to_string());
                    }
                }
                Err(_) if gone.load(Ordering::SeqCst) => {}
                Err(e) => {
                    // Refused: the turn ends here.
                    let out = {
                        let mut s = shared.lock().unwrap();
                        let fail = json!({"status": "failed", "error": {"message": e.message}});
                        s.end_turn(&fail)
                    };
                    for ev in out {
                        let _ = events.send(ev).await;
                    }
                }
            }
        });
        Ok(())
    }

    fn can_inject(&self) -> bool {
        self.run.as_ref().is_some_and(|r| {
            let s = r.shared.lock().unwrap();
            s.asked.is_empty() && s.turn.as_ref().is_some_and(|t| t.id.is_some())
        })
    }

    async fn inject_with(
        &mut self,
        input: &SessionInput,
        _files: &[crate::inbox::InboxFile],
    ) -> anyhow::Result<bool> {
        let Some(run) = &self.run else {
            return Ok(false);
        };
        let (thread, turn) = {
            let s = run.shared.lock().unwrap();
            match (s.thread.clone(), s.turn.as_ref().and_then(|t| t.id.clone())) {
                (Some(th), Some(tu)) => (th, tu),
                _ => return Ok(false),
            }
        };
        let params = json!({"threadId": thread, "expectedTurnId": turn,
            "input": [{"type": "text", "text": input.text, "text_elements": []}]});
        match self.timed("turn/steer", params).await {
            Ok(_) => {
                let _ = self
                    .events
                    .send(AdapterEvent::InputAccepted {
                        event_id: input.event_id,
                        joined: true,
                    })
                    .await;
                Ok(true)
            }
            // The turn ended meanwhile, or Codex cannot take a steer now (a review, a
            // compaction): the input waits for the next turn.
            Err(e) if e != RpcError::closed() => {
                tracing::info!(session_id = %self.session_id, "steer not taken: {e}");
                Ok(false)
            }
            Err(e) => bail!("turn/steer: {e}"),
        }
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        let Some(run) = &self.run else {
            return Ok(());
        };
        let (thread, turn, answers, cancels) = {
            let mut s = run.shared.lock().unwrap();
            let (answers, cancels) = s.cancel_asked(CancelReason::Stopped);
            (
                s.thread.clone(),
                s.turn.as_ref().and_then(|t| t.id.clone()),
                answers,
                cancels,
            )
        };
        for (id, a) in answers {
            run.rpc.respond(id, a).await;
        }
        for e in cancels {
            let _ = self.events.send(e).await;
        }
        let in_turn = run.shared.lock().unwrap().turn.is_some();
        if let (Some(thread), Some(turn)) = (thread, turn) {
            let _ = self
                .timed(
                    "turn/interrupt",
                    json!({"threadId": thread, "turnId": turn}),
                )
                .await;
        }
        if in_turn {
            if let Some(run) = &self.run {
                let (shared, signal, grace, kill_grace) = (
                    run.shared.clone(),
                    run.signal.clone(),
                    self.opts.stop_grace,
                    self.opts.kill_grace,
                );
                tokio::spawn(async move {
                    tokio::time::sleep(grace).await;
                    if shared.lock().unwrap().turn.is_some() {
                        tracing::warn!("Codex did not end its turn after turn/interrupt: ended");
                        let _ = signal(Sig::Terminate);
                        tokio::time::sleep(kill_grace).await;
                        let _ = signal(Sig::Kill);
                    }
                });
            }
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

    fn pid(&self) -> Option<u32> {
        self.run.as_ref().and_then(|r| r.pid)
    }

    /// Codex takes a mode with the next `turn/start`; so does a switch between turns, which
    /// is then confirmed at once. During a turn the turn keeps its mode: an error, and the
    /// supervisor asks again before the next turn (SPEC §8.6).
    async fn set_permission_mode(&mut self, mode: PermissionMode) -> anyhow::Result<()> {
        let Some(run) = &self.run else {
            return Ok(());
        };
        {
            let mut s = run.shared.lock().unwrap();
            if s.turn.is_some() && s.sent_mode != Some(mode) {
                bail!("Codex changes its mode between turns");
            }
            s.mode = mode;
        }
        let _ = self.events.send(AdapterEvent::PermissionMode(mode)).await;
        Ok(())
    }

    async fn set_model(&mut self, model: &str) -> anyhow::Result<()> {
        if let Some(run) = &self.run {
            run.shared.lock().unwrap().model = Some(model.to_string());
        }
        Ok(())
    }

    async fn answer_approval(
        &mut self,
        request_id: &str,
        option_id: &str,
        _message: Option<&str>,
    ) -> anyhow::Result<()> {
        let run = self.run.as_ref().context("no Codex process")?;
        let asked = run.shared.lock().unwrap().asked.remove(request_id);
        let Some(asked) = asked else {
            return Ok(());
        };
        let decision = match option_id {
            "accept" | "acceptForSession" | "decline" | "cancel" => option_id,
            // Not one of Codex's (the hub's own deny).
            _ => "decline",
        };
        run.rpc
            .respond(asked.rpc_id, json!({"decision": decision}))
            .await;
        Ok(())
    }

    async fn answer_clarify(
        &mut self,
        request_id: &str,
        answers: &BTreeMap<String, ClarifyAnswer>,
        _action: Option<ClarifyAction>,
    ) -> anyhow::Result<()> {
        let run = self.run.as_ref().context("no Codex process")?;
        let asked = run.shared.lock().unwrap().asked.remove(request_id);
        let Some(asked) = asked else {
            return Ok(());
        };
        let map: serde_json::Map<String, Value> = answers
            .iter()
            .map(|(k, a)| {
                let list = match a {
                    ClarifyAnswer::One(s) => vec![s.clone()],
                    ClarifyAnswer::Many(v) => v.clone(),
                };
                (k.clone(), json!({ "answers": list }))
            })
            .collect();
        run.rpc
            .respond(asked.rpc_id, json!({ "answers": map }))
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
