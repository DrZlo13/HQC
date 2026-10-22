//! hqc worker core (docs/ITERATION-1.md, T3): local config, enrollment, the hub
//! connection with an on-disk outbox, and the session supervisor over runtime adapters.
//!
//! [`Worker::run`] is the daemon; `main.rs` wires it to the real config, token store and
//! signals, tests wire it to a fake hub and [`adapter::fake::FakeAdapters`].

pub mod adapter;
pub mod capabilities;
pub mod config;
pub mod connection;
pub mod enroll;
pub mod link;
pub mod outbox;
pub mod supervisor;
pub mod token;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hqc_proto::frame::Hello;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::adapter::AdapterFactory;
use crate::config::{Config, DEFAULT_ENVIRONMENT};
use crate::connection::{ConnOptions, Connection, Status};
use crate::link::HubLink;
use crate::outbox::Outbox;
use crate::supervisor::Supervisor;
use crate::token::Secret;

pub struct Worker {
    pub config: Config,
    pub token: Secret,
    pub adapters: Arc<dyn AdapterFactory>,
    /// `hello.capabilities`, see [`capabilities::detect`].
    pub capabilities: serde_json::Value,
    /// Directory of the outbox database (`$HQC_HOME/outbox`).
    pub outbox_dir: PathBuf,
    pub conn: ConnOptions,
    /// How long shutdown waits for agents to be killed.
    pub shutdown_timeout: Duration,
}

impl Worker {
    pub fn hello(&self) -> Hello {
        Hello {
            protocol_version: hqc_proto::PROTOCOL_VERSION,
            capabilities: self.capabilities.clone(),
            environments: vec![DEFAULT_ENVIRONMENT.into()],
            env_bindings: vec![],
            allowed_roots: Config::path_strings(&self.config.allowed_roots),
            allow_foreign_sessions: self.config.allow_foreign_sessions,
            share_local_models: false,
            local_models: vec![],
        }
    }

    /// Runs until `shutdown`: then kills every agent, flushes the outbox and returns.
    pub async fn run(
        self,
        shutdown: CancellationToken,
        status: watch::Sender<Status>,
    ) -> anyhow::Result<()> {
        let outbox = Arc::new(Outbox::open(&self.outbox_dir)?);
        let pending = outbox.len()?;
        if pending > 0 {
            tracing::info!(pending, "outbox has events from a previous run");
        }
        let (link, direct) = HubLink::new(outbox);
        let (inbound_tx, inbound_rx) = mpsc::channel(256);
        let conn = Connection::new(
            self.config.hub_url.clone(),
            self.token.clone(),
            self.hello(),
            link.clone(),
            direct,
            inbound_tx,
            status,
            self.conn.clone(),
        );
        let conn_stop = CancellationToken::new();
        let conn_task = tokio::spawn(conn.run(conn_stop.clone()));

        let sup = Supervisor::new(
            Arc::new(self.config),
            self.adapters,
            link,
            self.shutdown_timeout,
        );
        sup.run(inbound_rx, shutdown).await;

        // Agents are down and their last events are in the outbox: flush and close.
        conn_stop.cancel();
        conn_task.await?;
        Ok(())
    }
}
