//! Authentication (SPEC §6, §20).
//!
//! People: the Cloudflare Access JWT from `Cf-Access-Jwt-Assertion`, signature checked
//! against the team's JWKS, issuer and audience from config; an email header without a JWT
//! is never accepted. With `HQC_OIDC_*` it is instead an OIDC ID token from
//! `Authorization: Bearer` (put there by a reverse proxy such as oauth2-proxy), checked the
//! same way against the configured issuer, audience and JWKS URL, and in addition with
//! `email_verified: true`, when it has a `typ`, `typ: ID`, with an array `aud`, `azp` equal to
//! the client id, and an email from `HQC_OIDC_ALLOWED_EMAILS`; the user is the one bound to
//! its `sub`, and `Cf-Access-Jwt-Assertion` is not read. `HQC_DEV=1` replaces this with a
//! fixed user for local development.
//!
//! Workers: a random 256-bit bearer token; the database keeps only its sha256.

use std::time::{Duration, Instant};

use axum::{extract::FromRequestParts, http::request::Parts};
use base64::Engine;
use hqc_proto::api::{Look, ProjectSort, Theme};
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
    Oidc(Box<AccessVerifier>),
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
            AuthConfig::Oidc {
                issuer,
                aud,
                jwks_url,
                allowed_emails,
            } => Auth::Oidc(Box::new(AccessVerifier::oidc(
                issuer,
                aud,
                jwks_url,
                allowed_emails,
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
    /// OIDC mode: only ID tokens (`typ`, when present, is `ID`) whose `email_verified` is
    /// `true`. Without it any account of the realm could set its email to someone else's.
    id_token: bool,
    /// OIDC mode: the only emails let in (`HQC_OIDC_ALLOWED_EMAILS`, lowercase); without it any
    /// account of the realm with an ID token for the client would become a hub user, however
    /// the token came.
    allowed_emails: Vec<String>,
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

/// What a client learns about a rejected token; the reason goes to the log.
const BAD_TOKEN: &str = "invalid token";

/// A rejected token: [`BAD_TOKEN`] to the client, `why` to the log.
fn refused(why: &dyn std::fmt::Display) -> ApiError {
    tracing::info!("token refused: {why}");
    ApiError::Unauthorized(BAD_TOKEN.into())
}

#[derive(Debug, serde::Deserialize)]
struct AccessClaims {
    #[serde(default)]
    email: Option<String>,
    /// Read in OIDC mode only; any JSON so that Access tokens decode as before.
    #[serde(default)]
    email_verified: Option<serde_json::Value>,
    #[serde(default)]
    typ: Option<serde_json::Value>,
    #[serde(default)]
    sub: Option<serde_json::Value>,
    #[serde(default)]
    aud: Option<serde_json::Value>,
    #[serde(default)]
    azp: Option<serde_json::Value>,
}

/// Who a verified token was issued for.
pub struct Identity {
    pub email: String,
    /// OIDC mode only: the token's `sub`, the key of the hub user (the issuer is fixed by
    /// `HQC_OIDC_ISSUER`).
    pub oidc_sub: Option<String>,
}

impl AccessVerifier {
    pub fn new(team_domain: &str, aud: &str, jwks_url: Option<String>) -> Self {
        let issuer = format!("https://{team_domain}");
        let certs_url = jwks_url.unwrap_or_else(|| format!("{issuer}/cdn-cgi/access/certs"));
        Self::with_issuer(&issuer, aud, &certs_url)
    }

    /// Tokens of exactly `issuer` for `aud`, signed by a key from the JWKS at `certs_url`.
    pub fn with_issuer(issuer: &str, aud: &str, certs_url: &str) -> Self {
        Self {
            issuer: issuer.to_string(),
            aud: aud.to_string(),
            certs_url: certs_url.to_string(),
            id_token: false,
            allowed_emails: Vec::new(),
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

    /// As [`Self::with_issuer`], for OIDC ID tokens: also `email_verified: true`, when the
    /// token has a `typ`, `typ: ID` (Keycloak's access tokens are `Bearer`), with an array
    /// `aud`, `azp: <aud>` (OIDC Core 3.1.3.7), an email of `allowed_emails` and a `sub`.
    pub fn oidc(issuer: &str, aud: &str, certs_url: &str, allowed_emails: &[String]) -> Self {
        Self {
            id_token: true,
            allowed_emails: allowed_emails.to_vec(),
            ..Self::with_issuer(issuer, aud, certs_url)
        }
    }

    /// Verify the token and return who it was issued for. The client gets a generic
    /// message; why the token was refused is logged.
    pub async fn verify(&self, token: &str) -> Result<Identity, ApiError> {
        let bad = refused;
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
        v.validate_nbf = true;
        let claims = jsonwebtoken::decode::<AccessClaims>(token, &key, &v)
            .map_err(|e| bad(&e))?
            .claims;
        if self.id_token {
            if let Some(typ) = claims.typ.as_ref().filter(|t| t.as_str() != Some("ID")) {
                return Err(bad(&format_args!("typ {typ} is not an ID token")));
            }
            let azp = claims.azp.as_ref().and_then(|a| a.as_str());
            if claims.aud.as_ref().is_some_and(|a| a.is_array()) && azp != Some(self.aud.as_str()) {
                return Err(bad(&format_args!(
                    "aud is an array and azp {azp:?} is not the client id"
                )));
            }
            if claims.email_verified != Some(serde_json::Value::Bool(true)) {
                tracing::info!("token refused: email_verified is not true");
                return Err(ApiError::Forbidden("the email is not verified".into()));
            }
        }
        let email = claims
            .email
            .filter(|e| e.contains('@'))
            .ok_or_else(|| ApiError::Forbidden("the token has no email".into()))?;
        if !self.id_token {
            return Ok(Identity {
                email,
                oidc_sub: None,
            });
        }
        if !self.allowed_emails.contains(&email.trim().to_lowercase()) {
            return Err(bad(&format_args!(
                "{email} is not in HQC_OIDC_ALLOWED_EMAILS"
            )));
        }
        let sub = claims
            .sub
            .as_ref()
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| bad(&"no sub"))?;
        Ok(Identity {
            email,
            oidc_sub: Some(sub.to_string()),
        })
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
                    "signing keys unavailable (fetch backing off)"
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
                        tracing::warn!(error = %e, retry_in = ?wait, "JWKS refresh failed; using the previous keys");
                        Ok(find(&set))
                    }
                    None => Err(ApiError::Internal(e.context("fetching JWKS"))),
                }
            }
        }
    }

    async fn fetch(&self) -> anyhow::Result<JwkSet> {
        tracing::debug!(url = %self.certs_url, "fetching JWKS");
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
    pub look: Look,
    pub project_sort: ProjectSort,
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &AppState) -> Result<Self, ApiError> {
        let id = match &app.auth {
            Auth::Dev { email } => Identity {
                email: email.clone(),
                oidc_sub: None,
            },
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
            Auth::Oidc(v) => {
                let token = bearer(&parts.headers)
                    .ok_or_else(|| ApiError::Unauthorized("missing bearer token".into()))?;
                v.verify(token).await?
            }
        };
        let u = match &id.oidc_sub {
            None => db::ensure_user(&app.db, &id.email).await?,
            Some(sub) => db::ensure_oidc_user(&app.db, &id.email, sub)
                .await?
                .ok_or_else(|| {
                    refused(&format_args!(
                        "{} belongs to a user bound to another subject",
                        id.email
                    ))
                })?,
        };
        Ok(CurrentUser {
            id: u.id,
            email: u.email,
            display_name: u.display_name,
            timezone: u.timezone,
            theme: Theme::parse(&u.theme).unwrap_or_default(),
            look: Look::parse(&u.look).unwrap_or_default(),
            project_sort: ProjectSort::parse(&u.project_sort).unwrap_or_default(),
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
