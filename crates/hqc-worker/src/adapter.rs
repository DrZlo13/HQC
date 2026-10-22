//! Runtime adapter interface (SPEC §8): `start`, `input`, `stop`, `kill`, `resume`,
//! `answer_approval`, `answer_clarify` plus a stream of [`AdapterEvent`]s.
//!
//! One adapter instance per session, created by an [`AdapterFactory`] and driven by the
//! session task of the supervisor. The input queue is the supervisor's job, not the
//! adapter's (SPEC §17.2): `input` is only called when no turn is running. `steer?` is not
//! here yet: iteration 1 has no `session.steer` frame.

use std::collections::BTreeMap;

use async_trait::async_trait;
use hqc_proto::event::{Author, ClarifyAnswer, Event, EventKind};
use hqc_proto::frame::{Runtime, SessionInput};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::config::{Config, StartSpec};

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
    /// next queued input may go.
    TurnEnded,
    /// The runtime reported its own session id (ACP); sent in `session.state`.
    RuntimeSessionId(String),
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
    /// Abort the current turn; the process stays.
    async fn stop(&mut self) -> anyhow::Result<()>;
    /// Terminate the process. Returns when it is gone.
    async fn kill(&mut self) -> anyhow::Result<()>;
    async fn answer_approval(&mut self, request_id: &str, option_id: &str) -> anyhow::Result<()>;
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
    /// `permission_mode`: see [`crate::capabilities::claude_permission_mode`].
    pub fn from_config(cfg: &Config, permission_mode: String) -> Self {
        Self {
            claude: ClaudeCodeAdapters::from_config(cfg, permission_mode),
        }
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
