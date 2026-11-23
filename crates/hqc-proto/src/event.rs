//! Event schema, SPEC §3 (Event) and §4.
//!
//! Optional fields (`?` in the spec) are omitted from the wire when absent; `null` is
//! accepted too. Events are tolerant (SPEC §4): unknown payload fields are ignored, an
//! unknown `type` becomes [`EventKind::Unknown`] and keeps its payload, an unknown value of
//! an enum inside a payload becomes that enum's `Unknown`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

/// Namespace for deterministic event ids derived from Claude Code jsonl record uuids
/// (SPEC §3 Event, §9.2). `Uuid::new_v5(&CLAUDE_RECORD_NS, record_uuid.as_bytes())`.
pub const CLAUDE_RECORD_NS: Uuid = Uuid::from_bytes([
    0x9e, 0x1b, 0x7a, 0x52, 0x3c, 0x4d, 0x4f, 0x6e, 0x8a, 0x1f, 0x2b, 0x3c, 0x4d, 0x5e, 0x6f, 0x70,
]);

/// `{kind: user|agent|system, id}`; a system author has no `id` (SPEC §3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Author {
    User {
        id: Uuid,
    },
    /// `id` is the session id of the agent (SPEC §3).
    Agent {
        id: Uuid,
    },
    System,
}

/// Chain bookkeeping for agent->agent traffic (SPEC §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chain {
    pub chain_id: Uuid,
    pub depth: u8,
    pub hops: u8,
    /// The highest permission mode a turn this message starts may run in (since SPEC 0.43,
    /// T39): the lower of the sender's mode and the cap of the input that started the
    /// sender's turn, i.e. the lowest mode up the chain (`ask` < `auto` < `allow_all`). Absent
    /// on chains made before 0.43: no cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_cap: Option<crate::frame::PermissionMode>,
    /// With a cap below `allow_all`: the address (`<project-slug>/<session-slug>`) of the
    /// first session up the chain in the cap's mode, which set it, for the header of the
    /// capped turn (for an `auto` cap since SPEC 0.129).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capped_by: Option<String>,
}

/// Delivery of `user.message` / `peer.message` to the runtime (SPEC §3 Event, §5.2).
/// The timestamp is the sibling field `Event::delivered_at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Pending,
    Delivered,
    Failed,
}

/// Session state as computed by the hub (SPEC §17.1), also used in `status` events.
/// Variants are listed in priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Archived,
    LockedVscode,
    WorkerOffline,
    Waiting,
    Error,
    Running,
    /// Paused by its user (SPEC §17.3): no turn runs and nothing is delivered until it is
    /// resumed.
    Paused,
    Queued,
    Idle,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalCanonical {
    Once,
    Session,
    Always,
    Deny,
    DenyAlways,
    #[serde(other)]
    Unknown,
}

/// Option ids of an MCP tool's approval besides `once`, `session`, `always` and `deny`: all
/// tools of the call's server, for the session (canonical `session`) or as a folder rule
/// (canonical `always`).
pub const OPTION_SERVER_SESSION: &str = "server_session";
pub const OPTION_SERVER_ALWAYS: &str = "server_always";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalOption {
    pub id: String,
    pub label: String,
    pub canonical: ApprovalCanonical,
    /// What choosing the option grants beyond this call (SPEC 0.207); absent for an option
    /// that grants nothing, and from a worker that does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<OptionGrant>,
}

/// `approval.option.grant`: where a standing allowance is kept and the rules it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionGrant {
    pub place: GrantPlace,
    /// The rules, as Claude Code writes a rule: `toolName` or `toolName(ruleContent)`; at most
    /// 10, each at most 256 bytes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    /// The exact call is allowed too (the worker's session rules always add it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exact: bool,
}

/// `OptionGrant.place`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantPlace {
    /// The worker's memory, until the agent's process ends.
    Session,
    /// `.claude/settings.local.json` at the root of the git repository containing the session's
    /// cwd (outside one, in the cwd), written by Claude Code (SPEC §8.1).
    LocalSettings,
    #[serde(other)]
    Unknown,
}

/// `approval.request.policy_hint`: a rule of the worker's `[permissions]` matches the call
/// (SPEC §8.1, §14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyHint {
    Allow,
    Deny,
    #[serde(other)]
    Unknown,
}

/// One simple part of a command and the hqc rule the worker suggests for it (T48); `rule`
/// absent: none (an unknown first word, `git -C` with a path outside the workspace).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSuggestion {
    pub part: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
}

/// Why an open approval or clarify request was closed without an answer (SPEC §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    Timeout,
    Stopped,
    WorkerRestart,
    Archived,
    /// The runtime withdrew its request (Claude Code: `control_cancel_request`, since SPEC
    /// 0.180).
    Withdrawn,
    #[serde(other)]
    Unknown,
}

/// `clarify.response.action` (since SPEC 0.180): what the person did with an MCP server's
/// request (`clarify.request.server`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClarifyAction {
    /// The answers are the input, also none (the link's Done).
    Accept,
    Decline,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerMode {
    Send,
    Ask,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    File,
    Image,
    Diff,
    Html,
    Text,
    /// A zip of a page and its files, or of one file, that `hqc_publish` publishes by a
    /// public link (since SPEC 0.209).
    Site,
    #[serde(other)]
    Unknown,
}

/// One question of `clarify.request` (SPEC §4). The Claude Code adapter maps `id` to the
/// question text that `AskUserQuestion` expects in `answers` (SPEC §8.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClarifyQuestion {
    pub id: String,
    pub text: String,
    /// A short label of the question (`AskUserQuestion`'s `header`, since SPEC 0.67).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// What each option means, in the order and number of `options`, `""` for an option
    /// without one (since SPEC 0.67); absent when no option has a description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_descriptions: Option<Vec<String>>,
    /// A mockup of each option shown as plain text (`AskUserQuestion`'s `preview`), by the
    /// rule of `option_descriptions` (since SPEC 0.199); absent when no option has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_previews: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi: Option<bool>,
    /// A field an MCP server's form requires (its schema's `required`, since SPEC 0.180).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    /// What the free-text answer of an MCP server's form field must read as (since SPEC 0.180);
    /// absent for text, a boolean, an enum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<AnswerInput>,
}

/// The kind of a free-text answer of an MCP server's form field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerInput {
    Integer,
    Number,
    #[serde(other)]
    Unknown,
}

/// The secret an `hqc_secret` card asks for (SPEC §4, since 0.147).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretAsk {
    pub name: String,
    pub kind: crate::frame::SecretKind,
    /// The Vaultwarden entry the agent proposes (only where its worker reads Vaultwarden,
    /// in its `worker.toml`); the owner confirms or changes it on the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault: Option<String>,
}

/// One subagent of `subagents.live` (SPEC §4, since 0.101), or (since 0.127) one background
/// shell command of the agent, with only `task_id`, `description`, `duration_ms`, `call_id`,
/// `kind` and `stoppable`; since 0.169 also a Monitor watcher (as a command), the wakeup the
/// agent armed (`task_id` and `call_id` of its call, `description` its reason, `due_in_ms`) or a
/// cron job of the agent (`task_id` its job id, `description` its prompt, `activity` its
/// schedule in words, `call_id` of its `CronCreate`, `due_in_ms`), neither `stoppable`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveSubagent {
    pub task_id: String,
    /// Its task (Claude Code: `description` of its `task_started`).
    pub description: String,
    /// What it does now (Claude Code: `description` of its latest `task_progress`); absent
    /// before any progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_uses: Option<u64>,
    /// How long it has run when the list was sent, by the worker's clock.
    pub duration_ms: u64,
    /// `call_id` of the `tool.call` that started it (Claude Code: `tool_use_id` of its
    /// `task_started`; since SPEC 0.135); absent before that comes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// A subagent or a background command (since SPEC 0.141), a Monitor watcher, a wakeup or a
    /// cron job (since SPEC 0.169); absent from an older worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<LiveKind>,
    /// The runtime stops it alone on `session.stop_task` (since SPEC 0.161): a subagent's row,
    /// a command's and (since SPEC 0.169) a Monitor's; absent — false, every row of an older
    /// worker.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stoppable: bool,
    /// Milliseconds until a wakeup or a cron job fires when the list was sent, by the worker's
    /// clock, 0 once due (since SPEC 0.169); only on those rows, and on a cron job only while
    /// its next fire is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_in_ms: Option<u64>,
    /// Its request to the model API failed and will be retried (since SPEC 0.179; Claude
    /// Code: the `subagent_retry` of its `tool_progress`, until the one that says the retry
    /// resolved); absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<ApiRetry>,
}

/// One schedule of `schedules.live` (SPEC §4, since 0.169): an enabled schedule of the session
/// with a next run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveSchedule {
    pub schedule_id: Uuid,
    pub message: String,
    /// Milliseconds until its next run when the list was sent, by the hub's clock, 0 when due.
    pub due_in_ms: u64,
    /// The pattern of a recurring schedule; absent for a one-off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// The time of a one-off; absent for a recurring one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<DateTime<Utc>>,
    pub timezone: String,
}

/// What a row of `subagents.live` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveKind {
    Agent,
    Command,
    /// A Monitor watcher of the agent (since SPEC 0.169).
    Monitor,
    /// The wakeup the agent armed with `ScheduleWakeup` (since SPEC 0.169).
    Wakeup,
    /// A job of the agent's `CronCreate` (since SPEC 0.169).
    Cron,
    #[serde(other)]
    Unknown,
}

/// A request to the model API that failed and will be retried (SPEC §4, §5.4; Claude Code:
/// `system/api_retry`, a subagent's `subagent_retry`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiRetry {
    pub attempt: u32,
    pub max_retries: u32,
    /// How long the runtime waits before the next attempt.
    pub retry_delay_ms: u64,
    /// The HTTP status of the failed attempt; absent when it got no response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_status: Option<u16>,
}

/// The subagent that asks in `approval.request` and `clarify.request` (SPEC §4, since 0.181):
/// its `task_id` as `subagents.live` names it, and its task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestSubagent {
    pub task_id: String,
    pub description: String,
}

/// What a background task spent, in `system task_notification` (SPEC §4, since 0.181; Claude
/// Code: the `usage` of its `task_notification`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskUsage {
    pub total_tokens: u64,
    pub tool_uses: u64,
    pub duration_ms: u64,
}

/// An answer in `clarify.response.answers`: one string, or several for `multi` questions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ClarifyAnswer {
    One(String),
    Many(Vec<String>),
}

/// Event payloads, SPEC §4. The `type` discriminator uses the spec's dotted names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum EventKind {
    #[serde(rename = "user.message")]
    UserMessage {
        text: String,
        #[serde(default)]
        artifact_ids: Vec<Uuid>,
        /// The hub cron sent it for this `ScheduledMessage` (SPEC §16), on behalf of the
        /// owner.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        schedule_id: Option<Uuid>,
        /// Set by the hub: the agent's text or, since SPEC 0.208, its thinking was being
        /// written when the input came, so clients show it under that text (since SPEC 0.108);
        /// absent when false.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        during_text: bool,
        /// The runtime took it into the turn already running (SPEC §17.2): the hub sets it
        /// when the worker acks the input `joined`. Absent when false.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        joined: bool,
    },
    /// Transport only, never stored (SPEC §4, §5.2).
    #[serde(rename = "agent.delta")]
    AgentDelta {
        text: String,
        /// The runtime abandoned the attempt that wrote the text so far (since SPEC 0.161): the
        /// hub and clients drop that text, and the thinking being written with it; `text` is
        /// empty. Absent when false.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        reset: bool,
        /// A piece of the agent's thinking being written, not of its answer (since SPEC
        /// 0.208); absent when false.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        thinking: bool,
    },
    /// The subagents the agent runs now, the whole list in the order they started, then (since
    /// SPEC 0.127) its background shell commands, and (since SPEC 0.169) its Monitor watchers,
    /// its wakeup and its cron jobs; empty: none (since SPEC 0.101). Transport only, never
    /// stored (SPEC §4, §5.2).
    #[serde(rename = "subagents.live")]
    SubagentsLive { agents: Vec<LiveSubagent> },
    /// The output tokens per second the agent and its subagents spent over the last 20 s
    /// (text, thinking and tool input; reading context does not count; a subagent's response
    /// counts when it ends), sent when it changes and 0 once the agent stops. Transport only,
    /// never stored (SPEC §4, §5.2).
    #[serde(rename = "tokens.rate")]
    TokensRate { output_per_s: u32 },
    /// The session's enabled schedules (SPEC §16) with a next run, the next one first; empty:
    /// none (since SPEC 0.169). Written by the hub only, transport only, never stored (SPEC
    /// §4, §5.2).
    #[serde(rename = "schedules.live")]
    SchedulesLive { schedules: Vec<LiveSchedule> },
    #[serde(rename = "agent.message")]
    AgentMessage {
        text: String,
        r#final: bool,
        /// `event_id` of the input that started the turn.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        in_reply_to: Option<Uuid>,
        /// A reply the runtime wrote itself instead of the model (an API error, "Credit
        /// balance is too low"; since SPEC 0.67); absent when false.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        runtime: bool,
        /// An interrupt cut the reply off before the runtime finished it (Stop, "No" to a
        /// permission request; since SPEC 0.161); absent when false.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        interrupted: bool,
        /// The runtime's reply to a model refusal (`stop_reason: "refusal"`; since SPEC
        /// 0.168); absent when false.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        refusal: bool,
    },
    #[serde(rename = "agent.thinking")]
    AgentThinking { text: String },
    #[serde(rename = "tool.call")]
    ToolCall {
        call_id: String,
        tool: String,
        args_preview: String,
    },
    #[serde(rename = "tool.result")]
    ToolResult {
        call_id: String,
        result_preview: String,
        error: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        /// A diff, or the whole text of a result longer than `result_preview` (Claude Code,
        /// since SPEC 0.180: its first 2 MiB).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_id: Option<Uuid>,
        /// The result's images, in order (since SPEC 0.180); absent when none.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        artifact_ids: Vec<Uuid>,
    },
    #[serde(rename = "approval.request")]
    ApprovalRequest {
        request_id: String,
        tool: String,
        description: String,
        /// The call's arguments as `tool.call.args_preview` carries them: the card shows the
        /// whole command and every argument from it (since SPEC 0.207).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args_preview: Option<String>,
        options: Vec<ApprovalOption>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<DateTime<Utc>>,
        /// What the worker's `[permissions]` say about this call (SPEC §8.1, §14); absent
        /// when no rule matches.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        policy_hint: Option<PolicyHint>,
        /// hqc rules the worker suggests for "Allow in this folder" (since SPEC 0.52, T48):
        /// one per simple part of a Bash command, the tool name for another tool; absent when
        /// the session has no workspace or the worker cannot split the command.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rule_suggestions: Option<Vec<RuleSuggestion>>,
        /// What an `hqc_secret` asks for (since SPEC 0.147): only in the hub's own card (`tool:
        /// hqc_secret`); the hub drops it from a worker's event, so no card but its own asks
        /// the owner for a secret.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secret: Option<SecretAsk>,
        /// Why the runtime asks (Claude Code: `decision_reason`, since SPEC 0.180; since 0.217,
        /// for a call an ask rule caught, the adapter's own text naming the rule and its source).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The runtime's name of the tool (Claude Code: `display_name`, since SPEC 0.180).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_name: Option<String>,
        /// Only a person may answer it (since SPEC 0.180): no stored rule and no
        /// `auto_approve`; absent when false.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        human_only: bool,
        /// The subagent whose call it is (since SPEC 0.181); absent for the agent itself and
        /// for a subagent the worker does not know.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent: Option<RequestSubagent>,
    },
    #[serde(rename = "approval.response")]
    ApprovalResponse {
        request_id: String,
        option_id: String,
        /// A user id or `auto`.
        by: String,
        /// The user's text with a deny option: "Reply with text instead" (T26). It
        /// goes to the runtime with the denial; the client shows it as the user's message.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    #[serde(rename = "approval.cancel")]
    ApprovalCancel {
        request_id: String,
        reason: CancelReason,
    },
    #[serde(rename = "clarify.request")]
    ClarifyRequest {
        request_id: String,
        questions: Vec<ClarifyQuestion>,
        /// The MCP server that asks (Claude Code: `elicitation`, since SPEC 0.180); absent: the
        /// agent asks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        server: Option<String>,
        /// The server's text above the questions.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        /// The page the server sends the person to, with no questions.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        /// As in `approval.request` (since SPEC 0.181).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent: Option<RequestSubagent>,
    },
    /// `answers` is keyed by `ClarifyQuestion::id`.
    #[serde(rename = "clarify.response")]
    ClarifyResponse {
        request_id: String,
        answers: BTreeMap<String, ClarifyAnswer>,
        /// Only for an MCP server's request (since SPEC 0.180).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<ClarifyAction>,
    },
    #[serde(rename = "clarify.cancel")]
    ClarifyCancel {
        request_id: String,
        reason: CancelReason,
    },
    #[serde(rename = "attachment")]
    Attachment {
        artifact_id: Uuid,
        kind: ArtifactKind,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
    #[serde(rename = "peer.message")]
    PeerMessage {
        from_session_id: Uuid,
        /// The sender's address `<project-slug>/<session-slug>` when it sent (since 0.34).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        text: String,
        mode: PeerMode,
        /// Only for `mode: ask`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        /// Files the sender gave with `files` (since 0.46, T41): artifacts of the owner,
        /// downloaded into the target's inbox like a user's attachments (SPEC §12).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        artifact_ids: Vec<Uuid>,
        /// Set by the hub: the agent's text or, since SPEC 0.208, its thinking was being
        /// written when the message came, so clients show it under that text (since SPEC
        /// 0.108); absent when false.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        during_text: bool,
    },
    #[serde(rename = "usage")]
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_read: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_write: Option<u64>,
        /// Cost of this turn (SPEC §4); Claude Code: the growth of `total_cost_usd`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        /// The runtime's cumulative cost of the whole runtime session after this turn
        /// (Claude Code: `result.total_cost_usd`, carried over `--resume`), if it reports one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_cost_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    #[serde(rename = "status")]
    Status {
        state: SessionState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// On the `idle` that ends a turn: `event_id` of the input that started it (since SPEC
        /// 0.67); absent on other statuses and from older workers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        in_reply_to: Option<Uuid>,
    },
    #[serde(rename = "system")]
    System {
        text: String,
        code: String,
        /// Only with `task_notification`: `call_id` of the `tool.call` that started the task
        /// (since SPEC 0.135); absent when the runtime does not name it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        /// Only with `task_notification`: how the task ended, as the runtime says it
        /// (Claude Code: `completed`, `failed`, `stopped`; since SPEC 0.135).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        /// Only with `task_notification`: the runtime's id of the task (Claude Code: its
        /// `task_id`, a subagent's `agentId`; since SPEC 0.135).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        /// Only with `room_passed`, in a room: the agent's session that passed (since SPEC
        /// 0.172, §7.1.1).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_session_id: Option<Uuid>,
        /// Only with `task_notification`: what the task spent, when the runtime says it
        /// (since SPEC 0.181).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<TaskUsage>,
        /// Only with [`SESSION_FORKED`]: its fields sit next to `text` and `code`.
        #[serde(flatten, default, skip_serializing_if = "Option::is_none")]
        fork: Option<SessionForked>,
        /// Only with `context_compacted`, when the runtime said (since SPEC 0.179): its fields
        /// sit next to `text` and `code`.
        #[serde(flatten, default, skip_serializing_if = "Option::is_none")]
        compaction: Option<ContextCompacted>,
        /// Only with `model_not_switched` (since SPEC 0.179): the model the runtime kept, which
        /// the hub makes the session's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Only with [`GOAL`] (since SPEC 0.218): the goal and how it stands.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        goal: Option<GoalStatus>,
    },
    /// Events of the session the runtime took back (since SPEC 0.179; Claude Code: the
    /// answer of a model that refused, replaced by another model's): clients do not show
    /// them. Nothing is deleted.
    #[serde(rename = "retraction")]
    Retraction { event_ids: Vec<Uuid> },
    /// An event type this build does not know; stored and passed on as is, not rendered.
    /// Never matches a known type: a known type with a bad payload is an error.
    #[serde(untagged)]
    Unknown {
        #[serde(deserialize_with = "unknown_type")]
        r#type: String,
        #[serde(default)]
        payload: serde_json::Value,
    },
}

impl EventKind {
    /// `type` values of the known variants, in SPEC §4 order.
    pub const TYPES: &'static [&'static str] = &[
        "user.message",
        "agent.delta",
        "subagents.live",
        "tokens.rate",
        "schedules.live",
        "agent.message",
        "agent.thinking",
        "tool.call",
        "tool.result",
        "approval.request",
        "approval.response",
        "approval.cancel",
        "clarify.request",
        "clarify.response",
        "clarify.cancel",
        "attachment",
        "peer.message",
        "usage",
        "status",
        "system",
        "retraction",
    ];
}

/// Serde falls back to the untagged `Unknown` on any error of the tagged variants;
/// rejecting known names here keeps a malformed known event an error.
fn unknown_type<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let t = String::deserialize(d)?;
    if EventKind::TYPES.contains(&t.as_str()) {
        return Err(serde::de::Error::custom(format!(
            "invalid payload for event type `{t}`"
        )));
    }
    Ok(t)
}

/// A stored event (SPEC §3 Event). `seq` is assigned by the hub and absent on the wire
/// from worker to hub.
///
/// For the hub's logic only: the hub stores and serves the payload as received and never
/// writes it back from this type, since `Unknown` enum values do not round-trip (SPEC §5.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub session_id: Uuid,
    pub event_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    pub ts: DateTime<Utc>,
    pub author: Author,
    #[serde(flatten)]
    pub kind: EventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<Chain>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_op_id: Option<String>,
    /// Only on `user.message` and `peer.message`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<DateTime<Utc>>,
    /// Only on the final `agent.message` that answered an `hqc_ask`, from the hub to clients:
    /// when the asking worker acknowledged the reply (SPEC §6, §7.1). Never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_delivered_at: Option<DateTime<Utc>>,
}

/// `system.code` of a fork in a Claude Code transcript (SPEC §4, §9.3). The payload carries,
/// besides `text` and `code`, the fields of [`SessionForked`].
pub const SESSION_FORKED: &str = "session_forked";

/// `system.code` of a model switch the runtime refused (SPEC §4, since 0.179). The payload
/// carries `model`, the model the runtime kept, which the hub makes the session's.
pub const MODEL_NOT_SWITCHED: &str = "model_not_switched";

/// `system.code` of a change in the session's goal (`/goal`, SPEC §4, since 0.218). The payload
/// carries [`GoalStatus`] as `goal`.
pub const GOAL: &str = "goal";

/// The goal of a session and how it stands (SPEC §4, since 0.218): the condition the runtime
/// keeps the agent working until it holds, and the runtime's own check of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalStatus {
    pub condition: String,
    /// `set`, `unmet` (a check that did not pass), `met`, `failed` (the runtime judged the
    /// condition impossible) or `cleared`.
    pub state: String,
    /// The runtime's reason of a check, when it gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// With `met` and `failed`: how many checks it took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
}

/// Extra payload of `system session_forked` (SPEC §4, §9.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionForked {
    /// The record that got a second line of turns.
    pub at_uuid: Uuid,
    /// Who started the later line: `vscode` (anyone outside hqc) or `hqc`.
    pub branch_from: String,
    /// Turns (prompts) of the other line, missing from the context of the new one.
    pub orphaned_turns: u32,
}

/// Extra payload of `system context_compacted` (SPEC §4, since 0.179; Claude Code: the
/// `compact_metadata` of `system/compact_boundary`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCompacted {
    /// `auto` | `manual` (`/compact`).
    pub trigger: String,
    /// The context's tokens before the compaction.
    pub pre_tokens: u64,
    /// And after it; absent when the runtime did not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_tokens: Option<u64>,
}

impl Event {
    /// Id of `system session_forked` at record `at_uuid`: one event per fork point, whoever
    /// reports it and however often (SPEC §9.3).
    pub fn id_of_fork(at_uuid: &Uuid) -> Uuid {
        Uuid::new_v5(&CLAUDE_RECORD_NS, format!("{at_uuid}/fork").as_bytes())
    }

    /// Deterministic event id for an event derived from a Claude Code jsonl record (SPEC §3).
    pub fn id_from_claude_record(record_uuid: &Uuid) -> Uuid {
        Uuid::new_v5(&CLAUDE_RECORD_NS, record_uuid.as_bytes())
    }

    /// Event id of content block `index` of a jsonl record (SPEC §3 Event, §9.2): block 0 is
    /// [`Event::id_from_claude_record`], block `i > 0` v5 of `"<uuid>/<i>"`. The adapter,
    /// the mirror and the import all use it, so one fact from two sources has one id.
    pub fn id_from_claude_block(record_uuid: &Uuid, index: usize) -> Uuid {
        if index == 0 {
            Self::id_from_claude_record(record_uuid)
        } else {
            Uuid::new_v5(
                &CLAUDE_RECORD_NS,
                format!("{record_uuid}/{index}").as_bytes(),
            )
        }
    }

    /// Event id of a further event `part` drawn from content block `index` of a jsonl record,
    /// beside the block's own ([`Event::id_from_claude_block`]): v5 of
    /// `"<uuid>/<index>/<part>"`. The mirror names the `clarify.request` and
    /// `clarify.response` of an answered `AskUserQuestion` so (SPEC §3 Event, §9.1).
    pub fn id_from_claude_part(record_uuid: &Uuid, index: usize, part: &str) -> Uuid {
        Uuid::new_v5(
            &CLAUDE_RECORD_NS,
            format!("{record_uuid}/{index}/{part}").as_bytes(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// Every `type` from the SPEC §4 table, written out by hand.
    const SPEC_EVENT_TYPES: &[&str] = &[
        "user.message",
        "agent.delta",
        "subagents.live",
        "tokens.rate",
        "schedules.live",
        "agent.message",
        "agent.thinking",
        "tool.call",
        "tool.result",
        "approval.request",
        "approval.response",
        "approval.cancel",
        "clarify.request",
        "clarify.response",
        "clarify.cancel",
        "attachment",
        "peer.message",
        "usage",
        "status",
        "system",
        "retraction",
    ];

    /// Wire examples shared with the TS client (`examples/wire/event.<type>.json`).
    const EXAMPLES: &[(&str, &str)] = &[
        (
            "user.message",
            include_str!("../examples/wire/event.user.message.json"),
        ),
        (
            "agent.delta",
            include_str!("../examples/wire/event.agent.delta.json"),
        ),
        (
            "subagents.live",
            include_str!("../examples/wire/event.subagents.live.json"),
        ),
        (
            "tokens.rate",
            include_str!("../examples/wire/event.tokens.rate.json"),
        ),
        (
            "schedules.live",
            include_str!("../examples/wire/event.schedules.live.json"),
        ),
        (
            "agent.message",
            include_str!("../examples/wire/event.agent.message.json"),
        ),
        (
            "agent.thinking",
            include_str!("../examples/wire/event.agent.thinking.json"),
        ),
        (
            "tool.call",
            include_str!("../examples/wire/event.tool.call.json"),
        ),
        (
            "tool.result",
            include_str!("../examples/wire/event.tool.result.json"),
        ),
        (
            "approval.request",
            include_str!("../examples/wire/event.approval.request.json"),
        ),
        (
            "approval.response",
            include_str!("../examples/wire/event.approval.response.json"),
        ),
        (
            "approval.cancel",
            include_str!("../examples/wire/event.approval.cancel.json"),
        ),
        (
            "clarify.request",
            include_str!("../examples/wire/event.clarify.request.json"),
        ),
        (
            "clarify.response",
            include_str!("../examples/wire/event.clarify.response.json"),
        ),
        (
            "clarify.cancel",
            include_str!("../examples/wire/event.clarify.cancel.json"),
        ),
        (
            "attachment",
            include_str!("../examples/wire/event.attachment.json"),
        ),
        (
            "peer.message",
            include_str!("../examples/wire/event.peer.message.json"),
        ),
        ("usage", include_str!("../examples/wire/event.usage.json")),
        ("status", include_str!("../examples/wire/event.status.json")),
        ("system", include_str!("../examples/wire/event.system.json")),
        (
            "retraction",
            include_str!("../examples/wire/event.retraction.json"),
        ),
    ];

    const UNKNOWN_EXAMPLE: &str = include_str!("../examples/wire/event.unknown.json");

    /// Exhaustive on purpose: a new variant does not compile until it gets a spec name here.
    fn spec_type(k: &EventKind) -> Option<&'static str> {
        Some(match k {
            EventKind::UserMessage { .. } => "user.message",
            EventKind::AgentDelta { .. } => "agent.delta",
            EventKind::SubagentsLive { .. } => "subagents.live",
            EventKind::TokensRate { .. } => "tokens.rate",
            EventKind::SchedulesLive { .. } => "schedules.live",
            EventKind::AgentMessage { .. } => "agent.message",
            EventKind::AgentThinking { .. } => "agent.thinking",
            EventKind::ToolCall { .. } => "tool.call",
            EventKind::ToolResult { .. } => "tool.result",
            EventKind::ApprovalRequest { .. } => "approval.request",
            EventKind::ApprovalResponse { .. } => "approval.response",
            EventKind::ApprovalCancel { .. } => "approval.cancel",
            EventKind::ClarifyRequest { .. } => "clarify.request",
            EventKind::ClarifyResponse { .. } => "clarify.response",
            EventKind::ClarifyCancel { .. } => "clarify.cancel",
            EventKind::Attachment { .. } => "attachment",
            EventKind::PeerMessage { .. } => "peer.message",
            EventKind::Usage { .. } => "usage",
            EventKind::Status { .. } => "status",
            EventKind::System { .. } => "system",
            EventKind::Retraction { .. } => "retraction",
            EventKind::Unknown { .. } => return None,
        })
    }

    /// T26: a denial with the user's text; without `message` the field is absent.
    #[test]
    fn approval_response_message_roundtrips() {
        let src = include_str!("../examples/wire/event.approval.response.message.json");
        let wire: Value = serde_json::from_str(src).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        let EventKind::ApprovalResponse { message, .. } = &e.kind else {
            panic!("not approval.response: {:?}", e.kind);
        };
        assert_eq!(
            message.as_deref(),
            Some("Leave the root alone, look only at ~/work/hqc")
        );
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
    }

    /// SPEC 0.180: a request only a person may answer, with the runtime's reason and tool
    /// name; an MCP server's form (a question `required`, one with `input`) and link; a declined
    /// one. Without the fields they are absent.
    #[test]
    fn request_variants_roundtrip() {
        for src in [
            include_str!("../examples/wire/event.approval.request.human_only.json"),
            include_str!("../examples/wire/event.clarify.request.elicitation.json"),
            include_str!("../examples/wire/event.clarify.request.elicitation_url.json"),
            include_str!("../examples/wire/event.clarify.response.decline.json"),
        ] {
            let wire: Value = serde_json::from_str(src).unwrap();
            let e: Event = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(serde_json::to_value(&e).unwrap(), wire);
        }
        let e: Event = serde_json::from_str(include_str!(
            "../examples/wire/event.approval.request.human_only.json"
        ))
        .unwrap();
        let EventKind::ApprovalRequest {
            reason,
            display_name,
            human_only,
            ..
        } = &e.kind
        else {
            panic!("{:?}", e.kind);
        };
        assert!(*human_only);
        assert_eq!(display_name.as_deref(), Some("Fetch"));
        assert!(reason.is_some());
        let e: Event = serde_json::from_str(include_str!(
            "../examples/wire/event.clarify.response.decline.json"
        ))
        .unwrap();
        assert!(matches!(
            e.kind,
            EventKind::ClarifyResponse {
                action: Some(ClarifyAction::Decline),
                ..
            }
        ));
        let plain: Event =
            serde_json::from_str(include_str!("../examples/wire/event.approval.request.json"))
                .unwrap();
        let p = &serde_json::to_value(&plain).unwrap()["payload"];
        assert!(p.get("human_only").is_none() && p.get("reason").is_none());
    }

    /// SPEC 0.180: a tool result's images; without them the field is absent.
    #[test]
    fn tool_result_images_roundtrip() {
        let src = include_str!("../examples/wire/event.tool.result.images.json");
        let wire: Value = serde_json::from_str(src).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        let EventKind::ToolResult { artifact_ids, .. } = &e.kind else {
            panic!("{:?}", e.kind);
        };
        assert_eq!(artifact_ids.len(), 1);
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
        let plain: Event =
            serde_json::from_str(include_str!("../examples/wire/event.tool.result.json")).unwrap();
        let p = &serde_json::to_value(&plain).unwrap()["payload"];
        assert!(p.get("artifact_ids").is_none());
    }

    /// SPEC 0.199: each option's preview, in the order and number of `options`, `""` for an
    /// option without one.
    #[test]
    fn clarify_request_previews_roundtrip() {
        let src = include_str!("../examples/wire/event.clarify.request.preview.json");
        let wire: Value = serde_json::from_str(src).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        let EventKind::ClarifyRequest { questions, .. } = &e.kind else {
            panic!("not clarify.request: {:?}", e.kind);
        };
        let previews = questions[0].option_previews.as_deref().unwrap();
        assert_eq!(previews.len(), questions[0].options.as_ref().unwrap().len());
        assert!(previews[0].starts_with("+--------+") && previews[2].is_empty());
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
    }

    #[test]
    fn known_types_match_spec() {
        assert_eq!(EventKind::TYPES, SPEC_EVENT_TYPES);
        let names: Vec<&str> = EXAMPLES.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, SPEC_EVENT_TYPES);
    }

    /// SPEC 0.147: the hub's `hqc_secret` card carries what it asks for.
    #[test]
    fn a_secret_card_roundtrips() {
        let src = include_str!("../examples/wire/event.approval.request.secret.json");
        let wire: serde_json::Value = serde_json::from_str(src).unwrap();
        let ev: Event = serde_json::from_value(wire.clone()).unwrap();
        let EventKind::ApprovalRequest { secret, tool, .. } = &ev.kind else {
            panic!("not approval.request");
        };
        assert_eq!(tool, "hqc_secret");
        assert_eq!(secret.as_ref().unwrap().name, "WIFI_PASS");
        assert_eq!(serde_json::to_value(&ev).unwrap(), wire);
    }

    /// SPEC 0.207: the call's arguments and what an option grants ride on the request.
    #[test]
    fn a_request_with_grants_roundtrips() {
        let src = include_str!("../examples/wire/event.approval.request.grants.json");
        let wire: serde_json::Value = serde_json::from_str(src).unwrap();
        let ev: Event = serde_json::from_value(wire.clone()).unwrap();
        let EventKind::ApprovalRequest {
            args_preview,
            options,
            ..
        } = &ev.kind
        else {
            panic!("not approval.request");
        };
        assert!(args_preview.as_deref().unwrap().contains("npm test"));
        assert_eq!(options[0].grant, None);
        let session = options[1].grant.as_ref().unwrap();
        assert_eq!(session.place, GrantPlace::Session);
        assert!(session.exact);
        let always = options[2].grant.as_ref().unwrap();
        assert_eq!(always.place, GrantPlace::LocalSettings);
        assert!(!always.exact);
        assert_eq!(serde_json::to_value(&ev).unwrap(), wire);
    }

    #[test]
    fn examples_roundtrip_exactly() {
        for (name, src) in EXAMPLES {
            let wire: Value = serde_json::from_str(src).unwrap();
            let e: Event =
                serde_json::from_value(wire.clone()).unwrap_or_else(|err| panic!("{name}: {err}"));
            assert_eq!(spec_type(&e.kind), Some(*name));
            let back = serde_json::to_value(&e).unwrap();
            assert_eq!(back, wire, "{name}: wire form changed");
            assert_eq!(back["type"], *name);
            assert_eq!(serde_json::from_value::<Event>(back).unwrap(), e);
        }
    }

    /// SPEC 0.181: the end of a background task with what it spent, and a request a subagent
    /// makes, naming it.
    #[test]
    fn task_usage_and_request_subagent_roundtrip() {
        let src = include_str!("../examples/wire/event.system.task_notification.json");
        let wire: Value = serde_json::from_str(src).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        let EventKind::System { usage, .. } = &e.kind else {
            panic!("not system: {:?}", e.kind);
        };
        assert_eq!(
            *usage,
            Some(TaskUsage {
                total_tokens: 155300,
                tool_uses: 188,
                duration_ms: 253000
            })
        );
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);

        let src = include_str!("../examples/wire/event.approval.request.subagent.json");
        let wire: Value = serde_json::from_str(src).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        let EventKind::ApprovalRequest { subagent, .. } = &e.kind else {
            panic!("not approval.request: {:?}", e.kind);
        };
        assert_eq!(
            subagent.as_ref().map(|s| s.task_id.as_str()),
            Some("a52af2b436ded4cea")
        );
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
    }

    /// SPEC 0.209: the attachment of a published page (`kind: site`) and the hub's card that
    /// asks the owner to allow `hqc_publish`.
    #[test]
    fn publication_events_roundtrip() {
        let src = include_str!("../examples/wire/event.attachment.site.json");
        let wire: Value = serde_json::from_str(src).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        assert!(matches!(
            e.kind,
            EventKind::Attachment {
                kind: ArtifactKind::Site,
                ..
            }
        ));
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
        let src = include_str!("../examples/wire/event.approval.request.publish.json");
        let wire: Value = serde_json::from_str(src).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        let EventKind::ApprovalRequest { tool, .. } = &e.kind else {
            panic!("not approval.request: {:?}", e.kind);
        };
        assert_eq!(tool, crate::mcp::PUBLISH);
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
    }

    #[test]
    fn a_scheduled_user_message_roundtrips() {
        let src = include_str!("../examples/wire/event.user.message.scheduled.json");
        let wire: Value = serde_json::from_str(src).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        assert!(matches!(
            e.kind,
            EventKind::UserMessage {
                schedule_id: Some(_),
                ..
            }
        ));
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
    }

    /// SPEC §17.2: a message the runtime took into the turn already running carries
    /// `joined: true`; any other leaves it out.
    #[test]
    fn a_joined_user_message_roundtrips() {
        let src = include_str!("../examples/wire/event.user.message.joined.json");
        let wire: Value = serde_json::from_str(src).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        assert!(matches!(
            e.kind,
            EventKind::UserMessage { joined: true, .. }
        ));
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
        let plain: Event =
            serde_json::from_str(include_str!("../examples/wire/event.user.message.json")).unwrap();
        assert!(matches!(
            plain.kind,
            EventKind::UserMessage { joined: false, .. }
        ));
        assert!(serde_json::to_value(&plain).unwrap()["payload"]
            .get("joined")
            .is_none());
    }

    /// SPEC 0.161: a reply an interrupt cut off carries `interrupted: true`; any other leaves
    /// it out.
    #[test]
    fn an_interrupted_agent_message_roundtrips() {
        let src = include_str!("../examples/wire/event.agent.message.interrupted.json");
        let wire: Value = serde_json::from_str(src).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
        let plain: Event =
            serde_json::from_str(include_str!("../examples/wire/event.agent.message.json"))
                .unwrap();
        assert!(serde_json::to_value(&plain).unwrap()["payload"]
            .get("interrupted")
            .is_none());
    }

    /// SPEC 0.161: a delta that drops the text so far carries `reset: true`; any other leaves it
    /// out.
    #[test]
    fn a_reset_agent_delta_roundtrips() {
        let src = include_str!("../examples/wire/event.agent.delta.reset.json");
        let wire: Value = serde_json::from_str(src).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
        let plain: Event =
            serde_json::from_str(include_str!("../examples/wire/event.agent.delta.json")).unwrap();
        assert!(serde_json::to_value(&plain).unwrap()["payload"]
            .get("reset")
            .is_none());
    }

    /// SPEC §4, §7.1.1: a room's `system room_passed` names the agent that passed by its
    /// session; any other line leaves `from_session_id` out.
    #[test]
    fn a_room_passed_line_roundtrips() {
        let src = include_str!("../examples/wire/event.system.room_passed.json");
        let wire: Value = serde_json::from_str(src).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        let EventKind::System {
            code,
            from_session_id,
            ..
        } = &e.kind
        else {
            panic!("{e:?}");
        };
        assert_eq!(
            (code.as_str(), *from_session_id),
            (
                "room_passed",
                Some(Uuid::parse_str("0199f3a2-7c10-7000-8000-000000000002").unwrap())
            )
        );
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
        let plain: Event =
            serde_json::from_str(include_str!("../examples/wire/event.system.json")).unwrap();
        assert!(serde_json::to_value(&plain).unwrap()["payload"]
            .get("from_session_id")
            .is_none());
    }

    /// SPEC 0.179: a subagent's row carries its API retry, `error_status` absent without a
    /// response; a row without one has none.
    #[test]
    fn a_subagent_retry_roundtrips() {
        let src = include_str!("../examples/wire/event.subagents.live.retry.json");
        let wire: Value = serde_json::from_str(src).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        let EventKind::SubagentsLive { agents } = &e.kind else {
            panic!("not subagents.live: {:?}", e.kind);
        };
        assert_eq!(
            agents[0].retry,
            Some(ApiRetry {
                attempt: 2,
                max_retries: 10,
                retry_delay_ms: 4000,
                error_status: None,
            })
        );
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
        let plain: Event =
            serde_json::from_str(include_str!("../examples/wire/event.subagents.live.json"))
                .unwrap();
        let EventKind::SubagentsLive { agents } = &plain.kind else {
            panic!("not subagents.live");
        };
        assert!(agents.iter().all(|a| a.retry.is_none()));
    }

    /// SPEC 0.179: a compaction carries its `trigger`, `pre_tokens` and `post_tokens` next to
    /// `text` and `code`; a `system` event without them has none.
    #[test]
    fn a_compaction_carries_its_tokens() {
        let src = include_str!("../examples/wire/event.system.context_compacted.json");
        let wire: Value = serde_json::from_str(src).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        let EventKind::System { compaction, .. } = &e.kind else {
            panic!("not system: {:?}", e.kind);
        };
        assert_eq!(
            compaction,
            &Some(ContextCompacted {
                trigger: "auto".into(),
                pre_tokens: 165000,
                post_tokens: Some(12400),
            })
        );
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
        let plain: Event =
            serde_json::from_str(include_str!("../examples/wire/event.system.json")).unwrap();
        assert!(matches!(
            plain.kind,
            EventKind::System {
                compaction: None,
                ..
            }
        ));
    }

    #[test]
    fn unknown_type_is_kept_as_is() {
        let wire: Value = serde_json::from_str(UNKNOWN_EXAMPLE).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        let EventKind::Unknown { r#type, payload } = &e.kind else {
            panic!("expected Unknown, got {:?}", e.kind);
        };
        assert!(!EventKind::TYPES.contains(&r#type.as_str()));
        assert_eq!(payload, &wire["payload"]);
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
    }

    #[test]
    fn malformed_known_event_is_an_error() {
        for (name, src) in EXAMPLES {
            let mut wire: Value = serde_json::from_str(src).unwrap();
            wire["payload"] = json!({"bogus": true});
            assert!(
                serde_json::from_value::<Event>(wire).is_err(),
                "{name}: bad payload must not become Unknown"
            );
        }
    }

    #[test]
    fn unknown_enum_values_are_tolerated() {
        let v = |s: &str| Value::String(s.into());
        assert_eq!(
            serde_json::from_value::<SessionState>(v("sleeping")).unwrap(),
            SessionState::Unknown
        );
        assert_eq!(
            serde_json::from_value::<ApprovalCanonical>(v("ask_later")).unwrap(),
            ApprovalCanonical::Unknown
        );
        assert_eq!(
            serde_json::from_value::<CancelReason>(v("superseded")).unwrap(),
            CancelReason::Unknown
        );
        assert_eq!(
            serde_json::from_value::<ClarifyAction>(v("snooze")).unwrap(),
            ClarifyAction::Unknown
        );
        assert_eq!(
            serde_json::from_value::<PeerMode>(v("broadcast")).unwrap(),
            PeerMode::Unknown
        );
        assert_eq!(
            serde_json::from_value::<ArtifactKind>(v("audio")).unwrap(),
            ArtifactKind::Unknown
        );
        assert_eq!(
            serde_json::from_value::<LiveKind>(v("workflow")).unwrap(),
            LiveKind::Unknown
        );
        assert_eq!(
            serde_json::from_value::<AnswerInput>(v("date")).unwrap(),
            AnswerInput::Unknown
        );
    }

    #[test]
    fn enum_wire_values() {
        let states = [
            (SessionState::Archived, "archived"),
            (SessionState::LockedVscode, "locked_vscode"),
            (SessionState::WorkerOffline, "worker_offline"),
            (SessionState::Waiting, "waiting"),
            (SessionState::Error, "error"),
            (SessionState::Running, "running"),
            (SessionState::Paused, "paused"),
            (SessionState::Queued, "queued"),
            (SessionState::Idle, "idle"),
        ];
        for (s, n) in states {
            assert_eq!(serde_json::to_value(s).unwrap(), n);
        }
        let reasons = [
            (CancelReason::Timeout, "timeout"),
            (CancelReason::Stopped, "stopped"),
            (CancelReason::WorkerRestart, "worker_restart"),
            (CancelReason::Archived, "archived"),
            (CancelReason::Withdrawn, "withdrawn"),
        ];
        for (r, n) in reasons {
            assert_eq!(serde_json::to_value(r).unwrap(), n);
        }
        for (a, n) in [
            (ClarifyAction::Accept, "accept"),
            (ClarifyAction::Decline, "decline"),
        ] {
            assert_eq!(serde_json::to_value(a).unwrap(), n);
        }
        let canon = [
            (ApprovalCanonical::Once, "once"),
            (ApprovalCanonical::Session, "session"),
            (ApprovalCanonical::Always, "always"),
            (ApprovalCanonical::Deny, "deny"),
            (ApprovalCanonical::DenyAlways, "deny_always"),
        ];
        for (c, n) in canon {
            assert_eq!(serde_json::to_value(c).unwrap(), n);
        }
        for (d, n) in [
            (Delivery::Pending, "pending"),
            (Delivery::Delivered, "delivered"),
            (Delivery::Failed, "failed"),
        ] {
            assert_eq!(serde_json::to_value(d).unwrap(), n);
        }
        for (k, n) in [
            (LiveKind::Agent, "agent"),
            (LiveKind::Command, "command"),
            (LiveKind::Monitor, "monitor"),
            (LiveKind::Wakeup, "wakeup"),
            (LiveKind::Cron, "cron"),
        ] {
            assert_eq!(serde_json::to_value(k).unwrap(), n);
        }
    }

    #[test]
    fn author_wire_shape() {
        let id = Uuid::nil();
        assert_eq!(
            serde_json::to_value(Author::User { id }).unwrap(),
            json!({"kind": "user", "id": id})
        );
        assert_eq!(
            serde_json::to_value(Author::Agent { id }).unwrap(),
            json!({"kind": "agent", "id": id})
        );
        assert_eq!(
            serde_json::to_value(Author::System).unwrap(),
            json!({"kind": "system"})
        );
    }

    #[test]
    fn unknown_payload_fields_are_ignored() {
        let e: Event = serde_json::from_value(json!({
            "session_id": Uuid::nil(), "event_id": Uuid::nil(),
            "ts": "2026-10-22T10:00:00Z", "author": {"kind": "system"},
            "type": "system", "payload": {"text": "t", "code": "c", "extra": 1}
        }))
        .unwrap();
        assert_eq!(
            e.kind,
            EventKind::System {
                text: "t".into(),
                code: "c".into(),
                call_id: None,
                status: None,
                task_id: None,
                from_session_id: None,
                usage: None,
                fork: None,
                compaction: None,
                model: None,
                goal: None
            }
        );
    }

    /// SPEC §4 (0.179): `system model_not_switched` names the model the runtime kept; other
    /// lines name none.
    #[test]
    fn a_refused_model_switch_names_the_model() {
        let src = include_str!("../examples/wire/event.system.model_not_switched.json");
        let wire: Value = serde_json::from_str(src).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        let EventKind::System { code, model, .. } = &e.kind else {
            panic!("not system: {:?}", e.kind);
        };
        assert_eq!(code, MODEL_NOT_SWITCHED);
        assert_eq!(model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
        let plain: Event =
            serde_json::from_str(include_str!("../examples/wire/event.system.json")).unwrap();
        assert!(matches!(plain.kind, EventKind::System { model: None, .. }));
    }

    /// SPEC §4 (0.218): `system goal` carries the goal as a nested object, whose empty fields
    /// are left out; other lines carry none.
    #[test]
    fn a_goal_line_carries_the_goal() {
        let src = include_str!("../examples/wire/event.system.goal.json");
        let wire: Value = serde_json::from_str(src).unwrap();
        let e: Event = serde_json::from_value(wire.clone()).unwrap();
        let EventKind::System { code, goal, .. } = &e.kind else {
            panic!("not system: {:?}", e.kind);
        };
        assert_eq!(code, GOAL);
        assert_eq!(
            goal,
            &Some(GoalStatus {
                condition: "all tests in packages/api pass".into(),
                state: "met".into(),
                reason: Some(
                    "The last run of npm test in packages/api passed all 214 tests.".into()
                ),
                checks: Some(3),
                duration_ms: Some(54786),
                tokens: Some(918),
            })
        );
        assert_eq!(serde_json::to_value(&e).unwrap(), wire);
        let set = GoalStatus {
            condition: "c".into(),
            state: "set".into(),
            reason: None,
            checks: None,
            duration_ms: None,
            tokens: None,
        };
        assert_eq!(
            serde_json::to_value(&set).unwrap(),
            serde_json::json!({"condition": "c", "state": "set"})
        );
        let plain: Event =
            serde_json::from_str(include_str!("../examples/wire/event.system.json")).unwrap();
        assert!(matches!(plain.kind, EventKind::System { goal: None, .. }));
    }

    #[test]
    fn claude_record_id_is_stable() {
        let u = Uuid::parse_str("36c72c19-b4ad-4d95-a482-ce4e281952cb").unwrap();
        assert_eq!(
            Event::id_from_claude_record(&u),
            Event::id_from_claude_record(&u)
        );
        assert_ne!(Event::id_from_claude_record(&u), u);
    }

    /// The ids of the events drawn from one block are fixed (a resend dedupes by them) and
    /// differ from each other and from the block's own.
    #[test]
    fn claude_part_id_is_stable() {
        let u = Uuid::parse_str("36c72c19-b4ad-4d95-a482-ce4e281952cb").unwrap();
        let (req, resp) = (
            Event::id_from_claude_part(&u, 0, "clarify.request"),
            Event::id_from_claude_part(&u, 0, "clarify.response"),
        );
        assert_eq!(req.to_string(), "6035cfda-c03f-5f8e-8027-0f3d69125170");
        assert_eq!(resp.to_string(), "c2a3d750-3b5a-5be9-8a81-587f88e43e22");
        assert_ne!(req, Event::id_from_claude_block(&u, 0));
        assert_ne!(req, Event::id_from_claude_part(&u, 1, "clarify.request"));
    }
}
