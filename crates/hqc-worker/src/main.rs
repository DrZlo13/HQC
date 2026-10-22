//! hqc worker. Skeleton with the CLI surface from the spec; every subcommand is a stub.
//! See docs/ITERATION-1.md tasks T3 and T4.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "hqc-worker", version, about = "hqc worker daemon")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Register this machine with a hub (SPEC §5.1): prints a code to confirm in the web UI.
    Enroll { hub_url: String },
    /// Run the daemon (SPEC §2).
    Run,
    /// Serve the hqc MCP server over stdio for one session (SPEC §7); spawned by runtimes.
    Mcp,
    /// Manage local environments and bindings (SPEC §14).
    Env,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?),
        )
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Enroll { hub_url } => anyhow::bail!("enroll {hub_url}: not implemented (T3)"),
        Cmd::Run => anyhow::bail!("run: not implemented (T3)"),
        Cmd::Mcp => anyhow::bail!("mcp: not implemented (iteration 3)"),
        Cmd::Env => anyhow::bail!("env: not implemented (iteration 5)"),
    }
}
