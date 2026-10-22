//! Local worker config, SPEC §14 (iteration 1 subset).
//!
//! The file is the only source of truth for `allowed_roots`, the workspace, environments
//! and `allow_foreign_sessions` (SPEC §2.1): `session.start` values are checked against it
//! and a mismatch is rejected before anything is spawned ([`Config::check_start`]).
//!
//! ```toml
//! hub_url = "https://hub.example.net"
//! allowed_roots = ["/Users/me/work"]
//! idle_timeout = 0               # seconds; 0 = idle agents are never stopped (SPEC §17.3)
//! allow_foreign_sessions = false
//! # claude_bin = "/opt/homebrew/bin/claude"   # default: `claude` from PATH
//!
//! [workspace]
//! path = "/Users/me/work/hqc"
//! ```

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use hqc_proto::frame::{Runtime, SessionStart};
use serde::Deserialize;
use uuid::Uuid;

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
    workspace: RawWorkspace,
    #[serde(default)]
    idle_timeout: u64,
    #[serde(default)]
    allow_foreign_sessions: bool,
    #[serde(default)]
    claude_bin: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWorkspace {
    path: PathBuf,
}

/// Validated config. Paths are absolute and normalized (canonical when they exist).
#[derive(Debug, Clone)]
pub struct Config {
    pub hub_url: String,
    pub allowed_roots: Vec<PathBuf>,
    pub workspace: PathBuf,
    /// `None`: idle agents are never stopped.
    pub idle_timeout: Option<Duration>,
    pub allow_foreign_sessions: bool,
    pub claude_bin: Option<PathBuf>,
}

/// What a session may be started with, after checking `session.start` against the config.
#[derive(Debug, Clone, PartialEq)]
pub struct StartSpec {
    pub session_id: Uuid,
    pub runtime: Runtime,
    pub model: Option<String>,
    /// The configured workspace, not the hub's string.
    pub workspace: PathBuf,
    /// `<workspace>/<cwd>`, the process working directory (SPEC §8.1).
    pub workdir: PathBuf,
    pub environment: String,
    pub runtime_session_id: Option<String>,
    pub system_prompt: Option<String>,
    pub disallowed_tools: Vec<String>,
    pub restricted: bool,
    pub auto_approve: bool,
    pub session_token: String,
    pub ask_timeout_s: u32,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading worker config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("worker config {}", path.display()))
    }

    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let raw: RawConfig = toml::from_str(text)?;
        if !(raw.hub_url.starts_with("http://") || raw.hub_url.starts_with("https://")) {
            bail!("hub_url must be http:// or https://");
        }
        if raw.allowed_roots.is_empty() {
            bail!("allowed_roots is empty");
        }
        let allowed_roots = raw
            .allowed_roots
            .iter()
            .map(|p| absolute(p).context("allowed_roots"))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let workspace = absolute(&raw.workspace.path).context("workspace.path")?;
        if !allowed_roots.iter().any(|r| workspace.starts_with(r)) {
            bail!(
                "workspace.path {} is outside allowed_roots",
                workspace.display()
            );
        }
        Ok(Self {
            hub_url: raw.hub_url.trim_end_matches('/').to_string(),
            allowed_roots,
            workspace,
            idle_timeout: (raw.idle_timeout > 0).then(|| Duration::from_secs(raw.idle_timeout)),
            allow_foreign_sessions: raw.allow_foreign_sessions,
            claude_bin: raw.claude_bin,
        })
    }

    /// Check `session.start` against the local config (SPEC §2.1, §14). `Err` is the reason,
    /// sent to the hub as `error {code: invalid_frame}`; nothing is spawned then.
    pub fn check_start(&self, s: &SessionStart) -> Result<StartSpec, String> {
        let Some(ws) = &s.workspace_path else {
            return Err("sessions without workspace are not supported by this worker".into());
        };
        let ws = absolute(Path::new(ws)).map_err(|e| format!("workspace_path: {e}"))?;
        if ws != self.workspace {
            return Err(format!(
                "workspace_path {} is not a workspace of this worker",
                ws.display()
            ));
        }
        if !self.allowed_roots.iter().any(|r| ws.starts_with(r)) {
            return Err(format!(
                "workspace_path {} is outside allowed_roots",
                ws.display()
            ));
        }
        let cwd = Path::new(&s.cwd);
        if !cwd
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return Err(format!(
                "cwd {:?} must be relative to the workspace, without `..`",
                s.cwd
            ));
        }
        let workdir = normalize(&ws.join(cwd));
        // A symlink inside the workspace must not lead out of it.
        if let Ok(real) = workdir.canonicalize() {
            if !real.starts_with(&ws) {
                return Err(format!("cwd {:?} resolves outside the workspace", s.cwd));
            }
        }
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
            session_token: s.session_token.clone(),
            ask_timeout_s: s.ask_timeout_s,
        })
    }

    pub fn path_strings(paths: &[PathBuf]) -> Vec<String> {
        paths.iter().map(|p| p.display().to_string()).collect()
    }
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
            "hub_url = \"http://127.0.0.1:1/\"\nallowed_roots = [{:?}]\nidle_timeout = 5\n\
             [workspace]\npath = {:?}\n",
            dir.display().to_string(),
            ws.display().to_string()
        ))
        .unwrap()
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
        let spec = c.check_start(&start(&c.workspace)).unwrap();
        assert_eq!(spec.workdir, c.workspace);
    }

    #[test]
    fn rejects_mismatches() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let ok = start(&c.workspace);

        let mut s = ok.clone();
        s.workspace_path = Some(dir.path().display().to_string());
        assert!(c.check_start(&s).is_err());
        s.workspace_path = Some(format!("{}/../ws", c.workspace.display()));
        assert!(c.check_start(&s).is_ok(), "same workspace via ..");
        s.workspace_path = None;
        assert!(c.check_start(&s).is_err());

        for cwd in ["..", "../x", "/etc", "a/../../b"] {
            let mut s = ok.clone();
            s.cwd = cwd.into();
            assert!(c.check_start(&s).is_err(), "cwd {cwd}");
        }
        let mut s = ok.clone();
        s.cwd = "sub/dir".into();
        assert_eq!(
            c.check_start(&s).unwrap().workdir,
            c.workspace.join("sub/dir")
        );

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
        std::os::unix::fs::symlink(dir.path(), c.workspace.join("out")).unwrap();
        let mut s = start(&c.workspace);
        s.cwd = "out".into();
        assert!(c.check_start(&s).is_err());
    }

    #[test]
    fn rejects_bad_files() {
        assert!(Config::parse("hub_url = \"x\"").is_err());
        let bad = "hub_url = \"https://h\"\nallowed_roots = [\"/a\"]\n[workspace]\npath = \"/b\"\n";
        assert!(Config::parse(bad).is_err(), "workspace outside roots");
        let typo = "hub_url = \"https://h\"\nallowed_root = [\"/a\"]\n[workspace]\npath = \"/a\"\n";
        assert!(Config::parse(typo).is_err(), "unknown field");
    }
}
