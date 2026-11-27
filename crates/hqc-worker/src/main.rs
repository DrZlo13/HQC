//! hqc worker. See docs/ITERATION-1.md, tasks T3 (core) and T4 (Claude Code adapter).

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use hqc_worker::adapter::claude_code::McpLaunch;
use hqc_worker::adapter::BuiltinAdapters;
use hqc_worker::config::{self, Config};
use hqc_worker::connection::{ConnOptions, Status};
use hqc_worker::permissions::PermissionMode;
use hqc_worker::token::TokenStore;
use hqc_worker::{capabilities, enroll, worker_log, Worker};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(name = "hqc-worker", version, about = "hqc worker daemon")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Register this machine with a hub (SPEC §5.1): prints a code to confirm in the web UI.
    Enroll {
        /// `https://...`; `http://` only for localhost / 127.0.0.0/8 / [::1].
        hub_url: String,
        /// Allow `http://` to a non-loopback hub: the token travels unencrypted.
        #[arg(long)]
        insecure_http: bool,
        /// Worker name shown in the web UI; the host name without its domain by default.
        #[arg(long)]
        name: Option<String>,
    },
    /// Run the daemon (SPEC §2).
    Run,
    /// Serve the hqc MCP server over stdio for one session (SPEC §7.2); spawned by runtimes,
    /// with the session token in `HQC_SESSION_TOKEN`. `approve` and `pins` manage the pins of
    /// the local MCP servers' tools.
    #[command(args_conflicts_with_subcommands = true)]
    Mcp {
        #[command(subcommand)]
        cmd: Option<McpCmd>,
        /// The daemon's socket; `$HQC_HOME/mcp.sock` by default.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
        /// Relay the hub's remote MCP server of this name instead (SPEC 0.235).
        #[arg(long, conflicts_with = "local")]
        upstream: Option<String>,
        /// Run the local MCP server of this name of the session's environment instead
        /// (`[environments.<env>.mcp_servers.<name>]` of worker.toml).
        #[arg(long)]
        local: Option<String>,
    },
    /// List the environments and bindings of worker.toml (SPEC §14); edit them there.
    Env {
        #[command(subcommand)]
        cmd: Option<EnvCmd>,
    },
}

#[derive(Subcommand)]
enum McpCmd {
    /// Show how the tools of a local MCP server changed since they were pinned, and pin the
    /// new ones, so that its sessions get the server again.
    Approve {
        /// `<environment>/<server>`, as the chat's line and `hqc-worker mcp pins` name it.
        server: String,
        /// Pin without asking.
        #[arg(long)]
        yes: bool,
    },
    /// The pinned local MCP servers and those whose tools changed.
    Pins,
}

#[derive(Subcommand)]
enum EnvCmd {
    /// The environment and the effective rules of a folder.
    Which { path: std::path::PathBuf },
    /// Copy a private key into the store `$HQC_HOME/keys/` (0600), for `ssh_keys`.
    AddKey {
        name: String,
        file: std::path::PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use tracing_subscriber::layer::{Layer, SubscriberExt};
    use tracing_subscriber::util::SubscriberInitExt;

    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    // At `trace` these crates log raw requests, including the worker token in
    // `Authorization` (tungstenite) or in enrollment responses. Capped even if RUST_LOG asks.
    // A cap, not a directive: `reqwest=debug` would raise them above the `info` default.
    const CAPPED: [&str; 7] = [
        "tungstenite",
        "tokio_tungstenite",
        "hyper",
        "hyper_util",
        "reqwest",
        "h2",
        "rustls",
    ];
    let cap = tracing_subscriber::filter::filter_fn(|m| {
        *m.level() != tracing::Level::TRACE
            || !CAPPED
                .iter()
                .any(|t| m.target() == *t || m.target().starts_with(&format!("{t}::")))
    });
    // Warn and error lines also go to the worker's service session (SPEC §10.2).
    let (log_layer, logs) = worker_log::layer();
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(filter)
                .with_filter(cap),
        )
        .with(log_layer)
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Enroll {
            hub_url,
            insecure_http,
            name,
        } => {
            let store = TokenStore::from_env(&hub_url)?;
            let name = name.unwrap_or_else(enroll::default_name);
            enroll::enroll(
                &hub_url,
                &name,
                insecure_http,
                &store,
                &mut std::io::stdout(),
            )
            .await?;
            if let Ok(path) = config::config_path() {
                if !path.exists() {
                    println!(
                        "Next: write {} (hub_url, allowed_roots), run `hqc-worker run`, then add workspaces inside allowed_roots in the web client.",
                        path.display()
                    );
                }
            }
            Ok(())
        }
        Cmd::Run => run(logs).await,
        Cmd::Mcp { cmd: Some(cmd), .. } => {
            let path = config::hqc_home()?.join(hqc_worker::mcp::pins::FILE);
            match cmd {
                McpCmd::Pins => print!("{}", hqc_worker::mcp::pins::list(&path)?),
                McpCmd::Approve { server, yes } => approve(&path, &server, yes)?,
            }
            Ok(())
        }
        Cmd::Mcp {
            cmd: None,
            socket,
            upstream,
            local,
        } => {
            let socket = match socket {
                Some(s) => s,
                None => hqc_worker::mcp::socket_path(&config::hqc_home()?),
            };
            let token = std::env::var(hqc_worker::mcp::TOKEN_ENV).ok();
            if let Some(name) = local {
                let r = hqc_worker::mcp::local::run(
                    &socket,
                    token,
                    &name,
                    tokio::io::stdin(),
                    tokio::io::stdout(),
                )
                .await;
                // The read of stdin blocks a thread the runtime would wait for at its end: a
                // launcher that stops before the CLI closes its stdin (a failed pin) exits now.
                if let Err(e) = &r {
                    eprintln!("hqc-worker mcp --local {name}: {e:#}");
                }
                std::process::exit(i32::from(r.is_err()));
            }
            let mut server = hqc_worker::mcp::server::McpServer::new(token, socket);
            if let Some(name) = upstream {
                server = server.with_upstream(name);
            }
            server.serve(tokio::io::stdin(), tokio::io::stdout()).await
        }
        Cmd::Env { cmd } => {
            let load = || Config::load(&config::config_path()?);
            let out = match cmd {
                None => hqc_worker::env_cmd::summary(&load()?),
                Some(EnvCmd::Which { path }) => hqc_worker::env_cmd::which(&load()?, &path)?,
                // The store needs no worker.toml.
                Some(EnvCmd::AddKey { name, file }) => hqc_worker::env_cmd::add_key(
                    &hqc_worker::ssh::Store::in_home(&config::hqc_home()?),
                    &name,
                    &file,
                )?,
            };
            print!("{out}");
            Ok(())
        }
    }
}

/// `hqc-worker mcp approve <env>/<server>`: shows the change and pins it once the owner agrees.
fn approve(path: &std::path::Path, key: &str, yes: bool) -> anyhow::Result<()> {
    use std::io::{BufRead, IsTerminal, Write};
    let Some((change, sha256)) = hqc_worker::mcp::pins::pending(path, key)? else {
        println!("{key}: its tools have not changed since they were pinned; nothing to approve");
        return Ok(());
    };
    println!("The tools of {key} changed since they were pinned: {change}");
    if !yes {
        if !std::io::stdin().is_terminal() {
            anyhow::bail!("not pinned: run it in a terminal to answer, or pass --yes");
        }
        print!("Pin these tools, so that agents get them? [y/N] ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("not pinned: the server stays off");
            return Ok(());
        }
    }
    hqc_worker::mcp::pins::approve(path, key, &sha256)?;
    println!("pinned: the next start of {key} in a session gets these tools");
    Ok(())
}

async fn run(logs: worker_log::LogReceiver) -> anyhow::Result<()> {
    let config = Config::load(&config::config_path()?)?;
    let token = TokenStore::from_env(&config.hub_url)?
        .load()?
        .context("no worker token; run `hqc-worker enroll <hub_url>` first")?;
    let mut capabilities = capabilities::detect(config.claude_bin.as_deref()).await;
    if let Some(d) = hqc_worker::adapter::claude_code::paths::default_config_dir() {
        capabilities::with_sessions_registry(&mut capabilities, &d.join("sessions"));
        capabilities::with_moves(&mut capabilities, &d);
    }
    // SPEC §8.2: the ACP agents of worker.toml that answer `initialize`, probed in the
    // worker's home folder (no session is made).
    capabilities["acp_agents"] =
        capabilities::acp_agents(&config.acp_agents, &config::hqc_home()?).await;
    // SPEC §8.6: Codex, when `[codex]` is set and a supported version answers.
    if let Some(c) = capabilities::codex(config.codex.as_deref(), &config::hqc_home()?).await {
        capabilities["codex"] = c;
    }
    let permission_mode = capabilities::claude_permission_mode(config.claude_bin.as_deref()).await;
    let p = &config.permissions;
    tracing::info!(hub = %config.hub_url, allowed_roots = ?config.allowed_roots, %capabilities, %permission_mode, permissions = ?p.mode, allow_auto = p.allow_auto, allow_all = p.allow_all, allow = p.allow.len(), deny = p.deny.len(), environments = config.environments.len(), env_bindings = config.env_bindings.len(), "starting worker");
    if config.legacy_workspace {
        tracing::warn!("worker.toml has an iteration 1 [workspace] block, which is ignored: add workspaces in the web client and remove the block");
    }
    for name in config.unbound_environments() {
        tracing::warn!(
            "worker.toml: no [[env_bindings]] names [environments.{name}], so no folder runs in it"
        );
    }
    match p.mode {
        PermissionMode::Auto => {
            tracing::warn!("[permissions] mode = \"auto\": Claude Code decides tool calls itself and never asks; only the deny list stops a call")
        }
        PermissionMode::AllowAll => {
            tracing::warn!("[permissions] mode = \"allow_all\": Claude Code runs tool calls without asking and without its classifier; only the deny list and the ask rules of its settings stop a call")
        }
        PermissionMode::Ask if p.allow_auto => {
            tracing::info!("[permissions] allow_auto: a session switched to auto in hqc runs without asking; only the deny list stops a call")
        }
        PermissionMode::Ask => {}
    }
    if p.allow_all && p.mode != PermissionMode::AllowAll {
        tracing::warn!("[permissions] allow_all: a session switched to allow_all in hqc runs tool calls without asking and without Claude Code's classifier; only the deny list and the ask rules of its settings stop a call")
    }
    // The agents' hqc MCP server is this binary (SPEC §7.2).
    let mcp_socket = hqc_worker::mcp::socket_path(&config::hqc_home()?);
    // ssh-agents a worker that died left behind (T44); not those of a live daemon, which
    // stops this one when it binds the MCP socket.
    if !hqc_worker::mcp::daemon::served(&mcp_socket) {
        let n = config.ssh.clean_agents().context("ssh-agent sockets")?;
        if n > 0 {
            tracing::info!(removed = n, "stale ssh-agent sockets removed");
        }
        // The secrets of processes it left behind (SPEC 0.147).
        let n = config.secrets.clean().context("secrets folders")?;
        if n > 0 {
            tracing::info!(removed = n, "stale secrets folders removed");
        }
    }
    let adapters = Arc::new(
        BuiltinAdapters::from_config(&config, permission_mode).with_mcp(McpLaunch {
            program: std::env::current_exe().context("the worker's own binary")?,
            socket: mcp_socket.clone(),
        }),
    );
    // The mirror reads the same config dir as the adapter (SPEC §9).
    let mirror = hqc_worker::adapter::claude_code::paths::default_config_dir().map(|d| {
        hqc_worker::MirrorOptions {
            projects_dir: d.join("projects"),
            sessions_dir: Some(d.join("sessions")),
            state_dir: config::hqc_home().unwrap_or_default().join("mirror"),
            own_records: adapters.own_records(),
            poll: hqc_worker::mirror::POLL,
            grace: hqc_worker::mirror::GRACE,
            quota_retry: hqc_worker::mirror::QUOTA_RETRY,
        }
    });
    let drain = CancellationToken::new();
    let worker = Worker {
        mirror,
        config,
        token,
        boot_id: uuid::Uuid::new_v4(),
        adapters,
        capabilities,
        outbox_dir: config::hqc_home()?.join("outbox"),
        known_paths_file: config::hqc_home()?.join("known_workspaces.json"),
        logs: Some(logs),
        conn: ConnOptions::default(),
        shutdown_timeout: Duration::from_secs(10),
        machine_id: hqc_worker::machine::fingerprint(config::hqc_home().ok().as_deref()),
        mcp_socket: Some(mcp_socket),
        drain: Some(drain.clone()),
    };

    let shutdown = CancellationToken::new();
    tokio::spawn(wait_for_signal(shutdown.clone()));
    tokio::spawn(wait_for_drain(drain));
    let (status, _) = tokio::sync::watch::channel(Status::Connecting);
    worker.run(shutdown, status).await?;
    tracing::info!("worker stopped");
    Ok(())
}

/// SIGUSR1 (#220, SPEC 0.256): stop once no agent works, at most
/// [`hqc_worker::supervisor::DRAIN_MAX`] from now; the service manager starts the worker again
/// (a new binary, say) and no agent loses a turn or its background work to it.
async fn wait_for_drain(drain: CancellationToken) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut usr1 = signal(SignalKind::user_defined1()).expect("SIGUSR1 handler");
        usr1.recv().await;
        tracing::info!("SIGUSR1 received: stopping once no agent works");
        drain.cancel();
    }
    #[cfg(not(unix))]
    {
        let _ = drain;
        std::future::pending::<()>().await;
    }
}

/// SIGTERM or Ctrl-C: graceful shutdown (kill agents, flush the outbox).
async fn wait_for_signal(shutdown: CancellationToken) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = term.recv() => tracing::info!("SIGTERM received, shutting down"),
            _ = tokio::signal::ctrl_c() => tracing::info!("interrupt received, shutting down"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("interrupt received, shutting down");
    }
    shutdown.cancel();
}
