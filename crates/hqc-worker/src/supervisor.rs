//! Session supervisor (SPEC §5.3, §17.2, §17.3).
//!
//! One task per session owns its adapter, its input queue and its `process_state`.
//! - `session.start` is checked against the local config first; a mismatch is answered
//!   with `error {invalid_frame}` and nothing is created or spawned (SPEC §2.1, §14).
//!   `mode: new` calls [`Adapter::start`], `mode: resume` [`Adapter::resume`] (SPEC §5.3).
//!   `session.start` for a live process is acked without touching it, unless it names another
//!   folder: a move, which waits until no turn (also none the runtime started by itself) and
//!   no subagent runs, nothing is written into a turn and no capped input waits, then stops
//!   the process and starts the next one there; meanwhile no input starts a turn in the old
//!   folder or is written into the running one, and a queued input moves the agent once
//!   nothing holds the move;
//!   before every start of a Claude Code agent its transcript is moved to where the working
//!   directory puts it, if it is elsewhere and no live process outside hqc holds it
//!   ([`crate::mirror::relocate`]).
//!   A failed spawn is `error {agent_start_failed, retryable: true}` (SPEC §5.2).
//! - A folder the worker has not run sessions in ([`KnownPaths`]), for a session that is
//!   neither the worker owner's (`owner_is_worker_owner`) nor in a workspace the owner
//!   added (`requested_by_owner`), needs the owner's consent first (SPEC §10.2), also when a
//!   running session moves there: the start
//!   waits for `worker.approval.request` -> `worker.approval.response`. `once` remembers
//!   the folder and goes on; `deny`, the hub's timeout or ours answers the start with
//!   `error {invalid_frame}`. Starts for the same folder wait for the same request, which is
//!   sent again on every connect while unanswered.
//! - `session.input` is queued; one input per turn, the next one waits for
//!   [`AdapterEvent::TurnEnded`] (SPEC §17.2). Its `ack {event_id}` is sent only when the
//!   adapter reports [`AdapterEvent::InputAccepted`] (SPEC §5.2). The queue lives in memory:
//!   the hub's `pending` inputs are the source of truth and come again after `hello`, so an
//!   input already queued or delivered is recognised by `event_id` and not run twice.
//! - The owner's Stop (`session.stop {quiet: true}`, SPEC 0.231) with a turn running (also one
//!   the runtime started by itself) marks what waits at that moment (the queue, a capped input
//!   waiting for its mode) and, when the turn ends, what the runtime dropped from it, to be
//!   written into the agent's history without a turn ([`Adapter::input_quiet_with`], in
//!   `event_id` order); a slash command among them fails with "cancelled by Stop". So does an
//!   input whose cap is below the session's own mode (T39): it was never handled under its cap
//!   and the owner's next turn would see it unhandled in the full mode, so the hub gets an error
//!   for it and the agent gets a quiet note of its own instead ([`peer::dropped_note`]): what
//!   was dropped (another session's message, a scheduled run), without its text. Until the
//!   turn ends nothing new goes into it; what comes after the Stop is never marked. A quiet
//!   input's acceptance is `ack {event_id, quiet: true}` ([`AdapterEvent::QuietTaken`]), also
//!   when it is resent; a note has no frame of the hub, so nothing is acked or remembered for
//!   it. A quiet line the runtime drops ([`AdapterEvent::QuietCancelled`]) is
//!   written again. A normal input waits for their acks (the CLI takes a "next" line before a
//!   "later" one), as do the idle unload and a move. A pause's stop (no `quiet`), a Stop with no
//!   turn running and a `session.kill` mark and write nothing.
//! - While a turn runs, the first waiting input that may join goes into it when the runtime
//!   can take it ([`Adapter::can_inject`]) and the turn's own input is no `hqc_ask`: a
//!   person's input (no `peer`, no `schedule_id`) that is no slash command and has no cap,
//!   or a `send` of another session whose cap does not lower the session's mode and which
//!   the runtime's confirmed mode is not above; it passes the waiting inputs before it that
//!   are no person's and cannot join
//!   ([`Session::inject_next`]), as the VS Code tab takes what the user types meanwhile.
//!   Its acceptance is `ack {event_id, joined: true}`, also when it is resent; the first
//!   input the turn carries to the next one becomes that turn, one the runtime will not take
//!   goes back to the head of the queue, as does every one not taken when the process exits.
//! - `subagents` (Claude Code: [`AdapterEvent::Subagents`]) and `background_tasks`
//!   ([`AdapterEvent::BackgroundTasks`]) are repeated in every `session.state` like `title`,
//!   0 when the agent is stopped; so are `foreground_calls` ([`AdapterEvent::ForegroundCalls`]),
//!   empty then, and `activity` ([`AdapterEvent::Activity`]), none then. `session.background`
//!   moves one of those calls to the background ([`Adapter::background`]); it is acked, also
//!   with no agent running. The subagents' list ([`AdapterEvent::LiveSubagents`]) goes as
//!   `subagents.live`, empty when the agent is stopped; the last one sent is sent again on
//!   every connect, its run times counted then, an empty one only when it followed a non-empty
//!   one and the process has not stopped since. `session.stop_task` stops one of its tasks
//!   alone ([`Adapter::stop_task`]); it is acked, also with no agent running. The token rate
//!   ([`AdapterEvent::TokenRate`]) goes as `tokens.rate` when it changes, 0 once when the agent
//!   stops, and again as it is on every [`AdapterEvent::Heartbeat`]. The output file of a
//!   background task that ended ([`AdapterEvent::TaskOutput`]) is uploaded and becomes an
//!   `attachment` of the session. The commands a subagent left running 30 minutes after it
//!   reported ([`AdapterEvent::SubagentLeftovers`]) go as `session.subagent_leftovers`, and the
//!   background commands and subagents that have each run through another `long_run_note`
//!   ([`AdapterEvent::LongRuns`]) as `session.long_runs`, neither once the agent is stopped.
//! - A tool result with files ([`AdapterEvent::ToolResult`]: its whole text, its images) goes to
//!   the hub once they are uploaded with `PUT /worker/artifacts`, before the adapter's next
//!   event (SPEC §8.1).
//! - Every change of `process_state` is sent as `session.state`; all states are sent again
//!   on every connect.
//! - Permission mode (T38): `session.start.permission_mode` and `session.permission_mode`
//!   go through [`Config::permission_mode`] (`auto` only with `allow_auto`, `allow_all` only
//!   with `allow_all`); a running agent switches at once, a stopped one starts in the mode.
//!   The mode the runtime confirms is sent in `session.state.permission_mode`, repeated like
//!   `title`.
//! - A cap on an input (`session.input.permission_cap`, T39; `ask` < `auto` < `allow_all`):
//!   the agent runs that turn in the lower of the cap and the session's own mode. A running
//!   process is switched to that mode and the input waits for the runtime's confirmation
//!   ([`MODE_SWITCH_TIMEOUT`]; without it, or if the runtime cannot switch, the process is
//!   restarted in it); a process started for a capped input starts in it. When the turn ends
//!   (also when it is stopped), the session's own mode comes back unless the next input is
//!   capped too; a process lost mid-turn starts again in the mode of the input it runs next.
//!   A cap never raises the mode, and a switch the user asks for meanwhile reaches the
//!   runtime only as far as the cap allows, the rest at the end of the capped turn.
//!   `session.state.capped_input` names the input while its cap holds the session's own mode
//!   down.
//! - Model: `session.model` switches a running agent at once, mid-turn too
//!   ([`Adapter::set_model`]); `null` (the runtime's default) reaches it at its next start,
//!   as any model reaches a stopped one. A `session.start` for a running agent with another
//!   model does the same. A switch the runtime refuses leaves the model it kept
//!   ([`AdapterEvent::ModelKept`]). A session on a provider (SPEC 0.144) keeps the provider's
//!   model: the hub changes a provider with a restart.
//! - `idle_timeout` (SPEC §17.3): an idle process (no turn, the runtime's own included:
//!   [`Adapter::runs_own_turn`]; no subagent running, no background task the runtime lists:
//!   [`AdapterEvent::BackgroundTasks`]) is killed, the next input resumes it; one whose agent
//!   armed a wakeup ([`AdapterEvent::Wakeup`]) or has a cron job (a `kind: cron` row of
//!   [`AdapterEvent::LiveSubagents`] with `due_in_ms`) is idle only from the latest of those
//!   times on. A reconnect and a holder report (`Cmd::Resync`, `Cmd::Held`) are no activity of
//!   the agent.
//! - `max_sessions`: at most that many agent processes at once; one more start is
//!   `agent_start_failed`, retryable.
//! - Workspaces hqc makes and deletes (T45): `workspace.create` is acked at once and runs in
//!   the background ([`crate::workspaces`]); `workspace.inspect` and `workspace.delete` are
//!   answered like `fs.*`.
//! - Holders (SPEC §9.3): the mirror tells the hub (a holder report through the outbox:
//!   `session.state` with the nil `session_id`, the transcript's `runtime_session_id` and
//!   `held_by`) and the supervisor when a live Claude Code process outside the worker opens
//!   a transcript or lets it go. The supervisor reports all current holders again on every
//!   connect; a session whose agent runs that transcript stops after its turn, as for
//!   another writer.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use std::path::{Path, PathBuf};

use hqc_proto::event::{ArtifactKind, EventKind, LiveKind, PeerMode};
use hqc_proto::frame::{
    error_code, worker_approval, Ack, ClaudeAuthResult, ClaudeLoginDoneResult, ClaudeLoginResult,
    Envelope, HeldBy, HubFrame, InputNowResult, InputWithdrawnResult, LockedBy, LongRuns,
    PermissionMode, ProcessState, RequestResult, ResultBody, Runtime, SecretOutcome, SecretPut,
    SecretPutResult, SessionInput, SessionStart, StartMode, SubagentHistoryResult,
    SubagentLeftovers, TitleRequest, TitleResult, WireError, WorkerApprovalRequest, WorkerFrame,
    WorkspaceKeyDeleteResult, WorkspaceKeysResult,
};

/// Why `secret.put` and the workspace keys are refused on a worker without them (SPEC 0.147).
const SECRETS_OFF: &str = "this worker takes no secrets: session_secrets is off in its worker.toml";
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::adapter::{Adapter, AdapterEvent, AdapterFactory, ResultFiles};
use crate::config::{Config, StartSpec};
use crate::inbox::{self, ArtifactClient, InboxFile};
use crate::known_paths::KnownPaths;
use crate::link::HubLink;
use crate::permissions::FolderRules;

/// An input that ends this many turns without being taken is given up (see `end_turn`).
const MAX_REQUEUES: u8 = 2;
/// How long a capped input waits for the runtime to confirm the cap's mode before the
/// process is restarted in it (T39).
pub const MODE_SWITCH_TIMEOUT: Duration = Duration::from_secs(10);
use crate::mirror::{MirrorCmd, MirrorHandle};

/// `error.code` for input to a session another writer continues (SPEC §9.3).
pub const LOCKED_VSCODE: &str = "locked_vscode";

/// From the connection to the supervisor.
// Almost every value is a `Frame`; boxing it would only add an allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Inbound {
    Frame(Envelope<HubFrame>),
    /// `hello.ack` received: resend every `session.state`.
    Connected,
    /// The mirror saw another writer continue transcript `runtime_session_id` while a
    /// process of this worker runs it (SPEC §9.3).
    ForeignWriter {
        runtime_session_id: Uuid,
    },
    /// A live process outside the worker holds transcript `runtime_session_id` open, or
    /// (`None`) no longer does (SPEC §9.3).
    Holder {
        runtime_session_id: Uuid,
        held_by: Option<HeldBy>,
    },
    /// The hub refused a record of transcript `runtime_session_id` with `mirror_quota`
    /// (SPEC §9.1, §20): the mirror reads it again later.
    MirrorQuota {
        runtime_session_id: Uuid,
    },
}

/// How many delivered input ids a session remembers to re-ack resent inputs.
const DELIVERED_MEMORY: usize = 256;
const ADAPTER_EVENT_BUFFER: usize = 1024;
/// How long the owner has to answer a confirmation (`expires_at` of the request).
pub const APPROVAL_TTL: Duration = Duration::from_secs(600);
/// After `expires_at`, the start gives up even if the hub never answered.
const APPROVAL_GRACE: Duration = Duration::from_secs(30);

pub fn wire_error(code: &str, message: impl Into<String>, retryable: bool) -> WorkerFrame {
    WorkerFrame::Error(WireError {
        code: code.into(),
        message: message.into(),
        retryable,
        details: None,
    })
}

/// `error {not_now}`: the runtime cannot take the input into the running turn now (SPEC 0.219).
fn not_now(why: &str) -> WorkerFrame {
    wire_error(error_code::NOT_NOW, why, false)
}

fn ack(event_id: Option<Uuid>) -> WorkerFrame {
    WorkerFrame::Ack(Ack {
        event_id,
        seq: None,
        joined: false,
        quiet: false,
    })
}

/// `ack {event_id, joined: true}`: the input went into the turn already running.
fn joined_ack(event_id: Uuid) -> WorkerFrame {
    WorkerFrame::Ack(Ack {
        event_id: Some(event_id),
        seq: None,
        joined: true,
        quiet: false,
    })
}

/// `ack {event_id, quiet: true}`: the input was written into the agent's history on a Stop,
/// without a turn.
fn quiet_ack(event_id: Uuid) -> WorkerFrame {
    WorkerFrame::Ack(Ack {
        event_id: Some(event_id),
        seq: None,
        joined: false,
        quiet: true,
    })
}

/// How the runtime took a delivered input, which a resent input is acked again with.
#[derive(Clone, Copy)]
enum Taken {
    /// Into a turn of its own: a plain `ack`.
    Turn,
    /// Into the turn already running: `joined`.
    Joined,
    /// Into the history on a Stop: `quiet`.
    Quiet,
}

impl Taken {
    fn ack(self, event_id: Uuid) -> WorkerFrame {
        match self {
            Taken::Turn => ack(Some(event_id)),
            Taken::Joined => joined_ack(event_id),
            Taken::Quiet => quiet_ack(event_id),
        }
    }
}

pub struct Supervisor {
    cfg: Arc<Config>,
    factory: Arc<dyn AdapterFactory>,
    link: HubLink,
    artifacts: ArtifactClient,
    sessions: HashMap<Uuid, SessionHandle>,
    shutdown_timeout: Duration,
    /// One permit per running agent process (`max_sessions`).
    slots: Arc<Semaphore>,
    known: Arc<KnownPaths>,
    /// Starts waiting for the owner's consent, by folder.
    waiting: HashMap<PathBuf, Waiting>,
    /// `request_id` -> its folder in `waiting`.
    asked: HashMap<String, PathBuf>,
    /// Our own timeouts of `waiting` (`option_id: None`).
    outcomes_tx: mpsc::UnboundedSender<Outcome>,
    outcomes_rx: Option<mpsc::UnboundedReceiver<Outcome>>,
    /// The mirror of Claude Code sessions (SPEC §9); `None` when not configured.
    mirror: Option<MirrorHandle>,
    /// Current holders by transcript, reported again on every connect (SPEC §9.3).
    holders: HashMap<Uuid, HeldBy>,
    /// `title.request` runs (T24).
    titler: Arc<crate::title::Titler>,
    /// hqc's allow rules by canonical workspace (T48), from `session.start` and
    /// `workspace.rules`; also the workspaces `permissions.inspect` may look at.
    folders: HashMap<PathBuf, FolderRules>,
    /// Claude Code's config folder, for `~/.claude/settings.json` in `permissions.inspect`.
    claude_dir: Option<PathBuf>,
    /// The folders it creates and deletes for workspaces (T45).
    workspaces: Arc<crate::workspaces::Workspaces>,
    /// The sign-ins of the worker's Claude Code under way (SPEC 0.185).
    logins: crate::claude_auth::Logins,
}

/// A confirmation asked for and the `session.start` frames that wait for it.
struct Waiting {
    request: WorkerApprovalRequest,
    starts: Vec<(String, SessionStart)>,
}

#[derive(Debug)]
struct Outcome {
    request_id: String,
    option_id: Option<String>,
}

struct SessionHandle {
    tx: mpsc::UnboundedSender<Cmd>,
    task: JoinHandle<()>,
    /// The workspace of the last `session.start` the session took: another one is a move.
    workspace: PathBuf,
}

#[derive(Debug)]
enum Cmd {
    Start {
        id: String,
        spec: Box<StartSpec>,
        mode: StartMode,
        /// `session.start.no_wait`: no queueing for a slot.
        no_wait: bool,
    },
    Input {
        id: String,
        input: SessionInput,
    },
    Stop {
        id: String,
        /// The owner's own Stop (SPEC 0.231).
        quiet: bool,
    },
    /// Move the agent's running call `call_id` to the background.
    Background {
        id: String,
        call_id: String,
    },
    /// Stop the agent's task `task_id` alone.
    StopTask {
        id: String,
        task_id: String,
    },
    /// A side question for the agent (`session.side_question`).
    SideQuestion {
        id: String,
        event_id: Uuid,
        question: String,
        history: Vec<hqc_proto::frame::SideExchange>,
    },
    /// `session.subagent_history`: what subagent `task_id` has done, after event `after` and
    /// before event `before`.
    SubagentHistory {
        id: String,
        request_id: String,
        task_id: String,
        after: Option<Uuid>,
        before: Option<Uuid>,
    },
    /// `session.withdraw`: the owner takes input `event_id` back (SPEC 0.219).
    Withdraw {
        id: String,
        request_id: String,
        event_id: Uuid,
    },
    /// `session.input_now`: the owner sends input `event_id` into the running turn at once
    /// (SPEC 0.219).
    InputNow {
        id: String,
        request_id: String,
        event_id: Uuid,
    },
    Kill {
        id: String,
    },
    /// The mode the session runs in from now on, already decided by the config (T38).
    PermissionMode {
        id: String,
        mode: PermissionMode,
    },
    /// The session's model from now on; `None` — the runtime's default.
    Model {
        id: String,
        model: Option<String>,
    },
    Approval {
        id: String,
        request_id: String,
        option_id: String,
        /// The user's text with a deny option (T26).
        message: Option<String>,
    },
    Clarify {
        id: String,
        request_id: String,
        answers: std::collections::BTreeMap<String, hqc_proto::event::ClarifyAnswer>,
        action: Option<hqc_proto::event::ClarifyAction>,
    },
    /// A secret or a key for the running agent (SPEC 0.147).
    Secret {
        id: String,
        put: Box<SecretPut>,
    },
    Resync,
    /// Another writer continues transcript `runtime_session_id`: if it is this session's,
    /// stop after the current turn (SPEC §9.3).
    Release {
        runtime_session_id: Uuid,
    },
    /// A process outside the worker opened transcript `runtime_session_id`: if it is this
    /// session's, stop after the current turn (SPEC §9.3).
    Held {
        runtime_session_id: Uuid,
    },
    Shutdown,
}

impl Supervisor {
    pub fn new(
        cfg: Arc<Config>,
        factory: Arc<dyn AdapterFactory>,
        link: HubLink,
        artifacts: ArtifactClient,
        shutdown_timeout: Duration,
        known: Arc<KnownPaths>,
        created: crate::workspaces::Created,
    ) -> Self {
        let (outcomes_tx, outcomes_rx) = mpsc::unbounded_channel();
        Self {
            workspaces: crate::workspaces::Workspaces::new(cfg.clone(), created, link.clone()),
            artifacts,
            slots: Arc::new(Semaphore::new(cfg.max_sessions)),
            titler: Arc::new(crate::title::Titler::from_config(&cfg)),
            cfg,
            factory,
            link,
            sessions: HashMap::new(),
            shutdown_timeout,
            known,
            waiting: HashMap::new(),
            asked: HashMap::new(),
            outcomes_tx,
            outcomes_rx: Some(outcomes_rx),
            mirror: None,
            holders: HashMap::new(),
            folders: HashMap::new(),
            claude_dir: crate::adapter::claude_code::paths::default_config_dir(),
            logins: crate::claude_auth::Logins::default(),
        }
    }

    /// The CLI the worker's agents run: `claude_bin`, else `claude` from `PATH`.
    fn claude_bin(&self) -> PathBuf {
        self.cfg
            .claude_bin
            .clone()
            .unwrap_or_else(|| PathBuf::from("claude"))
    }

    /// Serve `mirror.set` and `workspace.list_candidates` with this mirror.
    pub fn with_mirror(mut self, m: MirrorHandle) -> Self {
        self.mirror = Some(m);
        self
    }

    /// Runs until `shutdown`, then kills every agent (SPEC §17.3: on worker restart all
    /// agents are `stopped`) and returns once their final events are in the outbox.
    pub async fn run(mut self, mut rx: mpsc::Receiver<Inbound>, shutdown: CancellationToken) {
        let mut outcomes = self.outcomes_rx.take().expect("run once");
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                m = rx.recv() => match m {
                    Some(m) => self.handle(m),
                    None => break,
                },
                Some(o) = outcomes.recv() => self.resolve(&o.request_id, o.option_id.as_deref()),
            }
        }
        tracing::info!(sessions = self.sessions.len(), "stopping all agents");
        let tasks: Vec<_> = self
            .sessions
            .drain()
            .map(|(_, h)| {
                let _ = h.tx.send(Cmd::Shutdown);
                h.task
            })
            .collect();
        let all = futures_util::future::join_all(tasks);
        if tokio::time::timeout(self.shutdown_timeout, all)
            .await
            .is_err()
        {
            tracing::warn!("some agents did not stop in time");
        }
    }

    fn handle(&mut self, m: Inbound) {
        let env = match m {
            Inbound::Connected => {
                for h in self.sessions.values() {
                    let _ = h.tx.send(Cmd::Resync);
                }
                // A request sent while disconnected was lost; the hub stores it once.
                for w in self.waiting.values() {
                    self.link
                        .send(WorkerFrame::WorkerApprovalRequest(w.request.clone()));
                }
                // The hub forgets holders on `hello` (SPEC §9.3).
                for (rid, h) in &self.holders {
                    if let Err(e) = self.link.holder_report(*rid, Some(h.clone())) {
                        tracing::error!("outbox write failed, holder report lost: {e:#}");
                    }
                }
                return;
            }
            Inbound::MirrorQuota { runtime_session_id } => {
                if let Some(m) = &self.mirror {
                    let _ = m.cmds.send(MirrorCmd::Defer { runtime_session_id });
                }
                return;
            }
            Inbound::Holder {
                runtime_session_id,
                held_by,
            } => {
                // The mirror has reported it to the hub.
                match &held_by {
                    Some(h) => self.holders.insert(runtime_session_id, h.clone()),
                    None => self.holders.remove(&runtime_session_id),
                };
                if held_by.is_some() {
                    // The session's own task knows its transcript best (a new session learns
                    // it from the runtime).
                    for h in self.sessions.values() {
                        let _ = h.tx.send(Cmd::Held { runtime_session_id });
                    }
                }
                return;
            }
            Inbound::ForeignWriter { runtime_session_id } => {
                // As for a holder: the session's own task knows its transcript, another one
                // after `/clear` (SPEC §8.1).
                for h in self.sessions.values() {
                    let _ = h.tx.send(Cmd::Release { runtime_session_id });
                }
                return;
            }
            Inbound::Frame(env) => env,
        };
        let id = env.id;
        let (session_id, cmd) = match env.body {
            HubFrame::SessionStart(s) => {
                let mut spec = match self.cfg.check_start(&s) {
                    Ok(spec) if self.factory.supports(spec.runtime) => spec,
                    Ok(spec) => {
                        let msg =
                            format!("runtime {:?} is not available on this worker", spec.runtime);
                        tracing::warn!(session_id = %s.session_id, "session.start rejected: {msg}");
                        self.link
                            .reply(&id, wire_error(error_code::INVALID_FRAME, msg, false));
                        return;
                    }
                    Err(msg) => {
                        tracing::warn!(session_id = %s.session_id, "session.start rejected: {msg}");
                        self.link
                            .reply(&id, wire_error(error_code::INVALID_FRAME, msg, false));
                        return;
                    }
                };
                let sid = spec.session_id;
                // A new session, or one that moves to another folder (SPEC §5.3).
                let new_folder = self
                    .sessions
                    .get(&sid)
                    .is_none_or(|h| h.workspace != spec.workspace);
                if s.workspace_path.is_some() && new_folder {
                    if s.owner_is_worker_owner || s.requested_by_owner {
                        self.remember(&spec.workspace);
                    } else if !self.known.contains(&spec.workspace) {
                        let folder = spec.workspace.clone();
                        self.wait_for_owner(id, s, folder);
                        return;
                    }
                }
                if s.workspace_path.is_some() {
                    let rules = self.folders.entry(spec.workspace.clone()).or_default();
                    rules.set(&s.allow_rules);
                    spec.folder_rules = Some(rules.clone());
                }
                if !self.sessions.contains_key(&sid) {
                    let h = self.spawn_session(&spec);
                    self.sessions.insert(sid, h);
                }
                if let Some(h) = self.sessions.get_mut(&sid) {
                    h.workspace = spec.workspace.clone();
                }
                (
                    sid,
                    Cmd::Start {
                        id,
                        spec: Box::new(spec),
                        mode: s.mode,
                        no_wait: s.no_wait,
                    },
                )
            }
            HubFrame::SessionInput(input) => (input.session_id, Cmd::Input { id, input }),
            HubFrame::SessionStop { session_id, quiet } => (session_id, Cmd::Stop { id, quiet }),
            HubFrame::SessionBackground {
                session_id,
                call_id,
            } => {
                // Nothing runs, nothing to move; an empty id would move every foreground call
                // of Claude Code's.
                if call_id.is_empty() || !self.sessions.contains_key(&session_id) {
                    self.link.reply(&id, ack(None));
                    return;
                }
                (session_id, Cmd::Background { id, call_id })
            }
            HubFrame::SessionStopTask {
                session_id,
                task_id,
            } => {
                if !self.sessions.contains_key(&session_id) {
                    // Nothing runs, nothing to stop.
                    self.link.reply(&id, ack(None));
                    return;
                }
                (session_id, Cmd::StopTask { id, task_id })
            }
            HubFrame::SessionSideQuestion {
                session_id,
                event_id,
                question,
                history,
            } => (
                session_id,
                Cmd::SideQuestion {
                    id,
                    event_id,
                    question,
                    history,
                },
            ),
            HubFrame::SessionSubagentHistory(r) => {
                // The id names a file: the hub is not trusted with paths (SPEC §2.1).
                if !hqc_proto::frame::valid_history_task_id(&r.task_id) {
                    let max = hqc_proto::frame::MAX_HISTORY_TASK_ID;
                    self.link.reply(
                        &id,
                        wire_error(
                            error_code::INVALID_FRAME,
                            format!("task_id must be 1 to {max} bytes of [A-Za-z0-9_-]"),
                            false,
                        ),
                    );
                    return;
                }
                (
                    r.session_id,
                    Cmd::SubagentHistory {
                        id,
                        request_id: r.request_id,
                        task_id: r.task_id,
                        after: r.after,
                        before: r.before,
                    },
                )
            }
            HubFrame::SessionWithdraw(r) => {
                if !self.sessions.contains_key(&r.session_id) {
                    // This worker holds no input of a session it does not know: nothing will
                    // reach its agent.
                    self.link.reply(
                        &id,
                        WorkerFrame::Result(RequestResult {
                            request_id: r.request_id,
                            body: ResultBody::InputWithdrawn(InputWithdrawnResult {
                                withdrawn: true,
                            }),
                        }),
                    );
                    return;
                }
                (
                    r.session_id,
                    Cmd::Withdraw {
                        id,
                        request_id: r.request_id,
                        event_id: r.event_id,
                    },
                )
            }
            HubFrame::SessionInputNow(r) => {
                if !self.sessions.contains_key(&r.session_id) {
                    self.link.reply(&id, not_now("no agent runs this session"));
                    return;
                }
                (
                    r.session_id,
                    Cmd::InputNow {
                        id,
                        request_id: r.request_id,
                        event_id: r.event_id,
                    },
                )
            }
            HubFrame::SessionKill { session_id } => {
                if !self.sessions.contains_key(&session_id) {
                    // Nothing runs, nothing to kill.
                    self.link.reply(&id, ack(None));
                    return;
                }
                (session_id, Cmd::Kill { id })
            }
            HubFrame::SessionPermissionMode {
                session_id,
                permission_mode,
            } => {
                if !self.sessions.contains_key(&session_id) {
                    // Nothing runs: the next `session.start` carries the mode.
                    self.link.reply(&id, ack(None));
                    return;
                }
                let mode = self.cfg.permission_mode(session_id, Some(permission_mode));
                (session_id, Cmd::PermissionMode { id, mode })
            }
            // Checked as in `session.start`: the value ends up on the agent's command line.
            HubFrame::SessionModel { session_id, model } => {
                if let Some(m) = model.as_deref().filter(|m| !crate::config::valid_model(m)) {
                    let msg = format!("model {m:?} is not a model name");
                    tracing::warn!(%session_id, "session.model rejected: {msg}");
                    self.link
                        .reply(&id, wire_error(error_code::INVALID_FRAME, msg, false));
                    return;
                }
                if !self.sessions.contains_key(&session_id) {
                    // Nothing runs: the next `session.start` carries the model.
                    self.link.reply(&id, ack(None));
                    return;
                }
                (session_id, Cmd::Model { id, model })
            }
            HubFrame::ApprovalResponse {
                session_id,
                request_id,
                option_id,
                message,
            } => (
                session_id,
                Cmd::Approval {
                    id,
                    request_id,
                    option_id,
                    message,
                },
            ),
            HubFrame::ClarifyResponse {
                session_id,
                request_id,
                answers,
                action,
            } => (
                session_id,
                Cmd::Clarify {
                    id,
                    request_id,
                    answers,
                    action,
                },
            ),
            HubFrame::WorkerApprovalResponse {
                request_id,
                option_id,
            } => {
                // Acked also when nothing waits any more (SPEC §10.2).
                self.link.reply(&id, ack(None));
                self.resolve(&request_id, Some(&option_id));
                return;
            }
            HubFrame::FsStat(r) => {
                let cfg = self.cfg.clone();
                self.fs_request(id, r.request_id, move || {
                    crate::fs::stat(&cfg, &r.path, r.create).map(ResultBody::FsStat)
                });
                return;
            }
            HubFrame::FsListDirs(r) => {
                let cfg = self.cfg.clone();
                self.fs_request(id, r.request_id, move || {
                    crate::fs::list_dirs(&cfg, r.path.as_deref()).map(ResultBody::FsListDirs)
                });
                return;
            }
            HubFrame::WorkspaceListCandidates(r) => {
                let (cfg, mirror) = (self.cfg.clone(), self.mirror.clone());
                self.fs_request(id, r.request_id, move || {
                    let m = mirror.ok_or("the mirror is not configured on this worker")?;
                    Ok(ResultBody::Candidates(crate::mirror::candidates(
                        &m.projects,
                        &m.store,
                        |p| cfg.inside_roots(p),
                    )))
                });
                return;
            }
            HubFrame::MirrorSet(m) => {
                self.mirror_set(&id, m);
                return;
            }
            HubFrame::SessionCleanup(c) => {
                // Acked at once: the hub does not wait for it (T21).
                self.link.reply(&id, ack(None));
                let cfg = self.cfg.clone();
                tokio::task::spawn_blocking(move || match crate::cleanup::session(&cfg, &c) {
                    Ok(n) => {
                        tracing::info!(session_id = %c.session_id, removed = n, "session cleaned up")
                    }
                    Err(e) => {
                        tracing::warn!(session_id = %c.session_id, "session.cleanup refused: {e:#}")
                    }
                });
                return;
            }
            HubFrame::TitleRequest(r) => {
                self.title_request(id, r);
                return;
            }
            HubFrame::WorkspaceRules(w) => {
                match self.workspace(&w.workspace_path) {
                    Some(path) => {
                        tracing::info!(path = %path.display(), rules = w.allow_rules.len(), "hqc rules of the folder");
                        self.folders.entry(path).or_default().set(&w.allow_rules);
                        self.link.reply(&id, ack(None));
                    }
                    None => {
                        tracing::warn!(path = %w.workspace_path, "workspace.rules refused: not a folder inside allowed_roots");
                        self.link.reply(
                            &id,
                            wire_error(
                                error_code::INVALID_FRAME,
                                "workspace_path is not a folder inside allowed_roots",
                                false,
                            ),
                        );
                    }
                }
                return;
            }
            HubFrame::PermissionsInspect(r) => {
                // Only a workspace the hub told about (rules or a session) and that lies
                // inside `allowed_roots`: the hub is not trusted with paths (SPEC §2.1).
                let folder = self
                    .workspace(&r.path)
                    .and_then(|p| self.folders.get(&p).map(|f| (p, f.list())));
                let env = folder.as_ref().map(|(p, _)| self.cfg.environment(p));
                let (perms, claude_dir) = (self.cfg.permissions.clone(), self.claude_dir.clone());
                self.fs_request(id, r.request_id, move || {
                    let (root, hqc) = folder.ok_or("path is not a workspace of this worker")?;
                    let env = env.ok_or("path is not a workspace of this worker")?;
                    Ok(ResultBody::Inspect(crate::permissions::inspect(
                        &perms,
                        &env.name,
                        &env.rules,
                        env.project_settings,
                        claude_dir.as_deref(),
                        &root,
                        hqc,
                        r.command.as_deref(),
                        r.tool.as_deref(),
                    )))
                });
                return;
            }
            HubFrame::WorkspaceCreate(c) => {
                // The outcome comes as `workspace.created`, also for a repeated request.
                self.link.reply(&id, ack(None));
                let mut known: Vec<PathBuf> = self.folders.keys().cloned().collect();
                known.extend(self.known.paths());
                self.workspaces.create(c, known);
                return;
            }
            HubFrame::WorkspaceInspect(r) => {
                let cfg = self.cfg.clone();
                self.fs_request(id, r.request_id, move || {
                    crate::workspaces::inspect(&cfg, &r.path).map(ResultBody::WorkspaceInspect)
                });
                return;
            }
            HubFrame::WorkspaceDelete(r) => {
                let ws = self.workspaces.clone();
                self.fs_request(id, r.request_id.clone(), move || {
                    ws.delete(&r).map(ResultBody::WorkspaceDelete)
                });
                return;
            }
            // SPEC 0.147: never with `session_secrets` off, whatever the hub sends.
            HubFrame::SecretPut(put) => {
                if !self.cfg.session_secrets {
                    tracing::warn!(session_id = %put.session_id, "secret.put refused: session_secrets is off");
                    self.link.reply(
                        &id,
                        wire_error(error_code::INVALID_FRAME, SECRETS_OFF, false),
                    );
                    return;
                }
                (
                    put.session_id,
                    Cmd::Secret {
                        id,
                        put: Box::new(put),
                    },
                )
            }
            HubFrame::WorkspaceKeys(r) => {
                let keys = self.workspace_keys(&r.path);
                self.fs_request(id, r.request_id, move || {
                    let keys = keys?;
                    keys.list()
                        .map(|keys| ResultBody::WorkspaceKeys(WorkspaceKeysResult { keys }))
                        .map_err(|e| format!("{e:#}"))
                });
                return;
            }
            HubFrame::WorkspaceKeyDelete(r) => {
                let keys = self.workspace_keys(&r.path);
                self.fs_request(id, r.request_id, move || {
                    let deleted = keys?.delete(&r.name).map_err(|e| format!("{e:#}"))?;
                    if deleted {
                        tracing::info!(path = %r.path, name = %r.name, "workspace ssh key forgotten");
                    }
                    Ok(ResultBody::WorkspaceKeyDeleted(WorkspaceKeyDeleteResult {
                        key_deleted: deleted,
                    }))
                });
                return;
            }
            // SPEC 0.185: the worker's own Claude Code signed in from hqc. Off the loop: the CLI
            // takes seconds, a sign-in minutes.
            HubFrame::ClaudeAuth(r) => {
                let (link, bin) = (self.link.clone(), self.claude_bin());
                tokio::spawn(async move {
                    let claude_auth = crate::claude_auth::status(&bin).await;
                    link.reply(
                        &id,
                        WorkerFrame::Result(RequestResult {
                            request_id: r.request_id,
                            body: ResultBody::ClaudeAuth(ClaudeAuthResult { claude_auth }),
                        }),
                    );
                });
                return;
            }
            HubFrame::ClaudeLogin(r) => {
                let (link, bin, logins) =
                    (self.link.clone(), self.claude_bin(), self.logins.clone());
                tokio::spawn(async move {
                    let frame = match logins.start(&bin).await {
                        Ok(login) => WorkerFrame::Result(RequestResult {
                            request_id: r.request_id,
                            body: ResultBody::ClaudeLogin(ClaudeLoginResult { login }),
                        }),
                        Err(e) => {
                            tracing::warn!("Claude Code sign-in not started: {e:#}");
                            wire_error(error_code::INVALID_FRAME, format!("{e:#}"), false)
                        }
                    };
                    link.reply(&id, frame);
                });
                return;
            }
            HubFrame::ClaudeLoginCode(r) => {
                let (link, bin, logins) =
                    (self.link.clone(), self.claude_bin(), self.logins.clone());
                tokio::spawn(async move {
                    let frame = match logins.code(&bin, &r.login_id, &r.code.0).await {
                        Ok(login_done) => WorkerFrame::Result(RequestResult {
                            request_id: r.request_id,
                            body: ResultBody::ClaudeLoginDone(ClaudeLoginDoneResult { login_done }),
                        }),
                        Err(e) => wire_error(error_code::INVALID_FRAME, format!("{e:#}"), false),
                    };
                    link.reply(&id, frame);
                });
                return;
            }
            HubFrame::ClaudeLoginCancel(r) => {
                let (link, logins) = (self.link.clone(), self.logins.clone());
                tokio::spawn(async move {
                    logins.cancel(&r.login_id).await;
                    link.reply(&id, ack(None));
                });
                return;
            }
            other => {
                tracing::debug!(?other, "frame not for the supervisor");
                return;
            }
        };
        match self.sessions.get(&session_id) {
            Some(h) => {
                let _ = h.tx.send(cmd);
            }
            None => {
                // After a worker restart the hub must send `session.start` before resending
                // inputs; until then the session is unknown here.
                tracing::warn!(%session_id, "frame for a session that was not started here");
                let err = wire_error(
                    error_code::UNKNOWN_SESSION,
                    "session is not started on this worker; send session.start first",
                    matches!(cmd, Cmd::Input { .. }),
                );
                if matches!(cmd, Cmd::Input { .. }) {
                    self.link.reply_input(session_id, None, &id_of(&cmd), err);
                } else {
                    self.link.reply(&id_of(&cmd), err);
                }
            }
        }
    }

    /// The keys kept for the workspace at hub path `path` (SPEC 0.147): with `session_secrets`
    /// on, and only for a folder inside `allowed_roots`.
    fn workspace_keys(&self, path: &str) -> Result<crate::secrets::WorkspaceKeys, String> {
        if !self.cfg.session_secrets {
            return Err(SECRETS_OFF.into());
        }
        let folder = self
            .workspace(path)
            .ok_or("path is not a folder inside allowed_roots")?;
        Ok(self.cfg.secrets.workspace(&folder))
    }

    /// A hub path as the canonical folder it names, if that is a folder inside
    /// `allowed_roots`.
    fn workspace(&self, path: &str) -> Option<PathBuf> {
        let p = Path::new(path);
        let c = p.canonicalize().ok()?;
        (p.is_absolute() && c.is_dir() && self.cfg.inside_roots(&c)).then_some(c)
    }

    fn remember(&self, folder: &Path) {
        if self.known.contains(folder) {
            return;
        }
        if let Err(e) = self.known.insert(folder) {
            tracing::error!(folder = %folder.display(), "known folders not saved: {e:#}");
        }
    }

    /// Ask the owner before the first session of someone else in `folder` (SPEC §10.2).
    fn wait_for_owner(&mut self, id: String, s: SessionStart, folder: PathBuf) {
        tracing::info!(session_id = %s.session_id, folder = %folder.display(), "new folder: asking the worker owner");
        if let Some(w) = self.waiting.get_mut(&folder) {
            w.starts.push((id, s));
            return;
        }
        let request = WorkerApprovalRequest {
            request_id: format!("wa-{}", Uuid::now_v7()),
            kind: worker_approval::WORKSPACE_USE.into(),
            description: format!(
                "The hub wants to run sessions in {} for a session of another user; allowing it remembers the folder",
                folder.display()
            ),
            expires_at: chrono::Utc::now()
                + chrono::TimeDelta::from_std(APPROVAL_TTL).expect("small duration"),
        };
        self.link
            .send(WorkerFrame::WorkerApprovalRequest(request.clone()));
        let tx = self.outcomes_tx.clone();
        let request_id = request.request_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(APPROVAL_TTL + APPROVAL_GRACE).await;
            let _ = tx.send(Outcome {
                request_id,
                option_id: None,
            });
        });
        self.asked
            .insert(request.request_id.clone(), folder.clone());
        self.waiting.insert(
            folder,
            Waiting {
                request,
                starts: vec![(id, s)],
            },
        );
    }

    /// The owner's answer (`None`: our own timeout). `once` lets the waiting starts go on,
    /// anything else refuses them.
    fn resolve(&mut self, request_id: &str, option_id: Option<&str>) {
        let Some(folder) = self.asked.remove(request_id) else {
            tracing::debug!(%request_id, "answer for no waiting request");
            return;
        };
        let Some(w) = self.waiting.remove(&folder) else {
            return;
        };
        if option_id == Some(worker_approval::ONCE) {
            tracing::info!(folder = %folder.display(), "the worker owner allowed the folder");
            self.remember(&folder);
            for (id, s) in w.starts {
                self.handle(Inbound::Frame(Envelope::new(id, HubFrame::SessionStart(s))));
            }
            return;
        }
        let why = match option_id {
            None => "no answer in time",
            Some(_) => "refused",
        };
        tracing::warn!(folder = %folder.display(), "session start in a new folder: {why}");
        for (id, s) in w.starts {
            self.link.reply(
                &id,
                wire_error(
                    error_code::INVALID_FRAME,
                    format!(
                        "the worker owner did not allow sessions in {} ({why}; session {})",
                        folder.display(),
                        s.session_id
                    ),
                    false,
                ),
            );
        }
    }

    /// Answer an `fs.*` request off the supervisor loop (a big folder takes a while to read):
    /// `result {request_id, ...}`, or `error {invalid_frame}` with the reason.
    fn fs_request(
        &self,
        id: String,
        request_id: String,
        f: impl FnOnce() -> Result<ResultBody, String> + Send + 'static,
    ) {
        let link = self.link.clone();
        tokio::spawn(async move {
            let r = tokio::task::spawn_blocking(f)
                .await
                .unwrap_or_else(|e| Err(format!("fs request failed: {e}")));
            let frame = match r {
                Ok(body) => WorkerFrame::Result(RequestResult { request_id, body }),
                Err(msg) => {
                    tracing::info!(%request_id, "fs request refused: {msg}");
                    wire_error(error_code::INVALID_FRAME, msg, false)
                }
            };
            link.reply(&id, frame);
        });
    }

    /// `title.request` (T24): a one-off `claude -p` outside every session ([`crate::title`]);
    /// `result {title}` or `error` (`retryable` when [`crate::title::MAX_CONCURRENT`] runs
    /// are busy).
    fn title_request(&self, id: String, r: TitleRequest) {
        let (link, titler) = (self.link.clone(), self.titler.clone());
        tokio::spawn(async move {
            let frame = match titler.title(&r.user_text, &r.agent_text).await {
                Ok(title) => {
                    tracing::info!(session_id = %r.session_id, %title, "session title");
                    WorkerFrame::Result(RequestResult {
                        request_id: r.request_id,
                        body: ResultBody::Title(TitleResult { title }),
                    })
                }
                Err(crate::title::TitleError::Busy) => {
                    tracing::info!(session_id = %r.session_id, "title.request refused: busy");
                    wire_error(error_code::INTERNAL, "title runs are busy", true)
                }
                Err(crate::title::TitleError::Failed(e)) => {
                    tracing::warn!(session_id = %r.session_id, "no session title: {e}");
                    wire_error(error_code::INTERNAL, e, false)
                }
            };
            link.reply(&id, frame);
        });
    }

    /// `mirror.set` (SPEC §9.2): only a folder inside `allowed_roots`; the hub is not
    /// trusted with paths (SPEC §2.1).
    fn mirror_set(&self, id: &str, m: hqc_proto::frame::MirrorSet) {
        let refuse = |msg: &str| {
            tracing::warn!(path = %m.workspace_path, "mirror.set refused: {msg}");
            self.link
                .reply(id, wire_error(error_code::INVALID_FRAME, msg, false));
        };
        let Some(mirror) = &self.mirror else {
            return refuse("the mirror is not configured on this worker");
        };
        let path = std::path::Path::new(&m.workspace_path);
        let canonical = match path.canonicalize() {
            Ok(p) if path.is_absolute() && p.is_dir() && self.cfg.inside_roots(&p) => p,
            // Stopping needs no check: the folder may be gone already.
            _ if !m.enabled => path.to_path_buf(),
            _ => return refuse("workspace_path is not a folder inside allowed_roots"),
        };
        let since = m
            .sessions
            .iter()
            .filter_map(|s| Some((s.runtime_session_id.parse().ok()?, s.since_ts)))
            .collect();
        let _ = mirror.cmds.send(MirrorCmd::Set {
            path: canonical,
            enabled: m.enabled,
            since,
        });
        self.link.reply(id, ack(None));
    }

    fn spawn_session(&self, spec: &StartSpec) -> SessionHandle {
        let (ev_tx, ev_rx) = mpsc::channel(ADAPTER_EVENT_BUFFER);
        let adapter = self.factory.create(spec, ev_tx);
        let (tx, rx) = mpsc::unbounded_channel();
        let s = Session {
            id: spec.session_id,
            spec: spec.clone(),
            adapter,
            events: ev_rx,
            events_open: true,
            link: self.link.clone(),
            artifacts: self.artifacts.clone(),
            roots: self.cfg.allowed_roots.clone(),
            mirror: self.mirror.clone(),
            idle_timeout: self.cfg.idle_timeout,
            slots: self.slots.clone(),
            max_sessions: self.cfg.max_sessions,
            slot: None,
            waiting_slot: false,
            state: ProcessState::Stopped,
            runtime_session_id: None,
            title: None,
            commands: None,
            wake_at: None,
            permission_mode: None,
            asked_mode: spec.permission_mode,
            moving: None,
            lowering: None,
            released: false,
            started_once: false,
            resume_requested: false,
            queue: VecDeque::new(),
            turn: None,
            injected: Vec::new(),
            subagents: 0,
            live: None,
            live_cleared: false,
            background_tasks: 0,
            side_pending: Vec::new(),
            token_rate: 0,
            foreground_calls: Vec::new(),
            activity: None,
            stop_asked: false,
            stopped_by_move: 0,
            quiet_out: Vec::new(),
            stop_quiet: false,
            delivered: VecDeque::new(),
            last_activity: Instant::now(),
        };
        SessionHandle {
            tx,
            task: tokio::spawn(s.run(rx)),
            workspace: spec.workspace.clone(),
        }
    }
}

fn id_of(c: &Cmd) -> String {
    match c {
        Cmd::Start { id, .. }
        | Cmd::Input { id, .. }
        | Cmd::Stop { id, .. }
        | Cmd::Background { id, .. }
        | Cmd::StopTask { id, .. }
        | Cmd::SideQuestion { id, .. }
        | Cmd::SubagentHistory { id, .. }
        | Cmd::Withdraw { id, .. }
        | Cmd::InputNow { id, .. }
        | Cmd::Kill { id }
        | Cmd::PermissionMode { id, .. }
        | Cmd::Model { id, .. }
        | Cmd::Approval { id, .. }
        | Cmd::Clarify { id, .. }
        | Cmd::Secret { id, .. } => id.clone(),
        Cmd::Resync | Cmd::Release { .. } | Cmd::Held { .. } | Cmd::Shutdown => String::new(),
    }
}

/// The most of a subagent's transcript `session.subagent_history` reads, from its end: one runs
/// to 100 MB, and the client asks every 2 s while the history is open.
const SUBAGENT_TRANSCRIPT_TAIL: u64 = 4 * 1024 * 1024;

/// The answer to `session.subagent_history` from the subagent's transcript at `path`, its
/// events in session `session` ([`crate::mirror::map::subagent_events`]). The file is opened as
/// `hqc_attach` opens one (no symlink in its last component, a regular file: a FIFO would hang
/// the blocking thread), and only its last [`SUBAGENT_TRANSCRIPT_TAIL`] bytes are read, the
/// line cut at their start left out and `truncated` set. No such file, no events. Only the events
/// before event `before` are taken, none and `truncated` when it is not among them. When event
/// `after` is among the events taken, only the ones after it, with `continues` and not
/// `truncated`.
fn subagent_history(
    path: Option<PathBuf>,
    session: Uuid,
    after: Option<Uuid>,
    before: Option<Uuid>,
) -> SubagentHistoryResult {
    use std::io::{Read, Seek, SeekFrom};
    let mut bytes = Vec::new();
    let mut cut = false;
    if let Some(Ok((mut f, size))) = path.map(|p| crate::mcp::attach::open_regular(&p, u64::MAX)) {
        cut = size > SUBAGENT_TRANSCRIPT_TAIL;
        let read = f
            .seek(SeekFrom::Start(
                size.saturating_sub(SUBAGENT_TRANSCRIPT_TAIL),
            ))
            .and_then(|_| f.take(SUBAGENT_TRANSCRIPT_TAIL).read_to_end(&mut bytes));
        if read.is_err() {
            bytes.clear();
        } else if cut {
            let start = bytes
                .iter()
                .position(|&b| b == b'\n')
                .map_or(bytes.len(), |i| i + 1);
            bytes.drain(..start);
        }
    }
    let (mut events, truncated) =
        crate::mirror::map::subagent_events(&String::from_utf8_lossy(&bytes), before);
    // The client holds the page up to `after`; the ids come from the records' uuids, so the
    // same event has the same id in every read.
    let at = after.and_then(|a| events.iter().rposition(|e| e.event_id == a));
    if let Some(i) = at {
        events.drain(..=i);
    }
    for e in &mut events {
        e.session_id = session;
        e.author = hqc_proto::event::Author::Agent { id: session };
    }
    SubagentHistoryResult {
        events,
        truncated: at.is_none() && (truncated || cut),
        continues: at.is_some(),
    }
}

/// What [`Session::write_into_turn`] did with an input.
enum Injected {
    Written,
    /// The turn ended meanwhile: nothing written, the input is given back.
    Waits(Box<Queued>),
    /// The attachments or the write failed: the hub has the error.
    Failed,
}

struct Queued {
    /// Id of the latest `session.input` frame carrying this input; the ack answers it.
    frame_id: String,
    input: SessionInput,
    /// Times this input ended a turn without being taken and went back to the queue.
    requeued: u8,
    /// A Stop came while it waited: it is written into the agent's history without a turn
    /// ([`Session::write_quiet`]), never as an input of a turn.
    quiet: bool,
    /// Not an input of the hub but hqc's own note to the agent that an input was dropped at the
    /// Stop ([`Session::mark_quiet`]): it has no frame to ack or fail, and it is written after
    /// the quiet inputs.
    note: bool,
}

struct Turn {
    frame_id: String,
    input: SessionInput,
    accepted: bool,
    requeued: u8,
    /// The input has a cap (T39): the runtime confirmed the turn's mode before it got it,
    /// and the session's own mode waits for the end of the turn.
    capped: bool,
}

/// A capped input waiting for the runtime to confirm its turn's mode (T39), ready to hand
/// over.
struct Lowering {
    q: Queued,
    input: SessionInput,
    files: Vec<InboxFile>,
    deadline: Instant,
}

/// The highest mode the input's turn may run in (T39): its cap, `allow_all` without one.
fn cap(input: &SessionInput) -> PermissionMode {
    input.permission_cap.unwrap_or(PermissionMode::AllowAll)
}

/// The input has a cap below `allow_all` (T39): its turn runs in the lower of the cap and
/// the session's own mode.
fn capped(input: &SessionInput) -> bool {
    cap(input) < PermissionMode::AllowAll
}

struct Session {
    id: Uuid,
    spec: StartSpec,
    adapter: Box<dyn Adapter>,
    events: mpsc::Receiver<AdapterEvent>,
    events_open: bool,
    link: HubLink,
    artifacts: ArtifactClient,
    /// `allowed_roots` of the worker config: `.hqc/` is added to a git exclude file only
    /// inside them (SPEC §14).
    roots: Vec<std::path::PathBuf>,
    /// The mirror's projects folder and state: a moved session's transcript is moved there
    /// ([`crate::mirror::relocate`]); `None`: transcripts are not moved.
    mirror: Option<MirrorHandle>,
    idle_timeout: Option<Duration>,
    slots: Arc<Semaphore>,
    max_sessions: usize,
    /// Held while the agent process runs.
    slot: Option<OwnedSemaphorePermit>,
    /// The agent waits for a slot (#41): `max_sessions` agents run, so it starts when one of
    /// them stops, its inputs held until then; nothing is stopped to make room.
    waiting_slot: bool,
    state: ProcessState,
    runtime_session_id: Option<String>,
    /// The runtime's title of the session, repeated in every `session.state`.
    title: Option<String>,
    /// The runtime's slash commands, repeated in every `session.state` like `title`.
    commands: Option<Vec<hqc_proto::frame::SlashCommand>>,
    /// When the wakeup the agent armed is due ([`AdapterEvent::Wakeup`]): idle unload waits
    /// for it.
    wake_at: Option<Instant>,
    /// The permission mode the agent runs in (T38): the one it was started or switched
    /// with, then what the runtime reports; repeated in every `session.state`.
    permission_mode: Option<PermissionMode>,
    /// The mode last asked of the running process: its start flag or our last switch. The
    /// session's own mode is `spec.permission_mode`; they differ under a cap (T39).
    asked_mode: PermissionMode,
    /// A `session.start` with another folder that came while the agent ran (a move, SPEC
    /// §5.3): the next process starts with it.
    moving: Option<Box<StartSpec>>,
    /// A capped input waiting for the runtime's confirmation of its turn's mode (T39).
    lowering: Option<Lowering>,
    /// Stopped for that writer: `locked_by: vscode` until the next `session.start`.
    released: bool,
    started_once: bool,
    /// The last `session.start` had `mode: resume` (SPEC §5.3).
    resume_requested: bool,
    queue: VecDeque<Queued>,
    turn: Option<Turn>,
    /// Inputs written into the running turn and not accepted yet, in `seq` order.
    injected: Vec<Queued>,
    /// The runtime's count of running subagents, repeated in every `session.state`.
    subagents: u32,
    /// The last `subagents.live` list sent, unless it was empty, and when: sent again on every
    /// connect, its run times counted then.
    live: Option<(Vec<hqc_proto::event::LiveSubagent>, Instant)>,
    /// The last list sent was empty and came after a non-empty one: sent again on every connect
    /// until a non-empty one or the process's stop, since the hub keeps the last non-empty list
    /// while a background task runs, and one lost with the link would stay.
    live_cleared: bool,
    /// The background tasks the runtime lists ([`AdapterEvent::BackgroundTasks`]), repeated
    /// in every `session.state`.
    background_tasks: u32,
    /// The side questions asked of the process and not answered yet, by the `event_id` of
    /// their `side.question`: idle unload waits for them, and a process that ends answers them
    /// with an error.
    side_pending: Vec<Uuid>,
    /// The last `tokens.rate` sent.
    token_rate: u32,
    /// The calls the runtime runs in the foreground ([`AdapterEvent::ForegroundCalls`]),
    /// repeated in every `session.state`.
    foreground_calls: Vec<String>,
    /// What the agent waits on ([`AdapterEvent::Activity`]), repeated in every `session.state`.
    activity: Option<hqc_proto::frame::Activity>,
    /// A `session.stop` came since the last end of a turn: an input the runtime drops then
    /// was cancelled by the person, not lost ([`Session::end_turn`]).
    stop_asked: bool,
    /// The background tasks a move stopped with the process; told to the agent with its input
    /// until the runtime takes one ([`Session::prepared`]).
    stopped_by_move: u32,
    /// Inputs written quiet ([`Session::write_quiet`]) that the runtime has not taken yet.
    quiet_out: Vec<Queued>,
    /// The owner's Stop is ending the turn that ran (SPEC 0.231): nothing new goes into it, and
    /// what the runtime drops from it is written quiet at its end ([`Session::end_turn`]).
    stop_quiet: bool,
    /// Inputs the runtime took, with how.
    delivered: VecDeque<(Uuid, Taken)>,
    last_activity: Instant,
}

impl Session {
    async fn run(mut self, mut cmds: mpsc::UnboundedReceiver<Cmd>) {
        loop {
            let idle_deadline = self.idle_deadline();
            let lowering_deadline = self.lowering.as_ref().map(|l| l.deadline);
            tokio::select! {
                c = cmds.recv() => {
                    // A reconnect and a holder of some transcript (any Claude Code on the
                    // machine) are the worker's news, not this agent's work: a `Held` of its
                    // own transcript releases it anyway.
                    if !matches!(c, Some(Cmd::Resync | Cmd::Held { .. })) {
                        self.last_activity = Instant::now();
                    }
                    match c {
                        Some(Cmd::Shutdown) | None => {
                            self.kill_process("worker shutdown").await;
                            break;
                        }
                        Some(c) => self.on_cmd(c).await,
                    }
                }
                ev = self.events.recv(), if self.events_open => {
                    self.last_activity = Instant::now();
                    match ev {
                        Some(ev) => self.on_event(ev).await,
                        None => self.events_open = false,
                    }
                }
                p = self.slots.clone().acquire_owned(), if self.waiting_slot => {
                    if let Ok(p) = p {
                        self.slot = Some(p);
                        self.waiting_slot = false;
                        tracing::info!(session_id = %self.id, "a slot freed up, starting the agent");
                        match self.ensure_running().await {
                            Ok(()) => self.pump().await,
                            Err(e) if waits(&e) => {}
                            Err(e) => self.fail_queue(&e),
                        }
                    }
                }
                _ = sleep_until_opt(idle_deadline), if idle_deadline.is_some() => {
                    tracing::info!(session_id = %self.id, "idle timeout, stopping the agent");
                    self.close_process("idle timeout").await;
                }
                _ = sleep_until_opt(lowering_deadline), if lowering_deadline.is_some() => {
                    self.restart_lowered("the runtime did not confirm the turn's mode in time").await;
                }
            }
        }
        // Events the adapter emitted while being killed.
        while let Ok(ev) = self.events.try_recv() {
            self.on_event(ev).await;
        }
    }

    fn idle_deadline(&self) -> Option<Instant> {
        let t = self.idle_timeout?;
        // A background subagent or command outlives the turn and may be silent for long (a
        // build). A turn the runtime started by itself is a turn as well: it is silent while it
        // waits for an approval or an answer, or runs a long command. An armed wakeup and a cron
        // job die with the process: the agent is idle from the latest of their times on (a job
        // without a known next fire holds nothing). A side question waits for its answer.
        (self.state == ProcessState::Running
            && self.turn.is_none()
            && !self.adapter.runs_own_turn()
            && self.injected.is_empty()
            && self.quiet_out.is_empty()
            && self.lowering.is_none()
            && self.queue.is_empty()
            && self.subagents == 0
            && self.background_tasks == 0
            && self.side_pending.is_empty())
        .then(|| {
            let crons = self.live.iter().flat_map(|(rows, at)| {
                rows.iter()
                    .filter(|r| r.kind == Some(LiveKind::Cron))
                    .filter_map(move |r| r.due_in_ms.map(|d| *at + Duration::from_millis(d)))
            });
            self.wake_at
                .into_iter()
                .chain(crons)
                .fold(self.last_activity, Instant::max)
                + t
        })
    }

    async fn on_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::Start {
                id,
                spec,
                mode,
                no_wait,
            } => {
                if self.state != ProcessState::Stopped {
                    // The hub issued a new token with this start (SPEC §5.3): the running
                    // agent's MCP calls carry it from now on, and so does its next process.
                    self.link.mcp().set_hub_token(self.id, &spec.session_token);
                    self.spec.session_token = spec.session_token.clone();
                    // Switches the worker missed while it was offline (T38).
                    self.switch_mode(spec.permission_mode).await;
                    self.switch_model(spec.model.clone()).await;
                    // Another folder is a move (SPEC §5.3): the agent goes there once no turn
                    // and no subagent runs; the same folder again cancels a move still waiting.
                    let moving = spec.workdir != self.spec.workdir;
                    if moving {
                        tracing::info!(session_id = %self.id, workdir = %spec.workdir.display(), "session.start with another folder; the agent moves there after its turn and subagents");
                    } else if *spec != self.spec {
                        tracing::warn!(session_id = %self.id, "session.start for a running agent; keeping the running one");
                    }
                    self.moving = moving.then_some(spec);
                    self.link.reply(&id, ack(None));
                    self.restart_for_move().await;
                    // What waited for the move goes to the new process, or, the move
                    // cancelled, to this one.
                    self.pump().await;
                    return;
                }
                self.moving = None;
                self.spec = *spec;
                self.resume_requested = mode == StartMode::Resume;
                // A takeover (SPEC §9.3): the hub decided this worker runs the session again.
                self.released = false;
                // A start for a side question is no start worth waiting for (SPEC 0.232): with no
                // free slot the session is not put in line and the feed says nothing; its
                // settings are kept for the next start, and the side question that follows is
                // answered with an error.
                if no_wait && self.slot.is_none() && self.slots.available_permits() == 0 {
                    tracing::info!(session_id = %self.id, max_sessions = self.max_sessions, "no free slot for a side question; the agent is not started");
                    self.link.reply(&id, ack(None));
                    return;
                }
                match self.ensure_running().await {
                    Ok(()) => {
                        self.link.reply(&id, ack(None));
                        self.pump().await;
                    }
                    // Taken: the agent starts when a slot frees up (#41).
                    Err(e) if waits(&e) => self.link.reply(&id, ack(None)),
                    Err(e) => {
                        self.link.reply(&id, start_failed(&e));
                        self.fail_queue(&e);
                    }
                }
            }
            Cmd::Input { id, input } => self.on_input(id, input).await,
            Cmd::Stop { id, quiet } => {
                // Also with no turn of ours: the runtime may run one it started by itself
                // (Claude Code after a background task's end); the adapter knows.
                let turn = self.turn.is_some() || self.adapter.runs_own_turn();
                if self.state == ProcessState::Running {
                    // The owner's Stop of a turn writes what waits into the history (SPEC
                    // 0.231); any other stop is the plain one.
                    let quiet = quiet && turn;
                    // With no turn the flag would outlive the Stop and make the end of a later
                    // turn treat its dropped inputs as cancelled.
                    self.stop_asked = turn;
                    let r = if quiet {
                        self.adapter.stop_quiet().await
                    } else {
                        self.adapter.stop().await
                    };
                    if let Err(e) = r {
                        tracing::warn!(session_id = %self.id, "stop failed: {e:#}");
                        self.link.reply(
                            &id,
                            wire_error(error_code::INTERNAL, format!("{e:#}"), true),
                        );
                        return;
                    }
                    if quiet {
                        // What waits now is what the Stop is for; what comes later is not.
                        self.stop_quiet = true;
                        for q in std::mem::take(&mut self.queue) {
                            self.mark_quiet(q);
                        }
                        if let Some(l) = self.lowering.take() {
                            self.mark_quiet(l.q);
                        }
                    }
                }
                self.link.reply(&id, ack(None));
            }
            Cmd::Background { id, call_id } => {
                if self.state == ProcessState::Running {
                    if let Err(e) = self.adapter.background(&call_id).await {
                        tracing::warn!(session_id = %self.id, "background failed: {e:#}");
                        self.link.reply(
                            &id,
                            wire_error(error_code::INTERNAL, format!("{e:#}"), false),
                        );
                        return;
                    }
                }
                self.link.reply(&id, ack(None));
            }
            Cmd::StopTask { id, task_id } => {
                if self.state == ProcessState::Running {
                    if let Err(e) = self.adapter.stop_task(&task_id).await {
                        tracing::warn!(session_id = %self.id, "stop_task failed: {e:#}");
                        self.link.reply(
                            &id,
                            wire_error(error_code::INTERNAL, format!("{e:#}"), false),
                        );
                        return;
                    }
                }
                self.link.reply(&id, ack(None));
            }
            Cmd::SideQuestion {
                id,
                event_id,
                question,
                history,
            } => {
                self.on_side_question(event_id, &question, &history).await;
                self.link.reply(&id, ack(None));
            }
            Cmd::SubagentHistory {
                id,
                request_id,
                task_id,
                after,
                before,
            } => {
                // Off the session's loop: a subagent's transcript runs to megabytes.
                let path = self.adapter.subagent_transcript(&task_id);
                let (link, session) = (self.link.clone(), self.id);
                tokio::spawn(async move {
                    let history = tokio::task::spawn_blocking(move || {
                        subagent_history(path, session, after, before)
                    })
                    .await
                    .unwrap_or_else(|_| SubagentHistoryResult {
                        events: vec![],
                        truncated: false,
                        continues: false,
                    });
                    link.reply(
                        &id,
                        WorkerFrame::Result(RequestResult {
                            request_id,
                            body: ResultBody::SubagentHistory(history),
                        }),
                    );
                });
            }
            Cmd::Withdraw {
                id,
                request_id,
                event_id,
            } => match self.withdraw(event_id).await {
                Ok(withdrawn) => self.link.reply(
                    &id,
                    WorkerFrame::Result(RequestResult {
                        request_id,
                        body: ResultBody::InputWithdrawn(InputWithdrawnResult { withdrawn }),
                    }),
                ),
                Err(e) => {
                    tracing::warn!(session_id = %self.id, %event_id, "withdraw failed: {e:#}");
                    self.link.reply(
                        &id,
                        wire_error(error_code::INTERNAL, format!("{e:#}"), false),
                    );
                }
            },
            Cmd::InputNow {
                id,
                request_id,
                event_id,
            } => match self.input_now(event_id).await {
                Ok(Some(sent_now)) => self.link.reply(
                    &id,
                    WorkerFrame::Result(RequestResult {
                        request_id,
                        body: ResultBody::InputNow(InputNowResult { sent_now }),
                    }),
                ),
                Ok(None) => self.link.reply(
                    &id,
                    not_now("no turn takes input now, or the agent waits for an answer"),
                ),
                Err(e) => {
                    tracing::warn!(session_id = %self.id, %event_id, "input_now failed: {e:#}");
                    self.link.reply(
                        &id,
                        wire_error(error_code::INTERNAL, format!("{e:#}"), false),
                    );
                }
            },
            Cmd::Kill { id } => {
                // Queued inputs stay `pending` in the hub and come again after `hello`.
                self.queue.clear();
                // Nothing running: the process may exit by itself and report what it could
                // not write; with work going on it is killed.
                if self.quiet() {
                    self.close_process("session.kill").await;
                } else {
                    self.kill_process("session.kill").await;
                }
                self.link.reply(&id, ack(None));
            }
            Cmd::PermissionMode { id, mode } => {
                self.switch_mode(mode).await;
                self.link.reply(&id, ack(None));
            }
            Cmd::Model { id, model } => {
                self.switch_model(model).await;
                self.link.reply(&id, ack(None));
            }
            Cmd::Approval {
                id,
                request_id,
                option_id,
                message,
            } => {
                let r = self
                    .adapter
                    .answer_approval(&request_id, &option_id, message.as_deref())
                    .await;
                self.reply_result(&id, r);
                // The runtime asks nothing more: what waits may go into the turn.
                self.pump().await;
            }
            Cmd::Clarify {
                id,
                request_id,
                answers,
                action,
            } => {
                let r = self
                    .adapter
                    .answer_clarify(&request_id, &answers, action)
                    .await;
                self.reply_result(&id, r);
                self.pump().await;
            }
            Cmd::Secret { id, put } => {
                let frame = match self.put_secret(&put).await {
                    Ok(secret) => {
                        tracing::info!(session_id = %self.id, kind = ?put.kind, name = %put.name, remembered = secret.remembered, "secret given to the agent");
                        WorkerFrame::Result(RequestResult {
                            request_id: put.request_id.clone(),
                            body: ResultBody::Secret(SecretPutResult { secret }),
                        })
                    }
                    Err(e) => {
                        tracing::info!(session_id = %self.id, kind = ?put.kind, name = %put.name, "secret.put refused: {e:#}");
                        wire_error(error_code::INVALID_FRAME, format!("{e:#}"), false)
                    }
                };
                self.link.reply(&id, frame);
            }
            Cmd::Resync => {
                self.send_state();
                // A list sent while disconnected was lost; the next one may come only with
                // the subagent's next step. Its age is added to the run times and taken off
                // the time to a fire.
                if let Some((agents, at)) = &self.live {
                    let age = u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX);
                    let mut agents = agents.clone();
                    for a in &mut agents {
                        a.duration_ms = a.duration_ms.saturating_add(age);
                        a.due_in_ms = a.due_in_ms.map(|d| d.saturating_sub(age));
                    }
                    self.live_subagents(agents);
                } else if self.live_cleared {
                    self.live_subagents(vec![]);
                }
                // So was a rate; the hub dropped the session's rate when the link went, so a
                // rate above 0 is sent again.
                if self.token_rate != 0 {
                    self.tokens_rate(self.token_rate);
                }
            }
            Cmd::Release { runtime_session_id } => {
                if self.runs(runtime_session_id) {
                    tracing::info!(session_id = %self.id, %runtime_session_id, "session continued elsewhere; releasing it now");
                    self.release().await;
                }
            }
            Cmd::Held { runtime_session_id } => {
                if self.runs(runtime_session_id) && self.state != ProcessState::Stopped {
                    tracing::info!(session_id = %self.id, %runtime_session_id, "the session was opened outside hqc; releasing it now");
                    self.release().await;
                }
            }
            Cmd::Shutdown => unreachable!("handled in run"),
        }
    }

    /// Whether `runtime_session_id` is the transcript this session runs.
    fn runs(&self, runtime_session_id: Uuid) -> bool {
        self.runtime_session_id
            .as_deref()
            .or(self.spec.runtime_session_id.as_deref())
            == Some(runtime_session_id.to_string().as_str())
    }

    /// Run in `mode` from now on (T38): the next start takes it from the spec, a running
    /// agent switches at once, mid-turn too (the runtime applies it to its next permission
    /// check). A runtime that refuses keeps its mode until its next start; `session.state`
    /// says which mode it runs.
    ///
    /// Under a cap (T39) the runtime goes no higher than the cap: a lower mode comes at once,
    /// the rest when the capped turn ends.
    async fn switch_mode(&mut self, mode: PermissionMode) {
        if self.spec.permission_mode == mode {
            return;
        }
        tracing::info!(session_id = %self.id, mode = mode.as_str(), "permission mode switched");
        self.spec.permission_mode = mode;
        if self.state == ProcessState::Stopped {
            return;
        }
        if let Some(c) = self.turn_cap() {
            tracing::info!(session_id = %self.id, "the turn runs under a cap; the mode follows when it ends");
            // Whether the cap still holds the session down changed.
            self.send_state();
            self.ask_runtime(mode.min(c)).await;
            return;
        }
        self.ask_runtime(mode).await;
    }

    /// Run `model` from now on: the next start takes it from the spec, a running agent
    /// switches at once, mid-turn too. `None`, the runtime's default, has no name to switch
    /// to: a running agent takes it at its next start. A runtime that refuses keeps its model
    /// until then too.
    /// `secret.put` for this session's running agent (SPEC §14, since 0.147). The error is
    /// what the hub shows the owner; it never carries the value, the key or the passphrase.
    async fn put_secret(&mut self, put: &SecretPut) -> anyhow::Result<SecretOutcome> {
        use hqc_proto::frame::SecretKind;
        let Some(allowed) = self.spec.secrets.clone() else {
            anyhow::bail!(
                "this session takes no secrets: it is restricted or not the worker owner's"
            );
        };
        let Some(proc) = self.adapter.secrets() else {
            anyhow::bail!("the agent is not running: give it once it runs");
        };
        if put.vault_field.is_some() && put.vault.is_none() {
            anyhow::bail!("vault_field needs a vault entry");
        }
        // A vault entry is read here, by this machine's `rbw`: the value never came from the hub.
        let vaulted = match put.vault.as_deref() {
            None => None,
            Some(entry) => {
                let Some(rbw) = &allowed.rbw else {
                    anyhow::bail!(
                        "this worker reads no Vaultwarden: vault is off in its worker.toml"
                    );
                };
                if put.kind == SecretKind::SshKeygen {
                    anyhow::bail!("ssh_keygen takes no vault entry");
                }
                if put.value.is_some() {
                    anyhow::bail!("give the value or the vault entry, not both");
                }
                hqc_proto::frame::check_vault_entry(entry).map_err(anyhow::Error::msg)?;
                let field = put.vault_field.as_deref();
                if let Some(f) = field {
                    hqc_proto::frame::check_vault_field(f).map_err(anyhow::Error::msg)?;
                }
                tracing::info!(session_id = %self.id, %entry, ?field, "reading a secret from Vaultwarden");
                Some(
                    crate::vault::read_for(rbw, entry, field, put.kind == SecretKind::SshKey)
                        .await
                        .map_err(|e| {
                            // Typing the value does not get past a refusal of the entry itself.
                            if e.is::<crate::vault::Refused>() {
                                e
                            } else {
                                anyhow::anyhow!("{e}, or type the value")
                            }
                        })?,
                )
            }
        };
        if put.vault.is_none() && put.value.is_none() && put.kind != SecretKind::SshKeygen {
            anyhow::bail!("give the value or a vault entry");
        }
        let value = || -> anyhow::Result<&str> {
            match vaulted
                .as_deref()
                .or_else(|| put.value.as_ref().map(|v| v.0.as_str()))
            {
                Some(v) if v.len() > hqc_proto::frame::MAX_SECRET => anyhow::bail!(
                    "the value is longer than {} KiB",
                    hqc_proto::frame::MAX_SECRET / 1024
                ),
                Some(v) if !v.is_empty() => Ok(v),
                _ => anyhow::bail!("the value is empty"),
            }
        };
        let workspace = || {
            allowed
                .workspace
                .clone()
                .ok_or_else(|| anyhow::anyhow!("the session has no workspace to keep a key for"))
        };
        let name = &put.name;
        match put.kind {
            SecretKind::Secret => {
                if put.remember {
                    anyhow::bail!("only an ssh key is kept for the workspace");
                }
                let path = proc.put(name, value()?)?;
                Ok(SecretOutcome {
                    note: format!(
                        "The secret {name} is in the file \"{path}\" for as long as your process runs: read it in a command, as \"$(cat \"{path}\")\", and do not print it."
                    ),
                    path: Some(path),
                    fingerprint: None,
                    public_key: None,
                    remembered: false,
                })
            }
            SecretKind::SshKey => {
                let key = crate::secrets::read_key(
                    value()?,
                    put.passphrase.as_ref().map(|p| p.0.as_str()),
                )?;
                let fp = crate::secrets::fingerprint(&key);
                let kept = if put.remember {
                    Some(workspace()?)
                } else {
                    None
                };
                if let Some(w) = &kept {
                    w.add(name, &key)?;
                }
                if let Err(e) = proc.add_key(&key).await {
                    if let Some(w) = &kept {
                        let _ = w.delete(name);
                    }
                    return Err(e);
                }
                let also = if put.remember {
                    " and remembered for this workspace"
                } else {
                    ""
                };
                Ok(SecretOutcome {
                    note: format!(
                        "The ssh key {name} ({fp}) is loaded in your ssh-agent{also}: ssh and git use it."
                    ),
                    path: None,
                    fingerprint: Some(fp),
                    public_key: None,
                    remembered: put.remember,
                })
            }
            SecretKind::SshKeygen => {
                let comment = match put.comment.as_deref().map(str::trim) {
                    Some(c) if c.chars().count() > 100 || c.chars().any(|c| c.is_control()) => {
                        anyhow::bail!("the comment is one line of at most 100 characters")
                    }
                    Some(c) if !c.is_empty() => c.to_string(),
                    _ => format!("hqc {name}"),
                };
                let w = workspace()?;
                let key = crate::secrets::generate(&comment)?;
                w.add(name, &key)?;
                if let Err(e) = proc.add_key(&key).await {
                    let _ = w.delete(name);
                    return Err(e);
                }
                let fp = crate::secrets::fingerprint(&key);
                Ok(SecretOutcome {
                    note: format!(
                        "The ssh key {name} ({fp}) is made, remembered for this workspace and loaded in your ssh-agent; its public key is public_key."
                    ),
                    path: None,
                    fingerprint: Some(fp),
                    public_key: Some(crate::secrets::public_line(&key)?),
                    remembered: true,
                })
            }
        }
    }

    async fn switch_model(&mut self, model: Option<String>) {
        if self.spec.model == model {
            return;
        }
        // A provider has one model (SPEC §8.1): a Claude model name would go to its endpoint.
        if let Some(p) = &self.spec.provider {
            tracing::info!(session_id = %self.id, ?model, provider = %p.name, "session.model ignored: the session runs on a provider");
            self.spec.model = model;
            return;
        }
        tracing::info!(session_id = %self.id, ?model, "model switched");
        self.spec.model = model;
        let Some(m) = self.spec.model.clone() else {
            return;
        };
        if self.state == ProcessState::Stopped {
            return;
        }
        if let Err(e) = self.adapter.set_model(&m).await {
            tracing::warn!(session_id = %self.id, model = %m, "model not switched until the next start: {e:#}");
        }
    }

    /// A move waits (SPEC §5.3): the running process is stopped and started again in the new
    /// folder, its transcript moved there ([`Session::ensure_running`]), once no turn runs (a
    /// turn the runtime started by itself included: [`Adapter::runs_own_turn`]) and no
    /// subagent, nothing is written into a turn and no capped input waits for its mode (T39);
    /// the end of a turn calls this again. Not the end of a background subagent: the runtime then
    /// runs a turn of its own on its result, and the end of that turn moves; a queued input
    /// moves the agent once nothing runs ([`Session::pump`]), in case no such turn comes.
    /// Background commands of the agent itself die with the process; its next input says so.
    async fn restart_for_move(&mut self) {
        if self.moving.is_none()
            || self.state != ProcessState::Running
            || self.turn.is_some()
            || self.adapter.runs_own_turn()
            || self.subagents > 0
            || !self.injected.is_empty()
            || !self.quiet_out.is_empty()
            || self.lowering.is_some()
        {
            return;
        }
        self.stopped_by_move += self.background_tasks;
        // A Stop the old process got cancels nothing the new one drops (`end_turn`).
        self.stop_asked = false;
        // A process with background tasks is killed (they die with it); any other lets its
        // transcript be written out before it is moved.
        if self.quiet() {
            self.close_process("moved to another folder").await;
        } else {
            self.kill_process("moved to another folder").await;
        }
        if let Err(e) = self.ensure_running().await {
            if !waits(&e) {
                self.fail_queue(&e);
            }
        }
    }

    /// The spec of a waiting move becomes the session's, with what changed since that start
    /// (token, mode, model) kept.
    fn take_move(&mut self) {
        let Some(next) = self.moving.take() else {
            return;
        };
        self.spec = StartSpec {
            session_token: self.spec.session_token.clone(),
            permission_mode: self.spec.permission_mode,
            model: self.spec.model.clone(),
            ..*next
        };
    }

    /// A Claude Code transcript that is not where the working directory puts it is moved
    /// there from wherever it is ([`crate::mirror::relocate`]): the session moved. (`&mut`: as
    /// in [`Session::prepared`].)
    async fn relocate(&mut self) -> anyhow::Result<()> {
        let (Some(m), Runtime::ClaudeCode) = (&self.mirror, self.spec.runtime) else {
            return Ok(());
        };
        let Some(uuid) = self
            .runtime_session_id
            .as_deref()
            .or(self.spec.runtime_session_id.as_deref())
            .and_then(|r| r.parse::<Uuid>().ok())
        else {
            return Ok(());
        };
        let (projects, store, roots, workdir, sessions, own) = (
            m.projects.clone(),
            m.store.clone(),
            self.roots.clone(),
            self.spec.workdir.clone(),
            m.sessions.clone(),
            m.own.clone(),
        );
        tokio::task::spawn_blocking(move || {
            // A fresh read: a holder outside hqc keeps the transcript where it is (SPEC §9.3).
            let registry = sessions
                .as_deref()
                .map(|dir| crate::mirror::relocate::Registry {
                    dir,
                    own: &own,
                    procs: &crate::mirror::holders::System,
                });
            crate::mirror::relocate::relocate(&projects, &store, &roots, uuid, &workdir, registry)
        })
        .await??;
        Ok(())
    }

    /// The cap of the capped turn that runs or waits for the runtime's confirmation (T39).
    fn turn_cap(&self) -> Option<PermissionMode> {
        match (&self.lowering, &self.turn) {
            (Some(l), _) => Some(cap(&l.q.input)),
            (None, Some(t)) if t.capped => Some(cap(&t.input)),
            _ => None,
        }
    }

    /// Asks the running process for `mode` unless it was asked for it last. A refusal or a
    /// runtime that cannot switch leaves it in its mode; `session.state` says which.
    async fn ask_runtime(&mut self, mode: PermissionMode) -> bool {
        if self.asked_mode == mode {
            return true;
        }
        self.asked_mode = mode;
        match self.adapter.set_permission_mode(mode).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(session_id = %self.id, mode = mode.as_str(), "permission mode not switched until the next start: {e:#}");
                false
            }
        }
    }

    /// A reply to the `session.input` frame `id` of this session (SPEC §5.2).
    fn reply_input(&self, id: &str, frame: WorkerFrame) {
        self.link
            .reply_input(self.id, self.transcript().as_deref(), id, frame);
    }

    /// The transcript this session runs: what `session.state` reports as its
    /// `runtime_session_id`.
    fn transcript(&self) -> Option<String> {
        self.runtime_session_id
            .clone()
            .or_else(|| self.spec.runtime_session_id.clone())
    }

    fn reply_result(&self, id: &str, r: anyhow::Result<()>) {
        match r {
            Ok(()) => self.link.reply(id, ack(None)),
            Err(e) => {
                tracing::warn!(session_id = %self.id, "adapter call failed: {e:#}");
                self.link.reply(
                    id,
                    wire_error(error_code::INTERNAL, format!("{e:#}"), false),
                );
            }
        }
    }

    async fn on_input(&mut self, id: String, input: SessionInput) {
        let eid = input.event_id;
        if self.released {
            // The hub refuses input while VS Code drives the session; input sent anyway
            // must not take the session back silently (SPEC §9.3).
            self.reply_input(
                &id,
                wire_error(
                    LOCKED_VSCODE,
                    "the session continues in VS Code; take it over first",
                    false,
                ),
            );
            return;
        }
        if let Some(&(_, taken)) = self.delivered.iter().find(|(e, _)| *e == eid) {
            // The same ack again: a plain one for a joined or a quiet input would open a turn
            // in the hub that nothing closes.
            self.reply_input(&id, taken.ack(eid));
            return;
        }
        if let Some(t) = self.turn.as_mut().filter(|t| t.input.event_id == eid) {
            // Resent after a reconnect while still in flight; answer the new frame.
            t.frame_id = id;
            return;
        }
        if let Some(l) = self.lowering.as_mut().filter(|l| l.q.input.event_id == eid) {
            l.q.frame_id = id;
            return;
        }
        if let Some(q) = self.injected.iter_mut().find(|q| q.input.event_id == eid) {
            q.frame_id = id;
            return;
        }
        if let Some(q) = self.quiet_out.iter_mut().find(|q| q.input.event_id == eid) {
            q.frame_id = id;
            return;
        }
        if let Some(q) = self.queue.iter_mut().find(|q| q.input.event_id == eid) {
            q.frame_id = id;
            // The hub's latest frame: its `peer_prefix` is what the hub clears on the ack.
            q.input = input;
            return;
        }
        self.queue.push_back(Queued {
            frame_id: id,
            input,
            requeued: 0,
            quiet: false,
            note: false,
        });
        if self.state == ProcessState::Stopped {
            if let Err(e) = self.ensure_running().await {
                // Waiting for a slot, the input waits in the queue with it (#41).
                if !waits(&e) {
                    self.fail_queue(&e);
                }
                return;
            }
        }
        self.pump().await;
    }

    /// Hands the next queued input to the adapter if no turn is running, or writes it into
    /// the running turn ([`Session::inject_next`]). While a move waits the input is for the
    /// agent in the new folder: it waits as long as something holds the move, then the move
    /// goes first ([`Session::restart_for_move`]). A capped input first waits for the runtime
    /// to confirm its turn's mode, the lower of its cap and the session's own (T39); an
    /// uncapped one runs in the session's own mode.
    async fn pump(&mut self) {
        while self.lowering.is_none() && self.state == ProcessState::Running {
            // The stopped turn has not ended: what the owner's Stop marks is not complete yet,
            // and nothing new goes into that turn (SPEC 0.231).
            if self.stop_quiet {
                return;
            }
            if self.turn.is_some() {
                if !self.inject_next().await {
                    return;
                }
                continue;
            }
            if self.moving.is_some() {
                if self.queue.is_empty() {
                    return;
                }
                self.restart_for_move().await;
                if self.moving.is_some() {
                    return;
                }
                continue;
            }
            // What waited at a Stop goes first, also while the runtime runs a turn of its own:
            // a quiet line is written with `priority: "later"`, which the CLI never folds into a
            // running turn.
            if self.write_quiet().await {
                continue;
            }
            // The CLI takes a "next" line before a "later" one that is in its queue too, so a
            // normal input written now would run before the quiet lines (the agent would answer
            // it unaware of them): it waits for their acks, which pump again. So no turn of
            // ours runs while quiet lines are out.
            if !self.quiet_out.is_empty() {
                return;
            }
            let Some(q) = self.queue.pop_front() else {
                return;
            };
            let Some((input, files)) = self.prepared(&q).await else {
                continue;
            };
            let own = self.spec.permission_mode;
            let mode = own.min(cap(&q.input));
            if !capped(&q.input) {
                // After a capped turn whose next input was capped too (T39).
                self.ask_runtime(own).await;
            } else if !(self.asked_mode == mode && self.permission_mode.is_some_and(|m| m <= mode))
            {
                // The turn's mode is asked for or not confirmed yet. A runtime already at or
                // below it with nothing new to ask (it may have lowered itself, as `plan` does)
                // skips this wait: nothing would confirm a mode not asked for.
                tracing::info!(session_id = %self.id, event_id = %q.input.event_id, mode = mode.as_str(), "capped input: switching the agent first");
                if !self.ask_runtime(mode).await {
                    // It cannot switch: a new process in the turn's mode runs it.
                    self.queue.push_front(q);
                    self.restart_for_cap("the runtime cannot switch").await;
                    continue;
                }
                self.lowering = Some(Lowering {
                    q,
                    input,
                    files,
                    deadline: Instant::now() + MODE_SWITCH_TIMEOUT,
                });
                return;
            }
            self.hand(q, input, files).await;
        }
    }

    /// Writes the inputs a Stop marked into the agent's history without a turn, in `event_id`
    /// order (the hub's creation order) and the notes of dropped inputs after them, to wait in
    /// [`Session::quiet_out`] for the runtime to take them; one whose attachments or write fail
    /// gets the error as any input (a note has no frame: it is only logged). `true`: there were
    /// some.
    async fn write_quiet(&mut self) -> bool {
        let (mut marked, rest): (Vec<Queued>, Vec<Queued>) = std::mem::take(&mut self.queue)
            .into_iter()
            .partition(|q| q.quiet);
        self.queue = rest.into();
        if marked.is_empty() {
            return false;
        }
        marked.sort_by_key(|q| (q.note, q.input.event_id));
        for q in marked {
            let Some((input, files)) = self.prepared(&q).await else {
                continue;
            };
            match self.adapter.input_quiet_with(&input, &files).await {
                Ok(()) => {
                    tracing::info!(session_id = %self.id, event_id = %q.input.event_id, "input written without a turn (stopped)");
                    self.quiet_out.push(q);
                }
                Err(e) => {
                    tracing::warn!(session_id = %self.id, event_id = %q.input.event_id, note = q.note, "input failed: {e:#}");
                    if !q.note {
                        self.reply_input(
                            &q.frame_id,
                            wire_error(error_code::INTERNAL, format!("{e:#}"), true),
                        );
                    }
                }
            }
        }
        true
    }

    /// Input `q` as the runtime gets it: attachments in `.hqc/inbox` and their paths in the
    /// text (SPEC §12), messages from other sessions with headers and escaping (SPEC §7.1).
    /// `None`: the attachments failed, the hub has the error. (`&mut`: a `&Session` held
    /// across the await would need the adapter to be `Sync`.)
    async fn prepared(&mut self, q: &Queued) -> Option<(SessionInput, Vec<InboxFile>)> {
        match inbox::prepare(&self.artifacts, &self.spec, &q.input, &self.roots).await {
            Ok((input, files)) => {
                let mut input = crate::peer::runtime_input(&input);
                // An input that is no slash command (the runtime would not read it as one)
                // tells the agent, until the runtime takes one (`InputAccepted`); not one
                // written into a turn whose input tells it already (nothing was in flight at
                // the move).
                let told = self
                    .turn
                    .iter()
                    .map(|t| &t.input)
                    .chain(self.injected.iter().map(|q| &q.input))
                    .any(|i| !i.text.starts_with('/'));
                if self.stopped_by_move > 0 && !q.input.text.starts_with('/') && !told {
                    input.text = format!(
                        "[hqc] Moving this session to {} stopped {} background task(s) you had \
                         running (commands or monitors started in the background); start again \
                         the ones you still need.\n---\n\n{}",
                        self.spec.workdir.display(),
                        self.stopped_by_move,
                        input.text
                    );
                }
                Some((input, files))
            }
            Err(e) => {
                tracing::warn!(session_id = %self.id, event_id = %q.input.event_id, "attachments: {e:#}");
                self.reply_input(
                    &q.frame_id,
                    wire_error(
                        error_code::INTERNAL,
                        format!("attachments: {e:#}"),
                        e.retryable(),
                    ),
                );
                None
            }
        }
    }

    /// Writes the first waiting input that may join into the running turn (SPEC §17.2), as
    /// the VS Code tab does with what the user types meanwhile. May join: a person's input
    /// (no `peer`, no `schedule_id`) that is no slash command and has no cap (T39), and a
    /// `send` of another session (`peer.mode` `send`, no `schedule_id`) whose cap does not
    /// lower the session's own mode, which would not hold the turn's mode down, and which the
    /// runtime is confirmed at or below (as `pump` requires of a capped turn). It goes
    /// past the waiting inputs in front of it that are no person's and cannot join (an
    /// `hqc_ask`, a schedule run, a send with a lower cap), which keep their places for after
    /// the turn; a person's input that cannot join (a slash command, a capped one) stops the
    /// search, so a person's own inputs keep their order. The turn takes nothing if its own
    /// input is an `hqc_ask` (its result is the answer to its sender), the runtime asks
    /// nothing now and no move waits (the input is for the agent in the new folder).
    /// False: nothing goes, what waits waits for the end of the turn.
    async fn inject_next(&mut self) -> bool {
        if self.moving.is_some()
            || self.turn.as_ref().is_some_and(|t| {
                t.input
                    .peer
                    .as_ref()
                    .is_some_and(|p| p.mode != PeerMode::Send)
            })
            || !self.adapter.can_inject()
        {
            return false;
        }
        let own = self.spec.permission_mode;
        // The runtime is confirmed at or below the input's cap, as `pump` requires of a capped
        // turn: a switch it refused or has not confirmed yet leaves the turn above the cap.
        let (asked, running) = (self.asked_mode, self.permission_mode);
        let fits_cap =
            |input: &SessionInput| asked <= cap(input) && running.is_some_and(|m| m <= cap(input));
        let mut at = None;
        for (i, q) in self.queue.iter().enumerate() {
            let input = &q.input;
            // Stopped: goes into the history, not into a turn.
            if q.quiet || input.schedule_id.is_some() {
                continue;
            }
            match &input.peer {
                None if input.text.trim_start().starts_with('/') || capped(input) => break,
                None => {
                    at = Some(i);
                    break;
                }
                Some(p) if p.mode == PeerMode::Send && cap(input) >= own && fits_cap(input) => {
                    at = Some(i);
                    break;
                }
                Some(_) => {}
            }
        }
        let Some(i) = at else {
            return false;
        };
        let Some(q) = self.queue.remove(i) else {
            return false;
        };
        match self.write_into_turn(q, false).await {
            Injected::Written | Injected::Failed => true,
            // The turn ended meanwhile: the input goes once its end is handled, from its
            // own place.
            Injected::Waits(q) => {
                self.queue.insert(i.min(self.queue.len()), *q);
                false
            }
        }
    }

    /// Writes input `q`, out of every list, into the running turn; `now` with priority now,
    /// as the runtime moves the call the agent is in to the background for it (SPEC 0.219).
    async fn write_into_turn(&mut self, q: Queued, now: bool) -> Injected {
        let Some((input, files)) = self.prepared(&q).await else {
            return Injected::Failed;
        };
        let written = if now {
            self.adapter.inject_now_with(&input, &files).await
        } else {
            self.adapter.inject_with(&input, &files).await
        };
        match written {
            Ok(true) => {
                tracing::info!(session_id = %self.id, event_id = %q.input.event_id, now, "input written into the running turn");
                self.injected.push(q);
                Injected::Written
            }
            Ok(false) => Injected::Waits(Box::new(q)),
            Err(e) => {
                tracing::warn!(session_id = %self.id, event_id = %q.input.event_id, "input failed: {e:#}");
                self.reply_input(
                    &q.frame_id,
                    wire_error(error_code::INTERNAL, format!("{e:#}"), true),
                );
                Injected::Failed
            }
        }
    }

    /// `session.withdraw` (SPEC 0.219): the owner takes input `event_id` back. `true`: the
    /// agent will never see it and it is out of this session's books, with no `ack` and no
    /// error to the hub. One that waits here (the queue, the wait for a mode switch, the wait
    /// for a slot) just leaves, and a session that waited for a slot for it alone leaves the line;
    /// one written to the runtime goes by its own word
    /// ([`Adapter::withdraw_input`]), and is out only if the runtime had not taken it;
    /// one taken already, `false`. Nothing here holds an input the hub believes in flight
    /// after a worker restart: gone, `true`. The turn a withdrawn input kept running ends by
    /// the adapter's [`AdapterEvent::TurnEnded`], which also goes on with the queue.
    async fn withdraw(&mut self, event_id: Uuid) -> anyhow::Result<bool> {
        if let Some(i) = self.queue.iter().position(|q| q.input.event_id == event_id) {
            self.queue.remove(i);
            // The input a session waited for a slot for may have been the last one.
            if self.queue.is_empty() && self.state == ProcessState::Stopped {
                self.waiting_slot = false;
            }
            return Ok(true);
        }
        if self
            .lowering
            .as_ref()
            .is_some_and(|l| l.q.input.event_id == event_id)
        {
            self.lowering = None;
            self.pump().await;
            return Ok(true);
        }
        if self.delivered.iter().any(|(e, _)| *e == event_id)
            || self.quiet_out.iter().any(|q| q.input.event_id == event_id)
        {
            return Ok(false);
        }
        if let Some(i) = self
            .injected
            .iter()
            .position(|q| q.input.event_id == event_id)
        {
            if !self.adapter.withdraw_input(event_id).await? {
                return Ok(false);
            }
            self.injected.remove(i);
            return Ok(true);
        }
        if self
            .turn
            .as_ref()
            .is_some_and(|t| t.input.event_id == event_id)
        {
            if self.turn.as_ref().is_some_and(|t| t.accepted)
                || !self.adapter.withdraw_input(event_id).await?
            {
                return Ok(false);
            }
            self.turn = None;
        }
        Ok(true)
    }

    /// `session.input_now` (SPEC 0.219): the owner sends input `event_id`, which has not
    /// reached the agent, into the running turn at once. `Some(true)`: written with priority
    /// now; `Some(false)`: nothing to do, the agent took it already or the turn ended
    /// meanwhile (the input waits as before); `None`: the turn cannot take input now (none
    /// runs, the runtime asks for an answer, a move waits, the turn answers an `hqc_ask`), or
    /// the input is no person's plain message (a slash command, a capped one, one of another
    /// session), or this input is the turn's own, which the runtime starts by itself. An input
    /// the runtime holds back is taken out of its queue first ([`Session::withdraw`]) and
    /// written again as it was, with a new `uuid` of the runtime's.
    async fn input_now(&mut self, event_id: Uuid) -> anyhow::Result<Option<bool>> {
        let may = |i: &SessionInput| {
            i.peer.is_none()
                && i.schedule_id.is_none()
                && !i.text.trim_start().starts_with('/')
                && !capped(i)
        };
        // Not into the turn the owner's Stop is ending: what came after it goes after the quiet
        // lines (SPEC 0.231).
        let turn_takes = self.turn.is_some()
            && !self.stop_quiet
            && self.moving.is_none()
            && !self.turn.as_ref().is_some_and(|t| {
                t.input
                    .peer
                    .as_ref()
                    .is_some_and(|p| p.mode != PeerMode::Send)
            })
            && self.adapter.can_inject();
        if let Some(i) = self.queue.iter().position(|q| q.input.event_id == event_id) {
            if !turn_takes || !may(&self.queue[i].input) {
                return Ok(None);
            }
            let Some(q) = self.queue.remove(i) else {
                return Ok(None);
            };
            return Ok(match self.write_into_turn(q, true).await {
                Injected::Written => Some(true),
                Injected::Failed => Some(false),
                Injected::Waits(q) => {
                    self.queue.insert(i.min(self.queue.len()), *q);
                    None
                }
            });
        }
        if self.delivered.iter().any(|(e, _)| *e == event_id) {
            return Ok(Some(false));
        }
        let Some(i) = self
            .injected
            .iter()
            .position(|q| q.input.event_id == event_id)
        else {
            return Ok(None);
        };
        if !turn_takes || !may(&self.injected[i].input) {
            return Ok(None);
        }
        if !self.adapter.withdraw_input(event_id).await? {
            return Ok(Some(false));
        }
        let q = self.injected.remove(i);
        Ok(match self.write_into_turn(q, true).await {
            Injected::Written => Some(true),
            Injected::Failed => Some(false),
            // The turn ended while the message was out: it runs next, from the head.
            Injected::Waits(q) => {
                self.queue.push_front(*q);
                Some(false)
            }
        })
    }

    /// Gives input `q` (`input` as the runtime gets it) to the adapter: its turn begins.
    async fn hand(&mut self, q: Queued, input: SessionInput, files: Vec<InboxFile>) {
        // Before the runtime has it: its first tool call may come at once.
        self.link.mcp().set_turn(self.id, q.input.event_id);
        match self.adapter.input_with(&input, &files).await {
            Ok(()) => {
                let capped = capped(&q.input);
                self.turn = Some(Turn {
                    frame_id: q.frame_id,
                    input: q.input,
                    accepted: false,
                    requeued: q.requeued,
                    capped,
                });
                if capped {
                    self.send_state();
                }
            }
            Err(e) => {
                tracing::warn!(session_id = %self.id, event_id = %q.input.event_id, "input failed: {e:#}");
                self.reply_input(
                    &q.frame_id,
                    wire_error(error_code::INTERNAL, format!("{e:#}"), true),
                );
            }
        }
    }

    /// The runtime did not confirm the mode of the capped input that waits (T39): the input
    /// goes back to the head of the queue and a new process in that mode runs it.
    async fn restart_lowered(&mut self, why: &str) {
        let Some(l) = self.lowering.take() else {
            return;
        };
        self.queue.push_front(l.q);
        self.restart_for_cap(why).await;
        self.pump().await;
    }

    /// Kills the process and starts it again for the capped input at the head of the
    /// queue, which makes it start in that input's mode ([`Session::start_mode`]).
    async fn restart_for_cap(&mut self, why: &str) {
        tracing::warn!(session_id = %self.id, "restarting the agent in the mode of a capped input: {why}");
        self.kill_process("permission cap").await;
        if let Err(e) = self.ensure_running().await {
            if !waits(&e) {
                self.fail_queue(&e);
            }
        }
    }

    /// The mode a new process starts in: for the input at the head of the queue, the lower
    /// of its cap and the session's own mode (T39); the session's own without one.
    fn start_mode(&self) -> PermissionMode {
        let own = self.spec.permission_mode;
        self.queue.front().map_or(own, |q| own.min(cap(&q.input)))
    }

    /// The capped turn is over (T39): the session's own mode again, unless the next input
    /// is capped too.
    async fn after_capped_turn(&mut self) {
        if self.state != ProcessState::Running
            || self.queue.front().is_some_and(|q| capped(&q.input))
        {
            return;
        }
        let own = self.spec.permission_mode;
        self.ask_runtime(own).await;
    }

    /// Asks the agent a side question, starting its process first when none runs (it answers
    /// outside any turn). One that cannot be asked is answered with the reason at once, as the
    /// owner's block waits for `side.answer`.
    async fn on_side_question(
        &mut self,
        event_id: Uuid,
        question: &str,
        history: &[hqc_proto::frame::SideExchange],
    ) {
        if self.released {
            return self.side_answer(event_id, None, Some("The session continues in VS Code"));
        }
        if self.state == ProcessState::Stopped {
            // The agent starts only for a free slot: waiting for one would start it later for
            // nobody.
            if self.slot.is_none() && self.slots.available_permits() == 0 {
                return self.side_answer(
                    event_id,
                    None,
                    Some("All the agent slots of this worker are in use"),
                );
            }
            if let Err(e) = self.ensure_running().await {
                return self.side_answer(event_id, None, Some(&format!("{e:#}")));
            }
        }
        self.side_pending.push(event_id);
        if let Err(e) = self
            .adapter
            .side_question(event_id, question, history)
            .await
        {
            tracing::warn!(session_id = %self.id, "side question failed: {e:#}");
            self.side_answer(event_id, None, Some(&format!("{e:#}")));
        }
    }

    /// The stored `side.answer` to side question `event_id`, which is pending no more.
    fn side_answer(&mut self, event_id: Uuid, text: Option<String>, error: Option<&str>) {
        self.side_pending.retain(|e| *e != event_id);
        self.send_event(crate::adapter::new_event(
            self.id,
            hqc_proto::event::Author::Agent { id: self.id },
            EventKind::SideAnswer {
                question: event_id,
                text,
                error: error.map(str::to_owned),
            },
        ));
    }

    fn send_event(&self, e: hqc_proto::event::Event) {
        if e.session_id != self.id {
            tracing::warn!(session_id = %self.id, other = %e.session_id, "adapter event for another session dropped");
            return;
        }
        if let Err(err) = self.link.event(e) {
            tracing::error!(session_id = %self.id, "outbox write failed, event lost: {err:#}");
        }
    }

    /// Uploads what a tool result carries beyond its preview as artifacts of the session (SPEC
    /// §8.1): its whole text into `artifact_id`, its images into `artifact_ids`. One that fails
    /// is logged and left out.
    async fn upload_result(&mut self, event: &mut hqc_proto::event::Event, files: ResultFiles) {
        let EventKind::ToolResult {
            artifact_id,
            artifact_ids,
            ..
        } = &mut event.kind
        else {
            return;
        };
        if let Some(text) = files.text {
            match self
                .artifacts
                .upload_bytes(
                    self.id,
                    text.into_bytes(),
                    "result.txt",
                    "text/plain; charset=utf-8",
                    ArtifactKind::Text,
                )
                .await
            {
                Ok(id) => *artifact_id = Some(id),
                Err(e) => {
                    tracing::warn!(session_id = %self.id, code = %e.code, "upload of a tool result: {}", e.message)
                }
            }
        }
        for img in files.images {
            let name = format!(
                "image.{}",
                img.media_type.rsplit('/').next().unwrap_or_default()
            );
            match self
                .artifacts
                .upload_bytes(
                    self.id,
                    img.data,
                    &name,
                    &img.media_type,
                    ArtifactKind::Image,
                )
                .await
            {
                Ok(id) => artifact_ids.push(id),
                Err(e) => {
                    tracing::warn!(session_id = %self.id, code = %e.code, "upload of a tool result's image: {}", e.message)
                }
            }
        }
    }

    async fn on_event(&mut self, ev: AdapterEvent) {
        match ev {
            AdapterEvent::Event(e) => {
                // The runtime's own reply that it is signed out (SPEC 0.185): the feed says how
                // to sign it in again.
                let signed_out = matches!(&e.kind, EventKind::AgentMessage { runtime: true, text, .. } if crate::claude_auth::says_signed_out(text));
                self.send_event(e);
                if signed_out {
                    self.system_event(
                        "Claude Code on this worker is signed out: sign it in again from Settings → Workers",
                        CLAUDE_SIGNED_OUT,
                    );
                }
            }
            AdapterEvent::ToolResult { mut event, files } => {
                // Here, before the next event of the adapter: the feed keeps its order.
                if event.session_id == self.id {
                    self.upload_result(&mut event, files).await;
                }
                self.send_event(event);
            }
            AdapterEvent::SideAnswer {
                event_id,
                text,
                error,
            } => {
                // One answer per question: a pending one only.
                if self.side_pending.contains(&event_id) {
                    self.side_answer(event_id, text, error.as_deref());
                }
            }
            AdapterEvent::InputAccepted { event_id } => {
                let transcript = self.transcript();
                let (joined, slash) = if let Some(t) =
                    self.turn.as_mut().filter(|t| t.input.event_id == event_id)
                {
                    t.accepted = true;
                    self.link.reply_input(
                        self.id,
                        transcript.as_deref(),
                        &t.frame_id,
                        ack(Some(event_id)),
                    );
                    (false, t.input.text.starts_with('/'))
                } else if let Some(i) = self
                    .injected
                    .iter()
                    .position(|q| q.input.event_id == event_id)
                {
                    // Taken into the running turn: delivered, no turn of its own.
                    let q = self.injected.remove(i);
                    self.reply_input(&q.frame_id, joined_ack(event_id));
                    (true, q.input.text.starts_with('/'))
                } else {
                    tracing::warn!(session_id = %self.id, %event_id, "adapter accepted an input that is not in flight");
                    return;
                };
                // It carried the note of a move that stopped background tasks (`prepared`).
                if !slash {
                    self.stopped_by_move = 0;
                }
                self.remember_delivered(event_id, if joined { Taken::Joined } else { Taken::Turn });
            }
            AdapterEvent::QuietTaken { event_ids } => {
                for event_id in event_ids {
                    let Some(i) = self
                        .quiet_out
                        .iter()
                        .position(|q| q.input.event_id == event_id)
                    else {
                        tracing::warn!(session_id = %self.id, %event_id, "adapter took a quiet input that is not in flight");
                        continue;
                    };
                    let q = self.quiet_out.remove(i);
                    // A note answers no frame of the hub, which never sends its id again.
                    if !q.note {
                        self.reply_input(&q.frame_id, quiet_ack(event_id));
                        self.remember_delivered(event_id, Taken::Quiet);
                    }
                    // It carried the note of a move that stopped background tasks (`prepared`).
                    self.stopped_by_move = 0;
                }
                if self.quiet_out.is_empty() {
                    // A move that waited for the acks goes, and what came meanwhile runs.
                    self.restart_for_move().await;
                    self.pump().await;
                }
            }
            AdapterEvent::QuietCancelled { event_id } => {
                let Some(i) = self
                    .quiet_out
                    .iter()
                    .position(|q| q.input.event_id == event_id)
                else {
                    tracing::warn!(session_id = %self.id, %event_id, "adapter dropped a quiet input that is not in flight");
                    return;
                };
                // Still marked quiet: it is written again (up to the limit of any input the
                // runtime ends turns without taking).
                let q = self.quiet_out.remove(i);
                self.requeue(q);
                self.pump().await;
            }
            AdapterEvent::TurnEnded { carried, dropped } => {
                let before = self.turn.as_ref().map(|t| (t.input.event_id, t.capped));
                self.end_turn(&carried, &dropped);
                let after = self
                    .turn
                    .as_ref()
                    .map(|t| (t.input.event_id, t.capped, cap(&t.input)));
                match (before, after) {
                    // The same input is still the turn (a capped input the runtime carries
                    // keeps its cap), or no turn before or after: nothing changes.
                    (Some((was, _)), Some((is, _, _))) if was == is => {}
                    // Another input became the turn, written into the one before: the runtime
                    // already runs it, in the mode of the turn before, so no wait as in `pump`.
                    // This usually raises the mode, which may lag a moment; after the owner
                    // raised it during the turn before, it lowers the runtime to the send's cap,
                    // again without waiting for the confirmation.
                    (old, Some((_, now_capped, now_cap)))
                        if now_capped || old.is_some_and(|(_, was_capped)| was_capped) =>
                    {
                        let own = self.spec.permission_mode;
                        self.ask_runtime(if now_capped { own.min(now_cap) } else { own })
                            .await;
                        self.send_state();
                    }
                    // The capped turn is over (T39): the session's own mode again.
                    (Some((_, true)), None) => {
                        self.after_capped_turn().await;
                        self.send_state();
                    }
                    _ => {}
                }
                self.restart_for_move().await;
                self.pump().await;
            }
            // A count read from a process already stopped would outlive it.
            AdapterEvent::Subagents(n) => {
                if self.state != ProcessState::Stopped && self.subagents != n {
                    self.subagents = n;
                    self.send_state();
                }
            }
            // As the count: a list read from a process already stopped would outlive it.
            AdapterEvent::LiveSubagents(agents) => {
                if self.state != ProcessState::Stopped {
                    self.live_cleared =
                        agents.is_empty() && (self.live_cleared || self.live.is_some());
                    self.live = (!agents.is_empty()).then(|| (agents.clone(), Instant::now()));
                    self.live_subagents(agents);
                }
            }
            AdapterEvent::BackgroundTasks(n) => {
                if self.state != ProcessState::Stopped && self.background_tasks != n {
                    self.background_tasks = n;
                    self.send_state();
                }
            }
            // As the count: a rate read from a process already stopped would outlive it.
            AdapterEvent::TokenRate(r) => {
                if self.state != ProcessState::Stopped && self.token_rate != r {
                    self.token_rate = r;
                    self.tokens_rate(r);
                }
            }
            AdapterEvent::ForegroundCalls(calls) => {
                if self.state != ProcessState::Stopped && self.foreground_calls != calls {
                    self.foreground_calls = calls;
                    self.send_state();
                }
            }
            AdapterEvent::Activity(a) => {
                if self.state != ProcessState::Stopped && self.activity != a {
                    self.activity = a;
                    self.send_state();
                }
            }
            // The rate as it is: any event keeps the hub's turn watchdog off (SPEC §17.3).
            AdapterEvent::Heartbeat => {
                if self.state != ProcessState::Stopped {
                    self.tokens_rate(self.token_rate);
                }
            }
            // The hub makes it the session's model too, by the `system model_not_switched`
            // the adapter sends with it (SPEC §8.1).
            AdapterEvent::ModelKept(m) => self.spec.model = Some(m),
            AdapterEvent::Limits(l) => self.link.send(WorkerFrame::WorkerLimits(l)),
            // Uploaded beside the session's loop, which goes on meanwhile.
            AdapterEvent::TaskOutput(path) => {
                let (link, artifacts, id) = (self.link.clone(), self.artifacts.clone(), self.id);
                tokio::spawn(async move { attach_task_output(&link, &artifacts, id, &path).await });
            }
            // As the count: the commands of a process already stopped stopped with it.
            AdapterEvent::SubagentLeftovers {
                subagent,
                ended_s,
                commands,
            } => {
                if self.state != ProcessState::Stopped {
                    self.link
                        .send(WorkerFrame::SubagentLeftovers(SubagentLeftovers {
                            session_id: self.id,
                            subagent,
                            ended_s,
                            commands,
                        }));
                }
            }
            // As the leftovers: the tasks of a process already stopped stopped with it.
            AdapterEvent::LongRuns { tasks } => {
                if self.state != ProcessState::Stopped {
                    self.link.send(WorkerFrame::LongRuns(LongRuns {
                        session_id: self.id,
                        tasks,
                    }));
                }
            }
            AdapterEvent::RuntimeSessionId(r) => {
                // A start of this task resumes it, as the hub's next `session.start` will
                // (after `/clear` the transcript is another one, SPEC §8.1).
                self.spec.runtime_session_id = Some(r.clone());
                self.runtime_session_id = Some(r);
                self.send_state();
            }
            // As the count: a wakeup of a process already stopped died with it.
            AdapterEvent::Wakeup(at) if self.state != ProcessState::Stopped => {
                self.wake_at = at.map(|at| {
                    let after = (at - chrono::Utc::now()).to_std().unwrap_or_default();
                    Instant::now() + after
                });
            }
            AdapterEvent::Wakeup(_) => {}
            AdapterEvent::Title(t) => {
                if self.title.as_deref() != Some(t.as_str()) {
                    self.title = Some(t);
                    self.send_state();
                }
            }
            AdapterEvent::Commands(c) => {
                if self.commands.as_ref() != Some(&c) {
                    self.commands = Some(c);
                    self.send_state();
                }
            }
            AdapterEvent::PermissionMode(m) => {
                if self.permission_mode != Some(m) {
                    self.permission_mode = Some(m);
                    self.send_state();
                }
                // The confirmation a capped input waits for (T39).
                if m == self.asked_mode {
                    if let Some(l) = self.lowering.take() {
                        self.hand(l.q, l.input, l.files).await;
                    }
                }
            }
            AdapterEvent::Exited { error } => {
                if self.state == ProcessState::Stopped {
                    return;
                }
                let in_turn = self.turn.is_some();
                tracing::warn!(session_id = %self.id, ?error, in_turn, "agent process exited");
                // Nothing written to the gone process is on its way any more.
                let lost: Vec<Uuid> = self.injected.iter().map(|q| q.input.event_id).collect();
                // The Stop that wrote them still holds: the next process gets them quiet.
                for q in std::mem::take(&mut self.quiet_out).into_iter().rev() {
                    self.queue.push_front(q);
                }
                self.end_turn(&[], &lost);
                if let Some(l) = self.lowering.take() {
                    // It never reached the runtime; the next process starts in its mode.
                    self.queue.push_front(l.q);
                }
                self.set_state(ProcessState::Stopped);
                if error.is_some() || in_turn {
                    let text = match &error {
                        Some(e) => format!("Agent process exited: {e}"),
                        None => "Agent process exited during a turn".into(),
                    };
                    self.system_event(&text, "agent_exited");
                }
                if !self.queue.is_empty() {
                    match self.ensure_running().await {
                        Ok(()) => self.pump().await,
                        Err(e) if waits(&e) => {}
                        Err(e) => self.fail_queue(&e),
                    }
                }
            }
        }
    }

    /// Closes the current turn. `carried`: inputs written during it that the runtime runs
    /// next by itself (SPEC §17.2). The turn's own input, carried (it was written during a
    /// turn the runtime began by itself), stays the turn; else the first carried input written
    /// into the turn becomes it, the other carried ones stay written. The turn's own input
    /// neither accepted nor carried goes back to the head of the queue, as does a written one
    /// in `dropped` (the runtime will not take it), in the order they were written (an input
    /// written past a waiting one stands before it), unless a Stop came since the last end
    /// (then it fails: the person cancelled it; after the owner's Stop (SPEC 0.231) it goes
    /// into the agent's history without a turn instead, [`Session::stopped_input`]). Any other
    /// written input stays written: it went into the turn the runtime runs next before this
    /// end reached the session.
    fn end_turn(&mut self, carried: &[Uuid], dropped: &[Uuid]) {
        let stopped = std::mem::take(&mut self.stop_asked);
        let quiet = std::mem::take(&mut self.stop_quiet);
        let mut back = Vec::new();
        let mut next = None;
        if let Some(t) = self.turn.take() {
            if !t.accepted && carried.contains(&t.input.event_id) {
                next = Some(t);
            } else if !t.accepted && stopped && dropped.contains(&t.input.event_id) {
                self.stopped_input(
                    quiet,
                    Queued {
                        frame_id: t.frame_id,
                        input: t.input,
                        requeued: t.requeued,
                        quiet: false,
                        note: false,
                    },
                );
            } else if !t.accepted {
                back.push(Queued {
                    frame_id: t.frame_id,
                    input: t.input,
                    requeued: t.requeued,
                    quiet: false,
                    note: false,
                });
            }
        }
        let mut still = Vec::new();
        for q in std::mem::take(&mut self.injected) {
            if dropped.contains(&q.input.event_id) && stopped {
                self.stopped_input(quiet, q);
            } else if dropped.contains(&q.input.event_id) {
                back.push(q);
            } else if !carried.contains(&q.input.event_id) {
                still.push(q);
            } else if next.is_none() {
                tracing::info!(session_id = %self.id, event_id = %q.input.event_id, "an input written during the turn starts the next one");
                self.link.mcp().set_turn(self.id, q.input.event_id);
                // A send written into the turn carries its cap: the owner's raise stays at it.
                let capped = capped(&q.input);
                next = Some(Turn {
                    frame_id: q.frame_id,
                    input: q.input,
                    accepted: false,
                    requeued: q.requeued,
                    capped,
                });
            } else {
                still.push(q);
            }
        }
        self.injected = still;
        self.turn = next;
        for q in back.into_iter().rev() {
            self.requeue(q);
        }
    }

    /// An input the runtime cancelled on a Stop (SPEC §17.2): after the owner's Stop (`quiet`,
    /// SPEC 0.231) it is written into the agent's history without a turn
    /// ([`Session::mark_quiet`]), after any other it fails instead of running again.
    fn stopped_input(&mut self, quiet: bool, q: Queued) {
        if quiet {
            self.mark_quiet(q);
        } else {
            self.cancelled_by_stop(&q.frame_id, q.input.event_id);
        }
    }

    /// Marks input `q` of the owner's Stop (SPEC 0.231) to be written into the agent's history
    /// without a turn ([`Session::write_quiet`], in `event_id` order): the agent knows it next
    /// time and does not act on it now. A slash command fails: it runs or it does not, it is not
    /// text for the history. So does an input whose cap is below the session's own mode (T39):
    /// it was never handled under its cap, and the owner's next turn in the full mode would see
    /// it unhandled, so it is not written; the agent gets a note of its own after the inputs
    /// instead, naming the sender (the `peer` only) and not showing the text ([`peer::dropped_note`]).
    fn mark_quiet(&mut self, mut q: Queued) {
        // What the runtime gets decides: a message of another session that starts with a path
        // carries a header first and is no command.
        if crate::peer::runtime_input(&q.input)
            .text
            .trim_start()
            .starts_with('/')
        {
            self.cancelled_by_stop(&q.frame_id, q.input.event_id);
        } else if cap(&q.input) < self.spec.permission_mode {
            tracing::info!(session_id = %self.id, event_id = %q.input.event_id, "input dropped at stop: its cap is below the session's mode");
            self.reply_input(
                &q.frame_id,
                wire_error(
                    error_code::INTERNAL,
                    "dropped at Stop: it may not run in this session's permission mode".to_string(),
                    false,
                ),
            );
            self.queue.push_back(Queued {
                frame_id: String::new(),
                input: SessionInput {
                    session_id: self.id,
                    event_id: Uuid::now_v7(),
                    text: crate::peer::dropped_note(&q.input),
                    artifact_ids: vec![],
                    peer_prefix: vec![],
                    peer: None,
                    permission_cap: None,
                    schedule_id: None,
                    human: false,
                },
                requeued: 0,
                quiet: true,
                note: true,
            });
        } else {
            q.quiet = true;
            self.queue.push_back(q);
        }
    }

    /// Remembers that the runtime took input `event_id` as `taken`, to ack it again as it
    /// was when the hub resends it.
    fn remember_delivered(&mut self, event_id: Uuid, taken: Taken) {
        self.delivered.push_back((event_id, taken));
        if self.delivered.len() > DELIVERED_MEMORY {
            self.delivered.pop_front();
        }
    }

    /// A slash command that waited when the person pressed Stop (SPEC §17.2, 0.231): it fails
    /// instead of running later, since they wanted it gone.
    fn cancelled_by_stop(&self, frame_id: &str, event_id: Uuid) {
        tracing::info!(session_id = %self.id, %event_id, "input cancelled by stop");
        self.reply_input(
            frame_id,
            wire_error(error_code::INTERNAL, "cancelled by Stop".to_string(), false),
        );
    }

    /// Input `q` ended a turn without being taken: back to the head of the queue.
    fn requeue(&mut self, q: Queued) {
        if q.requeued >= MAX_REQUEUES {
            // The runtime keeps ending turns without taking this input: give it up instead of
            // resending it forever (the hub marks it failed after the retries of SPEC §5.2).
            tracing::error!(session_id = %self.id, event_id = %q.input.event_id, note = q.note, "input never taken by the runtime; giving up");
            if !q.note {
                self.reply_input(
                    &q.frame_id,
                    wire_error(
                        error_code::INTERNAL,
                        "the runtime ended its turns without taking this input".to_string(),
                        false,
                    ),
                );
            }
            return;
        }
        tracing::warn!(session_id = %self.id, event_id = %q.input.event_id, "turn ended before the input was accepted; requeued");
        self.queue.push_front(Queued {
            requeued: q.requeued + 1,
            ..q
        });
    }

    async fn ensure_running(&mut self) -> anyhow::Result<()> {
        if self.slot.is_none() {
            match self.slots.clone().try_acquire_owned() {
                Ok(p) => self.slot = Some(p),
                // Not a failed start (#41): the session waits in the line for a slot and starts
                // when another agent stops (`run`), its inputs held here meanwhile.
                Err(_) => {
                    if !self.waiting_slot {
                        self.waiting_slot = true;
                        tracing::info!(session_id = %self.id, max_sessions = self.max_sessions, "no free slot, the agent waits for one");
                        self.system_event(
                            &format!(
                                "{} agents already run on this worker (max_sessions): this session starts when one of them stops",
                                self.max_sessions
                            ),
                            WAITING_FOR_SLOT,
                        );
                    }
                    return Err(anyhow::Error::new(NoSlot));
                }
            }
        }
        self.take_move();
        self.set_state(ProcessState::Starting);
        if let Err(e) = self.relocate().await {
            tracing::warn!(session_id = %self.id, "agent start failed: {e:#}");
            self.set_state(ProcessState::Stopped);
            return Err(e);
        }
        // Its MCP server may connect as soon as the process runs (SPEC §7.2).
        self.link.mcp().bind(
            self.id,
            &self.spec.session_token,
            crate::mcp::attach::Folders {
                workspace: self.spec.workspace.clone(),
                workdir: self.spec.workdir.clone(),
            },
        );
        // `mode: resume` from the hub, or this process ran the agent before (kill, idle
        // timeout, crash). The Claude Code adapter checks for the transcript either way.
        let mode = self.start_mode();
        let mut spec = StartSpec {
            permission_mode: mode,
            ..self.spec.clone()
        };
        // A provider key kept in Vaultwarden is read at every start, into this copy only: a key
        // changed there is the next start's, and the session's own spec never holds it.
        if let Some(p) = spec.provider.as_mut() {
            if let Some(k) = p.api_key_vault.take() {
                tracing::info!(session_id = %self.id, provider = %p.name, entry = %k.entry, "reading a provider key from Vaultwarden");
                match crate::vault::read_for(&k.rbw, &k.entry, k.field.as_deref(), false).await {
                    Ok(v) => p.api_key = Some(crate::token::Secret::new(v)),
                    Err(e) => {
                        let e = anyhow::Error::new(ProviderKeyUnreadable(format!(
                            "the key of the provider {} could not be read from Vaultwarden: {e}",
                            p.name
                        )));
                        tracing::warn!(session_id = %self.id, "agent start failed: {e:#}");
                        self.set_state(ProcessState::Stopped);
                        return Err(e);
                    }
                }
            }
        }
        let r = if self.started_once || self.resume_requested {
            self.adapter.resume(&spec).await
        } else {
            self.adapter.start(&spec).await
        };
        match r {
            Ok(()) => {
                self.started_once = true;
                // What the process was started with, until it reports otherwise.
                self.permission_mode = Some(mode);
                self.asked_mode = mode;
                // Its MCP server is accepted only from inside this process (SPEC §7.2).
                self.link.mcp().set_pid(self.id, self.adapter.pid());
                self.set_state(ProcessState::Running);
                Ok(())
            }
            Err(e) => {
                tracing::warn!(session_id = %self.id, "agent start failed: {e:#}");
                self.set_state(ProcessState::Stopped);
                Err(e)
            }
        }
    }

    /// Hands the session to the other writer or holder (SPEC §9.3): the process stops at
    /// once, a turn in progress included. The other process continues the transcript from
    /// what it read when it opened it, so whatever this agent did afterwards would land on a
    /// branch nobody continues. Queued input stays `pending` in the hub, `session.state`
    /// says `locked_by: vscode`.
    async fn release(&mut self) {
        // One that waits for a slot leaves the line: no agent of hqc starts on a transcript
        // VS Code goes on with (#41).
        if self.waiting_slot {
            self.waiting_slot = false;
            self.released = true;
            self.queue.clear();
            tracing::info!(session_id = %self.id, "the session continues elsewhere (VS Code); it no longer waits for a slot");
            return;
        }
        if self.state == ProcessState::Stopped {
            return;
        }
        self.released = true;
        self.queue.clear();
        tracing::info!(session_id = %self.id, in_turn = self.turn.is_some(), "the session continues elsewhere (VS Code); stopping the agent");
        self.kill_process("continued in VS Code").await;
    }

    /// Nothing runs in the process: no turn (the runtime's own included), no subagent, no
    /// background task. Such a process may exit by itself ([`Adapter::close`]); any other is
    /// killed.
    fn quiet(&self) -> bool {
        self.turn.is_none()
            && !self.adapter.runs_own_turn()
            && self.subagents == 0
            && self.background_tasks == 0
    }

    async fn kill_process(&mut self, why: &str) {
        self.end_process(why, false).await;
    }

    /// [`Session::kill_process`] for a process with nothing running: it is let to exit by
    /// itself ([`Adapter::close`]).
    async fn close_process(&mut self, why: &str) {
        self.end_process(why, true).await;
    }

    async fn end_process(&mut self, why: &str, close: bool) {
        // A session that waits for a slot leaves the line: a restart that needs one again
        // joins it at its end.
        self.waiting_slot = false;
        if self.state == ProcessState::Stopped {
            return;
        }
        self.set_state(ProcessState::Stopping);
        let ended = if close {
            self.adapter.close().await
        } else {
            self.adapter.kill().await
        };
        if let Err(e) = ended {
            tracing::warn!(session_id = %self.id, "kill ({why}) failed: {e:#}");
        }
        if let Some(t) = self.turn.take() {
            tracing::info!(session_id = %self.id, event_id = %t.input.event_id, "turn lost: {why}");
        }
        // Not given to the runtime, or not taken: still `pending` in the hub, which sends
        // them again.
        self.lowering = None;
        self.injected.clear();
        self.quiet_out.clear();
        // The turn a Stop was meant for ended with the process.
        self.stop_asked = false;
        self.stop_quiet = false;
        self.set_state(ProcessState::Stopped);
    }

    /// Every queued input gets the start failure; the hub counts them (SPEC §5.2).
    fn fail_queue(&mut self, e: &anyhow::Error) {
        let transcript = self.transcript();
        for q in self.queue.drain(..).filter(|q| !q.note) {
            self.link
                .reply_input(self.id, transcript.as_deref(), &q.frame_id, start_failed(e));
        }
    }

    fn set_state(&mut self, s: ProcessState) {
        if s == ProcessState::Stopped {
            self.slot = None;
            // Its MCP server is gone with it; a call still open fails.
            self.link.mcp().unbind(self.id);
            // And its subagents, background tasks, foreground calls and wakeup; the next
            // process counts its own.
            self.subagents = 0;
            self.wake_at = None;
            self.live = None;
            // Its `session.state` counts nothing, which drops the hub's list.
            self.live_cleared = false;
            self.live_subagents(vec![]);
            self.background_tasks = 0;
            if self.token_rate != 0 {
                self.token_rate = 0;
                self.tokens_rate(0);
            }
            self.foreground_calls.clear();
            self.activity = None;
            for q in std::mem::take(&mut self.side_pending) {
                self.side_answer(q, None, Some(SIDE_PROCESS_ENDED));
            }
        }
        if self.state != s {
            self.state = s;
            self.send_state();
        }
    }

    fn send_state(&self) {
        self.link.send(WorkerFrame::SessionState {
            session_id: self.id,
            process_state: self.state,
            runtime_session_id: self.transcript(),
            locked_by: if self.released {
                LockedBy::Vscode
            } else if self.state == ProcessState::Stopped {
                LockedBy::None
            } else {
                LockedBy::Hqc
            },
            title: self.title.clone(),
            held_by: None,
            commands: self
                .commands
                .as_ref()
                .map(|c| c.iter().map(|c| c.name.clone()).collect()),
            command_info: self.commands.clone(),
            permission_mode: self.permission_mode,
            capped_input: self.capped_input(),
            subagents: self.subagents,
            background_tasks: self.background_tasks,
            foreground_calls: self.foreground_calls.clone(),
            activity: self.activity.clone(),
        });
    }

    /// The input of the current turn while its cap holds the session's own mode down (T39).
    fn capped_input(&self) -> Option<Uuid> {
        self.turn
            .as_ref()
            .filter(|t| t.capped && cap(&t.input) < self.spec.permission_mode)
            .map(|t| t.input.event_id)
    }

    /// `subagents.live` with `agents`: sent directly, never stored (SPEC §4).
    fn live_subagents(&self, agents: Vec<hqc_proto::event::LiveSubagent>) {
        let ev = crate::adapter::new_event(
            self.id,
            hqc_proto::event::Author::Agent { id: self.id },
            EventKind::SubagentsLive { agents },
        );
        if let Err(e) = self.link.event(ev) {
            tracing::error!(session_id = %self.id, "subagents.live not sent: {e:#}");
        }
    }

    /// `tokens.rate` with `output_per_s`: sent directly, never stored (SPEC §4).
    fn tokens_rate(&self, output_per_s: u32) {
        let ev = crate::adapter::new_event(
            self.id,
            hqc_proto::event::Author::Agent { id: self.id },
            EventKind::TokensRate { output_per_s },
        );
        if let Err(e) = self.link.event(ev) {
            tracing::error!(session_id = %self.id, "tokens.rate not sent: {e:#}");
        }
    }

    fn system_event(&self, text: &str, code: &str) {
        let ev = crate::adapter::new_event(
            self.id,
            hqc_proto::event::Author::System,
            EventKind::System {
                text: text.into(),
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
            },
        );
        if let Err(e) = self.link.event(ev) {
            tracing::error!(session_id = %self.id, "outbox write failed: {e:#}");
        }
    }
}

/// `system.code` of a turn that failed because the worker's Claude Code is signed out (SPEC
/// 0.185).
pub const CLAUDE_SIGNED_OUT: &str = "claude_signed_out";

/// The error of a side question whose process ended before it answered.
pub const SIDE_PROCESS_ENDED: &str = "The agent's process ended before the answer";

/// `system.code` of a session that waits for a slot (#41, SPEC §17.3).
pub const WAITING_FOR_SLOT: &str = "waiting_for_slot";

/// [`Session::ensure_running`] found no free slot: the session waits for one, which is no
/// failed start.
#[derive(Debug)]
struct NoSlot;

impl std::fmt::Display for NoSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("waiting for a free slot (max_sessions)")
    }
}

impl std::error::Error for NoSlot {}

/// The session waits for a slot: not a failure to report.
fn waits(e: &anyhow::Error) -> bool {
    e.is::<NoSlot>()
}

/// The key of the session's provider could not be read from Vaultwarden. Not retryable: a
/// pinentry window the read left open stays open on this machine, and the hub's retries (three,
/// seconds apart) would only stack more of them; the owner unlocks `rbw` and sends again.
#[derive(Debug)]
struct ProviderKeyUnreadable(String);

impl std::fmt::Display for ProviderKeyUnreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProviderKeyUnreadable {}

fn start_failed(e: &anyhow::Error) -> WorkerFrame {
    wire_error(
        error_code::AGENT_START_FAILED,
        format!("{e:#}"),
        !e.is::<ProviderKeyUnreadable>(),
    )
}

/// The most of a background task's output uploaded: 2 MB, what "Open" of a text shows in the
/// client (SPEC §8.1).
const MAX_TASK_OUTPUT: u64 = 2 * 1024 * 1024;

/// The output file `path` of a background task that ended ([`AdapterEvent::TaskOutput`]) as an
/// `attachment` of session `id` (SPEC §8.1): a regular file, opened as `hqc_attach` opens one
/// (no symlink in its last component), is uploaded as `hqc_attach` uploads, its last
/// [`MAX_TASK_OUTPUT`] bytes when it is larger (where a failed build says why; the CLI gives
/// the model the tail too), which the caption then says; the event is the agent's, `text`,
/// named as the file. A file that is not there, not regular or empty, or an upload the hub
/// refuses, gives no event.
async fn attach_task_output(link: &HubLink, artifacts: &ArtifactClient, id: Uuid, path: &Path) {
    use hqc_proto::event::{ArtifactKind, Author};
    use std::io::{Seek, SeekFrom};
    let (mut file, size) = match crate::mcp::attach::open_regular(path, u64::MAX) {
        Ok(f) => f,
        Err(e) => {
            tracing::info!(session_id = %id, path = %path.display(), "task output not attached: {}", e.message);
            return;
        }
    };
    if size == 0 {
        tracing::info!(session_id = %id, path = %path.display(), "task output empty, not attached");
        return;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let sent = size.min(MAX_TASK_OUTPUT);
    if let Err(e) = file.seek(SeekFrom::Start(size - sent)) {
        tracing::warn!(session_id = %id, path = %path.display(), "task output not attached: {e}");
        return;
    }
    let artifact_id = match artifacts
        .upload(id, file, sent, &name, Some(ArtifactKind::Text))
        .await
    {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(session_id = %id, path = %path.display(), code = %e.code, "task output upload: {}", e.message);
            return;
        }
    };
    tracing::info!(session_id = %id, path = %path.display(), %artifact_id, size, "task output attached");
    let ev = crate::adapter::new_event(
        id,
        Author::Agent { id },
        EventKind::Attachment {
            artifact_id,
            kind: ArtifactKind::Text,
            name,
            caption: (sent < size).then(|| format!("The last 2 MB of {size} bytes")),
        },
    );
    if let Err(e) = link.event(ev) {
        tracing::error!(session_id = %id, "outbox write failed: {e:#}");
    }
}

async fn sleep_until_opt(t: Option<Instant>) {
    match t {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}
