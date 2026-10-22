//! Browser-facing protections and limits of unauthenticated routes (SPEC §20).
//!
//! - [`guard`] (middleware on the whole router): a WebSocket upgrade on `/api/*` must carry an
//!   `Origin` equal to the request's own host, and any other non-GET request on `/api/*` with
//!   an `Origin` must match it too (cross-site WebSocket hijacking, CSRF with the Access
//!   cookie). The origin's scheme is `https`, or `http` for a loopback host (development).
//!   Every response gets the security headers ([`SECURITY_HEADERS`]).
//! - [`RateLimiter`]: sliding one-minute windows per key, for `/worker/*` by client address
//!   ([`ClientIp`]).

use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    extract::{ConnectInfo, FromRequestParts, Request},
    http::{header, request::Parts, HeaderValue, Method},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::error::ApiError;

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

pub async fn guard(req: Request, next: Next) -> Response {
    let mut resp = match check_origin(&req) {
        Ok(()) => next.run(req).await,
        Err(why) => {
            tracing::warn!(path = %req.uri().path(), "{why}");
            ApiError::Forbidden("cross-origin request refused".into()).into_response()
        }
    };
    let h = resp.headers_mut();
    for (k, v) in SECURITY_HEADERS {
        if !h.contains_key(*k) {
            h.insert(*k, HeaderValue::from_static(v));
        }
    }
    resp
}

fn check_origin(req: &Request) -> Result<(), &'static str> {
    let headers = req.headers();
    if !req.uri().path().starts_with("/api/") {
        return Ok(());
    }
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
        Some(o) => match host {
            Some(h) if same_origin(o, h) => Ok(()),
            _ => Err("Origin does not match the hub's host"),
        },
    }
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

/// The client's address: `CF-Connecting-IP` from Cloudflare (the hub is reachable only
/// through the tunnel, SPEC §20), else the TCP peer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClientIp(pub String);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        if let Some(ip) = parts
            .headers
            .get("cf-connecting-ip")
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

#[cfg(test)]
mod tests {
    use super::*;

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
