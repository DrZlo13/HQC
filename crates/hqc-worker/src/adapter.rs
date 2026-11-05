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
use hqc_proto::event::{Author, ClarifyAnswer, Event, EventKind};
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
    /// How many background tasks the runtime lists now, its subagents' included (Claude Code:
    /// the `tasks` of `system/background_tasks_changed`, SPEC §8.1). Not sent to the hub: the
    /// agent is not idle while it is above 0 (SPEC §17.3). Reported when it changes.
    BackgroundTasks(u32),
    /// The runtime reported its own session id (ACP); sent in `session.state`.
    RuntimeSessionId(String),
    /// The runtime named the session (Claude Code: `ai-title` in the transcript, read when
    /// a turn ends, SPEC §8.1); sent as `session.state.title`. Reported when it changes.
    Title(String),
    /// The runtime's slash commands, without `/` (Claude Code: `system/init`, SPEC §8.1);
    /// sent as `session.state.commands`. Reported when the list changes.
    Commands(Vec<String>),
    /// The permission mode the runtime runs in (T38, Claude Code: `permissionMode` of
    /// `system/init` and `system/status`); sent as `session.state.permission_mode`.
    /// Reported when it changes.
    PermissionMode(PermissionMode),
    /// The process ended by itself. Not required after `kill`.
    Exited { error: Option<String> },
}

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
    /// `message`: the user's text with a deny option (T26), the reason the runtime sees.
    async fn answer_approval(
        &mut self,
        request_id: &str,
        option_id: &str,
        message: Option<&str>,
    ) -> anyhow::Result<()>;
    /// Empty `answers`: the hub cancelled the request (SPEC §5.3).
    async fn answer_clarify(
        &mut self,
        request_id: &str,
        answers: &BTreeMap<String, ClarifyAnswer>,
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
