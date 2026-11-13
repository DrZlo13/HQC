//! The path from sessions to the hub connection.
//!
//! Stored events and `mcp.call` go through the outbox (SPEC §5.2): written first, then the
//! connection is woken to send them. Everything else (`agent.delta`, `subagents.live`,
//! `tokens.rate`, replies to hub frames, `session.state`, `mcp.cancel`) is sent directly when
//! connected and dropped otherwise: the hub resends pending inputs after `hello` and the
//! supervisor resends `session.state` on connect. A session's state and an input's `ack` keep
//! their place among the outbox entries ([`Direct::after`]).

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
    /// `Some(seq)`: on the connection it was queued on, the frame goes after the outbox
    /// entries up to local `seq`, the last one written when it was queued, and before those
    /// written later (SPEC §5.2): a session's state and an input's `ack` ([`after_events`]).
    pub after: Option<i64>,
}

/// Whether direct frame `f` keeps its place among the outbox entries (SPEC §5.2): a session's
/// state (`stopped` right after a turn must not overtake its final answer) and an input's `ack`.
/// The hub opens a turn on an input's `ack` and closes it on the final answer or `status idle`
/// (§17.1): the next input's `ack` must not overtake the final answer and `status` of the turn
/// before it, which would close that input's turn as it opens, and the `ack` of an input taken
/// at a turn's end must not come after its `status idle`, which would open a turn after its end.
fn after_events(f: &WorkerFrame) -> bool {
    match f {
        WorkerFrame::SessionState { .. } => true,
        WorkerFrame::Ack(a) => a.event_id.is_some(),
        _ => false,
    }
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
        if let Err(e) = self
            .outbox
            .push_frame(request_id, crate::outbox::FRAME_MCP_CALL, &payload)
        {
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
        self.outbox.push(ev.event_id, &serde_json::to_value(&ev)?)?;
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
        self.outbox.push(id, ev)?;
        self.outbox_ready.notify_one();
        Ok(())
    }

    /// Queue a `mirror.event` (SPEC §5.4, §9.2): through the outbox, keyed by its event id.
    pub fn mirror_event(&self, ev: hqc_proto::frame::MirrorEvent) -> anyhow::Result<()> {
        self.outbox.push_frame(
            ev.event.event_id,
            crate::outbox::FRAME_MIRROR_EVENT,
            &serde_json::to_value(&ev)?,
        )?;
        self.outbox_ready.notify_one();
        Ok(())
    }

    /// Queue a holder report (SPEC §5.4, §9.3) behind the `mirror.event`s already queued.
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
            activity: None,
        };
        let payload = serde_json::to_value(&frame)?["payload"].take();
        self.outbox
            .push_frame(Uuid::now_v7(), crate::outbox::FRAME_SESSION_STATE, &payload)?;
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
        self.direct(Some(id.to_string()), frame);
    }

    pub fn send(&self, frame: WorkerFrame) {
        self.direct(None, frame);
    }

    fn direct(&self, id: Option<String>, frame: WorkerFrame) {
        let after = after_events(&frame).then(|| {
            self.outbox.last_seq().unwrap_or_else(|e| {
                // Not known: the whole outbox goes before the frame.
                tracing::error!("reading the outbox: {e:#}");
                i64::MAX
            })
        });
        let _ = self.direct.send(Direct { id, frame, after });
    }
}
