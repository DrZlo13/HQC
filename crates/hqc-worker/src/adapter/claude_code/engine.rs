//! The I/O-free part of the Claude Code adapter: stream-json lines in, [`Action`]s out
//! (events for the supervisor, lines for the CLI's stdin). One [`Core`] per process, so
//! `session` rules live exactly as long as the process (SPEC §8.1).
//!
//! Mapping (SPEC §4, §8.1, docs/notes/claude-control-protocol.md):
//! - `stream_event` `text_delta` → `agent.delta`;
//! - `assistant` text → `agent.message`, held until the next element: another assistant
//!   block or a tool result makes it `final: false`, `result` makes it `final: true`;
//! - `assistant` `tool_use` → `tool.call`; non-empty `thinking` → `agent.thinking`;
//! - `assistant` text with `model` `<synthetic>` (the CLI's own reply: an API error, "Credit
//!   balance is too low") → `agent.message` with `runtime: true`, held and flushed as any;
//! - `system` `task_notification` (a background task ended) → `system task_notification`,
//!   except for a task that a subagent started or whose `task_started` said it runs in the
//!   foreground, until a `task_updated` moves it to the background;
//!   `system` `compact_boundary` → `system context_compacted`;
//! - `user` `tool_result` → `tool.result`; `user` with `isReplay: true` → input accepted;
//! - `result` → `usage` (none with `num_turns: 0`, no tokens and no cost), `status idle`, end
//!   of turn; `usage.cost_usd` is the growth of the CLI's cumulative `total_cost_usd` since the
//!   previous `result` of the session (or since the transcript's last `cost-state` after
//!   `--resume`), `session_cost_usd` the total itself;
//!   a request of the main agent or of a foreground subagent still open then gets
//!   `approval.cancel` / `clarify.cancel` `stopped`, any other subagent's stays open until its
//!   `task_notification`, which cancels it;
//! - `system` `init` → the model, and [`AdapterEvent::Commands`] when its `slash_commands`
//!   differ from what this process reported (the CLI sends `init` before every turn);
//! - `control_request` `can_use_tool` → `approval.request` (with `policy_hint` and the hqc
//!   rules suggested for the call) / `clarify.request`, an immediate allow by a `session`
//!   rule or by the folder's hqc rules (T48), or an immediate deny plus `system
//!   policy_denied` when a deny rule of `[permissions]` matches (checked first);
//! - `system` `permission_denied` (the CLI refused a call by its rules, no request) →
//!   `system policy_denied`;
//! - `permissionMode` of `system` `init` and `system` `status`, and the CLI's answer to our
//!   `set_permission_mode` → [`AdapterEvent::PermissionMode`] when it changes (T38), and
//!   on every success of our `set_permission_mode` (T39);
//! - other `control_response` lines are the CLI's echo of ours and are skipped.
//!
//! U+0000 in any text taken from the CLI (binary tool output) becomes U+FFFD ([`text`],
//! [`preview`]): the hub's Postgres refuses it (SPEC §5.2).
//!
//! Messages of subagents (`parent_tool_use_id` set) are not mapped: their records live in
//! separate sidechain files, not in the session transcript.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;

use anyhow::{anyhow, bail};
use hqc_proto::event::{
    ApprovalCanonical, ApprovalOption, Author, CancelReason, ClarifyAnswer, ClarifyQuestion, Event,
    EventKind, SessionState,
};
use hqc_proto::frame::{PermissionMode, SessionInput};
use serde_json::{json, Value};
use uuid::Uuid;

use super::rules::SessionRules;
use crate::adapter::{new_event, AdapterEvent};
use crate::permissions::{FolderRules, Policy};

/// `*_preview` limit (SPEC §4).
pub const PREVIEW_LIMIT: usize = 4096;
pub const ERROR_TEXT_LIMIT: usize = 300;

/// A denial without text, sent with `interrupt: true`: what the "No" of the VS Code tab sends
/// (extension 2.1.309, cc_remote docs/SPEC.md §2.12). The CLI ends the turn at once with
/// `result` `error_during_execution` (checked live on 2.1.312).
pub const DENY_MESSAGE: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.";
/// Before the user's text of a denial with text (T26, SPEC §10.1), sent with `interrupt:
/// false`: the tab's wording for "Tell Claude what to do instead", two spaces before the
/// text included. Checked live on 2.1.312 with haiku: the model did what the text said and
/// did not run the call. With a plain «denied and wrote instead:» (2.1.308) it saw the text
/// but took it for an instruction from the tool result and did not follow it.
pub const DENY_WITH_TEXT: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). The user provided the following reason for the rejection:  ";
pub const CLARIFY_CANCEL_MESSAGE: &str = "The user did not answer the questions.";
pub const POLICY_DENY_MESSAGE: &str = "Denied by the worker's permission policy";
/// `system.code` of a tool call refused by a deny rule of `[permissions]` (SPEC §4).
pub const POLICY_DENIED: &str = "policy_denied";
/// `system.code` of a background task (a shell command, a subagent) that finished or
/// stopped: stream-json `system` `task_notification` (SPEC §4, §8.1).
pub const TASK_NOTIFICATION: &str = "task_notification";
/// `system.code` of stream-json `system` `compact_boundary` (SPEC §4, §8.1).
pub const CONTEXT_COMPACTED: &str = "context_compacted";
pub const CONTEXT_COMPACTED_TEXT: &str = "Context compacted";
/// `message.model` of a reply the CLI wrote itself instead of the model ("Credit balance is
/// too low", API errors; 2.1.312, stream-json and transcript alike): `agent.message` with
/// `runtime: true`.
pub const SYNTHETIC_MODEL: &str = "<synthetic>";
/// `request_id` prefix of our `set_permission_mode` control requests.
const MODE_REQUEST: &str = "hqc-mode-";

// Mostly `Emit`, like `AdapterEvent` itself; boxing would only add an allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Emit(AdapterEvent),
    /// One JSON line for stdin.
    Write(Value),
}

#[derive(Debug)]
enum Pending {
    Approval {
        tool: String,
        input: Value,
        /// `permission_suggestions` entries with `type: addRules`, `destination: localSettings`.
        local_rules: Vec<Value>,
        /// Every `rules[]` entry of every `addRules` suggestion, for `session`.
        suggested: Vec<Value>,
        /// The subagent that asked ([`agent_of`]); `None`: the main agent.
        agent_id: Option<String>,
    },
    Clarify {
        input: Value,
        /// Question texts by position; `clarify.request` ids are `1`, `2`, ...
        questions: Vec<String>,
        /// The subagent that asked ([`agent_of`]); `None`: the main agent.
        agent_id: Option<String>,
    },
}

impl Pending {
    fn agent_id(&self) -> Option<&str> {
        match self {
            Pending::Approval { agent_id, .. } | Pending::Clarify { agent_id, .. } => {
                agent_id.as_deref()
            }
        }
    }
}

#[derive(Debug)]
pub struct Core {
    session_id: Uuid,
    restricted: bool,
    /// The last assistant text: event id, text, and whether the CLI wrote it itself.
    held: Option<(Uuid, String, bool)>,
    /// `event_id` of the input sent last: `in_reply_to` of agent messages once its echo has
    /// come ([`Core::reply_to`]).
    last_input: Option<Uuid>,
    /// Inputs written to stdin whose `--replay-user-messages` echo has not come yet.
    awaiting_echo: VecDeque<Uuid>,
    in_turn: bool,
    turn: u64,
    pending: HashMap<String, Pending>,
    rules: SessionRules,
    /// `[permissions]` of the worker config, for `policy_hint` and denying what a deny
    /// rule matches.
    policy: Arc<Policy>,
    /// hqc's allow rules of the session's workspace and its path (T48); `None` without one.
    folder: Option<(FolderRules, std::path::PathBuf)>,
    model: Option<String>,
    /// The session's cumulative cost as of the last `result` (or the transcript's last
    /// `cost-state` when resumed): the baseline of the next turn's cost.
    cost_total: Option<f64>,
    /// The slash commands last reported by this process.
    commands: Option<Vec<String>>,
    /// The permission mode last reported by this process.
    permission_mode: Option<PermissionMode>,
    /// Message uuids read since the last [`Core::take_seen`].
    seen: Vec<Uuid>,
    /// Tasks whose `task_started` had `is_backgrounded: false` (a foreground subagent, a Bash
    /// call that runs for more than about 2 s) or that a subagent started
    /// (`owned_by_subagent: true`), the latter marked `true`: the CLI
    /// sends `task_notification` for them too (2.1.312), but they are no background task of
    /// the session's agent. A task of the agent itself that the CLI moves to the background
    /// later (`task_updated` `patch.is_backgrounded: true`) leaves the set.
    quiet_tasks: HashMap<String, bool>,
}

impl Core {
    /// Uuids of the main-chain `user`/`assistant` messages (with the prompt echoes) read
    /// from stdout since the last call, for [`crate::mirror::OwnRecords`].
    pub fn take_seen(&mut self) -> Vec<Uuid> {
        std::mem::take(&mut self.seen)
    }

    pub fn new(session_id: Uuid, restricted: bool) -> Self {
        Self {
            session_id,
            restricted,
            held: None,
            last_input: None,
            awaiting_echo: VecDeque::new(),
            in_turn: false,
            turn: 0,
            pending: HashMap::new(),
            rules: SessionRules::default(),
            policy: Arc::default(),
            folder: None,
            model: None,
            cost_total: None,
            commands: None,
            permission_mode: None,
            seen: Vec::new(),
            quiet_tasks: HashMap::new(),
        }
    }

    pub fn with_policy(mut self, policy: Arc<Policy>) -> Self {
        self.policy = policy;
        self
    }

    /// The workspace's hqc rules (T48): applied after the deny check, and the source of
    /// `approval.request.rule_suggestions`.
    pub fn with_folder(mut self, rules: FolderRules, root: std::path::PathBuf) -> Self {
        self.folder = Some((rules, root));
        self
    }

    /// The session's cumulative cost before this process: `totalCostUSD` of the
    /// transcript's last `cost-state` when the process resumes it (the CLI starts its
    /// `total_cost_usd` from there, checked on 2.1.308, T27).
    pub fn with_cost_baseline(mut self, total: Option<f64>) -> Self {
        self.cost_total = total;
        self
    }

    /// Whether turn number `turn` (from [`Core::interrupt`]) is still running.
    pub fn turn_running(&self, turn: u64) -> bool {
        self.in_turn && self.turn == turn
    }

    pub fn session_rules(&self) -> usize {
        self.rules.len()
    }

    // ---- from the supervisor -------------------------------------------------------

    pub fn input(&mut self, input: &SessionInput) -> Vec<Action> {
        self.input_with_images(input, vec![])
    }

    /// The user message of stream-json: the text, or with images a content array of a text
    /// block followed by `images` (image blocks of the Messages API, SPEC §8.1). The
    /// artifacts' paths are already in `input.text` (`.hqc/inbox`, SPEC §12).
    pub fn input_with_images(&mut self, input: &SessionInput, images: Vec<Value>) -> Vec<Action> {
        // `peer_prefix` and `peer` are in `text` already (crate::peer, SPEC §7.1).
        self.last_input = Some(input.event_id);
        self.awaiting_echo.push_back(input.event_id);
        self.in_turn = true;
        self.turn += 1;
        self.held = None;
        let content = if images.is_empty() {
            json!(input.text)
        } else {
            let mut blocks = vec![json!({"type": "text", "text": input.text})];
            blocks.extend(images);
            Value::Array(blocks)
        };
        vec![Action::Write(json!({
            "type": "user",
            "message": {"role": "user", "content": content},
        }))]
    }

    /// `control_request` `interrupt` for the running turn; `None` if no turn runs.
    pub fn interrupt(&mut self) -> Option<(Action, u64)> {
        if !self.in_turn {
            return None;
        }
        let req = json!({
            "type": "control_request",
            "request_id": format!("hqc-interrupt-{}", Uuid::now_v7()),
            "request": {"subtype": "interrupt"},
        });
        Some((Action::Write(req), self.turn))
    }

    /// Switch the running process to `mode` (T38): `control_request {subtype:
    /// set_permission_mode}`, which the CLI applies to the next permission check and
    /// confirms with `system/status {permissionMode}` (checked on 2.1.308). `ask` is the
    /// CLI's `default`, the name it reports for `manual` too.
    pub fn set_permission_mode(&mut self, mode: PermissionMode) -> Action {
        Action::Write(json!({
            "type": "control_request",
            "request_id": format!("{MODE_REQUEST}{}", Uuid::now_v7()),
            "request": {"subtype": "set_permission_mode", "mode": match mode {
                PermissionMode::Ask => "default",
                PermissionMode::Auto => "auto",
            }},
        }))
    }

    /// `message`: the user's text with `deny` (T26); it goes to Claude Code as the reason of
    /// the denial, which the model gets as the tool result.
    pub fn answer_approval(
        &mut self,
        request_id: &str,
        option_id: &str,
        message: Option<&str>,
    ) -> anyhow::Result<Vec<Action>> {
        let p = self
            .pending
            .remove(request_id)
            .ok_or_else(|| anyhow!("no open approval request {request_id}"))?;
        let (tool, input, local_rules, suggested, agent_id) = match p {
            Pending::Approval {
                tool,
                input,
                local_rules,
                suggested,
                agent_id,
            } => (tool, input, local_rules, suggested, agent_id),
            Pending::Clarify { .. } if option_id == "deny" => {
                return Ok(vec![Action::Write(deny(
                    request_id,
                    CLARIFY_CANCEL_MESSAGE,
                ))]);
            }
            p @ Pending::Clarify { .. } => {
                self.pending.insert(request_id.into(), p);
                bail!("request {request_id} is a clarify request");
            }
        };
        let resp = match option_id {
            "once" => allow(request_id, &input, None),
            // As the VS Code tab: "No" stops the turn, a text answer lets it go on.
            "deny" => match message {
                Some(m) => deny_with(request_id, &format!("{DENY_WITH_TEXT}{m}"), false),
                None => deny_with(request_id, DENY_MESSAGE, true),
            },
            "always" if !self.restricted && !local_rules.is_empty() => {
                allow(request_id, &input, Some(local_rules))
            }
            // `always` without a local rule to write, or in restricted mode, is `session`
            // (SPEC §8.1).
            "session" | "always" => {
                self.rules.add(&tool, &input, &suggested);
                allow(request_id, &input, None)
            }
            other => {
                self.pending.insert(
                    request_id.into(),
                    Pending::Approval {
                        tool,
                        input,
                        local_rules,
                        suggested,
                        agent_id,
                    },
                );
                bail!("unknown approval option {other:?}");
            }
        };
        tracing::info!(session_id = %self.session_id, %tool, option = option_id, "approval answered");
        Ok(vec![Action::Write(resp)])
    }

    /// Empty `answers`: the hub cancelled the request; the tool call is denied (SPEC §5.3).
    pub fn answer_clarify(
        &mut self,
        request_id: &str,
        answers: &BTreeMap<String, ClarifyAnswer>,
    ) -> anyhow::Result<Vec<Action>> {
        let (input, questions) = match self.pending.remove(request_id) {
            Some(Pending::Clarify {
                input, questions, ..
            }) => (input, questions),
            Some(p) => {
                self.pending.insert(request_id.into(), p);
                bail!("request {request_id} is an approval request");
            }
            None => bail!("no open clarify request {request_id}"),
        };
        let mut out = serde_json::Map::new();
        for (id, a) in answers {
            let Some(text) = id
                .parse::<usize>()
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|i| questions.get(i))
            else {
                tracing::warn!(session_id = %self.session_id, question = %id, "answer to an unknown question dropped");
                continue;
            };
            let value = match a {
                ClarifyAnswer::One(s) => s.clone(),
                ClarifyAnswer::Many(v) => v.join(", "),
            };
            out.insert(text.clone(), Value::String(value));
        }
        if out.is_empty() {
            tracing::info!(session_id = %self.session_id, "clarify cancelled");
            return Ok(vec![Action::Write(deny(
                request_id,
                CLARIFY_CANCEL_MESSAGE,
            ))]);
        }
        let mut updated = input;
        updated["answers"] = Value::Object(out);
        tracing::info!(session_id = %self.session_id, "clarify answered");
        Ok(vec![Action::Write(allow(request_id, &updated, None))])
    }

    /// The process is gone: a held text is sent as is, open requests are dropped.
    pub fn on_exit(&mut self) -> Vec<Action> {
        self.pending.clear();
        self.awaiting_echo.clear();
        self.in_turn = false;
        self.flush(false).into_iter().collect()
    }

    // ---- from the CLI ----------------------------------------------------------------

    pub fn on_line(&mut self, line: &str) -> Vec<Action> {
        let line = line.trim();
        if line.is_empty() {
            return vec![];
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(session_id = %self.session_id, "unparseable stdout line from claude: {e}");
                return vec![];
            }
        };
        let ty = v["type"].as_str().unwrap_or("");
        tracing::trace!(session_id = %self.session_id, ty, "claude line");
        let sub = !v["parent_tool_use_id"].is_null();
        // The transcript records this process wrote, its prompts' echoes included: the
        // mirror must not take them for another writer (SPEC §9.3).
        if matches!(ty, "user" | "assistant") && !sub {
            if let Some(u) = record_uuid(&v) {
                self.seen.push(u);
            }
        }
        match ty {
            "stream_event" if !sub => self.on_stream_event(&v),
            "assistant" if !sub => self.on_assistant(&v),
            "user" if v["isReplay"] == true => self.on_echo(),
            "user" if !sub => self.on_user(&v),
            "result" => self.on_result(&v),
            "control_request" => self.on_control_request(&v),
            "control_response" => self.on_control_response(&v),
            "system" => match v["subtype"].as_str() {
                Some("init") => {
                    self.model = v["model"].as_str().map(str::to_owned);
                    let mut out = self.on_mode(&v["permissionMode"], false);
                    out.extend(self.on_init(&v));
                    out
                }
                Some("status") => self.on_mode(&v["permissionMode"], false),
                Some("permission_denied") => {
                    let mut out: Vec<Action> = self.flush(false).into_iter().collect();
                    out.extend(self.on_permission_denied(&v));
                    out
                }
                Some("task_started") => {
                    let subagent = v["owned_by_subagent"] == true;
                    if let Some(id) = v["task_id"]
                        .as_str()
                        .filter(|_| subagent || v["is_backgrounded"] == false)
                    {
                        self.quiet_tasks.insert(id.to_owned(), subagent);
                    }
                    vec![]
                }
                Some("task_updated") => {
                    if let Some(id) = v["task_id"]
                        .as_str()
                        .filter(|_| v["patch"]["is_backgrounded"] == true)
                    {
                        if self.quiet_tasks.get(id) == Some(&false) {
                            self.quiet_tasks.remove(id);
                        }
                    }
                    vec![]
                }
                Some("task_notification") => self.on_task_notification(&v),
                // Its uuid is that of the transcript's `system compact_boundary` record
                // (fixture `resume-compact`): the event id is derived from it as from a
                // record's.
                Some("compact_boundary") => {
                    tracing::info!(session_id = %self.session_id, trigger = ?v["compact_metadata"]["trigger"].as_str(), "context compacted");
                    vec![self.emit_system_as(
                        record_uuid(&v).map(|u| record_event_id(&u, 0)),
                        CONTEXT_COMPACTED,
                        CONTEXT_COMPACTED_TEXT.into(),
                    )]
                }
                _ => vec![],
            },
            _ => vec![],
        }
    }

    /// The CLI's permission mode: `auto` is `auto`, any other (`default`, which `manual`
    /// shows as, or `acceptEdits`/`plan` the CLI may enter by itself) asks. Reported when it
    /// changes, and on every confirmation of a switch of ours (`confirm`): the supervisor
    /// waits for that before it hands over a turn capped to `ask` (T39), also when the
    /// process already ran in `ask` because a switch back to `auto` was refused.
    fn on_mode(&mut self, v: &Value, confirm: bool) -> Vec<Action> {
        let Some(m) = v.as_str() else {
            return vec![];
        };
        let mode = if m == "auto" {
            PermissionMode::Auto
        } else {
            PermissionMode::Ask
        };
        if self.permission_mode == Some(mode) && !confirm {
            return vec![];
        }
        tracing::info!(session_id = %self.session_id, mode = m, "permission mode");
        self.permission_mode = Some(mode);
        vec![Action::Emit(AdapterEvent::PermissionMode(mode))]
    }

    /// The CLI's answer to our `set_permission_mode`; every other `control_response` is
    /// the echo of one of ours.
    fn on_control_response(&mut self, v: &Value) -> Vec<Action> {
        let r = &v["response"];
        if !r["request_id"]
            .as_str()
            .is_some_and(|id| id.starts_with(MODE_REQUEST))
        {
            return vec![];
        }
        if r["subtype"] == "success" {
            return self.on_mode(&r["response"]["mode"], true);
        }
        tracing::warn!(session_id = %self.session_id, error = %r["error"], "claude refused the permission mode switch");
        vec![]
    }

    /// `slash_commands` of `system/init` without those that only work in the terminal UI
    /// (`terminal_slash_commands`) and the CLI's internal ones (`__name`); reported once per
    /// process and again only when the list changes (`/reload-plugins`, a new skill).
    fn on_init(&mut self, v: &Value) -> Vec<Action> {
        // An older CLI without the list: nothing to report.
        if !v["slash_commands"].is_array() {
            return vec![];
        }
        let names = |v: &Value| -> Vec<String> {
            v.as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c.as_str())
                .map(|c| text(c.trim().trim_start_matches('/')))
                .collect()
        };
        let terminal = names(&v["terminal_slash_commands"]);
        let mut list: Vec<String> = Vec::new();
        for c in names(&v["slash_commands"]) {
            if !c.is_empty() && !c.starts_with('_') && !terminal.contains(&c) && !list.contains(&c)
            {
                list.push(c);
            }
        }
        if self.commands.as_ref() == Some(&list) {
            return vec![];
        }
        tracing::info!(session_id = %self.session_id, commands = list.len(), "slash commands");
        self.commands = Some(list.clone());
        vec![Action::Emit(AdapterEvent::Commands(list))]
    }

    /// This turn's share of the CLI's cumulative `total_cost_usd`: never negative, and a total
    /// below the baseline (a CLI that did not carry the cost over) does not lower it.
    fn turn_cost(&mut self, total: f64) -> f64 {
        let base = self.cost_total.unwrap_or(0.0);
        self.cost_total = Some(base.max(total));
        // Rounded to 1e-10 USD: the difference of two decimals is not one in binary.
        ((total - base).max(0.0) * 1e10).round() / 1e10
    }

    fn on_stream_event(&mut self, v: &Value) -> Vec<Action> {
        let ev = &v["event"];
        if ev["type"] == "content_block_delta" && ev["delta"]["type"] == "text_delta" {
            if let Some(t) = ev["delta"]["text"].as_str().filter(|t| !t.is_empty()) {
                return vec![self.emit_new(EventKind::AgentDelta { text: text(t) })];
            }
        }
        vec![]
    }

    fn on_assistant(&mut self, v: &Value) -> Vec<Action> {
        let Some(uuid) = record_uuid(v) else {
            tracing::warn!(session_id = %self.session_id, "assistant message without uuid skipped");
            return vec![];
        };
        // The CLI's own reply, not the model's: marked, but held and flushed like any text,
        // so it is the turn's final message when it ends the turn.
        let synthetic = v["message"]["model"] == SYNTHETIC_MODEL;
        let mut out = Vec::new();
        for (i, block) in blocks(v).iter().enumerate() {
            let id = record_event_id(&uuid, i);
            match block["type"].as_str() {
                Some("text") => {
                    let t = block["text"].as_str().unwrap_or("");
                    if t.is_empty() {
                        continue;
                    }
                    out.extend(self.flush(false));
                    if synthetic {
                        tracing::info!(session_id = %self.session_id, "runtime message");
                    }
                    self.held = Some((id, text(t), synthetic));
                }
                Some("tool_use") => {
                    out.extend(self.flush(false));
                    out.push(self.emit(
                        id,
                        EventKind::ToolCall {
                            call_id: block["id"].as_str().unwrap_or("").into(),
                            tool: block["name"].as_str().unwrap_or("").into(),
                            args_preview: preview(&compact(&block["input"]), PREVIEW_LIMIT),
                        },
                    ));
                }
                Some("thinking") => {
                    let t = block["thinking"].as_str().unwrap_or("");
                    if t.is_empty() {
                        continue;
                    }
                    out.extend(self.flush(false));
                    out.push(self.emit(id, EventKind::AgentThinking { text: text(t) }));
                }
                _ => {}
            }
        }
        out
    }

    fn on_echo(&mut self) -> Vec<Action> {
        match self.awaiting_echo.pop_front() {
            Some(event_id) => vec![Action::Emit(AdapterEvent::InputAccepted { event_id })],
            None => {
                tracing::warn!(session_id = %self.session_id, "replay echo without a sent input");
                vec![]
            }
        }
    }

    fn on_user(&mut self, v: &Value) -> Vec<Action> {
        let Some(uuid) = record_uuid(v) else {
            return vec![];
        };
        let mut out = Vec::new();
        for (i, block) in blocks(v).iter().enumerate() {
            if block["type"] != "tool_result" {
                continue;
            }
            out.extend(self.flush(false));
            let text = result_text(&block["content"]);
            let error = block["is_error"] == true;
            out.push(self.emit(
                record_event_id(&uuid, i),
                EventKind::ToolResult {
                    call_id: block["tool_use_id"].as_str().unwrap_or("").into(),
                    result_preview: preview(&text, PREVIEW_LIMIT),
                    error,
                    error_text: error.then(|| preview(&text, ERROR_TEXT_LIMIT)),
                    duration_ms: None,
                    artifact_id: None,
                },
            ));
        }
        out
    }

    fn on_result(&mut self, v: &Value) -> Vec<Action> {
        let mut out: Vec<Action> = self.flush(true).into_iter().collect();
        // A request the CLI no longer waits for: after "No" to one of two parallel calls it
        // ends the turn and drops the other. Without the cancel the hub keeps its card open
        // (Claude requests do not expire), and an answer to it finds no open request here.
        // That is a request of the main agent or of a foreground subagent (in `quiet_tasks`
        // with `false`), which cannot outlive the main turn. Any other subagent's request stays
        // open: a background subagent's outlives the main turn's `result`, and the CLI still
        // waits for its answer (checked live on 2.1.312).
        let quiet = self.quiet_tasks.clone();
        let cancels = self.cancel_pending(|p| match p.agent_id() {
            None => true,
            Some(agent) => quiet.get(agent) == Some(&false),
        });
        if !cancels.is_empty() {
            tracing::info!(session_id = %self.session_id, open = cancels.len(), "turn ended with open requests; cancelled");
        }
        out.extend(cancels);
        let usage = &v["usage"];
        let num = |k: &str| usage[k].as_u64();
        let (input_tokens, output_tokens) = (
            num("input_tokens").unwrap_or(0),
            num("output_tokens").unwrap_or(0),
        );
        let total = v["total_cost_usd"]
            .as_f64()
            .filter(|c| c.is_finite() && *c >= 0.0);
        let spent = total.is_some_and(|t| t > self.cost_total.unwrap_or(0.0));
        // No `usage` for a result with no model turn, no tokens and no cost (a local slash
        // command, the empty second `result` after a turn the CLI started by itself): the feed
        // would show an empty `0 → 0 tokens` line. `/compact` has `num_turns: 0` and no tokens
        // but costs, so it keeps its `usage`.
        if v["num_turns"].as_u64() != Some(0) || input_tokens + output_tokens > 0 || spent {
            let cost_usd = total.map(|t| self.turn_cost(t));
            out.push(self.emit_new(EventKind::Usage {
                input_tokens,
                output_tokens,
                cache_read: num("cache_read_input_tokens"),
                cache_write: num("cache_creation_input_tokens"),
                cost_usd,
                session_cost_usd: total,
                model: self.model.clone().or_else(|| {
                    v["modelUsage"]
                        .as_object()
                        .and_then(|m| m.keys().next().cloned())
                }),
            }));
        }
        let subtype = v["subtype"].as_str().unwrap_or("");
        out.push(self.emit_new(EventKind::Status {
            state: SessionState::Idle,
            // `error_max_turns`, `error_during_execution`, ...: why the turn ended early.
            detail: (subtype != "success" || v["is_error"] == true).then(|| subtype.to_owned()),
            // The turn's input: the hub fails an `hqc_ask` it carried that no final message
            // answered (SPEC §7.1).
            in_reply_to: self.reply_to(),
        }));
        // The CLI answered the input without echoing it: a local slash command (`/name`,
        // `/cost`, ...) runs without a model turn and without `--replay-user-messages`. The
        // result still means the input was taken; without this the supervisor resends it.
        while let Some(event_id) = self.awaiting_echo.pop_front() {
            tracing::info!(session_id = %self.session_id, %event_id, "input taken without an echo (local command)");
            out.push(Action::Emit(AdapterEvent::InputAccepted { event_id }));
        }
        out.push(Action::Emit(AdapterEvent::TurnEnded));
        tracing::info!(
            session_id = %self.session_id,
            subtype,
            num_turns = v["num_turns"].as_u64(),
            duration_ms = v["duration_ms"].as_u64(),
            "turn ended"
        );
        self.in_turn = false;
        out
    }

    fn on_control_request(&mut self, v: &Value) -> Vec<Action> {
        let request_id = v["request_id"].as_str().unwrap_or("").to_owned();
        let req = &v["request"];
        match req["subtype"].as_str() {
            Some("can_use_tool") => self.can_use_tool(request_id, req),
            other => {
                tracing::warn!(session_id = %self.session_id, subtype = ?other, "unsupported control_request");
                vec![Action::Write(json!({
                    "type": "control_response",
                    "response": {
                        "subtype": "error",
                        "request_id": request_id,
                        "error": format!("hqc does not support control_request {}", other.unwrap_or("?")),
                    },
                }))]
            }
        }
    }

    fn can_use_tool(&mut self, request_id: String, req: &Value) -> Vec<Action> {
        let tool = req["tool_name"].as_str().unwrap_or("").to_owned();
        let input = req.get("input").cloned().unwrap_or_else(|| json!({}));

        if tool == "AskUserQuestion" {
            if let Some((questions, texts)) = parse_questions(&input) {
                tracing::info!(session_id = %self.session_id, questions = texts.len(), "clarify requested");
                self.pending.insert(
                    request_id.clone(),
                    Pending::Clarify {
                        input,
                        questions: texts,
                        agent_id: agent_of(req),
                    },
                );
                return vec![self.emit_new(EventKind::ClarifyRequest {
                    request_id,
                    questions,
                })];
            }
            tracing::warn!(session_id = %self.session_id, "AskUserQuestion without questions; asking as an approval");
        }

        // The CLI never asks about a call its `--settings` deny rules match (2.1.308); one
        // that the adapter's reading of the same rules matches is denied here, so no
        // `always`, `session` or hub `auto_approve` can let it through.
        if let Some(rule) = self.policy.denied_by(&tool, &input) {
            tracing::warn!(session_id = %self.session_id, %tool, rule, "denied by the permission policy");
            let what = describe(req, &input, &tool);
            return vec![
                Action::Write(deny(
                    &request_id,
                    &format!("{POLICY_DENY_MESSAGE} ({rule})."),
                )),
                self.emit_system(
                    POLICY_DENIED,
                    preview(
                        &format!("{POLICY_DENY_MESSAGE} ({rule}): {tool}: {what}"),
                        ERROR_TEXT_LIMIT,
                    ),
                ),
            ];
        }

        if self.rules.matches(&tool, &input) {
            tracing::info!(session_id = %self.session_id, %tool, "allowed by a session rule");
            return vec![Action::Write(allow(&request_id, &input, None))];
        }
        // hqc's rules of the folder (T48), read at every call, so a change reaches a running
        // session at once; every part of a compound command must be covered.
        if let Some((rules, root)) = &self.folder {
            if let Some(rule) = self.policy.folder_allows(rules, root, &tool, &input) {
                tracing::info!(session_id = %self.session_id, %tool, %rule, "allowed by a folder rule");
                return vec![Action::Write(allow(&request_id, &input, None))];
            }
        }
        let policy_hint = self.policy.hint(&tool, &input);
        let rule_suggestions = self
            .folder
            .as_ref()
            .and_then(|(_, root)| crate::permissions::suggest(&tool, &input, root));

        let suggestions = req["permission_suggestions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let add_rules = suggestions
            .iter()
            .filter(|s| s["type"] == "addRules" && s["behavior"] == "allow");
        let local_rules: Vec<Value> = add_rules
            .clone()
            .filter(|s| s["destination"] == "localSettings")
            .cloned()
            .collect();
        let suggested: Vec<Value> = add_rules
            .flat_map(|s| s["rules"].as_array().cloned().unwrap_or_default())
            .collect();

        let mut options = vec![
            option("once", "Allow once", ApprovalCanonical::Once),
            option(
                "session",
                "Allow for this session",
                ApprovalCanonical::Session,
            ),
        ];
        if !local_rules.is_empty() && !self.restricted {
            options.push(option("always", "Always allow", ApprovalCanonical::Always));
        }
        options.push(option("deny", "Deny", ApprovalCanonical::Deny));

        let description = describe(req, &input, &tool);
        tracing::info!(session_id = %self.session_id, %tool, "approval requested");
        self.pending.insert(
            request_id.clone(),
            Pending::Approval {
                tool: tool.clone(),
                input,
                local_rules,
                suggested,
                agent_id: agent_of(req),
            },
        );
        vec![self.emit_new(EventKind::ApprovalRequest {
            request_id,
            tool,
            description,
            options,
            expires_at: None,
            policy_hint,
            rule_suggestions,
        })]
    }

    /// `system/permission_denied`: the CLI refused a call without asking. In this adapter's
    /// setup (`--permission-prompt-tool stdio`) that is a deny rule (`reason_type` `rule` or
    /// `subcommandResults`) or, with `mode = "auto"`, the CLI's classifier.
    fn on_permission_denied(&self, v: &Value) -> Vec<Action> {
        let message = v["message"].as_str().unwrap_or("Permission denied.");
        let reason = v["decision_reason_type"].as_str().unwrap_or("");
        tracing::info!(session_id = %self.session_id, tool = ?v["tool_name"].as_str(), reason, "the CLI denied a tool call");
        let (code, text) = match reason {
            "rule" | "subcommandResults" => {
                (POLICY_DENIED, format!("{POLICY_DENY_MESSAGE}: {message}"))
            }
            _ => ("permission_denied", message.to_owned()),
        };
        vec![self.emit_system(code, preview(&text, ERROR_TEXT_LIMIT))]
    }

    /// `system/task_notification`: a background task ended (checked live on 2.1.312: it comes
    /// as soon as the task ends, before the turn the CLI then starts by itself). The text is
    /// its `summary`, or "Background task" without one. Not a flush: a held text keeps
    /// waiting for what decides whether it is final. First the task's open requests (a
    /// subagent's, by `agent_id`) are cancelled; a task in [`Core::quiet_tasks`] gets no line;
    /// a task this process did not see start (it began before a `--resume`) counts as a
    /// background one.
    fn on_task_notification(&mut self, v: &Value) -> Vec<Action> {
        let id = v["task_id"].as_str();
        // A subagent that ended asks nothing any more: its requests still open are cancelled
        // here, whichever comes first, this or the main turn's `result`.
        let mut out = match id {
            Some(id) => self.cancel_pending(|p| p.agent_id() == Some(id)),
            None => vec![],
        };
        if id.is_some_and(|id| self.quiet_tasks.remove(id).is_some()) {
            return out;
        }
        let t = v["summary"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map_or_else(|| "Background task".into(), text);
        tracing::info!(session_id = %self.session_id, task_id = ?v["task_id"].as_str(), status = ?v["status"].as_str(), "background task notification");
        out.push(self.emit_system(TASK_NOTIFICATION, t));
        out
    }

    /// Cancels the open requests `which` picks, in request id order: `approval.cancel` or
    /// `clarify.cancel` with `stopped`, so the hub closes their cards.
    fn cancel_pending(&mut self, which: impl Fn(&Pending) -> bool) -> Vec<Action> {
        let (mut open, kept): (Vec<(String, Pending)>, Vec<_>) =
            self.pending.drain().partition(|(_, p)| which(p));
        self.pending.extend(kept);
        open.sort_by(|a, b| a.0.cmp(&b.0));
        open.into_iter()
            .map(|(request_id, p)| {
                let reason = CancelReason::Stopped;
                let kind = match p {
                    Pending::Approval { .. } => EventKind::ApprovalCancel { request_id, reason },
                    Pending::Clarify { .. } => EventKind::ClarifyCancel { request_id, reason },
                };
                Action::Emit(AdapterEvent::Event(new_event(
                    self.session_id,
                    Author::System,
                    kind,
                )))
            })
            .collect()
    }

    // ---- helpers -----------------------------------------------------------------------

    fn flush(&mut self, r#final: bool) -> Option<Action> {
        let (id, text, runtime) = self.held.take()?;
        Some(self.emit(
            id,
            EventKind::AgentMessage {
                text,
                r#final,
                in_reply_to: self.reply_to(),
                runtime,
            },
        ))
    }

    /// The input the running turn answers: the one sent last, once its echo has come. Until
    /// then the CLI has only queued it and what comes is no reply to it. Written during a turn
    /// the CLI started by itself after a background task, it is echoed after that turn's
    /// `result` (checked live on 2.1.312), so the turn's final message and `status idle` name
    /// none; taken into the turn after a tool result, the rest of the turn names it.
    fn reply_to(&self) -> Option<Uuid> {
        self.last_input
            .filter(|id| !self.awaiting_echo.contains(id))
    }

    fn emit(&self, event_id: Uuid, kind: EventKind) -> Action {
        let mut e = self.emit_event(kind);
        e.event_id = event_id;
        Action::Emit(AdapterEvent::Event(e))
    }

    /// An event with no record behind it: fresh v7 id.
    fn emit_new(&self, kind: EventKind) -> Action {
        Action::Emit(AdapterEvent::Event(self.emit_event(kind)))
    }

    /// `system {code, text}` authored by the worker (SPEC §4), fresh v7 id.
    fn emit_system(&self, code: &str, text: String) -> Action {
        self.emit_system_as(None, code, text)
    }

    /// [`Self::emit_system`] with `event_id`, when given.
    fn emit_system_as(&self, event_id: Option<Uuid>, code: &str, text: String) -> Action {
        let mut e = new_event(
            self.session_id,
            Author::System,
            EventKind::System {
                text,
                code: code.into(),
                fork: None,
            },
        );
        if let Some(id) = event_id {
            e.event_id = id;
        }
        Action::Emit(AdapterEvent::Event(e))
    }

    fn emit_event(&self, kind: EventKind) -> Event {
        new_event(
            self.session_id,
            Author::Agent {
                id: self.session_id,
            },
            kind,
        )
    }
}

/// Event id of content block `index` of a jsonl record. Claude Code writes one block per
/// record, so this is `Event::id_from_claude_record(uuid)`; a further block of the same
/// record (not seen on 2.1.307/2.1.308) gets v5 of `"<uuid>/<index>"`.
pub fn record_event_id(uuid: &Uuid, index: usize) -> Uuid {
    Event::id_from_claude_block(uuid, index)
}

fn record_uuid(v: &Value) -> Option<Uuid> {
    v["uuid"].as_str()?.parse().ok()
}

fn blocks(v: &Value) -> Vec<Value> {
    v["message"]["content"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The subagent a `can_use_tool` request comes from: its `agent_id`, the `task_id` of the
/// subagent's `task_started`; the main agent's request has none (checked live on 2.1.312 with
/// a `run_in_background` subagent's Bash call).
fn agent_of(req: &Value) -> Option<String> {
    req["agent_id"].as_str().map(str::to_owned)
}

fn allow(request_id: &str, input: &Value, permissions: Option<Vec<Value>>) -> Value {
    let mut r = json!({"behavior": "allow", "updatedInput": input});
    if let Some(p) = permissions {
        r["updatedPermissions"] = Value::Array(p);
    }
    control_response(request_id, r)
}

fn deny(request_id: &str, message: &str) -> Value {
    control_response(request_id, json!({"behavior": "deny", "message": message}))
}

/// A denial by the user, with the CLI's `interrupt`: `true` ends the turn at once.
fn deny_with(request_id: &str, message: &str, interrupt: bool) -> Value {
    control_response(
        request_id,
        json!({"behavior": "deny", "message": message, "interrupt": interrupt}),
    )
}

fn control_response(request_id: &str, response: Value) -> Value {
    json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request_id, "response": response},
    })
}

fn option(id: &str, label: &str, canonical: ApprovalCanonical) -> ApprovalOption {
    ApprovalOption {
        id: id.into(),
        label: label.into(),
        canonical,
    }
}

/// The CLI's own description plus what is being touched: the command, file, URL or the
/// blocked path.
fn describe(req: &Value, input: &Value, tool: &str) -> String {
    let what = ["command", "file_path", "notebook_path", "url", "pattern"]
        .iter()
        .find_map(|k| input[*k].as_str())
        .or_else(|| req["blocked_path"].as_str());
    let parts: Vec<&str> = [req["description"].as_str(), what]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        return preview(&format!("{tool} {}", compact(input)), ERROR_TEXT_LIMIT);
    }
    preview(&parts.join("\n"), PREVIEW_LIMIT)
}

fn parse_questions(input: &Value) -> Option<(Vec<ClarifyQuestion>, Vec<String>)> {
    let qs = input["questions"].as_array().filter(|q| !q.is_empty())?;
    let mut questions = Vec::new();
    let mut texts = Vec::new();
    for (i, q) in qs.iter().enumerate() {
        // The raw question goes back to the CLI as the key of its answer.
        let raw = q["question"].as_str()?;
        // Labels and descriptions pair up: an option without a label is dropped with its
        // description.
        let (options, descriptions): (Vec<String>, Vec<String>) = q["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|o| {
                let label = o["label"].as_str()?;
                Some((text(label), text(o["description"].as_str().unwrap_or(""))))
            })
            .unzip();
        let described = descriptions.iter().any(|d| !d.is_empty());
        questions.push(ClarifyQuestion {
            id: (i + 1).to_string(),
            text: text(raw),
            header: q["header"]
                .as_str()
                .filter(|h| !h.trim().is_empty())
                .map(text),
            options: (!options.is_empty()).then_some(options),
            option_descriptions: described.then_some(descriptions),
            multi: q["multiSelect"].as_bool(),
        });
        texts.push(raw.to_owned());
    }
    Some((questions, texts))
}

/// The text of a `tool_result` block's `content` (a string or text parts).
pub fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|p| match p["type"].as_str() {
                Some("text") => p["text"].as_str().unwrap_or("").to_owned(),
                Some(t) => format!("[{t}]"),
                None => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => compact(other),
    }
}

fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// Event text: U+0000 (binary tool output) becomes U+FFFD, which the hub can store
/// (SPEC §5.2).
pub fn text(s: &str) -> String {
    hqc_proto::replace_nul(s).into_owned()
}

/// [`text`], then at most `limit` bytes, cut at a char boundary.
pub fn preview(s: &str, limit: usize) -> String {
    let s = &*hqc_proto::replace_nul(s);
    if s.len() <= limit {
        return s.to_owned();
    }
    let mut end = limit;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}
