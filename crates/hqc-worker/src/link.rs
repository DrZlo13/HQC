//! The path from sessions to the hub connection.
//!
//! Stored events go through the outbox (SPEC §5.2): written first, then the connection is
//! woken to send them. Everything else (`agent.delta`, replies to hub frames,
//! `session.state`) is sent directly when connected and dropped otherwise: the hub resends
//! pending inputs after `hello` and the supervisor resends `session.state` on connect.

use std::sync::Arc;

use hqc_proto::event::{Event, EventKind};
use hqc_proto::frame::WorkerFrame;
use tokio::sync::{mpsc, Notify};

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
}

impl HubLink {
    pub fn new(outbox: Arc<Outbox>) -> (Self, mpsc::UnboundedReceiver<Direct>) {
        let (direct, rx) = mpsc::unbounded_channel();
        let link = Self {
            outbox,
            outbox_ready: Arc::new(Notify::new()),
            direct,
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
