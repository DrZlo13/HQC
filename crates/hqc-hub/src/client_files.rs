//! The built web client (`client/dist`) from `HQC_CLIENT_DIR`, on the hub's own origin.
//!
//! The client calls `/api/*` by relative path and routes in the URL hash, so it has to be
//! served from the same origin as the API, behind the same Cloudflare Access application
//! (SPEC §6, §20). Only plain files: `/` is `index.html`, there is no SPA fallback, and
//! `/api/*` and `/worker/*` never come from disk.

use std::path::{Component, Path};

use axum::{
    extract::State,
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};

use crate::app::AppState;

/// Router fallback: every path no route matched.
pub async fn handler(State(app): State<AppState>, uri: Uri) -> Response {
    match &app.config.client_dir {
        Some(dir) => serve(dir, uri.path()).await,
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn serve(dir: &Path, path: &str) -> Response {
    let rel = match path.trim_start_matches('/') {
        "" => "index.html",
        p => p,
    };
    let rel = Path::new(rel);
    let inside = rel.components().all(|c| matches!(c, Component::Normal(_)));
    if !inside || rel.starts_with("api") || rel.starts_with("worker") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(body) = tokio::fs::read(dir.join(rel)).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Vite puts a content hash into every name under `assets/`; everything else
    // (index.html, sw.js, the manifest) must be revalidated to pick up a new build.
    let cache = if rel.starts_with("assets") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, content_type(rel)),
            (header::CACHE_CONTROL, cache),
        ],
        body,
    )
        .into_response()
}

fn content_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn get(dir: &Path, path: &str) -> (StatusCode, String, String, String) {
        let r = serve(dir, path).await;
        let h = |k| {
            r.headers()
                .get(k)
                .map(|v: &axum::http::HeaderValue| v.to_str().unwrap().to_string())
                .unwrap_or_default()
        };
        let (ct, cc) = (h(header::CONTENT_TYPE), h(header::CACHE_CONTROL));
        let status = r.status();
        let body = to_bytes(r.into_body(), 1 << 20).await.unwrap();
        (status, ct, cc, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn serves_the_build_and_nothing_else() {
        let root = std::env::temp_dir().join(format!("hqc-client-{}", uuid::Uuid::new_v4()));
        let dist = root.join("dist");
        std::fs::create_dir_all(dist.join("assets")).unwrap();
        std::fs::create_dir_all(dist.join("api")).unwrap();
        std::fs::write(dist.join("index.html"), "<!doctype html>").unwrap();
        std::fs::write(dist.join("sw.js"), "self").unwrap();
        std::fs::write(dist.join("assets/index-abc.js"), "app").unwrap();
        std::fs::write(dist.join("manifest.webmanifest"), "{}").unwrap();
        std::fs::write(dist.join("api/me"), "shadow").unwrap();
        std::fs::write(root.join("secret"), "outside").unwrap();

        let (s, ct, cc, body) = get(&dist, "/").await;
        assert_eq!(
            (s, ct.as_str(), cc.as_str(), body.as_str()),
            (
                StatusCode::OK,
                "text/html; charset=utf-8",
                "no-cache",
                "<!doctype html>"
            )
        );
        let (s, ct, cc, _) = get(&dist, "/assets/index-abc.js").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(ct, "text/javascript; charset=utf-8");
        assert!(cc.contains("immutable"));
        assert_eq!(get(&dist, "/sw.js").await.2, "no-cache");
        assert_eq!(
            get(&dist, "/manifest.webmanifest").await.1,
            "application/manifest+json"
        );

        for bad in [
            "/missing.js",
            "/assets",
            "/../secret",
            "/assets/../../secret",
            "//etc/passwd",
            "/api/me",
            "/worker/ws",
        ] {
            assert_eq!(get(&dist, bad).await.0, StatusCode::NOT_FOUND, "{bad}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
}
