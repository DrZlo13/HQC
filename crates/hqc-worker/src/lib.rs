//! hqc worker core (docs/ITERATION-1.md, T3): local config, enrollment, the hub
//! connection with an on-disk outbox, and the session supervisor over runtime adapters.
//!
//! [`Worker::run`] is the daemon; `main.rs` wires it to the real config, token store and
//! signals, tests wire it to a fake hub and [`adapter::fake::FakeAdapters`].

pub mod adapter;
pub mod capabilities;
pub mod cleanup;
pub mod config;
pub mod connection;
pub mod enroll;
pub mod fs;
pub mod inbox;
pub mod known_paths;
pub mod link;
pub mod machine;
pub mod mcp;
pub mod mirror;
pub mod outbox;
pub mod permissions;
pub mod supervisor;
pub mod title;
pub mod token;
pub mod worker_log;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hqc_proto::frame::Hello;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::adapter::AdapterFactory;
use crate::config::{Config, DEFAULT_ENVIRONMENT};
use crate::connection::{ConnOptions, Connection, Status};
use crate::known_paths::KnownPaths;
use crate::link::HubLink;
use crate::outbox::Outbox;
use crate::supervisor::Supervisor;
use crate::token::Secret;

pub struct Worker {
    pub config: Config,
    pub token: Secret,
    /// A fresh uuid per process, sent in every `hello` (SPEC §5.4): the hub treats a new
    /// value as a worker restart and the same value as a reconnect (SPEC §17.3).
    pub boot_id: Uuid,
    pub adapters: Arc<dyn AdapterFactory>,
    /// `hello.capabilities`, see [`capabilities::detect`].
    pub capabilities: serde_json::Value,
    /// Directory of the outbox database (`$HQC_HOME/outbox`).
    pub outbox_dir: PathBuf,
    /// Folders the worker runs sessions in without asking (`$HQC_HOME/known_workspaces.json`,
    /// SPEC §10.2).
    pub known_paths_file: PathBuf,
    /// Warn/error lines of the worker's own log, for its service session (SPEC §10.2);
    /// `None`: not forwarded.
    pub logs: Option<worker_log::LogReceiver>,
    pub conn: ConnOptions,
    /// How long shutdown waits for agents to be killed.
    pub shutdown_timeout: Duration,
    /// The mirror of Claude Code sessions (SPEC §9); `None` leaves it off (`mirror.set` and
    /// `workspace.list_candidates` are refused).
    pub mirror: Option<MirrorOptions>,
    /// The machine fingerprint for `hello` ([`machine::fingerprint`], T21).
    pub machine_id: Option<String>,
    /// The socket of the hqc MCP servers (`$HQC_HOME/mcp.sock`, SPEC §7.2); `None`: not
    /// served.
    pub mcp_socket: Option<PathBuf>,
}

pub struct MirrorOptions {
    /// `<claude config>/projects`.
    pub projects_dir: PathBuf,
    /// `<claude config>/sessions`, Claude Code's registry of live sessions (SPEC §9.3);
    /// `None`: holders are not looked at.
    pub sessions_dir: Option<PathBuf>,
    /// Where the mirror keeps offsets and the "file → cwd" cache (`$HQC_HOME/mirror`).
    pub state_dir: PathBuf,
    /// Shared with the Claude Code adapter ([`adapter::BuiltinAdapters::own_records`]).
    pub own_records: Arc<mirror::OwnRecords>,
    pub poll: Duration,
    pub grace: Duration,
    /// A transcript the hub refused with `mirror_quota` is read again after this
    /// ([`mirror::QUOTA_RETRY`]).
    pub quota_retry: Duration,
}

impl Worker {
    pub fn hello(&self) -> Hello {
        // Workspaces are hub objects (SPEC §3); only `allowed_roots` bounds them here.
        Hello {
            protocol_version: hqc_proto::PROTOCOL_VERSION,
            boot_id: self.boot_id,
            capabilities: self.capabilities.clone(),
            environments: vec![DEFAULT_ENVIRONMENT.into()],
            env_bindings: vec![],
            allowed_roots: Config::path_strings(&self.config.allowed_roots),
            allow_foreign_sessions: self.config.allow_foreign_sessions,
            share_local_models: false,
            local_models: vec![],
            machine_id: self.machine_id.clone(),
        }
    }

    /// Runs until `shutdown`: then kills every agent, flushes the outbox and returns.
    pub async fn run(
        mut self,
        shutdown: CancellationToken,
        status: watch::Sender<Status>,
    ) -> anyhow::Result<()> {
        let outbox = Arc::new(Outbox::open(&self.outbox_dir)?);
        let pending = outbox.len()?;
        if pending > 0 {
            tracing::info!(pending, "outbox has events from a previous run");
        }
        let known = Arc::new(KnownPaths::load(&self.known_paths_file)?);
        // Working directories the worker wrote inboxes into (T21).
        let inboxes = Arc::new(KnownPaths::load(
            &self.known_paths_file.with_file_name("inbox_dirs.json"),
        )?);
        {
            let (cfg, ws, dirs) = (self.config.clone(), known.paths(), inboxes.paths());
            tokio::task::spawn_blocking(move || {
                let n = cleanup::sweep(
                    &cfg,
                    &ws,
                    &dirs,
                    cleanup::INBOX_MAX_AGE,
                    std::time::SystemTime::now(),
                );
                if n > 0 {
                    tracing::info!(removed = n, "old inbox folders removed");
                }
            });
        }
        let (link, direct) = HubLink::new(outbox);
        // Bound before anything can start an agent whose MCP server would connect to it.
        let mcp_stop = CancellationToken::new();
        let mcp_task = match &self.mcp_socket {
            Some(path) => {
                let socket = mcp::daemon::McpSocket::bind(path)?;
                tracing::info!(path = %socket.path().display(), "mcp socket ready");
                Some(tokio::spawn(socket.run(link.clone(), mcp_stop.clone())))
            }
            None => None,
        };
        let log_stop = CancellationToken::new();
        let log_task = self
            .logs
            .take()
            .map(|rx| tokio::spawn(worker_log::forward(rx, link.clone(), log_stop.clone())));
        let (inbound_tx, inbound_rx) = mpsc::channel(256);
        let inbound_tx_mirror = inbound_tx.clone();
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

        let artifacts = inbox::ArtifactClient::new(&self.config.hub_url, self.token.clone())
            .with_inboxes(inboxes);
        let mut sup = Supervisor::new(
            Arc::new(self.config),
            self.adapters,
            link.clone(),
            artifacts,
            self.shutdown_timeout,
            known,
        );
        let mirror_stop = CancellationToken::new();
        let mut mirror_task = None;
        if let Some(m) = self.mirror {
            let store = Arc::new(mirror::store::Store::open(&m.state_dir)?);
            let (cmds, cmd_rx) = mpsc::unbounded_channel();
            let mut core =
                mirror::Mirror::new(m.projects_dir.clone(), store.clone(), m.own_records);
            if let Some(d) = m.sessions_dir {
                core = core.with_registry(d, Box::new(mirror::holders::System));
            }
            core.grace = m.grace;
            core.quota_retry = m.quota_retry;
            sup = sup.with_mirror(mirror::MirrorHandle {
                cmds,
                store,
                projects: m.projects_dir,
            });
            mirror_task = Some(tokio::spawn(mirror::run(
                core,
                cmd_rx,
                link,
                inbound_tx_mirror,
                m.poll,
                mirror_stop.clone(),
            )));
        }
        sup.run(inbound_rx, shutdown).await;
        log_stop.cancel();
        if let Some(t) = log_task {
            let _ = t.await;
        }

        // Agents are down and their last events are in the outbox: flush and close.
        mcp_stop.cancel();
        if let Some(t) = mcp_task {
            let _ = t.await;
        }
        mirror_stop.cancel();
        if let Some(t) = mirror_task {
            let _ = t.await;
        }
        conn_stop.cancel();
        conn_task.await?;
        Ok(())
    }
}
