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
use jsonwebtoken::{jwk::JwkSet, Algorithm, DecodingKey, Validation};
use rand::RngCore;
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{app::AppState, config::AuthConfig, db, error::ApiError};

pub const ACCESS_HEADER: &str = "cf-access-jwt-assertion";

/// How often the JWKS may be re-fetched because of an unknown `kid`.
const JWKS_MIN_REFRESH: Duration = Duration::from_secs(30);
/// Keys are re-fetched at least this often (Access rotates them).
const JWKS_MAX_AGE: Duration = Duration::from_secs(3600);

pub enum Auth {
    Access(AccessVerifier),
    Dev { email: String },
}

impl Auth {
    pub fn from_config(c: &AuthConfig) -> Self {
        match c {
            AuthConfig::Access {
                team_domain,
                aud,
                jwks_url,
            } => Auth::Access(AccessVerifier::new(team_domain, aud, jwks_url.clone())),
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
    keys: RwLock<Option<(JwkSet, Instant)>>,
}

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
        }
    }

    /// Verify the token and return the email it was issued for.
    pub async fn verify(&self, token: &str) -> Result<String, ApiError> {
        let bad = |m: &str| ApiError::Unauthorized(m.to_string());
        let header =
            jsonwebtoken::decode_header(token).map_err(|_| bad("malformed access token"))?;
        if header.alg != Algorithm::RS256 {
            return Err(bad("unexpected token algorithm"));
        }
        let kid = header.kid.ok_or_else(|| bad("token without kid"))?;
        let key = self
            .key(&kid)
            .await?
            .ok_or_else(|| bad("unknown signing key"))?;
        let mut v = Validation::new(Algorithm::RS256);
        v.set_issuer(&[&self.issuer]);
        v.set_audience(&[&self.aud]);
        v.set_required_spec_claims(&["exp", "iss", "aud"]);
        let data = jsonwebtoken::decode::<AccessClaims>(token, &key, &v)
            .map_err(|e| ApiError::Unauthorized(format!("invalid access token: {e}")))?;
        data.claims
            .email
            .filter(|e| e.contains('@'))
            .ok_or_else(|| ApiError::Forbidden("access token has no email".into()))
    }

    async fn key(&self, kid: &str) -> Result<Option<DecodingKey>, ApiError> {
        {
            let keys = self.keys.read().await;
            if let Some((set, at)) = keys.as_ref() {
                if at.elapsed() < JWKS_MAX_AGE {
                    if let Some(k) = set.find(kid) {
                        return Ok(DecodingKey::from_jwk(k).ok());
                    }
                    if at.elapsed() < JWKS_MIN_REFRESH {
                        return Ok(None);
                    }
                }
            }
        }
        let mut keys = self.keys.write().await;
        // Another request may have refreshed while we waited for the lock.
        let fresh = matches!(keys.as_ref(), Some((_, at)) if at.elapsed() < JWKS_MIN_REFRESH);
        if !fresh {
            let set = self.fetch().await.map_err(ApiError::Internal)?;
            *keys = Some((set, Instant::now()));
        }
        let (set, _) = keys.as_ref().expect("keys just set");
        Ok(set.find(kid).and_then(|k| DecodingKey::from_jwk(k).ok()))
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
