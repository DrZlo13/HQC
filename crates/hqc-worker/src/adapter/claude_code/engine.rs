//! The I/O-free part of the Claude Code adapter: stream-json lines in, [`Action`]s out
//! (events for the supervisor, lines for the CLI's stdin). One [`Core`] per process, so
//! `session` rules live exactly as long as the process (SPEC §8.1).
//!
//! Mapping (SPEC §4, §8.1, docs/notes/claude-control-protocol.md):
//! - `stream_event` `text_delta` → `agent.delta`, `thinking_delta` with text → `agent.delta`
//!   with `thinking`; deltas (text, thinking, tool input at four characters a token, redacted
//!   thinking at its `estimated_tokens`), then `message_delta`'s `usage.output_tokens` for the
//!   token rate ([`Core::take_output_tokens`], [`super::rate`]); after text deltas no
//!   assistant text took up, a `message_start` or an `assistant` of another `message.id` than
//!   the one streamed → `agent.delta` with `reset: true` ([`Core::live_text`]);
//! - `assistant` text → `agent.message`, held until the next element: another assistant
//!   block or a tool result makes it `final: false`, `result` makes it `final: true`; an
//!   input written during a turn the CLI started by itself leaves it held; a text that is the
//!   CLI's [`NO_CONTENT`] gives none;
//! - `assistant` `tool_use` → `tool.call` ([`args_preview`]: an `Agent` call's `prompt`
//!   last); non-empty `thinking` → `agent.thinking`;
//! - `assistant` text with `model` `<synthetic>` (the CLI's own reply: an API error, "Credit
//!   balance is too low") → `agent.message` with `runtime: true`, held and flushed as any;
//!   with `stop_reason` `refusal` (its reply to a model refusal) also `refusal: true`; with
//!   `local_command_source` (a slash command's output, which alone carries it) → `system
//!   command_output` instead, after the held text is flushed (SPEC 0.218);
//! - an `attachment` `goal_status` of the transcript (the CLI writes it nowhere else; the
//!   meter reads it, [`Core::on_goal`]) → `system goal`, behind a text held when the record was
//!   written after it (SPEC 0.218);
//! - `assistant` text with `aborted: true` (an interrupt cut it off) → `agent.message` with
//!   `interrupted: true`, held and flushed as any;
//! - `system` `task_notification` (a background task ended) → `system task_notification`
//!   with its `status`, its `task_id`, its `usage` and the task's `tool_use_id` as
//!   `call_id`, then its `output_file` as
//!   [`AdapterEvent::TaskOutput`], except for a task with `skip_transcript: true`, a
//!   task that a subagent started or whose `task_started` said it runs in the foreground,
//!   until a `task_updated` moves it to the background; the CLI's placeholder "This agent has
//!   not reported yet…" without `handback` while the latest `background_tasks_changed` still
//!   holds the subagent, or a task of it, is no end: the latest one is handled when the
//!   subagent stops counting (below); any other notification of a subagent is its end, also
//!   while commands it started still run (2.1.329 sends its report in `summary`, no
//!   `handback`); a later notification of a task gives a line only when its text differs
//!   from the one shown, the placeholder never; a subagent that reported while commands it
//!   started were listed, and that the CLI neither starts again nor rids of them for 30 min,
//!   → [`AdapterEvent::SubagentLeftovers`] once ([`Core::leftovers`], on the token rate's
//!   tick);
//!   `system` `compact_boundary` → `system context_compacted` with the `trigger`, `pre_tokens`
//!   and `post_tokens` of its `compact_metadata`, held until the summary the CLI
//!   writes after it, the main agent's `user` line with `isSynthetic: true`, which gives its
//!   text; another main agent's `user`, `assistant`, `stream_event` or `result` line first, or
//!   the process's end, gives it "Context compacted";
//! - `system` `status` with `compact_result: failed` → `system compaction_failed` with its
//!   `compact_error`;
//! - [`AdapterEvent::Activity`] when it changes, a new estimate of the thinking at most once
//!   per 2 s: `system` `api_retry` → retrying, `system` `status` `compacting` → compacting,
//!   `system` `thinking_tokens` → thinking with its `estimated_tokens`; any other `system`
//!   `status` that does not report the permission mode (`requesting`, `null` at a
//!   compaction's end), a main agent's `stream_event` `message_start`, `content_block_start`
//!   or `content_block_stop`, its `assistant` line and `result` → none ([`activity_of`]);
//! - `tool_progress` with `heartbeat: true` (a long call of the main agent, every 30 s) →
//!   [`AdapterEvent::Heartbeat`]; with `subagent_retry` → `retry` of the subagent's row of
//!   [`AdapterEvent::LiveSubagents`] (`agent_id` is its `task_id`), until a `tool_progress` of
//!   the same `parent_tool_use_id` without it, which says the retry resolved;
//! - `system` `task_started` with `task_type: local_agent` (a subagent, in the background or
//!   not, a subagent's own too) until `task_notification` or a `task_updated` status other
//!   than `running` of the same `task_id`, a foreground one at the latest until the `result`,
//!   and any subagent the latest `background_tasks_changed` list holds or a task of it names
//!   as its `parent_task_id`, unless that subagent ended since its last `task_started`
//!   ([`Core::reported`]) → [`AdapterEvent::Subagents`] when the count changes, a drop that
//!   a `background_tasks_changed` brings at the next line of another kind: the CLI reports a
//!   subagent ended while it waits for a command moved to the background, and lists and
//!   starts its `task_id` again when the command ends (2.1.312);
//! - the same subagents one by one → [`AdapterEvent::LiveSubagents`] when they or a title
//!   change, and on every `system` `task_progress` of one of them: the title and the
//!   `tool_use_id` (`call_id`) from its `task_started`, what it does now (`description`),
//!   `total_tokens` and `tool_uses` from its latest `task_progress` (2.1.312), `stoppable`
//!   ([`Core::stop_task`]); after them, in the same list, the main agent's background shell
//!   commands of the latest
//!   `background_tasks_changed` list (`task_type: local_bash`, not `ambient`, whose
//!   `task_started` has come, of no subagent that counts; `kind` `monitor` when a `Monitor`
//!   call started it), each with its `description`, run time and `stoppable` only, when they
//!   or a `description` change; then the wakeup the agent armed with `ScheduleWakeup` and its
//!   `CronCreate` jobs (`kind` `wakeup` and `cron`, each with `due_in_ms`, not `stoppable`),
//!   when a result of `ScheduleWakeup`, `CronCreate` or `CronDelete` or a `command_lifecycle`
//!   of a wakeup or a cron fire changes them;
//! - `system` `background_tasks_changed` (`tasks`: the CLI's list of the session's running
//!   background tasks, subagents and commands, its subagents' too) →
//!   [`AdapterEvent::BackgroundTasks`] with the number of those neither `ambient` nor
//!   `off_host` when it changes; a task with `ambient: true` there or in its `task_started`
//!   is none of the above (no activity, the SDK says), nor a subagent as the `parent_task_id`
//!   of another task; one with `off_host: true` there is not counted either (2.1.325, a cloud
//!   session: its work runs outside the process);
//! - `system` `task_started` with `is_backgrounded: false`, `task_type` `local_bash` or
//!   `local_agent`, a `tool_use_id`, no `skip_transcript: true`, no `owned_by_subagent:
//!   true` and no `parent_task_id` (a Bash call or a subagent of the agent itself in the
//!   foreground, not of a subagent) until a `task_started` of the same `task_id` without all
//!   that, a `task_updated` with `patch.is_backgrounded: true` or `patch.status` `completed`,
//!   `failed` or `killed`, or its `task_notification` → [`AdapterEvent::ForegroundCalls`]
//!   with their `tool_use_id`s when they change (the rule of the VS Code tab's "Run in background", extension 2.1.319);
//! - `rate_limit_event` → [`AdapterEvent::Limits`] with the 5-hour and 7-day windows of its
//!   `rate_limit_info.unifiedWindows` (2.1.310: one before each turn's answer), its `status`,
//!   `rateLimitType` and `resetsAt`;
//! - `system` `informational` with `model_restriction` (2.1.325: the CLI refused the model asked
//!   for and runs on another) → `system model_not_switched` "Model not switched: <asked> is not
//!   available. The agent runs on <model>" with that model, and [`AdapterEvent::ModelKept`],
//!   but nothing for one `at: switch` of a model other than the latest `set_model`'s;
//!   another of a level other than `info` and `system` `notification` →
//!   `system runtime_notice` with the text; `model_fallback` and `model_consent_fallback` →
//!   `system model_fallback` "<model> → <fallback>: <content>", the fallback the model of this
//!   turn's `usage`; `model_refusal_no_fallback` → `system model_refusal`; an `assistant` with
//!   `supersedes`, and `model_refusal_fallback` and a `user` (2.1.325: the message that asks the
//!   model again after a response whose tool call could not be parsed) by their
//!   `retracted_message_uuids` → `retraction` of the events of those records of this turn not
//!   retracted yet ([`Core::retract`]), `model_refusal_fallback` then `system
//!   model_refusal_fallback`, its fallback the model of this turn's `usage` unless its `scope` is
//!   `local`;
//! - `user` `tool_result` → `tool.result`, its text the report of a subagent that completed
//!   when the record's `tool_use_result` has one ([`agent_report`]); a text longer than the
//!   preview and the result's images go with it to be uploaded ([`AdapterEvent::ToolResult`]);
//!   `user` with `isReplay: true` → input accepted;
//!   every user message written to stdin gets a fresh v4 `uuid`, which its echo and its
//!   `command_lifecycle` lines name (2.1.312);
//! - `result` → `usage` (one with no tokens and no `compact_boundary` in it gives none, or,
//!   when the total grew, one with no tokens, no `cost_usd` and the total),
//!   then `system turn_interrupted` for a `terminal_reason` `aborted_*`, or `system
//!   turn_ended_early` for an error or a `terminal_reason` other than `completed`,
//!   `background_requested` and `tool_deferred` ([`turn_end_line`]), then `status idle`, end
//!   of turn;
//!   `usage.cost_usd` is the growth of the CLI's cumulative `total_cost_usd` since the previous
//!   `usage` with a `cost_usd` (or since the transcript's last `cost-state` after `--resume`,
//!   or since 0 after `conversation_reset`), `session_cost_usd` the total itself; the tokens
//!   of a compaction's `result` (zeros in its `usage`) are the growth of its cumulative
//!   `modelUsage`, summed over the models, since the same baseline; `context_tokens` is the
//!   sum of the usage of the main agent's last API response (the size `post_tokens` of a
//!   `compact_boundary` gives, until the next response; none after a boundary without it, the
//!   runtime not having counted the size),
//!   `context_limit` the `contextWindow` of the main model's entry in `modelUsage` less the
//!   reserve for output and the CLI's margin ([`context_limit`], [`usage_entry`]),
//!   `cache_ttl_s` the TTL of the cache writes the response or `modelUsage` shows, less the
//!   seconds since that response started ([`ContextUse::cache_ttl_s`]); the output tokens of
//!   the context are the response's `message_delta` count; `/clear` forgets the context;
//!   a request of the main agent or of a foreground subagent still open then gets
//!   `approval.cancel` / `clarify.cancel` `stopped`, any other subagent's stays open until its
//!   `task_notification`, which cancels it; an input written during the turn and not echoed
//!   is taken, carried into the next turn or not taken by its last `command_lifecycle` (SPEC
//!   §17.2); a carried input `cancelled` after the `result` ends the turn it was carried
//!   into; a carried local command is taken by its own `result`;
//! - `system` `init` → the model, and [`AdapterEvent::Commands`] when the commands differ
//!   from what this process reported (the CLI sends `init` before every turn): those of the
//!   CLI's answer to our `initialize` or of its latest `system` `commands_changed`, with
//!   their descriptions and argument hints, else the names of `slash_commands`, without
//!   `terminal_slash_commands` ([`Core::report_commands`]); the answer and
//!   `commands_changed` report them too; one `init` while no turn of ours runs (a turn the CLI
//!   starts by itself) → `status running` first; its MCP servers neither `connected` nor
//!   `disabled` → `system mcp_not_connected`, its `plugin_errors` → `system plugin_errors`, each
//!   when it differs from the one given last;
//! - `conversation_reset` (`/clear`) → `system conversation_cleared`, and the cost baseline,
//!   token sums with it, starts from 0 again; the next `system` `init` with another
//!   `session_id` → [`AdapterEvent::RuntimeSessionId`] with it, the transcript the CLI writes
//!   from then on;
//! - `system` `session_title_changed` (a person's title, `/rename`; the CLI's own is only in
//!   the transcript) → [`AdapterEvent::Title`] at once;
//! - the result of the main agent's `ScheduleWakeup` call → [`AdapterEvent::Wakeup`] with its
//!   `scheduledFor`, none for 0; a `command_lifecycle` `started` with `wakeup_source:
//!   loop_wakeup` and that `scheduled_for` → [`AdapterEvent::Wakeup`] with none;
//! - the result of the main agent's `TodoWrite`, `TaskCreate`, `TaskUpdate` or `TaskList` call
//!   (none of a subagent's, none of a failed call) → `tasks` with the whole list when it
//!   changed ([`apply_task_result`]); `conversation_reset` empties it;
//! - `control_request` `can_use_tool` → `approval.request` (with `policy_hint`, the hqc
//!   rules suggested for the call and the subagent whose `agent_id` it names) /
//!   `clarify.request` (with that subagent too), an immediate allow by a `session`
//!   rule or by the folder's hqc rules (T48), or an immediate deny plus `system
//!   policy_denied` when a deny rule of `[permissions]` matches (checked first); a request
//!   only a person may answer gets no allow by a rule and only `once` / `deny`;
//! - `control_request` `elicitation` (an MCP server asks the person) → `clarify.request` with
//!   `server`, or `cancel` plus `system elicitation_unsupported` for a form it cannot ask or
//!   one too large;
//! - `control_cancel_request` (the CLI withdrew its request) → `approval.cancel` /
//!   `clarify.cancel` `withdrawn`;
//! - `system` `permission_denied` (the CLI refused a call by its rules, no request) →
//!   `system policy_denied`;
//! - `permissionMode` of `system` `init` and `system` `status`, and the CLI's answer to our
//!   `set_permission_mode` → [`AdapterEvent::PermissionMode`] when it changes (T38), and
//!   on every success of our `set_permission_mode` (T39);
//! - an `error` answer to our latest `set_model` → `system model_not_switched` with the reason
//!   and the model the CLI kept, and [`AdapterEvent::ModelKept`]; the other answers to our
//!   `set_model`, and those to `background_tasks` and `stop_task`, are only logged;
//! - the other `control_response` lines, the CLI's echo of ours, are skipped.
//!
//! U+0000 in any text taken from the CLI (binary tool output) becomes U+FFFD ([`text`],
//! [`preview`]): the hub's Postgres refuses it (SPEC §5.2).
//!
//! Messages of subagents (`parent_tool_use_id` set) are not mapped: their records live in
//! separate sidechain files, not in the session transcript.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};
use hqc_claude_jsonl::{model_usage_tokens, CostState, GoalRecord, GoalState, TokenSums};
use hqc_proto::event::{
    AgentTask, AnswerInput, ApiRetry, ApprovalCanonical, ApprovalOption, Author, CancelReason,
    ClarifyAction, ClarifyAnswer, ClarifyQuestion, ContextCompacted, Event, EventKind, GoalStatus,
    GrantPlace, LiveKind, LiveSubagent, OptionGrant, RequestSubagent, SessionState, TaskStatus,
    TaskUsage, GOAL, MODEL_NOT_SWITCHED, OPTION_SERVER_ALWAYS, OPTION_SERVER_SESSION,
};
use hqc_proto::frame::{Activity, LeftoverCommand, PermissionMode, SessionInput, SlashCommand};
use serde_json::{json, Value};
use uuid::Uuid;

use super::rules::{rule_text, shown, SessionRules};
use crate::adapter::{new_event, AdapterEvent, ResultFiles, ResultImage, FULL_RESULT_LIMIT};
use crate::permissions::{FolderRules, Policy};

/// `*_preview` limit (SPEC §4).
pub const PREVIEW_LIMIT: usize = 4096;

/// How far past now a cron job's due time may lie for it to be the job a fire belongs to
/// (`oneShotMaxMs` of the CLI's defaults: it fires a one-shot job that much early at most).
const CRON_FIRE_SLACK_MS: i64 = 90_000;
/// The age at which a recurring cron job expires after its fire (`recurringMaxAgeMs` of the
/// CLI's defaults, 7 days).
const CRON_MAX_AGE_MS: i64 = 604_800_000;
/// The CLI's `recurringCapMs`: the most a recurring job's fire is put off by its own offset.
const CRON_OFFSET_CAP_MS: f64 = 1_800_000.0;
/// The CLI's `cacheLeadMs`, and the 5 minutes (`g7n`) its prompt cache lives, which a job that
/// runs every N minutes is kept inside of.
const CRON_CACHE_LEAD_MS: i64 = 15_000;
const CRON_CACHE_TTL_MS: i64 = 300_000;
pub const ERROR_TEXT_LIMIT: usize = 300;
/// Cap of one option's `preview` of `AskUserQuestion` in `clarify.request.option_previews`
/// (SPEC §4, §8.1), in UTF-8 bytes, so that one question does not swell the event and the
/// feed: a longer one is cut on a char boundary and [`OPTION_PREVIEW_TRUNCATED`] follows.
pub const OPTION_PREVIEW_LIMIT: usize = 8192;
/// The mark after an option's preview cut to [`OPTION_PREVIEW_LIMIT`].
pub const OPTION_PREVIEW_TRUNCATED: &str = "\n… (truncated)";
/// The most rules an option's grant lists, and the longest one, in bytes (SPEC §4).
const GRANT_RULES: usize = 10;
const GRANT_RULE_LIMIT: usize = 256;

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
/// How the `summary` of a subagent's `task_notification` that is no end starts: the CLI's
/// placeholder while the subagent waits on its own background work before it reports
/// (2.1.312; `PWr` of 2.1.325).
const NOT_REPORTED_YET: &str = "This agent has not reported yet";
/// How long after a subagent reported, with background commands it started still listed, the
/// main agent is told of those still listed then ([`Core::leftovers`]).
const LEFTOVER_AFTER: Duration = Duration::from_secs(30 * 60);
/// The CLI keeps this many tokens of the model's output out of the window the auto-compaction
/// counts against, at most (`get_context_usage` of 2.1.329; `yPt` of the VS Code tab): the
/// reserve is `min(maxOutputTokens, this)`.
const AUTO_COMPACT_OUTPUT_RESERVE_MAX: u64 = 20_000;
/// A further margin below the window and the output reserve at which the CLI compacts.
const AUTO_COMPACT_BUFFER: u64 = 13_000;
/// `cache_ttl_s` of a prompt cache written with the 1-hour TTL, and of the default 5-minute one.
const CACHE_TTL_1H_S: u64 = 3600;
const CACHE_TTL_5M_S: u64 = 300;
/// `system.code` of stream-json `system` `compact_boundary` (SPEC §4, §8.1).
pub const CONTEXT_COMPACTED: &str = "context_compacted";
pub const CONTEXT_COMPACTED_TEXT: &str = "Context compacted";
/// `system.code` of a turn an interrupt ended (Stop, "No" to a permission request):
/// `result` with a `terminal_reason` `aborted_*` (SPEC §4, §8.1).
pub const TURN_INTERRUPTED: &str = "turn_interrupted";
pub const TURN_INTERRUPTED_TEXT: &str = "Interrupted";
/// The [`TURN_ENDED_EARLY`] text of a turn whose answer the API stopped over a possible prompt
/// injection (`stop_reason` `prompt_injection_risk`), after the Claude app's warning.
pub const PROMPT_INJECTION_TEXT: &str = "Turn ended early: prompt_injection_risk · the content the agent read may contain a prompt injection; review it before continuing";
/// `system.code` of a turn the CLI ended early for another reason: an error `result`, or a
/// `terminal_reason` other than `completed`, `background_requested` and `tool_deferred` (SPEC
/// §4, §8.1).
pub const TURN_ENDED_EARLY: &str = "turn_ended_early";
/// `system.code` of `/clear`: stream-json `conversation_reset`, after which the CLI goes on in
/// a new conversation, another transcript (SPEC §4, §8.1).
pub const CONVERSATION_CLEARED: &str = "conversation_cleared";
pub const CONVERSATION_CLEARED_TEXT: &str = "Conversation cleared";
/// `system.code` of a slash command's output in a transcript the mirror reads:
/// `<local-command-stdout>` / `<local-command-stderr>` (SPEC §4, §9.1).
pub const COMMAND_OUTPUT: &str = "command_output";
/// At most this many bytes of a slash command's description and argument hint are reported.
pub const COMMAND_TEXT_LIMIT: usize = 200;
/// At most this many bytes of a task's subject and active form, and at most this many tasks, are
/// kept: the `tasks` event stays far under the hub's WebSocket frame limit (SPEC §4).
pub const TASK_TEXT_LIMIT: usize = 512;
pub const TASKS_LIMIT: usize = 200;
/// At most this many slash commands are reported, and none whose name is longer than
/// [`COMMAND_NAME_LIMIT`] characters: what the hub keeps (SPEC §5.4).
pub const COMMANDS_LIMIT: usize = 500;
pub const COMMAND_NAME_LIMIT: usize = 100;
/// `system.code` of stream-json `system` `status` with `compact_result: failed` (SPEC §4, §8.1).
pub const COMPACTION_FAILED: &str = "compaction_failed";
/// `system.code` of a `system/informational` of a level other than `info` and of a
/// `system/notification` (SPEC §4, §8.1): the CLI's or a hook's message, as text.
pub const RUNTIME_NOTICE: &str = "runtime_notice";
/// `system.code` of `system/model_fallback` and `system/model_consent_fallback`: the CLI went
/// on with another model.
pub const MODEL_FALLBACK: &str = "model_fallback";
/// `system.code` of `system/model_refusal_fallback`: the model refused, another one answered.
pub const MODEL_REFUSAL_FALLBACK: &str = "model_refusal_fallback";
/// `system.code` of `system/model_refusal_no_fallback`: the model refused, none answered.
pub const MODEL_REFUSAL: &str = "model_refusal";
/// `system.code` of the MCP servers of `system/init` that are not connected.
pub const MCP_NOT_CONNECTED: &str = "mcp_not_connected";
/// `system.code` of the `plugin_errors` of `system/init`.
pub const PLUGIN_ERRORS: &str = "plugin_errors";
/// How often a new estimate of the thinking goes as [`AdapterEvent::Activity`], as the token
/// rate (SPEC §8.1).
const THINKING_EVERY: std::time::Duration = std::time::Duration::from_secs(2);
/// `system.code` of an MCP server's request for input the card cannot ask (SPEC §4, §8.1).
pub const ELICITATION_UNSUPPORTED: &str = "elicitation_unsupported";
/// `approval.request.reason` and `display_name` limits, in bytes (SPEC §4).
pub const REASON_LIMIT: usize = 1024;
pub const DISPLAY_NAME_LIMIT: usize = 256;
/// An MCP server's request (SPEC §8.1): its `server` and each question's text and header are
/// cut to these many bytes; one whose `clarify.request` is over [`ELICITATION_LIMIT`] bytes of
/// JSON is cancelled, as the hub drops a worker's connection on a frame over 1 MiB.
pub const SERVER_NAME_LIMIT: usize = 256;
pub const QUESTION_TEXT_LIMIT: usize = 1024;
pub const ELICITATION_LIMIT: usize = 256 * 1024;
/// `message.model` of a reply the CLI wrote itself instead of the model ("Credit balance is
/// too low", API errors; 2.1.312, stream-json and transcript alike): `agent.message` with
/// `runtime: true`.
pub const SYNTHETIC_MODEL: &str = "<synthetic>";
/// The CLI's text for a reply without text (2.1.312): a non-streaming reply whose blocks it
/// all dropped, a message of its own with empty text. No `agent.message`, as the VS Code tab
/// draws none (extension 2.1.319).
pub const NO_CONTENT: &str = "(no content)";
/// `request_id` prefix of our `initialize` control request.
const INIT_REQUEST: &str = "hqc-init-";
/// `message.stop_reason` of the CLI's own reply to a model refusal (2.1.312, stream-json and
/// transcript alike): `agent.message` with `refusal: true`.
pub const REFUSAL: &str = "refusal";
/// `request_id` prefix of our `set_permission_mode` control requests.
const MODE_REQUEST: &str = "hqc-mode-";
/// `request_id` prefix of our `set_model` control requests.
const MODEL_REQUEST: &str = "hqc-model-";
/// `request_id` prefix of our `background_tasks` control requests.
const BACKGROUND_REQUEST: &str = "hqc-background-";
/// `request_id` prefix of our `cancel_async_message` control requests.
const CANCEL_REQUEST: &str = "hqc-cancel-";
/// `request_id` prefix of our `stop_task` control requests.
const STOP_TASK_REQUEST: &str = "hqc-stop-task-";
const INTERRUPT_REQUEST: &str = "hqc-interrupt-";
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
    /// An MCP server's request (`elicitation`): its properties by position, as the questions.
    Elicitation { fields: Fields },
}

/// The type of a property of an MCP server's form, by which its answer goes back.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Field {
    Text,
    Integer,
    Number,
    Boolean,
}

/// The properties of an MCP server's form by position: each one's name and type.
type Fields = Vec<(String, Field)>;

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
    /// The `request_id` of the `cancel_async_message` asking the CLI to drop it (SPEC 0.219),
    /// until the CLI answers.
    withdrawing: Option<String>,
}

/// A subagent of [`Core::live`], a command or a Monitor of [`Core::live_commands`], the wakeup
/// of [`Core::wakeup_row`], or a job of [`Core::cron_jobs`].
#[derive(Debug)]
struct LiveAgent {
    /// What the row is.
    kind: LiveKind,
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
    /// Its API retry of a `tool_progress` `subagent_retry`, with that line's
    /// `parent_tool_use_id`, until the line of the same call that says it resolved.
    retry: Option<(String, ApiRetry)>,
}

impl LiveAgent {
    fn new(kind: LiveKind, task_id: &str, title: &str, call_id: Option<&str>) -> Self {
        Self {
            kind,
            task_id: task_id.to_owned(),
            title: text(title),
            since: Instant::now(),
            activity: None,
            total_tokens: None,
            tool_uses: None,
            call_id: call_id.map(str::to_owned),
            retry: None,
        }
    }
}

/// A task of the agent's task list ([`Core::task_list`]).
#[derive(Debug, Clone)]
pub struct TaskEntry {
    /// The runtime's id of the task; `TodoWrite` gives none.
    id: Option<String>,
    task: AgentTask,
}

/// A job of the agent's `CronCreate`, shown as a row ([`Core::cron_jobs`]).
#[derive(Debug)]
pub struct CronJob {
    /// The row: its `task_id` is the job's id, its `title` the job's prompt, its `activity`
    /// the schedule in words, its `since` when the result of `CronCreate` came.
    row: LiveAgent,
    /// The 5-field cron expression, in local time.
    cron: String,
    recurring: bool,
    /// When the result of `CronCreate` came, in epoch ms: a recurring job lives
    /// [`CRON_MAX_AGE_MS`] from it.
    created: i64,
    /// When the job fires next, in epoch ms; none while that is not known.
    due: Option<i64>,
}

impl Pending {
    fn agent_id(&self) -> Option<&str> {
        match self {
            Pending::Approval { agent_id, .. } | Pending::Clarify { agent_id, .. } => {
                agent_id.as_deref()
            }
            Pending::Elicitation { .. } => None,
        }
    }
}

#[derive(Debug)]
pub struct Core {
    session_id: Uuid,
    restricted: bool,
    /// The last assistant text: event id, text, whether the CLI wrote it itself, whether an
    /// interrupt cut it off, and whether it is the CLI's reply to a model refusal.
    held: Option<(Uuid, String, bool, bool, bool)>,
    /// When [`Core::held`] was set, by this process's clock: a goal record of the transcript
    /// written at or after it came after that text ([`Core::on_goal`]).
    held_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Goal events ([`goal_event`]) whose record was written after the held text and read
    /// before the next element brought it out: they go out behind it ([`Core::flush`]), so a
    /// "Goal not yet met" line is not above the text the check judged (SPEC 0.218).
    goals_behind: Vec<Event>,
    /// Event id of the `context_compacted` of a `compact_boundary`, and its `compact_metadata`,
    /// held for the summary that follows it ([`Core::release_compaction`]).
    compaction: Option<(Uuid, Option<ContextCompacted>)>,
    /// What the main agent waits on, as last reported ([`AdapterEvent::Activity`]), and when.
    activity: Option<Activity>,
    activity_at: Option<Instant>,
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
    /// The main agent's background shell commands and Monitor watchers of
    /// [`Core::background`], in the order they became rows, sent after [`Core::live`]
    /// ([`Core::update_live_commands`]). Apart from it: they are no subagents and never change
    /// the subagent count.
    live_commands: Vec<LiveAgent>,
    /// Ids of the `Monitor` `tool_use` blocks of `assistant` lines, the agent's and its
    /// subagents', until the `task_started` that names one in `tool_use_id`.
    monitor_calls: HashSet<String>,
    /// `local_bash` tasks whose `task_started` came, `true` for a Monitor's (2.1.312 lists a
    /// Monitor's task as `local_bash` without `ambient`, nothing in the list tells it from a
    /// command), and when the first came, until the task's `task_notification`.
    bash_tasks: HashMap<String, (bool, Instant)>,
    /// Ids of the main agent's `ScheduleWakeup` `tool_use` blocks, until their result.
    wakeup_calls: HashSet<String>,
    /// The `reason` of each call of [`Core::wakeup_calls`].
    wakeup_reasons: HashMap<String, String>,
    /// When the wakeup the main agent armed last is due, in epoch ms (the `scheduledFor` of
    /// its `ScheduleWakeup` result), until it starts or a later call replaces or stops it.
    wakeup: Option<i64>,
    /// The row of [`Core::wakeup`], sent after [`Core::live_commands`]; none exactly when
    /// `wakeup` is none.
    wakeup_row: Option<LiveAgent>,
    /// The main agent's `CronCreate` calls until their result: call id → (`cron`, `prompt`).
    cron_creates: HashMap<String, (String, String)>,
    /// Ids of the main agent's `CronDelete` `tool_use` blocks, until their result.
    cron_deletes: HashSet<String>,
    /// The main agent's cron jobs, in the order they were created, sent after
    /// [`Core::wakeup_row`].
    cron_jobs: Vec<CronJob>,
    /// The main agent's `TodoWrite`, `TaskCreate`, `TaskUpdate` and `TaskList` calls until their
    /// result: call id → (tool, input).
    todo_calls: HashMap<String, (String, Value)>,
    /// The main agent's task list as the `tasks` event last sent says it
    /// ([`Core::tasks_changed`]).
    task_list: Vec<TaskEntry>,
    /// [`Core::live`] got a subagent or a title since it was last reported.
    live_changed: bool,
    /// A `background_tasks_changed` lowered [`Core::running_subagents`] below the count last
    /// reported: the next line of another kind reports it.
    recount: bool,
    /// `tasks` of the latest `system` `background_tasks_changed`, without the `ambient` and the
    /// `off_host` ones.
    background: Vec<Value>,
    /// The `task_id`s the CLI marked `ambient: true`, in a `task_started` or the list: none is
    /// a subagent, also as the `parent_task_id` of a task ([`Core::parent`]; 2.1.312 runs a
    /// skill with `context: fork` as such a `local_agent`, and its commands name it).
    ambient: HashSet<String>,
    in_turn: bool,
    /// A turn the CLI started by itself (`system/init` while no turn of ours runs, e.g. after
    /// a background task's `task_notification`), until its `result`: Stop interrupts it too.
    self_turn: bool,
    /// The turn is being ended on purpose, by Stop ([`Core::interrupt`]) or by "No" that
    /// interrupts, until the turn ends — its `result`, a carried input's `cancelled` that ends
    /// it with none ([`Core::on_lifecycle`]), or the process's exit: the
    /// `control_cancel_request` the CLI then writes for each request of the turn is the turn's
    /// end, not a withdrawal ([`Core::on_cancel_request`]).
    stopping: bool,
    turn: u64,
    /// The ids of the `interrupt` requests the CLI answered.
    interrupt_answers: HashSet<String>,
    /// The CLI's answers to our `cancel_async_message` requests by `request_id`: whether it
    /// dropped the message ([`Core::withdraw_answer`]).
    cancel_answers: HashMap<String, bool>,
    pending: HashMap<String, Pending>,
    rules: SessionRules,
    /// `[permissions]` of the worker config, for `policy_hint` and denying what a deny
    /// rule matches.
    policy: Arc<Policy>,
    /// hqc's allow rules of the session's workspace and its path (T48); `None` without one.
    folder: Option<(FolderRules, std::path::PathBuf)>,
    model: Option<String>,
    /// `request_id` of the latest `set_model` sent ([`Core::set_model`]), and its model.
    model_request: Option<String>,
    model_requested: Option<String>,
    /// The `request_id` of a `set_model` whose refusal a `model_restriction` line has shown
    /// ([`Core::model_restricted`]; 2.1.325 sends it before the `restricted_by_org` error).
    model_noticed: Option<String>,
    /// The session's cumulative cost and `modelUsage` token sums as of the last `usage` with a
    /// `cost_usd` (or the transcript's last `cost-state` when resumed, none after `/clear`):
    /// the baseline of the next turn's cost and of a compaction's tokens, one value so that
    /// both move together.
    cost_base: Option<CostState>,
    /// The context the main agent's last API response left, or the size a compaction
    /// reported; the `context_*` and `cache_ttl_s` of the next `usage` ([`ContextUse`]).
    context: Option<ContextUse>,
    /// `message.id` of the main agent's latest response and when it started: its first line
    /// (`message_start` or `assistant`) that came here. The prompt cache is read or written
    /// at that moment ([`ContextUse::cache_ttl_s`]).
    response_started: Option<(String, Instant)>,
    /// A `compact_boundary` came since the last `result`: a `result` with no model turn costs
    /// for the compaction itself.
    compacted: bool,
    /// The slash commands last reported by this process.
    commands: Option<Vec<SlashCommand>>,
    /// The commands of the CLI's answer to our `initialize` or of its latest `system`
    /// `commands_changed`, with what it says of each ([`Core::report_commands`]).
    listed: Option<Vec<SlashCommand>>,
    /// `slash_commands` and `terminal_slash_commands` of the latest `system/init`.
    init_commands: Option<Vec<String>>,
    terminal_commands: Vec<String>,
    /// A `conversation_reset` came (`/clear`): the next `system/init` with another
    /// `session_id` names the transcript the CLI goes on in.
    cleared: bool,
    /// [`Core::clearing`]: from `conversation_reset` until that `system/init` or the `result`
    /// of the turn it came in.
    clearing: bool,
    /// The conversation is new (a new session, `/clear`) and its first model turn has not
    /// ended: the CLI writes its `ai-title` then.
    fresh: bool,
    /// The lines last given for `system/init`'s MCP servers that are not connected and its
    /// plugin errors ([`Core::on_init_problems`]): a later `init` with the same gives none.
    init_problems: (Option<String>, Option<String>),
    /// The main agent's records of this turn and how many content blocks each had: the
    /// events a retraction names come from them ([`Core::retract`]).
    turn_records: HashMap<Uuid, usize>,
    /// Events retracted in this turn.
    retracted: HashSet<Uuid>,
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
    /// The latest placeholder `task_notification` ([`not_reported_yet`]) of each subagent the
    /// latest `background_tasks_changed` still held ([`Core::listed`]) when it came: not its
    /// end yet. It is handled as the end when the subagent stops counting
    /// ([`Core::report_subagents`]) or the process exits.
    held_ends: BTreeMap<String, Value>,
    /// Subagents whose latest `task_notification` was their end (any but a held placeholder),
    /// until the CLI starts them again: a task of theirs still listed is the session's
    /// background work, it does not count them as running ([`Core::running_subagents`]).
    reported: HashSet<String>,
    /// Tasks whose end gave its line, with a hash of the line's text: a later
    /// `task_notification` of the same `task_id` gives a line only when its text differs and is
    /// not the placeholder (2.1.312 sends one each time a subagent stops, also after its report
    /// when a command it left running ends and the CLI resumes it, with the same text; 2.1.329
    /// sends the text a subagent stopped with while its commands ran, "the result below may be
    /// interim", and its real report after the CLI resumed it: "the later one replaces what the
    /// earlier one showed", which an append-only feed cannot do).
    announced: HashMap<String, u64>,
    /// Subagents of [`Core::reported`] whose end came while the latest list held commands they
    /// started ([`left_by`]), with when and their task (the `description` of their
    /// `task_started`), until those commands leave the list, the CLI starts the subagent again
    /// or [`Core::leftovers`] tells of them.
    left_running: HashMap<String, (Instant, String)>,
    /// The main agent's output tokens since the last [`Core::take_output_tokens`]: those of
    /// the response being written as estimated from its deltas, the rest of its
    /// `usage.output_tokens` at its end.
    output_tokens: u64,
    /// The response being written: characters of its deltas, the CLI's estimates of its
    /// redacted thinking, and the tokens counted for it so far.
    streamed_chars: u64,
    streamed_estimated: u64,
    streamed: u64,
    /// The main agent's text deltas were sent since its last assistant text: the hub and
    /// clients show text that no `agent.message` will cover if the CLI abandons the attempt
    /// ([`Core::on_stream_event`], [`Core::on_assistant`]).
    live_text: bool,
    /// The same for the main agent's thinking deltas since its last saved thinking or text: an
    /// abandoned attempt often breaks while it thinks, and its thinking must go with it.
    live_thinking: bool,
    /// `message.id` of the main agent's latest `message_start`: the response being streamed.
    streamed_id: Option<String>,
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
    /// A `conversation_reset` came, and neither has a `system/init` named the new transcript
    /// yet nor has the turn it came in ended.
    pub fn clearing(&self) -> bool {
        self.clearing
    }

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
            held_at: None,
            goals_behind: Vec::new(),
            compaction: None,
            activity: None,
            activity_at: None,
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
            wakeup_calls: HashSet::new(),
            wakeup_reasons: HashMap::new(),
            wakeup: None,
            wakeup_row: None,
            cron_creates: HashMap::new(),
            cron_deletes: HashSet::new(),
            cron_jobs: Vec::new(),
            todo_calls: HashMap::new(),
            task_list: Vec::new(),
            live_changed: false,
            recount: false,
            background: Vec::new(),
            ambient: HashSet::new(),
            in_turn: false,
            self_turn: false,
            stopping: false,
            turn: 0,
            interrupt_answers: HashSet::new(),
            cancel_answers: HashMap::new(),
            pending: HashMap::new(),
            rules: SessionRules::default(),
            policy: Arc::default(),
            folder: None,
            model: None,
            model_request: None,
            model_requested: None,
            model_noticed: None,
            cost_base: None,
            context: None,
            response_started: None,
            compacted: false,
            commands: None,
            listed: None,
            init_commands: None,
            terminal_commands: Vec::new(),
            cleared: false,
            clearing: false,
            fresh: false,
            init_problems: (None, None),
            turn_records: HashMap::new(),
            retracted: HashSet::new(),
            permission_mode: None,
            seen: Vec::new(),
            quiet_tasks: HashMap::new(),
            held_ends: BTreeMap::new(),
            reported: HashSet::new(),
            announced: HashMap::new(),
            left_running: HashMap::new(),
            output_tokens: 0,
            streamed_chars: 0,
            streamed_estimated: 0,
            streamed: 0,
            live_text: false,
            live_thinking: false,
            streamed_id: None,
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

    /// Moves the start of the main agent's last response `by` into the past (tests).
    #[cfg(test)]
    pub fn rewind_response(&mut self, by: Duration) {
        if let Some(c) = self.context.as_mut() {
            c.rewind(by);
        }
        if let Some((_, t)) = self.response_started.as_mut() {
            *t = t.checked_sub(by).unwrap_or(*t);
        }
    }

    /// The session's cumulative cost and `modelUsage` tokens before this process: the
    /// transcript's last `cost-state` when the process resumes it (the CLI starts its
    /// `total_cost_usd` and `modelUsage` from there, checked on 2.1.308, T27).
    pub fn with_cost_baseline(mut self, state: Option<CostState>) -> Self {
        self.cost_base = state;
        self
    }

    /// The process starts a new session's conversation ([`super::Launch::new_conversation`]).
    pub fn with_new_conversation(mut self, new: bool) -> Self {
        self.fresh = new;
        self
    }

    /// The conversation is new and its first model turn has not ended: the token rate's
    /// ticker reads the transcript's title on each tick of it.
    pub fn fresh(&self) -> bool {
        self.fresh
    }

    /// The cron jobs the CLI resurrects when it resumes or forks a transcript
    /// ([`resurrected_cron_jobs`]). A pending wakeup is not re-armed by a resume (checked live
    /// on 2.1.312), so none is seeded.
    pub fn with_resumed_cron(mut self, jobs: Vec<CronJob>) -> Self {
        self.cron_jobs = jobs;
        self
    }

    /// The list of rows to send when the process starts: only when [`Core::with_resumed_cron`]
    /// was given a job, nothing otherwise.
    pub fn resumed_live(&self) -> Vec<Action> {
        if self.cron_jobs.is_empty() {
            return vec![];
        }
        vec![self.live_list()]
    }

    /// Whether turn number `turn` (from [`Core::interrupt`]) is still running.
    pub fn turn_running(&self, turn: u64) -> bool {
        self.turn_open() && self.turn == turn
    }

    /// Whether a turn runs, ours or the CLI's own.
    pub fn turn_open(&self) -> bool {
        self.in_turn || self.self_turn
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
        self.turn_open() || !self.live.is_empty()
    }

    /// [`Core::working`] or a background task of the CLI's list: what keeps a process from
    /// being closed through its stdin ([`Adapter::close`]).
    pub fn busy(&self) -> bool {
        self.working() || self.background_work() > 0
    }

    /// The CLI's session id, from its latest `system/init`.
    pub fn cli_session(&self) -> Option<&str> {
        self.cli_session.as_deref()
    }

    /// The `task_id`s of the subagents of [`Core::live`].
    pub fn live_ids(&self) -> Vec<String> {
        self.live.iter().map(|a| a.task_id.clone()).collect()
    }

    /// Each subagent of [`Core::left_running`] that ended [`LEFTOVER_AFTER`] or more before
    /// `now` → [`AdapterEvent::SubagentLeftovers`] with the commands of the latest list it
    /// started, whose `task_started` came ([`Core::bash_tasks`]), how long each has run and how
    /// long ago the subagent ended; the subagent is then forgotten, so it is told once.
    pub fn leftovers(&mut self, now: Instant) -> Vec<Action> {
        let due: Vec<String> = self
            .left_running
            .iter()
            .filter(|(_, (at, _))| now.saturating_duration_since(*at) >= LEFTOVER_AFTER)
            .map(|(id, _)| id.clone())
            .collect();
        let mut out = Vec::new();
        for id in due {
            let Some((ended, subagent)) = self.left_running.remove(&id) else {
                continue;
            };
            let commands: Vec<LeftoverCommand> = self
                .background
                .iter()
                .filter(|t| left_by(t, &id))
                .filter_map(|t| {
                    let task_id = t["task_id"].as_str()?;
                    let (_, started) = self.bash_tasks.get(task_id)?;
                    Some(LeftoverCommand {
                        task_id: task_id.to_owned(),
                        description: preview(
                            t["description"].as_str().unwrap_or(""),
                            PREVIEW_LIMIT,
                        ),
                        running_s: now.saturating_duration_since(*started).as_secs(),
                    })
                })
                .collect();
            if commands.is_empty() {
                continue;
            }
            tracing::info!(session_id = %self.session_id, task_id = %id, commands = commands.len(), "subagent left commands running");
            out.push(Action::Emit(AdapterEvent::SubagentLeftovers {
                subagent,
                ended_s: now.saturating_duration_since(ended).as_secs(),
                commands,
            }));
        }
        out
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
        // The last text of a turn the CLI started by itself stays held, as an input written
        // into a turn of hqc's leaves it ([`Core::inject_with_images`]): that turn's next line,
        // its `result`, ends it. Dropped here, it was lost when the input came between the
        // text and the `result` (SPEC §8.1).
        let mut out = Vec::new();
        if !self.self_turn {
            out.extend(self.drop_held());
        }
        out.push(self.write(input, images, false, false));
        out
    }

    /// An input that may join (a person's input or a `send`, SPEC §17.2) may go into the
    /// running turn: a turn runs and no permission request or question is open.
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
        Some(vec![self.write(input, images, true, false)])
    }

    /// [`Core::inject_with_images`] with `priority: "now"` (SPEC 0.219): the CLI moves the
    /// running Bash, MCP call or subagent to the background and takes the line into the same
    /// turn at once, instead of at the end of the call; a turn with no open call ends
    /// `aborted_streaming` and runs the line as a turn of its own, which comes back as a
    /// carried input.
    pub fn inject_now_with_images(
        &mut self,
        input: &SessionInput,
        images: Vec<Value>,
    ) -> Option<Vec<Action>> {
        if !self.can_inject() {
            return None;
        }
        Some(vec![self.write(input, images, true, true)])
    }

    /// The user message of stream-json, with a fresh `uuid` its echo and `command_lifecycle`
    /// will name. `now`: `priority: "now"`. What the owner typed (`input.human`, which the hub
    /// sets for no message of another session, room input, schedule run or note of its own)
    /// carries `origin: {kind: "human"}`, without which the CLI takes the line as unattributed
    /// and its strict `isHuman()` gates refuse it (2.1.312). Every input is a line of its own:
    /// the worker joins no two, so no line mixes a human's text with another's.
    fn write(
        &mut self,
        input: &SessionInput,
        images: Vec<Value>,
        mid_turn: bool,
        now: bool,
    ) -> Action {
        // `peer_prefix` and `peer` are in `text` already (crate::peer, SPEC §7.1).
        let uuid = Uuid::new_v4();
        self.wrote = true;
        self.awaiting_echo.push_back(Written {
            uuid,
            event_id: input.event_id,
            mid_turn,
            state: None,
            carried: false,
            withdrawing: None,
        });
        let content = if images.is_empty() {
            json!(input.text)
        } else {
            let mut blocks = vec![json!({"type": "text", "text": input.text})];
            blocks.extend(images);
            Value::Array(blocks)
        };
        let mut line = json!({
            "type": "user",
            "uuid": uuid,
            "message": {"role": "user", "content": content},
        });
        if input.human {
            line["origin"] = json!({"kind": "human"});
        }
        if now {
            line["priority"] = json!("now");
        }
        Action::Write(line)
    }

    /// `control_request` `cancel_async_message` for the input `event_id` the CLI has not taken
    /// (SPEC 0.219), with the request's id ([`Core::withdraw_answer`]); `None` when no such
    /// input waits: it was echoed, or its `command_lifecycle` says the CLI started it, or the
    /// CLI is already asked. On `cancelled: true` the CLI drops the line (`command_lifecycle`
    /// `cancelled`, 2.1.312), which then never reaches the model and is no `dropped` input of
    /// a turn's end; the input is gone from [`Core::awaiting_echo`] and, when it was what kept
    /// a turn running, that turn is over ([`Core::carried_gone`]).
    pub fn withdraw(&mut self, event_id: Uuid) -> Option<(Action, String)> {
        let w = self.awaiting_echo.iter_mut().find(|w| {
            w.event_id == event_id
                && w.withdrawing.is_none()
                && !matches!(w.state.as_deref(), Some("started" | "completed"))
        })?;
        let request_id = format!("{CANCEL_REQUEST}{}", Uuid::now_v7());
        w.withdrawing = Some(request_id.clone());
        let req = json!({
            "type": "control_request",
            "request_id": request_id,
            "request": {"subtype": "cancel_async_message", "message_uuid": w.uuid},
        });
        Some((Action::Write(req), request_id))
    }

    /// Whether the CLI dropped the message of the `cancel_async_message` with this id, once
    /// it answered; the answer is taken out, so it is told once.
    pub fn withdraw_answer(&mut self, request_id: &str) -> Option<bool> {
        self.cancel_answers.remove(request_id)
    }

    /// The CLI did not answer the `cancel_async_message` with this id in time (SPEC 0.219): the
    /// input is no longer being withdrawn. An answer or a `cancelled` line that comes late is
    /// then no word of the owner's withdrawal: the input goes the way of any other the CLI
    /// drops (`dropped` at the turn's end, resent), instead of being taken for a withdrawn one
    /// that is out of the supervisor's books.
    pub fn withdraw_unanswered(&mut self, request_id: &str) {
        self.cancel_answers.remove(request_id);
        if let Some(w) = self
            .awaiting_echo
            .iter_mut()
            .find(|w| w.withdrawing.as_deref() == Some(request_id))
        {
            w.withdrawing = None;
        }
    }

    /// `control_request` `initialize` with only `perTaskStopAffordance: true`, the first line
    /// of every process's stdin (SPEC §8.1): with it an `interrupt` stops the turn and spares
    /// the background subagents and workflows, which it kills otherwise (2.1.312). The CLI
    /// applies it only before the first user message: one after it is taken as a second
    /// client's and changes nothing. Its answer gives the CLI's slash commands with their
    /// descriptions ([`Core::on_control_response`]).
    pub fn initialize(&self) -> Action {
        Action::Write(json!({
            "type": "control_request",
            "request_id": format!("{INIT_REQUEST}{}", Uuid::now_v7()),
            "request": {"subtype": "initialize", "perTaskStopAffordance": true},
        }))
    }

    /// `control_request` `interrupt` for the running turn, also one the CLI started by
    /// itself, with the turn's number and the request's id ([`Core::interrupt_answered`]);
    /// `None` if no turn runs.
    pub fn interrupt(&mut self) -> Option<(Action, u64, String)> {
        if !self.turn_open() {
            return None;
        }
        self.stopping = true;
        let request_id = format!("{INTERRUPT_REQUEST}{}", Uuid::now_v7());
        let req = json!({
            "type": "control_request",
            "request_id": request_id,
            "request": {"subtype": "interrupt"},
        });
        Some((Action::Write(req), self.turn, request_id))
    }

    /// Whether the CLI answered the `interrupt` with this id; the answer is taken out of
    /// the set, so it is told once.
    pub fn interrupt_answered(&mut self, request_id: &str) -> bool {
        self.interrupt_answers.remove(request_id)
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
    pub fn set_model(&mut self, model: &str) -> Action {
        let id = format!("{MODEL_REQUEST}{}", Uuid::now_v7());
        self.model_request = Some(id.clone());
        self.model_requested = Some(model.to_owned());
        Action::Write(json!({
            "type": "control_request",
            "request_id": id,
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

    /// Stop task `task_id` alone, a `stoppable` row of [`AdapterEvent::LiveSubagents`], a
    /// subagent, a command or a Monitor (2.1.312 registers a Monitor's task as `local_bash`, and
    /// `stop_task` stops a task as its type does): `control_request {subtype: stop_task,
    /// task_id}`. The CLI answers
    /// `success` with `{}`, also for a task it does not know or one that has ended, or an error
    /// with the reason it did not stop it; a stopped subagent then ends with `task_updated` and
    /// `task_notification` `stopped`, as any end of it (by the 2.1.312 code, not checked live); a
    /// command ends so too (checked live on 2.1.312).
    pub fn stop_task(&self, task_id: &str) -> Action {
        Action::Write(json!({
            "type": "control_request",
            "request_id": format!("{STOP_TASK_REQUEST}{}", Uuid::now_v7()),
            "request": {"subtype": "stop_task", "task_id": task_id},
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
            p @ (Pending::Clarify { .. } | Pending::Elicitation { .. }) => {
                self.pending.insert(request_id.into(), p);
                bail!("request {request_id} is a clarify request");
            }
        };
        // A background subagent's request outlives the main turn's `result` (`on_result`): its
        // "No" must not interrupt a turn that runs by then for something else.
        let foreground = ends_with_main_turn(&self.quiet_tasks, agent_id.as_deref());
        let server = hqc_proto::rules::mcp_server(&tool).map(String::from);
        let resp = match option_id {
            "once" => allow(request_id, &input, None),
            // As the VS Code tab: "No" stops the turn, a text answer lets it go on.
            "deny" => match message {
                Some(m) => deny_with(request_id, &format!("{DENY_WITH_TEXT}{m}"), false),
                None => {
                    self.stopping |= foreground;
                    deny_with(request_id, DENY_MESSAGE, foreground)
                }
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
            // All tools of the call's MCP server; for `server_always` the hub adds the
            // folder rule `mcp__<server>` before it forwards the answer, and the session
            // rule covers the calls until the worker has that list. The session rule lives
            // as long as the process: a folder rule deleted later still allows the server
            // in this session until it restarts.
            OPTION_SERVER_SESSION | OPTION_SERVER_ALWAYS if server.is_some() => {
                if let Some(server) = &server {
                    self.rules.add_server(server);
                }
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
    /// `action`: the person's choice on an MCP server's request.
    pub fn answer_clarify(
        &mut self,
        request_id: &str,
        answers: &BTreeMap<String, ClarifyAnswer>,
        action: Option<ClarifyAction>,
    ) -> anyhow::Result<Vec<Action>> {
        let (input, questions) = match self.pending.remove(request_id) {
            Some(Pending::Clarify {
                input, questions, ..
            }) => (input, questions),
            Some(Pending::Elicitation { fields }) => {
                return Ok(vec![
                    self.answer_elicitation(request_id, &fields, answers, action)
                ]);
            }
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

    /// The answer to an MCP server's request: `accept` with the answers typed by its schema (a
    /// Yes/No as a boolean, a number's text as a number when it reads as one, an empty answer
    /// left out; no `content` without questions, as for a link), `decline`, or, for empty
    /// `answers` without `action` (the hub cancelled the card), `cancel`.
    fn answer_elicitation(
        &self,
        request_id: &str,
        fields: &[(String, Field)],
        answers: &BTreeMap<String, ClarifyAnswer>,
        action: Option<ClarifyAction>,
    ) -> Action {
        let accept = match action {
            Some(ClarifyAction::Accept) => true,
            None => !answers.is_empty(),
            Some(_) => false,
        };
        let response = match action {
            Some(ClarifyAction::Decline) => json!({"action": "decline"}),
            _ if accept && fields.is_empty() => json!({"action": "accept"}),
            _ if accept => json!({"action": "accept", "content": content(fields, answers)}),
            _ => json!({"action": "cancel"}),
        };
        tracing::info!(session_id = %self.session_id, action = %response["action"], "elicitation answered");
        Action::Write(control_response(request_id, response))
    }

    /// The process is gone: a held compaction and a held text are sent as they are, open
    /// requests are dropped.
    pub fn on_exit(&mut self) -> Vec<Action> {
        self.pending.clear();
        self.awaiting_echo.clear();
        self.in_turn = false;
        self.self_turn = false;
        self.stopping = false;
        self.interrupt_answers.clear();
        self.cancel_answers.clear();
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
                self.turn_records.insert(u, blocks(&v).len());
            }
        }
        // A Monitor's task is listed as a command ([`Core::bash_tasks`]), a subagent's too. A
        // wakeup and a cron job are the main agent's only (2.1.312 arms none for a subagent).
        if ty == "assistant" {
            for b in blocks(&v) {
                let Some(id) = b["id"].as_str().filter(|_| b["type"] == "tool_use") else {
                    continue;
                };
                match b["name"].as_str() {
                    Some("Monitor") => {
                        self.monitor_calls.insert(id.to_owned());
                    }
                    Some("ScheduleWakeup") if !sub => {
                        self.wakeup_calls.insert(id.to_owned());
                        self.wakeup_reasons.insert(
                            id.to_owned(),
                            preview(b["input"]["reason"].as_str().unwrap_or(""), PREVIEW_LIMIT),
                        );
                    }
                    Some("CronCreate") if !sub => {
                        self.cron_creates.insert(
                            id.to_owned(),
                            (
                                b["input"]["cron"].as_str().unwrap_or("").to_owned(),
                                preview(b["input"]["prompt"].as_str().unwrap_or(""), PREVIEW_LIMIT),
                            ),
                        );
                    }
                    Some("CronDelete") if !sub => {
                        self.cron_deletes.insert(id.to_owned());
                    }
                    Some(tool @ ("TodoWrite" | "TaskCreate" | "TaskUpdate" | "TaskList"))
                        if !sub =>
                    {
                        self.todo_calls
                            .insert(id.to_owned(), (tool.to_owned(), b["input"].clone()));
                    }
                    _ => {}
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
        // What the agent waited on ends with its next output, before that output's events.
        if let Some(a) = activity_of(ty, sub, &v) {
            out.extend(self.set_activity(a));
        }
        out.extend(match ty {
            "tool_progress" => self.on_tool_progress(&v),
            "stream_event" if !sub => self.on_stream_event(&v),
            "assistant" if !sub => self.on_assistant(&v),
            "user" if v["isReplay"] == true => self.on_echo(&v),
            "user" if !sub => self.on_user(&v),
            "command_lifecycle" => self.on_lifecycle(&v),
            "rate_limit_event" => limits(&v["rate_limit_info"])
                .map(|l| Action::Emit(AdapterEvent::Limits(l)))
                .into_iter()
                .collect(),
            "result" => self.on_result(&v),
            "control_request" => self.on_control_request(&v),
            "control_cancel_request" => self.on_cancel_request(&v),
            "control_response" => self.on_control_response(&v),
            // `/clear` (2.1.312): the CLI goes on in a new conversation, whose transcript the
            // next `system/init` names, and counts `total_cost_usd` from 0 again.
            "conversation_reset" => {
                tracing::info!(session_id = %self.session_id, trigger = ?v["trigger"].as_str(), "conversation cleared");
                self.cleared = true;
                self.clearing = true;
                self.fresh = true;
                self.cost_base = None;
                // The new conversation holds nothing of the old one's context or task list.
                (self.context, self.response_started) = (None, None);
                self.todo_calls.clear();
                let mut out = self.tasks_changed(|list| list.clear());
                out.push(self.emit_system(CONVERSATION_CLEARED, CONVERSATION_CLEARED_TEXT.into()));
                out
            }
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
                    // The first `init` after `/clear` comes with the new conversation's id, the
                    // name of its transcript (`w1-clear`): the session goes on there.
                    let id = v["session_id"].as_str();
                    if let Some(new) = id
                        .filter(|id| self.cleared && Some(*id) != self.cli_session.as_deref())
                        .and_then(|id| id.parse::<Uuid>().ok())
                    {
                        self.cleared = false;
                        self.clearing = false;
                        tracing::info!(session_id = %self.session_id, runtime_session_id = %new, "the conversation goes on in a new transcript");
                        out.push(Action::Emit(AdapterEvent::RuntimeSessionId(new.to_string())));
                    }
                    self.cli_session = id.map(str::to_owned);
                    out.extend(self.on_mode(&v["permissionMode"], false));
                    out.extend(self.on_init(&v));
                    out.extend(self.on_init_problems(&v));
                    out
                }
                Some("status") => {
                    let mut out = self.on_mode(&v["permissionMode"], false);
                    if v["compact_result"] == "failed" {
                        // After a text held before it, as the other lines of a turn.
                        out.extend(self.flush(false));
                        let t = match v["compact_error"].as_str() {
                            Some(e) => format!("Compaction failed: {}", text(e)),
                            None => "Compaction failed".into(),
                        };
                        out.push(self.emit_system(COMPACTION_FAILED, t));
                    }
                    out
                }
                Some("permission_denied") => {
                    let mut out = self.flush(false);
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
                    // An `ambient` one is no activity (the SDK): no running subagent.
                    if let Some(id) = v["task_id"].as_str().filter(|_| v["ambient"] == true) {
                        self.ambient.insert(id.to_owned());
                    }
                    let agent = v["task_type"] == "local_agent" && v["ambient"] != true;
                    if let Some(id) = v["task_id"].as_str().filter(|_| agent) {
                        self.reported.remove(id);
                        self.left_running.remove(id);
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
                    // The size after the compaction until the next response, which is in no
                    // cache yet; a boundary that gives none (the runtime did not count it) must
                    // not leave the size before it.
                    let last = self.context.take();
                    self.context = compact_metadata(&v["compact_metadata"])
                        .and_then(|m| m.post_tokens)
                        .map(|tokens| ContextUse {
                            tokens,
                            output: 0,
                            message_id: None,
                            model: last.and_then(|l| l.model),
                            cache_1h: false,
                            cache_5m: false,
                            started: None,
                        });
                    tracing::info!(session_id = %self.session_id, trigger = ?v["compact_metadata"]["trigger"].as_str(), "context compacted");
                    self.compaction = Some((
                        record_uuid(&v).map_or_else(Uuid::now_v7, |u| record_event_id(&u, 0)),
                        compact_metadata(&v["compact_metadata"]),
                    ));
                    vec![]
                }
                // A person gave the session a title (`/rename`), at once. The CLI's own title
                // is not streamed (2.1.312): the adapter reads it from the transcript.
                Some("session_title_changed") => v["title"]
                    .as_str()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(|t| Action::Emit(AdapterEvent::Title(text(t))))
                    .into_iter()
                    .collect(),
                // The whole list again after a change during the session (a skill found as
                // the agent works in a subfolder, `/reload-plugins`).
                Some("commands_changed") => match command_list(&v["commands"]) {
                    Some(list) => {
                        self.listed = Some(list);
                        self.report_commands()
                    }
                    None => vec![],
                },
                Some("informational") => match (
                    v["model_restriction"]["effective_model"].as_str(),
                    v["content"].as_str(),
                ) {
                    // The CLI refused the model asked for and runs on another (2.1.325): the
                    // model kept, as for a refused `set_model`, in hqc's words.
                    (Some(m), _) if !m.trim().is_empty() => self.model_restricted(
                        v["model_restriction"]["requested_model"].as_str(),
                        v["model_restriction"]["at"].as_str(),
                        m,
                    ),
                    // "Stop hook failed: " with no reason after it says nothing; the
                    // desktop app hides it too.
                    (_, Some(t))
                        if v["level"] != "info"
                            && !t.trim().is_empty()
                            && t.trim() != "Stop hook failed:" =>
                    {
                        vec![self.emit_system(RUNTIME_NOTICE, text(t))]
                    }
                    _ => vec![],
                },
                // The CLI's Stop-hook notices are not lines, as in the VS Code tab and the
                // desktop app: `stop-hook-error` is the terminal's "Stop hook error occurred ·
                // ctrl+o to see", sent the first time in a row a Stop hook blocks the end of
                // the turn and when a Stop hook fails (its output is only in the terminal's
                // transcript view); `stop-hook-block-cap` repeats the `informational` line of
                // the same cap.
                Some("notification")
                    if matches!(
                        v["key"].as_str(),
                        Some("stop-hook-error" | "stop-hook-block-cap")
                    ) =>
                {
                    vec![]
                }
                Some("notification") => match v["text"].as_str() {
                    Some(t) if !t.trim().is_empty() => vec![self.emit_system(RUNTIME_NOTICE, text(t))],
                    _ => vec![],
                },
                Some("model_fallback" | "model_consent_fallback") => {
                    // The model of this turn's `usage` from now on.
                    if let Some(m) = v["fallback_model"].as_str() {
                        self.model = Some(m.to_owned());
                    }
                    vec![self.emit_system(MODEL_FALLBACK, switch_line(&v))]
                }
                Some("model_refusal_fallback") => {
                    // `scope` `session` (or none): the session's model is swapped; `local`:
                    // only a subagent's or a side question's answer came from it (2.1.312).
                    if v["scope"] != "local" {
                        if let Some(m) = v["fallback_model"].as_str() {
                            self.model = Some(m.to_owned());
                        }
                    }
                    let mut out = self.retract(&v["retracted_message_uuids"]);
                    out.push(self.emit_system(MODEL_REFUSAL_FALLBACK, switch_line(&v)));
                    out
                }
                Some("model_refusal_no_fallback") => {
                    vec![self.emit_system(MODEL_REFUSAL, refusal_line(&v))]
                }
                // A CLI that exits with status 0 (after its stdin is closed) reports a disk
                // that was full: what it could not write is lost.
                Some("session_writes_lost") => self.on_writes_lost(&v),
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

    /// The CLI's answer to our `initialize` (its slash commands with their descriptions, its
    /// models and account, or an error), `set_permission_mode`, `set_model` (a refusal of the
    /// latest one is a line and the model kept, another answer logged), `background_tasks` or
    /// `stop_task` (both logged), or `interrupt` (noted for [`Core::interrupt_answered`]);
    /// every other `control_response`, the echo of one of ours, is skipped.
    fn on_control_response(&mut self, v: &Value) -> Vec<Action> {
        let r = &v["response"];
        let id = r["request_id"].as_str().unwrap_or("");
        if id.starts_with(INIT_REQUEST) {
            let Some(list) =
                command_list(&r["response"]["commands"]).filter(|_| r["subtype"] == "success")
            else {
                tracing::warn!(session_id = %self.session_id, error = %r["error"], "claude answered initialize without commands");
                return vec![];
            };
            self.listed = Some(list);
            return self.report_commands();
        }
        if id.starts_with(INTERRUPT_REQUEST) {
            self.interrupt_answers.insert(id.to_owned());
            return vec![];
        }
        if id.starts_with(CANCEL_REQUEST) {
            return self.on_cancel_answer(id, r);
        }
        if id.starts_with(STOP_TASK_REQUEST) {
            if r["subtype"] == "success" {
                tracing::info!(session_id = %self.session_id, request_id = id, "task stopped");
            } else {
                tracing::warn!(session_id = %self.session_id, request_id = id, error = %r["error"], "claude did not stop the task");
            }
            return vec![];
        }
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
                return vec![];
            }
            tracing::warn!(session_id = %self.session_id, error = %r["error"], "claude refused the model switch");
            // A refusal of a switch the user made again since: the model it names is not the
            // one asked for last, so it gives neither a line nor the model kept.
            if self
                .model_request
                .as_deref()
                .is_some_and(|latest| latest != id)
            {
                return vec![];
            }
            // The `model_restriction` line before it said so already (2.1.325).
            if r["error_code"] == "restricted_by_org" && self.model_noticed.as_deref() == Some(id) {
                return vec![];
            }
            // The error names the model the CLI kept (2.1.312): the session's again. The fixed
            // "set_model failed" can follow a switch that applied, naming the model asked for.
            let kept = r["model"].as_str().map(text);
            if kept.is_some() && kept == self.model_requested {
                return vec![];
            }
            let why = r["error"].as_str().map_or_else(|| "refused".into(), text);
            let t = match &kept {
                Some(m) => format!("Model not switched: {why}. The agent stays on {m}"),
                None => format!("Model not switched: {why}"),
            };
            let mut out = vec![self.emit_new(EventKind::System {
                text: t,
                code: MODEL_NOT_SWITCHED.into(),
                call_id: None,
                status: None,
                task_id: None,
                from_session_id: None,
                usage: None,
                fork: None,
                compaction: None,
                model: kept.clone(),
                goal: None,
            })];
            out.extend(kept.map(|m| Action::Emit(AdapterEvent::ModelKept(m))));
            return out;
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

    /// The CLI's answer to a `cancel_async_message` ([`Core::withdraw`]): `cancelled: true`
    /// drops the input from the books, any other answer leaves it waiting. Either is noted for
    /// [`Core::withdraw_answer`].
    fn on_cancel_answer(&mut self, id: &str, r: &Value) -> Vec<Action> {
        let cancelled = r["subtype"] == "success" && r["response"]["cancelled"] == true;
        self.cancel_answers.insert(id.to_owned(), cancelled);
        let Some(i) = self
            .awaiting_echo
            .iter()
            .position(|w| w.withdrawing.as_deref() == Some(id))
        else {
            // A turn's end dropped it already ([`Core::on_result`]).
            return vec![];
        };
        if !cancelled {
            tracing::info!(session_id = %self.session_id, request_id = id, "claude did not cancel the message; it has been taken");
            self.awaiting_echo[i].withdrawing = None;
            return vec![];
        }
        let Some(w) = self.awaiting_echo.remove(i) else {
            return vec![];
        };
        tracing::info!(session_id = %self.session_id, event_id = %w.event_id, "input withdrawn from the CLI's queue");
        if w.carried || !w.mid_turn {
            return self.carried_gone(vec![]);
        }
        vec![]
    }

    /// A carried input, or the one that began the turn, left the CLI's queue without being
    /// taken (cancelled by the CLI, or withdrawn): the turn it kept running is over, with no
    /// `result` to come, unless other carried inputs go on. [`AdapterEvent::TurnEnded`] with
    /// those in `carried` and `dropped` the inputs the runtime will not take.
    fn carried_gone(&mut self, dropped: Vec<Uuid>) -> Vec<Action> {
        let carried: Vec<Uuid> = self
            .awaiting_echo
            .iter()
            .filter(|w| w.carried)
            .map(|w| w.event_id)
            .collect();
        // As at `result`: the CLI goes on with the other carried inputs, in a turn nobody stops.
        self.stopping = false;
        self.in_turn = !carried.is_empty();
        if self.in_turn {
            self.turn += 1;
        }
        vec![Action::Emit(AdapterEvent::TurnEnded { carried, dropped })]
    }

    /// `system/session_writes_lost` (CLI 2.1.317): a line when the transcript is among what
    /// was lost; the global config alone is only logged.
    fn on_writes_lost(&self, v: &Value) -> Vec<Action> {
        let code = v["error_code"].as_str();
        let lost = |what: &str| {
            v["lost"]
                .as_array()
                .is_some_and(|l| l.iter().any(|w| w == what))
        };
        tracing::warn!(session_id = %self.session_id, error_code = code.unwrap_or(""), lost = %v["lost"], "claude could not write to disk");
        if !lost("transcript") {
            return vec![];
        }
        let (why, fix) = match code {
            Some("ENOSPC") => (
                "the disk is full (ENOSPC)".to_owned(),
                "Free up space on the worker's disk",
            ),
            Some("EDQUOT") => (
                "the disk quota is used up (EDQUOT)".to_owned(),
                "Free up space or raise the quota on the worker's disk",
            ),
            Some(other) => (
                format!("a write to the worker's disk failed ({other})"),
                "Check the worker's disk",
            ),
            None => (
                "a write to the worker's disk failed".to_owned(),
                "Check the worker's disk",
            ),
        };
        vec![self.emit_system(
            "transcript_not_saved",
            format!(
                "Transcript not saved: {why}; resuming this session may miss part of its conversation. {fix}"
            ),
        )]
    }

    /// `slash_commands` and `terminal_slash_commands` of `system/init`, for
    /// [`Core::report_commands`].
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
        self.terminal_commands = names(&v["terminal_slash_commands"]);
        self.init_commands = Some(names(&v["slash_commands"]));
        self.report_commands()
    }

    /// The CLI's slash commands: those of its answer to `initialize` or of its latest
    /// `commands_changed` with their descriptions and argument hints, else (before either, or
    /// a CLI without them) the names of `slash_commands` of the latest `system/init`; without
    /// those that only work in the terminal UI (`terminal_slash_commands` of the latest
    /// `init`) and the CLI's internal ones (`__name`), within [`COMMANDS_LIMIT`] and
    /// [`COMMAND_NAME_LIMIT`]. Reported once per process and again only when the list changes
    /// (`/reload-plugins`, a new skill).
    fn report_commands(&mut self) -> Vec<Action> {
        let all: Vec<SlashCommand> = match (&self.listed, &self.init_commands) {
            (Some(l), _) => l.clone(),
            (None, Some(names)) => names
                .iter()
                .map(|n| SlashCommand {
                    name: n.clone(),
                    description: None,
                    argument_hint: None,
                })
                .collect(),
            (None, None) => return vec![],
        };
        let mut list: Vec<SlashCommand> = Vec::new();
        for c in all {
            if !c.name.is_empty()
                && !c.name.starts_with('_')
                && c.name.chars().count() <= COMMAND_NAME_LIMIT
                && !self.terminal_commands.contains(&c.name)
                && !list.iter().any(|l| l.name == c.name)
            {
                list.push(c);
                if list.len() == COMMANDS_LIMIT {
                    break;
                }
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
    /// below the baseline (a CLI that did not carry the cost over) does not lower it. The
    /// token baseline moves with it, to the result's `modelUsage` sums, likewise never lowered.
    fn turn_cost(&mut self, total: f64, tokens: TokenSums) -> f64 {
        let base = self.cost_base.unwrap_or_default();
        self.cost_base = Some(CostState {
            total_cost_usd: base.total_cost_usd.max(total),
            tokens: base.tokens.max(tokens),
        });
        // Rounded to 1e-10 USD: the difference of two decimals is not one in binary.
        ((total - base.total_cost_usd).max(0.0) * 1e10).round() / 1e10
    }

    fn on_stream_event(&mut self, v: &Value) -> Vec<Action> {
        let ev = &v["event"];
        match ev["type"].as_str() {
            Some("message_start") => {
                (self.streamed_chars, self.streamed_estimated, self.streamed) = (0, 0, 0);
                self.streamed_id = ev["message"]["id"].as_str().map(str::to_owned);
                self.response_started = self.streamed_id.clone().map(|id| (id, Instant::now()));
                // A new request of the model after text no assistant text took up: the CLI
                // abandoned the attempt that wrote it (a stream that broke, then a retry), as the
                // VS Code tab takes it (`web:163707`); the hub and clients drop it, and the
                // thinking written with it.
                if std::mem::take(&mut self.live_text) | std::mem::take(&mut self.live_thinking) {
                    return vec![self.emit_new(EventKind::AgentDelta {
                        text: String::new(),
                        reset: true,
                        thinking: false,
                    })];
                }
            }
            // The exact count comes once per response, at its end: it adds what the estimate
            // of its deltas missed.
            Some("message_delta") => {
                let n = ev["usage"]["output_tokens"].as_u64().unwrap_or(0);
                // The lines of a response carry the usage of its `message_start`, with a few
                // output tokens: its own count is the context's.
                if let Some(c) = self.context.as_mut().filter(|c| {
                    c.message_id.is_some() && c.message_id.as_deref() == self.streamed_id.as_deref()
                }) {
                    c.tokens = c.tokens.saturating_sub(c.output) + n;
                    c.output = n;
                }
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
        // A thinking chunk carries text only with `--thinking-display summarized` (2.1.312); an
        // empty one gives nothing.
        let (field, thinking) = match delta["type"].as_str() {
            Some("text_delta") => ("text", false),
            Some("thinking_delta") => ("thinking", true),
            _ => return vec![],
        };
        match delta[field].as_str().filter(|t| !t.is_empty()) {
            Some(t) => {
                if thinking {
                    self.live_thinking = true;
                } else {
                    self.live_text = true;
                }
                vec![self.emit_new(EventKind::AgentDelta {
                    text: text(t),
                    reset: false,
                    thinking,
                })]
            }
            None => vec![],
        }
    }

    fn on_assistant(&mut self, v: &Value) -> Vec<Action> {
        let Some(uuid) = record_uuid(v) else {
            tracing::warn!(session_id = %self.session_id, "assistant message without uuid skipped");
            return vec![];
        };
        // The CLI's own reply, not the model's: marked, but held and flushed like any text,
        // so it is the turn's final message when it ends the turn.
        let synthetic = v["message"]["model"] == SYNTHETIC_MODEL;
        // The frames of one response repeat its usage: the last one stands.
        if !synthetic {
            let id = v["message"]["id"].as_str();
            // A response whose `message_start` did not come here starts with its first line.
            if id.is_some() && id != self.response_started.as_ref().map(|(i, _)| i.as_str()) {
                self.response_started = id.map(|id| (id.to_owned(), Instant::now()));
            }
            let started = self
                .response_started
                .as_ref()
                .map_or_else(Instant::now, |(_, t)| *t);
            if let Some(used) = ContextUse::of_response(&v["message"], started) {
                self.context = Some(used);
            }
        }
        // Cut off by an interrupt before the stream completed (2.1.312).
        let aborted = v["aborted"] == true;
        let refusal = synthetic && v["message"]["stop_reason"] == REFUSAL;
        // Another model's answer in place of a refused one: what it replaces goes first.
        let mut out = self.retract(&v["supersedes"]);
        // A response other than the one streamed while that one's text stands: the CLI gave up
        // the stream and asked again without one ("Error streaming, falling back to
        // non-streaming mode", 2.1.312), which brings no `message_start`; as the VS Code tab
        // takes it (`web:171092`), the text so far is dropped first.
        let id = v["message"]["id"].as_str();
        if (self.live_text || self.live_thinking)
            && !synthetic
            && id.is_some()
            && id != self.streamed_id.as_deref()
        {
            (self.live_text, self.live_thinking) = (false, false);
            out.push(self.emit_new(EventKind::AgentDelta {
                text: String::new(),
                reset: true,
                thinking: false,
            }));
        }
        for (i, block) in blocks(v).iter().enumerate() {
            let id = record_event_id(&uuid, i);
            match block["type"].as_str() {
                Some("text") => {
                    let t = block["text"].as_str().unwrap_or("");
                    if t.is_empty() || t == NO_CONTENT {
                        continue;
                    }
                    out.extend(self.flush(false));
                    // A slash command's output, not a reply of the CLI to the model's turn: a
                    // line of its own, as the mirror draws it from the transcript (SPEC 0.218).
                    if synthetic && v["local_command_source"].is_string() {
                        out.push(self.emit_system_as(
                            Some(id),
                            COMMAND_OUTPUT,
                            preview(t, PREVIEW_LIMIT),
                            None,
                        ));
                        continue;
                    }
                    if synthetic {
                        tracing::info!(session_id = %self.session_id, "runtime message");
                    }
                    self.held = Some((id, text(t), synthetic, aborted, refusal));
                    self.held_at = Some(chrono::Utc::now());
                    // A saved text ends the live thinking too, on the hub and in clients.
                    (self.live_text, self.live_thinking) = (false, false);
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
                    self.live_thinking = false;
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
        // The armed wakeup fired: its turn starts, with the CLI's own `command_uuid`. One armed
        // later while this fire waited is still due (`scheduled_for` names the one that fired).
        if v["state"] == "started"
            && v["wakeup_source"] == "loop_wakeup"
            && self.wakeup.is_some()
            && v["scheduled_for"].as_i64() == self.wakeup
        {
            return self.set_wakeup(None);
        }
        // A cron job fired; the line names none (2.1.312).
        if v["state"] == "started" && v["wakeup_source"] == "schedule_wakeup" {
            return self.cron_fired(epoch_ms());
        }
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
        // A withdrawn input is no one's drop: its `cancel_async_message` answer closes it.
        if !(w.carried && state == "cancelled") || w.withdrawing.is_some() {
            return vec![];
        }
        let Some(w) = self.awaiting_echo.remove(i) else {
            return vec![];
        };
        tracing::info!(session_id = %self.session_id, event_id = %w.event_id, "carried input cancelled by the CLI, not taken");
        self.carried_gone(vec![w.event_id])
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
    /// session's running background tasks, sent on every change and `[]` at the end. The
    /// number of [`Core::background_work`] when it changed; a subagent count that went up. One
    /// that went down, or a subagent of [`Core::held_ends`] that stopped counting, waits for the
    /// next line of another kind ([`Core::recount`]): when the last command of a subagent the
    /// CLI already called ended leaves the list, the CLI lists the subagent again only on the
    /// next line, `[]` then `[subagent]`, and then starts it again (SPEC §8.1 **[verify]**:
    /// inferred from a capture where a second task of the subagent kept the list non-empty).
    fn on_background_tasks(&mut self, v: &Value) -> Vec<Action> {
        let before = self.background_work();
        // An `ambient` task is no activity (the SDK): not counted, no row, no subagent, nor the
        // parent of one of its tasks.
        let (ambient, tasks): (Vec<&Value>, Vec<&Value>) = v["tasks"]
            .as_array()
            .into_iter()
            .flatten()
            .partition(|t| t["ambient"] == true);
        self.ambient.extend(
            ambient
                .iter()
                .filter_map(|t| t["task_id"].as_str().map(str::to_owned)),
        );
        // Nor is an `off_host` one (2.1.325, a cloud session): its work runs outside the process,
        // which need not stay up for it.
        self.background = tasks
            .into_iter()
            .filter(|t| t["off_host"] != true)
            .cloned()
            .collect();
        // A subagent whose commands all left the list left none running.
        let background = &self.background;
        self.left_running
            .retain(|id, _| background.iter().any(|t| left_by(t, id)));
        let mut out = Vec::new();
        let n = self.background_work();
        if n != before {
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
    /// subagent ended and left the task running.
    fn running_subagents(&self) -> usize {
        self.running_subagent_ids().len()
    }

    /// The tasks of the latest `background_tasks_changed` list that are work: those of
    /// [`Core::background`], which [`Core::on_background_tasks`] keeps without the ones marked
    /// `ambient: true`. 2.1.312 sets it on tasks that are no activity: its own housekeeping
    /// (`skip_transcript`) and every live-update watcher, such as the one that follows the
    /// comments on a page the agent published with the Artifact tool (`monitor_ws`,
    /// `persistent`, no timeout, so it stays for the whole process). The SDK tells hosts to
    /// leave them out of activity, and the VS Code tab does. Nor the ones marked `off_host: true`
    /// (2.1.325, a cloud session: the work runs outside the process).
    fn background_work(&self) -> u32 {
        u32::try_from(self.background.len()).unwrap_or(u32::MAX)
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

    /// The subagent listed task `t` belongs to, unless that subagent has reported or is one of
    /// [`Core::ambient`] (no subagent: its task is the agent's own). A task marked `ambient` is
    /// none of the list ([`Core::on_background_tasks`]): 2.1.312 gives the comment watcher of a
    /// page a subagent published the subagent as `parent_task_id`, and the watcher stays for
    /// the whole process, so it would keep a subagent that ended counted for good
    /// ([`Core::background_work`]).
    fn parent<'a>(&self, t: &'a Value) -> Option<&'a str> {
        t["parent_task_id"]
            .as_str()
            .filter(|p| !self.reported.contains(*p) && !self.ambient.contains(*p))
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
            self.live
                .push(LiveAgent::new(LiveKind::Agent, id, "", call));
            changed = true;
        }
        changed |= self.update_live_commands();
        changed.then(|| self.live_list())
    }

    /// Brings [`Core::live_commands`] to the main agent's background shell commands and
    /// Monitor watchers of the latest `background_tasks_changed` list ([`Core::background`]:
    /// none is `ambient`, no activity, the SDK says): `task_type: local_bash`, its
    /// `task_started` come (a task joins the list before it, and only that tells a Monitor's
    /// task from a command's, [`Core::bash_tasks`]), and no subagent of [`Core::parent`] (none,
    /// an `ambient` task, or one that has reported): a subagent that counts stands for its
    /// commands with its own row. A new one goes last, a changed `description` replaces its
    /// row's, one that no longer qualifies goes. Whether anything changed.
    fn update_live_commands(&mut self) -> bool {
        let listed: Vec<(String, String, Instant, LiveKind)> = self
            .background
            .iter()
            .filter(|t| t["task_type"] == "local_bash" && self.parent(t).is_none())
            .filter_map(|t| {
                let id = t["task_id"].as_str()?;
                let (monitor, started) = self.bash_tasks.get(id)?;
                let kind = if *monitor {
                    LiveKind::Monitor
                } else {
                    LiveKind::Command
                };
                Some((
                    id.to_owned(),
                    t["description"].as_str().unwrap_or("").to_owned(),
                    *started,
                    kind,
                ))
            })
            .collect();
        let before = self.live_commands.len();
        self.live_commands
            .retain(|c| listed.iter().any(|(id, _, _, _)| *id == c.task_id));
        let mut changed = self.live_commands.len() != before;
        for (id, description, started, kind) in listed {
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
                    let mut c = LiveAgent::new(kind, &id, "", call);
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
                self.live
                    .push(LiveAgent::new(LiveKind::Agent, id, title, call));
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

    /// `tool_progress` (2.1.312): `heartbeat: true` comes every 30 s of a long call of the main
    /// agent (its `parent_tool_use_id` is the call's own id, not a subagent's) →
    /// [`AdapterEvent::Heartbeat`]; `subagent_retry` (`agent_id`, `attempt`, `max_retries`,
    /// `retry_delay_ms`, `error_status`) is an API retry of a subagent of [`Core::live`], whose
    /// row carries it until a line of the same `parent_tool_use_id` (the subagent's `Agent`
    /// call) without it says the retry resolved: the list again.
    fn on_tool_progress(&mut self, v: &Value) -> Vec<Action> {
        if v["heartbeat"] == true {
            return vec![Action::Emit(AdapterEvent::Heartbeat)];
        }
        let call = v["parent_tool_use_id"].as_str().unwrap_or("");
        let r = &v["subagent_retry"];
        let mut changed = false;
        for a in &mut self.live {
            if r.is_object() && r["agent_id"] == a.task_id.as_str() {
                a.retry = Some((call.to_owned(), api_retry(r)));
                changed = true;
            } else if !r.is_object() && a.retry.as_ref().is_some_and(|(c, _)| c == call) {
                a.retry = None;
                changed = true;
            }
        }
        if changed {
            vec![self.live_list()]
        } else {
            vec![]
        }
    }

    /// [`AdapterEvent::Activity`] when `a` differs from what was reported last. A new
    /// estimate of the thinking reported goes at most once per [`THINKING_EVERY`]: the CLI
    /// sends one with every piece of a thinking, and each report is a `session.state`.
    fn set_activity(&mut self, a: Option<Activity>) -> Option<Action> {
        if self.activity == a {
            return None;
        }
        let thinking = |a: &Option<Activity>| matches!(a, Some(Activity::Thinking { .. }));
        if thinking(&self.activity)
            && thinking(&a)
            && self
                .activity_at
                .is_some_and(|t| t.elapsed() < THINKING_EVERY)
        {
            return None;
        }
        self.activity = a.clone();
        self.activity_at = Some(Instant::now());
        Some(Action::Emit(AdapterEvent::Activity(a)))
    }

    /// `retraction` of the events of this turn's records named in `uuids` (the CLI's wire
    /// uuids of `supersedes` and `retracted_message_uuids`) not retracted yet; a held text
    /// among them is dropped. Nothing for none.
    fn retract(&mut self, uuids: &Value) -> Vec<Action> {
        let mut ids = Vec::new();
        for u in uuids.as_array().into_iter().flatten() {
            let Some(u) = u.as_str().and_then(|u| u.parse::<Uuid>().ok()) else {
                continue;
            };
            let n = self.turn_records.get(&u).copied().unwrap_or(0);
            for i in 0..n {
                let id = record_event_id(&u, i);
                if self.retracted.insert(id) {
                    ids.push(id);
                }
            }
        }
        if ids.is_empty() {
            return vec![];
        }
        let mut out = Vec::new();
        if self.held.as_ref().is_some_and(|(id, ..)| ids.contains(id)) {
            out.extend(self.drop_held());
        }
        tracing::info!(session_id = %self.session_id, events = ids.len(), "events retracted");
        out.push(self.emit_new(EventKind::Retraction { event_ids: ids }));
        out
    }

    /// A line for `system/init`'s MCP servers whose `status` is not `connected` nor `disabled`
    /// (turned off by the user) and one for its `plugin_errors`, each when it differs from the
    /// one given last: the CLI sends `init` before every turn.
    fn on_init_problems(&mut self, v: &Value) -> Vec<Action> {
        let list = |k: &str| v[k].as_array().cloned().unwrap_or_default();
        let s = |x: &Value| text(x.as_str().unwrap_or(""));
        let servers: Vec<String> = list("mcp_servers")
            .iter()
            .filter(|m| !matches!(m["status"].as_str(), Some("connected" | "disabled")))
            .map(|m| format!("{} ({})", s(&m["name"]), s(&m["status"])))
            .collect();
        let plugins: Vec<String> = list("plugin_errors")
            .iter()
            .map(|e| format!("{}: {}", s(&e["plugin"]), s(&e["message"])))
            .collect();
        let mcp = (!servers.is_empty())
            .then(|| format!("MCP servers not connected: {}", servers.join(", ")));
        let plugin =
            (!plugins.is_empty()).then(|| format!("Plugin errors: {}", plugins.join("; ")));
        let mut out = Vec::new();
        if mcp != self.init_problems.0 {
            out.extend(mcp.clone().map(|t| self.emit_system(MCP_NOT_CONNECTED, t)));
            self.init_problems.0 = mcp;
        }
        if plugin != self.init_problems.1 {
            out.extend(plugin.clone().map(|t| self.emit_system(PLUGIN_ERRORS, t)));
            self.init_problems.1 = plugin;
        }
        out
    }

    /// [`Core::live`], [`Core::live_commands`], [`Core::wakeup_row`], then [`Core::cron_jobs`],
    /// as sent: the run time of each, and the time to the fire of a wakeup and a job, counted
    /// now; a subagent, a command and a Monitor are `stoppable` ([`Core::stop_task`]), a wakeup
    /// and a job, no tasks of the CLI, are not.
    fn live_list(&self) -> Action {
        let now = epoch_ms();
        let due_in =
            |due: Option<i64>| due.map(|d| u64::try_from(d.saturating_sub(now)).unwrap_or(0));
        Action::Emit(AdapterEvent::LiveSubagents(
            self.live
                .iter()
                .chain(&self.live_commands)
                .map(|a| (a, None))
                .chain(self.wakeup_row.iter().map(|a| (a, due_in(self.wakeup))))
                .chain(self.cron_jobs.iter().map(|j| (&j.row, due_in(j.due))))
                .map(|(a, due_in_ms)| LiveSubagent {
                    task_id: a.task_id.clone(),
                    description: a.title.clone(),
                    activity: a.activity.clone(),
                    total_tokens: a.total_tokens,
                    tool_uses: a.tool_uses,
                    duration_ms: u64::try_from(a.since.elapsed().as_millis()).unwrap_or(u64::MAX),
                    call_id: a.call_id.clone(),
                    kind: Some(a.kind),
                    due_in_ms,
                    stoppable: matches!(
                        a.kind,
                        LiveKind::Agent | LiveKind::Command | LiveKind::Monitor
                    ),
                    retry: a.retry.as_ref().map(|(_, r)| r.clone()),
                })
                .collect(),
        ))
    }

    /// The wakeup now armed, `armed`: its due time in epoch ms and its row; none for none.
    /// [`AdapterEvent::Wakeup`] when the time changed, then the list again.
    fn set_wakeup(&mut self, armed: Option<(i64, LiveAgent)>) -> Vec<Action> {
        if self.wakeup.is_none() && armed.is_none() {
            return vec![];
        }
        let at = armed.as_ref().map(|a| a.0);
        tracing::info!(session_id = %self.session_id, scheduled_for = ?at, "wakeup");
        let mut out = Vec::new();
        if self.wakeup != at {
            out.push(Action::Emit(AdapterEvent::Wakeup(
                at.and_then(chrono::DateTime::from_timestamp_millis),
            )));
        }
        (self.wakeup, self.wakeup_row) = match armed {
            Some((at, row)) => (Some(at), Some(row)),
            None => (None, None),
        };
        out.push(self.live_list());
        out
    }

    /// The result of the main agent's `CronCreate` call `call`, `result` its `tool_use_result`:
    /// a job, with `nextFireAt` for its first due time (none: no time to show). The list again.
    fn cron_created(
        &mut self,
        call: &str,
        cron: &str,
        prompt: &str,
        result: &Value,
    ) -> Vec<Action> {
        let Some(id) = result["id"].as_str() else {
            return vec![];
        };
        let mut row = LiveAgent::new(LiveKind::Cron, id, prompt, Some(call));
        row.activity = result["humanSchedule"].as_str().map(text);
        self.cron_jobs.retain(|j| j.row.task_id != id);
        self.cron_jobs.push(CronJob {
            row,
            cron: cron.to_owned(),
            recurring: result["recurring"] == true,
            created: epoch_ms(),
            // 2.1.312 sends it with a fraction of a millisecond (live: 1794855129333.2852).
            due: result["nextFireAt"].as_f64().map(|t| t as i64),
        });
        vec![self.live_list()]
    }

    /// A cron job fired at `now` (epoch ms), by `command_lifecycle` `started` with
    /// `wakeup_source: schedule_wakeup`, which names no job: the one with the smallest due time,
    /// when it is due within [`CRON_FIRE_SLACK_MS`] (any other such line is the CLI's notice of
    /// tasks it missed). A one-shot job is gone, a recurring job older than
    /// [`CRON_MAX_AGE_MS`] too (the CLI deletes it after this fire), another waits for its next
    /// fire ([`next_cron_fire`]). The list again.
    pub(super) fn cron_fired(&mut self, now: i64) -> Vec<Action> {
        let Some((i, _)) = self
            .cron_jobs
            .iter()
            .enumerate()
            .filter_map(|(i, j)| Some((i, j.due.filter(|d| *d <= now + CRON_FIRE_SLACK_MS)?)))
            .min_by_key(|(_, due)| *due)
        else {
            return vec![];
        };
        let job = &mut self.cron_jobs[i];
        if !job.recurring || now - job.created >= CRON_MAX_AGE_MS {
            self.cron_jobs.remove(i);
        } else {
            job.due = next_cron_fire(&job.cron, now, &job.row.task_id);
        }
        vec![self.live_list()]
    }

    /// The result of the main agent's `CronDelete` call, `result` its `tool_use_result`: the
    /// job it names is gone. The list again when there was one.
    fn cron_deleted(&mut self, result: &Value) -> Vec<Action> {
        let before = self.cron_jobs.len();
        self.cron_jobs
            .retain(|j| result["id"] != j.row.task_id.as_str());
        if self.cron_jobs.len() == before {
            return vec![];
        }
        vec![self.live_list()]
    }

    /// `apply` to the task list; a `tasks` event with the whole list when it changed.
    fn tasks_changed(&mut self, apply: impl FnOnce(&mut Vec<TaskEntry>)) -> Vec<Action> {
        let shown = |list: &[TaskEntry]| list.iter().map(|e| e.task.clone()).collect::<Vec<_>>();
        let before = shown(&self.task_list);
        apply(&mut self.task_list);
        let items = shown(&self.task_list);
        if items == before {
            return vec![];
        }
        vec![self.emit_new(EventKind::Tasks { items })]
    }

    /// The task list the CLI has after resuming the session's transcript
    /// ([`resurrected_tasks`]); a fork starts with none. No event, the hub has the latest stored
    /// one.
    pub fn with_resumed_tasks(mut self, tasks: Vec<TaskEntry>) -> Self {
        self.task_list = tasks;
        self
    }

    fn on_user(&mut self, v: &Value) -> Vec<Action> {
        // The message that asks the model again after a response whose tool call the CLI could
        // not parse names that response's records, which the CLI dropped from the conversation
        // (2.1.325): they go at once, not only with the next answer's `supersedes`, which a turn
        // stopped first never brings.
        let mut out = self.retract(&v["retracted_message_uuids"]);
        let Some(uuid) = record_uuid(v) else {
            return out;
        };
        let blocks = blocks(v);
        // The record's `tool_use_result` is its `tool_result`'s only when it has one, as the VS
        // Code tab reads it (extension 2.1.319).
        let single = blocks.iter().filter(|b| b["type"] == "tool_result").count() == 1;
        for (i, block) in blocks.iter().enumerate() {
            if block["type"] != "tool_result" {
                continue;
            }
            out.extend(self.flush(false));
            let whole = text(
                &single
                    .then(|| agent_report(&v["tool_use_result"]))
                    .flatten()
                    .unwrap_or_else(|| result_text(&block["content"])),
            );
            let error = block["is_error"] == true;
            // The wakeup a `ScheduleWakeup` call armed, `scheduledFor: 0` for none (`stopped`);
            // a failed call changed nothing. Likewise a job a `CronCreate` call made and one a
            // `CronDelete` call removed.
            let call = block["tool_use_id"].as_str().unwrap_or("");
            let reason = self.wakeup_reasons.remove(call);
            let cron = self.cron_creates.remove(call);
            let result = &v["tool_use_result"];
            if self.wakeup_calls.remove(call) && !error {
                let at = result["scheduledFor"].as_i64().filter(|t| *t > 0);
                let armed = at.map(|at| {
                    let reason = reason.unwrap_or_default();
                    (
                        at,
                        LiveAgent::new(LiveKind::Wakeup, call, &reason, Some(call)),
                    )
                });
                out.extend(self.set_wakeup(armed));
            }
            if let Some((cron, prompt)) = cron.filter(|_| !error) {
                out.extend(self.cron_created(call, &cron, &prompt, result));
            }
            if self.cron_deletes.remove(call) && !error {
                out.extend(self.cron_deleted(result));
            }
            if let Some((tool, input)) = self.todo_calls.remove(call).filter(|_| !error) {
                out.extend(self.tasks_changed(|list| {
                    apply_task_result(list, &tool, &input, &whole, single, result)
                }));
            }
            let kind = EventKind::ToolResult {
                call_id: block["tool_use_id"].as_str().unwrap_or("").into(),
                result_preview: preview(&whole, PREVIEW_LIMIT),
                error,
                error_text: error.then(|| preview(&whole, ERROR_TEXT_LIMIT)),
                duration_ms: None,
                artifact_id: None,
                artifact_ids: vec![],
            };
            let files = ResultFiles {
                text: (whole.len() > PREVIEW_LIMIT).then(|| preview(&whole, FULL_RESULT_LIMIT)),
                images: result_images(&block["content"]),
            };
            let id = record_event_id(&uuid, i);
            out.push(if files == ResultFiles::default() {
                self.emit(id, kind)
            } else {
                let mut event = self.emit_event(kind);
                event.event_id = id;
                Action::Emit(AdapterEvent::ToolResult { event, files })
            });
        }
        out
    }

    fn on_result(&mut self, v: &Value) -> Vec<Action> {
        self.self_turn = false;
        self.stopping = false;
        // The turn of `/clear` ended without an `init` naming the new transcript: the folder is
        // not held for it any longer.
        self.clearing = false;
        // A local command's result (`/clear`'s own) ends no model turn.
        if v["local_command"].is_null() {
            self.fresh = false;
        }
        self.turn_records.clear();
        self.retracted.clear();
        // The goal events behind the final text go after the turn's `usage` and end line, which
        // the feed draws with the answer (SPEC 0.218).
        let mut out: Vec<Action> = self.flush_text(true).into_iter().collect();
        // A request the CLI no longer waits for: after "No" to one of two parallel calls it
        // ends the turn and drops the other. Without the cancel the hub keeps its card open
        // (Claude requests do not expire), and an answer to it finds no open request here.
        // That is a request of the main agent or of a foreground subagent (in `quiet_tasks`
        // with `false`), which cannot outlive the main turn. Any other subagent's request stays
        // open: a background subagent's outlives the main turn's `result`, and the CLI still
        // waits for its answer (checked live on 2.1.312). An MCP server's request names no
        // agent: it stays open until answered or withdrawn.
        let quiet = self.quiet_tasks.clone();
        let cancels = self.cancel_pending(|p| ends_with_main_turn_request(&quiet, p));
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
        let base = self.cost_base.unwrap_or_default();
        let spent = total.is_some_and(|t| t > base.total_cost_usd);
        let compacted = std::mem::take(&mut self.compacted);
        let model = self.model.clone().or_else(|| {
            v["modelUsage"]
                .as_object()
                .and_then(|m| m.keys().next().cloned())
        });
        // A result with no tokens (a local slash command, the empty second `result` after a turn
        // the CLI started by itself, one the CLI closes on a background subagent's
        // notification, a turn interrupted while its reply streamed) is not a turn's `usage`:
        // the feed would show an empty `0 → 0 tokens` line. `/compact` has zeros in `usage` but
        // costs, so it keeps its `usage`; its tokens are in the cumulative `modelUsage`, and are
        // the growth of it since the baseline of the cost.
        let has_tokens = input_tokens + output_tokens > 0;
        let compaction = !has_tokens && spent && compacted;
        if has_tokens || compaction {
            let model_tokens = model_usage_tokens(&v["modelUsage"]);
            let (input_tokens, output_tokens, cache_read, cache_write) = if compaction {
                let grown = model_tokens.since(base.tokens);
                (
                    grown.input,
                    grown.output,
                    Some(grown.cache_read),
                    Some(grown.cache_write),
                )
            } else {
                (
                    input_tokens,
                    output_tokens,
                    num("cache_read_input_tokens"),
                    num("cache_creation_input_tokens"),
                )
            };
            let cost_usd = total.map(|t| self.turn_cost(t, model_tokens));
            let context = self.context.as_ref();
            let entry = usage_entry(
                &v["modelUsage"],
                self.model.as_deref(),
                context.and_then(|c| c.model.as_deref()),
            );
            let context_limit = entry.and_then(context_limit);
            let cache_ttl_s = context.and_then(|c| c.cache_ttl_s(entry));
            out.push(self.emit_new(EventKind::Usage {
                input_tokens,
                output_tokens,
                cache_read,
                cache_write,
                cost_usd,
                session_cost_usd: total,
                model,
                context_tokens: context.map(|c| c.tokens),
                context_limit,
                cache_ttl_s,
            }));
        } else if spent {
            // The total grew with no tokens in the result: what background subagents spent
            // meanwhile, or a reply an interrupt cut off before its end. A `usage` with the
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
                context_tokens: None,
                context_limit: None,
                cache_ttl_s: None,
            }));
        }
        let subtype = v["subtype"].as_str().unwrap_or("");
        // At the end of the turn, after its `usage`, which the feed draws under the answer.
        if let Some((code, t)) = turn_end_line(v) {
            out.push(self.emit_system(code, t));
        }
        out.extend(self.release_goals());
        out.push(self.emit_new(EventKind::Status {
            state: SessionState::Idle,
            // Why the turn ended early is the line above.
            detail: None,
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
        // A taken one goes before the events of this `result` (the final message, usage,
        // `status idle`; SPEC §5.2): the hub opens a turn on an `ack`, and one after the
        // `status idle` would leave the session running after its turn.
        let (mut carried, mut dropped, mut taken) = (Vec::new(), Vec::new(), Vec::new());
        for mut w in std::mem::take(&mut self.awaiting_echo) {
            let carry = match w.state.as_deref() {
                // Withdrawn: the owner took it back, so it is dropped with no word to the
                // supervisor, which has it out of its books already.
                Some("cancelled") if w.withdrawing.is_some() => continue,
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
                taken.push(Action::Emit(AdapterEvent::InputAccepted {
                    event_id: w.event_id,
                }));
            }
        }
        out.splice(0..0, taken);
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
            Some("elicitation") => self.elicitation(request_id, req),
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
                    server: None,
                    message: None,
                    url: None,
                    subagent: self.subagent_of(req),
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

        // Only a person may answer (SPEC §8.1): the CLI says no stored grant or automatic
        // approval may, a user's ask rule forced the prompt, or no standing rule may be offered.
        let human_only = req["requires_user_interaction"] == true
            || !req["matched_ask_rule"].is_null()
            || req["suppress_always_allow_rule"] == true;
        if !human_only && self.rules.matches(&tool, &input) {
            tracing::info!(session_id = %self.session_id, %tool, "allowed by a session rule");
            return vec![Action::Write(allow(&request_id, &input, None))];
        }
        // hqc's rules of the folder (T48), read at every call, so a change reaches a running
        // session at once; every part of a compound command must be covered.
        if let Some((rules, root)) = self.folder.as_ref().filter(|_| !human_only) {
            if let Some(rule) = self.policy.folder_allows(rules, root, &tool, &input) {
                tracing::info!(session_id = %self.session_id, %tool, %rule, "allowed by a folder rule");
                return vec![Action::Write(allow(&request_id, &input, None))];
            }
        }
        let policy_hint = self.policy.hint(&tool, &input);
        let rule_suggestions = self
            .folder
            .as_ref()
            .filter(|_| !human_only)
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

        // What `session` and `always` grant, in the terms `answer_approval` acts on them.
        let mut options = vec![option("once", "Allow once", ApprovalCanonical::Once)];
        if !human_only {
            options.push(ApprovalOption {
                grant: Some(OptionGrant {
                    place: GrantPlace::Session,
                    rules: grant_rules(shown(&tool, &suggested)),
                    exact: true,
                }),
                ..option(
                    "session",
                    "Allow for this session",
                    ApprovalCanonical::Session,
                )
            });
        }
        if !human_only && !local_rules.is_empty() && !self.restricted {
            options.push(ApprovalOption {
                grant: Some(OptionGrant {
                    place: GrantPlace::LocalSettings,
                    rules: grant_rules(
                        local_rules
                            .iter()
                            .flat_map(|s| s["rules"].as_array().into_iter().flatten())
                            .filter_map(rule_text),
                    ),
                    exact: false,
                }),
                ..option("always", "Always allow", ApprovalCanonical::Always)
            });
        }
        if let Some(server) = hqc_proto::rules::mcp_server(&tool).filter(|_| !human_only) {
            options.push(option(
                OPTION_SERVER_SESSION,
                &format!("Allow all {server} tools for this session"),
                ApprovalCanonical::Session,
            ));
            if self.folder.is_some() && !self.restricted {
                options.push(option(
                    OPTION_SERVER_ALWAYS,
                    &format!("Always allow all {server} tools in this folder"),
                    ApprovalCanonical::Always,
                ));
            }
        }
        options.push(option("deny", "Deny", ApprovalCanonical::Deny));

        let description = describe(req, &input, &tool);
        let field = |k: &str, limit: usize| {
            req[k]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(|s| preview(s, limit))
        };
        let call_args = args_preview(&tool, &input);
        let reason = field("decision_reason", REASON_LIMIT).or_else(|| ask_rule_reason(req, &tool));
        tracing::info!(session_id = %self.session_id, %tool, human_only, "approval requested");
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
            args_preview: Some(call_args),
            tool,
            description,
            options,
            expires_at: None,
            policy_hint,
            rule_suggestions,
            secret: None,
            reason,
            display_name: field("display_name", DISPLAY_NAME_LIMIT),
            human_only,
            subagent: self.subagent_of(req),
        })]
    }

    /// `control_request` `elicitation`: an MCP server asks the person for input (SPEC §8.1) →
    /// `clarify.request` with `server` and `message`, the questions of its form or, with `mode:
    /// url`, its `url` and none. A form the card cannot ask, a link without a URL, or a
    /// request over [`ELICITATION_LIMIT`] is answered `cancel` at once, with a `system` line.
    fn elicitation(&mut self, request_id: String, req: &Value) -> Vec<Action> {
        let server = preview(
            req["mcp_server_name"].as_str().unwrap_or(""),
            SERVER_NAME_LIMIT,
        );
        let link = req["mode"] == "url";
        let url = req["url"]
            .as_str()
            .filter(|u| link && !u.is_empty())
            .map(text);
        let form = if link {
            url.is_some().then(|| (vec![], vec![]))
        } else {
            form_fields(req)
        };
        let Some((questions, fields)) = form else {
            tracing::warn!(session_id = %self.session_id, %server, "MCP server's request the card cannot ask; cancelled");
            return self.elicitation_cancelled(
                &request_id,
                format!("The MCP server {server} asked for input that hqc cannot show; its request was cancelled."),
            );
        };
        let count = questions.len();
        let kind = EventKind::ClarifyRequest {
            request_id: request_id.clone(),
            questions,
            server: Some(server.clone()),
            message: req["message"]
                .as_str()
                .filter(|m| !m.trim().is_empty())
                .map(|m| {
                    // The person answers in their own name: a cut text says it was cut.
                    let shown = preview(m, PREVIEW_LIMIT);
                    if shown.len() < hqc_proto::replace_nul(m).len() {
                        format!("{shown}…")
                    } else {
                        shown
                    }
                }),
            url,
            subagent: None,
        };
        let size = serde_json::to_vec(&kind).map_or(0, |v| v.len());
        if size > ELICITATION_LIMIT {
            tracing::warn!(session_id = %self.session_id, %server, size, "MCP server's request too large; cancelled");
            return self.elicitation_cancelled(
                &request_id,
                format!("The MCP server {server} asked for input too large for hqc to show; its request was cancelled."),
            );
        }
        tracing::info!(session_id = %self.session_id, %server, questions = count, link, "MCP server's request");
        self.pending
            .insert(request_id, Pending::Elicitation { fields });
        vec![self.emit_new(kind)]
    }

    /// An MCP server's request answered `cancel` at once, with a `system
    /// elicitation_unsupported` line of `text` ([`Self::elicitation`]).
    fn elicitation_cancelled(&self, request_id: &str, text: String) -> Vec<Action> {
        vec![
            Action::Write(control_response(request_id, json!({"action": "cancel"}))),
            self.emit_system(ELICITATION_UNSUPPORTED, text),
        ]
    }

    /// `control_cancel_request {request_id}`: the CLI withdrew a request of its own (it stopped
    /// waiting, SPEC §8.1). An open one is forgotten, so a later answer writes nothing, and its
    /// card is closed `withdrawn`. While the turn is being ended on purpose ([`Core::stopping`])
    /// the CLI writes it for every request of the turn it aborts: one the turn's `result` would
    /// cancel is closed `stopped`, as that cancel would, so that a question can be answered by
    /// a message (SPEC §10.1).
    fn on_cancel_request(&mut self, v: &Value) -> Vec<Action> {
        let id = v["request_id"].as_str().unwrap_or("");
        let Some(p) = self.pending.remove(id) else {
            return vec![];
        };
        let reason = if self.stopping && ends_with_main_turn_request(&self.quiet_tasks, &p) {
            CancelReason::Stopped
        } else {
            CancelReason::Withdrawn
        };
        tracing::info!(session_id = %self.session_id, request_id = id, ?reason, "the CLI withdrew its request");
        vec![self.cancel_event(id.to_owned(), &p, reason)]
    }

    /// The subagent request `req` comes from: its `agent_id` is the subagent's `task_id`
    /// (2.1.312: a `local_agent` task's id is its `agentId`), one of [`Core::live`] with the
    /// title of its `task_started`, cut to [`PREVIEW_LIMIT`]; `None` for the agent itself and
    /// a subagent not known so.
    fn subagent_of(&self, req: &Value) -> Option<RequestSubagent> {
        let id = req["agent_id"].as_str()?;
        let a = self
            .live
            .iter()
            .find(|a| a.task_id == id && !a.title.is_empty())?;
        Some(RequestSubagent {
            task_id: a.task_id.clone(),
            description: preview(&a.title, PREVIEW_LIMIT),
        })
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
    /// its `summary`, or "Background task" without one; for a subagent that delivered its
    /// report through `SubagentHandback` (2.1.312, `handback` `send` or `flagged`), whose
    /// `summary` is then a note for the model, the report: `handback_report.text`, its
    /// `warning` above it after a blank line. Not a flush: a held text keeps
    /// waiting for what decides whether it is final. First the task's open requests (a
    /// subagent's, by `agent_id`) are cancelled; a task in [`Core::quiet_tasks`] gets no line,
    /// nor one in [`Core::announced`] with the same text;
    /// a task this process did not see start (it began before a `--resume`) counts as a
    /// background one. The CLI's placeholder ([`not_reported_yet`]) while the latest
    /// `background_tasks_changed` still holds the subagent or a task of it is no end (2.1.312
    /// sends it when the subagent waits on its own command moved to the background): it waits
    /// in [`Core::held_ends`], with the line and the cancels, until the subagent stops counting.
    /// Any other is the end, also while a task the subagent left running is listed
    /// ([`Core::reported`]): 2.1.329 sends one with the subagent's last text in `summary`, no
    /// `handback`, when it stops with commands of its own still running, and may resume it
    /// when they end. A subagent that ends so with commands it started listed joins
    /// [`Core::left_running`].
    fn on_task_notification(&mut self, v: &Value) -> Vec<Action> {
        let id = v["task_id"].as_str();
        let waits = not_reported_yet(v);
        if let Some(id) = id.filter(|id| waits && self.listed(id)) {
            tracing::info!(session_id = %self.session_id, task_id = id, "subagent end held: it still counts");
            self.held_ends.insert(id.to_owned(), v.clone());
            return vec![];
        }
        if let Some(id) = id {
            self.held_ends.remove(id);
            if !waits {
                self.reported.insert(id.to_owned());
                if self.background.iter().any(|t| left_by(t, id)) {
                    let task = self
                        .live
                        .iter()
                        .find(|a| a.task_id == id)
                        .map(|a| a.title.clone())
                        .unwrap_or_default();
                    self.left_running
                        .insert(id.to_owned(), (Instant::now(), task));
                }
            }
        }
        self.on_task_ended(v)
    }

    /// The end of the task of `task_notification` `v` ([`Core::on_task_notification`]). The
    /// line carries the task's `tool_use_id` (the notification's, else that of its
    /// `task_started`, [`Core::task_calls`]) as `call_id`, and the notification's `status`,
    /// `task_id` and `usage`; its `output_file` follows it as [`AdapterEvent::TaskOutput`].
    /// One with `skip_transcript: true` gives no line.
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
        // A task the CLI keeps out of the transcript is no activity (the SDK): no line.
        if v["skip_transcript"] == true {
            return out;
        }
        fn field(v: &Value) -> Option<&str> {
            v.as_str().map(str::trim).filter(|s| !s.is_empty())
        }
        let report = &v["handback_report"];
        let t = match (field(&report["text"]), field(&report["warning"])) {
            (Some(r), Some(w)) => text(&format!("{w}\n\n{r}")),
            (Some(r), None) => text(r),
            (None, _) => field(&v["summary"]).map_or_else(|| "Background task".into(), text),
        };
        // A later end of the task: its line again only with a new text, never the placeholder.
        if let Some(id) = id {
            let mut h = std::hash::DefaultHasher::new();
            t.hash(&mut h);
            let hash = h.finish();
            match self.announced.get(id) {
                Some(shown) if *shown == hash || t.starts_with(NOT_REPORTED_YET) => return out,
                _ => {
                    self.announced.insert(id.to_owned(), hash);
                }
            }
        }
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
                from_session_id: None,
                usage: task_usage(&v["usage"]),
                fork: None,
                compaction: None,
                model: None,
                goal: None,
            },
        ))));
        if let Some(path) = v["output_file"].as_str().filter(|p| !p.is_empty()) {
            out.push(Action::Emit(AdapterEvent::TaskOutput(path.into())));
        }
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
            .map(|(request_id, p)| self.cancel_event(request_id, &p, CancelReason::Stopped))
            .collect()
    }

    /// `approval.cancel` or `clarify.cancel` of `request_id`, so the hub closes its card.
    fn cancel_event(&self, request_id: String, p: &Pending, reason: CancelReason) -> Action {
        let kind = match p {
            Pending::Approval { .. } => EventKind::ApprovalCancel { request_id, reason },
            Pending::Clarify { .. } | Pending::Elicitation { .. } => {
                EventKind::ClarifyCancel { request_id, reason }
            }
        };
        Action::Emit(AdapterEvent::Event(new_event(
            self.session_id,
            Author::System,
            kind,
        )))
    }

    // ---- helpers -----------------------------------------------------------------------

    /// The held text, then the goal events that came after it ([`Core::on_goal`]); only
    /// those when nothing is held.
    fn flush(&mut self, r#final: bool) -> Vec<Action> {
        let mut out: Vec<Action> = self.flush_text(r#final).into_iter().collect();
        out.extend(self.release_goals());
        out
    }

    /// The held text alone; the goal events behind it stay for [`Core::release_goals`].
    fn flush_text(&mut self, r#final: bool) -> Option<Action> {
        self.held_at = None;
        let (id, text, runtime, interrupted, refusal) = self.held.take()?;
        Some(self.emit(
            id,
            EventKind::AgentMessage {
                text,
                r#final,
                in_reply_to: self.reply_to(),
                runtime,
                interrupted,
                refusal,
            },
        ))
    }

    /// The held text is dropped, not sent (an input written into the turn's end, a retraction):
    /// the goal events behind it still go out, now.
    fn drop_held(&mut self) -> Vec<Action> {
        self.held = None;
        self.held_at = None;
        self.release_goals()
    }

    fn release_goals(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.goals_behind)
            .into_iter()
            .map(|e| Action::Emit(AdapterEvent::Event(e)))
            .collect()
    }

    /// A goal record the CLI wrote into the transcript, read by the meter (SPEC 0.218). The
    /// transcript is read on a clock of its own, stdout in order: a record may be read before
    /// the next element of stdout brings out the text it judged, which is held. One written
    /// at or after the moment that text was held (or without a readable time) follows it, so it
    /// waits behind it; one written before it is sent now, which is before the text. The held
    /// text is not flushed for it: the last one of a turn must reach `on_result` to be `final`.
    pub fn on_goal(&mut self, g: &GoalRecord) -> Vec<Action> {
        let event = goal_event(self.session_id, g);
        let written_after_held = self.held.is_some()
            && self.held_at.is_some_and(|at| {
                g.timestamp
                    .as_deref()
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    .is_none_or(|t| t >= at)
            });
        if written_after_held {
            self.goals_behind.push(event);
            return Vec::new();
        }
        vec![Action::Emit(AdapterEvent::Event(event))]
    }

    /// The held `context_compacted` ([`Core::compaction`]): its text is the summary's
    /// `message.content` (a string, or the text blocks of an array), "Context compacted"
    /// without one.
    fn release_compaction(&mut self, summary: Option<&Value>) -> Option<Action> {
        let (id, compaction) = self.compaction.take()?;
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
        Some(self.emit_system_as(Some(id), CONTEXT_COMPACTED, t, compaction))
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
        self.emit_system_as(None, code, text, None)
    }

    /// [`Self::emit_system`] with `event_id`, when given, and a compaction's fields.
    fn emit_system_as(
        &self,
        event_id: Option<Uuid>,
        code: &str,
        text: String,
        compaction: Option<ContextCompacted>,
    ) -> Action {
        let mut e = new_event(
            self.session_id,
            Author::System,
            EventKind::System {
                text,
                code: code.into(),
                call_id: None,
                status: None,
                task_id: None,
                from_session_id: None,
                usage: None,
                fork: None,
                compaction,
                model: None,
                goal: None,
            },
        );
        if let Some(id) = event_id {
            e.event_id = id;
        }
        Action::Emit(AdapterEvent::Event(e))
    }

    /// The CLI refused the model asked for (`asked`, when it says) and runs on `kept`
    /// (`system/informational` with `model_restriction`, 2.1.325): `system model_not_switched`
    /// with `kept` and [`AdapterEvent::ModelKept`], as for a refused `set_model`, so the
    /// session's model is the one that answers; the model of this turn's `usage` from now on.
    /// A line `at: switch` about a model other than the latest `set_model`'s is about an
    /// earlier pick, and its `kept` may be outdated ("a later switch can outdate it"): only
    /// logged. One of the latest `set_model`'s comes before that request's `restricted_by_org`
    /// error, which then gives no second line.
    fn model_restricted(
        &mut self,
        asked: Option<&str>,
        at: Option<&str>,
        kept: &str,
    ) -> Vec<Action> {
        if at == Some("switch") && self.model_requested.is_some() {
            if asked.is_some() && asked != self.model_requested.as_deref() {
                tracing::info!(session_id = %self.session_id, asked = ?asked, kept, "model restriction of an earlier pick");
                return vec![];
            }
            self.model_noticed.clone_from(&self.model_request);
        }
        let kept = text(kept);
        let t = match asked.map(text).filter(|a| !a.trim().is_empty()) {
            Some(a) => {
                format!("Model not switched: {a} is not available. The agent runs on {kept}")
            }
            None => format!("Model not switched. The agent runs on {kept}"),
        };
        self.model = Some(kept.clone());
        vec![
            self.emit_new(EventKind::System {
                text: t,
                code: MODEL_NOT_SWITCHED.into(),
                call_id: None,
                status: None,
                task_id: None,
                from_session_id: None,
                usage: None,
                fork: None,
                compaction: None,
                model: Some(kept.clone()),
                goal: None,
            }),
            Action::Emit(AdapterEvent::ModelKept(kept)),
        ]
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
/// The 5-hour and 7-day windows of a `rate_limit_event`'s `rate_limit_info.unifiedWindows`,
/// with its `status`, `rateLimitType` and `resetsAt`; `None` when it names neither window.
fn limits(info: &Value) -> Option<hqc_proto::frame::Limits> {
    let w = &info["unifiedWindows"];
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
        status: info["status"].as_str().map(text),
        rate_limit_type: info["rateLimitType"].as_str().map(text),
        resets_at: info["resetsAt"].as_i64(),
    };
    (l.five_hour.is_some() || l.seven_day.is_some()).then_some(l)
}

/// The `system` line a `result` ends its turn with (SPEC §8.1), by its `terminal_reason`
/// (2.1.312: 19 values, of which `completed`, `background_requested` and `tool_deferred` end a
/// turn on purpose; a local command has none): `aborted_streaming` or `aborted_tools` →
/// [`TURN_INTERRUPTED`]; any other but those three, or an error result (`subtype` other than `success`, `is_error: true`) → [`TURN_ENDED_EARLY`]
/// "Turn ended early: " with the reason (the subtype without one), `api_error_status` and
/// `errors`. Not the `result` text: with `is_error: true` it is the CLI's own reply, which
/// came as its `assistant` line already. A `result` whose answer the API stopped over a
/// possible prompt injection (`stop_reason` `prompt_injection_risk`, a value of the API the CLI
/// passes on as is), an error one too → [`TURN_ENDED_EARLY`] with that reason.
fn turn_end_line(v: &Value) -> Option<(&'static str, String)> {
    let reason = v["terminal_reason"].as_str();
    if reason.is_some_and(|r| r.starts_with("aborted_")) {
        return Some((TURN_INTERRUPTED, TURN_INTERRUPTED_TEXT.into()));
    }
    if v["stop_reason"] == "prompt_injection_risk" {
        return Some((TURN_ENDED_EARLY, PROMPT_INJECTION_TEXT.into()));
    }
    let subtype = v["subtype"].as_str().unwrap_or("");
    let error = subtype != "success" || v["is_error"] == true;
    // `background_requested` (the model moved its work to the background) and `tool_deferred`
    // end a turn on purpose, not because something failed.
    if !error
        && reason
            .is_none_or(|r| matches!(r, "completed" | "background_requested" | "tool_deferred"))
    {
        return None;
    }
    let mut parts = vec![reason.unwrap_or(subtype).to_owned()];
    if let Some(status) = v["api_error_status"].as_u64() {
        parts.push(format!("API status {status}"));
    }
    let errors: Vec<&str> = v["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .collect();
    if !errors.is_empty() {
        parts.push(errors.join("; "));
    }
    Some((
        TURN_ENDED_EARLY,
        preview(
            &format!("Turn ended early: {}", parts.join(" · ")),
            ERROR_TEXT_LIMIT,
        ),
    ))
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

/// The `commands` of the CLI's answer to `initialize` and of `system/commands_changed`
/// (2.1.312: `{name, description, argumentHint, aliases?, builtin?}`): the name without `/`,
/// the first line of the description and the argument hint, each at most
/// [`COMMAND_TEXT_LIMIT`] bytes, an empty one absent; `None` unless `v` is a list.
fn command_list(v: &Value) -> Option<Vec<SlashCommand>> {
    let line = |s: &Value| {
        s.as_str()
            .and_then(|s| s.lines().next())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| preview(s, COMMAND_TEXT_LIMIT))
    };
    Some(
        v.as_array()?
            .iter()
            .filter_map(|c| {
                Some(SlashCommand {
                    name: text(c["name"].as_str()?.trim().trim_start_matches('/')),
                    description: line(&c["description"]),
                    argument_hint: line(&c["argumentHint"]),
                })
            })
            .collect(),
    )
}

/// What the main agent waits on after stdout line `v` of type `ty` (`sub`: it has a
/// `parent_tool_use_id`), `None` when the line changes nothing: `system` `api_retry` (2.1.312:
/// `attempt`, `max_retries`, `retry_delay_ms`, `error_status`, `null` without a response) →
/// retrying; `system` `status` `compacting` → compacting; `system` `thinking_tokens` → thinking
/// with its `estimated_tokens`, the estimate of the thinking block being written. It ends with
/// any other `system` `status` that does not report the permission mode (`requesting` before a
/// request, `null` at a compaction's end), with a main agent's `stream_event` `message_start`
/// (a response begins: a retry went through), `content_block_start` or `content_block_stop` (a
/// thinking block ends, another block begins), with its `assistant` line and with `result`.
/// The deltas of a block and `ping` leave it: the CLI sends `thinking_tokens` before each
/// thinking delta.
fn activity_of(ty: &str, sub: bool, v: &Value) -> Option<Option<Activity>> {
    match (ty, v["subtype"].as_str()) {
        ("system", Some("api_retry")) => Some(Some(Activity::Retrying(api_retry(v)))),
        ("system", Some("status")) if v["permissionMode"].is_null() => {
            Some((v["status"] == "compacting").then_some(Activity::Compacting))
        }
        ("system", Some("thinking_tokens")) => Some(Some(Activity::Thinking {
            estimated_tokens: v["estimated_tokens"].as_u64().unwrap_or(0),
        })),
        ("stream_event", _)
            if !sub
                && matches!(
                    v["event"]["type"].as_str(),
                    Some("message_start" | "content_block_start" | "content_block_stop")
                ) =>
        {
            Some(None)
        }
        ("assistant", _) if !sub => Some(None),
        ("result", _) => Some(None),
        _ => None,
    }
}

/// `<original model> → <fallback model>` of a `model_*fallback` line, with its `content`, the
/// CLI's own words, after a colon.
fn switch_line(v: &Value) -> String {
    let s = |k: &str| text(v[k].as_str().unwrap_or(""));
    let line = format!("{} → {}", s("original_model"), s("fallback_model"));
    match v["content"].as_str().filter(|c| !c.trim().is_empty()) {
        Some(c) => format!("{line}: {}", text(c)),
        None => line,
    }
}

/// The text of `model_refusal_no_fallback`: its `content`; the CLI sends it empty, then the
/// model that refused and the API's explanation.
fn refusal_line(v: &Value) -> String {
    if let Some(c) = v["content"].as_str().filter(|c| !c.trim().is_empty()) {
        return text(c);
    }
    let line = format!(
        "{} refused to answer",
        text(v["original_model"].as_str().unwrap_or("The model"))
    );
    match v["api_refusal_explanation"].as_str() {
        Some(e) => format!("{line}: {}", text(e)),
        None => line,
    }
}

/// An `api_retry` line or a `subagent_retry` object.
fn api_retry(v: &Value) -> ApiRetry {
    let n = |k: &str| v[k].as_u64().unwrap_or(0);
    ApiRetry {
        attempt: u32::try_from(n("attempt")).unwrap_or(u32::MAX),
        max_retries: u32::try_from(n("max_retries")).unwrap_or(u32::MAX),
        retry_delay_ms: n("retry_delay_ms"),
        error_status: v["error_status"]
            .as_u64()
            .and_then(|s| u16::try_from(s).ok()),
    }
}

/// What the main agent's context holds, as its last API response says, and what that response
/// wrote to the prompt cache.
#[derive(Debug, Clone)]
struct ContextUse {
    /// Input, cache reads, cache writes and output of the response: what the next request
    /// starts with (the VS Code tab's `updateUsage` sums the same).
    tokens: u64,
    /// The output part of `tokens`: the lines of a response carry the count of its
    /// `message_start`, and its `message_delta` the final one.
    output: u64,
    /// `message.id` of the response; none for a size a compaction reported.
    message_id: Option<String>,
    /// `message.model` of the response.
    model: Option<String>,
    /// The response wrote tokens to the cache with the 1-hour or the 5-minute TTL.
    cache_1h: bool,
    cache_5m: bool,
    /// When the response started; none for a size a compaction reported, which is in no
    /// cache yet.
    started: Option<Instant>,
}

impl ContextUse {
    /// `message` of a main agent's `assistant` line that started at `started`; `None` without
    /// `usage`.
    fn of_response(message: &Value, started: Instant) -> Option<Self> {
        let usage = message["usage"].as_object()?;
        let num = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
        let written = |k: &str| usage.get("cache_creation").and_then(|c| c[k].as_u64()) > Some(0);
        Some(Self {
            tokens: num("input_tokens")
                + num("cache_read_input_tokens")
                + num("cache_creation_input_tokens")
                + num("output_tokens"),
            output: num("output_tokens"),
            message_id: message["id"].as_str().map(str::to_owned),
            model: message["model"].as_str().map(str::to_owned),
            cache_1h: written("ephemeral_1h_input_tokens"),
            cache_5m: written("ephemeral_5m_input_tokens"),
            started: Some(started),
        })
    }

    /// Seconds the session's cache has left, counted from the request of the response: the
    /// 1-hour TTL when the response or the session's `modelUsage` entry of its model shows a
    /// write with it, else the 5-minute one when either shows that (Claude Desktop reads 1 hour
    /// from `ephemeral_1h_input_tokens`, else 5 minutes); none when no write is seen, and none
    /// for a size a compaction reported.
    fn cache_ttl_s(&self, entry: Option<&Value>) -> Option<u64> {
        let started = self.started?;
        let seen = |k: &str| entry.is_some_and(|e| e[k].as_u64() > Some(0));
        let ttl = if self.cache_1h || seen("cacheCreationEphemeral1hInputTokens") {
            CACHE_TTL_1H_S
        } else if self.cache_5m || seen("cacheCreationEphemeral5mInputTokens") {
            CACHE_TTL_5M_S
        } else {
            return None;
        };
        Some(ttl.saturating_sub(started.elapsed().as_secs()))
    }

    /// Moves the response's start `by` into the past (tests).
    #[cfg(test)]
    fn rewind(&mut self, by: Duration) {
        self.started = self.started.and_then(|t| t.checked_sub(by));
    }
}

/// The `result.modelUsage` entry of the main agent's model. The CLI keys it by the model as
/// requested (`claude-sonnet-4-6[1m]`, a provider's id), not as the response names it: by the
/// model the main loop runs (`system/init`, a switch), else by `message.model`, else the entry
/// whose `canonicalModel` is that.
fn usage_entry<'a>(
    model_usage: &'a Value,
    main: Option<&str>,
    response: Option<&str>,
) -> Option<&'a Value> {
    let map = model_usage.as_object()?;
    main.and_then(|m| map.get(m))
        .or_else(|| response.and_then(|m| map.get(m)))
        .or_else(|| {
            response.and_then(|m| {
                map.values()
                    .find(|e| e["canonicalModel"].as_str() == Some(m))
            })
        })
}

/// The tokens the context may hold before the CLI compacts it by itself, from a model's
/// `result.modelUsage` entry: `contextWindow − min(maxOutputTokens, 20000) − 13000`, the
/// threshold of the model's window. An override of the auto-compaction window
/// (`CLAUDE_CODE_AUTO_COMPACT_WINDOW`, `autoCompactWindow`, `/autocompact`) and a disabled
/// auto-compaction are not seen in the stream; the VS Code tab computes it the same way.
fn context_limit(entry: &Value) -> Option<u64> {
    let window = entry["contextWindow"].as_u64()?;
    let reserve = entry["maxOutputTokens"]
        .as_u64()
        .unwrap_or(0)
        .min(AUTO_COMPACT_OUTPUT_RESERVE_MAX);
    Some(window.saturating_sub(reserve + AUTO_COMPACT_BUFFER))
}

/// `compact_metadata` of `compact_boundary` (2.1.312: `trigger` `auto` | `manual`,
/// `pre_tokens`, `post_tokens?`); `None` without `trigger` or `pre_tokens`.
fn compact_metadata(m: &Value) -> Option<ContextCompacted> {
    Some(ContextCompacted {
        trigger: text(m["trigger"].as_str()?),
        pre_tokens: m["pre_tokens"].as_u64()?,
        post_tokens: m["post_tokens"].as_u64(),
    })
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

/// An agent's request ends with the main turn when it is the main agent's or a foreground
/// subagent's (in `quiet_tasks` with `false`): a background subagent's outlives the turn.
fn ends_with_main_turn(quiet_tasks: &HashMap<String, bool>, agent_id: Option<&str>) -> bool {
    agent_id.is_none_or(|a| quiet_tasks.get(a) == Some(&false))
}

/// Whether the main turn's end, or its being ended, cancels `p`: an agent's request that
/// [`ends_with_main_turn`]. An MCP server's request names no agent and stays open until
/// answered or withdrawn.
fn ends_with_main_turn_request(quiet_tasks: &HashMap<String, bool>, p: &Pending) -> bool {
    !matches!(p, Pending::Elicitation { .. }) && ends_with_main_turn(quiet_tasks, p.agent_id())
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
        grant: None,
    }
}

/// A grant's rules: at most [`GRANT_RULES`], each cut to [`GRANT_RULE_LIMIT`] bytes; a rule
/// that was cut ends with "…" (within the limit), so the card does not show a stub as the rule
/// the CLI writes whole.
pub(super) fn grant_rules(rules: impl IntoIterator<Item = String>) -> Vec<String> {
    let rules: Vec<String> = rules.into_iter().collect();
    // Past the cap the last item says how many more the option grants: the card never shows
    // fewer rules than "Always" adds as if that were all of them.
    let keep = if rules.len() > GRANT_RULES {
        GRANT_RULES - 1
    } else {
        rules.len()
    };
    let more = rules.len() - keep;
    let mut out: Vec<String> = rules
        .into_iter()
        .take(keep)
        .map(|r| {
            let whole = hqc_proto::replace_nul(&r);
            if whole.len() <= GRANT_RULE_LIMIT {
                return whole.into_owned();
            }
            // The mark counts in the limit.
            let mut text = preview(&whole, GRANT_RULE_LIMIT - '…'.len_utf8());
            text.push('…');
            text
        })
        .collect();
    if more > 0 {
        out.push(format!("… {more} more"));
    }
    out
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

/// Why the CLI asks when a `permissions.ask` rule caught the call (SPEC §8.1, since 0.217).
/// Claude Code sends no `decision_reason` text for a rule, only `decision_reason_type: "rule"`
/// and the rule's `decision_reason_rule_source` (checked live on 2.1.329, also in
/// `bypassPermissions`); `matched_ask_rule`, when it comes, names the rule itself. `None` when
/// no rule caught the call.
fn ask_rule_reason(req: &Value, tool: &str) -> Option<String> {
    let matched = &req["matched_ask_rule"];
    if req["decision_reason_type"] != "rule" && !matched.is_object() {
        return None;
    }
    // The CLI's own names of the sources; the settings files are named as well, so the owner
    // knows where the rule lives.
    let place = match matched["source"]
        .as_str()
        .or_else(|| req["decision_reason_rule_source"].as_str())
    {
        Some("userSettings") => "user settings (~/.claude/settings.json)",
        Some("projectSettings") => "shared project settings (.claude/settings.json)",
        Some("localSettings") => "project local settings (.claude/settings.local.json)",
        Some("policySettings") => "enterprise managed settings",
        Some("flagSettings") => "command line arguments",
        Some("cliArg") => "a CLI argument",
        Some("command") => "command configuration",
        Some("session") => "the current session",
        Some("toolsNarrowing") => "CLI tool narrowing",
        Some("mcpServerPolicy") => "MCP server policy",
        Some("hostCredential") => "cloud-session credential guard",
        Some("mcpMount") => "Remote Control server mount",
        Some(other) => other,
        None => "Claude Code's settings",
    };
    let text = match matched["tool_name"].as_str() {
        Some(name) => match matched["rule_content"].as_str() {
            Some(content) => {
                format!("The ask rule {name}({content}) in {place} requires approval for this call")
            }
            None => format!("The ask rule {name} in {place} requires approval for this call"),
        },
        None => format!("An ask rule for {tool} in {place} requires approval for this call"),
    };
    Some(preview(&text, REASON_LIMIT))
}

pub(crate) fn parse_questions(input: &Value) -> Option<(Vec<ClarifyQuestion>, Vec<String>)> {
    let qs = input["questions"].as_array().filter(|q| !q.is_empty())?;
    let mut questions = Vec::new();
    let mut texts = Vec::new();
    for (i, q) in qs.iter().enumerate() {
        // The raw question goes back to the CLI as the key of its answer.
        let raw = q["question"].as_str()?;
        // Labels, descriptions and previews pair up: an option without a label is dropped
        // with its description and preview.
        let (options, (descriptions, previews)): (Vec<String>, (Vec<String>, Vec<String>)) = q
            ["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|o| {
                let label = o["label"].as_str()?;
                let description = text(o["description"].as_str().unwrap_or(""));
                let preview = option_preview(o["preview"].as_str().unwrap_or(""));
                Some((text(label), (description, preview)))
            })
            .unzip();
        let described = descriptions.iter().any(|d| !d.is_empty());
        let previewed = previews.iter().any(|p| !p.is_empty());
        questions.push(ClarifyQuestion {
            id: (i + 1).to_string(),
            text: text(raw),
            header: q["header"]
                .as_str()
                .filter(|h| !h.trim().is_empty())
                .map(text),
            options: (!options.is_empty()).then_some(options),
            option_descriptions: described.then_some(descriptions),
            option_previews: previewed.then_some(previews),
            multi: q["multiSelect"].as_bool(),
            required: false,
            input: None,
        });
        texts.push(raw.to_owned());
    }
    Some((questions, texts))
}

/// The questions of an MCP server's form (`elicitation` `requested_schema.properties`, in
/// `property_order`, then any it leaves out), with each one's type: an `enum` gives its values
/// as options, a boolean Yes and No, a string, number or integer free text (an integer's and a
/// number's with that `input`); a property the schema's `required` lists is `required`; `title`
/// is the question's header and `description` its text (the title, or the name, without one),
/// each cut to [`QUESTION_TEXT_LIMIT`]; names and values are kept whole. `None`:
/// a property of another type (an object, an array), which the card cannot ask.
fn form_fields(req: &Value) -> Option<(Vec<ClarifyQuestion>, Fields)> {
    let props = match &req["requested_schema"]["properties"] {
        Value::Null => return Some((vec![], vec![])),
        Value::Object(p) => p,
        _ => return None,
    };
    let required: Vec<&str> = req["requested_schema"]["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut names: Vec<&str> = req["property_order"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|n| props.contains_key(*n))
        .collect();
    for n in props.keys() {
        if !names.contains(&n.as_str()) {
            names.push(n);
        }
    }
    let mut questions = Vec::new();
    let mut fields = Vec::new();
    for (i, name) in names.into_iter().enumerate() {
        let p = &props[name];
        let (field, options) = match p["type"].as_str()? {
            "boolean" => (Field::Boolean, Some(vec!["Yes".into(), "No".into()])),
            "integer" => (Field::Integer, None),
            "number" => (Field::Number, None),
            "string" if p["enum"].is_null() => (Field::Text, None),
            "string" => {
                let values = p["enum"].as_array()?.iter().map(|v| v.as_str().map(text));
                (Field::Text, Some(values.collect::<Option<Vec<_>>>()?))
            }
            _ => return None,
        };
        let title = p["title"].as_str().filter(|s| !s.trim().is_empty());
        let description = p["description"].as_str().filter(|s| !s.trim().is_empty());
        questions.push(ClarifyQuestion {
            id: (i + 1).to_string(),
            text: preview(description.or(title).unwrap_or(name), QUESTION_TEXT_LIMIT),
            header: title
                .filter(|_| description.is_some())
                .map(|t| preview(t, QUESTION_TEXT_LIMIT)),
            options,
            option_descriptions: None,
            option_previews: None,
            multi: None,
            required: required.contains(&name),
            input: match field {
                Field::Integer => Some(AnswerInput::Integer),
                Field::Number => Some(AnswerInput::Number),
                Field::Text | Field::Boolean => None,
            },
        });
        fields.push((name.to_owned(), field));
    }
    Some((questions, fields))
}

/// `content` of an accepted MCP server's request ([`Core::answer_elicitation`]).
fn content(
    fields: &[(String, Field)],
    answers: &BTreeMap<String, ClarifyAnswer>,
) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for (i, (name, field)) in fields.iter().enumerate() {
        let s = match answers.get(&(i + 1).to_string()) {
            Some(ClarifyAnswer::One(s)) => s.clone(),
            Some(ClarifyAnswer::Many(v)) => v.join(", "),
            None => continue,
        };
        let t = s.trim();
        if t.is_empty() {
            continue;
        }
        let v = match field {
            Field::Boolean if t == "Yes" => Value::Bool(true),
            Field::Boolean if t == "No" => Value::Bool(false),
            Field::Integer => t.parse::<i64>().map_or(Value::String(s), Value::from),
            Field::Number => t
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map_or(Value::String(s), Value::Number),
            _ => Value::String(s),
        };
        out.insert(name.clone(), v);
    }
    out
}

/// [`text`] of an option's preview, cut to [`OPTION_PREVIEW_LIMIT`] with
/// [`OPTION_PREVIEW_TRUNCATED`] after it.
fn option_preview(s: &str) -> String {
    let full = text(s);
    if full.len() <= OPTION_PREVIEW_LIMIT {
        return full;
    }
    preview(&full, OPTION_PREVIEW_LIMIT) + OPTION_PREVIEW_TRUNCATED
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

/// The report of a subagent that completed, from the `tool_use_result` of its `Agent` (`Task`)
/// call, `{status: completed, agentId, content: [text blocks], …}`: the report without the
/// trailer with `agentId` and `<usage>` that the CLI adds to the `tool_result` for the model
/// (2.1.312; "render from it instead of parsing the tool_result text", SDK 0.3.312).
fn agent_report(r: &Value) -> Option<String> {
    if r["status"] != "completed" || !r["agentId"].is_string() {
        return None;
    }
    let parts = r["content"].as_array()?;
    Some(
        parts
            .iter()
            .filter(|p| p["type"] == "text")
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// The images of a `tool_result` block's `content`, decoded: `{type: image, source: {type:
/// base64, media_type, data}}` (a screenshot, a `Read` of a picture); one that does not decode
/// is left out.
fn result_images(content: &Value) -> Vec<ResultImage> {
    use base64::Engine;
    let Some(parts) = content.as_array() else {
        return vec![];
    };
    parts
        .iter()
        .filter(|p| p["type"] == "image" && p["source"]["type"] == "base64")
        .filter_map(|p| {
            let data = base64::engine::general_purpose::STANDARD
                .decode(p["source"]["data"].as_str()?)
                .ok()?;
            Some(ResultImage {
                media_type: p["source"]["media_type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                data,
            })
        })
        .collect()
}

fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// The length of a value in [`args_preview`] from which it counts as long: 3 KB.
const LONG_FIELD: usize = PREVIEW_LIMIT * 3 / 4;

/// `args_preview` of a `tool_use` with `input` (SPEC §4): the input as JSON, cut to
/// [`PREVIEW_LIMIT`]. An object's fields go from the shortest value to the longest: with the keys
/// sorted, a cut at 4 KB took the fields after a long value with it (a Write's `file_path` after
/// its `content`, a Bash call's `description` after its script), while now it falls in the longest
/// values and the others come whole. Values of `LONG_FIELD` or more count as equally long and
/// keep their sorted order: an Edit of two long strings still shows the start of its
/// `new_string`, not only a cut `old_string` that took the whole preview. An `Agent` (`Task` in
/// older Claude Code) input has its `prompt` last whatever its length: the client reads a cut one
/// by it. The adapter and the mirror both use it.
pub fn args_preview(tool: &str, input: &Value) -> String {
    let json = match input.as_object() {
        Some(o) => {
            let agent = tool == "Agent" || tool == "Task";
            let mut fields: Vec<(&String, String)> =
                o.iter().map(|(k, v)| (k, compact(v))).collect();
            // A stable sort: fields of the same length keep their sorted order.
            fields.sort_by_key(|(k, v)| (agent && k.as_str() == "prompt", v.len().min(LONG_FIELD)));
            let body: Vec<String> = fields
                .iter()
                .map(|(k, v)| format!("{}:{v}", serde_json::to_string(k).unwrap_or_default()))
                .collect();
            format!("{{{}}}", body.join(","))
        }
        None => compact(input),
    };
    preview(&json, PREVIEW_LIMIT)
}

/// `task_notification` `v` is the CLI's placeholder for a subagent that waits on its own
/// background work before it reports: no `handback`, a `summary` that starts with
/// [`NOT_REPORTED_YET`].
fn not_reported_yet(v: &Value) -> bool {
    v["handback"].is_null()
        && v["summary"]
            .as_str()
            .is_some_and(|s| s.starts_with(NOT_REPORTED_YET))
}

/// Listed task `t` is a command subagent `id` started (`local_bash`, a shell command or a
/// Monitor, whose `parent_task_id` is the subagent).
fn left_by(t: &Value, id: &str) -> bool {
    t["task_type"] == "local_bash" && t["parent_task_id"] == id
}

/// The `usage` of a `task_notification` (`total_tokens`, `tool_uses`, `duration_ms`); `None`
/// unless it has all three.
fn task_usage(u: &Value) -> Option<TaskUsage> {
    Some(TaskUsage {
        total_tokens: u["total_tokens"].as_u64()?,
        tool_uses: u["tool_uses"].as_u64()?,
        duration_ms: u["duration_ms"].as_u64()?,
    })
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

/// `system goal` for a goal record of the transcript (SPEC 0.218): its id follows the record's
/// `uuid`, so the hub drops one it has. The text is Claude Code's own wording of the state; the
/// condition and the reason are cut at [`PREVIEW_LIMIT`] bytes.
pub fn goal_event(session_id: Uuid, g: &GoalRecord) -> Event {
    let condition = preview(&g.condition, PREVIEW_LIMIT);
    let t = match g.state {
        GoalState::Set => format!("Goal set: {condition}"),
        GoalState::Unmet => "Goal not yet met… continuing".into(),
        GoalState::Met => "Goal achieved".into(),
        GoalState::Failed => "Goal could not be achieved".into(),
        GoalState::Cleared => format!("Goal cleared: {condition}"),
    };
    let mut e = new_event(
        session_id,
        Author::System,
        EventKind::System {
            text: t,
            code: GOAL.into(),
            call_id: None,
            status: None,
            task_id: None,
            from_session_id: None,
            usage: None,
            fork: None,
            compaction: None,
            model: None,
            goal: Some(GoalStatus {
                condition,
                state: g.state.as_str().into(),
                reason: g.reason.as_deref().map(|r| preview(r, PREVIEW_LIMIT)),
                checks: g.checks,
                duration_ms: g.duration_ms,
                tokens: g.tokens,
            }),
        },
    );
    e.event_id = Event::id_from_claude_record(&g.uuid);
    e
}

/// Now, in epoch ms.
fn epoch_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// A task call of the main agent, `tool` with `input`, whose result did not fail, applied to the
/// task list `list`: the result block's `text`, the record's `tool_use_result` (`toolUseResult`
/// in a transcript) `result`, which is this call's only when the record holds one result
/// (`single`). `TodoWrite` replaces the list, as the CLI keeps it (every task done: none);
/// `TaskCreate` adds the task under the id and subject its text names after the CLI corrected the
/// call (`Task #<id> created successfully: <subject>`; else the result's `task.id` when `single`
/// and the input's subject, `title` or `name`), with the `activeForm` or `active_form` of the
/// input (in a `task` wrapper too); `TaskUpdate` changes (its `status`, `subject`,
/// `activeForm` or `active_form`) or, with `deleted`, removes the task whose id its text names
/// (`Updated task #<id> …`; a task it did not find has the error or `Task #<id> not found`);
/// `TaskList` replaces the list, a known task keeping its `active_form`, only from a `single`
/// result with a `tasks` array. A task with a status other than `pending`, `in_progress` and
/// `completed` is left out. Texts are cut at [`TASK_TEXT_LIMIT`] bytes and the list at
/// [`TASKS_LIMIT`] tasks. The live process and the replay of a transcript
/// ([`resurrected_tasks`]) both go through here.
fn apply_task_result(
    list: &mut Vec<TaskEntry>,
    tool: &str,
    input: &Value,
    text: &str,
    single: bool,
    result: &Value,
) {
    let status = |v: &Value| match v.as_str()? {
        "pending" => Some(TaskStatus::Pending),
        "in_progress" => Some(TaskStatus::InProgress),
        "completed" => Some(TaskStatus::Completed),
        _ => None,
    };
    let owned = |v: &Value| v.as_str().map(|s| preview(s, TASK_TEXT_LIMIT));
    // The fields of a call's input as the CLI corrects it before it runs (2.1.312). A
    // `TaskCreate` input with neither `subject` nor `description` at the top takes the fields of
    // an object in `task` (a string there is its description, which the result text covers),
    // and `title` or `name` stand for `subject`, `active_form` for `activeForm`. A `TaskUpdate`
    // input takes only `active_form` for `activeForm` (and `id`, `task_id` for `taskId`).
    let unwrapped;
    let fields = match &input["task"] {
        Value::Object(inner)
            if tool == "TaskCreate"
                && input.get("subject").is_none()
                && input.get("description").is_none() =>
        {
            let mut all = input.clone();
            if let Some(o) = all.as_object_mut() {
                o.remove("task");
                o.extend(inner.clone());
            }
            unwrapped = all;
            &unwrapped
        }
        _ => input,
    };
    let first = |keys: &[&str]| keys.iter().find_map(|k| owned(&fields[*k]));
    let active_form = || first(&["activeForm", "active_form"]);
    // What follows `prefix` in the text, up to the first white space.
    let id_after = |prefix: &str, then: &str| {
        let rest = text.strip_prefix(prefix)?;
        let (id, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        (!id.is_empty() && tail.starts_with(then)).then(|| id.to_owned())
    };
    match tool {
        "TodoWrite" => {
            let todos: Vec<TaskEntry> = input["todos"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| {
                    Some(TaskEntry {
                        id: None,
                        task: AgentTask {
                            subject: owned(&t["content"])?,
                            status: status(&t["status"])?,
                            active_form: owned(&t["activeForm"]),
                        },
                    })
                })
                .collect();
            let done = todos.iter().all(|t| t.task.status == TaskStatus::Completed);
            *list = if done { vec![] } else { todos };
        }
        "TaskCreate" => {
            let named = id_after("Task #", "created successfully");
            let subject = text
                .split_once("created successfully:")
                .map(|(_, s)| preview(s.trim(), TASK_TEXT_LIMIT))
                .filter(|s| !s.is_empty())
                .or_else(|| first(&["subject", "title", "name"]));
            let (Some(id), Some(subject)) = (
                named.or_else(|| single.then(|| owned(&result["task"]["id"])).flatten()),
                subject,
            ) else {
                return;
            };
            list.retain(|e| e.id.as_deref() != Some(id.as_str()));
            list.push(TaskEntry {
                id: Some(id),
                task: AgentTask {
                    subject,
                    status: TaskStatus::Pending,
                    active_form: active_form(),
                },
            });
        }
        "TaskUpdate" => {
            let Some(id) = id_after("Updated task #", "") else {
                return;
            };
            if fields["status"] == "deleted" {
                list.retain(|e| e.id.as_deref() != Some(id.as_str()));
            } else if let Some(e) = list
                .iter_mut()
                .find(|e| e.id.as_deref() == Some(id.as_str()))
            {
                if let Some(s) = status(&fields["status"]) {
                    e.task.status = s;
                }
                if let Some(s) = owned(&fields["subject"]) {
                    e.task.subject = s;
                }
                if let Some(s) = active_form() {
                    e.task.active_form = Some(s);
                }
            }
        }
        _ => {
            // A result of another call (a `TaskGet`'s) in a record of several must not reset
            // the list; no `tasks` array changes nothing.
            let (true, Some(tasks)) = (single, result["tasks"].as_array()) else {
                return;
            };
            let listed: Vec<TaskEntry> = tasks
                .iter()
                .filter_map(|t| {
                    let id = owned(&t["id"])?;
                    let active_form = list
                        .iter()
                        .find(|k| k.id.as_deref() == Some(id.as_str()))
                        .and_then(|k| k.task.active_form.clone());
                    Some(TaskEntry {
                        id: Some(id),
                        task: AgentTask {
                            subject: owned(&t["subject"])?,
                            status: status(&t["status"])?,
                            active_form,
                        },
                    })
                })
                .collect();
            *list = listed;
        }
    }
    list.truncate(TASKS_LIMIT);
}

/// The task list the CLI has after resuming the session's `transcript` (a fork starts with none;
/// a copy's resume passes only what follows its cut, SPEC §8.1): the main
/// agent's (not a sidechain's) `TodoWrite`, `TaskCreate`, `TaskUpdate` and `TaskList` calls
/// whose results did not fail, applied in order by [`apply_task_result`]. Only the lines that can
/// matter are parsed, as for [`resurrected_cron_jobs`].
pub fn resurrected_tasks(transcript: &str) -> Vec<TaskEntry> {
    const TOOLS: [&str; 4] = ["TodoWrite", "TaskCreate", "TaskUpdate", "TaskList"];
    let mut calls: HashMap<String, (String, Value)> = HashMap::new();
    let mut list = Vec::new();
    for line in transcript.lines() {
        let is_call = TOOLS.iter().any(|t| line.contains(&format!("\"{t}\"")));
        let is_result =
            line.contains("\"tool_result\"") && calls.keys().any(|id| line.contains(id.as_str()));
        if !is_call && !is_result {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["isSidechain"] == true {
            continue;
        }
        match v["type"].as_str() {
            Some("assistant") => {
                for b in blocks(&v).iter().filter(|b| b["type"] == "tool_use") {
                    if let (Some(tool), Some(id)) = (
                        b["name"].as_str().filter(|n| TOOLS.contains(n)),
                        b["id"].as_str(),
                    ) {
                        calls.insert(id.to_owned(), (tool.to_owned(), b["input"].clone()));
                    }
                }
            }
            Some("user") => {
                let results = blocks(&v)
                    .iter()
                    .filter(|b| b["type"] == "tool_result")
                    .count();
                for b in blocks(&v).iter().filter(|b| b["type"] == "tool_result") {
                    let call = b["tool_use_id"].as_str().and_then(|id| calls.remove(id));
                    if let Some((tool, input)) = call.filter(|_| b["is_error"] != true) {
                        let text = result_text(&b["content"]);
                        let one = results == 1;
                        apply_task_result(
                            &mut list,
                            &tool,
                            &input,
                            &text,
                            one,
                            &v["toolUseResult"],
                        );
                    }
                }
            }
            _ => {}
        }
    }
    list
}

/// The cron jobs the CLI resurrects from the session's `transcript` when it resumes it or
/// forks a copy of it, at `now` (epoch ms), as it picks them (`os` over the calls, results and
/// deletions it collects from the transcript): every main-thread `CronCreate` call whose
/// result, a successful one, has a string `id`, that is not `durable` (those live in a file of
/// the project, which the worker does not read), is not named by a `CronDelete` call and has
/// a string `cron` and `prompt`. A recurring job (all but `recurring: false`) is left out when
/// it was created [`CRON_MAX_AGE_MS`] before `now` or earlier, else it fires next at
/// [`next_cron_fire`] from `now` (the CLI sets its last fire to the resume); a one-shot job is
/// kept while its time, the result's `nextFireAt`, is ahead of `now` (the CLI computes the same
/// time again from the call's timestamp, equal up to the call's delay). A job's `createdAt` is
/// its call's timestamp. The first call of an id counts.
///
/// A transcript is up to hundreds of MB, its tool results most of it, so only the lines that
/// can matter are parsed: those with a `CronCreate` or `CronDelete` call, and the results of
/// the calls found.
pub fn resurrected_cron_jobs(transcript: &str, now: i64) -> Vec<CronJob> {
    // The calls in order: id, input, timestamp.
    let mut calls: Vec<(String, Value, i64)> = Vec::new();
    let mut results: HashMap<String, Value> = HashMap::new();
    let mut deleted: HashSet<String> = HashSet::new();
    for line in transcript.lines() {
        let is_call = line.contains("\"CronCreate\"") || line.contains("\"CronDelete\"");
        let is_result = line.contains("\"toolUseResult\"")
            && calls.iter().any(|(id, ..)| line.contains(id.as_str()));
        if !is_call && !is_result {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["isSidechain"] == true {
            continue;
        }
        match v["type"].as_str() {
            Some("assistant") => {
                for b in blocks(&v).iter().filter(|b| b["type"] == "tool_use") {
                    match (b["name"].as_str(), b["id"].as_str()) {
                        (Some("CronCreate"), Some(id)) => {
                            let Some(at) = v["timestamp"]
                                .as_str()
                                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                                .map(|t| t.timestamp_millis())
                            else {
                                continue;
                            };
                            let input = if b["input"].is_object() {
                                b["input"].clone()
                            } else {
                                json!({})
                            };
                            calls.push((id.to_owned(), input, at));
                        }
                        (Some("CronDelete"), _) => {
                            if let Some(id) = b["input"]["id"].as_str() {
                                deleted.insert(id.to_owned());
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("user") if v["toolUseResult"].is_object() => {
                for b in blocks(&v) {
                    if b["type"] == "tool_result" && b["is_error"] != true {
                        if let Some(id) = b["tool_use_id"]
                            .as_str()
                            .filter(|id| calls.iter().any(|(call, ..)| call == id))
                        {
                            results.insert(id.to_owned(), v["toolUseResult"].clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut jobs: Vec<CronJob> = Vec::new();
    for (call, input, created) in calls {
        let Some(result) = results.get(&call) else {
            continue;
        };
        let Some(id) = result["id"].as_str() else {
            continue;
        };
        if result["durable"] == true
            || deleted.contains(id)
            || jobs.iter().any(|j| j.row.task_id == id)
        {
            continue;
        }
        let (Some(cron), Some(prompt)) = (input["cron"].as_str(), input["prompt"].as_str()) else {
            continue;
        };
        let recurring = result["recurring"] != false;
        let due = if recurring {
            if now - created >= CRON_MAX_AGE_MS {
                continue;
            }
            next_cron_fire(cron, now, id)
        } else {
            // 2.1.312 sends it with a fraction of a millisecond (live: 1794855129333.2852).
            match result["nextFireAt"].as_f64() {
                Some(t) if t as i64 >= now => Some(t as i64),
                _ => continue,
            }
        };
        let mut row = LiveAgent::new(
            LiveKind::Cron,
            id,
            &preview(prompt, PREVIEW_LIMIT),
            Some(&call),
        );
        row.activity = result["humanSchedule"].as_str().map(text);
        let age = Duration::from_millis(u64::try_from(now - created).unwrap_or(0));
        row.since = Instant::now().checked_sub(age).unwrap_or(row.since);
        jobs.push(CronJob {
            row,
            cron: cron.to_owned(),
            recurring,
            created,
            due,
        });
    }
    jobs
}

/// The first minute after `after` (epoch ms) that the 5-field `cron` matches, in epoch ms, in
/// the machine's local time zone as the CLI matches (`UWt`); none when it never matches.
fn next_cron_match(cron: &croner::Cron, after: i64) -> Option<i64> {
    let after = chrono::DateTime::from_timestamp_millis(after)?.with_timezone(&chrono::Local);
    cron.find_next_occurrence(&after, false)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// When the recurring cron job `id` with `cron` fires after its fire at `fire_time` (epoch ms),
/// as the CLI works it out (`Hmn` with its default settings): the next match, put off by a
/// share of the job's id (the first 8 hex digits as a fraction of 2^32) of half the interval to
/// the match after it, at most 30 minutes; a job of `*/N * * * *` whose interval is 5 minutes
/// to just under 5 minutes and 15 seconds fires 15 seconds short of an interval from
/// `fire_time` instead, to keep inside its prompt cache. None when the expression is not one
/// the CLI takes or never matches.
pub(super) fn next_cron_fire(cron: &str, fire_time: i64, id: &str) -> Option<i64> {
    let parsed = croner::parser::CronParser::builder()
        .seconds(croner::parser::Seconds::Disallowed)
        .year(croner::parser::Year::Disallowed)
        .build()
        .parse(cron.trim())
        .ok()?;
    let first = next_cron_match(&parsed, fire_time)?;
    let Some(second) = next_cron_match(&parsed, first) else {
        return Some(first);
    };
    let interval = second - first;
    let every_n_minutes = {
        let mut f = cron.split(' ');
        f.next().is_some_and(|m| {
            m.strip_prefix("*/")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        }) && f.eq(["*"; 4])
    };
    if every_n_minutes
        && CRON_CACHE_LEAD_MS < interval
        && interval >= CRON_CACHE_TTL_MS
        && interval - CRON_CACHE_LEAD_MS < CRON_CACHE_TTL_MS
    {
        return Some(fire_time + interval - CRON_CACHE_LEAD_MS);
    }
    let share = id
        .get(..8)
        .and_then(|h| u32::from_str_radix(h, 16).ok())
        .map_or(0.0, |n| f64::from(n) / 4_294_967_296.0);
    let offset = (share * 0.5 * interval as f64).min(CRON_OFFSET_CAP_MS);
    Some(first + offset.round() as i64)
}
