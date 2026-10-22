//! Local worker config, SPEC §14 (iteration 1 subset).
//!
//! The file is the only source of truth for `allowed_roots`, environments and
//! `allow_foreign_sessions` (SPEC §2.1): `session.start` values are checked against it and a
//! mismatch is rejected before anything is spawned ([`Config::check_start`]). Workspaces are
//! hub objects (SPEC §3); the worker only requires their paths to lie inside
//! `allowed_roots`.
//!
//! ```toml
//! hub_url = "https://hub.example.net"  # https; http only for loopback or with insecure_http
//! allowed_roots = ["/Users/me/work"]
//! idle_timeout = 0               # seconds; 0 = idle agents are never stopped (SPEC §17.3)
//! allow_foreign_sessions = false
//! # claude_bin = "/opt/homebrew/bin/claude"   # default: `claude` from PATH
//! # max_sessions = 8             # agent processes at once
//! # insecure_http = false        # allow http:// to a non-loopback hub (never in production)
//! # allow_native_messaging = false  # keep Claude Code's ListAgents/SendMessage (bypass the hub)
//!
//! [permissions]                  # optional, see crate::permissions
//! mode = "ask"                   # "ask" | "auto"
//! allow = ["Bash(cargo test:*)"]
//! # deny: default crate::permissions::DEFAULT_DENY (sudo, ssh, scp, rm -rf /*, ...)
//! ```

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use hqc_proto::frame::{Runtime, SessionStart};
use serde::Deserialize;
use uuid::Uuid;

use crate::permissions::{Permissions, RawPermissions};
use crate::token::Secret;

/// The only environment in iteration 1 (SPEC §14: no `environments{}` / `env_bindings[]` yet).
pub const DEFAULT_ENVIRONMENT: &str = "default";

/// `$HQC_HOME`, default `~/.hqc`.
pub fn hqc_home() -> anyhow::Result<PathBuf> {
    if let Some(p) = std::env::var_os("HQC_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".hqc"))
}

/// `$HQC_WORKER_CONFIG`, default `$HQC_HOME/worker.toml`.
pub fn config_path() -> anyhow::Result<PathBuf> {
    if let Some(p) = std::env::var_os("HQC_WORKER_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    Ok(hqc_home()?.join("worker.toml"))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    hub_url: String,
    allowed_roots: Vec<PathBuf>,
    /// Iteration 1's single workspace, ignored since T14 (workspaces are added in the
    /// client); kept readable so an old file still starts, with a warning.
    #[serde(default)]
    workspace: Option<toml::Table>,
    #[serde(default)]
    idle_timeout: u64,
    #[serde(default)]
    allow_foreign_sessions: bool,
    #[serde(default)]
    claude_bin: Option<PathBuf>,
    #[serde(default)]
    insecure_http: bool,
    #[serde(default = "default_max_sessions")]
    max_sessions: usize,
    #[serde(default)]
    allow_native_messaging: bool,
    #[serde(default)]
    permissions: Option<RawPermissions>,
}

fn default_max_sessions() -> usize {
    DEFAULT_MAX_SESSIONS
}

/// Agent processes running at once, unless `max_sessions` says otherwise.
pub const DEFAULT_MAX_SESSIONS: usize = 8;

/// `hub_url` (and the argument of `enroll`) is `https://`; `http://` only for a loopback
/// host (`localhost`, `127.0.0.0/8`, `[::1]`) or with `insecure_http`: the worker token and
/// the enrollment secret travel in it (SPEC §5.1).
pub fn check_hub_url(url: &str, insecure_http: bool) -> anyhow::Result<()> {
    let u = reqwest::Url::parse(url).with_context(|| format!("hub_url {url:?}"))?;
    if !u.username().is_empty() || u.password().is_some() {
        bail!("hub_url must not carry credentials");
    }
    match u.scheme() {
        "https" => Ok(()),
        "http" if insecure_http || is_loopback(&u) => Ok(()),
        "http" => bail!(
            "hub_url {url} is plain http to a non-loopback host: the worker token would travel \
             unencrypted; use https:// (or set insecure_http = true / pass --insecure-http)"
        ),
        s => bail!("hub_url must be https:// (got {s}://)"),
    }
}

fn is_loopback(u: &reqwest::Url) -> bool {
    let Some(h) = u.host_str() else {
        return false;
    };
    let h = h.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost")
        || h.parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Validated config. Paths are absolute and normalized (canonical when they exist).
#[derive(Debug, Clone)]
pub struct Config {
    pub hub_url: String,
    pub allowed_roots: Vec<PathBuf>,
    /// The file still has iteration 1's `[workspace]` block, which is ignored.
    pub legacy_workspace: bool,
    /// `None`: idle agents are never stopped.
    pub idle_timeout: Option<Duration>,
    pub allow_foreign_sessions: bool,
    pub claude_bin: Option<PathBuf>,
    /// `hub_url` may be plain http to a non-loopback host (explicit opt-in).
    pub insecure_http: bool,
    /// Agent processes running at once; more are refused with `agent_start_failed`.
    pub max_sessions: usize,
    /// Keep Claude Code's own cross-session tools (`ListAgents`, `SendMessage`). Off by
    /// default: they bypass the hub, so the other session's history misses the input that
    /// started its turn. Agent-to-agent traffic goes through `hqc_send`/`hqc_ask` (SPEC §7).
    pub allow_native_messaging: bool,
    /// `[permissions]`: allow/deny rules for `--settings` and the ask/auto mode (SPEC §14).
    pub permissions: Permissions,
    /// Where sessions without a workspace get their empty folder (SPEC §8.4):
    /// `$HQC_HOME/scratch/<session_id>`.
    pub scratch_root: PathBuf,
}

/// What a session may be started with, after checking `session.start` against the config.
#[derive(Debug, Clone, PartialEq)]
pub struct StartSpec {
    pub session_id: Uuid,
    pub runtime: Runtime,
    pub model: Option<String>,
    /// The workspace (canonical, inside `allowed_roots`), not the hub's string; for a
    /// session without workspace its scratch folder.
    pub workspace: PathBuf,
    /// Canonical `<workspace>/<cwd>`, the process working directory (SPEC §8.1); checked
    /// again before every spawn ([`StartSpec::recheck_workdir`]).
    pub workdir: PathBuf,
    pub environment: String,
    pub runtime_session_id: Option<String>,
    pub system_prompt: Option<String>,
    pub disallowed_tools: Vec<String>,
    pub restricted: bool,
    pub auto_approve: bool,
    pub session_token: Secret,
    pub ask_timeout_s: u32,
}

impl StartSpec {
    /// The working directory still exists and still resolves inside the workspace: a symlink
    /// may have been swapped since `session.start` (SPEC §2.1).
    pub fn recheck_workdir(&self) -> anyhow::Result<()> {
        let ws = self
            .workspace
            .canonicalize()
            .with_context(|| format!("workspace {}", self.workspace.display()))?;
        let real = self
            .workdir
            .canonicalize()
            .with_context(|| format!("working directory {}", self.workdir.display()))?;
        if real != self.workdir || !real.starts_with(&ws) || !real.is_dir() {
            bail!(
                "working directory {} no longer resolves inside the workspace",
                self.workdir.display()
            );
        }
        Ok(())
    }
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading worker config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("worker config {}", path.display()))
    }

    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let raw: RawConfig = toml::from_str(text)?;
        check_hub_url(&raw.hub_url, raw.insecure_http)?;
        if raw.max_sessions == 0 {
            bail!("max_sessions must be at least 1");
        }
        if raw.allowed_roots.is_empty() {
            bail!("allowed_roots is empty");
        }
        let allowed_roots = raw
            .allowed_roots
            .iter()
            .map(|p| absolute(p).context("allowed_roots"))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self {
            hub_url: raw.hub_url.trim_end_matches('/').to_string(),
            allowed_roots,
            legacy_workspace: raw.workspace.is_some(),
            idle_timeout: (raw.idle_timeout > 0).then(|| Duration::from_secs(raw.idle_timeout)),
            allow_foreign_sessions: raw.allow_foreign_sessions,
            claude_bin: raw.claude_bin,
            insecure_http: raw.insecure_http,
            allow_native_messaging: raw.allow_native_messaging,
            permissions: Permissions::from_raw(raw.permissions)?,
            max_sessions: raw.max_sessions,
            scratch_root: hqc_home()
                .unwrap_or_else(|_| std::env::temp_dir().join("hqc"))
                .join("scratch"),
        })
    }

    /// Check `session.start` against the local config (SPEC §2.1, §14). `Err` is the reason,
    /// sent to the hub as `error {code: invalid_frame}`; nothing is spawned then.
    pub fn check_start(&self, s: &SessionStart) -> Result<StartSpec, String> {
        // Values that end up on the agent's command line.
        if let Some(m) = &s.model {
            if !valid_model(m) {
                return Err(format!("model {m:?} is not a model name"));
            }
        }
        if let Some(t) = s.disallowed_tools.iter().find(|t| !valid_tool_rule(t)) {
            return Err(format!("disallowed_tools entry {t:?} is not a tool rule"));
        }
        let (ws, workdir) = match &s.workspace_path {
            Some(ws) => self.check_workdir(ws, &s.cwd)?,
            // SPEC §8.4: Claude Code without a workspace runs `--restricted` in an empty
            // folder of its own.
            None if s.restricted && s.runtime == Runtime::ClaudeCode => {
                let dir = self.scratch(s.session_id)?;
                (dir.clone(), dir)
            }
            None => return Err("a session without workspace must be restricted (SPEC §8.4)".into()),
        };
        if s.environment != DEFAULT_ENVIRONMENT {
            return Err(format!(
                "environment {:?} does not match the local binding {DEFAULT_ENVIRONMENT:?}",
                s.environment
            ));
        }
        if s.acp_agent.is_some() {
            return Err("acp_agent is not configured on this worker".into());
        }
        if !s.owner_is_worker_owner && !self.allow_foreign_sessions {
            return Err("foreign sessions are not allowed on this worker".into());
        }
        Ok(StartSpec {
            session_id: s.session_id,
            runtime: s.runtime,
            model: s.model.clone(),
            workspace: ws,
            workdir,
            environment: s.environment.clone(),
            runtime_session_id: s.runtime_session_id.clone(),
            system_prompt: s.system_prompt.clone(),
            disallowed_tools: s.disallowed_tools.clone(),
            restricted: s.restricted,
            auto_approve: s.auto_approve,
            session_token: Secret::new(s.session_token.clone()),
            ask_timeout_s: s.ask_timeout_s,
        })
    }

    /// The canonical workspace, which must lie inside `allowed_roots` (symlinks resolved),
    /// and the canonical working directory `<workspace>/<cwd>`, which must exist and resolve
    /// inside the workspace (no `..`, no symlink out).
    fn check_workdir(&self, ws: &str, cwd: &str) -> Result<(PathBuf, PathBuf), String> {
        let ws = Path::new(ws);
        if !ws.is_absolute() {
            return Err(format!("workspace_path {} is not absolute", ws.display()));
        }
        let ws = ws
            .canonicalize()
            .map_err(|e| format!("workspace {}: {e}", ws.display()))?;
        if !self.inside_roots(&ws) {
            return Err(format!(
                "workspace_path {} is outside allowed_roots",
                ws.display()
            ));
        }
        if !ws.is_dir() {
            return Err(format!("workspace {} is not a directory", ws.display()));
        }
        let rel = Path::new(cwd);
        if !rel
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return Err(format!(
                "cwd {cwd:?} must be relative to the workspace, without `..`"
            ));
        }
        let workdir = ws
            .join(rel)
            .canonicalize()
            .map_err(|e| format!("cwd {cwd:?}: {e}"))?;
        // A symlink inside the workspace must not lead out of it.
        if !workdir.starts_with(&ws) {
            return Err(format!("cwd {cwd:?} resolves outside the workspace"));
        }
        if !workdir.is_dir() {
            return Err(format!("cwd {cwd:?} is not a directory"));
        }
        Ok((ws, workdir))
    }

    /// `<scratch_root>/<session_id>`, created 0700 (SPEC §8.4), canonical.
    fn scratch(&self, session_id: Uuid) -> Result<PathBuf, String> {
        let dir = self.scratch_root.join(session_id.to_string());
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        b.create(&dir)
            .and_then(|_| dir.canonicalize())
            .map_err(|e| format!("scratch folder {}: {e}", dir.display()))
    }

    /// `path` (canonical) lies inside one of `allowed_roots`, by components.
    pub fn inside_roots(&self, path: &Path) -> bool {
        self.allowed_roots.iter().any(|r| path.starts_with(r))
    }

    pub fn path_strings(paths: &[PathBuf]) -> Vec<String> {
        paths.iter().map(|p| p.display().to_string()).collect()
    }
}

/// `^[A-Za-z0-9._:-]{1,64}$` without a leading `-` (the hub checks the same, SPEC §20).
fn valid_model(m: &str) -> bool {
    (1..=64).contains(&m.len())
        && !m.starts_with('-')
        && m.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b':' | b'-'))
}

/// A tool name or rule such as `Bash(git push:*)`: one argv entry that is not a flag.
fn valid_tool_rule(t: &str) -> bool {
    !t.is_empty() && t.len() <= 256 && !t.starts_with('-') && !t.chars().any(char::is_control)
}

/// Absolute, `..`-free; canonical if the path exists (resolves symlinks such as /tmp).
fn absolute(p: &Path) -> anyhow::Result<PathBuf> {
    if !p.is_absolute() {
        bail!("{} is not an absolute path", p.display());
    }
    Ok(p.canonicalize().unwrap_or_else(|_| normalize(p)))
}

/// Lexical normalization: drops `.`, resolves `..` against the preceding component.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dir: &Path) -> Config {
        let ws = dir.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        Config::parse(&format!(
            "hub_url = \"http://127.0.0.1:1/\"\nallowed_roots = [{:?}]\nidle_timeout = 5\n",
            dir.display().to_string(),
        ))
        .unwrap()
    }

    /// The workspace [`cfg`] made: `<root>/ws`.
    fn ws(c: &Config) -> PathBuf {
        c.allowed_roots[0].join("ws")
    }

    fn start(ws: &Path) -> SessionStart {
        let mut s: hqc_proto::frame::Envelope<hqc_proto::frame::HubFrame> = serde_json::from_str(
            include_str!("../../hqc-proto/examples/wire/hub.session.start.json"),
        )
        .unwrap();
        let hqc_proto::frame::HubFrame::SessionStart(ref mut s) = s.body else {
            unreachable!()
        };
        s.workspace_path = Some(ws.display().to_string());
        s.clone()
    }

    #[test]
    fn parses_and_checks() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        assert_eq!(c.idle_timeout, Some(Duration::from_secs(5)));
        assert!(!c.hub_url.ends_with('/'));
        let spec = c.check_start(&start(&ws(&c))).unwrap();
        assert_eq!(spec.workdir, ws(&c));
    }

    #[test]
    fn rejects_mismatches() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let ok = start(&ws(&c));

        // Any folder inside allowed_roots is a workspace; nothing outside is.
        let mut s = ok.clone();
        std::fs::create_dir(dir.path().join("other")).unwrap();
        s.workspace_path = Some(dir.path().join("other").display().to_string());
        assert_eq!(
            c.check_start(&s).unwrap().workspace,
            c.allowed_roots[0].join("other")
        );
        s.workspace_path = Some(format!("{}/../ws", ws(&c).display()));
        assert!(c.check_start(&s).is_ok(), "same workspace via ..");
        let outside = tempfile::tempdir().unwrap();
        for bad in [
            outside.path().display().to_string(),
            format!("{}/..", dir.path().display()),
            dir.path().join("missing").display().to_string(),
            "ws".into(),
        ] {
            s.workspace_path = Some(bad.clone());
            assert!(c.check_start(&s).is_err(), "{bad}");
        }
        std::fs::write(dir.path().join("file"), "").unwrap();
        s.workspace_path = Some(dir.path().join("file").display().to_string());
        assert!(c.check_start(&s).is_err(), "a file");
        s.workspace_path = None;
        assert!(c.check_start(&s).is_err());

        for cwd in ["..", "../x", "/etc", "a/../../b"] {
            let mut s = ok.clone();
            s.cwd = cwd.into();
            assert!(c.check_start(&s).is_err(), "cwd {cwd}");
        }
        let mut s = ok.clone();
        s.cwd = "sub/dir".into();
        assert!(c.check_start(&s).is_err(), "cwd must exist");
        std::fs::create_dir_all(ws(&c).join("sub/dir")).unwrap();
        assert_eq!(c.check_start(&s).unwrap().workdir, ws(&c).join("sub/dir"));
        std::fs::write(ws(&c).join("file"), "").unwrap();
        s.cwd = "file".into();
        assert!(c.check_start(&s).is_err(), "cwd must be a directory");

        for model in ["--resume", "a b", "", "x;y"] {
            let mut s = ok.clone();
            s.model = Some(model.into());
            assert!(c.check_start(&s).is_err(), "model {model:?}");
        }
        for tool in ["--permission-mode", "", "Bash\nx"] {
            let mut s = ok.clone();
            s.disallowed_tools = vec![tool.into()];
            assert!(c.check_start(&s).is_err(), "tool {tool:?}");
        }

        let mut s = ok.clone();
        s.environment = "infra".into();
        assert!(c.check_start(&s).is_err());

        let mut s = ok.clone();
        s.owner_is_worker_owner = false;
        assert!(c.check_start(&s).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        std::os::unix::fs::symlink(dir.path(), ws(&c).join("out")).unwrap();
        let mut s = start(&ws(&c));
        s.cwd = "out".into();
        assert!(c.check_start(&s).is_err());

        // A link inside the workspace is stored resolved, and checked again before a spawn.
        std::fs::create_dir(ws(&c).join("real")).unwrap();
        std::os::unix::fs::symlink(ws(&c).join("real"), ws(&c).join("link")).unwrap();
        s.cwd = "link".into();
        let spec = c.check_start(&s).unwrap();
        assert_eq!(spec.workdir, ws(&c).join("real"));
        spec.recheck_workdir().unwrap();
        std::fs::remove_dir(ws(&c).join("real")).unwrap();
        std::os::unix::fs::symlink(dir.path(), ws(&c).join("real")).unwrap();
        assert!(spec.recheck_workdir().is_err(), "swapped for a link out");

        // A workspace path that is a link out of allowed_roots.
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        s.workspace_path = Some(dir.path().join("escape").display().to_string());
        s.cwd = ".".into();
        assert!(c.check_start(&s).is_err());
    }

    #[test]
    fn restricted_session_without_workspace_gets_a_scratch_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(dir.path());
        c.scratch_root = dir.path().join("home/scratch");
        let mut s = start(&ws(&c));
        s.workspace_path = None;
        assert!(c.check_start(&s).is_err(), "only restricted");
        s.restricted = true;
        let spec = c.check_start(&s).unwrap();
        let want = dir
            .path()
            .canonicalize()
            .unwrap()
            .join(format!("home/scratch/{}", s.session_id));
        assert_eq!((&spec.workspace, &spec.workdir), (&want, &want));
        assert!(spec.restricted);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&want).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        spec.recheck_workdir().unwrap();
    }

    #[test]
    fn session_token_is_not_printed() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let mut s = start(&ws(&c));
        s.session_token = "hqcs_very_secret".into();
        let spec = c.check_start(&s).unwrap();
        assert_eq!(spec.session_token.expose(), "hqcs_very_secret");
        assert!(!format!("{spec:?}").contains("very_secret"));
    }

    #[test]
    fn hub_url_is_https_unless_loopback_or_insecure() {
        for ok in [
            "https://hqc.example.com",
            "http://127.0.0.1:8080",
            "http://localhost:8080",
            "http://[::1]:8080",
        ] {
            check_hub_url(ok, false).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "http://hqc.example.com",
            "http://192.168.1.5:8080",
            "http://localhost.evil.example",
            "ws://hqc.example.com",
            "https://user:pw@hqc.example.com",
            "hqc.example.com",
        ] {
            assert!(check_hub_url(bad, false).is_err(), "{bad}");
        }
        check_hub_url("http://hub.lan:8080", true).unwrap();
        let roots = "allowed_roots = [\"/a\"]\n";
        assert!(Config::parse(&format!("hub_url = \"http://hub.lan\"\n{roots}")).is_err());
        let c = Config::parse(&format!(
            "hub_url = \"http://hub.lan\"\ninsecure_http = true\n{roots}"
        ))
        .unwrap();
        assert!(c.insecure_http);
        assert_eq!(c.max_sessions, DEFAULT_MAX_SESSIONS);
        let c = Config::parse(&format!(
            "hub_url = \"https://h\"\nmax_sessions = 2\n{roots}"
        ))
        .unwrap();
        assert_eq!(c.max_sessions, 2);
        assert!(Config::parse(&format!(
            "hub_url = \"https://h\"\nmax_sessions = 0\n{roots}"
        ))
        .is_err());
    }

    #[test]
    fn permissions_block() {
        use crate::permissions::{PermissionMode, DEFAULT_DENY};
        let base = "hub_url = \"https://h\"\nallowed_roots = [\"/a\"]\n";

        let c = Config::parse(base).unwrap();
        assert_eq!(c.permissions, Permissions::default());
        assert_eq!(c.permissions.mode, PermissionMode::Ask);
        assert!(c.permissions.allow.is_empty());
        assert_eq!(c.permissions.deny, DEFAULT_DENY);

        // Only `allow`: `deny` keeps its default.
        let c = Config::parse(&format!(
            "{base}[permissions]\nallow = [\"Bash(cargo test:*)\", \"Read\", \"mcp__hqc\"]\n"
        ))
        .unwrap();
        assert_eq!(
            c.permissions.allow,
            ["Bash(cargo test:*)", "Read", "mcp__hqc"]
        );
        assert_eq!(c.permissions.deny, DEFAULT_DENY);

        let c = Config::parse(&format!(
            "{base}[permissions]\nmode = \"auto\"\ndeny = []\n"
        ))
        .unwrap();
        assert_eq!(c.permissions.mode, PermissionMode::Auto);
        assert!(
            c.permissions.deny.is_empty(),
            "deny = [] turns the default off"
        );

        for bad in [
            "[permissions]\nmode = \"bypass\"\n",
            "[permissions]\nallow = [\"Bash(git push\"]\n",
            "[permissions]\ndeny = [\"\"]\n",
            "[permissions]\ndeny = [\"--dangerously-skip-permissions\"]\n",
            "[permissions]\nask = [\"Bash\"]\n",
            "[permissions]\nallow = \"Read\"\n",
        ] {
            assert!(Config::parse(&format!("{base}{bad}")).is_err(), "{bad}");
        }
        let e = Config::parse(&format!("{base}[permissions]\ndeny = [\"Bash()\"]\n")).unwrap_err();
        assert!(format!("{e:#}").contains("permissions.deny"), "{e:#}");
    }

    #[test]
    fn rejects_bad_files() {
        assert!(Config::parse("hub_url = \"x\"").is_err());
        let typo = "hub_url = \"https://h\"\nallowed_root = [\"/a\"]\n";
        assert!(Config::parse(typo).is_err(), "unknown field");
        assert!(Config::parse("hub_url = \"https://h\"\nallowed_roots = []\n").is_err());
    }

    #[test]
    fn iteration1_workspace_block_is_ignored() {
        let old = "hub_url = \"https://h\"\nallowed_roots = [\"/a\"]\n[workspace]\npath = \"/b\"\n";
        assert!(Config::parse(old).unwrap().legacy_workspace);
        let new = "hub_url = \"https://h\"\nallowed_roots = [\"/a\"]\n";
        assert!(!Config::parse(new).unwrap().legacy_workspace);
    }
}
