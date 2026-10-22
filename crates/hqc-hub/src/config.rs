//! Hub configuration from the environment.

use std::{path::PathBuf, time::Duration};

use anyhow::{bail, Context};

#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub listen: String,
    pub auth: AuthConfig,
    pub vapid: Option<VapidConfig>,
    /// The built web client (`client/dist`) served on `/`; `None`: the hub serves only the APIs.
    pub client_dir: Option<PathBuf>,
    pub timing: Timing,
}

/// How people are authenticated on `/api/*` (SPEC §6, §20).
#[derive(Clone, Debug)]
pub enum AuthConfig {
    /// Cloudflare Access: the `Cf-Access-Jwt-Assertion` JWT, checked against the team's JWKS.
    Access {
        /// `<team>.cloudflareaccess.com`; the issuer is `https://<team_domain>`.
        team_domain: String,
        /// The Access application audience tag.
        aud: String,
        /// Override of `https://<team_domain>/cdn-cgi/access/certs`; tests only.
        jwks_url: Option<String>,
    },
    /// Local development without Access: every request is this user. Only with `HQC_DEV=1`.
    Dev { email: String },
}

/// VAPID keys for Web Push (SPEC §16), base64url without padding as most generators print them.
#[derive(Clone)]
pub struct VapidConfig {
    pub private_key: String,
    pub public_key: String,
    /// `sub` claim, `mailto:` or `https:` URL.
    pub subject: String,
}

impl std::fmt::Debug for VapidConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VapidConfig")
            .field("public_key", &self.public_key)
            .field("subject", &self.subject)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct Timing {
    /// WebSocket ping interval to workers and clients (SPEC §5: heartbeat 20 s).
    pub heartbeat: Duration,
    /// A worker connection with no traffic for this long is dropped.
    pub worker_idle_timeout: Duration,
    /// Delay before re-sending `session.start` after a retryable failure.
    pub start_retry_delay: Duration,
    /// Failed starts in a row before pending input is marked `failed` (SPEC §5.2).
    pub max_start_failures: i32,
    /// How often expired approval / clarify requests are looked for.
    pub expiry_interval: Duration,
    /// Lifetime of an enrollment code (SPEC §5.1).
    pub enroll_ttl: Duration,
    /// `ask_timeout_s` in `session.start` (SPEC §7.1).
    pub ask_timeout_s: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            heartbeat: Duration::from_secs(20),
            worker_idle_timeout: Duration::from_secs(60),
            start_retry_delay: Duration::from_secs(2),
            max_start_failures: 3,
            expiry_interval: Duration::from_secs(1),
            enroll_ttl: Duration::from_secs(600),
            ask_timeout_s: 600,
        }
    }
}

impl Config {
    /// Variables: `HQC_DATABASE_URL`, `HQC_HUB_LISTEN`, `HQC_ACCESS_TEAM_DOMAIN`,
    /// `HQC_ACCESS_AUD`, `HQC_DEV`, `HQC_DEV_USER`, `HQC_VAPID_PRIVATE`, `HQC_VAPID_PUBLIC`,
    /// `HQC_VAPID_SUBJECT`, `HQC_CLIENT_DIR`.
    pub fn from_env() -> anyhow::Result<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let database_url = var("HQC_DATABASE_URL").context("HQC_DATABASE_URL is not set")?;
        let listen = var("HQC_HUB_LISTEN").unwrap_or_else(|| "127.0.0.1:8080".into());

        let dev = var("HQC_DEV").as_deref() == Some("1");
        let team_domain = var("HQC_ACCESS_TEAM_DOMAIN");
        let aud = var("HQC_ACCESS_AUD");
        let auth = if dev {
            if team_domain.is_some() || aud.is_some() {
                bail!("HQC_DEV=1 together with HQC_ACCESS_*: refusing to start in an ambiguous auth mode");
            }
            let email = var("HQC_DEV_USER").context("HQC_DEV=1 requires HQC_DEV_USER=<email>")?;
            // Without Access every request is HQC_DEV_USER: never reachable from outside.
            if !is_loopback_listen(&listen) {
                bail!("HQC_DEV=1 requires a loopback HQC_HUB_LISTEN (127.0.0.1, [::1] or localhost), got {listen}");
            }
            AuthConfig::Dev { email }
        } else {
            if var("HQC_DEV_USER").is_some() {
                tracing::warn!("HQC_DEV_USER is ignored without HQC_DEV=1");
            }
            AuthConfig::Access {
                team_domain: normalize_team_domain(
                    &team_domain.context("HQC_ACCESS_TEAM_DOMAIN is not set")?,
                ),
                aud: aud.context("HQC_ACCESS_AUD is not set")?,
                jwks_url: None,
            }
        };

        let vapid = match (var("HQC_VAPID_PRIVATE"), var("HQC_VAPID_PUBLIC")) {
            (Some(private_key), Some(public_key)) => Some(VapidConfig {
                private_key,
                public_key,
                subject: var("HQC_VAPID_SUBJECT").unwrap_or_else(|| "mailto:hqc@localhost".into()),
            }),
            (None, None) => None,
            _ => bail!("set both HQC_VAPID_PRIVATE and HQC_VAPID_PUBLIC, or neither"),
        };

        let client_dir = var("HQC_CLIENT_DIR").map(PathBuf::from);
        if let Some(dir) = &client_dir {
            if !dir.join("index.html").is_file() {
                bail!(
                    "HQC_CLIENT_DIR={}: no index.html (build the client: npm run build in client/)",
                    dir.display()
                );
            }
        }

        Ok(Self {
            database_url,
            listen,
            auth,
            vapid,
            client_dir,
            timing: Timing::default(),
        })
    }
}

/// `HQC_HUB_LISTEN` binds a loopback address only: `127.0.0.0/8`, `[::1]` or `localhost`.
pub fn is_loopback_listen(listen: &str) -> bool {
    if let Ok(addr) = listen.parse::<std::net::SocketAddr>() {
        return addr.ip().is_loopback();
    }
    listen
        .rsplit_once(':')
        .is_some_and(|(host, port)| host == "localhost" && port.parse::<u16>().is_ok())
}

/// `https://team.cloudflareaccess.com/` -> `team.cloudflareaccess.com`.
pub fn normalize_team_domain(s: &str) -> String {
    s.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_mode_listens_on_loopback_only() {
        for ok in [
            "127.0.0.1:8080",
            "127.0.0.2:1",
            "[::1]:8080",
            "localhost:8080",
        ] {
            assert!(is_loopback_listen(ok), "{ok}");
        }
        for bad in [
            "0.0.0.0:8080",
            "[::]:8080",
            "192.168.1.5:8080",
            "hub.lan:8080",
            "localhost",
        ] {
            assert!(!is_loopback_listen(bad), "{bad}");
        }
    }

    #[test]
    fn team_domain_is_normalized() {
        assert_eq!(
            normalize_team_domain("https://acme.cloudflareaccess.com/"),
            "acme.cloudflareaccess.com"
        );
        assert_eq!(
            normalize_team_domain("acme.cloudflareaccess.com"),
            "acme.cloudflareaccess.com"
        );
    }
}
