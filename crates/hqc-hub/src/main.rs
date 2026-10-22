//! hqc hub. Skeleton: `/health` only. See docs/ITERATION-1.md task T2 for what goes here.

use axum::{routing::get, Router};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?),
        )
        .init();
    let app = Router::new().route("/health", get(|| async { "ok" }));
    let addr = std::env::var("HQC_HUB_LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".into());
    tracing::info!(%addr, protocol = hqc_proto::PROTOCOL_VERSION, "hqc-hub listening");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
