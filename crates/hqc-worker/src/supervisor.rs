//! Session supervisor (SPEC §5.3, §17.2, §17.3).
//!
//! One task per session owns its adapter, its input queue and its `process_state`.
//! - `session.start` is checked against the local config first; a mismatch is answered
//!   with `error {invalid_frame}` and nothing is created or spawned (SPEC §2.1, §14).
//!   A failed spawn is `error {agent_start_failed, retryable: true}` (SPEC §5.2).
//! - `session.input` is queued; one input per turn, the next one waits for
//!   [`AdapterEvent::TurnEnded`] (SPEC §17.2). Its `ack {event_id}` is sent only when the
//!   adapter reports [`AdapterEvent::InputAccepted`] (SPEC §5.2). The queue lives in memory:
//!   the hub's `pending` inputs are the source of truth and come again after `hello`, so an
//!   input already queued or delivered is recognised by `event_id` and not run twice.
//! - Every change of `process_state` is sent as `session.state`; all states are sent again
//!   on every connect.
//! - `idle_timeout` (SPEC §17.3): an idle process is killed, the next input resumes it.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use hqc_proto::event::EventKind;
use hqc_proto::frame::{
    error_code, Ack, Envelope, HubFrame, LockedBy, ProcessState, SessionInput, WireError,
    WorkerFrame,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::adapter::{Adapter, AdapterEvent, AdapterFactory};
use crate::config::{Config, StartSpec};
use crate::link::HubLink;

/// From the connection to the supervisor.
// Almost every value is a `Frame`; boxing it would only add an allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Inbound {
    Frame(Envelope<HubFrame>),
    /// `hello.ack` received: resend every `session.state`.
    Connected,
}

/// How many delivered input ids a session remembers to re-ack resent inputs.
const DELIVERED_MEMORY: usize = 256;
const ADAPTER_EVENT_BUFFER: usize = 1024;

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
    })
}

pub struct Supervisor {
    cfg: Arc<Config>,
    factory: Arc<dyn AdapterFactory>,
    link: HubLink,
    sessions: HashMap<Uuid, SessionHandle>,
    shutdown_timeout: Duration,
}

struct SessionHandle {
    tx: mpsc::UnboundedSender<Cmd>,
    task: JoinHandle<()>,
}

#[derive(Debug)]
enum Cmd {
    Start {
        id: String,
        spec: Box<StartSpec>,
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
    Approval {
        id: String,
        request_id: String,
        option_id: String,
    },
    Clarify {
        id: String,
        request_id: String,
        answers: std::collections::BTreeMap<String, hqc_proto::event::ClarifyAnswer>,
    },
    Resync,
    Shutdown,
}

impl Supervisor {
    pub fn new(
        cfg: Arc<Config>,
        factory: Arc<dyn AdapterFactory>,
        link: HubLink,
        shutdown_timeout: Duration,
    ) -> Self {
        Self {
            cfg,
            factory,
            link,
            sessions: HashMap::new(),
            shutdown_timeout,
        }
    }

    /// Runs until `shutdown`, then kills every agent (SPEC §17.3: on worker restart all
    /// agents are `stopped`) and returns once their final events are in the outbox.
    pub async fn run(mut self, mut rx: mpsc::Receiver<Inbound>, shutdown: CancellationToken) {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                m = rx.recv() => match m {
                    Some(m) => self.handle(m),
                    None => break,
                },
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
                return;
            }
            Inbound::Frame(env) => env,
        };
        let id = env.id;
        let (session_id, cmd) = match env.body {
            HubFrame::SessionStart(s) => {
                let spec = match self.cfg.check_start(&s) {
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
                if !self.sessions.contains_key(&sid) {
                    let h = self.spawn_session(&spec);
                    self.sessions.insert(sid, h);
                }
                (
                    sid,
                    Cmd::Start {
                        id,
                        spec: Box::new(spec),
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
            HubFrame::ApprovalResponse {
                session_id,
                request_id,
                option_id,
            } => (
                session_id,
                Cmd::Approval {
                    id,
                    request_id,
                    option_id,
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
            idle_timeout: self.cfg.idle_timeout,
            state: ProcessState::Stopped,
            runtime_session_id: None,
            started_once: false,
            queue: VecDeque::new(),
            turn: None,
            delivered: VecDeque::new(),
            last_activity: Instant::now(),
        };
        SessionHandle {
            tx,
            task: tokio::spawn(s.run(rx)),
        }
    }
}

fn id_of(c: &Cmd) -> String {
    match c {
        Cmd::Start { id, .. }
        | Cmd::Input { id, .. }
        | Cmd::Stop { id }
        | Cmd::Kill { id }
        | Cmd::Approval { id, .. }
        | Cmd::Clarify { id, .. } => id.clone(),
        Cmd::Resync | Cmd::Shutdown => String::new(),
    }
}

struct Queued {
    /// Id of the latest `session.input` frame carrying this input; the ack answers it.
    frame_id: String,
    input: SessionInput,
}

struct Turn {
    frame_id: String,
    input: SessionInput,
    accepted: bool,
}

struct Session {
    id: Uuid,
    spec: StartSpec,
    adapter: Box<dyn Adapter>,
    events: mpsc::Receiver<AdapterEvent>,
    events_open: bool,
    link: HubLink,
    idle_timeout: Option<Duration>,
    state: ProcessState,
    runtime_session_id: Option<String>,
    started_once: bool,
    queue: VecDeque<Queued>,
    turn: Option<Turn>,
    delivered: VecDeque<Uuid>,
    last_activity: Instant,
}

impl Session {
    async fn run(mut self, mut cmds: mpsc::UnboundedReceiver<Cmd>) {
        loop {
            let idle_deadline = self.idle_deadline();
            tokio::select! {
                c = cmds.recv() => {
                    self.last_activity = Instant::now();
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
            }
        }
        // Events the adapter emitted while being killed.
        while let Ok(ev) = self.events.try_recv() {
            self.on_event(ev).await;
        }
    }

    fn idle_deadline(&self) -> Option<Instant> {
        let t = self.idle_timeout?;
        (self.state == ProcessState::Running && self.turn.is_none() && self.queue.is_empty())
            .then(|| self.last_activity + t)
    }

    async fn on_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::Start { id, spec } => {
                if self.state != ProcessState::Stopped {
                    if *spec != self.spec {
                        tracing::warn!(session_id = %self.id, "session.start for a running agent; keeping the running one");
                    }
                    self.link.reply(&id, ack(None));
                    return;
                }
                self.spec = *spec;
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
                if self.turn.is_some() {
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
            Cmd::Approval {
                id,
                request_id,
                option_id,
            } => {
                let r = self.adapter.answer_approval(&request_id, &option_id).await;
                self.reply_result(&id, r);
            }
            Cmd::Clarify {
                id,
                request_id,
                answers,
            } => {
                let r = self.adapter.answer_clarify(&request_id, &answers).await;
                self.reply_result(&id, r);
            }
            Cmd::Resync => self.send_state(),
            Cmd::Shutdown => unreachable!("handled in run"),
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
        if self.delivered.contains(&eid) {
            self.link.reply(&id, ack(Some(eid)));
            return;
        }
        if let Some(t) = self.turn.as_mut().filter(|t| t.input.event_id == eid) {
            // Resent after a reconnect while still in flight; answer the new frame.
            t.frame_id = id;
            return;
        }
        if let Some(q) = self.queue.iter_mut().find(|q| q.input.event_id == eid) {
            q.frame_id = id;
            return;
        }
        self.queue.push_back(Queued {
            frame_id: id,
            input,
        });
        if self.state == ProcessState::Stopped {
            if let Err(e) = self.ensure_running().await {
                self.fail_queue(&e);
                return;
            }
        }
        self.pump().await;
    }

    /// Hands the next queued input to the adapter if no turn is running.
    async fn pump(&mut self) {
        while self.turn.is_none() && self.state == ProcessState::Running {
            let Some(q) = self.queue.pop_front() else {
                return;
            };
            match self.adapter.input(&q.input).await {
                Ok(()) => {
                    self.turn = Some(Turn {
                        frame_id: q.frame_id,
                        input: q.input,
                        accepted: false,
                    })
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
            AdapterEvent::InputAccepted { event_id } => match self.turn.as_mut() {
                Some(t) if t.input.event_id == event_id => {
                    t.accepted = true;
                    self.link.reply(&t.frame_id, ack(Some(event_id)));
                    self.delivered.push_back(event_id);
                    if self.delivered.len() > DELIVERED_MEMORY {
                        self.delivered.pop_front();
                    }
                }
                _ => {
                    tracing::warn!(session_id = %self.id, %event_id, "adapter accepted an input that is not in flight")
                }
            },
            AdapterEvent::TurnEnded => {
                self.end_turn();
                self.pump().await;
            }
            AdapterEvent::RuntimeSessionId(r) => {
                self.runtime_session_id = Some(r);
                self.send_state();
            }
            AdapterEvent::Exited { error } => {
                if self.state == ProcessState::Stopped {
                    return;
                }
                let in_turn = self.turn.is_some();
                tracing::warn!(session_id = %self.id, ?error, in_turn, "agent process exited");
                self.end_turn();
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

    /// Closes the current turn. An input the runtime never accepted goes back to the head
    /// of the queue: without the acceptance it did not reach the runtime.
    fn end_turn(&mut self) {
        if let Some(t) = self.turn.take() {
            if !t.accepted {
                tracing::warn!(session_id = %self.id, event_id = %t.input.event_id, "turn ended before the input was accepted; requeued");
                self.queue.push_front(Queued {
                    frame_id: t.frame_id,
                    input: t.input,
                });
            }
        }
    }

    async fn ensure_running(&mut self) -> anyhow::Result<()> {
        self.set_state(ProcessState::Starting);
        let r = if self.started_once {
            self.adapter.resume(&self.spec).await
        } else {
            self.adapter.start(&self.spec).await
        };
        match r {
            Ok(()) => {
                self.started_once = true;
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
        self.set_state(ProcessState::Stopped);
    }

    /// Every queued input gets the start failure; the hub counts them (SPEC §5.2).
    fn fail_queue(&mut self, e: &anyhow::Error) {
        for q in self.queue.drain(..) {
            self.link.reply(&q.frame_id, start_failed(e));
        }
    }

    fn set_state(&mut self, s: ProcessState) {
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
            locked_by: if self.state == ProcessState::Stopped {
                LockedBy::None
            } else {
                LockedBy::Hqc
            },
        });
    }

    fn system_event(&self, text: &str, code: &str) {
        let ev = crate::adapter::new_event(
            self.id,
            hqc_proto::event::Author::System,
            EventKind::System {
                text: text.into(),
                code: code.into(),
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
