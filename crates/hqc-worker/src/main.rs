//! hqc worker. See docs/ITERATION-1.md, tasks T3 (core) and T4 (Claude Code adapter).

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use hqc_worker::adapter::BuiltinAdapters;
use hqc_worker::config::{self, Config};
use hqc_worker::connection::{ConnOptions, Status};
use hqc_worker::token::TokenStore;
use hqc_worker::{capabilities, enroll, Worker};
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
    },
    /// Run the daemon (SPEC §2).
    Run,
    /// Serve the hqc MCP server over stdio for one session (SPEC §7); spawned by runtimes.
    Mcp,
    /// Manage local environments and bindings (SPEC §14).
    Env,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    // At `trace` these crates log raw requests, including the worker token in
    // `Authorization` (tungstenite) or in enrollment responses. Capped even if RUST_LOG asks.
    for target in [
        "tungstenite",
        "tokio_tungstenite",
        "hyper",
        "hyper_util",
        "reqwest",
        "h2",
        "rustls",
    ] {
        filter = filter.add_directive(format!("{target}=debug").parse()?);
    }
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Enroll {
            hub_url,
            insecure_http,
        } => {
            let store = TokenStore::from_env(&hub_url)?;
            enroll::enroll(&hub_url, insecure_http, &store, &mut std::io::stdout()).await?;
            if let Ok(path) = config::config_path() {
                if !path.exists() {
                    println!(
                        "Next: write {} (hub_url, allowed_roots, [workspace] path), then run `hqc-worker run`.",
                        path.display()
                    );
                }
            }
            Ok(())
        }
        Cmd::Run => run().await,
        Cmd::Mcp => anyhow::bail!("mcp: not implemented (iteration 3)"),
        Cmd::Env => anyhow::bail!("env: not implemented (iteration 5)"),
    }
}

async fn run() -> anyhow::Result<()> {
    let config = Config::load(&config::config_path()?)?;
    let token = TokenStore::from_env(&config.hub_url)?
        .load()?
        .context("no worker token; run `hqc-worker enroll <hub_url>` first")?;
    let capabilities = capabilities::detect(config.claude_bin.as_deref()).await;
    let permission_mode = capabilities::claude_permission_mode(config.claude_bin.as_deref()).await;
    let p = &config.permissions;
    tracing::info!(hub = %config.hub_url, workspace = %config.workspace.display(), %capabilities, %permission_mode, permissions = ?p.mode, allow = p.allow.len(), deny = p.deny.len(), "starting worker");
    if p.mode == hqc_worker::permissions::PermissionMode::Auto {
        tracing::warn!("[permissions] mode = \"auto\": Claude Code decides tool calls itself and never asks; only the deny list stops a call");
    }
    let adapters = Arc::new(BuiltinAdapters::from_config(&config, permission_mode));
    let worker = Worker {
        config,
        token,
        boot_id: uuid::Uuid::new_v4(),
        adapters,
        capabilities,
        outbox_dir: config::hqc_home()?.join("outbox"),
        conn: ConnOptions::default(),
        shutdown_timeout: Duration::from_secs(10),
    };

    let shutdown = CancellationToken::new();
    tokio::spawn(wait_for_signal(shutdown.clone()));
    let (status, _) = tokio::sync::watch::channel(Status::Connecting);
    worker.run(shutdown, status).await?;
    tracing::info!("worker stopped");
    Ok(())
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
