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
//! - While a turn runs, the head of the queue goes into it when the runtime can take it
//!   ([`Adapter::can_inject`]), neither it nor the turn is capped to `ask`, the turn's own
//!   input has no `peer`, and only a person's input (no `peer`, no `schedule_id`) that is no
//!   slash command: as the VS Code tab does with what the user types meanwhile.
//!   Its acceptance is `ack {event_id, joined: true}`, also when it is resent; the first
//!   input the turn carries to the next one becomes that turn, one the runtime will not take
//!   goes back to the head of the queue, as does every one not taken when the process exits.
//! - `subagents` (Claude Code: [`AdapterEvent::Subagents`]) and `background_tasks`
//!   ([`AdapterEvent::BackgroundTasks`]) are repeated in every `session.state` like `title`,
//!   0 when the agent is stopped. The subagents' list ([`AdapterEvent::LiveSubagents`]) goes
//!   as `subagents.live`, empty when the agent is stopped; the last one sent is sent again on
//!   every connect unless it was empty, its run times counted then.
//! - Every change of `process_state` is sent as `session.state`; all states are sent again
//!   on every connect.
//! - Permission mode (T38): `session.start.permission_mode` and `session.permission_mode`
//!   go through [`Config::permission_mode`] (`auto` only with `allow_auto`); a running agent
//!   switches at once, a stopped one starts in the mode. The mode the runtime confirms is
//!   sent in `session.state.permission_mode`, repeated like `title`.
//! - A cap of `ask` on an input (`session.input.permission_cap`, T39): the agent runs that
//!   turn in `ask` whatever the session's mode. A running process is switched down and the
//!   input waits for the runtime's confirmation ([`MODE_SWITCH_TIMEOUT`]; without it, or if
//!   the runtime cannot switch, the process is restarted in `ask`); a process started for a
//!   capped input starts in `ask`. When the turn ends (also when it is stopped), the
//!   session's own mode comes back unless the next input is capped too; a process lost
//!   mid-turn starts again in the mode of the input it runs next. A cap never raises the
//!   mode, and a switch the user asks for meanwhile waits for the end of the capped turn.
//!   `session.state.capped_input` names the input while a cap holds an `auto` session in
//!   `ask`.
//! - Model: `session.model` switches a running agent at once, mid-turn too
//!   ([`Adapter::set_model`]); `null` (the runtime's default) reaches it at its next start,
//!   as any model reaches a stopped one. A `session.start` for a running agent with another
//!   model does the same.
//! - `idle_timeout` (SPEC §17.3): an idle process (no turn, no subagent running, no
//!   background task the runtime lists: [`AdapterEvent::BackgroundTasks`]) is killed, the
//!   next input resumes it. A reconnect and a holder report (`Cmd::Resync`, `Cmd::Held`) are
//!   no activity of the agent.
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

use hqc_proto::event::EventKind;
use hqc_proto::frame::{
    error_code, worker_approval, Ack, Envelope, HeldBy, HubFrame, LockedBy, PermissionMode,
    ProcessState, RequestResult, ResultBody, Runtime, SessionInput, SessionStart, StartMode,
    TitleRequest, TitleResult, WireError, WorkerApprovalRequest, WorkerFrame,
};
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::adapter::{Adapter, AdapterEvent, AdapterFactory};
use crate::config::{Config, StartSpec};
use crate::inbox::{self, ArtifactClient, InboxFile};
use crate::known_paths::KnownPaths;
use crate::link::HubLink;
use crate::permissions::FolderRules;

/// An input that ends this many turns without being taken is given up (see `end_turn`).
const MAX_REQUEUES: u8 = 2;
/// How long a capped input waits for the runtime to confirm `ask` before the process is
/// restarted in `ask` (T39).
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

fn ack(event_id: Option<Uuid>) -> WorkerFrame {
    WorkerFrame::Ack(Ack {
        event_id,
        seq: None,
        joined: false,
    })
}

/// `ack {event_id, joined: true}`: the input went into the turn already running.
fn joined_ack(event_id: Uuid) -> WorkerFrame {
    WorkerFrame::Ack(Ack {
        event_id: Some(event_id),
        seq: None,
        joined: true,
    })
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
    /// `runtime_session_id` of the last `session.start`: the transcript the session runs.
    runtime_session_id: Option<String>,
    /// The workspace of the last `session.start` the session took: another one is a move.
    workspace: PathBuf,
}

#[derive(Debug)]
enum Cmd {
    Start {
        id: String,
        spec: Box<StartSpec>,
        mode: StartMode,
    },
    Input {
        id: String,
        input: SessionInput,
    },
    Stop {
        id: String,
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
    },
    Resync,
    /// Another writer continues the transcript: stop after the current turn (SPEC §9.3).
    Release,
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
        }
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
                let rid = runtime_session_id.to_string();
                match self
                    .sessions
                    .iter()
                    .find(|(_, h)| h.runtime_session_id.as_deref() == Some(rid.as_str()))
                {
                    Some((sid, h)) => {
                        tracing::info!(session_id = %sid, runtime_session_id = %rid, "session continued elsewhere; releasing it now");
                        let _ = h.tx.send(Cmd::Release);
                    }
                    None => {
                        tracing::debug!(runtime_session_id = %rid, "foreign writer for a session not run here")
                    }
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
                    h.runtime_session_id = spec.runtime_session_id.clone();
                    h.workspace = spec.workspace.clone();
                }
                (
                    sid,
                    Cmd::Start {
                        id,
                        spec: Box::new(spec),
                        mode: s.mode,
                    },
                )
            }
            HubFrame::SessionInput(input) => (input.session_id, Cmd::Input { id, input }),
            HubFrame::SessionStop { session_id } => (session_id, Cmd::Stop { id }),
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
            } => (
                session_id,
                Cmd::Clarify {
                    id,
                    request_id,
                    answers,
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
                self.link.reply(
                    &id_of(&cmd),
                    wire_error(
                        error_code::UNKNOWN_SESSION,
                        "session is not started on this worker; send session.start first",
                        matches!(cmd, Cmd::Input { .. }),
                    ),
                );
            }
        }
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
            state: ProcessState::Stopped,
            runtime_session_id: None,
            title: None,
            commands: None,
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
            background_tasks: 0,
            stop_asked: false,
            stopped_by_move: 0,
            delivered: VecDeque::new(),
            last_activity: Instant::now(),
        };
        SessionHandle {
            tx,
            task: tokio::spawn(s.run(rx)),
            runtime_session_id: spec.runtime_session_id.clone(),
            workspace: spec.workspace.clone(),
        }
    }
}

fn id_of(c: &Cmd) -> String {
    match c {
        Cmd::Start { id, .. }
        | Cmd::Input { id, .. }
        | Cmd::Stop { id }
        | Cmd::Kill { id }
        | Cmd::PermissionMode { id, .. }
        | Cmd::Model { id, .. }
        | Cmd::Approval { id, .. }
        | Cmd::Clarify { id, .. } => id.clone(),
        Cmd::Resync | Cmd::Release | Cmd::Held { .. } | Cmd::Shutdown => String::new(),
    }
}

struct Queued {
    /// Id of the latest `session.input` frame carrying this input; the ack answers it.
    frame_id: String,
    input: SessionInput,
    /// Times this input ended a turn without being taken and went back to the queue.
    requeued: u8,
}

struct Turn {
    frame_id: String,
    input: SessionInput,
    accepted: bool,
    requeued: u8,
    /// The input's cap is `ask` (T39): the runtime confirmed `ask` before it got it, and the
    /// session's own mode waits for the end of the turn.
    capped: bool,
}

/// A capped input waiting for the runtime to confirm `ask` (T39), ready to hand over.
struct Lowering {
    q: Queued,
    input: SessionInput,
    files: Vec<InboxFile>,
    deadline: Instant,
}

/// The input must run in `ask` (T39).
fn capped(input: &SessionInput) -> bool {
    input.permission_cap == Some(PermissionMode::Ask)
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
    state: ProcessState,
    runtime_session_id: Option<String>,
    /// The runtime's title of the session, repeated in every `session.state`.
    title: Option<String>,
    /// The runtime's slash commands, repeated in every `session.state` like `title`.
    commands: Option<Vec<String>>,
    /// The permission mode the agent runs in (T38): the one it was started or switched
    /// with, then what the runtime reports; repeated in every `session.state`.
    permission_mode: Option<PermissionMode>,
    /// The mode last asked of the running process: its start flag or our last switch. The
    /// session's own mode is `spec.permission_mode`; they differ under a cap (T39).
    asked_mode: PermissionMode,
    /// A `session.start` with another folder that came while the agent ran (a move, SPEC
    /// §5.3): the next process starts with it.
    moving: Option<Box<StartSpec>>,
    /// A capped input waiting for the runtime's `ask` (T39).
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
    /// The background tasks the runtime lists ([`AdapterEvent::BackgroundTasks`]), repeated
    /// in every `session.state`.
    background_tasks: u32,
    /// A `session.stop` came since the last end of a turn: an input the runtime drops then
    /// was cancelled by the person, not lost ([`Session::end_turn`]).
    stop_asked: bool,
    /// The background tasks a move stopped with the process; told to the agent with its input
    /// until the runtime takes one ([`Session::prepared`]).
    stopped_by_move: u32,
    /// Inputs the runtime took, with whether it took them into a running turn (`joined`).
    delivered: VecDeque<(Uuid, bool)>,
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
                _ = sleep_until_opt(idle_deadline), if idle_deadline.is_some() => {
                    tracing::info!(session_id = %self.id, "idle timeout, stopping the agent");
                    self.kill_process("idle timeout").await;
                }
                _ = sleep_until_opt(lowering_deadline), if lowering_deadline.is_some() => {
                    self.restart_lowered("the runtime did not confirm ask in time").await;
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
        // build).
        (self.state == ProcessState::Running
            && self.turn.is_none()
            && self.injected.is_empty()
            && self.lowering.is_none()
            && self.queue.is_empty()
            && self.subagents == 0
            && self.background_tasks == 0)
            .then(|| self.last_activity + t)
    }

    async fn on_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::Start { id, spec, mode } => {
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
                match self.ensure_running().await {
                    Ok(()) => {
                        self.link.reply(&id, ack(None));
                        self.pump().await;
                    }
                    Err(e) => {
                        self.link.reply(&id, start_failed(&e));
                        self.fail_queue(&e);
                    }
                }
            }
            Cmd::Input { id, input } => self.on_input(id, input).await,
            Cmd::Stop { id } => {
                // Also with no turn of ours: the runtime may run one it started by itself
                // (Claude Code after a background task's end); the adapter knows.
                if self.state == ProcessState::Running {
                    self.stop_asked = true;
                    if let Err(e) = self.adapter.stop().await {
                        tracing::warn!(session_id = %self.id, "stop failed: {e:#}");
                        self.link.reply(
                            &id,
                            wire_error(error_code::INTERNAL, format!("{e:#}"), true),
                        );
                        return;
                    }
                }
                self.link.reply(&id, ack(None));
            }
            Cmd::Kill { id } => {
                // Queued inputs stay `pending` in the hub and come again after `hello`.
                self.queue.clear();
                self.kill_process("session.kill").await;
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
            } => {
                let r = self.adapter.answer_clarify(&request_id, &answers).await;
                self.reply_result(&id, r);
                self.pump().await;
            }
            Cmd::Resync => {
                self.send_state();
                // A list sent while disconnected was lost; the next one may come only with
                // the subagent's next step.
                if let Some((agents, at)) = &self.live {
                    let age = u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX);
                    let mut agents = agents.clone();
                    for a in &mut agents {
                        a.duration_ms = a.duration_ms.saturating_add(age);
                    }
                    self.live_subagents(agents);
                }
            }
            Cmd::Release => self.release().await,
            Cmd::Held { runtime_session_id } => {
                let rid = runtime_session_id.to_string();
                let mine = self
                    .runtime_session_id
                    .as_deref()
                    .or(self.spec.runtime_session_id.as_deref())
                    == Some(rid.as_str());
                if mine && self.state != ProcessState::Stopped {
                    tracing::info!(session_id = %self.id, runtime_session_id = %rid, "the session was opened outside hqc; releasing it now");
                    self.release().await;
                }
            }
            Cmd::Shutdown => unreachable!("handled in run"),
        }
    }

    /// Run in `mode` from now on (T38): the next start takes it from the spec, a running
    /// agent switches at once, mid-turn too (the runtime applies it to its next permission
    /// check). A runtime that refuses keeps its mode until its next start; `session.state`
    /// says which mode it runs.
    ///
    /// Under a cap (T39) the runtime stays in `ask`: the new mode comes when the capped turn
    /// ends.
    async fn switch_mode(&mut self, mode: PermissionMode) {
        if self.spec.permission_mode == mode {
            return;
        }
        tracing::info!(session_id = %self.id, mode = mode.as_str(), "permission mode switched");
        self.spec.permission_mode = mode;
        if self.state == ProcessState::Stopped {
            return;
        }
        if self.under_cap() {
            tracing::info!(session_id = %self.id, "the turn runs under a cap; the mode follows when it ends");
            // Whether the cap still holds the session down changed.
            self.send_state();
            return;
        }
        self.ask_runtime(mode).await;
    }

    /// Run `model` from now on: the next start takes it from the spec, a running agent
    /// switches at once, mid-turn too. `None`, the runtime's default, has no name to switch
    /// to: a running agent takes it at its next start. A runtime that refuses keeps its model
    /// until then too.
    async fn switch_model(&mut self, model: Option<String>) {
        if self.spec.model == model {
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
    /// subagent, nothing is written into a turn and no capped input waits for `ask` (T39); the
    /// end of a turn calls this again. Not the end of a background subagent: the runtime then
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
            || self.lowering.is_some()
        {
            return;
        }
        self.stopped_by_move += self.background_tasks;
        // A Stop the old process got cancels nothing the new one drops (`end_turn`).
        self.stop_asked = false;
        self.kill_process("moved to another folder").await;
        if let Err(e) = self.ensure_running().await {
            self.fail_queue(&e);
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

    /// A capped turn runs or waits for the runtime's `ask` (T39).
    fn under_cap(&self) -> bool {
        self.lowering.is_some() || self.turn.as_ref().is_some_and(|t| t.capped)
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
            self.link.reply(
                &id,
                wire_error(
                    LOCKED_VSCODE,
                    "the session continues in VS Code; take it over first",
                    false,
                ),
            );
            return;
        }
        if let Some(&(_, joined)) = self.delivered.iter().find(|(e, _)| *e == eid) {
            // The same ack again: a plain one for a joined input would open a turn in the hub
            // that nothing closes.
            let a = if joined {
                joined_ack(eid)
            } else {
                ack(Some(eid))
            };
            self.link.reply(&id, a);
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
        });
        if self.state == ProcessState::Stopped {
            if let Err(e) = self.ensure_running().await {
                self.fail_queue(&e);
                return;
            }
        }
        self.pump().await;
    }

    /// Hands the next queued input to the adapter if no turn is running, or writes it into
    /// the running turn ([`Session::inject_head`]). While a move waits the input is for the
    /// agent in the new folder: it waits as long as something holds the move, then the move
    /// goes first ([`Session::restart_for_move`]). A capped input first waits for the runtime
    /// to confirm `ask` (T39); an uncapped one runs in the session's own mode.
    async fn pump(&mut self) {
        while self.lowering.is_none() && self.state == ProcessState::Running {
            if self.turn.is_some() {
                if !self.inject_head().await {
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
            let Some(q) = self.queue.pop_front() else {
                return;
            };
            let Some((input, files)) = self.prepared(&q).await else {
                continue;
            };
            if !capped(&q.input) {
                // After a capped turn whose next input was capped too (T39).
                let own = self.spec.permission_mode;
                self.ask_runtime(own).await;
            } else if !(self.asked_mode == PermissionMode::Ask
                && self.permission_mode == Some(PermissionMode::Ask))
            {
                tracing::info!(session_id = %self.id, event_id = %q.input.event_id, "capped input: switching the agent to ask first");
                if !self.ask_runtime(PermissionMode::Ask).await {
                    // It cannot switch: a new process in `ask` runs it.
                    self.queue.push_front(q);
                    self.restart_for_cap("the runtime cannot switch to ask")
                        .await;
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
                self.link.reply(
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

    /// Writes the head of the queue into the running turn (SPEC §17.2), as the VS Code tab
    /// does with what the user types meanwhile: a person's input (no `peer`, no
    /// `schedule_id`) that is no slash command, into a turn whose own input has no `peer`
    /// (an `hqc_ask` is answered by its own turn alone), neither of them capped to `ask`
    /// (T39), the runtime asks nothing now and no move waits (the input is for the agent in
    /// the new folder). Only the head goes, so `seq` order holds.
    /// False: the head waits for the end of the turn.
    async fn inject_head(&mut self) -> bool {
        let fits = |q: &Queued| {
            q.input.peer.is_none()
                && q.input.schedule_id.is_none()
                && !q.input.text.trim_start().starts_with('/')
                && !capped(&q.input)
        };
        if !self.queue.front().is_some_and(fits)
            || self.moving.is_some()
            || self
                .turn
                .as_ref()
                .is_some_and(|t| t.capped || t.input.peer.is_some())
            || !self.adapter.can_inject()
        {
            return false;
        }
        let Some(q) = self.queue.pop_front() else {
            return false;
        };
        let Some((input, files)) = self.prepared(&q).await else {
            return true;
        };
        match self.adapter.inject_with(&input, &files).await {
            Ok(true) => {
                tracing::info!(session_id = %self.id, event_id = %q.input.event_id, "input written into the running turn");
                self.injected.push(q);
                true
            }
            // The turn ended meanwhile: the input goes once its end is handled.
            Ok(false) => {
                self.queue.push_front(q);
                false
            }
            Err(e) => {
                tracing::warn!(session_id = %self.id, event_id = %q.input.event_id, "input failed: {e:#}");
                self.link.reply(
                    &q.frame_id,
                    wire_error(error_code::INTERNAL, format!("{e:#}"), true),
                );
                true
            }
        }
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
                self.link.reply(
                    &q.frame_id,
                    wire_error(error_code::INTERNAL, format!("{e:#}"), true),
                );
            }
        }
    }

    /// The runtime did not confirm `ask` for the capped input that waits (T39): the input
    /// goes back to the head of the queue and a new process in `ask` runs it.
    async fn restart_lowered(&mut self, why: &str) {
        let Some(l) = self.lowering.take() else {
            return;
        };
        self.queue.push_front(l.q);
        self.restart_for_cap(why).await;
        self.pump().await;
    }

    /// Kills the process and starts it again for the capped input at the head of the
    /// queue, which makes it start in `ask` ([`Session::start_mode`]).
    async fn restart_for_cap(&mut self, why: &str) {
        tracing::warn!(session_id = %self.id, "restarting the agent in ask for a capped input: {why}");
        self.kill_process("permission cap").await;
        if let Err(e) = self.ensure_running().await {
            self.fail_queue(&e);
        }
    }

    /// The mode a new process starts in: `ask` for a capped input at the head of the queue
    /// (T39), otherwise the session's own.
    fn start_mode(&self) -> PermissionMode {
        if self.queue.front().is_some_and(|q| capped(&q.input)) {
            PermissionMode::Ask
        } else {
            self.spec.permission_mode
        }
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

    async fn on_event(&mut self, ev: AdapterEvent) {
        match ev {
            AdapterEvent::Event(e) => {
                if e.session_id != self.id {
                    tracing::warn!(session_id = %self.id, other = %e.session_id, "adapter event for another session dropped");
                    return;
                }
                if let Err(err) = self.link.event(e) {
                    tracing::error!(session_id = %self.id, "outbox write failed, event lost: {err:#}");
                }
            }
            AdapterEvent::InputAccepted { event_id } => {
                let (joined, slash) = if let Some(t) =
                    self.turn.as_mut().filter(|t| t.input.event_id == event_id)
                {
                    t.accepted = true;
                    self.link.reply(&t.frame_id, ack(Some(event_id)));
                    (false, t.input.text.starts_with('/'))
                } else if let Some(i) = self
                    .injected
                    .iter()
                    .position(|q| q.input.event_id == event_id)
                {
                    // Taken into the running turn: delivered, no turn of its own.
                    let q = self.injected.remove(i);
                    self.link.reply(&q.frame_id, joined_ack(event_id));
                    (true, q.input.text.starts_with('/'))
                } else {
                    tracing::warn!(session_id = %self.id, %event_id, "adapter accepted an input that is not in flight");
                    return;
                };
                // It carried the note of a move that stopped background tasks (`prepared`).
                if !slash {
                    self.stopped_by_move = 0;
                }
                self.delivered.push_back((event_id, joined));
                if self.delivered.len() > DELIVERED_MEMORY {
                    self.delivered.pop_front();
                }
            }
            AdapterEvent::TurnEnded { carried, dropped } => {
                let capped = self.turn.as_ref().is_some_and(|t| t.capped);
                self.end_turn(&carried, &dropped);
                // A capped input the runtime carries is still the turn: its cap holds.
                if capped && !self.turn.as_ref().is_some_and(|t| t.capped) {
                    self.after_capped_turn().await;
                    self.send_state();
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
            AdapterEvent::Limits(l) => self.link.send(WorkerFrame::WorkerLimits(l)),
            AdapterEvent::RuntimeSessionId(r) => {
                self.runtime_session_id = Some(r);
                self.send_state();
            }
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
                if m == PermissionMode::Ask && self.asked_mode == PermissionMode::Ask {
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
                self.end_turn(&[], &lost);
                if let Some(l) = self.lowering.take() {
                    // It never reached the runtime; the next process starts in `ask` for it.
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
    /// in `dropped` (the runtime will not take it), in `seq` order, unless a Stop came since
    /// the last end (then it fails: the person cancelled it). Any other written input
    /// stays written: it went into the turn the runtime runs next before this end reached
    /// the session.
    fn end_turn(&mut self, carried: &[Uuid], dropped: &[Uuid]) {
        let stopped = std::mem::take(&mut self.stop_asked);
        let mut back = Vec::new();
        let mut next = None;
        if let Some(t) = self.turn.take() {
            if !t.accepted && carried.contains(&t.input.event_id) {
                next = Some(t);
            } else if !t.accepted && stopped && dropped.contains(&t.input.event_id) {
                self.cancelled_by_stop(&t.frame_id, t.input.event_id);
            } else if !t.accepted {
                back.push(Queued {
                    frame_id: t.frame_id,
                    input: t.input,
                    requeued: t.requeued,
                });
            }
        }
        let mut still = Vec::new();
        for q in std::mem::take(&mut self.injected) {
            if dropped.contains(&q.input.event_id) && stopped {
                self.cancelled_by_stop(&q.frame_id, q.input.event_id);
            } else if dropped.contains(&q.input.event_id) {
                back.push(q);
            } else if !carried.contains(&q.input.event_id) {
                still.push(q);
            } else if next.is_none() {
                tracing::info!(session_id = %self.id, event_id = %q.input.event_id, "an input written during the turn starts the next one");
                self.link.mcp().set_turn(self.id, q.input.event_id);
                next = Some(Turn {
                    frame_id: q.frame_id,
                    input: q.input,
                    accepted: false,
                    requeued: q.requeued,
                    capped: false,
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

    /// An input the runtime cancelled on the person's Stop (SPEC §17.2): it fails instead of
    /// running again, since they wanted it gone.
    fn cancelled_by_stop(&self, frame_id: &str, event_id: Uuid) {
        tracing::info!(session_id = %self.id, %event_id, "input cancelled by stop");
        self.link.reply(
            frame_id,
            wire_error(error_code::INTERNAL, "cancelled by Stop".to_string(), false),
        );
    }

    /// Input `q` ended a turn without being taken: back to the head of the queue.
    fn requeue(&mut self, q: Queued) {
        if q.requeued >= MAX_REQUEUES {
            // The runtime keeps ending turns without taking this input: give it up instead of
            // resending it forever (the hub marks it failed after the retries of SPEC §5.2).
            tracing::error!(session_id = %self.id, event_id = %q.input.event_id, "input never taken by the runtime; giving up");
            self.link.reply(
                &q.frame_id,
                wire_error(
                    error_code::INTERNAL,
                    "the runtime ended its turns without taking this input".to_string(),
                    false,
                ),
            );
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
            // Retryable for the hub: a slot frees up when another agent stops.
            match self.slots.clone().try_acquire_owned() {
                Ok(p) => self.slot = Some(p),
                Err(_) => anyhow::bail!(
                    "{} agents already run on this worker (max_sessions)",
                    self.max_sessions
                ),
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
        let spec = StartSpec {
            permission_mode: mode,
            ..self.spec.clone()
        };
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
        if self.state == ProcessState::Stopped {
            return;
        }
        self.released = true;
        self.queue.clear();
        tracing::info!(session_id = %self.id, in_turn = self.turn.is_some(), "the session continues elsewhere (VS Code); stopping the agent");
        self.kill_process("continued in VS Code").await;
    }

    async fn kill_process(&mut self, why: &str) {
        if self.state == ProcessState::Stopped {
            return;
        }
        self.set_state(ProcessState::Stopping);
        if let Err(e) = self.adapter.kill().await {
            tracing::warn!(session_id = %self.id, "kill ({why}) failed: {e:#}");
        }
        if let Some(t) = self.turn.take() {
            tracing::info!(session_id = %self.id, event_id = %t.input.event_id, "turn lost: {why}");
        }
        // Not given to the runtime, or not taken: still `pending` in the hub, which sends
        // them again.
        self.lowering = None;
        self.injected.clear();
        self.set_state(ProcessState::Stopped);
    }

    /// Every queued input gets the start failure; the hub counts them (SPEC §5.2).
    fn fail_queue(&mut self, e: &anyhow::Error) {
        for q in self.queue.drain(..) {
            self.link.reply(&q.frame_id, start_failed(e));
        }
    }

    fn set_state(&mut self, s: ProcessState) {
        if s == ProcessState::Stopped {
            self.slot = None;
            // Its MCP server is gone with it; a call still open fails.
            self.link.mcp().unbind(self.id);
            // And its subagents and background tasks; the next process counts its own.
            self.subagents = 0;
            self.live = None;
            self.live_subagents(vec![]);
            self.background_tasks = 0;
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
            runtime_session_id: self
                .runtime_session_id
                .clone()
                .or_else(|| self.spec.runtime_session_id.clone()),
            locked_by: if self.released {
                LockedBy::Vscode
            } else if self.state == ProcessState::Stopped {
                LockedBy::None
            } else {
                LockedBy::Hqc
            },
            title: self.title.clone(),
            held_by: None,
            commands: self.commands.clone(),
            permission_mode: self.permission_mode,
            capped_input: self.capped_input(),
            subagents: self.subagents,
            background_tasks: self.background_tasks,
        });
    }

    /// The input of the current turn while its cap holds a session of its own in `auto`
    /// down to `ask` (T39).
    fn capped_input(&self) -> Option<Uuid> {
        self.turn
            .as_ref()
            .filter(|t| t.capped && self.spec.permission_mode == PermissionMode::Auto)
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

    fn system_event(&self, text: &str, code: &str) {
        let ev = crate::adapter::new_event(
            self.id,
            hqc_proto::event::Author::System,
            EventKind::System {
                text: text.into(),
                code: code.into(),
                fork: None,
            },
        );
        if let Err(e) = self.link.event(ev) {
            tracing::error!(session_id = %self.id, "outbox write failed: {e:#}");
        }
    }
}

fn start_failed(e: &anyhow::Error) -> WorkerFrame {
    wire_error(error_code::AGENT_START_FAILED, format!("{e:#}"), true)
}

async fn sleep_until_opt(t: Option<Instant>) {
    match t {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}
