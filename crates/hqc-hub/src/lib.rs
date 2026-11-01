//! hqc hub core (SPEC §2, §5, §6, §10.1, §16, §17, §20; docs/ITERATION-1.md T2).
//!
//! - `/api/*` — people, authenticated by Cloudflare Access (or `HQC_DEV=1`);
//! - `/worker/*` — workers: enrollment and the `/worker/ws` WebSocket with a bearer token;
//! - everything else — the web client from `HQC_CLIENT_DIR`, if set.

use std::sync::Arc;

use axum::{
    routing::{get, post, put},
    Router,
};
use sqlx::PgPool;

pub mod api;
pub mod app;
pub mod artifacts;
pub mod auth;
pub mod client_files;
pub mod client_ws;
pub mod config;
pub mod db;
pub mod delivery;
pub mod error;
pub mod events;
pub mod icons;
pub mod mcp;
pub mod mirror;
pub mod peers;
pub mod push;
pub mod requests;
pub mod rules;
pub mod schedules;
pub mod search;
pub mod security;
pub mod spawn;
pub mod state;
pub mod titles;
pub mod watchdog;
pub mod worker_ws;

pub use app::AppState;
pub use config::Config;

pub fn router(app: AppState) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/api/me", get(api::me).patch(api::patch_me))
        .route(
            "/api/projects",
            get(api::projects).post(api::create_project),
        )
        .route(
            "/api/projects/{id}",
            get(api::project)
                .patch(api::patch_project)
                .delete(api::delete_project),
        )
        .route(
            "/api/projects/{id}/icon",
            get(icons::get)
                .put(icons::put)
                .layer(axum::extract::DefaultBodyLimit::max(icons::BODY_LIMIT)),
        )
        .route(
            "/api/workspaces",
            get(api::workspaces).post(api::create_workspace),
        )
        .route(
            "/api/workspaces/{id}",
            axum::routing::patch(api::patch_workspace).delete(api::delete_workspace),
        )
        .route(
            "/api/workspaces/{id}/rules",
            get(rules::list).post(rules::add),
        )
        .route(
            "/api/workspaces/{id}/rules/{rule_id}",
            axum::routing::delete(rules::delete),
        )
        .route("/api/workspaces/{id}/permissions", get(rules::inspect))
        .route(
            "/api/projects/{id}/sessions",
            get(api::project_sessions).post(api::create_session),
        )
        .route(
            "/api/projects/{id}/session_order",
            get(api::get_session_order).put(api::put_session_order),
        )
        .route(
            "/api/sessions/{id}",
            get(api::get_session)
                .patch(api::patch_session)
                .delete(api::delete_session),
        )
        .route("/api/sessions/{id}/events", get(api::session_events))
        .route("/api/search", get(api::search))
        .route("/api/sessions/{id}/input", post(api::input))
        .route("/api/sessions/{id}/stop", post(api::stop))
        .route("/api/sessions/{id}/retry", post(api::retry))
        .route("/api/sessions/{id}/archive", post(api::archive))
        .route("/api/sessions/{id}/unarchive", post(api::unarchive))
        .route(
            "/api/sessions/{id}/requests/{request_id}",
            post(api::answer_request),
        )
        .route("/api/requests", get(api::open_requests))
        .route("/api/sessions/{id}/read_cursor", put(api::read_cursor))
        .route("/api/workers", get(api::workers))
        .route("/api/workers/enroll/confirm", post(api::enroll_confirm))
        .route("/api/workers/enroll/{code}", get(api::enroll_info))
        .route(
            "/api/workers/{id}",
            axum::routing::delete(api::delete_worker),
        )
        .route("/api/workers/{id}/dirs", get(api::worker_dirs))
        .route("/api/workers/{id}/candidates", get(api::worker_candidates))
        .route("/api/sessions/{id}/takeover", post(api::takeover))
        .route("/api/push/vapid", get(api::vapid))
        .route(
            "/api/push/subscribe",
            post(api::push_subscribe).delete(api::push_unsubscribe),
        )
        .route(
            "/api/artifacts",
            post(artifacts::upload).layer(axum::extract::DefaultBodyLimit::max(
                artifacts::upload_body_limit(&app.config.artifacts),
            )),
        )
        .route("/api/artifacts/{id}", get(artifacts::meta))
        .route("/api/artifacts/{id}/url", get(artifacts::url))
        .route("/artifacts/{id}", get(artifacts::signed))
        .route(
            "/worker/artifacts",
            axum::routing::put(artifacts::from_worker)
                .layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .route("/worker/artifacts/{id}", get(artifacts::for_worker))
        .route(
            "/api/schedules",
            get(schedules::list).post(schedules::create_api),
        )
        .route(
            "/api/schedules/{id}",
            axum::routing::patch(schedules::patch).delete(schedules::delete),
        )
        .route("/api/ws", get(client_ws::handler))
        .route("/worker/enroll", post(api::worker_enroll))
        .route("/worker/enroll/poll", post(api::worker_enroll_poll))
        .route("/worker/ws", get(worker_ws::handler))
        .fallback(client_files::handler)
        .layer(axum::middleware::from_fn_with_state(
            app.clone(),
            security::guard,
        ))
        .with_state(app)
}

/// The router as a service that records the peer address for [`security::ClientIp`].
pub fn service(
    app: AppState,
) -> axum::extract::connect_info::IntoMakeServiceWithConnectInfo<Router, std::net::SocketAddr> {
    router(app).into_make_service_with_connect_info::<std::net::SocketAddr>()
}

/// Build the hub state over a migrated pool and start its background tasks.
pub fn start(db: PgPool, config: Config, push: Arc<dyn push::PushSender>) -> AppState {
    let app = AppState::new(db, config, push);
    tokio::spawn(requests::expiry_loop(app.clone()));
    if let Some(limit) = app.config.timing.turn_watchdog {
        tokio::spawn(watchdog::turn_watchdog_loop(app.clone(), limit));
    }
    tokio::spawn(schedules::cron_loop(app.clone()));
    if let Some(keep) = app.config.artifacts.retention {
        tokio::spawn(artifacts::cleanup_loop(app.clone(), keep));
    }
    app
}
