//! The I/O-free part of the Claude Code adapter: stream-json lines in, [`Action`]s out
//! (events for the supervisor, lines for the CLI's stdin). One [`Core`] per process, so
//! `session` rules live exactly as long as the process (SPEC §8.1).
//!
//! Mapping (SPEC §4, §8.1, docs/notes/claude-control-protocol.md):
//! - `stream_event` `text_delta` → `agent.delta`; deltas (text, thinking, tool input at four
//!   characters a token, redacted thinking at its `estimated_tokens`), then `message_delta`'s
//!   `usage.output_tokens` for the token rate ([`Core::take_output_tokens`], [`super::rate`]);
//! - `assistant` text → `agent.message`, held until the next element: another assistant
//!   block or a tool result makes it `final: false`, `result` makes it `final: true`;
//! - `assistant` `tool_use` → `tool.call` ([`args_preview`]: an `Agent` call's `prompt`
//!   last); non-empty `thinking` → `agent.thinking`;
//! - `assistant` text with `model` `<synthetic>` (the CLI's own reply: an API error, "Credit
//!   balance is too low") → `agent.message` with `runtime: true`, held and flushed as any;
//! - `system` `task_notification` (a background task ended) → `system task_notification`
//!   with its `status`, its `task_id` and the task's `tool_use_id` as `call_id`, except for a
//!   task that a subagent started or whose `task_started` said it runs in the foreground,
//!   until a `task_updated` moves it to the background; one without the subagent's report
//!   (`handback`) while the latest `background_tasks_changed` still holds the subagent, or a
//!   task of it, is no end: the latest one is handled when the subagent stops counting
//!   (below); a task gives one line, a later notification of it none;
//!   `system` `compact_boundary` → `system context_compacted`, held until the summary the CLI
//!   writes after it, the main agent's `user` line with `isSynthetic: true`, which gives its
//!   text; another main agent's `user`, `assistant`, `stream_event` or `result` line first, or
//!   the process's end, gives it "Context compacted";
//! - `system` `task_started` with `task_type: local_agent` (a subagent, in the background or
//!   not, a subagent's own too) until `task_notification` or a `task_updated` status other
//!   than `running` of the same `task_id`, a foreground one at the latest until the `result`,
//!   and any subagent the latest `background_tasks_changed` list holds or a task of it names
//!   as its `parent_task_id`, unless that subagent reported since its last `task_started`
//!   ([`Core::reported`]) → [`AdapterEvent::Subagents`] when the count changes, a drop that
//!   a `background_tasks_changed` brings at the next line of another kind: the CLI reports a
//!   subagent ended while it waits for a command moved to the background, and lists and
//!   starts its `task_id` again when the command ends (2.1.312);
//! - the same subagents one by one → [`AdapterEvent::LiveSubagents`] when they or a title
//!   change, and on every `system` `task_progress` of one of them: the title and the
//!   `tool_use_id` (`call_id`) from its `task_started`, what it does now (`description`),
//!   `total_tokens` and `tool_uses` from its latest `task_progress` (2.1.312); after them, in
//!   the same list, the main agent's background shell commands of the latest
//!   `background_tasks_changed` list (`task_type: local_bash`, not `ambient`, whose
//!   `task_started` has come and was not a `Monitor` call's, of no subagent that counts), each
//!   with its `description` and run time only, when they or a `description` change;
//! - `system` `background_tasks_changed` (`tasks`: the CLI's list of the session's running
//!   background tasks, subagents and commands, its subagents' too) →
//!   [`AdapterEvent::BackgroundTasks`] with their number when it changes;
//! - `system` `task_started` with `is_backgrounded: false`, `task_type` `local_bash` or
//!   `local_agent`, a `tool_use_id`, no `skip_transcript: true`, no `owned_by_subagent:
//!   true` and no `parent_task_id` (a Bash call or a subagent of the agent itself in the
//!   foreground, not of a subagent) until a `task_started` of the same `task_id` without all
//!   that, a `task_updated` with `patch.is_backgrounded: true` or `patch.status` `completed`,
//!   `failed` or `killed`, or its `task_notification` → [`AdapterEvent::ForegroundCalls`]
//!   with their `tool_use_id`s when they change (the rule of the VS Code tab's "Run in background", extension 2.1.319);
//! - `rate_limit_event` → [`AdapterEvent::Limits`] with the 5-hour and 7-day windows of its
//!   `rate_limit_info.unifiedWindows` (2.1.310: one before each turn's answer);
//! - `user` `tool_result` → `tool.result`; `user` with `isReplay: true` → input accepted;
//!   every user message written to stdin gets a fresh v4 `uuid`, which its echo and its
//!   `command_lifecycle` lines name (2.1.312);
//! - `result` → `usage` (one with `num_turns: 0`, no tokens and no `compact_boundary` in it
//!   gives none, or, when the total grew, one with no tokens, no `cost_usd` and the total),
//!   `status idle`, end of turn;
//!   `usage.cost_usd` is the growth of the CLI's cumulative `total_cost_usd` since the previous
//!   `usage` with a `cost_usd` (or since the transcript's last `cost-state` after `--resume`),
//!   `session_cost_usd` the total itself;
//!   a request of the main agent or of a foreground subagent still open then gets
//!   `approval.cancel` / `clarify.cancel` `stopped`, any other subagent's stays open until its
//!   `task_notification`, which cancels it; an input written during the turn and not echoed
//!   is taken, carried into the next turn or not taken by its last `command_lifecycle` (SPEC
//!   §17.2); a carried input `cancelled` after the `result` ends the turn it was carried
//!   into; a carried local command is taken by its own `result`;
//! - `system` `init` → the model, and [`AdapterEvent::Commands`] when its `slash_commands`
//!   differ from what this process reported (the CLI sends `init` before every turn); one
//!   while no turn of ours runs (a turn the CLI starts by itself) → `status running` first;
//! - `control_request` `can_use_tool` → `approval.request` (with `policy_hint` and the hqc
//!   rules suggested for the call) / `clarify.request`, an immediate allow by a `session`
//!   rule or by the folder's hqc rules (T48), or an immediate deny plus `system
//!   policy_denied` when a deny rule of `[permissions]` matches (checked first);
//! - `system` `permission_denied` (the CLI refused a call by its rules, no request) →
//!   `system policy_denied`;
//! - `permissionMode` of `system` `init` and `system` `status`, and the CLI's answer to our
//!   `set_permission_mode` → [`AdapterEvent::PermissionMode`] when it changes (T38), and
//!   on every success of our `set_permission_mode` (T39);
//! - the CLI's answers to our `set_model` and `background_tasks` are only logged;
//! - other `control_response` lines are the CLI's echo of ours and are skipped.
//!
//! U+0000 in any text taken from the CLI (binary tool output) becomes U+FFFD ([`text`],
//! [`preview`]): the hub's Postgres refuses it (SPEC §5.2).
//!
//! Messages of subagents (`parent_tool_use_id` set) are not mapped: their records live in
//! separate sidechain files, not in the session transcript.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, bail};
use hqc_proto::event::{
    ApprovalCanonical, ApprovalOption, Author, CancelReason, ClarifyAnswer, ClarifyQuestion, Event,
    EventKind, LiveSubagent, SessionState,
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
/// `request_id` prefix of our `set_model` control requests.
const MODEL_REQUEST: &str = "hqc-model-";
/// `request_id` prefix of our `background_tasks` control requests.
const BACKGROUND_REQUEST: &str = "hqc-background-";
/// `patch.status` of a `task_updated` that ends a foreground call ([`Core::foreground`]).
const CALL_ENDED: [&str; 3] = ["completed", "failed", "killed"];

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

/// A user message written to stdin whose `--replay-user-messages` echo has not come yet.
#[derive(Debug)]
struct Written {
    /// The line's own `uuid`, fresh v4 (not the `event_id`: the CLI silently drops a line
    /// whose `uuid` it has seen, 2.1.312).
    uuid: Uuid,
    event_id: Uuid,
    /// Written into the turn running then (SPEC §17.2).
    mid_turn: bool,
    /// The last `state` of its `command_lifecycle`: `queued`, `started`, `completed`,
    /// `cancelled`; none yet, or a CLI without those lines.
    state: Option<String>,
    /// A turn ended without taking it: the CLI runs it next by itself.
    carried: bool,
}

/// A subagent of [`Core::live`], or a command of [`Core::live_commands`].
#[derive(Debug)]
struct LiveAgent {
    task_id: String,
    /// `description` of its `task_started`; empty while the CLI's list counts it before that.
    /// A command's: `description` of its entry in the CLI's list.
    title: String,
    /// Its first `task_started`, or the line that counted it first when that came earlier.
    /// A command's: its first `task_started`.
    since: Instant,
    /// `description` of its latest `task_progress`: what it does now.
    activity: Option<String>,
    total_tokens: Option<u64>,
    tool_uses: Option<u64>,
    /// `tool_use_id` of its latest `task_started` ([`Core::task_calls`]); none before that.
    call_id: Option<String>,
}

impl LiveAgent {
    fn new(task_id: &str, title: &str, call_id: Option<&str>) -> Self {
        Self {
            task_id: task_id.to_owned(),
            title: text(title),
            since: Instant::now(),
            activity: None,
            total_tokens: None,
            tool_uses: None,
            call_id: call_id.map(str::to_owned),
        }
    }
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
    /// Event id of the `context_compacted` of a `compact_boundary`, held for the summary that
    /// follows it ([`Core::release_compaction`]).
    compaction: Option<Uuid>,
    /// `event_id` of the input of the turn: `in_reply_to` of agent messages once its echo has
    /// come ([`Core::reply_to`]).
    last_input: Option<Uuid>,
    /// Inputs written to stdin whose echo has not come yet, in the order written.
    awaiting_echo: VecDeque<Written>,
    /// An input was written to this process.
    wrote: bool,
    /// The CLI sends `command_lifecycle` lines (2.1.312).
    lifecycle: bool,
    /// `task_id`s of the subagents the CLI reports running now.
    subagents: HashSet<String>,
    /// The subagent count last reported ([`Core::running_subagents`]).
    subagents_reported: usize,
    /// The subagents of the count last reported, in the order they started, with what the CLI
    /// said of each ([`AdapterEvent::LiveSubagents`]).
    live: Vec<LiveAgent>,
    /// The main agent's background shell commands of [`Core::background`], in the order they
    /// became rows, sent after [`Core::live`] ([`Core::update_live_commands`]). Apart from it:
    /// they are no subagents and never change the subagent count.
    live_commands: Vec<LiveAgent>,
    /// Ids of the `Monitor` `tool_use` blocks of `assistant` lines, the agent's and its
    /// subagents', until the `task_started` that names one in `tool_use_id`.
    monitor_calls: HashSet<String>,
    /// `local_bash` tasks whose `task_started` came, `true` for a Monitor's (2.1.312 lists a
    /// Monitor's task as `local_bash` without `ambient`, nothing in the list tells it from a
    /// command), and when the first came, until the task's `task_notification`.
    bash_tasks: HashMap<String, (bool, Instant)>,
    /// [`Core::live`] got a subagent or a title since it was last reported.
    live_changed: bool,
    /// A `background_tasks_changed` lowered [`Core::running_subagents`] below the count last
    /// reported: the next line of another kind reports it.
    recount: bool,
    /// `tasks` of the latest `system` `background_tasks_changed`.
    background: Vec<Value>,
    in_turn: bool,
    /// A turn the CLI started by itself (`system/init` while no turn of ours runs, e.g. after
    /// a background task's `task_notification`), until its `result`: Stop interrupts it too.
    self_turn: bool,
    turn: u64,
    pending: HashMap<String, Pending>,
    rules: SessionRules,
    /// `[permissions]` of the worker config, for `policy_hint` and denying what a deny
    /// rule matches.
    policy: Arc<Policy>,
    /// hqc's allow rules of the session's workspace and its path (T48); `None` without one.
    folder: Option<(FolderRules, std::path::PathBuf)>,
    model: Option<String>,
    /// The session's cumulative cost as of the last `usage` with a `cost_usd` (or the
    /// transcript's last `cost-state` when resumed): the baseline of the next turn's cost.
    cost_total: Option<f64>,
    /// A `compact_boundary` came since the last `result`: a `result` with no model turn costs
    /// for the compaction itself.
    compacted: bool,
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
    /// The latest `task_notification` without a report (`handback`) of each subagent the
    /// latest `background_tasks_changed` still held ([`Core::listed`]) when it came: not its
    /// end yet. It is handled as the end when the subagent stops counting
    /// ([`Core::report_subagents`]) or the process exits.
    held_ends: BTreeMap<String, Value>,
    /// Subagents whose latest `task_notification` carried their report (`handback`), until
    /// the CLI starts them again: a task of theirs still listed is the session's background
    /// work, it does not count them as running ([`Core::running_subagents`]).
    reported: HashSet<String>,
    /// Tasks whose end gave its line: a later `task_notification` of the same `task_id` gives
    /// none (2.1.312 sends one each time a subagent stops, also after its report when a command
    /// it left running ends and the CLI resumes it; "the later one replaces what the earlier one
    /// showed", which an append-only feed cannot do).
    announced: HashSet<String>,
    /// The main agent's output tokens since the last [`Core::take_output_tokens`]: those of
    /// the response being written as estimated from its deltas, the rest of its
    /// `usage.output_tokens` at its end.
    output_tokens: u64,
    /// The response being written: characters of its deltas, the CLI's estimates of its
    /// redacted thinking, and the tokens counted for it so far.
    streamed_chars: u64,
    streamed_estimated: u64,
    streamed: u64,
    /// `session_id` of the latest `system/init`: its subagents' transcripts are in its folder.
    cli_session: Option<String>,
    /// The Bash calls and subagents of the agent itself the CLI runs in the foreground, by
    /// `task_id`: their `tool_use_id`, which `background_tasks` takes
    /// ([`AdapterEvent::ForegroundCalls`]).
    foreground: BTreeMap<String, String>,
    /// `tool_use_id` of each task's latest `task_started`, by `task_id`, until its end gives
    /// its line or none ([`Core::on_task_ended`]): the `call_id` of its row above the input and
    /// of its `task_notification` line when the notification names none.
    task_calls: HashMap<String, String>,
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
            compaction: None,
            last_input: None,
            awaiting_echo: VecDeque::new(),
            wrote: false,
            lifecycle: false,
            subagents: HashSet::new(),
            subagents_reported: 0,
            live: Vec::new(),
            live_commands: Vec::new(),
            monitor_calls: HashSet::new(),
            bash_tasks: HashMap::new(),
            live_changed: false,
            recount: false,
            background: Vec::new(),
            in_turn: false,
            self_turn: false,
            turn: 0,
            pending: HashMap::new(),
            rules: SessionRules::default(),
            policy: Arc::default(),
            folder: None,
            model: None,
            cost_total: None,
            compacted: false,
            commands: None,
            permission_mode: None,
            seen: Vec::new(),
            quiet_tasks: HashMap::new(),
            held_ends: BTreeMap::new(),
            reported: HashSet::new(),
            announced: HashSet::new(),
            output_tokens: 0,
            streamed_chars: 0,
            streamed_estimated: 0,
            streamed: 0,
            cli_session: None,
            foreground: BTreeMap::new(),
            task_calls: HashMap::new(),
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
        (self.in_turn || self.self_turn) && self.turn == turn
    }

    /// Whether a turn the CLI started by itself runs, until its `result`.
    pub fn runs_own_turn(&self) -> bool {
        self.self_turn
    }

    pub fn session_rules(&self) -> usize {
        self.rules.len()
    }

    /// The main agent's output tokens since the last call (thinking and tool input included):
    /// those of the response being written as estimated from its deltas, the rest of its
    /// `usage.output_tokens` at its end.
    pub fn take_output_tokens(&mut self) -> u64 {
        std::mem::take(&mut self.output_tokens)
    }

    /// A turn runs, or a subagent does.
    pub fn working(&self) -> bool {
        self.in_turn || self.self_turn || !self.live.is_empty()
    }

    /// The CLI's session id, from its latest `system/init`.
    pub fn cli_session(&self) -> Option<&str> {
        self.cli_session.as_deref()
    }

    /// The `task_id`s of the subagents of [`Core::live`].
    pub fn live_ids(&self) -> Vec<String> {
        self.live.iter().map(|a| a.task_id.clone()).collect()
    }

    // ---- from the supervisor -------------------------------------------------------

    pub fn input(&mut self, input: &SessionInput) -> Vec<Action> {
        self.input_with_images(input, vec![])
    }

    /// The user message of stream-json: the text, or with images a content array of a text
    /// block followed by `images` (image blocks of the Messages API, SPEC §8.1). The
    /// artifacts' paths are already in `input.text` (`.hqc/inbox`, SPEC §12).
    pub fn input_with_images(&mut self, input: &SessionInput, images: Vec<Value>) -> Vec<Action> {
        // With `command_lifecycle` the CLI's `started` names the turn's input: one written
        // during a turn the CLI began by itself runs only after it (SPEC §17.2).
        if !self.wrote || !self.lifecycle {
            self.last_input = Some(input.event_id);
        }
        self.in_turn = true;
        self.turn += 1;
        self.held = None;
        vec![self.write(input, images, false)]
    }

    /// A person's input may go into the running turn (SPEC §17.2): a turn runs and no
    /// permission request or question is open.
    pub fn can_inject(&self) -> bool {
        self.in_turn && self.pending.is_empty()
    }

    /// [`Core::input_with_images`] into the running turn; `None` unless
    /// [`Core::can_inject`]. The CLI takes the line at its next step, or runs it next by
    /// itself when the turn ends first; it stays out of `in_reply_to` until then.
    pub fn inject_with_images(
        &mut self,
        input: &SessionInput,
        images: Vec<Value>,
    ) -> Option<Vec<Action>> {
        if !self.can_inject() {
            return None;
        }
        Some(vec![self.write(input, images, true)])
    }

    /// The user message of stream-json, with a fresh `uuid` its echo and `command_lifecycle`
    /// will name.
    fn write(&mut self, input: &SessionInput, images: Vec<Value>, mid_turn: bool) -> Action {
        // `peer_prefix` and `peer` are in `text` already (crate::peer, SPEC §7.1).
        let uuid = Uuid::new_v4();
        self.wrote = true;
        self.awaiting_echo.push_back(Written {
            uuid,
            event_id: input.event_id,
            mid_turn,
            state: None,
            carried: false,
        });
        let content = if images.is_empty() {
            json!(input.text)
        } else {
            let mut blocks = vec![json!({"type": "text", "text": input.text})];
            blocks.extend(images);
            Value::Array(blocks)
        };
        Action::Write(json!({
            "type": "user",
            "uuid": uuid,
            "message": {"role": "user", "content": content},
        }))
    }

    /// `control_request` `interrupt` for the running turn, also one the CLI started by
    /// itself; `None` if no turn runs.
    pub fn interrupt(&mut self) -> Option<(Action, u64)> {
        if !self.in_turn && !self.self_turn {
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
    /// CLI's `default`, the name it reports for `manual` too; `allow_all` is
    /// `bypassPermissions`, which the CLI takes only from a process started with
    /// `--allow-dangerously-skip-permissions` or in that mode.
    pub fn set_permission_mode(&mut self, mode: PermissionMode) -> Action {
        Action::Write(json!({
            "type": "control_request",
            "request_id": format!("{MODE_REQUEST}{}", Uuid::now_v7()),
            "request": {"subtype": "set_permission_mode", "mode": match mode {
                PermissionMode::Ask => "default",
                PermissionMode::Auto => "auto",
                PermissionMode::AllowAll => "bypassPermissions",
            }},
        }))
    }

    /// Switch the running process to `model`: `control_request {subtype: set_model}`. The CLI
    /// answers `success` with the model it resolved the name to, and its next model call uses
    /// it, the next one of a running turn too (checked on 2.1.312).
    pub fn set_model(&self, model: &str) -> Action {
        Action::Write(json!({
            "type": "control_request",
            "request_id": format!("{MODEL_REQUEST}{}", Uuid::now_v7()),
            "request": {"subtype": "set_model", "model": model},
        }))
    }

    /// Move the foreground Bash call or subagent of tool call `call_id` to the background:
    /// `control_request {subtype: background_tasks, tool_use_id}`. The CLI answers `success`
    /// with `{backgrounded}` (`false`: no such call in the foreground), or an error while
    /// background tasks are disabled; the call itself gets its tool result ("Command was
    /// manually backgrounded by user with ID: …" for Bash, `async_launched` for a subagent) and
    /// the agent goes on (2.1.312).
    pub fn background(&self, call_id: &str) -> Action {
        Action::Write(json!({
            "type": "control_request",
            "request_id": format!("{BACKGROUND_REQUEST}{}", Uuid::now_v7()),
            "request": {"subtype": "background_tasks", "tool_use_id": call_id},
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
        // A background subagent's request outlives the main turn's `result` (`on_result`): its
        // "No" must not interrupt a turn that runs by then for something else.
        let foreground = agent_id
            .as_deref()
            .is_none_or(|a| self.quiet_tasks.get(a) == Some(&false));
        let resp = match option_id {
            "once" => allow(request_id, &input, None),
            // As the VS Code tab: "No" stops the turn, a text answer lets it go on.
            "deny" => match message {
                Some(m) => deny_with(request_id, &format!("{DENY_WITH_TEXT}{m}"), false),
                None => deny_with(request_id, DENY_MESSAGE, foreground),
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

    /// The process is gone: a held compaction and a held text are sent as they are, open
    /// requests are dropped.
    pub fn on_exit(&mut self) -> Vec<Action> {
        self.pending.clear();
        self.awaiting_echo.clear();
        self.in_turn = false;
        self.self_turn = false;
        let mut out: Vec<Action> = self.release_compaction(None).into_iter().collect();
        out.extend(self.flush(false));
        // A subagent end held while the subagent still counted ([`Core::held_ends`]) is its
        // last word now: the process that ran it is gone.
        for (_, v) in std::mem::take(&mut self.held_ends) {
            out.extend(self.on_task_ended(&v));
        }
        out
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
        // A Monitor's task is listed as a command ([`Core::bash_tasks`]), a subagent's too.
        if ty == "assistant" {
            for b in blocks(&v) {
                if b["type"] == "tool_use" && b["name"] == "Monitor" {
                    self.monitor_calls
                        .extend(b["id"].as_str().map(str::to_owned));
                }
            }
        }
        // A held compaction gets the summary from the main agent's synthetic `user` line;
        // another line of the main conversation comes without one. `system` lines and a
        // subagent's lines keep it held.
        let mut out: Vec<Action> = match ty {
            "user" if !sub && v["isSynthetic"] == true => {
                self.release_compaction(Some(&v["message"]["content"]))
            }
            "user" | "assistant" | "stream_event" | "result" if !sub => {
                self.release_compaction(None)
            }
            _ => None,
        }
        .into_iter()
        .collect();
        out.extend(match ty {
            "stream_event" if !sub => self.on_stream_event(&v),
            "assistant" if !sub => self.on_assistant(&v),
            "user" if v["isReplay"] == true => self.on_echo(&v),
            "user" if !sub => self.on_user(&v),
            "command_lifecycle" => self.on_lifecycle(&v),
            "rate_limit_event" => limits(&v["rate_limit_info"]["unifiedWindows"])
                .map(|l| Action::Emit(AdapterEvent::Limits(l)))
                .into_iter()
                .collect(),
            "result" => self.on_result(&v),
            "control_request" => self.on_control_request(&v),
            "control_response" => self.on_control_response(&v),
            "system" => match v["subtype"].as_str() {
                Some("init") => {
                    // The CLI sends `init` before every turn; one without ours is its own, and
                    // `status running` opens it on the hub, which saw no input start it
                    // (SPEC §17.1).
                    self.self_turn = !self.in_turn;
                    let mut out = Vec::new();
                    if self.self_turn {
                        out.push(self.emit_new(EventKind::Status {
                            state: SessionState::Running,
                            detail: None,
                            in_reply_to: None,
                        }));
                    }
                    self.model = v["model"].as_str().map(str::to_owned);
                    self.cli_session = v["session_id"].as_str().map(str::to_owned);
                    out.extend(self.on_mode(&v["permissionMode"], false));
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
                    let call = v["tool_use_id"].as_str();
                    if let (Some(id), Some(call)) = (v["task_id"].as_str(), call) {
                        self.task_calls.insert(id.to_owned(), call.to_owned());
                    }
                    let agent = v["task_type"] == "local_agent";
                    if let Some(id) = v["task_id"].as_str().filter(|_| agent) {
                        self.reported.remove(id);
                        self.live_started(id, v["description"].as_str().unwrap_or(""), call);
                    }
                    let mut out =
                        self.count_subagent(v["task_id"].as_str().filter(|_| agent), true);
                    if let Some(id) = v["task_id"]
                        .as_str()
                        .filter(|_| v["task_type"] == "local_bash")
                    {
                        let monitor = v["tool_use_id"]
                            .as_str()
                            .is_some_and(|c| self.monitor_calls.remove(c));
                        self.bash_tasks
                            .entry(id.to_owned())
                            .or_insert((monitor, Instant::now()));
                        // A command's row comes now, with the count, as the list's.
                        out.extend(self.report_subagents(None));
                    }
                    out.extend(self.foreground_started(&v));
                    out
                }
                Some("task_progress") => self.on_task_progress(&v),
                // Its end comes before the task's `task_notification` (2.1.312).
                Some("task_updated") => {
                    let moved = v["patch"]["is_backgrounded"] == true;
                    if let Some(id) = v["task_id"].as_str().filter(|_| moved) {
                        if self.quiet_tasks.get(id) == Some(&false) {
                            self.quiet_tasks.remove(id);
                        }
                    }
                    let ended = v["patch"]["status"]
                        .as_str()
                        .is_some_and(|s| s != "running");
                    let mut out =
                        self.count_subagent(v["task_id"].as_str().filter(|_| ended), false);
                    let call_ended = v["patch"]["status"]
                        .as_str()
                        .is_some_and(|s| CALL_ENDED.contains(&s));
                    if let Some(id) = v["task_id"].as_str().filter(|_| moved || call_ended) {
                        out.extend(self.foreground_ended(id));
                    }
                    out
                }
                Some("task_notification") => {
                    // A `local_bash` task that ended is no row any more.
                    if let Some(id) = v["task_id"].as_str() {
                        self.bash_tasks.remove(id);
                    }
                    let mut out = self.on_task_notification(&v);
                    out.extend(self.count_subagent(v["task_id"].as_str(), false));
                    if let Some(id) = v["task_id"].as_str() {
                        out.extend(self.foreground_ended(id));
                    }
                    out
                }
                Some("background_tasks_changed") => self.on_background_tasks(&v),
                // Its uuid is that of the transcript's `system compact_boundary` record
                // (fixture `resume-compact`): the event id is derived from it as from a
                // record's. The event waits for the summary ([`Core::compaction`]).
                Some("compact_boundary") => {
                    self.compacted = true;
                    tracing::info!(session_id = %self.session_id, trigger = ?v["compact_metadata"]["trigger"].as_str(), "context compacted");
                    self.compaction = Some(
                        record_uuid(&v).map_or_else(Uuid::now_v7, |u| record_event_id(&u, 0)),
                    );
                    vec![]
                }
                _ => vec![],
            },
            _ => vec![],
        });
        // A drop a `background_tasks_changed` left for a line of another kind (this one).
        if self.recount && !(ty == "system" && v["subtype"] == "background_tasks_changed") {
            out.extend(self.report_subagents(None));
        }
        out
    }

    /// The CLI's permission mode: `auto` is `auto`, `bypassPermissions` is `allow_all`, any
    /// other (`default`, which `manual` shows as, or `acceptEdits`/`plan` the CLI may enter
    /// by itself) asks. Reported when it changes, and on every confirmation of a switch of
    /// ours (`confirm`): the supervisor waits for that before it hands over a capped turn
    /// (T39), also when the process already ran in the cap's mode because a switch back up
    /// was refused.
    fn on_mode(&mut self, v: &Value, confirm: bool) -> Vec<Action> {
        let Some(m) = v.as_str() else {
            return vec![];
        };
        let mode = match m {
            "auto" => PermissionMode::Auto,
            "bypassPermissions" => PermissionMode::AllowAll,
            _ => PermissionMode::Ask,
        };
        if self.permission_mode == Some(mode) && !confirm {
            return vec![];
        }
        tracing::info!(session_id = %self.session_id, mode = m, "permission mode");
        self.permission_mode = Some(mode);
        vec![Action::Emit(AdapterEvent::PermissionMode(mode))]
    }

    /// The CLI's answer to our `set_permission_mode`, `set_model` or `background_tasks` (the
    /// latter two logged); every other `control_response` is the echo of one of ours.
    fn on_control_response(&mut self, v: &Value) -> Vec<Action> {
        let r = &v["response"];
        let id = r["request_id"].as_str().unwrap_or("");
        if id.starts_with(BACKGROUND_REQUEST) {
            if r["subtype"] == "success" && r["response"]["backgrounded"] != false {
                tracing::info!(session_id = %self.session_id, request_id = id, "call moved to the background");
            } else {
                tracing::warn!(session_id = %self.session_id, request_id = id, error = %r["error"], "claude did not move the call to the background");
            }
            return vec![];
        }
        if id.starts_with(MODEL_REQUEST) {
            if r["subtype"] == "success" {
                tracing::info!(session_id = %self.session_id, model = ?r["response"]["model"].as_str(), "model switched");
            } else {
                tracing::warn!(session_id = %self.session_id, error = %r["error"], "claude refused the model switch");
            }
            return vec![];
        }
        if !id.starts_with(MODE_REQUEST) {
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
        match ev["type"].as_str() {
            Some("message_start") => {
                (self.streamed_chars, self.streamed_estimated, self.streamed) = (0, 0, 0)
            }
            // The exact count comes once per response, at its end: it adds what the estimate
            // of its deltas missed.
            Some("message_delta") => {
                let n = ev["usage"]["output_tokens"].as_u64().unwrap_or(0);
                self.output_tokens += n.saturating_sub(self.streamed);
                (self.streamed_chars, self.streamed_estimated, self.streamed) = (0, 0, 0);
            }
            _ => {}
        }
        if ev["type"] != "content_block_delta" {
            return vec![];
        }
        // While the response is written, its tokens are estimated, so a long answer speeds the
        // rate up as it streams, not after it: a redacted thinking delta carries the CLI's own
        // estimate of its tokens (`estimated_tokens`, an increment), any other delta its text,
        // thinking summary or tool input at four characters a token, as the CLI counts.
        let delta = &ev["delta"];
        match delta["estimated_tokens"].as_u64() {
            Some(n) => self.streamed_estimated += n,
            None => {
                let chunk = ["text", "thinking", "partial_json"]
                    .iter()
                    .find_map(|f| delta[f].as_str())
                    .unwrap_or("");
                self.streamed_chars += chunk.chars().count() as u64;
            }
        }
        let estimate = self.streamed_estimated + self.streamed_chars / 4;
        self.output_tokens += estimate - self.streamed;
        self.streamed = estimate;
        if delta["type"] == "text_delta" {
            if let Some(t) = delta["text"].as_str().filter(|t| !t.is_empty()) {
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
                            args_preview: args_preview(
                                block["name"].as_str().unwrap_or(""),
                                &block["input"],
                            ),
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

    /// The echo of the input written with its `uuid`. A CLI without `command_lifecycle` may
    /// not keep our `uuid` (the T0 fixtures, 2.1.307 and 2.1.308): there it is the oldest
    /// input's echo. An input that is no write into the running turn becomes the turn's input
    /// here as by its `started`: taken into a turn the CLI began by itself, the echo comes
    /// first and `started` finds nothing left.
    fn on_echo(&mut self, v: &Value) -> Vec<Action> {
        let at = record_uuid(v)
            .and_then(|u| self.awaiting_echo.iter().position(|w| w.uuid == u))
            .or_else(|| (!self.lifecycle && !self.awaiting_echo.is_empty()).then_some(0));
        match at.and_then(|i| self.awaiting_echo.remove(i)) {
            Some(w) => {
                if self.lifecycle && (!w.mid_turn || w.carried) {
                    self.last_input = Some(w.event_id);
                }
                vec![Action::Emit(AdapterEvent::InputAccepted {
                    event_id: w.event_id,
                })]
            }
            None => {
                tracing::warn!(session_id = %self.session_id, "replay echo without a sent input");
                vec![]
            }
        }
    }

    /// `command_lifecycle {command_uuid, state}` (2.1.312) of an input not echoed yet:
    /// `queued` when the CLI read the line, `started` when it took it (right after the echo
    /// when it went into a running turn), `completed` or `cancelled`. `started` of an input
    /// that begins a turn of its own makes it the turn's input. A carried input `cancelled`
    /// after the `result` that carried it (a second Stop before it started) ends the turn it
    /// was carried into, for which no `result` comes: [`AdapterEvent::TurnEnded`] with it in
    /// `dropped`, the other carried inputs in `carried`. One `completed` without an echo (a
    /// local command) is taken by the `result` the CLI sends for it: a local command has a
    /// `result` of its own (`/name` without an echo, fixture of
    /// `local_command_without_echo_is_taken`; `/cost` on 2.1.312).
    fn on_lifecycle(&mut self, v: &Value) -> Vec<Action> {
        self.lifecycle = true;
        let Some(uuid) = v["command_uuid"]
            .as_str()
            .and_then(|u| u.parse::<Uuid>().ok())
        else {
            return vec![];
        };
        let Some(i) = self.awaiting_echo.iter().position(|w| w.uuid == uuid) else {
            return vec![];
        };
        let w = &mut self.awaiting_echo[i];
        let state = v["state"].as_str().unwrap_or_default();
        if state == "started" && (!w.mid_turn || w.carried) {
            self.last_input = Some(w.event_id);
        }
        w.state = Some(state.to_owned());
        if !(w.carried && state == "cancelled") {
            return vec![];
        }
        let Some(w) = self.awaiting_echo.remove(i) else {
            return vec![];
        };
        tracing::info!(session_id = %self.session_id, event_id = %w.event_id, "carried input cancelled by the CLI, not taken");
        let dropped = vec![w.event_id];
        let carried: Vec<Uuid> = self
            .awaiting_echo
            .iter()
            .filter(|w| w.carried)
            .map(|w| w.event_id)
            .collect();
        // As at `result`: the CLI goes on with the other carried inputs.
        self.in_turn = !carried.is_empty();
        if self.in_turn {
            self.turn += 1;
        }
        vec![Action::Emit(AdapterEvent::TurnEnded { carried, dropped })]
    }

    /// Subagent `task_id` started (`add`) or ended by the CLI's word: the new count when it
    /// changed.
    fn count_subagent(&mut self, task_id: Option<&str>, add: bool) -> Vec<Action> {
        let Some(id) = task_id else {
            return vec![];
        };
        let changed = if add {
            self.subagents.insert(id.to_owned())
        } else {
            self.subagents.remove(id)
        };
        if !changed {
            return vec![];
        }
        self.report_subagents(Some(id))
    }

    /// `system/background_tasks_changed` (2.1.312): `tasks`, the CLI's whole list of the
    /// session's running background tasks, sent on every change and `[]` at the end. Their
    /// number when it changed; a subagent count that went up. One that went down, or a subagent
    /// of [`Core::held_ends`] that stopped counting, waits for the
    /// next line of another kind ([`Core::recount`]): when the last command of a subagent the
    /// CLI already called ended leaves the list, the CLI lists the subagent again only on the
    /// next line, `[]` then `[subagent]`, and then starts it again (SPEC §8.1 **[verify]**:
    /// inferred from a capture where a second task of the subagent kept the list non-empty).
    fn on_background_tasks(&mut self, v: &Value) -> Vec<Action> {
        let before = self.background.len();
        self.background = v["tasks"].as_array().cloned().unwrap_or_default();
        let mut out = Vec::new();
        if self.background.len() != before {
            let n = u32::try_from(self.background.len()).unwrap_or(u32::MAX);
            tracing::info!(session_id = %self.session_id, background_tasks = n, "background tasks");
            out.push(Action::Emit(AdapterEvent::BackgroundTasks(n)));
        }
        if self.running_subagents() < self.subagents_reported
            || self.held_ends.keys().any(|id| !self.counts(id))
        {
            self.recount = true;
        } else {
            out.extend(self.report_subagents(None));
        }
        out
    }

    /// `system/task_started`: a Bash call or a subagent of the agent itself in the foreground
    /// (`is_backgrounded: false`, `tool_use_id`, `task_type` `local_bash` or `local_agent`, no
    /// `skip_transcript: true`, no `owned_by_subagent: true`, no `parent_task_id`: the CLI marks
    /// a subagent's Bash calls `owned_by_subagent` and gives its subagents the `parent_task_id`
    /// of their parent) joins [`Core::foreground`], any other task of that `task_id` leaves it;
    /// [`AdapterEvent::ForegroundCalls`] when it changed.
    fn foreground_started(&mut self, v: &Value) -> Vec<Action> {
        let Some(id) = v["task_id"].as_str() else {
            return vec![];
        };
        let call = v["tool_use_id"].as_str().filter(|_| {
            v["is_backgrounded"] == false
                && v["skip_transcript"] != true
                && v["owned_by_subagent"] != true
                && v["parent_task_id"].is_null()
                && (v["task_type"] == "local_bash" || v["task_type"] == "local_agent")
        });
        let Some(call) = call else {
            return self.foreground_ended(id);
        };
        if self.foreground.get(id).map(String::as_str) == Some(call) {
            return vec![];
        }
        self.foreground.insert(id.to_owned(), call.to_owned());
        vec![self.foreground_calls()]
    }

    /// Task `id` left the foreground (moved, ended, or started again in the background):
    /// [`AdapterEvent::ForegroundCalls`] if it was there.
    fn foreground_ended(&mut self, id: &str) -> Vec<Action> {
        if self.foreground.remove(id).is_none() {
            return vec![];
        }
        vec![self.foreground_calls()]
    }

    /// [`Core::foreground`] as sent: the `tool_use_id`s.
    fn foreground_calls(&self) -> Action {
        Action::Emit(AdapterEvent::ForegroundCalls(
            self.foreground.values().cloned().collect(),
        ))
    }

    /// The subagents at work: those the CLI reports running, and those the latest
    /// `background_tasks_changed` list holds (`task_type: local_agent`) or a task of it belongs
    /// to (its `parent_task_id`). With `CLAUDE_CODE_ENTRYPOINT=claude-vscode` 2.1.312 moves a
    /// subagent's Bash call that runs for more than about 180 s to the background (the agent's
    /// own stays in the foreground) and reports the subagent ended (`task_updated`
    /// `completed`, `task_notification` "This agent has not reported yet: it is waiting on its
    /// own background work…") while the command still runs; when it ends, the CLI starts the
    /// same `task_id` again. A task of a subagent in [`Core::reported`] does not count it: the
    /// subagent handed its report over and left the task running.
    fn running_subagents(&self) -> usize {
        self.running_subagent_ids().len()
    }

    /// The `task_id`s of [`Core::running_subagents`].
    fn running_subagent_ids(&self) -> HashSet<&str> {
        let mut ids: HashSet<&str> = self.subagents.iter().map(String::as_str).collect();
        for t in &self.background {
            if t["task_type"] == "local_agent" {
                ids.extend(t["task_id"].as_str());
            }
            ids.extend(self.parent(t));
        }
        ids
    }

    /// The subagent listed task `t` belongs to, unless that subagent has reported.
    fn parent<'a>(&self, t: &'a Value) -> Option<&'a str> {
        t["parent_task_id"]
            .as_str()
            .filter(|p| !self.reported.contains(*p))
    }

    /// The latest `background_tasks_changed` list holds subagent `id` or a task of it.
    fn listed(&self, id: &str) -> bool {
        self.background.iter().any(|t| {
            (t["task_type"] == "local_agent" && t["task_id"] == id) || self.parent(t) == Some(id)
        })
    }

    /// Subagent `id` is one of [`Core::running_subagents`].
    fn counts(&self, id: &str) -> bool {
        self.subagents.contains(id) || self.listed(id)
    }

    /// [`AdapterEvent::Subagents`] when [`Core::running_subagents`] differs from the count
    /// last reported, their list ([`Core::report_live`]), then the end of each subagent in
    /// [`Core::held_ends`] that no longer counts; `task_id`: the subagent whose start or end
    /// the CLI reported, for the log.
    fn report_subagents(&mut self, task_id: Option<&str>) -> Vec<Action> {
        self.recount = false;
        let n = self.running_subagents();
        let mut out = Vec::new();
        if n != self.subagents_reported {
            self.subagents_reported = n;
            let n = u32::try_from(n).unwrap_or(u32::MAX);
            tracing::info!(session_id = %self.session_id, task_id, subagents = n, "subagents");
            out.push(Action::Emit(AdapterEvent::Subagents(n)));
        }
        out.extend(self.report_live());
        let (ended, held): (BTreeMap<_, _>, BTreeMap<_, _>) = std::mem::take(&mut self.held_ends)
            .into_iter()
            .partition(|(id, _)| !self.counts(id));
        self.held_ends = held;
        for v in ended.into_values() {
            out.extend(self.on_task_ended(&v));
        }
        out
    }

    /// [`AdapterEvent::LiveSubagents`] when the subagents of [`Core::running_subagents`] or a
    /// title differ from those last reported, or [`Core::live_commands`] changed.
    /// [`Core::live`] keeps exactly those subagents; one the CLI's list counts before its
    /// `task_started` has no title until that comes.
    fn report_live(&mut self) -> Option<Action> {
        let ids: HashSet<String> = self
            .running_subagent_ids()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let before = self.live.len();
        self.live.retain(|a| ids.contains(&a.task_id));
        let mut changed = std::mem::take(&mut self.live_changed) || self.live.len() != before;
        let mut new: Vec<&String> = ids
            .iter()
            .filter(|id| !self.live.iter().any(|a| &a.task_id == *id))
            .collect();
        new.sort();
        for id in new {
            let call = self.task_calls.get(id).map(String::as_str);
            self.live.push(LiveAgent::new(id, "", call));
            changed = true;
        }
        changed |= self.update_live_commands();
        changed.then(|| self.live_list())
    }

    /// Brings [`Core::live_commands`] to the main agent's background shell commands of the
    /// latest `background_tasks_changed` list: `task_type: local_bash`, not `ambient` (no
    /// activity, the SDK says), its `task_started` come and not a Monitor's
    /// ([`Core::bash_tasks`]; a task joins the list before its `task_started`, so a Monitor
    /// never shows), and no subagent of [`Core::parent`] (none, or one that has reported): a
    /// subagent that counts stands for its commands with its own row. A new one goes last, a
    /// changed `description` replaces its row's, one that no longer qualifies goes. Whether
    /// anything changed.
    fn update_live_commands(&mut self) -> bool {
        let listed: Vec<(String, String, Instant)> = self
            .background
            .iter()
            .filter(|t| {
                t["task_type"] == "local_bash" && t["ambient"] != true && self.parent(t).is_none()
            })
            .filter_map(|t| {
                let id = t["task_id"].as_str()?;
                let (monitor, started) = self.bash_tasks.get(id)?;
                (!monitor).then(|| {
                    (
                        id.to_owned(),
                        t["description"].as_str().unwrap_or("").to_owned(),
                        *started,
                    )
                })
            })
            .collect();
        let before = self.live_commands.len();
        self.live_commands
            .retain(|c| listed.iter().any(|(id, _, _)| *id == c.task_id));
        let mut changed = self.live_commands.len() != before;
        for (id, description, started) in listed {
            // Without a description it is the whole command, which may run to any length; the
            // row shows one line of it.
            let title = preview(&description, PREVIEW_LIMIT);
            match self.live_commands.iter_mut().find(|c| c.task_id == id) {
                Some(c) if c.title == title => {}
                Some(c) => {
                    c.title = title;
                    changed = true;
                }
                None => {
                    // Its run time counts from its `task_started`, also for a command that
                    // becomes a row only when the subagent that left it reports.
                    let call = self.task_calls.get(&id).map(String::as_str);
                    let mut c = LiveAgent::new(&id, "", call);
                    c.title = title;
                    c.since = started;
                    self.live_commands.push(c);
                    changed = true;
                }
            }
        }
        changed
    }

    /// Subagent `id`'s `task_started` with `title` (its `description`) and `call` (its
    /// `tool_use_id`): a repeated one keeps the first start; one the CLI's list counted before
    /// gets its title; either takes a call that differs.
    fn live_started(&mut self, id: &str, title: &str, call: Option<&str>) {
        match self.live.iter_mut().find(|a| a.task_id == id) {
            Some(a) => {
                if a.title.is_empty() && !title.is_empty() {
                    a.title = text(title);
                    self.live_changed = true;
                }
                if call.is_some() && a.call_id.as_deref() != call {
                    a.call_id = call.map(str::to_owned);
                    self.live_changed = true;
                }
            }
            None => {
                self.live.push(LiveAgent::new(id, title, call));
                self.live_changed = true;
            }
        }
    }

    /// `system/task_progress` (2.1.312: `description` is what the subagent does now, `usage`
    /// its `total_tokens` and `tool_uses` so far) of a subagent of [`Core::live`]: the list
    /// again.
    fn on_task_progress(&mut self, v: &Value) -> Vec<Action> {
        let Some(a) = self
            .live
            .iter_mut()
            .find(|a| v["task_id"] == a.task_id.as_str())
        else {
            return vec![];
        };
        a.activity = v["description"].as_str().map(text);
        a.total_tokens = v["usage"]["total_tokens"].as_u64();
        a.tool_uses = v["usage"]["tool_uses"].as_u64();
        vec![self.live_list()]
    }

    /// [`Core::live`], then [`Core::live_commands`], as sent: the run time of each counted now.
    fn live_list(&self) -> Action {
        Action::Emit(AdapterEvent::LiveSubagents(
            self.live
                .iter()
                .chain(&self.live_commands)
                .map(|a| LiveSubagent {
                    task_id: a.task_id.clone(),
                    description: a.title.clone(),
                    activity: a.activity.clone(),
                    total_tokens: a.total_tokens,
                    tool_uses: a.tool_uses,
                    duration_ms: u64::try_from(a.since.elapsed().as_millis()).unwrap_or(u64::MAX),
                    call_id: a.call_id.clone(),
                })
                .collect(),
        ))
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
        self.self_turn = false;
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
        // A foreground subagent cannot outlive the main turn either: it stops counting here
        // also when the CLI sent no end of it (an interrupt, a denied call,
        // `error_max_turns`). A background one goes on.
        let before = self.subagents.len();
        self.subagents.retain(|id| quiet.get(id) != Some(&false));
        if self.subagents.len() != before {
            tracing::info!(session_id = %self.session_id, "foreground subagents ended with the turn");
        }
        // Also a drop a `background_tasks_changed` left to this line: before `TurnEnded`.
        out.extend(self.report_subagents(None));
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
        let compacted = std::mem::take(&mut self.compacted);
        let model = self.model.clone().or_else(|| {
            v["modelUsage"]
                .as_object()
                .and_then(|m| m.keys().next().cloned())
        });
        // A result with no model turn and no tokens (a local slash command, the empty second
        // `result` after a turn the CLI started by itself, one the CLI closes on a background
        // subagent's notification) is not a turn's `usage`: the feed would show an empty `0 → 0
        // tokens` line. `/compact` has `num_turns: 0` and no tokens but costs, so it keeps its
        // `usage`.
        if v["num_turns"].as_u64() != Some(0)
            || input_tokens + output_tokens > 0
            || (spent && compacted)
        {
            let cost_usd = total.map(|t| self.turn_cost(t));
            out.push(self.emit_new(EventKind::Usage {
                input_tokens,
                output_tokens,
                cache_read: num("cache_read_input_tokens"),
                cache_write: num("cache_creation_input_tokens"),
                cost_usd,
                session_cost_usd: total,
                model,
            }));
        } else if spent {
            // The total grew by what background subagents spent meanwhile: a `usage` with the
            // total only, which the client counts for the session and draws no line for. The
            // baseline stays, so the next turn's `cost_usd` carries it.
            out.push(self.emit_new(EventKind::Usage {
                input_tokens: 0,
                output_tokens: 0,
                cache_read: None,
                cache_write: None,
                cost_usd: None,
                session_cost_usd: total,
                model,
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
        // Inputs not echoed (SPEC §17.2), by their last `command_lifecycle`:
        // - `started` without an echo: taken. A local slash command (`/name`, ...) runs
        //   without a model turn and may come without one; without this the supervisor
        //   resends it;
        // - only `queued`: carried, the CLI runs it next by itself (an input written during
        //   the final answer, or before an interrupt; 2.1.312);
        // - `cancelled`: not taken, `dropped`;
        // - no line (an older CLI): carried if written during the turn, else taken.
        let (mut carried, mut dropped) = (Vec::new(), Vec::new());
        for mut w in std::mem::take(&mut self.awaiting_echo) {
            let carry = match w.state.as_deref() {
                Some("cancelled") => {
                    tracing::info!(session_id = %self.session_id, event_id = %w.event_id, "input cancelled by the CLI, not taken");
                    dropped.push(w.event_id);
                    continue;
                }
                Some("started" | "completed") => false,
                Some(_) => true,
                None => w.mid_turn,
            };
            if carry {
                tracing::info!(session_id = %self.session_id, event_id = %w.event_id, "input carried into the next turn");
                w.carried = true;
                carried.push(w.event_id);
                self.awaiting_echo.push_back(w);
            } else {
                tracing::info!(session_id = %self.session_id, event_id = %w.event_id, "input taken without an echo (local command)");
                out.push(Action::Emit(AdapterEvent::InputAccepted {
                    event_id: w.event_id,
                }));
            }
        }
        tracing::info!(
            session_id = %self.session_id,
            subtype,
            num_turns = v["num_turns"].as_u64(),
            duration_ms = v["duration_ms"].as_u64(),
            carried = carried.len(),
            "turn ended"
        );
        // The CLI goes on with a carried input at once: that is a turn running.
        self.in_turn = !carried.is_empty();
        if self.in_turn {
            self.turn += 1;
        }
        out.push(Action::Emit(AdapterEvent::TurnEnded { carried, dropped }));
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
    /// subagent's, by `agent_id`) are cancelled; a task in [`Core::quiet_tasks`] gets no line,
    /// nor one in [`Core::announced`];
    /// a task this process did not see start (it began before a `--resume`) counts as a
    /// background one. A notification without the subagent's report (`handback`) while the
    /// latest `background_tasks_changed` still holds the subagent or a task of it is no end
    /// (2.1.312 sends it when the subagent waits on its own command moved to the background,
    /// "This agent has not reported yet…"): it waits in [`Core::held_ends`], with the line and
    /// the cancels, until the subagent stops counting. One with the report is the end, also
    /// while a task the subagent left running is listed ([`Core::reported`]).
    fn on_task_notification(&mut self, v: &Value) -> Vec<Action> {
        let id = v["task_id"].as_str();
        let report = !v["handback"].is_null();
        if let Some(id) = id.filter(|id| !report && self.listed(id)) {
            tracing::info!(session_id = %self.session_id, task_id = id, "subagent end held: it still counts");
            self.held_ends.insert(id.to_owned(), v.clone());
            return vec![];
        }
        if let Some(id) = id {
            self.held_ends.remove(id);
            if report {
                self.reported.insert(id.to_owned());
            }
        }
        self.on_task_ended(v)
    }

    /// The end of the task of `task_notification` `v` ([`Core::on_task_notification`]). The
    /// line carries the task's `tool_use_id` (the notification's, else that of its
    /// `task_started`, [`Core::task_calls`]) as `call_id`, and the notification's `status` and
    /// `task_id`.
    fn on_task_ended(&mut self, v: &Value) -> Vec<Action> {
        let id = v["task_id"].as_str();
        let started = id.and_then(|id| self.task_calls.remove(id));
        // A subagent that ended asks nothing any more: its requests still open are cancelled
        // here, whichever comes first, this or the main turn's `result`.
        let mut out = match id {
            Some(id) => self.cancel_pending(|p| p.agent_id() == Some(id)),
            None => vec![],
        };
        if id.is_some_and(|id| self.quiet_tasks.remove(id).is_some()) {
            return out;
        }
        if id.is_some_and(|id| !self.announced.insert(id.to_owned())) {
            return out;
        }
        let t = v["summary"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map_or_else(|| "Background task".into(), text);
        tracing::info!(session_id = %self.session_id, task_id = ?v["task_id"].as_str(), status = ?v["status"].as_str(), "background task notification");
        out.push(Action::Emit(AdapterEvent::Event(new_event(
            self.session_id,
            Author::System,
            EventKind::System {
                text: t,
                code: TASK_NOTIFICATION.into(),
                call_id: v["tool_use_id"].as_str().map(str::to_owned).or(started),
                status: v["status"].as_str().map(str::to_owned),
                task_id: id.map(str::to_owned),
                fork: None,
            },
        ))));
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

    /// The held `context_compacted` ([`Core::compaction`]): its text is the summary's
    /// `message.content` (a string, or the text blocks of an array), "Context compacted"
    /// without one.
    fn release_compaction(&mut self, summary: Option<&Value>) -> Option<Action> {
        let id = self.compaction.take()?;
        let t = match summary {
            Some(Value::String(s)) => text(s),
            Some(c) => text(
                &c.as_array()
                    .into_iter()
                    .flatten()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            None => CONTEXT_COMPACTED_TEXT.into(),
        };
        Some(self.emit_system_as(Some(id), CONTEXT_COMPACTED, t))
    }

    /// The input the running turn answers ([`Core::last_input`]), once its echo has come. Until
    /// then the CLI has only queued it and what comes is no reply to it: without
    /// `command_lifecycle` the input is the one sent last, and one written during a turn the
    /// CLI started by itself is echoed after that turn's `result` (checked live on 2.1.312).
    fn reply_to(&self) -> Option<Uuid> {
        self.last_input
            .filter(|id| !self.awaiting_echo.iter().any(|w| w.event_id == *id))
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
                call_id: None,
                status: None,
                task_id: None,
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
/// The 5-hour and 7-day windows of a `rate_limit_event`'s `unifiedWindows`; `None` when it
/// names neither.
fn limits(w: &Value) -> Option<hqc_proto::frame::Limits> {
    let window = |name: &str| {
        let v = &w[name];
        Some(hqc_proto::frame::LimitWindow {
            utilization: v["utilization"].as_f64()?,
            resets_at: v["resetsAt"].as_i64()?,
        })
    };
    let l = hqc_proto::frame::Limits {
        five_hour: window("five_hour"),
        seven_day: window("seven_day"),
    };
    (l.five_hour.is_some() || l.seven_day.is_some()).then_some(l)
}

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

pub(crate) fn parse_questions(input: &Value) -> Option<(Vec<ClarifyQuestion>, Vec<String>)> {
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

/// `args_preview` of a `tool_use` with `input` (SPEC §4): the input as JSON, cut to
/// [`PREVIEW_LIMIT`]. An `Agent` (`Task` in older Claude Code) input has its `prompt` last: the
/// keys come sorted, and a long prompt cut at 4 KB took `subagent_type` and `run_in_background`,
/// which the subagent's card shows, with it. The adapter and the mirror both use it.
pub fn args_preview(tool: &str, input: &Value) -> String {
    let json = match input.as_object() {
        Some(o) if (tool == "Agent" || tool == "Task") && o.contains_key("prompt") => {
            let rest: serde_json::Map<String, Value> = o
                .iter()
                .filter(|(k, _)| k.as_str() != "prompt")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            // `{…}` of the other fields, then `"prompt":…` before its `}`.
            let mut s = compact(&Value::Object(rest));
            s.pop();
            if s.len() > 1 {
                s.push(',');
            }
            s.push_str("\"prompt\":");
            s.push_str(&compact(&o["prompt"]));
            s.push('}');
            s
        }
        _ => compact(input),
    };
    preview(&json, PREVIEW_LIMIT)
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
