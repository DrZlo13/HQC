//! Browser-facing protections and limits of unauthenticated routes (SPEC §20).
//!
//! - [`guard`] (middleware on the whole router): a WebSocket upgrade on `/api/*` must carry an
//!   `Origin` equal to the request's own host, and any other non-GET request on `/api/*` with
//!   an `Origin` must match it too (cross-site WebSocket hijacking, CSRF with the Access
//!   cookie). The origin's scheme is `https`, or `http` for a loopback host (development).
//!   Every response gets the security headers ([`SECURITY_HEADERS`]); with a separate artifact
//!   domain its origin is added to `img-src`, `frame-src` and `connect-src` ([`client_csp`]).
//!   Artifact content (`/artifacts/*`) sets its own sandbox CSP and framing (SPEC §12), and
//!   the artifact domain serves nothing else.
//! - [`RateLimiter`]: sliding one-minute windows per key, for `/worker/*` by client address
//!   ([`ClientIp`]).
//! - [`Budget`]: weighted sliding windows of any length, for the caps of the MCP surface.

use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    extract::{ConnectInfo, FromRequestParts, Request, State},
    http::{header, request::Parts, HeaderValue, Method},
    middleware::Next,
    response::{IntoResponse, Response},
};

use chrono::{DateTime, TimeDelta, Utc};

use crate::{app::AppState, error::ApiError};

/// Set on every response of the hub, unless the handler set the header itself.
pub const SECURITY_HEADERS: &[(&str, &str)] = &[
    (
        "content-security-policy",
        "default-src 'self'; connect-src 'self' wss: https:; img-src 'self' data:; \
         style-src 'self' 'unsafe-inline'; object-src 'none'; base-uri 'none'; \
         frame-ancestors 'none'",
    ),
    ("x-frame-options", "DENY"),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
];

/// Largest WebSocket message accepted from a client or a worker.
pub const MAX_WS_MESSAGE: usize = 1 << 20;

/// The hub's CSP; `artifact_origin` (`HQC_ARTIFACT_PUBLIC_BASE`) may be shown in `<img>`,
/// `<iframe>` and read with `fetch` (SPEC §12).
pub fn client_csp(artifact_origin: Option<&str>) -> String {
    let base = SECURITY_HEADERS[0].1;
    match artifact_origin {
        None => base.to_string(),
        Some(o) => base
            .replace("connect-src 'self'", &format!("connect-src 'self' {o}"))
            .replace(
                "img-src 'self' data:",
                &format!("img-src 'self' data: {o}; frame-src {o}"),
            ),
    }
}

pub async fn guard(State(app): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let artifact_route = path.starts_with("/artifacts/");
    if !artifact_route
        && path != "/health"
        && crate::artifacts::is_artifact_host(&app.config.artifacts, req.headers())
    {
        // The artifact domain is outside Access: it serves signed content and nothing else.
        let mut r = ApiError::NotFound.into_response();
        r.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(crate::artifacts::CSP),
        );
        return r;
    }
    let mut resp = match check_origin(&req, &app.config.public_origins) {
        Ok(()) => next.run(req).await,
        Err(why) => {
            let h = req.headers();
            let hdr = |k: &str| {
                h.get(k)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-")
                    .to_string()
            };
            tracing::warn!(
                path = %req.uri().path(),
                origin = %hdr("origin"),
                host = %hdr("host"),
                forwarded_host = %hdr("x-forwarded-host"),
                "{why}"
            );
            ApiError::Forbidden("cross-origin request refused".into()).into_response()
        }
    };
    let h = resp.headers_mut();
    for (k, v) in SECURITY_HEADERS {
        if h.contains_key(*k) {
            continue;
        }
        if *k == "content-security-policy" {
            if artifact_route {
                h.insert(*k, HeaderValue::from_static(crate::artifacts::CSP));
            } else if let Ok(v) =
                HeaderValue::from_str(&client_csp(app.config.artifacts.public_base.as_deref()))
            {
                h.insert(*k, v);
            }
        } else if !(artifact_route && *k == "x-frame-options") {
            h.insert(*k, HeaderValue::from_static(v));
        }
    }
    resp
}

fn check_origin(req: &Request, public_origins: &[String]) -> Result<(), &'static str> {
    let headers = req.headers();
    if !req.uri().path().starts_with("/api/") {
        return Ok(());
    }
    // A reverse proxy (Caddy, cloudflared with `httpHostHeader`) may rewrite `Host` to the
    // upstream name; it then says where the request came from in `X-Forwarded-Host`. A
    // browser cannot set that header cross-site, so trusting it loses nothing here.
    let forwarded_host = headers
        .get("x-forwarded-host")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let upgrade = headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .or_else(|| req.uri().authority().map(|a| a.as_str()));
    match origin {
        // Browsers always send Origin on a WebSocket upgrade; /api has no other clients.
        None if upgrade => Err("websocket upgrade without Origin"),
        None => Ok(()),
        Some(_)
            if !upgrade
                && matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) =>
        {
            Ok(())
        }
        Some(o) if !public_origins.is_empty() => match normalize_origin(o) {
            Some(n) if public_origins.contains(&n) => Ok(()),
            _ => Err("Origin is not in HQC_PUBLIC_ORIGINS"),
        },
        Some(o) => {
            let ok = forwarded_host
                .into_iter()
                .chain(host)
                .any(|h| same_origin(o, h));
            if ok {
                Ok(())
            } else {
                Err("Origin does not match the hub's host")
            }
        }
    }
}

/// `scheme://host[:port]` lowercased, default port dropped; `None` for anything else.
pub fn normalize_origin(origin: &str) -> Option<String> {
    let (scheme, rest) = origin.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "https" => ":443",
        "http" => ":80",
        _ => return None,
    };
    if rest.is_empty() || rest.contains(['/', '?', '#', '@', ' ']) {
        return None;
    }
    let host = rest.trim().to_ascii_lowercase();
    let host = host.strip_suffix(default_port).unwrap_or(&host);
    Some(format!("{scheme}://{host}"))
}

/// `origin` (`scheme://host[:port]`) names the host the request was sent to: `https`, or
/// `http` for a loopback host only.
pub fn same_origin(origin: &str, host: &str) -> bool {
    let Some((scheme, rest)) = origin.split_once("://") else {
        return false;
    };
    let (default_port, secure) = match scheme.to_ascii_lowercase().as_str() {
        "https" => (":443", true),
        "http" => (":80", false),
        _ => return false,
    };
    if rest.contains(['/', '?', '#', '@']) {
        return false;
    }
    let strip = |s: &str| {
        let s = s.trim().to_ascii_lowercase();
        s.strip_suffix(default_port)
            .map(str::to_string)
            .unwrap_or(s)
    };
    let (o, h) = (strip(rest), strip(host));
    if o.is_empty() || o != h {
        return false;
    }
    secure || is_loopback_host(&o)
}

/// `localhost`, `127.0.0.0/8` or `[::1]`, with or without a port.
pub fn is_loopback_host(hostport: &str) -> bool {
    let host = if let Some(r) = hostport.strip_prefix('[') {
        r.split(']').next().unwrap_or("")
    } else {
        hostport.rsplit_once(':').map_or(hostport, |(h, _)| h)
    };
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The client's address (SPEC §20): the header named by `HQC_CLIENT_IP_HEADER`, which a
/// reverse proxy such as Traefik sets from the TCP peer, and then `CF-Connecting-IP` is not
/// read; without that setting `CF-Connecting-IP` from Cloudflare (the hub is reachable only
/// through the tunnel). Else the TCP peer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClientIp(pub String);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        app: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = app
            .config
            .client_ip_header
            .as_ref()
            .map_or("cf-connecting-ip", |h| h.as_str());
        if let Some(ip) = parts
            .headers
            .get(header)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
        {
            return Ok(ClientIp(ip.to_string()));
        }
        Ok(ClientIp(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0.ip().to_string())
                .unwrap_or_else(|| "unknown".into()),
        ))
    }
}

/// Sliding one-minute windows, per `(bucket, key)`.
#[derive(Default)]
pub struct RateLimiter {
    calls: Mutex<HashMap<(&'static str, String), VecDeque<Instant>>>,
}

const WINDOW: Duration = Duration::from_secs(60);
/// Above this many keys, empty windows are dropped.
const MAX_KEYS: usize = 10_000;

impl RateLimiter {
    /// Counts a call and returns whether it fits into `per_minute`.
    pub fn allow(&self, bucket: &'static str, key: &str, per_minute: usize) -> bool {
        let mut m = self.calls.lock().unwrap();
        if m.len() > MAX_KEYS {
            m.retain(|_, q| q.back().is_some_and(|t| t.elapsed() < WINDOW));
        }
        let q = m.entry((bucket, key.to_string())).or_default();
        while q.front().is_some_and(|t| t.elapsed() >= WINDOW) {
            q.pop_front();
        }
        if q.len() >= per_minute {
            return false;
        }
        q.push_back(Instant::now());
        true
    }

    /// Whether `key` has used up `per_minute` without counting a call.
    pub fn exhausted(&self, bucket: &'static str, key: &str, per_minute: usize) -> bool {
        let m = self.calls.lock().unwrap();
        m.get(&(bucket, key.to_string()))
            .is_some_and(|q| q.iter().filter(|t| t.elapsed() < WINDOW).count() >= per_minute)
    }
}

/// Amounts spent and when, oldest first.
type Spent = VecDeque<(DateTime<Utc>, u64)>;

/// Amounts spent in a sliding window of any length, per `(bucket, key)`, at times the caller
/// gives (the hub cron's clock is one a test sets). The caps of the MCP surface (SPEC §20):
/// calls, messages, searches, upload bytes, scheduled runs.
#[derive(Default)]
pub struct Budget {
    spent: Mutex<HashMap<(&'static str, String), Spent>>,
}

impl Budget {
    /// Spends `amount` if the window ending at `now` still has room for it under `max`;
    /// whether it did.
    pub fn spend(
        &self,
        bucket: &'static str,
        key: &str,
        amount: u64,
        max: u64,
        window: TimeDelta,
        now: DateTime<Utc>,
    ) -> bool {
        let mut m = self.spent.lock().unwrap();
        if m.len() > MAX_KEYS {
            m.retain(|_, q| q.back().is_some_and(|(t, _)| now - *t < window));
        }
        let q = m.entry((bucket, key.to_string())).or_default();
        while q.front().is_some_and(|(t, _)| now - *t >= window) {
            q.pop_front();
        }
        let used: u64 = q.iter().map(|(_, a)| a).sum();
        if used.saturating_add(amount) > max {
            return false;
        }
        q.push_back((now, amount));
        true
    }

    /// What is left of `max` in the window ending at `now`, without spending.
    pub fn left(
        &self,
        bucket: &'static str,
        key: &str,
        max: u64,
        window: TimeDelta,
        now: DateTime<Utc>,
    ) -> u64 {
        let m = self.spent.lock().unwrap();
        let used: u64 = m.get(&(bucket, key.to_string())).map_or(0, |q| {
            q.iter()
                .filter(|(t, _)| now - *t < window)
                .map(|(_, a)| a)
                .sum()
        });
        max.saturating_sub(used)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_slide() {
        let b = Budget::default();
        let t0 = Utc::now();
        let min = TimeDelta::minutes(1);
        assert!(b.spend("x", "k", 60, 100, min, t0));
        assert!(!b.spend("x", "k", 41, 100, min, t0), "over the cap");
        assert!(b.spend("x", "other", 100, 100, min, t0), "keys are apart");
        assert_eq!(b.left("x", "k", 100, min, t0), 40);
        assert!(b.spend("x", "k", 40, 100, min, t0 + TimeDelta::seconds(30)));
        // The first spend leaves the window.
        let later = t0 + TimeDelta::seconds(61);
        assert_eq!(b.left("x", "k", 100, min, later), 60);
        assert!(b.spend("x", "k", 60, 100, min, later));
    }

    fn ws_request(headers: &[(&str, &str)]) -> Request {
        let mut b = Request::builder()
            .method(Method::GET)
            .uri("/api/ws")
            .header("connection", "Upgrade")
            .header("upgrade", "websocket");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        b.body(axum::body::Body::empty()).unwrap()
    }

    /// Behind a proxy that rewrites `Host`, `X-Forwarded-Host` or `HQC_PUBLIC_ORIGINS` carries
    /// the public name (SPEC §20).
    #[test]
    fn origin_behind_a_proxy() {
        let public = "https://hqc.example.com";
        assert!(check_origin(
            &ws_request(&[("host", "hqc.example.com"), ("origin", public)]),
            &[]
        )
        .is_ok());
        assert!(check_origin(
            &ws_request(&[("host", "hqc-hub:8080"), ("origin", public)]),
            &[]
        )
        .is_err());
        assert!(check_origin(
            &ws_request(&[
                ("host", "hqc-hub:8080"),
                ("x-forwarded-host", "hqc.example.com"),
                ("origin", public)
            ]),
            &[]
        )
        .is_ok());
        assert!(check_origin(
            &ws_request(&[
                ("host", "hqc-hub:8080"),
                ("x-forwarded-host", "evil.example"),
                ("origin", public)
            ]),
            &[]
        )
        .is_err());
        let list = vec!["https://hqc.example.com".to_string()];
        assert!(check_origin(
            &ws_request(&[("host", "hqc-hub:8080"), ("origin", public)]),
            &list
        )
        .is_ok());
        assert!(check_origin(
            &ws_request(&[
                ("host", "hqc-hub:8080"),
                ("origin", "https://HQC.example.com:443")
            ]),
            &list
        )
        .is_ok());
        assert!(check_origin(
            &ws_request(&[
                ("host", "hqc.example.com"),
                ("origin", "https://evil.example")
            ]),
            &list
        )
        .is_err());
        // With a list, the request's own host no longer counts.
        assert!(check_origin(
            &ws_request(&[
                ("host", "other.example"),
                ("origin", "https://other.example")
            ]),
            &list
        )
        .is_err());
        assert_eq!(
            normalize_origin("https://HQC.example.com:443").as_deref(),
            Some("https://hqc.example.com")
        );
        assert_eq!(normalize_origin("https://hqc.example.com/x"), None);
        assert_eq!(
            crate::config::public_origins(Some(" https://a.example , https://b.example:8443 "))
                .unwrap(),
            ["https://a.example", "https://b.example:8443"]
        );
        assert!(crate::config::public_origins(Some("http://a.example")).is_err());
    }

    #[test]
    fn origins() {
        assert!(same_origin("https://hqc.example.com", "hqc.example.com"));
        assert!(same_origin(
            "https://HQC.example.com",
            "hqc.example.com:443"
        ));
        assert!(same_origin("http://127.0.0.1:5173", "127.0.0.1:5173"));
        assert!(same_origin("http://localhost:8080", "localhost:8080"));
        assert!(same_origin("http://[::1]:8080", "[::1]:8080"));
        for (o, h) in [
            ("https://evil.example", "hqc.example.com"),
            ("http://hqc.example.com", "hqc.example.com"),
            ("http://192.168.1.5:5173", "192.168.1.5:5173"),
            ("https://hqc.example.com.evil", "hqc.example.com"),
            ("https://hqc.example.com/x", "hqc.example.com"),
            ("null", "hqc.example.com"),
            ("ftp://hqc.example.com", "hqc.example.com"),
            ("https://127.0.0.1:1", "127.0.0.1:2"),
        ] {
            assert!(!same_origin(o, h), "{o} vs {h}");
        }
    }

    #[test]
    fn artifact_origin_in_csp() {
        assert_eq!(client_csp(None), SECURITY_HEADERS[0].1);
        let c = client_csp(Some("https://artifacts.example.com"));
        assert!(
            c.contains("img-src 'self' data: https://artifacts.example.com;"),
            "{c}"
        );
        assert!(
            c.contains("frame-src https://artifacts.example.com;"),
            "{c}"
        );
        assert!(
            c.contains("connect-src 'self' https://artifacts.example.com wss: https:;"),
            "{c}"
        );
        assert!(c.contains("frame-ancestors 'none'"), "{c}");
    }

    #[test]
    fn loopback_hosts() {
        for h in [
            "localhost",
            "127.0.0.1:80",
            "127.1.2.3",
            "[::1]:1",
            "LOCALHOST:5",
        ] {
            assert!(is_loopback_host(h), "{h}");
        }
        for h in [
            "hqc.example.com",
            "10.0.0.1:80",
            "[::2]:1",
            "localhost.evil",
        ] {
            assert!(!is_loopback_host(h), "{h}");
        }
    }

    #[test]
    fn limiter_windows_per_key() {
        let l = RateLimiter::default();
        for _ in 0..3 {
            assert!(l.allow("b", "1.1.1.1", 3));
        }
        assert!(!l.allow("b", "1.1.1.1", 3));
        assert!(l.exhausted("b", "1.1.1.1", 3));
        assert!(l.allow("b", "2.2.2.2", 3));
        assert!(l.allow("other", "1.1.1.1", 3));
    }
}
