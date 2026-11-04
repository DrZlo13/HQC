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
    pub limits: Limits,
    pub artifacts: ArtifactConfig,
    /// `HQC_PUBLIC_ORIGINS`: the origins browsers reach the hub at (`https://hqc.example.com`),
    /// comma-separated. When set, `Origin` on `/api/*` is checked against this list instead of
    /// the request's `Host`, which a reverse proxy may rewrite (SPEC §20). Empty: the `Host`
    /// or `X-Forwarded-Host` of the request.
    pub public_origins: Vec<String>,
}

/// The artifact store and its signed links (SPEC §12).
#[derive(Clone)]
pub struct ArtifactConfig {
    /// `HQC_ARTIFACT_DIR`: content by sha256, `<dir>/<sha[0..2]>/<sha>`.
    pub dir: PathBuf,
    /// `HQC_ARTIFACT_PUBLIC_BASE`: origin of the artifact domain (`https://artifacts.<hub>`),
    /// outside Cloudflare Access. Signed links point there, HTML is shown inline only on it.
    /// `None`: links are relative to the hub and HTML is only downloaded.
    pub public_base: Option<String>,
    /// Largest artifact (SPEC §12: 100 MB).
    pub max_bytes: u64,
    /// Lifetime of a signed link (SPEC §12: 10 minutes).
    pub link_ttl: Duration,
    /// HMAC key of signed links: `HQC_ARTIFACT_KEY`, else random per process (links of a
    /// previous run stop working; the client asks for a new one).
    pub key: Vec<u8>,
    /// `HQC_ARTIFACT_RETENTION_DAYS` (T21, default 30, `0` off): artifacts no event refers to
    /// and unused for this long are removed by a daily cleanup.
    pub retention: Option<Duration>,
}

pub const DEFAULT_ARTIFACT_RETENTION_DAYS: u64 = 30;

/// `HQC_ARTIFACT_RETENTION_DAYS`: whole days, 0 = never remove, unset = 30.
pub fn retention_from(v: Option<&str>) -> anyhow::Result<Option<Duration>> {
    let days = match v.map(str::trim).filter(|v| !v.is_empty()) {
        None => DEFAULT_ARTIFACT_RETENTION_DAYS,
        Some(v) => v
            .parse::<u64>()
            .with_context(|| format!("HQC_ARTIFACT_RETENTION_DAYS={v}: expected whole days"))?,
    };
    Ok((days > 0).then(|| Duration::from_secs(days * 86_400)))
}

pub const MAX_ARTIFACT_BYTES: u64 = 100 * 1000 * 1000;

impl ArtifactConfig {
    /// Defaults with a random key.
    pub fn new(dir: PathBuf) -> Self {
        let mut key = vec![0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut key);
        Self {
            dir,
            public_base: None,
            max_bytes: MAX_ARTIFACT_BYTES,
            link_ttl: Duration::from_secs(600),
            key,
            retention: Some(Duration::from_secs(
                DEFAULT_ARTIFACT_RETENTION_DAYS * 86_400,
            )),
        }
    }

    /// `host[:port]` of [`Self::public_base`], lowercase.
    pub fn public_host(&self) -> Option<String> {
        let b = self.public_base.as_deref()?;
        let rest = b.split_once("://")?.1;
        Some(rest.trim_end_matches('/').to_ascii_lowercase())
    }
}

/// `HQC_ARTIFACT_PUBLIC_BASE`: `https://host[:port]` (or `http://` on loopback), no path.
pub fn artifact_public_base(v: &str) -> anyhow::Result<String> {
    let v = v.trim().trim_end_matches('/');
    let ok = match v.split_once("://") {
        Some(("https", h)) => !h.is_empty(),
        Some(("http", h)) => crate::security::is_loopback_host(h),
        _ => false,
    };
    let host = v.split_once("://").map(|x| x.1).unwrap_or("");
    if !ok || host.contains(['/', '?', '#', '@']) {
        bail!("HQC_ARTIFACT_PUBLIC_BASE={v}: expected https://host[:port] (http only on loopback), without a path");
    }
    Ok(v.to_string())
}

/// Limits of the unauthenticated `/worker/*` routes and of push subscriptions (SPEC §20).
/// Per-address limits use [`crate::security::ClientIp`].
#[derive(Clone, Debug)]
pub struct Limits {
    /// `POST /worker/enroll` per address and minute.
    pub enroll_per_min: usize,
    /// Unconfirmed enrollment codes per address.
    pub enroll_active_per_ip: i64,
    /// Unconfirmed enrollment codes in total.
    pub enroll_active_total: i64,
    /// `POST /worker/enroll/poll` per address and minute.
    pub poll_per_min: usize,
    /// `/worker/ws` attempts with a bad token per address and minute; then 429 without a
    /// database lookup.
    pub ws_auth_failures_per_min: usize,
    /// `/worker/ws` token lookups in flight at once; more get 503.
    pub ws_auth_concurrency: usize,
    /// Push subscriptions per user.
    pub push_subs_per_user: i64,
    /// Open `worker.approval.request` per worker; more get `error invalid_frame` (SPEC §20).
    pub worker_approvals_open: i64,
    /// Sessions the mirror may create per worker in 24 h (SPEC §9.2, §20).
    pub mirror_sessions_per_day: i64,
    /// Sessions of a workspace created by the mirror, in total (SPEC §9.2, §20).
    pub mirror_sessions_per_workspace: i64,
    /// `fs.*` requests in flight per worker; more get `429 worker_busy` (SPEC §20).
    pub worker_fs_concurrency: usize,
    /// `mcp.call` per calling session and minute; more get `limit_exceeded` (SPEC §20).
    pub mcp_calls_per_session_min: usize,
    /// `mcp.call` per worker and minute, counted before the token is looked up.
    pub mcp_calls_per_worker_min: usize,
    /// Open `hqc_ask`s of one session.
    pub open_asks_per_session: i64,
    /// Peer messages (`hqc_send`, `hqc_ask`, the prompt of `hqc_spawn`) a session sends per
    /// minute.
    pub peer_messages_per_session_min: usize,
    /// Peer messages in one chain, in total (SPEC §7.1).
    pub peer_messages_per_chain: i64,
    /// Sessions spawned in one chain (SPEC §7.1) ...
    pub spawns_per_chain: i64,
    /// ... and per owner in the last hour.
    pub spawns_per_hour: i64,
    /// Searches (`GET /api/search`, `hqc_search_history`) per owner and minute ...
    pub searches_per_owner_min: usize,
    /// ... of which fuzzy ones.
    pub fuzzy_searches_per_owner_min: usize,
    /// Searches running at once on the hub; more are refused.
    pub search_concurrency: usize,
    /// Runs of scheduled messages sent per owner and hour (SPEC §16); more are skipped.
    pub schedule_runs_per_owner_hour: usize,
    /// Bytes a worker uploads with `PUT /worker/artifacts` per hour.
    pub worker_upload_bytes_per_hour: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            enroll_per_min: 10,
            enroll_active_per_ip: 10,
            enroll_active_total: 1000,
            poll_per_min: 60,
            ws_auth_failures_per_min: 20,
            ws_auth_concurrency: 16,
            push_subs_per_user: 10,
            worker_approvals_open: 20,
            mirror_sessions_per_day: 200,
            mirror_sessions_per_workspace: 2000,
            worker_fs_concurrency: 4,
            mcp_calls_per_session_min: 60,
            mcp_calls_per_worker_min: 300,
            open_asks_per_session: 5,
            peer_messages_per_session_min: 20,
            peer_messages_per_chain: 50,
            spawns_per_chain: 5,
            spawns_per_hour: 20,
            searches_per_owner_min: 30,
            fuzzy_searches_per_owner_min: 10,
            search_concurrency: 4,
            schedule_runs_per_owner_hour: 60,
            worker_upload_bytes_per_hour: 2_000_000_000,
        }
    }
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
    /// An OIDC provider behind a reverse proxy (oauth2-proxy with Keycloak, for example): the
    /// ID token from `Authorization: Bearer`, checked against the provider's JWKS.
    Oidc {
        /// The exact expected `iss` (`https://auth.example.com/realms/<realm>`).
        issuer: String,
        /// The expected `aud`: the client id.
        aud: String,
        /// The provider's JWKS (no discovery).
        jwks_url: String,
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
    /// A `running` session with no event from its worker for this long loses its turn
    /// (SPEC §17.3); `HQC_TURN_WATCHDOG_S`, `None` (0) turns the watchdog off. Checked every
    /// `expiry_interval`.
    pub turn_watchdog: Option<Duration>,
    /// How long the hub waits for the worker's answer to `title.request` (T24, SPEC §3
    /// Session) before the title comes from the first prompt.
    pub title_request: Duration,
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
            turn_watchdog: Some(Duration::from_secs(DEFAULT_TURN_WATCHDOG_S)),
            title_request: Duration::from_secs(30),
        }
    }
}

pub const DEFAULT_TURN_WATCHDOG_S: u64 = 900;

/// `HQC_TURN_WATCHDOG_S`: whole seconds, 0 = off, unset = [`DEFAULT_TURN_WATCHDOG_S`].
pub fn turn_watchdog_from(v: Option<&str>) -> anyhow::Result<Option<Duration>> {
    let secs = match v.map(str::trim).filter(|v| !v.is_empty()) {
        None => DEFAULT_TURN_WATCHDOG_S,
        Some(v) => v
            .parse::<u64>()
            .with_context(|| format!("HQC_TURN_WATCHDOG_S={v}: expected whole seconds"))?,
    };
    Ok((secs > 0).then(|| Duration::from_secs(secs)))
}

impl Config {
    /// Variables: `HQC_DATABASE_URL`, `HQC_HUB_LISTEN`, `HQC_ACCESS_TEAM_DOMAIN`,
    /// `HQC_ACCESS_AUD`, `HQC_OIDC_ISSUER`, `HQC_OIDC_AUD`, `HQC_OIDC_JWKS_URL`, `HQC_DEV`,
    /// `HQC_DEV_USER`, `HQC_VAPID_PRIVATE`, `HQC_VAPID_PUBLIC`, `HQC_VAPID_SUBJECT`,
    /// `HQC_CLIENT_DIR`, `HQC_TURN_WATCHDOG_S`, `HQC_ARTIFACT_DIR`, `HQC_ARTIFACT_PUBLIC_BASE`,
    /// `HQC_ARTIFACT_KEY`, `HQC_ARTIFACT_RETENTION_DAYS`, `HQC_PUBLIC_ORIGINS`.
    pub fn from_env() -> anyhow::Result<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let database_url = var("HQC_DATABASE_URL").context("HQC_DATABASE_URL is not set")?;
        let listen = var("HQC_HUB_LISTEN").unwrap_or_else(|| "127.0.0.1:8080".into());
        let auth = auth_from(var, &listen)?;

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

        let mut artifacts = ArtifactConfig::new(
            var("HQC_ARTIFACT_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("artifacts")),
        );
        artifacts.public_base = var("HQC_ARTIFACT_PUBLIC_BASE")
            .map(|v| artifact_public_base(&v))
            .transpose()?;
        if let Some(k) = var("HQC_ARTIFACT_KEY") {
            if k.len() < 32 {
                bail!("HQC_ARTIFACT_KEY: at least 32 characters");
            }
            artifacts.key = k.into_bytes();
        }
        artifacts.retention = retention_from(var("HQC_ARTIFACT_RETENTION_DAYS").as_deref())?;
        std::fs::create_dir_all(&artifacts.dir).with_context(|| {
            format!(
                "HQC_ARTIFACT_DIR={}: cannot create",
                artifacts.dir.display()
            )
        })?;

        Ok(Self {
            database_url,
            listen,
            auth,
            vapid,
            client_dir,
            timing: Timing {
                turn_watchdog: turn_watchdog_from(var("HQC_TURN_WATCHDOG_S").as_deref())?,
                ..Timing::default()
            },
            limits: Limits::default(),
            artifacts,
            public_origins: public_origins(var("HQC_PUBLIC_ORIGINS").as_deref())?,
        })
    }
}

/// How people are authenticated, from `var` (unset or blank: `None`): Cloudflare Access
/// (`HQC_ACCESS_TEAM_DOMAIN`, `HQC_ACCESS_AUD`), an OIDC ID token (`HQC_OIDC_ISSUER`,
/// `HQC_OIDC_AUD`, `HQC_OIDC_JWKS_URL`, all three) or `HQC_DEV=1` with `HQC_DEV_USER` on a
/// loopback `listen`. Variables of two modes at once refuse to start.
fn auth_from(var: impl Fn(&str) -> Option<String>, listen: &str) -> anyhow::Result<AuthConfig> {
    let dev = var("HQC_DEV").as_deref() == Some("1");
    let team_domain = var("HQC_ACCESS_TEAM_DOMAIN");
    let aud = var("HQC_ACCESS_AUD");
    let oidc = ["HQC_OIDC_ISSUER", "HQC_OIDC_AUD", "HQC_OIDC_JWKS_URL"]
        .map(|k| (k, var(k).map(|v| v.trim().to_string())));
    let oidc_set = oidc.iter().any(|(_, v)| v.is_some());
    let auth = if dev {
        if team_domain.is_some() || aud.is_some() {
            bail!(
                "HQC_DEV=1 together with HQC_ACCESS_*: refusing to start in an ambiguous auth mode"
            );
        }
        if oidc_set {
            bail!(
                "HQC_DEV=1 together with HQC_OIDC_*: refusing to start in an ambiguous auth mode"
            );
        }
        let email = var("HQC_DEV_USER").context("HQC_DEV=1 requires HQC_DEV_USER=<email>")?;
        // Without Access every request is HQC_DEV_USER: never reachable from outside.
        if !is_loopback_listen(listen) {
            bail!("HQC_DEV=1 requires a loopback HQC_HUB_LISTEN (127.0.0.1, [::1] or localhost), got {listen}");
        }
        AuthConfig::Dev { email }
    } else {
        if var("HQC_DEV_USER").is_some() {
            tracing::warn!("HQC_DEV_USER is ignored without HQC_DEV=1");
        }
        if oidc_set {
            if team_domain.is_some() || aud.is_some() {
                bail!("HQC_OIDC_* together with HQC_ACCESS_*: refusing to start in an ambiguous auth mode");
            }
            let missing: Vec<&str> = oidc
                .iter()
                .filter(|(_, v)| v.is_none())
                .map(|(k, _)| *k)
                .collect();
            if !missing.is_empty() {
                bail!(
                    "HQC_OIDC_* needs all three variables, not set: {}",
                    missing.join(", ")
                );
            }
            let [issuer, aud, jwks_url] = oidc.map(|(_, v)| v.unwrap_or_default());
            AuthConfig::Oidc {
                issuer,
                aud,
                jwks_url,
            }
        } else {
            AuthConfig::Access {
                team_domain: normalize_team_domain(
                    &team_domain.context("HQC_ACCESS_TEAM_DOMAIN is not set")?,
                ),
                aud: aud.context("HQC_ACCESS_AUD is not set")?,
                jwks_url: None,
            }
        }
    };
    Ok(auth)
}

/// `HQC_PUBLIC_ORIGINS`: comma-separated `https://host[:port]` (or `http://` on loopback),
/// no path; lowercase, default ports dropped.
pub fn public_origins(v: Option<&str>) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    for raw in v.unwrap_or("").split(',') {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let o =
            artifact_public_base(raw).map_err(|e| anyhow::anyhow!("HQC_PUBLIC_ORIGINS: {e}"))?;
        out.push(crate::security::normalize_origin(&o).unwrap_or(o));
    }
    Ok(out)
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
    fn turn_watchdog_variable() {
        let d = |s| Some(Duration::from_secs(s));
        assert_eq!(turn_watchdog_from(None).unwrap(), d(900));
        assert_eq!(turn_watchdog_from(Some(" ")).unwrap(), d(900));
        assert_eq!(turn_watchdog_from(Some("30")).unwrap(), d(30));
        assert_eq!(turn_watchdog_from(Some("0")).unwrap(), None);
        assert!(turn_watchdog_from(Some("15m")).is_err());
    }

    #[test]
    fn retention_variable() {
        let days = |d: u64| Some(Duration::from_secs(d * 86_400));
        assert_eq!(retention_from(None).unwrap(), days(30));
        assert_eq!(retention_from(Some("7")).unwrap(), days(7));
        assert_eq!(retention_from(Some("0")).unwrap(), None);
        assert!(retention_from(Some("2w")).is_err());
    }

    #[test]
    fn artifact_base() {
        assert_eq!(
            artifact_public_base("https://artifacts.example.com/").unwrap(),
            "https://artifacts.example.com"
        );
        assert!(artifact_public_base("http://127.0.0.1:8081").is_ok());
        for bad in [
            "http://artifacts.example.com",
            "https://a.example.com/x",
            "artifacts.example.com",
            "https://",
        ] {
            assert!(artifact_public_base(bad).is_err(), "{bad}");
        }
        let mut c = ArtifactConfig::new("x".into());
        c.public_base = Some("https://Artifacts.example.com:8443".into());
        assert_eq!(c.public_host().unwrap(), "artifacts.example.com:8443");
    }

    const OIDC: [(&str, &str); 3] = [
        ("HQC_OIDC_ISSUER", " https://auth.example.com/realms/home "),
        ("HQC_OIDC_AUD", "hqc"),
        (
            "HQC_OIDC_JWKS_URL",
            "https://auth.example.com/realms/home/protocol/openid-connect/certs",
        ),
    ];

    fn auth(vars: &[(&str, &str)], listen: &str) -> anyhow::Result<AuthConfig> {
        auth_from(
            |k| {
                vars.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            },
            listen,
        )
    }

    #[test]
    fn oidc_mode_with_all_three_variables() {
        match auth(&OIDC, "0.0.0.0:8080").unwrap() {
            AuthConfig::Oidc {
                issuer,
                aud,
                jwks_url,
            } => {
                assert_eq!(issuer, "https://auth.example.com/realms/home");
                assert_eq!(aud, "hqc");
                assert_eq!(jwks_url, OIDC[2].1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn oidc_mode_partly_set_names_the_missing() {
        let e = auth(&OIDC[..2], "0.0.0.0:8080").unwrap_err().to_string();
        assert!(
            e.contains("HQC_OIDC_JWKS_URL") && !e.contains("HQC_OIDC_AUD"),
            "{e}"
        );
        let e = auth(&OIDC[2..], "0.0.0.0:8080").unwrap_err().to_string();
        assert!(
            e.contains("HQC_OIDC_ISSUER") && e.contains("HQC_OIDC_AUD"),
            "{e}"
        );
    }

    #[test]
    fn oidc_mode_with_access_is_ambiguous() {
        for access in [
            ("HQC_ACCESS_TEAM_DOMAIN", "team.cloudflareaccess.com"),
            ("HQC_ACCESS_AUD", "tag"),
        ] {
            let mut vars = OIDC.to_vec();
            vars.push(access);
            let e = auth(&vars, "0.0.0.0:8080").unwrap_err().to_string();
            assert!(e.contains("ambiguous"), "{e}");
        }
    }

    #[test]
    fn oidc_mode_with_dev_is_ambiguous() {
        let mut vars = OIDC.to_vec();
        vars.extend([("HQC_DEV", "1"), ("HQC_DEV_USER", "dev@example.com")]);
        let e = auth(&vars, "127.0.0.1:8080").unwrap_err().to_string();
        assert!(e.contains("ambiguous"), "{e}");
    }

    #[test]
    fn access_and_dev_modes_are_selected_as_before() {
        let access = [
            ("HQC_ACCESS_TEAM_DOMAIN", "https://t.cloudflareaccess.com/"),
            ("HQC_ACCESS_AUD", "a"),
        ];
        match auth(&access, "0.0.0.0:8080").unwrap() {
            AuthConfig::Access {
                team_domain, aud, ..
            } => {
                assert_eq!(team_domain, "t.cloudflareaccess.com");
                assert_eq!(aud, "a");
            }
            other => panic!("{other:?}"),
        }
        let e = auth(&[], "0.0.0.0:8080").unwrap_err().to_string();
        assert!(e.contains("HQC_ACCESS_TEAM_DOMAIN"), "{e}");
        let dev = [("HQC_DEV", "1"), ("HQC_DEV_USER", "dev@example.com")];
        let e = auth(&dev, "0.0.0.0:8080").unwrap_err().to_string();
        assert!(e.contains("loopback"), "{e}");
        let mut vars = dev.to_vec();
        vars.push(access[1]);
        let e = auth(&vars, "127.0.0.1:8080").unwrap_err().to_string();
        assert!(e.contains("ambiguous"), "{e}");
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
