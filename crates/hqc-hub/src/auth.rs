//! Authentication (SPEC §6, §20).
//!
//! People: the Cloudflare Access JWT from `Cf-Access-Jwt-Assertion`, signature checked
//! against the team's JWKS, issuer and audience from config; an email header without a JWT
//! is never accepted. `HQC_DEV=1` replaces this with a fixed user for local development.
//!
//! Workers: a random 256-bit bearer token; the database keeps only its sha256.

use std::time::{Duration, Instant};

use axum::{extract::FromRequestParts, http::request::Parts};
use base64::Engine;
use hqc_proto::api::Theme;
use jsonwebtoken::{jwk::JwkSet, Algorithm, DecodingKey, Validation};
use rand::RngCore;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::{app::AppState, config::AuthConfig, db, error::ApiError};

pub const ACCESS_HEADER: &str = "cf-access-jwt-assertion";

/// How often the JWKS may be re-fetched because of an unknown `kid`.
const JWKS_MIN_REFRESH: Duration = Duration::from_secs(30);
/// Keys are re-fetched at least this often (Access rotates them).
const JWKS_MAX_AGE: Duration = Duration::from_secs(3600);

pub enum Auth {
    Access(Box<AccessVerifier>),
    Dev { email: String },
}

impl Auth {
    pub fn from_config(c: &AuthConfig) -> Self {
        match c {
            AuthConfig::Access {
                team_domain,
                aud,
                jwks_url,
            } => Auth::Access(Box::new(AccessVerifier::new(
                team_domain,
                aud,
                jwks_url.clone(),
            ))),
            AuthConfig::Dev { email } => Auth::Dev {
                email: email.clone(),
            },
        }
    }
}

pub struct AccessVerifier {
    issuer: String,
    aud: String,
    certs_url: String,
    http: reqwest::Client,
    /// The last fetched key set and when it was fetched. Readers never wait for a fetch.
    keys: RwLock<Option<(JwkSet, Instant)>>,
    /// One fetch at a time; remembers failures for the backoff.
    refresh: Mutex<Refresh>,
    min_refresh: Duration,
    max_age: Duration,
}

#[derive(Default)]
struct Refresh {
    /// When the last fetch failed and how long to wait before the next one.
    failed: Option<(Instant, Duration)>,
}

/// Backoff after a failed JWKS fetch, doubling up to [`JWKS_BACKOFF_MAX`].
const JWKS_BACKOFF_MIN: Duration = Duration::from_secs(5);
const JWKS_BACKOFF_MAX: Duration = Duration::from_secs(300);

/// What a client learns about a rejected Access token; the reason goes to the log.
const BAD_TOKEN: &str = "invalid access token";

#[derive(Debug, serde::Deserialize)]
struct AccessClaims {
    #[serde(default)]
    email: Option<String>,
}

impl AccessVerifier {
    pub fn new(team_domain: &str, aud: &str, jwks_url: Option<String>) -> Self {
        let issuer = format!("https://{team_domain}");
        let certs_url = jwks_url.unwrap_or_else(|| format!("{issuer}/cdn-cgi/access/certs"));
        Self {
            issuer,
            aud: aud.to_string(),
            certs_url,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("http client"),
            keys: RwLock::new(None),
            refresh: Mutex::new(Refresh::default()),
            min_refresh: JWKS_MIN_REFRESH,
            max_age: JWKS_MAX_AGE,
        }
    }

    /// Verify the token and return the email it was issued for. The client gets a generic
    /// message; why the token was refused is logged.
    pub async fn verify(&self, token: &str) -> Result<String, ApiError> {
        let bad = |why: &dyn std::fmt::Display| {
            tracing::info!("access token refused: {why}");
            ApiError::Unauthorized(BAD_TOKEN.into())
        };
        let header = jsonwebtoken::decode_header(token).map_err(|e| bad(&e))?;
        if header.alg != Algorithm::RS256 {
            return Err(bad(&format_args!("algorithm {:?}", header.alg)));
        }
        let kid = header.kid.ok_or_else(|| bad(&"no kid"))?;
        let key = self
            .key(&kid)
            .await?
            .ok_or_else(|| bad(&format_args!("unknown signing key {kid:?}")))?;
        let mut v = Validation::new(Algorithm::RS256);
        v.set_issuer(&[&self.issuer]);
        v.set_audience(&[&self.aud]);
        v.set_required_spec_claims(&["exp", "iss", "aud"]);
        let data = jsonwebtoken::decode::<AccessClaims>(token, &key, &v).map_err(|e| bad(&e))?;
        data.claims
            .email
            .filter(|e| e.contains('@'))
            .ok_or_else(|| ApiError::Forbidden("access token has no email".into()))
    }

    /// The key `kid` from the cached set, refreshing it when it is old or does not know
    /// `kid`. A failed refresh keeps the previous set (Cloudflare unreachable must not lock
    /// everybody out) and backs off; the fetch runs outside the cache lock.
    async fn key(&self, kid: &str) -> Result<Option<DecodingKey>, ApiError> {
        let find = |set: &JwkSet| set.find(kid).and_then(|k| DecodingKey::from_jwk(k).ok());
        let needs_refresh = |cached: &Option<(JwkSet, Instant)>| match cached {
            Some((set, at)) => {
                at.elapsed() >= self.max_age
                    || (set.find(kid).is_none() && at.elapsed() >= self.min_refresh)
            }
            None => true,
        };
        {
            let keys = self.keys.read().await;
            if !needs_refresh(&keys) {
                return Ok(keys.as_ref().and_then(|(set, _)| find(set)));
            }
        }
        let mut refresh = self.refresh.lock().await;
        // Another request may have refreshed while this one waited.
        let cached = self.keys.read().await.clone();
        let backing_off = refresh.failed.is_some_and(|(at, wait)| at.elapsed() < wait);
        if !needs_refresh(&cached) || backing_off {
            return match cached {
                Some((set, _)) => Ok(find(&set)),
                None => Err(ApiError::Internal(anyhow::anyhow!(
                    "access keys unavailable (fetch backing off)"
                ))),
            };
        }
        match self.fetch().await {
            Ok(set) => {
                refresh.failed = None;
                let k = find(&set);
                *self.keys.write().await = Some((set, Instant::now()));
                Ok(k)
            }
            Err(e) => {
                let wait = match refresh.failed {
                    Some((_, w)) => (w * 2).min(JWKS_BACKOFF_MAX),
                    None => JWKS_BACKOFF_MIN,
                };
                refresh.failed = Some((Instant::now(), wait));
                match cached {
                    Some((set, _)) => {
                        tracing::warn!(error = %e, retry_in = ?wait, "access JWKS refresh failed; using the previous keys");
                        Ok(find(&set))
                    }
                    None => Err(ApiError::Internal(e.context("fetching access JWKS"))),
                }
            }
        }
    }

    async fn fetch(&self) -> anyhow::Result<JwkSet> {
        tracing::debug!(url = %self.certs_url, "fetching access JWKS");
        let set = self
            .http
            .get(&self.certs_url)
            .send()
            .await?
            .error_for_status()?
            .json::<JwkSet>()
            .await?;
        Ok(set)
    }
}

/// The authenticated person on `/api/*`.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub timezone: String,
    pub personal_project_id: Uuid,
    pub theme: Theme,
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &AppState) -> Result<Self, ApiError> {
        let email = match &app.auth {
            Auth::Dev { email } => email.clone(),
            Auth::Access(v) => {
                let token = parts
                    .headers
                    .get(ACCESS_HEADER)
                    .and_then(|h| h.to_str().ok())
                    .ok_or_else(|| {
                        ApiError::Unauthorized("missing Cf-Access-Jwt-Assertion".into())
                    })?;
                v.verify(token).await?
            }
        };
        let u = db::ensure_user(&app.db, &email).await?;
        Ok(CurrentUser {
            id: u.id,
            email: u.email,
            display_name: u.display_name,
            timezone: u.timezone,
            theme: Theme::parse(&u.theme).unwrap_or_default(),
            personal_project_id: u.personal_project_id.ok_or_else(|| {
                ApiError::Internal(anyhow::anyhow!("user without personal project"))
            })?,
        })
    }
}

/// A random 256-bit secret, base64url, with a readable prefix.
pub fn new_secret(prefix: &str) -> String {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    format!(
        "{prefix}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
    )
}

pub fn hash_secret(s: &str) -> String {
    let d = Sha256::digest(s.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// `Authorization: Bearer <token>`.
pub fn bearer(parts: &axum::http::HeaderMap) -> Option<&str> {
    parts
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

/// Human-typed enrollment code: 8 characters without look-alikes, shown as `ABCD-EFGH`.
pub fn new_enroll_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut b = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut b);
    let s: String = b
        .iter()
        .map(|x| ALPHABET[*x as usize % ALPHABET.len()] as char)
        .collect();
    format!("{}-{}", &s[..4], &s[4..])
}

/// Codes are compared case-insensitively and without the dash.
pub fn normalize_enroll_code(s: &str) -> String {
    let s: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if s.len() == 8 {
        format!("{}-{}", &s[..4], &s[4..])
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_random_and_hashed() {
        let a = new_secret("hqcw_");
        assert!(a.starts_with("hqcw_"));
        assert_ne!(a, new_secret("hqcw_"));
        assert_eq!(hash_secret(&a).len(), 64);
        assert_eq!(hash_secret(&a), hash_secret(&a));
    }

    #[tokio::test]
    async fn jwks_failure_keeps_previous_keys_and_backs_off() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let app = axum::Router::new().route(
            "/certs",
            axum::routing::get(move || {
                h.fetch_add(1, Ordering::SeqCst);
                async { axum::http::StatusCode::INTERNAL_SERVER_ERROR }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/certs", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

        let mut v = AccessVerifier::new("t.example", "aud", Some(url));
        // Nothing cached and Cloudflare down: an error, not a silent "unknown key".
        assert!(v.key("k1").await.is_err());
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        let set: JwkSet = serde_json::from_value(serde_json::json!({"keys": [
            {"kty": "RSA", "kid": "k1", "alg": "RS256", "use": "sig", "n": "AQAB", "e": "AQAB"}
        ]}))
        .unwrap();
        v.max_age = Duration::ZERO;
        v.refresh = Mutex::new(Refresh::default());
        *v.keys.write().await = Some((set, Instant::now()));
        // Stale keys, failing refresh: the previous set is used.
        assert!(v.key("k1").await.unwrap().is_some());
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        // Backing off: no new fetch for a while.
        assert!(v.key("k1").await.unwrap().is_some());
        assert!(v.key("k2").await.unwrap().is_none());
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn enroll_codes_normalize() {
        let c = new_enroll_code();
        assert_eq!(c.len(), 9);
        assert_eq!(
            normalize_enroll_code(&c.to_lowercase().replace('-', " ")),
            c
        );
    }
}
