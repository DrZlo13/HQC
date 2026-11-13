//! Runtime adapter interface (SPEC §8): `start`, `input`, `stop`, `kill`, `resume`,
//! `answer_approval`, `answer_clarify` plus a stream of [`AdapterEvent`]s.
//!
//! One adapter instance per session, created by an [`AdapterFactory`] and driven by the
//! session task of the supervisor. The input queue is the supervisor's job, not the
//! adapter's (SPEC §17.2): `input` is only called when no turn is running, `inject_with`
//! only while one runs and [`Adapter::can_inject`] holds. `steer?` is not here yet:
//! iteration 1 has no `session.steer` frame.

use std::collections::BTreeMap;

use async_trait::async_trait;
use hqc_proto::event::{Author, ClarifyAction, ClarifyAnswer, Event, EventKind};
use hqc_proto::frame::{PermissionMode, Runtime, SessionInput};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::config::{Config, StartSpec};
use crate::inbox::InboxFile;

pub mod claude_code;
pub mod fake;

use claude_code::ClaudeCodeAdapters;

/// What an adapter reports to the supervisor.
// Almost every value is an `Event`; boxing it would only add an allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum AdapterEvent {
    /// An event for the hub. Everything except `agent.delta` goes through the outbox.
    Event(Event),
    /// A `tool.result` with files of the result to upload first (Claude Code, SPEC §8.1): the
    /// supervisor uploads them as artifacts of the session, names them in `artifact_id` and
    /// `artifact_ids`, then sends the event as [`AdapterEvent::Event`]; a failed upload leaves
    /// its field out.
    ToolResult { event: Event, files: ResultFiles },
    /// The runtime has taken the input `event_id` (Claude Code: the `--replay-user-messages`
    /// echo; ACP: `session/prompt` sent). Only now does the hub get `ack {event_id}`
    /// (SPEC §5.2).
    InputAccepted { event_id: Uuid },
    /// The turn started by the last `input` is over (final answer, error or `stop`); the
    /// next queued input may go. `carried`: inputs written during the turn that it did not
    /// take and that the runtime runs next by itself (Claude Code), in the order written.
    /// `dropped`: inputs written that the runtime will not take (Claude Code: `cancelled`).
    /// An input written into the turn and named in neither is still on its way.
    TurnEnded {
        carried: Vec<Uuid>,
        dropped: Vec<Uuid>,
    },
    /// How many subagents the runtime runs now (Claude Code: `system/task_started` with
    /// `task_type: local_agent` until the task ends, and while a background task the CLI
    /// lists belongs to it); sent as `session.state.subagents`. Reported when it changes.
    Subagents(u32),
    /// The subagents of [`AdapterEvent::Subagents`] one by one, in the order they started
    /// (Claude Code: the title of each from its `system/task_started`, what it does now from
    /// its latest `system/task_progress`), then the agent's background shell commands (SPEC
    /// §8.1); sent as `subagents.live`, transport only (SPEC §4). Reported when the list or a
    /// title changes and on every progress of one of them.
    LiveSubagents(Vec<hqc_proto::event::LiveSubagent>),
    /// How many background tasks the runtime lists now, its subagents' included (Claude Code:
    /// the `tasks` of `system/background_tasks_changed` without `ambient: true`, SPEC §8.1);
    /// sent as `session.state.background_tasks`. The agent is not idle while it is above 0
    /// (SPEC §17.3), and a move tells the agent how many it stopped (SPEC §5.3). Reported when
    /// it changes.
    BackgroundTasks(u32),
    /// The output tokens per second the agent and its subagents spent over the last 20 s
    /// (Claude Code: the main agent's response while it streams, made exact by its
    /// `message_delta`; a subagent's response when its transcript line is written), every 2 s
    /// while a turn or a subagent runs, 0 once neither does; sent as `tokens.rate`, transport
    /// only (SPEC §4). Reported when it changes.
    TokenRate(u32),
    /// The calls of the agent itself the runtime runs in the foreground and can move to the
    /// background ([`Adapter::background`]), by `call_id` (Claude Code: the Bash calls and
    /// subagents of `system/task_started` with `is_backgrounded: false`, SPEC §8.1); sent as
    /// `session.state.foreground_calls`. Reported when they change.
    ForegroundCalls(Vec<String>),
    /// What the agent waits on now that its output does not show, `None` once it does not
    /// (Claude Code: `system/api_retry`, `system/status`, `system/thinking_tokens`, SPEC
    /// §8.1); sent as `session.state.activity`. Reported when it changes.
    Activity(Option<hqc_proto::frame::Activity>),
    /// A long call of the agent is still running (Claude Code: `tool_progress` with
    /// `heartbeat: true`, every 30 s); `tokens.rate` goes again, which keeps the hub's turn
    /// watchdog off a silent call (SPEC §17.3).
    Heartbeat,
    /// The runtime refused a model switch and kept this model (Claude Code: the `error`
    /// answer to `set_model`, its `model`): the session's model from now on.
    ModelKept(String),
    /// The usage limits of the account the runtime runs on (Claude Code: `rate_limit_event`,
    /// SPEC §8.1); sent as `worker.limits`, the account being the worker's.
    Limits(hqc_proto::frame::Limits),
    /// The file a background task that ended wrote its output to (Claude Code: `output_file`
    /// of its `task_notification`, SPEC §8.1), right after its `system task_notification`: the
    /// worker uploads it as an `attachment` of the session.
    TaskOutput(std::path::PathBuf),
    /// The runtime reported its own session id (ACP), or the session goes on under another
    /// one (Claude Code: after `/clear`, the new transcript, SPEC §8.1); sent in
    /// `session.state`, and later starts continue it.
    RuntimeSessionId(String),
    /// The runtime named the session (Claude Code: a title a person gave, `/rename`, from
    /// `system/session_title_changed` at once; the transcript's title, the CLI's own included,
    /// read every 2 s of the first model turn of a new session (`Launch::New`) or after
    /// `/clear` while this process has sent no title, and at the end of each turn, SPEC §8.1);
    /// sent as `session.state.title`. Reported when it changes.
    Title(String),
    /// The runtime's slash commands, without `/`, with what it says of each (Claude Code: the
    /// answer to `initialize`, `system/commands_changed`, `system/init`, SPEC §8.1); sent as
    /// `session.state.commands` and `command_info`. Reported when the list changes.
    Commands(Vec<hqc_proto::frame::SlashCommand>),
    /// When the wakeup the agent armed for itself is due (Claude Code: the result of its
    /// `ScheduleWakeup` call), `None` once it fired or was stopped: until then idle unload
    /// waits for it (SPEC §17.3). Reported when it changes.
    Wakeup(Option<chrono::DateTime<chrono::Utc>>),
    /// The permission mode the runtime runs in (T38, Claude Code: `permissionMode` of
    /// `system/init` and `system/status`); sent as `session.state.permission_mode`.
    /// Reported when it changes.
    PermissionMode(PermissionMode),
    /// The process ended by itself. Not required after `kill`.
    Exited { error: Option<String> },
}

/// What a tool result carries beyond its `result_preview` ([`AdapterEvent::ToolResult`]).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResultFiles {
    /// The whole text of a result longer than its preview, at most its first
    /// [`FULL_RESULT_LIMIT`] bytes: a `text` artifact, `artifact_id`.
    pub text: Option<String>,
    /// The result's images in order: `image` artifacts, `artifact_ids`.
    pub images: Vec<ResultImage>,
}

/// An image block of a tool result.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultImage {
    pub media_type: String,
    pub data: Vec<u8>,
}

/// The most of a result's text uploaded: what "Open" of a text shows in the client (2 MiB).
pub const FULL_RESULT_LIMIT: usize = 2 * 1024 * 1024;

pub type EventSink = mpsc::Sender<AdapterEvent>;

/// A new event with a fresh `event_id` and the current time; `seq` is the hub's.
pub fn new_event(session_id: Uuid, author: Author, kind: EventKind) -> Event {
    Event {
        session_id,
        event_id: Uuid::now_v7(),
        seq: None,
        ts: chrono::Utc::now(),
        author,
        kind,
        chain: None,
        client_op_id: None,
        delivery: None,
        delivered_at: None,
        reply_delivered_at: None,
    }
}

/// Errors are reported to the hub: `start`/`resume` as `agent_start_failed` (retryable),
/// the rest as `internal`.
#[async_trait]
pub trait Adapter: Send {
    /// Spawn the runtime for a new session (`session.start` with `mode: new`, SPEC §5.3).
    async fn start(&mut self, spec: &StartSpec) -> anyhow::Result<()>;
    /// Spawn the runtime again for a session that ran before (`mode: resume`, or after
    /// `kill`, idle timeout or a crash in this process), continuing `spec.runtime_session_id`.
    async fn resume(&mut self, spec: &StartSpec) -> anyhow::Result<()>;
    /// Hand one input to the runtime. Returns once written; acceptance comes later as
    /// [`AdapterEvent::InputAccepted`].
    async fn input(&mut self, input: &SessionInput) -> anyhow::Result<()>;
    /// [`Adapter::input`] with the artifacts of the input already in `.hqc/inbox` (SPEC
    /// §12); `input.text` carries their paths. Runtimes that take images natively override
    /// this to attach `files` of kind image as well.
    async fn input_with(
        &mut self,
        input: &SessionInput,
        files: &[InboxFile],
    ) -> anyhow::Result<()> {
        let _ = files;
        self.input(input).await
    }
    /// Whether a person's input can go into the running turn now (SPEC §17.2; Claude Code: a
    /// turn runs and no permission request or question is open). By default never: input
    /// waits for the end of the turn.
    fn can_inject(&self) -> bool {
        false
    }
    /// Whether the runtime runs a turn it started by itself, with no input of ours (Claude
    /// Code: from `system/init` after a background task's end until its `result`, which ends
    /// it with [`AdapterEvent::TurnEnded`]); a move waits for it (SPEC §5.3). By default never.
    fn runs_own_turn(&self) -> bool {
        false
    }
    /// Writes input into the running turn, as [`Adapter::input_with`] does at its start; the
    /// runtime takes it at its next step, or runs it next by itself (see
    /// [`AdapterEvent::TurnEnded`]). `Ok(false)`: nothing written, [`Adapter::can_inject`] no
    /// longer holds (the turn ended meanwhile).
    async fn inject_with(
        &mut self,
        input: &SessionInput,
        files: &[InboxFile],
    ) -> anyhow::Result<bool> {
        let _ = (input, files);
        anyhow::bail!("this runtime cannot take input during a turn")
    }
    /// Abort the current turn; the process stays.
    async fn stop(&mut self) -> anyhow::Result<()>;
    /// Terminate the process. Returns when it is gone.
    async fn kill(&mut self) -> anyhow::Result<()>;
    /// The runtime's process while it runs: its MCP server must descend from it (SPEC §7.2).
    /// `None`: no process is known, and the session's MCP server is refused.
    fn pid(&self) -> Option<u32> {
        None
    }
    /// The secrets folder and ssh-agent of the process while it runs (SPEC 0.147); `None`:
    /// no process runs, or the session may not have secrets.
    fn secrets(&self) -> Option<std::sync::Arc<crate::secrets::ProcessSecrets>> {
        None
    }
    /// Switch the running process to `mode` (T38); with no process, nothing to do: the next
    /// start takes the mode from its [`StartSpec`]. The runtime confirms with
    /// [`AdapterEvent::PermissionMode`].
    async fn set_permission_mode(&mut self, mode: PermissionMode) -> anyhow::Result<()> {
        let _ = mode;
        anyhow::bail!("this runtime cannot switch its permission mode while it runs")
    }
    /// Switch the running process to `model`, mid-turn too; with no process, nothing to do:
    /// the next start takes the model from its [`StartSpec`].
    async fn set_model(&mut self, model: &str) -> anyhow::Result<()> {
        let _ = model;
        anyhow::bail!("this runtime cannot switch its model while it runs")
    }
    /// Move the running call `call_id`, one of [`AdapterEvent::ForegroundCalls`], to the
    /// background; the agent goes on. With no process, nothing to do. What the runtime answers
    /// is only logged.
    async fn background(&mut self, call_id: &str) -> anyhow::Result<()> {
        let _ = call_id;
        anyhow::bail!("this runtime cannot move a call to the background")
    }
    /// Stop task `task_id` alone, a `stoppable` one of [`AdapterEvent::LiveSubagents`]; the
    /// turn and the other tasks go on. With no process, nothing to do. What the runtime answers
    /// is only logged.
    async fn stop_task(&mut self, task_id: &str) -> anyhow::Result<()> {
        let _ = task_id;
        anyhow::bail!("this runtime cannot stop a task alone")
    }
    /// `message`: the user's text with a deny option (T26), the reason the runtime sees.
    async fn answer_approval(
        &mut self,
        request_id: &str,
        option_id: &str,
        message: Option<&str>,
    ) -> anyhow::Result<()>;
    /// Empty `answers` without `action`: the hub cancelled the request (SPEC §5.3).
    async fn answer_clarify(
        &mut self,
        request_id: &str,
        answers: &BTreeMap<String, ClarifyAnswer>,
        action: Option<ClarifyAction>,
    ) -> anyhow::Result<()>;
}

pub trait AdapterFactory: Send + Sync + 'static {
    fn supports(&self, runtime: Runtime) -> bool;
    /// A new, not yet started adapter for one session; it reports through `events`.
    fn create(&self, spec: &StartSpec, events: EventSink) -> Box<dyn Adapter>;
}

/// The runtimes of this build: Claude Code (T4).
pub struct BuiltinAdapters {
    claude: ClaudeCodeAdapters,
}

impl BuiltinAdapters {
    /// `ask_mode`: see [`crate::capabilities::claude_permission_mode`].
    pub fn from_config(cfg: &Config, ask_mode: String) -> Self {
        Self {
            claude: ClaudeCodeAdapters::from_config(cfg, ask_mode),
        }
    }

    /// Every Claude Code session gets the hqc MCP server (SPEC §7.2).
    pub fn with_mcp(mut self, mcp: claude_code::McpLaunch) -> Self {
        self.claude = self.claude.with_mcp(mcp);
        self
    }

    /// What the Claude Code processes wrote, for the mirror (SPEC §9.3).
    pub fn own_records(&self) -> std::sync::Arc<crate::mirror::OwnRecords> {
        self.claude.own_records()
    }
}

impl AdapterFactory for BuiltinAdapters {
    fn supports(&self, runtime: Runtime) -> bool {
        self.claude.supports(runtime)
    }

    fn create(&self, spec: &StartSpec, events: EventSink) -> Box<dyn Adapter> {
        self.claude.create(spec, events)
    }
}
