//! The I/O-free part of the Claude Code adapter: stream-json lines in, [`Action`]s out
//! (events for the supervisor, lines for the CLI's stdin). One [`Core`] per process, so
//! `session` rules live exactly as long as the process (SPEC §8.1).
//!
//! Mapping (SPEC §4, §8.1, docs/notes/claude-control-protocol.md):
//! - `stream_event` `text_delta` → `agent.delta`;
//! - `assistant` text → `agent.message`, held until the next element: another assistant
//!   block or a tool result makes it `final: false`, `result` makes it `final: true`;
//! - `assistant` `tool_use` → `tool.call`; non-empty `thinking` → `agent.thinking`;
//! - `user` `tool_result` → `tool.result`; `user` with `isReplay: true` → input accepted;
//! - `result` → `usage`, `status idle`, end of turn; `usage.cost_usd` is the growth of the
//!   CLI's cumulative `total_cost_usd` since the previous `result` of the session (or since
//!   the transcript's last `cost-state` after `--resume`), `session_cost_usd` the total itself;
//! - `system` `init` → the model, and [`AdapterEvent::Commands`] when its `slash_commands`
//!   differ from what this process reported (the CLI sends `init` before every turn);
//! - `control_request` `can_use_tool` → `approval.request` (with `policy_hint`) /
//!   `clarify.request`, an immediate allow by a `session` rule, or an immediate deny plus
//!   `system policy_denied` when a deny rule of `[permissions]` matches;
//! - `system` `permission_denied` (the CLI refused a call by its rules, no request) →
//!   `system policy_denied`;
//! - `permissionMode` of `system` `init` and `system` `status`, and the CLI's answer to our
//!   `set_permission_mode` → [`AdapterEvent::PermissionMode`] when it changes (T38);
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
    ApprovalCanonical, ApprovalOption, Author, ClarifyAnswer, ClarifyQuestion, Event, EventKind,
    SessionState,
};
use hqc_proto::frame::{PermissionMode, SessionInput};
use serde_json::{json, Value};
use uuid::Uuid;

use super::rules::SessionRules;
use crate::adapter::{new_event, AdapterEvent};
use crate::permissions::Policy;

/// `*_preview` limit (SPEC §4).
pub const PREVIEW_LIMIT: usize = 4096;
pub const ERROR_TEXT_LIMIT: usize = 300;

pub const DENY_MESSAGE: &str = "The user denied this tool call.";
/// Before the user's text of a denial with text (T26, SPEC §10.1): Claude Code's own wording
/// of a rejection with feedback. Checked live on 2.1.308 with haiku: with it the model does
/// what the text says; with a plain «denied and wrote instead:» it saw the text but took it
/// for an instruction from the tool result and did not follow it.
pub const DENY_WITH_TEXT: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). To tell you how to proceed, the user said:";
pub const CLARIFY_CANCEL_MESSAGE: &str = "The user did not answer the questions.";
pub const POLICY_DENY_MESSAGE: &str = "Denied by the worker's permission policy";
/// `system.code` of a tool call refused by a deny rule of `[permissions]` (SPEC §4).
pub const POLICY_DENIED: &str = "policy_denied";
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
    },
    Clarify {
        input: Value,
        /// Question texts by position; `clarify.request` ids are `1`, `2`, ...
        questions: Vec<String>,
    },
}

#[derive(Debug)]
pub struct Core {
    session_id: Uuid,
    restricted: bool,
    held: Option<(Uuid, String)>,
    /// `event_id` of the input sent last: `in_reply_to` of agent messages.
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
            model: None,
            cost_total: None,
            commands: None,
            permission_mode: None,
            seen: Vec::new(),
        }
    }

    pub fn with_policy(mut self, policy: Arc<Policy>) -> Self {
        self.policy = policy;
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
        let (tool, input, local_rules, suggested) = match p {
            Pending::Approval {
                tool,
                input,
                local_rules,
                suggested,
            } => (tool, input, local_rules, suggested),
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
            "deny" => match message {
                Some(m) => deny(request_id, &format!("{DENY_WITH_TEXT}\n{m}")),
                None => deny(request_id, DENY_MESSAGE),
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
            Some(Pending::Clarify { input, questions }) => (input, questions),
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
                    let mut out = self.on_mode(&v["permissionMode"]);
                    out.extend(self.on_init(&v));
                    out
                }
                Some("status") => self.on_mode(&v["permissionMode"]),
                Some("permission_denied") => {
                    let mut out: Vec<Action> = self.flush(false).into_iter().collect();
                    out.extend(self.on_permission_denied(&v));
                    out
                }
                _ => vec![],
            },
            _ => vec![],
        }
    }

    /// The CLI's permission mode: `auto` is `auto`, any other (`default`, which `manual`
    /// shows as, or `acceptEdits`/`plan` the CLI may enter by itself) asks. Reported when it
    /// changes.
    fn on_mode(&mut self, v: &Value) -> Vec<Action> {
        let Some(m) = v.as_str() else {
            return vec![];
        };
        let mode = if m == "auto" {
            PermissionMode::Auto
        } else {
            PermissionMode::Ask
        };
        if self.permission_mode == Some(mode) {
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
            return self.on_mode(&r["response"]["mode"]);
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
                    self.held = Some((id, text(t)));
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
        let usage = &v["usage"];
        let num = |k: &str| usage[k].as_u64();
        let total = v["total_cost_usd"]
            .as_f64()
            .filter(|c| c.is_finite() && *c >= 0.0);
        let cost_usd = total.map(|t| self.turn_cost(t));
        out.push(self.emit_new(EventKind::Usage {
            input_tokens: num("input_tokens").unwrap_or(0),
            output_tokens: num("output_tokens").unwrap_or(0),
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
        let subtype = v["subtype"].as_str().unwrap_or("");
        out.push(self.emit_new(EventKind::Status {
            state: SessionState::Idle,
            // `error_max_turns`, `error_during_execution`, ...: why the turn ended early.
            detail: (subtype != "success" || v["is_error"] == true).then(|| subtype.to_owned()),
        }));
        // The CLI answered the input without echoing it: a local slash command (`/name`,
        // `/cost`, ...) runs without a model turn and without `--replay-user-messages`. The
        // result still means the input was taken; without this the supervisor resends it.
        while let Some(event_id) = self.awaiting_echo.pop_front() {
            tracing::info!(session_id = %self.session_id, %event_id, "input taken without an echo (local command)");
            out.push(Action::Emit(AdapterEvent::InputAccepted { event_id }));
        }
        out.push(Action::Emit(AdapterEvent::TurnEnded));
        if !self.pending.is_empty() {
            tracing::warn!(session_id = %self.session_id, open = self.pending.len(), "turn ended with open requests; dropped");
            self.pending.clear();
        }
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
        let policy_hint = self.policy.hint(&tool, &input);

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
            },
        );
        vec![self.emit_new(EventKind::ApprovalRequest {
            request_id,
            tool,
            description,
            options,
            expires_at: None,
            policy_hint,
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

    // ---- helpers -----------------------------------------------------------------------

    fn flush(&mut self, r#final: bool) -> Option<Action> {
        let (id, text) = self.held.take()?;
        Some(self.emit(
            id,
            EventKind::AgentMessage {
                text,
                r#final,
                in_reply_to: self.last_input,
            },
        ))
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
        Action::Emit(AdapterEvent::Event(new_event(
            self.session_id,
            Author::System,
            EventKind::System {
                text,
                code: code.into(),
                fork: None,
            },
        )))
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
        let options: Vec<String> = q["options"]
            .as_array()
            .map(|o| {
                o.iter()
                    .filter_map(|o| o["label"].as_str().map(text))
                    .collect()
            })
            .unwrap_or_default();
        questions.push(ClarifyQuestion {
            id: (i + 1).to_string(),
            text: text(raw),
            options: (!options.is_empty()).then_some(options),
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
