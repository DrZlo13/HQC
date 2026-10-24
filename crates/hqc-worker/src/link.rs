//! The path from sessions to the hub connection.
//!
//! Stored events go through the outbox (SPEC §5.2): written first, then the connection is
//! woken to send them. Everything else (`agent.delta`, replies to hub frames,
//! `session.state`) is sent directly when connected and dropped otherwise: the hub resends
//! pending inputs after `hello` and the supervisor resends `session.state` on connect.

use std::sync::Arc;

use hqc_proto::event::{Event, EventKind};
use hqc_proto::frame::WorkerFrame;
use tokio::sync::{mpsc, watch, Notify};
use uuid::Uuid;

use crate::outbox::Outbox;

/// A frame to send without the outbox; `id: None` gets a fresh worker frame id.
#[derive(Debug)]
pub struct Direct {
    pub id: Option<String>,
    pub frame: WorkerFrame,
}

#[derive(Clone)]
pub struct HubLink {
    pub(crate) outbox: Arc<Outbox>,
    pub(crate) outbox_ready: Arc<Notify>,
    direct: mpsc::UnboundedSender<Direct>,
    /// The worker's service session from `hello.ack` (SPEC §10.2), for its log lines.
    system_session: Arc<watch::Sender<Option<Uuid>>>,
}

impl HubLink {
    pub fn new(outbox: Arc<Outbox>) -> (Self, mpsc::UnboundedReceiver<Direct>) {
        let (direct, rx) = mpsc::unbounded_channel();
        let link = Self {
            outbox,
            outbox_ready: Arc::new(Notify::new()),
            direct,
            system_session: Arc::new(watch::channel(None).0),
        };
        (link, rx)
    }

    /// Queue an event for the hub. Fails only if the outbox cannot be written.
    pub fn event(&self, ev: Event) -> anyhow::Result<()> {
        if matches!(ev.kind, EventKind::AgentDelta { .. }) {
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
        let _ = self.direct.send(Direct {
            id: Some(id.to_string()),
            frame,
        });
    }

    pub fn send(&self, frame: WorkerFrame) {
        let _ = self.direct.send(Direct { id: None, frame });
    }
}
