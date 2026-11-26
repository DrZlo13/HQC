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
//! # clone_timeout = 600          # seconds a `git clone` of a new workspace may take (T45)
//! # insecure_http = false        # allow http:// to a non-loopback hub (never in production)
//! # allow_native_messaging = false  # keep Claude Code's ListAgents/SendMessage (bypass the hub)
//! # allow_subagent_messages = false # keep SendMessage for own subagents, refuse other sessions'
//! # show_in_vscode = true          # sessions run by hqc appear in VS Code's history
//! # session_secrets = false        # take secrets and ssh keys for a session from the chat
//! # vault = false                  # true: an hqc_secret card may take its value from Vaultwarden (Bitwarden) through this machine's rbw
//! # browser = false                # a session the owner gives it to drives Chrome here
//! # task_tools = false             # true: give the agent Claude Code's task tools
//!                                  # (TaskCreate, TaskUpdate, TaskList, TaskGet)
//! # long_run_note = 3300           # seconds: tell the agent every so long that a background
//!                                  # command or subagent of its own still runs; 0 = never
//!
//! [permissions]                  # optional, see crate::permissions
//! mode = "ask"                   # "ask" | "auto" | "allow_all": sessions that ask for none
//! allow_auto = true              # false: one that asks for "auto" runs "ask"
//! allow_all = false              # true: one may run "allow_all" (bypassPermissions)
//! allow = ["Bash(cargo test:*)"]
//! # deny: default crate::permissions::DEFAULT_DENY (sudo, ssh, scp, rm -rf /*, ...)
//!
//! [environments.infra]           # T43: what sessions in the bound folders run with
//! env = { GITHUB_TOKEN = "..." } # variables on top of the child allowlist
//! ssh_keys = ["infra_ed25519"]   # T44: keys of $HQC_HOME/keys/, an ssh-agent per process
//! allow_local_mcp = false        # true: every MCP server the machine's CLI knows, claude.ai
//!                                # connectors included (no --strict-mcp-config); prefer mcp_servers
//! project_settings = false       # true: the repository's CLAUDE.md and .claude/settings.json too
//! hub_mcp = false                # true: the remote MCP servers the hub keeps (SPEC 0.235)
//! hub_skills = false             # true: the skills the hub keeps (SPEC 0.245)
//! [environments.infra.permissions]
//! unblock = ["Bash(ssh:*)"]      # rules of the worker's deny lifted here only
//! allow = ["Bash(ssh debian@192.168.1.20:*)"]
//! deny = ["Bash(git push:*)"]    # added to the worker's deny
//!
//! [environments.infra.mcp_servers.figma]   # iteration 9: an MCP server of this machine
//! command = "npx"                # absolute, or looked up in PATH; runs in the session's folder
//! args = ["-y", "figma-developer-mcp@0.6.4", "--stdio"]
//! env = { FIGMA_API_KEY = { vault = "figma token" } }   # plain, or read from Vaultwarden at every start
//! # cwd = "/abs/folder"          # instead of the session's working folder
//! # pin = false                  # true by default: a changed tools/list fails the server until
//!                                # `hqc-worker mcp approve infra/figma`
//! [environments.infra.mcp_servers.figma-dev]
//! type = "http"                  # Streamable HTTP
//! url = "http://127.0.0.1:3845/mcp"
//! # headers = { Authorization = { vault = "dev token" } }
//!
//! [[env_bindings]]               # the longest path that contains the workspace wins
//! path = "/Users/me/work/lab/infra"
//! environment = "infra"
//!
//! [providers.qwen-local]         # SPEC 0.144: a model provider of the hub this worker runs
//! base_url = "http://192.168.1.229:8082"   # must equal the hub's description
//! # api_key = "..."              # for an endpoint that needs one; never in `hello` or logs
//! # api_key = { vault = "zai key", field = "password" }   # or read from Vaultwarden (rbw) at every agent start; `field` optional
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use hqc_proto::frame::{
    EnvBinding, EnvPermissions, EnvironmentInfo, Runtime, SessionStart, DEFAULT_ENVIRONMENT,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::adapter::claude_code::child_env;
use crate::permissions::{PermissionMode, Permissions, RawPermissions};
use crate::token::Secret;

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
    #[serde(default = "default_clone_timeout")]
    clone_timeout: u64,
    #[serde(default)]
    allow_native_messaging: bool,
    #[serde(default)]
    allow_subagent_messages: bool,
    #[serde(default = "default_true")]
    show_in_vscode: bool,
    #[serde(default)]
    session_secrets: bool,
    #[serde(default)]
    vault: bool,
    #[serde(default)]
    browser: bool,
    #[serde(default)]
    task_tools: bool,
    #[serde(default = "default_long_run_note")]
    long_run_note: u64,
    #[serde(default)]
    permissions: Option<RawPermissions>,
    #[serde(default)]
    environments: BTreeMap<String, RawEnvironment>,
    #[serde(default)]
    env_bindings: Vec<RawBinding>,
    #[serde(default)]
    providers: BTreeMap<String, RawProvider>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProvider {
    base_url: String,
    #[serde(default)]
    api_key: Option<RawProviderKey>,
}

/// A provider's `api_key`: the key itself, or the Vaultwarden entry (and its field) the worker
/// reads it from with `rbw` whenever it starts the agent.
#[derive(Debug, Clone, Deserialize)]
#[serde(
    untagged,
    expecting = "a key, or a table { vault = \"<entry>\", field = \"<field>\" }"
)]
enum RawProviderKey {
    Value(String),
    Vault(RawVaultKey),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVaultKey {
    vault: String,
    #[serde(default)]
    field: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEnvironment {
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    ssh_keys: Vec<String>,
    #[serde(default)]
    allow_local_mcp: bool,
    #[serde(default)]
    project_settings: bool,
    #[serde(default)]
    hub_mcp: bool,
    #[serde(default)]
    hub_skills: bool,
    #[serde(default)]
    permissions: RawEnvPermissions,
    #[serde(default)]
    mcp_servers: BTreeMap<String, RawMcpServer>,
}

/// `[environments.<name>.mcp_servers.<server>]` (SPEC §14, iteration 9).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMcpServer {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    env: Option<BTreeMap<String, RawValue>>,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    headers: Option<BTreeMap<String, RawValue>>,
    #[serde(default = "default_true")]
    pin: bool,
}

/// A value of a local MCP server's `env` or `headers`: as written, or a Vaultwarden entry.
#[derive(Debug, Clone, Deserialize)]
#[serde(
    untagged,
    expecting = "a value, or a table { vault = \"<entry>\", field = \"<field>\" }"
)]
enum RawValue {
    Value(String),
    Vault(RawVaultKey),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEnvPermissions {
    #[serde(default)]
    unblock: Vec<String>,
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
    #[serde(default)]
    remote_unblock: std::collections::BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBinding {
    path: PathBuf,
    environment: String,
}

/// Variables of an environment's `env`. Their values go into the agent's process only: not
/// into `hello`, logs, errors or `hqc-worker env`, and `Debug` prints `***`.
#[derive(Clone, Default, PartialEq)]
pub struct EnvVars(BTreeMap<String, String>);

impl EnvVars {
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }
}

impl fmt::Debug for EnvVars {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.0.keys().map(|k| (k, "***")))
            .finish()
    }
}

/// An environment (SPEC §14): what the agent process of a session in a bound folder is
/// launched with. `default` has no variables, no keys, no local MCP and the worker's
/// `[permissions]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Environment {
    pub name: String,
    pub env: EnvVars,
    /// Names of keys in the worker's store ([`crate::ssh`]); each agent process gets an
    /// ssh-agent of its own with them. Empty: no `SSH_AUTH_SOCK` at all.
    pub ssh_keys: Vec<String>,
    pub allow_local_mcp: bool,
    /// Claude Code reads the project setting source too (`--setting-sources
    /// user,project,local`); worker-local, not in `hello`.
    pub project_settings: bool,
    /// The sessions get the remote MCP servers the hub keeps for their project, each as a
    /// stdio relay to the hub (SPEC 0.235); worker-local, not in `hello`.
    pub hub_mcp: bool,
    /// The sessions get the skills the hub keeps for their project, in a plugin folder the
    /// worker builds (`--plugin-dir`, SPEC 0.245); worker-local, not in `hello`.
    pub hub_skills: bool,
    /// `[environments.<name>.mcp_servers.*]` by name (iteration 9): the MCP servers of this
    /// machine its sessions get, each through the launcher `hqc-worker mcp --local <name>`.
    pub mcp_servers: Vec<Arc<LocalMcpServer>>,
    /// `[environments.<name>.permissions]` as written.
    pub rules: EnvPermissions,
    /// What its sessions run with ([`Permissions::for_environment`]).
    pub permissions: Permissions,
}

impl Environment {
    /// `default` over the worker's `[permissions]`.
    pub fn default_of(permissions: &Permissions) -> Self {
        Self {
            name: DEFAULT_ENVIRONMENT.into(),
            env: EnvVars::default(),
            ssh_keys: vec![],
            allow_local_mcp: false,
            project_settings: false,
            hub_mcp: false,
            hub_skills: false,
            mcp_servers: vec![],
            rules: EnvPermissions::default(),
            permissions: permissions.clone(),
        }
    }

    /// The environment of a process the worker starts for this environment: the agent
    /// (its adapter adds its own variables after these) and `git` for a workspace (T45). The
    /// child allowlist of the worker's own environment ([`child_env`]) with `env` on top.
    pub fn process_env(&self) -> Vec<(String, String)> {
        let mut env = child_env(std::env::vars());
        for (k, v) in self.env.iter() {
            env.retain(|(x, _)| x != k);
            env.push((k.into(), v.into()));
        }
        env.sort();
        env
    }

    /// What `hello` says about it: no values of `env`.
    pub fn info(&self) -> EnvironmentInfo {
        EnvironmentInfo {
            name: self.name.clone(),
            allow_local_mcp: self.allow_local_mcp,
            permissions: self.rules.clone(),
        }
    }
}

fn parse_provider(name: &str, raw: RawProvider) -> anyhow::Result<AllowedProvider> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("a provider's name is letters, digits, '-' and '_'");
    }
    let base_url = raw.base_url.trim_end_matches('/').to_string();
    if !(base_url.starts_with("http://") || base_url.starts_with("https://"))
        || base_url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        bail!("base_url must be an http:// or https:// URL");
    }
    let (api_key, api_key_vault) = match raw.api_key {
        Some(RawProviderKey::Value(k)) if !k.is_empty() => (Some(Secret::new(k)), None),
        Some(RawProviderKey::Vault(v)) => {
            hqc_proto::frame::check_vault_entry(&v.vault).map_err(anyhow::Error::msg)?;
            if let Some(f) = &v.field {
                hqc_proto::frame::check_vault_field(f).map_err(anyhow::Error::msg)?;
            }
            (
                None,
                Some(VaultRef {
                    entry: v.vault,
                    field: v.field,
                }),
            )
        }
        _ => (None, None),
    };
    Ok(AllowedProvider {
        name: name.to_string(),
        base_url,
        api_key,
        api_key_vault,
    })
}

fn parse_environment(
    name: &str,
    raw: RawEnvironment,
    worker: &Permissions,
) -> anyhow::Result<Environment> {
    if name == DEFAULT_ENVIRONMENT {
        bail!("\"default\" is the environment of unbound folders and cannot be defined");
    }
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        bail!("{name:?} is not an environment name (letters, digits, _ - .)");
    }
    for (k, v) in &raw.env {
        check_var_name(k)?;
        if k == crate::ssh::SSH_AUTH_SOCK {
            bail!("env: {k}: the worker sets it for an environment with ssh_keys only");
        }
        if v.contains('\0') {
            bail!("env: {k}: the value contains a NUL character");
        }
    }
    if let Some(k) = raw.ssh_keys.iter().find(|k| !crate::ssh::valid_key_name(k)) {
        bail!(
            "ssh_keys: {k:?} is not a key name (letters, digits, _ - ., not starting with . or -)"
        );
    }
    let mcp_servers = raw
        .mcp_servers
        .into_iter()
        .map(|(n, s)| {
            parse_mcp_server(&n, s)
                .map(Arc::new)
                .with_context(|| format!("mcp_servers.{n}"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let rules = EnvPermissions {
        unblock: raw.permissions.unblock,
        allow: raw.permissions.allow,
        deny: raw.permissions.deny,
        remote_unblock: raw.permissions.remote_unblock,
    };
    Ok(Environment {
        name: name.into(),
        env: EnvVars(raw.env),
        ssh_keys: raw.ssh_keys,
        allow_local_mcp: raw.allow_local_mcp,
        project_settings: raw.project_settings,
        hub_mcp: raw.hub_mcp,
        hub_skills: raw.hub_skills,
        mcp_servers,
        permissions: worker.for_environment(&rules)?,
        rules,
    })
}

/// A variable name of an `env` table: letters, digits and `_`, not starting with a digit,
/// never the worker's own `HQC_*`.
fn check_var_name(k: &str) -> anyhow::Result<()> {
    if k.is_empty()
        || k.starts_with(|c: char| c.is_ascii_digit())
        || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        bail!("env: {k:?} is not a variable name (letters, digits and _)");
    }
    if k.starts_with("HQC_") {
        bail!("env: {k}: HQC_* variables are the worker's own");
    }
    Ok(())
}

fn parse_value(what: &str, raw: RawValue) -> anyhow::Result<McpValue> {
    match raw {
        RawValue::Value(v) => {
            if v.contains('\0') {
                bail!("{what}: the value contains a NUL character");
            }
            Ok(McpValue::Plain(Secret::new(v)))
        }
        RawValue::Vault(v) => {
            hqc_proto::frame::check_vault_entry(&v.vault)
                .map_err(|e| anyhow::anyhow!("{what}: {e}"))?;
            if let Some(f) = &v.field {
                hqc_proto::frame::check_vault_field(f)
                    .map_err(|e| anyhow::anyhow!("{what}: {e}"))?;
            }
            Ok(McpValue::Vault(VaultRef {
                entry: v.vault,
                field: v.field,
            }))
        }
    }
}

/// `[environments.<env>.mcp_servers.<name>]`: checked at load, so a mistake stops the worker
/// rather than a session; a program that is missing fails only its server, at the start.
fn parse_mcp_server(name: &str, raw: RawMcpServer) -> anyhow::Result<LocalMcpServer> {
    if !hqc_proto::mcp::valid_server_name(name) {
        bail!(
            "{name:?} is not a server name: 1 to {} of a-z, 0-9, - and _, not hqc",
            hqc_proto::mcp::MAX_SERVER_NAME
        );
    }
    let transport = match raw.kind.as_deref().unwrap_or("stdio") {
        "stdio" => {
            if raw.url.is_some() || raw.headers.is_some() {
                bail!("url and headers are for type = \"http\"");
            }
            let command = raw.command.context("a stdio server needs command")?;
            if command.is_empty() || command.contains('\0') {
                bail!("command is empty");
            }
            if command.contains('/') && !Path::new(&command).is_absolute() {
                bail!(
                    "command {command:?} is an absolute path or a program name looked up in PATH"
                );
            }
            let args = raw.args.unwrap_or_default();
            if args.iter().any(|a| a.contains('\0')) {
                bail!("args: an argument contains a NUL character");
            }
            let mut env = Vec::new();
            for (k, v) in raw.env.unwrap_or_default() {
                check_var_name(&k)?;
                env.push((k.clone(), parse_value(&format!("env.{k}"), v)?));
            }
            let cwd = match raw.cwd {
                Some(c) if !c.is_absolute() => {
                    bail!("cwd {} is not an absolute path", c.display())
                }
                c => c,
            };
            LocalMcpTransport::Stdio {
                command,
                args,
                env,
                cwd,
            }
        }
        "http" => {
            if raw.command.is_some() || raw.args.is_some() || raw.env.is_some() || raw.cwd.is_some()
            {
                bail!("command, args, env and cwd are for a stdio server");
            }
            let url = raw.url.context("an http server needs url")?;
            let u = reqwest::Url::parse(&url).with_context(|| format!("url {url:?}"))?;
            if !matches!(u.scheme(), "http" | "https") {
                bail!("url must be http:// or https://");
            }
            if !u.username().is_empty() || u.password().is_some() {
                bail!("url must not carry credentials: put them in headers");
            }
            let mut headers = Vec::new();
            for (k, v) in raw.headers.unwrap_or_default() {
                if k.is_empty()
                    || !k
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
                {
                    bail!("headers: {k:?} is not a header name");
                }
                let v = parse_value(&format!("headers.{k}"), v)?;
                if let McpValue::Plain(p) = &v {
                    if p.expose().contains(['\r', '\n']) {
                        bail!("headers.{k}: the value contains a line break");
                    }
                }
                headers.push((k, v));
            }
            LocalMcpTransport::Http { url, headers }
        }
        other => bail!("type {other:?} is \"stdio\" or \"http\""),
    };
    Ok(LocalMcpServer {
        name: name.into(),
        pin: raw.pin,
        transport,
    })
}

fn default_true() -> bool {
    true
}

fn default_max_sessions() -> usize {
    DEFAULT_MAX_SESSIONS
}

fn default_clone_timeout() -> u64 {
    DEFAULT_CLONE_TIMEOUT.as_secs()
}

fn default_long_run_note() -> u64 {
    DEFAULT_LONG_RUN_NOTE.as_secs()
}

/// How long `git clone` of a new workspace may take, unless `clone_timeout` says otherwise.
pub const DEFAULT_CLONE_TIMEOUT: Duration = Duration::from_secs(600);

/// How often the agent is told of a background command or subagent of its own that still runs,
/// unless `long_run_note` says otherwise (SPEC §14, §8.1). With the 1-hour prompt cache
/// (`cache_ttl_s` 3600 in `usage`) it is below the hour the cache lives after the agent's last
/// request, which is never before a watched task started (for one a subagent left: before that
/// subagent's report started a turn of the agent): the turn of the first note reads the cache,
/// and each note's turn is a request that renews it for the next. A value of an hour or more
/// misses it; with the 5-minute cache or on a provider the note's turn misses it at any value.
pub const DEFAULT_LONG_RUN_NOTE: Duration = Duration::from_secs(55 * 60);

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

/// A model provider `worker.toml` allows (`[providers.<name>]`, SPEC §14, since 0.144): a
/// session the hub starts with a provider of this name and this `base_url` runs on it. The
/// hub keeps the description; the worker decides, and keeps the key.
#[derive(Debug, Clone, PartialEq)]
pub struct AllowedProvider {
    pub name: String,
    /// Without a trailing `/`, as it is compared.
    pub base_url: String,
    /// `ANTHROPIC_AUTH_TOKEN` for an endpoint that needs one; never in `hello` or logs.
    pub api_key: Option<Secret>,
    /// Instead of `api_key`: the Vaultwarden entry the worker reads it from at every start of
    /// the agent.
    pub api_key_vault: Option<VaultRef>,
}

/// A Vaultwarden entry and, when it is not the password, the field of it (as
/// [`crate::vault::read_for`] takes them): neither is secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultRef {
    pub entry: String,
    pub field: Option<String>,
}

/// A value of a local MCP server's `env` or `headers` (iteration 9): `Debug` never prints a
/// plain one, and a Vaultwarden one is read at every start of the server.
#[derive(Debug, Clone, PartialEq)]
pub enum McpValue {
    Plain(Secret),
    Vault(VaultRef),
}

/// An MCP server of this machine an environment gives its sessions (`[environments.<env>.
/// mcp_servers.<name>]`, iteration 9). Neither the hub nor the agent can add or change one.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalMcpServer {
    pub name: String,
    /// Its tools are pinned ([`crate::mcp::pins`]); `pin = false` turns that off.
    pub pin: bool,
    pub transport: LocalMcpTransport,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LocalMcpTransport {
    /// A program the launcher starts, in the session's working folder unless `cwd` says
    /// otherwise; `command` is absolute or looked up in `PATH`.
    Stdio {
        command: String,
        args: Vec<String>,
        env: Vec<(String, McpValue)>,
        cwd: Option<PathBuf>,
    },
    /// A Streamable HTTP server the launcher relays to, adding `headers`.
    Http {
        url: String,
        headers: Vec<(String, McpValue)>,
    },
}

/// The local MCP servers a session gets ([`StartSpec::local_mcp`]): its environment's, with
/// what the launcher needs to resolve and pin them.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalMcp {
    pub environment: String,
    pub servers: Vec<Arc<LocalMcpServer>>,
    /// The `rbw` the `{ vault = ... }` values are read with ([`Config::rbw_bin`]).
    pub rbw: PathBuf,
    /// `$HQC_HOME/mcp-pins.json`.
    pub pins: PathBuf,
}

impl LocalMcp {
    pub fn names(&self) -> Vec<String> {
        self.servers.iter().map(|s| s.name.clone()).collect()
    }
}

/// A [`VaultRef`] with the `rbw` that reads it ([`Config::rbw_bin`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultKey {
    pub entry: String,
    pub field: Option<String>,
    pub rbw: PathBuf,
}

/// A provider a session runs on: the hub's description, checked against the worker's
/// [`AllowedProvider`] ([`Config::check_start`]).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionProvider {
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub context_tokens: Option<u32>,
    pub api_key: Option<Secret>,
    /// The key is in Vaultwarden: the supervisor reads it into a copy of the spec at every
    /// start of the agent, so `api_key` is empty here.
    pub api_key_vault: Option<VaultKey>,
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
    /// How long `git clone` of a new workspace may take before it is killed (T45).
    pub clone_timeout: Duration,
    /// Keep Claude Code's own cross-session tools (`ListAgents`, `SendMessage`). Off by
    /// default: they bypass the hub, so the other session's history misses the input that
    /// started its turn. Agent-to-agent traffic goes through `hqc_send`/`hqc_ask` (SPEC §7).
    pub allow_native_messaging: bool,
    /// Keep `SendMessage` only, which is also how an agent continues its own subagent, and
    /// refuse messages from other sessions (`crossSessionInbound`). The agent can still send
    /// to another local session that accepts them, past the hub (SPEC §8.1). Off by default;
    /// `allow_native_messaging` wins.
    pub allow_subagent_messages: bool,
    /// Write transcripts with `entrypoint: claude-vscode`, so sessions run by hqc appear in
    /// the VS Code extension's history (it lists only its own entrypoint, SPEC §9).
    pub show_in_vscode: bool,
    /// Take `secret.put` from the hub for sessions of the worker's owner that are not
    /// restricted (SPEC §14, since 0.147): a secret becomes a file of the agent's process, a key
    /// goes into its ssh-agent and may be kept for the workspace. Off by default.
    pub session_secrets: bool,
    /// An `hqc_secret` card may take its value from Vaultwarden (Bitwarden): the worker reads
    /// the entry the owner confirms with `rbw` on this machine, and the value does not pass
    /// through the hub. Effective only with `session_secrets`. Off by default.
    pub vault: bool,
    /// The `rbw` the worker runs to read the vault: `rbw` from `PATH`; set by [`Config::parse`]
    /// (not a `worker.toml` key), a test points it at a fake script.
    pub rbw_bin: PathBuf,
    /// A session its owner gives the browser runs Claude Code with `--chrome`: the user's
    /// Chrome on this machine, through the Claude in Chrome extension (SPEC §14, since 0.148).
    /// Off by default; every other session runs with `--no-chrome`.
    pub browser: bool,
    /// Claude Code's task list tools (`CLAUDE_CODE_ENABLE_TODO_TOOLS=1`): without it CLI
    /// 2.1.312 gives the 5.x models none, and the chat has no task list to show (SPEC §14, §8.1,
    /// since 0.223). Off by default.
    pub task_tools: bool,
    /// How often the agent is told that a background command or subagent of its own still runs,
    /// from the task's start (SPEC §14, §8.1, since 0.244); `None` (`long_run_note = 0`): never.
    /// [`DEFAULT_LONG_RUN_NOTE`] unless written.
    pub long_run_note: Option<Duration>,
    /// `[permissions]`: the worker's allow/deny rules and the ask/auto mode (SPEC §14). A
    /// session runs with its environment's [`Environment::permissions`], built on these.
    pub permissions: Permissions,
    /// `[environments.*]` by name; `default` is not among them ([`Config::environment`]).
    pub environments: Vec<Arc<Environment>>,
    /// `[[env_bindings]]`, paths absolute and canonical when they existed at load.
    pub env_bindings: Vec<EnvBinding>,
    /// The environment of unbound folders and of sessions without a workspace.
    pub default_environment: Arc<Environment>,
    /// Where sessions without a workspace get their empty folder (SPEC §8.4):
    /// `$HQC_HOME/scratch/<session_id>`.
    pub scratch_root: PathBuf,
    /// The skills of the hub (SPEC 0.245): `$HQC_HOME/hub-skills`, its archives and plugin
    /// folders ([`crate::hub_skills`]).
    pub skills_root: PathBuf,
    /// The key store `$HQC_HOME/keys` and the agent sockets `$HQC_HOME/agents` (T44).
    pub ssh: crate::ssh::Store,
    /// `[providers.*]` by name.
    pub providers: Vec<AllowedProvider>,
    /// `$HQC_HOME/secrets` and `$HQC_HOME/workspace-keys` (SPEC 0.147).
    pub secrets: crate::secrets::Stores,
    /// The pins of the local MCP servers' tools: `$HQC_HOME/mcp-pins.json` (iteration 9).
    pub mcp_pins: PathBuf,
}

/// What a session that may have secrets gets (SPEC §14, since 0.147): the stores, the keys
/// kept for its workspace (none for a session without one) and, with `vault` on, the `rbw` its
/// secrets may be read from Vaultwarden with.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSecrets {
    pub stores: crate::secrets::Stores,
    pub workspace: Option<crate::secrets::WorkspaceKeys>,
    pub rbw: Option<PathBuf>,
}

/// What a session may be started with, after checking `session.start` against the config.
#[derive(Debug, Clone, PartialEq)]
pub struct StartSpec {
    pub session_id: Uuid,
    pub runtime: Runtime,
    /// `session.start.model`; a later `session.model` changes it.
    pub model: Option<String>,
    /// The workspace (canonical, inside `allowed_roots`), not the hub's string; for a
    /// session without workspace its scratch folder.
    pub workspace: PathBuf,
    /// Canonical `<workspace>/<cwd>`, the process working directory (SPEC §8.1); checked
    /// again before every spawn ([`StartSpec::recheck_workdir`]).
    pub workdir: PathBuf,
    /// Resolved by the worker from the canonical workspace path (SPEC §14), never taken
    /// from the hub.
    pub environment: Arc<Environment>,
    pub runtime_session_id: Option<String>,
    pub system_prompt: Option<String>,
    pub disallowed_tools: Vec<String>,
    pub restricted: bool,
    pub auto_approve: bool,
    pub session_token: Secret,
    pub ask_timeout_s: u32,
    /// The mode the agent runs in: `session.start.permission_mode` as `[permissions]`
    /// allows it (T38, [`crate::permissions::Permissions::mode_for`]); a later
    /// `session.permission_mode` changes it.
    pub permission_mode: PermissionMode,
    /// hqc's allow rules of the workspace (T48), shared by every session in it; `None` for a
    /// session without workspace.
    pub folder_rules: Option<crate::permissions::FolderRules>,
    /// `session.start.fork` (SPEC §5.3): the conversation this session forks, at its first
    /// start; the Claude Code adapter reads the source transcript in the same folder only.
    pub fork: Option<hqc_proto::frame::ForkFrom>,
    /// `session.start.has_history` (SPEC §5.3): the session has had a conversation, so a
    /// missing transcript is reported, not just started anew.
    pub has_history: bool,
    /// `session.start.provider` as this worker allows it; `None`: the runtime's own endpoint.
    pub provider: Option<SessionProvider>,
    /// Why `session.start.provider` was not run (SPEC §8.1 `provider_refused`): the session
    /// starts without it.
    pub provider_refused: Option<String>,
    /// `session_secrets` is on, and the session is its owner's on this worker's owner and not
    /// restricted (SPEC §14): the process gets `HQC_SECRETS`, an ssh-agent and the workspace's
    /// kept keys. `None`: none of it.
    pub secrets: Option<SessionSecrets>,
    /// `session.start.browser` as this worker allows it (SPEC 0.148): `--chrome`.
    pub browser: bool,
    /// Why `session.start.browser` was not run (`system browser_refused`): the session starts
    /// without it.
    pub browser_refused: Option<String>,
    /// `session.start.mcp_servers` as this worker takes them (SPEC 0.235): only in an
    /// environment with `hub_mcp`, for an unrestricted Claude Code session of the worker's
    /// owner, and only valid names.
    pub mcp_servers: Vec<String>,
    /// The MCP servers of this machine the session's environment names (iteration 9), on the
    /// same terms as [`Self::mcp_servers`] but decided by `worker.toml` alone; a server of the
    /// hub of the same name is left out of [`Self::mcp_servers`]. `None`: none.
    pub local_mcp: Option<LocalMcp>,
    /// `session.start.skills` as this worker takes them (SPEC 0.245): only in an environment
    /// with `hub_skills`, for an unrestricted Claude Code session of the worker's owner, valid
    /// names and hashes, one per name.
    pub skills: Vec<hqc_proto::skills::SkillRef>,
    /// The plugin folder with [`Self::skills`] the supervisor built before the start
    /// ([`crate::hub_skills::HubSkills::prepare`]): `--plugin-dir`. `None`: no skill of the
    /// hub could be had, or none was given.
    pub skill_dir: Option<PathBuf>,
}

impl StartSpec {
    /// The working directory still exists and still resolves inside the workspace: a symlink
    /// may have been swapped since `session.start` (SPEC §2.1).
    pub fn recheck_workdir(&self) -> anyhow::Result<()> {
        recheck_workdir(&self.workspace, &self.workdir)
    }
}

/// [`StartSpec::recheck_workdir`] for the canonical `workspace` and `workdir` of a session.
pub fn recheck_workdir(workspace: &Path, workdir: &Path) -> anyhow::Result<()> {
    let ws = workspace
        .canonicalize()
        .with_context(|| format!("workspace {}", workspace.display()))?;
    let real = workdir
        .canonicalize()
        .with_context(|| format!("working directory {}", workdir.display()))?;
    if real != workdir || !real.starts_with(&ws) || !real.is_dir() {
        bail!(
            "working directory {} no longer resolves inside the workspace",
            workdir.display()
        );
    }
    Ok(())
}

impl Config {
    /// [`Config::parse`] the file, then [`Config::check_ssh`]: the worker does not start
    /// (and `hqc-worker env` does not print) with a key store others could read.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading worker config {}", path.display()))?;
        let cfg =
            Self::parse(&text).with_context(|| format!("worker config {}", path.display()))?;
        cfg.check_ssh()
            .with_context(|| format!("worker config {}", path.display()))?;
        Ok(cfg)
    }

    /// The modes of the key store and the agents folder, and every `ssh_keys` name is in the
    /// store ([`crate::ssh::Store::check`]).
    pub fn check_ssh(&self) -> anyhow::Result<()> {
        self.ssh.check(
            self.environments
                .iter()
                .flat_map(|e| e.ssh_keys.iter().map(|k| k.as_str())),
        )
    }

    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let raw: RawConfig = toml::from_str(text)?;
        check_hub_url(&raw.hub_url, raw.insecure_http)?;
        if raw.max_sessions == 0 {
            bail!("max_sessions must be at least 1");
        }
        if raw.clone_timeout == 0 {
            bail!("clone_timeout must be at least 1 second");
        }
        if raw.allowed_roots.is_empty() {
            bail!("allowed_roots is empty");
        }
        let allowed_roots = raw
            .allowed_roots
            .iter()
            .map(|p| absolute(p).context("allowed_roots"))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let permissions = Permissions::from_raw(raw.permissions)?;
        let environments = raw
            .environments
            .into_iter()
            .map(|(name, e)| {
                parse_environment(&name, e, &permissions)
                    .map(Arc::new)
                    .with_context(|| format!("environments.{name}"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut env_bindings: Vec<EnvBinding> = Vec::new();
        for b in raw.env_bindings {
            let path = absolute(&b.path).context("env_bindings")?;
            let path = path
                .to_str()
                .with_context(|| format!("env_bindings: {} is not UTF-8", path.display()))?
                .to_string();
            if b.environment != DEFAULT_ENVIRONMENT
                && !environments.iter().any(|e| e.name == b.environment)
            {
                bail!(
                    "env_bindings: {path}: there is no [environments.{}]",
                    b.environment
                );
            }
            if env_bindings.iter().any(|x| x.path == path) {
                bail!("env_bindings: {path} is bound twice");
            }
            env_bindings.push(EnvBinding {
                path,
                environment: b.environment,
            });
        }
        let providers = raw
            .providers
            .into_iter()
            .map(|(name, p)| parse_provider(&name, p).with_context(|| format!("providers.{name}")))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let home = hqc_home().unwrap_or_else(|_| std::env::temp_dir().join("hqc"));
        Ok(Self {
            hub_url: raw.hub_url.trim_end_matches('/').to_string(),
            allowed_roots,
            legacy_workspace: raw.workspace.is_some(),
            idle_timeout: (raw.idle_timeout > 0).then(|| Duration::from_secs(raw.idle_timeout)),
            allow_foreign_sessions: raw.allow_foreign_sessions,
            claude_bin: raw.claude_bin,
            insecure_http: raw.insecure_http,
            allow_native_messaging: raw.allow_native_messaging,
            allow_subagent_messages: raw.allow_subagent_messages,
            show_in_vscode: raw.show_in_vscode,
            session_secrets: raw.session_secrets,
            vault: raw.vault,
            rbw_bin: PathBuf::from("rbw"),
            browser: raw.browser,
            task_tools: raw.task_tools,
            long_run_note: (raw.long_run_note > 0).then(|| Duration::from_secs(raw.long_run_note)),
            default_environment: Arc::new(Environment::default_of(&permissions)),
            permissions,
            environments,
            env_bindings,
            max_sessions: raw.max_sessions,
            clone_timeout: Duration::from_secs(raw.clone_timeout),
            scratch_root: home.join("scratch"),
            skills_root: home.join("hub-skills"),
            ssh: crate::ssh::Store::in_home(&home),
            secrets: crate::secrets::Stores::in_home(&home),
            mcp_pins: home.join(crate::mcp::pins::FILE),
            providers,
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
        // Resolved here on every `session.start`, never taken from the hub (SPEC §2.1).
        let environment = match &s.workspace_path {
            Some(_) => self.environment(&ws),
            None => self.default_environment.clone(),
        };
        if s.acp_agent.is_some() {
            return Err("acp_agent is not configured on this worker".into());
        }
        if !s.owner_is_worker_owner && !self.allow_foreign_sessions {
            return Err("foreign sessions are not allowed on this worker".into());
        }
        let (provider, provider_refused) = match &s.provider {
            None => (None, None),
            Some(p) => match self.provider(p) {
                Ok(p) => (Some(p), None),
                Err(why) => (None, Some(why)),
            },
        };
        // SPEC 0.148: the user's Chrome, only where `worker.toml` gives it, for the owner.
        let (browser, browser_refused) = if !s.browser {
            (false, None)
        } else if !self.browser {
            (
                false,
                Some("this worker gives no browser: browser is off in its worker.toml".to_string()),
            )
        } else if !s.owner_is_worker_owner || s.restricted || s.runtime != Runtime::ClaudeCode {
            (false, Some("only an unrestricted Claude Code session of the worker's owner gets the browser".to_string()))
        } else {
            (true, None)
        };
        let secrets = (self.session_secrets
            && s.owner_is_worker_owner
            && !s.restricted
            && s.runtime == Runtime::ClaudeCode)
            .then(|| SessionSecrets {
                stores: self.secrets.clone(),
                workspace: s
                    .workspace_path
                    .as_ref()
                    .map(|_| self.secrets.workspace(&ws)),
                rbw: self.vault.then(|| self.rbw_bin.clone()),
            });
        // Iteration 9: the machine's own servers, for the owner's unrestricted Claude Code
        // session, as the hub's are.
        let local_mcp = (!environment.mcp_servers.is_empty()
            && s.owner_is_worker_owner
            && !s.restricted
            && s.runtime == Runtime::ClaudeCode)
            .then(|| LocalMcp {
                environment: environment.name.clone(),
                servers: environment.mcp_servers.clone(),
                rbw: self.rbw_bin.clone(),
                pins: self.mcp_pins.clone(),
            });
        // SPEC 0.235: the hub's MCP servers, only where `worker.toml` takes them.
        let mut mcp_servers = if s.mcp_servers.is_empty() {
            vec![]
        } else if !environment.hub_mcp
            || !s.owner_is_worker_owner
            || s.restricted
            || s.runtime != Runtime::ClaudeCode
        {
            tracing::info!(
                session_id = %s.session_id,
                servers = ?s.mcp_servers,
                "the hub's MCP servers not taken: hub_mcp is off here or the session is not \
                 the owner's unrestricted Claude Code session"
            );
            vec![]
        } else {
            let mut names: Vec<String> = s
                .mcp_servers
                .iter()
                .filter(|n| hqc_proto::mcp::valid_server_name(n))
                .cloned()
                .collect();
            names.sort();
            names.dedup();
            names
        };
        // A server of the machine wins over the hub's of the same name (iteration 9).
        if let Some(l) = &local_mcp {
            let local = l.names();
            let shadowed: Vec<String> = mcp_servers
                .iter()
                .filter(|n| local.contains(n))
                .cloned()
                .collect();
            if !shadowed.is_empty() {
                tracing::info!(
                    session_id = %s.session_id,
                    servers = ?shadowed,
                    environment = %l.environment,
                    "the hub's MCP servers of these names left out: the environment has its own"
                );
                mcp_servers.retain(|n| !local.contains(n));
            }
        }
        // SPEC 0.245: the hub's skills, only where `worker.toml` takes them.
        let skills = if s.skills.is_empty() {
            vec![]
        } else if !environment.hub_skills
            || !s.owner_is_worker_owner
            || s.restricted
            || s.runtime != Runtime::ClaudeCode
        {
            tracing::info!(
                session_id = %s.session_id,
                skills = ?s.skills.iter().map(|k| &k.name).collect::<Vec<_>>(),
                "the hub's skills not taken: hub_skills is off here or the session is not \
                 the owner's unrestricted Claude Code session"
            );
            vec![]
        } else {
            let mut skills: Vec<hqc_proto::skills::SkillRef> = s
                .skills
                .iter()
                .filter(|k| {
                    hqc_proto::skills::valid_skill_name(&k.name)
                        && hqc_proto::skills::valid_sha256(&k.sha256)
                })
                .cloned()
                .collect();
            skills.sort();
            skills.dedup_by(|a, b| a.name == b.name);
            skills
        };
        Ok(StartSpec {
            session_id: s.session_id,
            runtime: s.runtime,
            model: s.model.clone(),
            workspace: ws,
            workdir,
            environment,
            runtime_session_id: s.runtime_session_id.clone(),
            system_prompt: s.system_prompt.clone(),
            disallowed_tools: s.disallowed_tools.clone(),
            restricted: s.restricted,
            auto_approve: s.auto_approve,
            session_token: Secret::new(s.session_token.clone()),
            ask_timeout_s: s.ask_timeout_s,
            permission_mode: auto_off_provider(
                s.session_id,
                self.permission_mode(s.session_id, s.permission_mode),
                provider.is_some(),
            ),
            // The supervisor gives a session in a workspace the folder's shared list.
            folder_rules: None,
            fork: s.fork.clone(),
            has_history: s.has_history,
            provider,
            provider_refused,
            secrets,
            browser,
            browser_refused,
            mcp_servers,
            local_mcp,
            skills,
            skill_dir: None,
        })
    }

    /// The hub's description of a provider as this worker allows it (SPEC §14): its name in
    /// `[providers.*]` with the same `base_url`, and a model that may go on a command line.
    /// `Err` is the reason the session starts without it.
    pub fn provider(&self, p: &hqc_proto::frame::Provider) -> Result<SessionProvider, String> {
        let Some(allowed) = self.providers.iter().find(|a| a.name == p.name) else {
            return Err(format!(
                "this worker does not allow the provider {:?}",
                p.name
            ));
        };
        if p.base_url.trim_end_matches('/') != allowed.base_url {
            return Err(format!(
                "the provider {:?} is allowed on this worker with another base_url",
                p.name
            ));
        }
        if !valid_model(&p.model) {
            return Err(format!("{:?} is not a model name", p.model));
        }
        Ok(SessionProvider {
            name: allowed.name.clone(),
            base_url: allowed.base_url.clone(),
            model: p.model.clone(),
            context_tokens: p.context_tokens,
            api_key: allowed.api_key.clone(),
            api_key_vault: allowed.api_key_vault.as_ref().map(|v| VaultKey {
                entry: v.entry.clone(),
                field: v.field.clone(),
                rbw: self.rbw_bin.clone(),
            }),
        })
    }

    /// The environment of a canonical folder (SPEC §14): the binding with the longest path
    /// that contains it, by components, else `default`. Binding paths are canonicalized again
    /// here, so a binding through a symlink made after the start names its target.
    pub fn environment(&self, path: &Path) -> Arc<Environment> {
        self.binding(path)
            .as_ref()
            .and_then(|b| self.environments.iter().find(|e| e.name == b.environment))
            .unwrap_or(&self.default_environment)
            .clone()
    }

    /// The binding [`Config::environment`] goes by, with its path canonical now.
    pub fn binding(&self, path: &Path) -> Option<EnvBinding> {
        let bindings: Vec<EnvBinding> = self
            .env_bindings
            .iter()
            .map(|b| EnvBinding {
                path: Path::new(&b.path)
                    .canonicalize()
                    .ok()
                    .and_then(|p| p.to_str().map(String::from))
                    .unwrap_or_else(|| b.path.clone()),
                environment: b.environment.clone(),
            })
            .collect();
        hqc_proto::frame::binding_for(&bindings, &path.to_string_lossy()).cloned()
    }

    /// Environments no `[[env_bindings]]` names: no session runs in them, which is most
    /// often a misnamed table (`[environments.x.permissions]` under `[environments.y]`).
    pub fn unbound_environments(&self) -> Vec<&str> {
        self.environments
            .iter()
            .map(|e| e.name.as_str())
            .filter(|n| !self.env_bindings.iter().any(|b| b.environment == *n))
            .collect()
    }

    /// The mode a session runs in when hqc asks for `asked` (T38): `auto` only with
    /// `[permissions] allow_auto`, `allow_all` only with `[permissions] allow_all` (SPEC
    /// 0.129); a refusal is logged, the session runs in the highest mode the worker allows.
    pub fn permission_mode(
        &self,
        session_id: Uuid,
        asked: Option<PermissionMode>,
    ) -> PermissionMode {
        let mode = self.permissions.mode_for(asked);
        if let Some(a) = asked.filter(|a| *a != mode) {
            let off = match (a, mode) {
                (PermissionMode::AllowAll, PermissionMode::Auto) => "allow_all = false",
                (PermissionMode::AllowAll, _) => "allow_all = false, allow_auto = false",
                _ => "allow_auto = false",
            };
            tracing::warn!(%session_id, "the hub asked for permission mode {}; [permissions] {off}, running {}", a.as_str(), mode.as_str());
        }
        mode
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
pub(crate) fn valid_model(m: &str) -> bool {
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

/// The mode a session on a model provider runs in (#194, SPEC 0.246): `ask` for `auto`. Auto's
/// safety check runs on Anthropic's servers and does not pass through a provider's endpoint,
/// so in Auto the CLI refuses every call that needs it; `ask` and `allow_all` are as asked.
pub fn auto_off_provider(
    session_id: Uuid,
    mode: PermissionMode,
    on_provider: bool,
) -> PermissionMode {
    if on_provider && mode == PermissionMode::Auto {
        tracing::info!(%session_id, "auto on a model provider: running ask, the safety check does not pass through a provider");
        return PermissionMode::Ask;
    }
    mode
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

    /// SPEC 0.147: secrets only with `session_secrets`, for the owner's session that is not
    /// restricted; its workspace's kept keys with it.
    #[test]
    fn secrets_only_for_the_owners_unrestricted_sessions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("ws")).unwrap();
        let toml = |extra: &str| {
            format!(
                "hub_url = \"http://127.0.0.1:1/\"\nallowed_roots = [{:?}]\nallow_foreign_sessions = true\n{extra}",
                dir.path().display().to_string(),
            )
        };
        let off = Config::parse(&toml("")).unwrap();
        assert!(!off.session_secrets);
        let ws = dir.path().join("ws");
        assert!(off.check_start(&start(&ws)).unwrap().secrets.is_none());
        assert!(!off.vault && off.rbw_bin == Path::new("rbw"));
        assert!(Config::parse(&toml("vault = true\n")).unwrap().vault);
        let on = Config::parse(&toml("session_secrets = true\n")).unwrap();
        let mut s = start(&ws);
        s.owner_is_worker_owner = true;
        s.restricted = false;
        let got = on.check_start(&s).unwrap().secrets.unwrap();
        let canonical = ws.canonicalize().unwrap();
        assert_eq!(got.workspace, Some(on.secrets.workspace(&canonical)));
        let mut restricted = s.clone();
        restricted.restricted = true;
        assert!(on.check_start(&restricted).unwrap().secrets.is_none());
        let mut foreign = s.clone();
        foreign.owner_is_worker_owner = false;
        assert!(on.check_start(&foreign).unwrap().secrets.is_none());
    }

    /// SPEC 0.223: `task_tools` is off unless `task_tools = true` is written.
    #[test]
    fn the_task_tools_are_off_unless_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let toml = |extra: &str| {
            format!(
                "hub_url = \"http://127.0.0.1:1/\"\nallowed_roots = [{:?}]\n{extra}",
                dir.path().display().to_string(),
            )
        };
        assert!(!Config::parse(&toml("")).unwrap().task_tools);
        assert!(
            Config::parse(&toml("task_tools = true\n"))
                .unwrap()
                .task_tools
        );
    }

    /// SPEC 0.244: `long_run_note` is 3300 s unless written, `0` turns the notes off.
    #[test]
    fn the_long_run_note_has_a_default_and_can_be_turned_off() {
        let dir = tempfile::tempdir().unwrap();
        let toml = |extra: &str| {
            format!(
                "hub_url = \"http://127.0.0.1:1/\"\nallowed_roots = [{:?}]\n{extra}",
                dir.path().display().to_string(),
            )
        };
        let note = |extra: &str| Config::parse(&toml(extra)).unwrap().long_run_note;
        assert_eq!(note(""), Some(Duration::from_secs(3300)));
        assert_eq!(note("long_run_note = 0\n"), None);
        assert_eq!(
            note("long_run_note = 1200\n"),
            Some(Duration::from_secs(1200))
        );
    }

    /// SPEC 0.148: the browser only with `browser = true`, for the owner's unrestricted
    /// session; otherwise the session starts without it and says why.
    #[test]
    fn the_browser_only_where_the_worker_gives_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("ws")).unwrap();
        let toml = |extra: &str| {
            format!(
                "hub_url = \"http://127.0.0.1:1/\"\nallowed_roots = [{:?}]\nallow_foreign_sessions = true\n{extra}",
                dir.path().display().to_string(),
            )
        };
        let mut s = start(&dir.path().join("ws"));
        s.owner_is_worker_owner = true;
        s.restricted = false;
        let off = Config::parse(&toml("")).unwrap();
        let on = Config::parse(&toml("browser = true\n")).unwrap();
        let plain = on.check_start(&s).unwrap();
        assert!(
            !plain.browser && plain.browser_refused.is_none(),
            "not asked for"
        );
        s.browser = true;
        let given = on.check_start(&s).unwrap();
        assert!(given.browser && given.browser_refused.is_none());
        let refused = off.check_start(&s).unwrap();
        assert!(!refused.browser && refused.browser_refused.unwrap().contains("browser is off"));
        let mut foreign = s.clone();
        foreign.owner_is_worker_owner = false;
        assert!(on.check_start(&foreign).unwrap().browser_refused.is_some());
        let mut restricted = s.clone();
        restricted.restricted = true;
        assert!(!on.check_start(&restricted).unwrap().browser);
    }

    /// SPEC 0.243: a provider's `api_key` is the key, or a Vaultwarden entry (and field) the
    /// session's provider carries with the worker's `rbw`.
    #[test]
    fn a_providers_key_is_a_value_or_a_vault_entry() {
        let toml = |key: &str| {
            format!(
                "hub_url = \"http://127.0.0.1:1/\"\nallowed_roots = [\"/a\"]\n\
                 [providers.zai]\nbase_url = \"https://api.z.ai/api/anthropic\"\n{key}\n"
            )
        };
        let key = |k: &str| Config::parse(&toml(k)).map(|c| c.providers[0].clone());
        // A string, as before.
        let p = key("api_key = \"sk-zai\"").unwrap();
        assert_eq!(p.api_key.unwrap().expose(), "sk-zai");
        assert!(p.api_key_vault.is_none());
        // A table: the entry, and the field when there is one; no key of its own.
        let p = key("api_key = { vault = \"zai key\" }").unwrap();
        assert!(p.api_key.is_none());
        assert_eq!(
            p.api_key_vault,
            Some(VaultRef {
                entry: "zai key".into(),
                field: None
            })
        );
        let p = key("api_key = { vault = \"zai\", field = \"token\" }").unwrap();
        assert_eq!(
            p.api_key_vault,
            Some(VaultRef {
                entry: "zai".into(),
                field: Some("token".into())
            })
        );
        // Neither a stray key in the table, nor a table without an entry, nor a bad entry or
        // field.
        for bad in [
            "api_key = { vault = \"zai\", feild = \"x\" }",
            "api_key = { field = \"x\" }",
            "api_key = { vault = \"\" }",
            "api_key = { vault = \"a\\nb\" }",
            "api_key = { vault = \"zai\", field = \"\" }",
            "api_key = { vault = \"zai\", field = \"a\\nb\" }",
        ] {
            assert!(key(bad).is_err(), "{bad}");
        }
        // A table that is not a vault reference says what is expected.
        let e = format!(
            "{:#}",
            key("api_key = { vault = \"zai\", feild = \"x\" }").unwrap_err()
        );
        assert!(
            e.contains("a key, or a table { vault = \"<entry>\", field = \"<field>\" }"),
            "{e}"
        );
        // The session's provider carries the entry with the worker's `rbw`.
        let mut c = Config::parse(&toml("api_key = { vault = \"zai\" }")).unwrap();
        c.rbw_bin = PathBuf::from("/opt/rbw");
        let p = c
            .provider(&hqc_proto::frame::Provider {
                name: "zai".into(),
                base_url: "https://api.z.ai/api/anthropic".into(),
                model: "glm-5".into(),
                context_tokens: None,
            })
            .unwrap();
        assert!(p.api_key.is_none());
        assert_eq!(
            p.api_key_vault,
            Some(VaultKey {
                entry: "zai".into(),
                field: None,
                rbw: PathBuf::from("/opt/rbw")
            })
        );
    }

    /// SPEC 0.144: a provider runs only by its name in `[providers.*]` with the same
    /// `base_url`; any other description starts the session without it and says why. The
    /// key is never in `hello` or in `Debug`.
    #[test]
    fn providers_are_allowed_by_name_and_base_url() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("ws")).unwrap();
        let c = Config::parse(&format!(
            "hub_url = \"http://127.0.0.1:1/\"\nallowed_roots = [{:?}]\n\
             [providers.qwen-local]\nbase_url = \"http://192.168.1.229:8082/\"\n\
             [providers.zai]\nbase_url = \"https://api.z.ai/api/anthropic\"\napi_key = \"sk-zai-secret\"\n",
            dir.path().display().to_string(),
        ))
        .unwrap();
        let names: Vec<&str> = c.providers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["qwen-local", "zai"]);
        assert_eq!(c.providers[0].base_url, "http://192.168.1.229:8082");
        assert!(!format!("{c:?}").contains("sk-zai-secret"));

        let described = |name: &str, base_url: &str| hqc_proto::frame::Provider {
            name: name.into(),
            base_url: base_url.into(),
            model: "qwen3.8-27b".into(),
            context_tokens: Some(200_000),
        };
        let mut s = start(&ws(&c));
        // A trailing `/` is the same endpoint.
        s.provider = Some(described("qwen-local", "http://192.168.1.229:8082/"));
        let spec = c.check_start(&s).unwrap();
        let p = spec.provider.unwrap();
        assert_eq!(
            (p.name.as_str(), p.model.as_str()),
            ("qwen-local", "qwen3.8-27b")
        );
        assert_eq!(p.context_tokens, Some(200_000));
        assert!(p.api_key.is_none());
        assert!(spec.provider_refused.is_none());
        // The key comes from `worker.toml`, never from the hub.
        s.provider = Some(described("zai", "https://api.z.ai/api/anthropic"));
        let p = c.check_start(&s).unwrap().provider.unwrap();
        assert_eq!(p.api_key.unwrap().expose(), "sk-zai-secret");
        assert!(p.api_key_vault.is_none());

        for (name, url, why) in [
            ("other", "http://192.168.1.229:8082", "does not allow"),
            ("qwen-local", "http://evil.example:8082", "another base_url"),
        ] {
            s.provider = Some(described(name, url));
            let spec = c.check_start(&s).unwrap();
            assert!(spec.provider.is_none(), "{name} {url}");
            assert!(spec.provider_refused.unwrap().contains(why));
        }
        s.provider = Some(hqc_proto::frame::Provider {
            model: "--dangerously-skip-permissions".into(),
            ..described("qwen-local", "http://192.168.1.229:8082")
        });
        assert!(c.check_start(&s).unwrap().provider.is_none());

        for bad in [
            "[providers.\"a b\"]\nbase_url = \"http://x\"\n",
            "[providers.x]\nbase_url = \"ftp://x\"\n",
            "[providers.x]\nbase_url = \"http://x\"\nkey = \"k\"\n",
        ] {
            let text = format!("hub_url = \"https://h\"\nallowed_roots = [\"/a\"]\n{bad}");
            assert!(Config::parse(&text).is_err(), "{bad}");
        }
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
        assert_eq!(c.clone_timeout, DEFAULT_CLONE_TIMEOUT);
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

    /// T38: the mode a session runs in is hqc's choice, else `mode`; `auto` only with
    /// `allow_auto`.
    #[test]
    fn session_permission_mode() {
        use crate::permissions::PermissionMode::{Ask, Auto};
        let base = "hub_url = \"https://h\"\nallowed_roots = [\"/a\"]\n";
        let id = Uuid::nil();
        let c = Config::parse(base).unwrap();
        assert!(c.permissions.allow_auto, "allowed by default");
        assert!(!c.permissions.allow_all, "allow_all is not");
        assert_eq!(c.permission_mode(id, None), Ask);
        assert_eq!(c.permission_mode(id, Some(Auto)), Auto);
        assert_eq!(c.permission_mode(id, Some(Ask)), Ask);

        let c = Config::parse(&format!("{base}[permissions]\nmode = \"auto\"\n")).unwrap();
        assert_eq!(c.permission_mode(id, None), Auto);
        assert_eq!(c.permission_mode(id, Some(Ask)), Ask);

        let c = Config::parse(&format!("{base}[permissions]\nallow_auto = false\n")).unwrap();
        assert_eq!(c.permission_mode(id, Some(Auto)), Ask);
        assert_eq!(c.permission_mode(id, None), Ask);

        let e = Config::parse(&format!(
            "{base}[permissions]\nmode = \"auto\"\nallow_auto = false\n"
        ))
        .unwrap_err();
        assert!(format!("{e:#}").contains("allow_auto"), "{e:#}");
    }

    /// SPEC 0.129: `allow_all` has a setting of its own, off by default and independent of
    /// `allow_auto`: without it a session hqc asks to run in `allow_all` runs in the highest
    /// mode the worker allows (`auto` with `allow_auto`, else `ask`), and `mode = "allow_all"`
    /// is a config error, as `mode = "auto"` is with `allow_auto = false`.
    #[test]
    fn allow_all_permission_mode() {
        use crate::permissions::PermissionMode::{AllowAll, Ask, Auto};
        let base = "hub_url = \"https://h\"\nallowed_roots = [\"/a\"]\n";
        let id = Uuid::nil();
        let c = Config::parse(base).unwrap();
        assert_eq!(c.permission_mode(id, Some(AllowAll)), Auto);

        let c = Config::parse(&format!("{base}[permissions]\nallow_auto = false\n")).unwrap();
        assert_eq!(c.permission_mode(id, Some(AllowAll)), Ask);

        let c = Config::parse(&format!("{base}[permissions]\nallow_all = true\n")).unwrap();
        assert!(c.permissions.allow_all);
        assert_eq!(c.permission_mode(id, Some(AllowAll)), AllowAll);
        assert_eq!(c.permission_mode(id, Some(Auto)), Auto);
        assert_eq!(c.permission_mode(id, None), Ask);

        let c = Config::parse(&format!(
            "{base}[permissions]\nallow_all = true\nallow_auto = false\n"
        ))
        .unwrap();
        assert_eq!(c.permission_mode(id, Some(AllowAll)), AllowAll);
        assert_eq!(c.permission_mode(id, Some(Auto)), Ask);

        let c = Config::parse(&format!(
            "{base}[permissions]\nmode = \"allow_all\"\nallow_all = true\nallow_auto = false\n"
        ))
        .unwrap();
        assert_eq!(c.permissions.mode, AllowAll);
        assert_eq!(c.permission_mode(id, None), AllowAll);
        assert_eq!(c.permission_mode(id, Some(Ask)), Ask);

        for bad in [
            "mode = \"allow_all\"\n",
            "mode = \"allow_all\"\nallow_all = false\n",
            "mode = \"allow_all\"\nallow_auto = true\n",
        ] {
            let e = Config::parse(&format!("{base}[permissions]\n{bad}")).unwrap_err();
            assert!(
                format!("{e:#}").contains("allow_all = true"),
                "{bad}: {e:#}"
            );
        }
        let e = Config::parse(&format!(
            "{base}[permissions]\nmode = \"auto\"\nallow_all = true\nallow_auto = false\n"
        ))
        .unwrap_err();
        assert!(format!("{e:#}").contains("allow_auto = false"), "{e:#}");
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

    /// A worker config with environments `infra` (bound to `<root>/a/b`) and `lab` (bound to
    /// `<root>/a`), and `<root>/a/b/pub` given back to `default`.
    fn envs(root: &Path) -> Config {
        for d in ["a/b/pub", "a/bc", "c"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        Config::parse(&format!(
            r#"hub_url = "https://h"
allowed_roots = [{root:?}]
[environments.infra]
env = {{ GITHUB_TOKEN = "ghp_never_shown" }}
[environments.infra.permissions]
unblock = ["Bash(ssh:*)"]
allow = ["Bash(ssh debian@192.168.1.20:*)"]
deny = ["Bash(git push:*)"]
[environments.lab]
allow_local_mcp = true
[[env_bindings]]
path = "{root}/a/b"
environment = "infra"
[[env_bindings]]
path = "{root}/a"
environment = "lab"
[[env_bindings]]
path = "{root}/a/b/pub"
environment = "default"
"#,
            root = root.display()
        ))
        .unwrap()
    }

    /// T43: the longest binding wins, by components; anything else is `default`.
    #[test]
    fn binding_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let c = envs(&root);
        let env = |p: &str| c.environment(&root.join(p)).name.clone();
        assert_eq!(env("a/b"), "infra");
        assert_eq!(env("a/b/pub"), "default");
        assert_eq!(env("a/bc"), "lab", "/a/b does not contain /a/bc");
        assert_eq!(env("a"), "lab");
        assert_eq!(env("c"), "default");
        let infra = c.environment(&root.join("a/b"));
        assert!(!infra.permissions.deny.contains(&"Bash(ssh:*)".to_string()));
        assert!(infra
            .permissions
            .deny
            .contains(&"Bash(git push:*)".to_string()));
        assert!(c.environment(&root.join("a")).allow_local_mcp);
        assert_eq!(*c.environment(&root.join("c")), *c.default_environment);
        assert!(c.unbound_environments().is_empty());
    }

    /// An environment no binding names is reported: here the permissions table names
    /// another environment than the one with the keys.
    #[test]
    fn unbound_environment() {
        let c = Config::parse(
            r#"
hub_url = "https://h"
allowed_roots = ["/a"]
[environments.infra]
allow_local_mcp = true
[environments.homemac.permissions]
unblock = ["Bash(ssh:*)"]
[[env_bindings]]
path = "/a/infra"
environment = "infra"
"#,
        )
        .unwrap();
        assert_eq!(c.unbound_environments(), vec!["homemac"]);
    }

    /// `project_settings` is per environment and off unless set, `default` included.
    #[test]
    fn project_settings_per_environment() {
        let c = Config::parse(
            r#"
hub_url = "https://h"
allowed_roots = ["/a"]
[environments.infra]
project_settings = true
[environments.lab]
allow_local_mcp = true
[[env_bindings]]
path = "/a/infra"
environment = "infra"
[[env_bindings]]
path = "/a/lab"
environment = "lab"
"#,
        )
        .unwrap();
        let env = |n: &str| c.environments.iter().find(|e| e.name == n).unwrap();
        assert!(env("infra").project_settings);
        assert!(!env("lab").project_settings);
        assert!(!c.default_environment.project_settings);
    }

    /// #162: `remote_unblock` is read per host and reaches the environment's permissions; a
    /// rule that is not one of its deny rules is an error at load.
    #[test]
    fn remote_unblock_in_worker_toml() {
        let toml = |rule: &str| {
            format!(
                "hub_url = \"https://h\"\nallowed_roots = [\"/a\"]\n\
                 [environments.bench.permissions]\nunblock = [\"Bash(ssh:*)\"]\n\
                 remote_unblock = {{ \"pi@bench.local\" = [\"{rule}\"] }}\n\
                 [[env_bindings]]\npath = \"/a/bench\"\nenvironment = \"bench\"\n"
            )
        };
        let c = Config::parse(&toml("Bash(sudo:*)")).unwrap();
        let e = c.environments.iter().find(|e| e.name == "bench").unwrap();
        assert_eq!(
            e.permissions.remote_unblock["pi@bench.local"],
            ["Bash(sudo:*)"]
        );
        assert_eq!(e.rules.remote_unblock["pi@bench.local"], ["Bash(sudo:*)"]);
        let err = Config::parse(&toml("Bash(curl:*)")).unwrap_err();
        assert!(format!("{err:#}").contains("remote_unblock"), "{err:#}");
    }

    /// SPEC 0.235: the hub's MCP servers only in an environment with `hub_mcp`, for an
    /// unrestricted session of the worker's owner, and only valid names.
    #[cfg(unix)]
    #[test]
    fn the_hubs_mcp_servers_only_where_hub_mcp_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for d in ["on", "off"] {
            std::fs::create_dir(root.join(d)).unwrap();
        }
        let c = Config::parse(&format!(
            r#"
hub_url = "https://h"
allowed_roots = [{root:?}]
[environments.mcp]
hub_mcp = true
[[env_bindings]]
path = {on:?}
environment = "mcp"
"#,
            root = root.display().to_string(),
            on = root.join("on").display().to_string(),
        ))
        .unwrap();
        let start = |ws: &str, owner: bool, restricted: bool| {
            let s: hqc_proto::frame::SessionStart = serde_json::from_value(serde_json::json!({
                "session_id": Uuid::now_v7(), "mode": "new", "runtime": "claude_code",
                "workspace_path": root.join(ws).display().to_string(), "cwd": ".",
                "disallowed_tools": [], "restricted": restricted, "auto_approve": false,
                "session_token": "hqcs_x", "owner_is_worker_owner": owner, "ask_timeout_s": 600,
                "mcp_servers": ["github", "wiki", "hqc", "Bad Name"],
            }))
            .unwrap();
            c.check_start(&s).map(|s| s.mcp_servers).unwrap_or_default()
        };
        assert_eq!(start("on", true, false), ["github", "wiki"]);
        assert!(start("off", true, false).is_empty(), "no hub_mcp");
        assert!(start("on", true, true).is_empty(), "restricted");
        assert!(start("on", false, false).is_empty(), "not the owner's");
    }

    /// Iteration 9: `[environments.<env>.mcp_servers.<name>]`, checked at load.
    #[test]
    fn local_mcp_servers_are_parsed_and_checked() {
        let parse = |servers: &str| {
            Config::parse(&format!(
                "hub_url = \"https://h\"\nallowed_roots = [\"/a\"]\n[environments.fw]\n{servers}"
            ))
        };
        let c = parse(
            "[environments.fw.mcp_servers.figma]\ncommand = \"npx\"\nargs = [\"-y\", \"figma-developer-mcp\"]\n\
             env = { FIGMA_API_KEY = { vault = \"figma token\", field = \"token\" }, MODE = \"stdio\" }\n\
             [environments.fw.mcp_servers.figma-dev]\ntype = \"http\"\nurl = \"http://127.0.0.1:3845/mcp\"\n\
             headers = { X-Token = \"t\" }\npin = false\n\
             [environments.fw.mcp_servers.kicad]\ncommand = \"/opt/kicad/mcp\"\ncwd = \"/tmp\"\n",
        )
        .unwrap();
        let e = &c.environments[0];
        let names: Vec<&str> = e.mcp_servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["figma", "figma-dev", "kicad"]);
        let figma = &e.mcp_servers[0];
        assert!(figma.pin);
        let LocalMcpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } = &figma.transport
        else {
            panic!("stdio")
        };
        assert_eq!(command, "npx");
        assert_eq!(args, &["-y", "figma-developer-mcp"]);
        assert!(cwd.is_none());
        assert_eq!(
            env[0],
            (
                "FIGMA_API_KEY".to_string(),
                McpValue::Vault(VaultRef {
                    entry: "figma token".into(),
                    field: Some("token".into())
                })
            )
        );
        assert_eq!(
            env[1],
            ("MODE".to_string(), McpValue::Plain(Secret::new("stdio")))
        );
        assert!(
            !format!("{figma:?}").contains("stdio\")"),
            "a plain value is not printed"
        );
        let dev = &e.mcp_servers[1];
        assert!(!dev.pin);
        assert!(
            matches!(&dev.transport, LocalMcpTransport::Http { url, headers }
            if url == "http://127.0.0.1:3845/mcp" && headers.len() == 1)
        );
        assert!(c.default_environment.mcp_servers.is_empty());

        for (bad, why) in [
            ("[environments.fw.mcp_servers.hqc]\ncommand = \"x\"\n", "not a server name"),
            ("[environments.fw.mcp_servers.Figma]\ncommand = \"x\"\n", "not a server name"),
            ("[environments.fw.mcp_servers.a__b]\ncommand = \"x\"\n", "not a server name"),
            ("[environments.fw.mcp_servers.f]\nargs = []\n", "needs command"),
            ("[environments.fw.mcp_servers.f]\ncommand = \"bin/x\"\n", "absolute path or a program name"),
            ("[environments.fw.mcp_servers.f]\ncommand = \"x\"\ncwd = \"rel\"\n", "not an absolute path"),
            ("[environments.fw.mcp_servers.f]\ncommand = \"x\"\nenv = { HQC_X = \"1\" }\n", "HQC_"),
            ("[environments.fw.mcp_servers.f]\ncommand = \"x\"\nenv = { \"1A\" = \"1\" }\n", "not a variable name"),
            ("[environments.fw.mcp_servers.f]\ncommand = \"x\"\nurl = \"http://x\"\n", "for type = \"http\""),
            ("[environments.fw.mcp_servers.f]\ntype = \"http\"\n", "needs url"),
            ("[environments.fw.mcp_servers.f]\ntype = \"http\"\nurl = \"ftp://x\"\n", "http:// or https://"),
            ("[environments.fw.mcp_servers.f]\ntype = \"http\"\nurl = \"http://u:p@x\"\n", "credentials"),
            ("[environments.fw.mcp_servers.f]\ntype = \"http\"\nurl = \"http://x\"\ncommand = \"x\"\n", "for a stdio server"),
            ("[environments.fw.mcp_servers.f]\ntype = \"http\"\nurl = \"http://x\"\nheaders = { \"a b\" = \"1\" }\n", "not a header name"),
            ("[environments.fw.mcp_servers.f]\ntype = \"http\"\nurl = \"http://x\"\nheaders = { A = \"1\\r\\nB: 2\" }\n", "line break"),
            ("[environments.fw.mcp_servers.f]\ntype = \"sse\"\nurl = \"http://x\"\n", "\"stdio\" or \"http\""),
            ("[environments.fw.mcp_servers.f]\ncommand = \"x\"\nenv = { A = { vault = \"\" } }\n", "vault"),
            ("[environments.fw.mcp_servers.f]\ncommand = \"x\"\nenv = { A = { vualt = \"e\" } }\n", "a value, or a table"),
            ("[environments.fw.mcp_servers.f]\ncommand = \"x\"\nbogus = 1\n", "unknown field"),
        ] {
            let e = format!("{:#}", parse(bad).unwrap_err());
            assert!(e.contains(why), "{bad}: {e}");
            assert!(e.contains("mcp_servers.") || e.contains("unknown field") || e.contains("a value"), "{e}");
        }
    }

    /// Iteration 9: the machine's servers on the same terms as the hub's, and one of them wins
    /// over the hub's server of the same name.
    #[cfg(unix)]
    #[test]
    fn local_mcp_servers_for_the_owners_unrestricted_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for d in ["fw", "other"] {
            std::fs::create_dir(root.join(d)).unwrap();
        }
        let c = Config::parse(&format!(
            r#"
hub_url = "https://h"
allowed_roots = [{root:?}]
allow_foreign_sessions = true
[environments.fw]
hub_mcp = true
[environments.fw.mcp_servers.figma]
command = "npx"
[environments.fw.mcp_servers.github]
type = "http"
url = "http://127.0.0.1:1/mcp"
[[env_bindings]]
path = {fw:?}
environment = "fw"
"#,
            root = root.display().to_string(),
            fw = root.join("fw").display().to_string(),
        ))
        .unwrap();
        let start = |ws: &str, owner: bool, restricted: bool| {
            let s: hqc_proto::frame::SessionStart = serde_json::from_value(serde_json::json!({
                "session_id": Uuid::now_v7(), "mode": "new", "runtime": "claude_code",
                "workspace_path": root.join(ws).display().to_string(), "cwd": ".",
                "disallowed_tools": [], "restricted": restricted, "auto_approve": false,
                "session_token": "hqcs_x", "owner_is_worker_owner": owner, "ask_timeout_s": 600,
                "mcp_servers": ["github", "wiki"],
            }))
            .unwrap();
            c.check_start(&s).unwrap()
        };
        let s = start("fw", true, false);
        let local = s.local_mcp.expect("local servers");
        assert_eq!(local.environment, "fw");
        assert_eq!(local.names(), ["figma", "github"]);
        assert_eq!(local.pins, c.mcp_pins);
        assert_eq!(s.mcp_servers, ["wiki"], "the machine's github wins");
        assert!(
            start("other", true, false).local_mcp.is_none(),
            "default has none"
        );
        assert!(start("fw", true, true).local_mcp.is_none(), "restricted");
        assert!(
            start("fw", false, false).local_mcp.is_none(),
            "not the owner's"
        );
    }

    /// SPEC 0.245: the hub's skills only in an environment with `hub_skills`, for an
    /// unrestricted session of the worker's owner, valid names and hashes, one per name.
    #[cfg(unix)]
    #[test]
    fn the_hubs_skills_only_where_hub_skills_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for d in ["on", "off"] {
            std::fs::create_dir(root.join(d)).unwrap();
        }
        let c = Config::parse(&format!(
            r#"
hub_url = "https://h"
allowed_roots = [{root:?}]
[environments.sk]
hub_skills = true
[[env_bindings]]
path = {on:?}
environment = "sk"
"#,
            root = root.display().to_string(),
            on = root.join("on").display().to_string(),
        ))
        .unwrap();
        let (a, b) = ("a".repeat(64), "b".repeat(64));
        let start = |ws: &str, owner: bool, restricted: bool| {
            let s: hqc_proto::frame::SessionStart = serde_json::from_value(serde_json::json!({
                "session_id": Uuid::now_v7(), "mode": "new", "runtime": "claude_code",
                "workspace_path": root.join(ws).display().to_string(), "cwd": ".",
                "disallowed_tools": [], "restricted": restricted, "auto_approve": false,
                "session_token": "hqcs_x", "owner_is_worker_owner": owner, "ask_timeout_s": 600,
                "skills": [
                    {"name": "wiki", "sha256": b}, {"name": "bench", "sha256": a},
                    {"name": "bench", "sha256": b}, {"name": "Bad", "sha256": a},
                    {"name": "x", "sha256": "../.."},
                ],
            }))
            .unwrap();
            c.check_start(&s)
                .map(|s| s.skills.into_iter().map(|k| k.name).collect::<Vec<_>>())
                .unwrap_or_default()
        };
        assert_eq!(start("on", true, false), ["bench", "wiki"]);
        assert!(start("off", true, false).is_empty(), "no hub_skills");
        assert!(start("on", true, true).is_empty(), "restricted");
        assert!(start("on", false, false).is_empty(), "not the owner's");
    }

    /// T43: a session.start resolves the environment from the canonical workspace path,
    /// whatever the hub's path looked like.
    #[cfg(unix)]
    #[test]
    fn binding_through_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("real/infra")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        // The binding names the folder through a link, the session comes by the real path…
        let c = Config::parse(&format!(
            "hub_url = \"https://h\"\nallowed_roots = [{root:?}]\n[environments.infra]\n\
             [[env_bindings]]\npath = \"{}\"\nenvironment = \"infra\"\n",
            root.join("link/infra").display()
        ))
        .unwrap();
        assert_eq!(
            c.env_bindings[0].path,
            root.join("real/infra").display().to_string()
        );
        let spec = c.check_start(&start(&root.join("real/infra"))).unwrap();
        assert_eq!(spec.environment.name, "infra");
        // …or the other way round: the hub's path is resolved first.
        let spec = c.check_start(&start(&root.join("link/infra"))).unwrap();
        assert_eq!(spec.environment.name, "infra");
        // A link into a bound folder from outside it is that folder.
        std::os::unix::fs::symlink(root.join("real/infra"), root.join("alias")).unwrap();
        let spec = c.check_start(&start(&root.join("alias"))).unwrap();
        assert_eq!(spec.environment.name, "infra");
        // A binding whose path appears only after the start, through a link, still applies.
        let c = Config::parse(&format!(
            "hub_url = \"https://h\"\nallowed_roots = [{root:?}]\n[environments.infra]\n\
             [[env_bindings]]\npath = \"{}\"\nenvironment = \"infra\"\n",
            root.join("later").display()
        ))
        .unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("later")).unwrap();
        assert_eq!(c.environment(&root.join("real")).name, "infra");
        assert_eq!(c.environment(&root.join("other")).name, "default");
    }

    #[test]
    fn environment_config_errors() {
        let base = "hub_url = \"https://h\"\nallowed_roots = [\"/a\"]\n";
        for (bad, why) in [
            (
                "[environments.infra.permissions]\nunblock = [\"Bash(curl:*)\"]\n",
                "unblock",
            ),
            (
                "[permissions]\ndeny = []\n[environments.infra.permissions]\nunblock = [\"Bash(ssh:*)\"]\n",
                "unblock",
            ),
            ("[environments.infra.permissions]\nallow = [\"Bash(\"]\n", "allow"),
            ("[environments.default]\n", "default"),
            ("[environments.\"a b\"]\n", "name"),
            ("[environments.infra]\nenv = { HQC_HOME = \"/x\" }\n", "HQC_"),
            ("[environments.infra]\nenv = { \"A-B\" = \"x\" }\n", "variable"),
            ("[environments.infra]\nenv = { SSH_AUTH_SOCK = \"/x\" }\n", "ssh_keys only"),
            ("[environments.infra]\nssh_keys = [\"../id\"]\n", "key name"),
            ("[environments.infra]\nlaunch = \"process\"\n", "unknown field"),
            ("[environments.infra]\nsettings = {}\n", "unknown field"),
            ("[[env_bindings]]\npath = \"/a/x\"\nenvironment = \"infra\"\n", "no [environments.infra]"),
            ("[[env_bindings]]\npath = \"x\"\nenvironment = \"default\"\n", "absolute"),
            (
                "[environments.infra]\n[[env_bindings]]\npath = \"/a/x\"\nenvironment = \"infra\"\n\
                 [[env_bindings]]\npath = \"/a/x/\"\nenvironment = \"default\"\n",
                "twice",
            ),
        ] {
            let e = Config::parse(&format!("{base}{bad}")).unwrap_err();
            assert!(format!("{e:#}").contains(why), "{bad}: {e:#}");
        }
        // The unblock rule must be in the worker's deny: the default deny has ssh.
        let ok = "[environments.infra.permissions]\nunblock = [\"Bash(ssh:*)\"]\n";
        Config::parse(&format!("{base}{ok}")).unwrap();
    }

    /// T44: a key named in `ssh_keys` must be in the store when the config is loaded.
    #[test]
    fn ssh_keys_must_be_in_the_store() {
        let home = tempfile::tempdir().unwrap();
        let mut c = Config::parse(
            "hub_url = \"https://h\"\nallowed_roots = [\"/a\"]\n\
             [environments.infra]\nssh_keys = [\"infra_ed25519\"]\n",
        )
        .unwrap();
        assert_eq!(c.environments[0].ssh_keys, ["infra_ed25519"]);
        c.ssh = crate::ssh::Store::in_home(home.path());
        let e = c.check_ssh().unwrap_err();
        assert!(
            format!("{e:#}").contains("\"infra_ed25519\" is not in the key store"),
            "{e:#}"
        );
        let key = home.path().join("k");
        std::fs::write(&key, "key").unwrap();
        c.ssh.add_key("infra_ed25519", &key).unwrap();
        c.check_ssh().unwrap();
    }

    /// T43: `hello` carries names, bound paths and rules, never the values of `env`; neither
    /// does a `StartSpec` printed in a log.
    #[test]
    fn env_values_stay_on_the_worker() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let c = envs(&root);
        let infos: Vec<_> = c.environments.iter().map(|e| e.info()).collect();
        let hello = serde_json::to_string(&(infos, &c.env_bindings)).unwrap();
        assert!(
            hello.contains("\"infra\"") && hello.contains("Bash(ssh:*)"),
            "{hello}"
        );
        assert!(
            hello.contains(&format!("{}/a/b", root.display())),
            "{hello}"
        );
        assert!(!hello.contains("ghp_never_shown"), "{hello}");
        assert!(!hello.contains("GITHUB_TOKEN"), "{hello}");
        let spec = c.check_start(&start(&root.join("a/b"))).unwrap();
        assert_eq!(spec.environment.name, "infra");
        let printed = format!("{spec:?} {c:?}");
        assert!(printed.contains("GITHUB_TOKEN"), "names are fine");
        assert!(!printed.contains("ghp_never_shown"), "{printed}");
    }
}
