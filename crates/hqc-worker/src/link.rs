//! The path from sessions to the hub connection.
//!
//! Stored events and `mcp.call` go through the outbox (SPEC §5.2): written first, then the
//! connection is woken to send them. Everything else (`agent.delta`, `subagents.live`,
//! `tokens.rate`, replies to hub frames, `session.state`, `mcp.cancel`) is sent directly when
//! connected and dropped otherwise: the hub resends pending inputs after `hello` and the
//! supervisor resends `session.state` on connect. A session's state and every reply to its
//! `session.input` (the `ack`, the joined ack, an `error`) keep their place among the outbox
//! entries: after the last entry of the session's stream and of its transcript's
//! ([`Direct::after`]).

use std::sync::Arc;

use hqc_proto::event::{Event, EventKind};
use hqc_proto::frame::{McpCancel, WorkerFrame};
use tokio::sync::{mpsc, oneshot, watch, Notify};
use uuid::Uuid;

use crate::mcp::{McpCalls, Outcome};
use crate::outbox::Outbox;

/// A frame to send without the outbox; `id: None` gets a fresh worker frame id.
#[derive(Debug)]
pub struct Direct {
    pub id: Option<String>,
    pub frame: WorkerFrame,
    /// `Some(seq)`: on the connection it was queued on, the frame goes after every outbox
    /// entry up to local `seq`, the last entry of its session's stream or of its transcript's
    /// written when it was queued, and before those written later (SPEC §5.2): a session's state
    /// and the replies to its `session.input` ([`HubLink::reply_input`]). The outbox is sent in
    /// order, so the entries of other sessions written before that one go first; those written
    /// after it do not hold the frame.
    pub after: Option<i64>,
}

#[derive(Clone)]
pub struct HubLink {
    pub(crate) outbox: Arc<Outbox>,
    pub(crate) outbox_ready: Arc<Notify>,
    direct: mpsc::UnboundedSender<Direct>,
    /// The worker's service session from `hello.ack` (SPEC §10.2), for its log lines.
    system_session: Arc<watch::Sender<Option<Uuid>>>,
    /// Session tokens and MCP calls waiting for `mcp.result` (SPEC §7.2).
    pub(crate) mcp: Arc<McpCalls>,
}

impl HubLink {
    pub fn new(outbox: Arc<Outbox>) -> (Self, mpsc::UnboundedReceiver<Direct>) {
        let (direct, rx) = mpsc::unbounded_channel();
        let link = Self {
            outbox,
            outbox_ready: Arc::new(Notify::new()),
            direct,
            system_session: Arc::new(watch::channel(None).0),
            mcp: Arc::default(),
        };
        (link, rx)
    }

    pub fn mcp(&self) -> &McpCalls {
        &self.mcp
    }

    /// Queue `mcp.call` for a tool call of `session_id` (SPEC §5.2): through the outbox,
    /// keyed by its `request_id`, without the session token, which the connection adds when
    /// it sends the frame. `None`: the session's agent does not run.
    pub fn mcp_call(
        &self,
        session_id: Uuid,
        tool: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<Option<(Uuid, oneshot::Receiver<Outcome>)>> {
        let Some((request_id, rx)) = self.mcp.open(session_id) else {
            return Ok(None);
        };
        let mut payload = serde_json::json!({"request_id": request_id, "tool": tool, "args": args});
        // The hub takes the call's chain from the turn's input (SPEC §7.1).
        if let Some(input) = self.mcp.turn(session_id) {
            payload["turn_input"] = serde_json::json!(input);
        }
        if let Err(e) = self.outbox.push_frame(
            &session_id.to_string(),
            request_id,
            crate::outbox::FRAME_MCP_CALL,
            &payload,
        ) {
            self.mcp.forget(request_id);
            return Err(e);
        }
        self.outbox_ready.notify_one();
        Ok(Some((request_id, rx)))
    }

    /// The caller gave the call up (SPEC §7.2): a call the hub has not taken yet leaves the
    /// outbox, and the hub learns of it with `mcp.cancel`.
    pub fn mcp_cancel(&self, request_id: Uuid, reason: &str) {
        if !self.mcp.forget(request_id) {
            return;
        }
        if let Err(e) = self.outbox.remove(request_id) {
            tracing::error!(%request_id, "outbox delete failed: {e:#}");
        }
        self.send(WorkerFrame::McpCancel(McpCancel {
            request_id: request_id.to_string(),
            reason: reason.into(),
        }));
    }

    /// Queue an event for the hub. Fails only if the outbox cannot be written.
    pub fn event(&self, ev: Event) -> anyhow::Result<()> {
        if matches!(
            ev.kind,
            EventKind::AgentDelta { .. }
                | EventKind::SubagentsLive { .. }
                | EventKind::TokensRate { .. }
        ) {
            self.send(WorkerFrame::Event { event: ev });
            return Ok(());
        }
        self.outbox.push(
            &ev.session_id.to_string(),
            ev.event_id,
            &serde_json::to_value(&ev)?,
        )?;
        self.outbox_ready.notify_one();
        Ok(())
    }

    /// Queue an event given as JSON (fields the typed [`Event`] does not have, such as
    /// `level` of a worker log line, SPEC §10.2).
    pub fn event_json(&self, ev: &serde_json::Value) -> anyhow::Result<()> {
        let id: Uuid = ev
            .get("event_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow::anyhow!("event without event_id"))?;
        let session_id: Uuid = ev
            .get("session_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow::anyhow!("event without session_id"))?;
        self.outbox.push(&session_id.to_string(), id, ev)?;
        self.outbox_ready.notify_one();
        Ok(())
    }

    /// Queue a `mirror.event` (SPEC §5.4, §9.2): through the outbox, keyed by its event id, in
    /// the stream of its transcript.
    pub fn mirror_event(&self, ev: hqc_proto::frame::MirrorEvent) -> anyhow::Result<()> {
        self.outbox.push_frame(
            &transcript_stream(&ev.runtime_session_id),
            ev.event.event_id,
            crate::outbox::FRAME_MIRROR_EVENT,
            &serde_json::to_value(&ev)?,
        )?;
        self.outbox_ready.notify_one();
        Ok(())
    }

    /// Queue a holder report (SPEC §5.4, §9.3) behind the `mirror.event`s already queued, in the
    /// stream of its transcript: the state of a session that runs the transcript goes after it.
    pub fn holder_report(
        &self,
        runtime_session_id: Uuid,
        held_by: Option<hqc_proto::frame::HeldBy>,
    ) -> anyhow::Result<()> {
        let frame = WorkerFrame::SessionState {
            session_id: Uuid::nil(),
            process_state: hqc_proto::frame::ProcessState::Stopped,
            runtime_session_id: Some(runtime_session_id.to_string()),
            locked_by: hqc_proto::frame::LockedBy::None,
            title: None,
            held_by: Some(held_by),
            commands: None,
            command_info: None,
            permission_mode: None,
            capped_input: None,
            subagents: 0,
            background_tasks: 0,
            foreground_calls: vec![],
        };
        let payload = serde_json::to_value(&frame)?["payload"].take();
        self.outbox.push_frame(
            &runtime_session_id.to_string(),
            Uuid::now_v7(),
            crate::outbox::FRAME_SESSION_STATE,
            &payload,
        )?;
        self.outbox_ready.notify_one();
        Ok(())
    }

    pub fn set_system_session(&self, id: Uuid) {
        self.system_session.send_if_modified(|v| {
            let changed = *v != Some(id);
            *v = Some(id);
            changed
        });
    }

    pub fn system_session(&self) -> watch::Receiver<Option<Uuid>> {
        self.system_session.subscribe()
    }

    /// A reply to the hub frame `id`.
    pub fn reply(&self, id: &str, frame: WorkerFrame) {
        let _ = self.direct.send(Direct {
            id: Some(id.to_string()),
            frame,
            after: None,
        });
    }

    /// A reply to the `session.input` frame `id` of `session_id`, which runs transcript
    /// `runtime_session_id`: the `ack`, the joined ack or an `error`. It goes after the outbox
    /// entries of the session and of its transcript written before it (SPEC §5.2).
    ///
    /// Why it keeps its place: the hub opens a turn on an input's `ack` and closes it on the
    /// final answer or `status idle` (§17.1). The next input's `ack` must not overtake the final
    /// answer and `status` of the turn before it, which would close that input's turn as it
    /// opens; the `ack` of an input taken at a turn's end must not come after its `status idle`,
    /// which would open a turn after its end; and an `error` answer (a failed delivery) must not
    /// overtake the answer of the turn before it either.
    pub fn reply_input(
        &self,
        session_id: Uuid,
        runtime_session_id: Option<&str>,
        id: &str,
        frame: WorkerFrame,
    ) {
        let after = self.after_of(Some((session_id, runtime_session_id)));
        let _ = self.direct.send(Direct {
            id: Some(id.to_string()),
            frame,
            after,
        });
    }

    /// Sends `frame` directly. A `session.state` keeps its place like a reply to an input: a
    /// session's `stopped` right after a turn must not overtake its final answer, nor a
    /// `locked_by: vscode` the holder report of its transcript (§9.3).
    pub fn send(&self, frame: WorkerFrame) {
        let after = match &frame {
            WorkerFrame::SessionState {
                session_id,
                runtime_session_id,
                ..
            } => self.after_of(Some((*session_id, runtime_session_id.as_deref()))),
            _ => None,
        };
        let _ = self.direct.send(Direct {
            id: None,
            frame,
            after,
        });
    }

    /// The last outbox entry of the stream of a session and of its transcript.
    fn after_of(&self, place: Option<(Uuid, Option<&str>)>) -> Option<i64> {
        let (session_id, runtime) = place?;
        let runtime = runtime.map(transcript_stream);
        Some(
            self.outbox
                .last_seq_of(&session_id.to_string(), runtime.as_deref())
                .unwrap_or_else(|e| {
                    // Not known: the whole outbox goes before the frame.
                    tracing::error!("reading the outbox: {e:#}");
                    i64::MAX
                }),
        )
    }
}

/// The stream of a transcript in the outbox: its id as `Uuid` writes it.
fn transcript_stream(runtime_session_id: &str) -> String {
    runtime_session_id
        .parse::<Uuid>()
        .map(|u| u.to_string())
        .unwrap_or_else(|_| runtime_session_id.to_string())
}
