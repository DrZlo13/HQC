//! Sign-in of the native iOS client (SPEC §6 "Native client sign-in"). The app cannot read the
//! session cookies of the system browser, and identity providers such as Google refuse a login
//! page embedded in the app, so the app signs in through the system browser
//! (`ASWebAuthenticationSession`) with a PKCE-style exchange:
//!
//! 1. The app opens `GET /api/native/login?challenge=<base64url(sha256(verifier))>&state=<s>`.
//!    The route is behind Access or the OIDC reverse proxy like every `/api/*`, so the person
//!    signs in there first; the hub then keeps the credential that gets requests past the front
//!    proxy under a random one-time code for [`CODE_TTL`] and redirects to
//!    `hqc-app://signed-in?code=<code>&state=<s>`.
//! 2. The app posts `{code, verifier}` to `POST /worker/native/token`, outside auth as every
//!    `/worker/*` (RUNBOOK §1.6, §1.9), and gets the credential:
//!    - Access mode: `{token, cookie}` (the Access JWT and `CF_Authorization=<jwt>`, keeping
//!      `token` for older builds);
//!    - OIDC mode: `{cookie}` (the proxy's `Cookie` header, e.g. `_oauth2_proxy*`), or `{bearer}`
//!      (the verified ID token) when no cookies were forwarded;
//!    - Dev mode: `{token: ""}`.
//!
//! The credential never travels in a URL: another app that claims the `hqc-app` scheme and catches
//! the redirect holds a code it cannot exchange without the verifier, which never leaves the app.

use std::time::Duration;

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::time::Instant;

use crate::{
    app::AppState,
    auth::{Auth, CurrentUser, ACCESS_HEADER},
    error::{ApiError, ApiResult},
    security::ClientIp,
};

/// The scheme of the redirect back to the app; fixed, so the route never redirects elsewhere.
pub const APP_SCHEME: &str = "hqc-app";
/// How long a code waits for its exchange.
pub const CODE_TTL: Duration = Duration::from_secs(120);
/// Codes kept at once; a flood of sign-ins drops the oldest.
const MAX_CODES: usize = 256;
/// Largest `Cookie` header kept for an OIDC proxy sign-in (split oauth2-proxy sessions are
/// commonly 4–12 KB, RUNBOOK §1.9).
const MAX_COOKIE_BYTES: usize = 64 * 1024;

enum NativeCredential {
    Access(String),
    Cookie(String),
    Bearer(String),
    None,
}

pub struct NativeCode {
    credential: NativeCredential,
    challenge: String,
    issued: Instant,
}

/// Join all `Cookie` headers of the request into one `Cookie` header value (HTTP/2 splits them).
fn cookie_header(headers: &HeaderMap) -> Option<String> {
    let joined = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    pub challenge: String,
    pub state: String,
}

/// A base64url SHA-256 (43 characters, no padding).
fn valid_challenge(c: &str) -> bool {
    c.len() == 43
        && c.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The app's own value, echoed back: short and URL-safe, so it needs no escaping.
fn valid_state(s: &str) -> bool {
    (8..=128).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

pub fn challenge_of(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// `GET /api/native/login`: see the module docs.
pub async fn login(
    State(app): State<AppState>,
    _user: CurrentUser,
    headers: HeaderMap,
    Query(q): Query<LoginQuery>,
) -> ApiResult<Response> {
    if !valid_challenge(&q.challenge) {
        return Err(ApiError::BadRequest(
            "challenge must be base64url(sha256(verifier)), 43 characters".into(),
        ));
    }
    if !valid_state(&q.state) {
        return Err(ApiError::BadRequest(
            "state must be 8 to 128 characters of [A-Za-z0-9._-]".into(),
        ));
    }
    let credential = match &app.auth {
        // `CurrentUser` checked the header already.
        Auth::Access(_) => NativeCredential::Access(
            headers
                .get(ACCESS_HEADER)
                .and_then(|h| h.to_str().ok())
                .unwrap_or_default()
                .to_string(),
        ),
        // Dev mode lets every request in: the app needs no credential.
        Auth::Dev { .. } => NativeCredential::None,
        // Behind an OIDC reverse proxy (e.g. Traefik + oauth2-proxy, RUNBOOK §1.9), the proxy's
        // session cookie(s) carry the sign-in; fall back to the verified bearer token when a
        // gateway authenticates without cookies.
        Auth::Oidc(_) => match cookie_header(&headers) {
            Some(c) if c.len() > MAX_COOKIE_BYTES => {
                return Err(ApiError::BadRequest("cookie header is too large".into()));
            }
            Some(c) => NativeCredential::Cookie(c),
            None => NativeCredential::Bearer(
                crate::auth::bearer(&headers)
                    .unwrap_or_default()
                    .to_string(),
            ),
        },
    };
    let code = crate::auth::new_secret("nc");
    {
        let mut m = app.native_codes.lock().unwrap();
        m.retain(|_, c| c.issued.elapsed() < CODE_TTL);
        if m.len() >= MAX_CODES {
            if let Some(oldest) = m
                .iter()
                .min_by_key(|(_, c)| c.issued)
                .map(|(k, _)| k.clone())
            {
                m.remove(&oldest);
            }
        }
        m.insert(
            code.clone(),
            NativeCode {
                credential,
                challenge: q.challenge,
                issued: Instant::now(),
            },
        );
    }
    let location = format!("{APP_SCHEME}://signed-in?code={code}&state={}", q.state);
    Ok((
        StatusCode::FOUND,
        [
            (header::LOCATION, location),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
pub struct TokenRequest {
    pub code: String,
    pub verifier: String,
}

/// `POST /worker/native/token`: a code and its verifier for the Access token, once.
pub async fn token(
    State(app): State<AppState>,
    ClientIp(ip): ClientIp,
    Json(b): Json<TokenRequest>,
) -> ApiResult<Response> {
    if !app
        .limiter
        .allow("native_token", &ip, app.config.limits.enroll_per_min)
    {
        return Err(ApiError::TooManyRequests);
    }
    // Taken out whatever the verifier: a wrong guess burns the code.
    let entry = app.native_codes.lock().unwrap().remove(&b.code);
    let Some(entry) = entry else {
        return Err(ApiError::Invalid("bad_code", "unknown or used code".into()));
    };
    if entry.issued.elapsed() >= CODE_TTL {
        return Err(ApiError::Invalid(
            "bad_code",
            "the code expired; sign in again".into(),
        ));
    }
    if b.verifier.len() < 43
        || b.verifier.len() > 128
        || challenge_of(&b.verifier) != entry.challenge
    {
        return Err(ApiError::Invalid(
            "bad_code",
            "the verifier does not match".into(),
        ));
    }
    let body = match entry.credential {
        NativeCredential::Access(t) => json!({
            "token": t,
            "cookie": format!("CF_Authorization={t}"),
        }),
        NativeCredential::Cookie(c) => json!({ "cookie": c }),
        NativeCredential::Bearer(b) => json!({ "bearer": b }),
        NativeCredential::None => json!({ "token": "" }),
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_and_state_rules() {
        let v = "a".repeat(64);
        let c = challenge_of(&v);
        assert!(valid_challenge(&c));
        assert!(!valid_challenge("short"));
        assert!(!valid_challenge(&format!("{}=", &c[..42])));
        assert!(valid_state("abcDEF12-_."));
        assert!(!valid_state("short"));
        assert!(!valid_state("has space in it"));
        assert!(!valid_state("a&b=c&evil=xxxxx"));
    }

    #[test]
    fn cookie_header_joins_multiple_headers() {
        let mut h = HeaderMap::new();
        assert_eq!(cookie_header(&h), None);
        h.append(header::COOKIE, "  ".parse().unwrap());
        assert_eq!(cookie_header(&h), None);
        h.append(header::COOKIE, "_oauth2_proxy_0=aaa==".parse().unwrap());
        h.append(header::COOKIE, "_oauth2_proxy_1=bbb==".parse().unwrap());
        assert_eq!(
            cookie_header(&h).as_deref(),
            Some("_oauth2_proxy_0=aaa==; _oauth2_proxy_1=bbb==")
        );
    }
}
