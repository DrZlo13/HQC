//! hqc hub binary: configuration from the environment, see `hqc_hub::Config::from_env`.

use hqc_hub::{config::AuthConfig, db, push, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if let Some(arg) = std::env::args().nth(1) {
        if arg == "--version" || arg == "-V" {
            println!("hqc-hub {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        anyhow::bail!("unknown argument {arg:?}; the hub is configured by HQC_* environment variables (--version prints the version)");
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?),
        )
        .init();
    let config = Config::from_env()?;
    if let AuthConfig::Dev { email } = &config.auth {
        tracing::warn!(%email, "HQC_DEV=1: Cloudflare Access is OFF, every request is this user");
    }
    // `/.dockerenv`: in a container a store outside a volume is lost on redeploy (T53).
    let root = std::path::Path::new("/.dockerenv")
        .exists()
        .then_some(std::path::Path::new("/"));
    if let Some(w) = hqc_hub::artifacts::storage_warning(&config.artifacts.dir, root) {
        tracing::warn!("{w}");
    }
    let pool = db::connect(&config.database_url).await?;
    let push = push::from_config(config.vapid.as_ref())?;
    let listener = tokio::net::TcpListener::bind(&config.listen).await?;
    tracing::info!(addr = %config.listen, protocol = hqc_proto::PROTOCOL_VERSION, "hqc-hub listening");
    let app = hqc_hub::start(pool, config, push).await;
    axum::serve(listener, hqc_hub::service(app)).await?;
    Ok(())
}
