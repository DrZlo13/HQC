//! Claude Code adapter (SPEC §8.1, docs/ITERATION-1.md T4): one `claude -p` process per
//! session in stream-json mode, mapped to SPEC §4 events by [`engine::Core`].
//!
//! - `start`/`resume` spawn the CLI in `<workspace>/<cwd>`. The flag follows the transcript,
//!   not the call: `--resume <uuid>` if `<config>/projects/<encoded cwd>/<uuid>.jsonl`
//!   exists (the worker restarted, or the process ran before), else `--session-id <uuid>`
//!   (a process that never got a prompt left no transcript to resume).
//! - Every process gets `control_request {subtype: initialize}` first
//!   ([`engine::Core::initialize`]).
//! - `stop` sends `control_request {subtype: interrupt}`; if no `result` follows within
//!   [`ClaudeOptions::stop_grace`], the process group gets SIGINT; if the turn still runs
//!   another `stop_grace` later, SIGTERM, and SIGKILL 5 s after that, so a process stuck in a
//!   tool call is stopped with what it started.
//! - `kill` sends SIGTERM to the process group, SIGKILL after 5 s.
//! - A process that exits by itself is reported as [`AdapterEvent::Exited`].
//! - While it runs, its token rate is counted every 2 s ([`rate`]).

pub mod engine;
pub mod paths;
pub mod process;
pub mod rate;
pub mod rules;
pub mod scripted;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Claude Code's built-in cross-session tools: they find other local Claude sessions and
/// inject messages into them directly, so the hub never sees that input (SPEC §7 routes
/// agent-to-agent traffic through `hqc_send`/`hqc_ask` instead).
pub const NATIVE_MESSAGING_TOOLS: &[&str] = &["ListAgents", "SendMessage"];

/// `system.code` of a session that had a conversation but whose transcript is gone, so the
/// agent starts without it (SPEC §4, §8.1).
pub const TRANSCRIPT_MISSING: &str = "transcript_missing";
pub const TRANSCRIPT_MISSING_TEXT: &str = "The Claude Code transcript of this session is gone \
    (Claude Code deletes transcripts older than its cleanupPeriodDays setting, 30 days by \
    default): the agent continues without the earlier conversation";

/// `system.code` of a session whose provider this worker does not allow (SPEC §4, §8.1).
pub const PROVIDER_REFUSED: &str = "provider_refused";
/// `system.code` of a session that asked for the browser this worker does not give (SPEC 0.148).
pub const BROWSER_REFUSED: &str = "browser_refused";
/// The browser tools a session with the browser never gets (SPEC 0.148): they send files of
/// the machine to a web page.
pub const BROWSER_DISALLOWED: &[&str] = &[
    "mcp__claude-in-chrome__file_upload",
    "mcp__claude-in-chrome__upload_image",
];

use anyhow::Context;
use async_trait::async_trait;
use hqc_proto::event::{Author, ClarifyAnswer, EventKind};
use hqc_proto::frame::{ForkFrom, Runtime, SessionInput};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

use self::engine::{Action, Core};
use self::process::{CommandSpec, ExitInfo, OsSpawner, Sig, SignalFn, Spawner};
use super::{Adapter, AdapterEvent, AdapterFactory, EventSink};
use crate::config::{Config, StartSpec};
use crate::inbox::InboxFile;
use crate::mirror::OwnRecords;
use crate::permissions::{PermissionMode, Policy};

/// A live process of transcript `uuid` in [`OwnRecords`]; ends with the monitor, even an
/// aborted one.
struct Live(Arc<OwnRecords>, Uuid, Option<u32>);

impl Live {
    fn new(own: Arc<OwnRecords>, uuid: Uuid, pid: Option<u32>) -> Self {
        own.begin(uuid, pid);
        Self(own, uuid, pid)
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.0.end(self.1, self.2);
    }
}

/// The transcript folder of a process that cleared its conversation, marked in [`OwnRecords`]
/// until the process names the new transcript or ends.
struct Clearing(Arc<OwnRecords>, PathBuf);

impl Clearing {
    fn new(own: Arc<OwnRecords>, folder: PathBuf) -> Self {
        own.clearing(&folder);
        Self(own, folder)
    }
}

impl Drop for Clearing {
    fn drop(&mut self) {
        self.0.cleared(&self.1);
    }
}

const KILL_GRACE: Duration = Duration::from_secs(5);
/// How long the output of an exited process is still read when its stdout stays open
/// (held by a grandchild).
const EXIT_DRAIN: Duration = Duration::from_secs(2);
const STDERR_TAIL: usize = 2048;

/// Settings files the CLI reads: the user's own and `.claude/settings.local.json` (where
/// `always` writes, SPEC §8.1), not the repository's `.claude/settings.json` or `CLAUDE.md`;
/// in an environment with `project_settings` (SPEC §14) those too,
/// [`PROJECT_SETTING_SOURCES`].
pub const SETTING_SOURCES: &str = "user,local";
pub const PROJECT_SETTING_SOURCES: &str = "user,project,local";

/// Credentials of the worker's own environment that a session with a provider does not get:
/// the CLI would send them to the provider's endpoint.
const PROVIDER_DROPS: &[&str] = &["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"];

/// The CLI's variables for a provider (SPEC §8.1, since 0.144), on top of the environment's:
/// the endpoint, its key (a placeholder when it needs none: the CLI then sends no OAuth token
/// of the subscription), and every model name the CLI uses — the main model, the aliases, the
/// small fast model, subagents — on the provider's one model, so that a server which swaps
/// models on one GPU is never asked for another.
pub fn provider_env(p: &crate::config::SessionProvider) -> Vec<(String, String)> {
    let key = p.api_key.as_ref().map_or("hqc", |k| k.expose()).to_string();
    let mut env = vec![
        ("ANTHROPIC_BASE_URL".to_string(), p.base_url.clone()),
        ("ANTHROPIC_AUTH_TOKEN".to_string(), key),
        (
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(),
            "1".to_string(),
        ),
    ];
    for k in [
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_DEFAULT_FABLE_MODEL",
        "ANTHROPIC_SMALL_FAST_MODEL",
        "CLAUDE_CODE_SUBAGENT_MODEL",
    ] {
        env.push((k.to_string(), p.model.clone()));
    }
    if let Some(n) = p.context_tokens {
        env.push(("CLAUDE_CODE_MAX_CONTEXT_TOKENS".to_string(), n.to_string()));
    }
    env
}

/// The agent's environment: only these variables of the worker's (SPEC §8.1), never `HQC_*`
/// (the worker's token store, config) or other secrets the worker runs with. `SHELL` and
/// `LOGNAME` are what the CLI's Bash tool and the Keychain lookup need. Not the user's
/// `SSH_AUTH_SOCK` either (T44): a session gets the socket of its own ssh-agent, and only in
/// an environment with `ssh_keys`. The `env` of the session's environment comes on top (T43,
/// [`ClaudeCodeAdapter::command`]).
pub fn child_env(vars: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
    const NAMES: &[&str] = &[
        "PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "TERM", "TMPDIR",
    ];
    const PREFIXES: &[&str] = &["LC_", "CLAUDE_", "ANTHROPIC_"];
    let mut env: Vec<(String, String)> = vars
        .filter(|(k, _)| {
            !k.starts_with("HQC_")
                && (NAMES.contains(&k.as_str()) || PREFIXES.iter().any(|p| k.starts_with(p)))
        })
        .collect();
    env.sort();
    env
}

/// What a session keeps of Claude Code's own cross-session messaging (SPEC §8.1): one value,
/// so `--disallowedTools` and `crossSessionInbound` cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeMessaging {
    /// The default: [`NATIVE_MESSAGING_TOOLS`] disallowed.
    Off,
    /// `allow_subagent_messages`: only `ListAgents` disallowed, so an agent continues its own
    /// subagents with `SendMessage` (a restricted session keeps it disallowed too), and
    /// `"crossSessionInbound": "refuse"` in `--settings`.
    Subagents,
    /// `allow_native_messaging`: nothing disallowed, no `crossSessionInbound`.
    On,
}

impl NativeMessaging {
    pub fn from_config(cfg: &Config) -> Self {
        if cfg.allow_native_messaging {
            Self::On
        } else if cfg.allow_subagent_messages {
            Self::Subagents
        } else {
            Self::Off
        }
    }

    /// What goes into `--disallowedTools` after the session's own rules.
    fn disallowed(self, restricted: bool) -> &'static [&'static str] {
        match self {
            Self::Subagents if !restricted => &["ListAgents"],
            Self::Off | Self::Subagents => NATIVE_MESSAGING_TOOLS,
            Self::On => &[],
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClaudeOptions {
    /// `claude_bin` from the worker config; `claude` from `PATH` if unset.
    pub claude_bin: Option<PathBuf>,
    /// Claude Code's config dir (`$CLAUDE_CONFIG_DIR` or `~/.claude`), for transcripts.
    pub config_dir: Option<PathBuf>,
    /// The hqc MCP server the CLI starts for each session (SPEC §7.2); `None`: no
    /// `--mcp-config`, the agent has no hqc tools (tests).
    pub mcp: Option<McpLaunch>,
    /// `CLAUDE_CODE_ENTRYPOINT` for the child: `claude-vscode` makes the transcript show up in
    /// the VS Code extension's history; `None` leaves the CLI's own `sdk-cli`.
    pub entrypoint: Option<String>,
    /// Appended to the command line (tests: `--max-turns`).
    pub extra_args: Vec<String>,
    /// The CLI's "ask" mode, `default` or `manual` by version
    /// ([`crate::capabilities::claude_permission_mode`]). `--permission-mode` is always
    /// passed, so that a `defaultMode` from settings (`bypassPermissions`, `acceptEdits`)
    /// does not apply: this, `auto` for a session in auto or `bypassPermissions` for one in
    /// allow_all (T38).
    pub ask_mode: String,
    /// Claude Code's own cross-session messaging, on top of `session.start.disallowed_tools`
    /// (`allow_native_messaging` and `allow_subagent_messages` in the worker config).
    pub native_messaging: NativeMessaging,
    /// Interrupt → SIGINT fallback delay, and SIGINT → SIGTERM.
    pub stop_grace: Duration,
    /// A process that dies this soon after spawning fails `start` instead of being
    /// reported as `Exited` (bad flags, unknown `--resume` id, not logged in).
    pub early_exit: Duration,
    /// Shared with the mirror: which transcripts a process of this worker writes now and
    /// which records it wrote (SPEC §9.3).
    pub own_records: Arc<OwnRecords>,
    /// The key store and the agents' sockets, for a session whose environment has
    /// `ssh_keys` (T44).
    pub ssh: crate::ssh::Store,
}

impl Default for ClaudeOptions {
    fn default() -> Self {
        Self {
            entrypoint: None,
            claude_bin: None,
            config_dir: paths::default_config_dir(),
            mcp: None,
            extra_args: vec![],
            ask_mode: "default".into(),
            native_messaging: NativeMessaging::Off,
            stop_grace: Duration::from_secs(10),
            early_exit: Duration::from_millis(500),
            own_records: Arc::default(),
            ssh: crate::ssh::Store::in_home(
                &crate::config::hqc_home().unwrap_or_else(|_| std::env::temp_dir().join("hqc")),
            ),
        }
    }
}

/// How the CLI starts the hqc MCP server (SPEC §7.2): `<program> mcp --socket <socket>`.
#[derive(Debug, Clone, PartialEq)]
pub struct McpLaunch {
    /// The worker's own binary.
    pub program: PathBuf,
    /// The daemon's socket ([`crate::mcp::socket_path`]).
    pub socket: PathBuf,
}

impl McpLaunch {
    /// The `--mcp-config` JSON: the server `hqc` with the session token in its own `env`.
    /// The token is thereby on the CLI's command line, which on macOS other users cannot
    /// read (T30, SPEC §7), and not in the CLI's environment, which the agent's Bash tool
    /// inherits. `secret_tools`: the server lists `hqc_secret` and `hqc_ssh_keygen` too
    /// (a session with [`crate::config::SessionSecrets`], SPEC 0.147).
    pub fn config(&self, token: &crate::token::Secret, secret_tools: bool) -> String {
        let mut env = serde_json::json!({ (crate::mcp::TOKEN_ENV): token.expose() });
        if secret_tools {
            env[crate::secrets::SECRET_TOOLS_ENV] = "1".into();
        }
        serde_json::json!({
            "mcpServers": {
                "hqc": {
                    "type": "stdio",
                    "command": self.program,
                    "args": ["mcp", "--socket", self.socket],
                    "env": env,
                }
            }
        })
        .to_string()
    }
}

/// `MCP_TOOL_TIMEOUT` for a session (SPEC §7.1): a minute more than `ask_timeout_s`, in ms.
pub fn mcp_tool_timeout_ms(ask_timeout_s: u32) -> u64 {
    (u64::from(ask_timeout_s) + 60) * 1000
}

/// Factory for [`ClaudeCodeAdapter`]s.
#[derive(Clone)]
pub struct ClaudeCodeAdapters {
    opts: Arc<ClaudeOptions>,
    spawner: Arc<dyn Spawner>,
}

impl ClaudeCodeAdapters {
    pub fn new(opts: ClaudeOptions, spawner: Arc<dyn Spawner>) -> Self {
        Self {
            opts: Arc::new(opts),
            spawner,
        }
    }

    /// Give every session the hqc MCP server (SPEC §7.2).
    pub fn with_mcp(mut self, mcp: McpLaunch) -> Self {
        Arc::make_mut(&mut self.opts).mcp = Some(mcp);
        self
    }

    /// The registry of own transcript records, for the mirror.
    pub fn own_records(&self) -> Arc<OwnRecords> {
        self.opts.own_records.clone()
    }

    /// `ask_mode`: the CLI's "ask" mode ([`ClaudeOptions::ask_mode`]).
    pub fn from_config(cfg: &Config, ask_mode: String) -> Self {
        Self::new(
            ClaudeOptions {
                claude_bin: cfg.claude_bin.clone(),
                entrypoint: cfg.show_in_vscode.then(|| "claude-vscode".to_string()),
                ask_mode,
                ssh: cfg.ssh.clone(),
                native_messaging: NativeMessaging::from_config(cfg),
                ..ClaudeOptions::default()
            },
            Arc::new(OsSpawner),
        )
    }
}

impl AdapterFactory for ClaudeCodeAdapters {
    fn supports(&self, runtime: Runtime) -> bool {
        runtime == Runtime::ClaudeCode
    }

    fn create(&self, spec: &StartSpec, events: EventSink) -> Box<dyn Adapter> {
        Box::new(ClaudeCodeAdapter {
            session_id: spec.session_id,
            opts: self.opts.clone(),
            spawner: self.spawner.clone(),
            events,
            proc: None,
        })
    }
}

pub struct ClaudeCodeAdapter {
    session_id: Uuid,
    opts: Arc<ClaudeOptions>,
    spawner: Arc<dyn Spawner>,
    events: EventSink,
    proc: Option<Proc>,
}

/// One running CLI process.
struct Proc {
    core: Arc<Mutex<Core>>,
    io: Arc<Io>,
    killing: Arc<AtomicBool>,
    exited: watch::Receiver<Option<ExitInfo>>,
    /// Set once `start` has decided; until then the monitor does not report an exit.
    started: watch::Sender<bool>,
    monitor: JoinHandle<()>,
    /// The CLI's process (SPEC §7.2: the hqc MCP server must descend from it).
    pid: Option<u32>,
    /// Its secrets folder and ssh-agent (SPEC 0.147), held by the monitor until it exits.
    secrets: Option<std::sync::Weak<crate::secrets::ProcessSecrets>>,
}

struct Io {
    session_id: Uuid,
    stdin: tokio::sync::Mutex<Pin<Box<dyn AsyncWrite + Send>>>,
    events: EventSink,
    signal: SignalFn,
    /// The session's transcript, where the CLI writes `ai-title` (SPEC §8.1); another one
    /// after `/clear` ([`Io::moved_to`]).
    transcript: Mutex<Option<PathBuf>>,
    /// The last title reported by this process.
    title: Mutex<Option<String>>,
    /// The transcript's uuid and the registry its records go to (SPEC §9.3).
    runtime: Mutex<Uuid>,
    own: Arc<OwnRecords>,
}

impl Io {
    async fn run(&self, actions: Vec<Action>) -> anyhow::Result<()> {
        for a in actions {
            match a {
                Action::Emit(ev @ AdapterEvent::TurnEnded { .. }) => {
                    self.report_title().await;
                    let _ = self.events.send(ev).await;
                }
                // A person's title (`/rename`): the transcript gives the same one later.
                Action::Emit(AdapterEvent::Title(t)) => self.title_changed(t).await,
                Action::Emit(ev) => {
                    // The session task is gone only when the session is being torn down.
                    let _ = self.events.send(ev).await;
                }
                Action::Write(v) => {
                    let mut line = serde_json::to_vec(&v)?;
                    line.push(b'\n');
                    let mut w = self.stdin.lock().await;
                    w.write_all(&line)
                        .await
                        .context("writing to claude stdin")?;
                    w.flush().await.context("writing to claude stdin")?;
                }
            }
        }
        Ok(())
    }

    /// After a turn, and during one until the process has a title ([`rate::meter`]): the
    /// transcript's title (its last `custom-title`, else its last `ai-title`), if there is one
    /// and it is new. The CLI writes `ai-title` after the first prompt; a missing file or title
    /// is not an error.
    async fn report_title(&self) {
        let Some(path) = self.transcript() else {
            return;
        };
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(e) => {
                tracing::debug!(session_id = %self.session_id, "no transcript for ai-title: {e}");
                return;
            }
        };
        let Some(title) = hqc_claude_jsonl::last_title(&text) else {
            return;
        };
        self.title_changed(hqc_proto::replace_nul(&title).into_owned())
            .await;
    }

    /// [`AdapterEvent::Title`] unless `title` is the one last reported.
    async fn title_changed(&self, title: String) {
        {
            let mut last = lock(&self.title);
            if last.as_deref() == Some(title.as_str()) {
                return;
            }
            *last = Some(title.clone());
        }
        let _ = self.events.send(AdapterEvent::Title(title)).await;
    }

    fn transcript(&self) -> Option<PathBuf> {
        lock(&self.transcript).clone()
    }

    fn runtime(&self) -> Uuid {
        *lock(&self.runtime)
    }

    /// After `/clear` the CLI writes transcript `new` in the same folder (SPEC §8.1).
    fn moved_to(&self, new: Uuid) {
        *lock(&self.runtime) = new;
        if let Some(p) = lock(&self.transcript).as_mut() {
            p.set_file_name(format!("{new}.jsonl"));
        }
    }

    fn signal(&self, s: Sig) {
        if let Err(e) = (self.signal)(s) {
            tracing::warn!(session_id = %self.session_id, ?s, "signalling claude failed: {e}");
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// How the CLI is launched for a transcript (SPEC §8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// `--session-id`: a new conversation.
    New,
    /// `--resume`: the transcript exists.
    Resume,
    /// `--resume <fork_copy_path(uuid)> --fork-session --session-id <uuid> --resume-session-at
    /// <at>`: a new transcript with the source's records up to `at`.
    Fork { at: Uuid },
}

/// The copy of a fork's source transcript, cut right after the record the fork starts at
/// (SPEC §8.1). On `--resume` the CLI reads a transcript from its newest record back to the
/// last compaction, so a record before a later compaction of the source is not found (`No
/// message found with message.uuid`); in the copy that record is the newest. The name ends in
/// `.jsonl`, which makes `--resume` read it as a file; it is outside the Claude Code config
/// folder, so the mirror does not take it for a session.
fn fork_copy_path(uuid: &Uuid) -> PathBuf {
    std::env::temp_dir().join(format!("hqc-fork-{uuid}.jsonl"))
}

/// The end, past its newline, of the line of record `uuid` in transcript `text`.
fn line_end(text: &str, uuid: &Uuid) -> Option<usize> {
    let id = uuid.to_string();
    let mut end = 0;
    for line in text.split_inclusive('\n') {
        end += line.len();
        if line.contains(&id)
            && serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .is_some_and(|r| r["uuid"] == id.as_str())
        {
            return Some(end);
        }
    }
    None
}

/// A file removed when this is dropped: the fork copy, which lives as long as the process that
/// read it.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Where a fork cuts transcript `text`: the record of the first of `at` (event ids, SPEC §3)
/// whose content block it holds, and the main-chain user/assistant records up to it, which the
/// fork copies; `None` when no event is in it.
pub fn fork_point(text: &str, at: &[Uuid]) -> Option<(Uuid, Vec<Uuid>)> {
    let (records, _) = hqc_claude_jsonl::parse_str(text);
    let mut main = Vec::new();
    let mut found = std::collections::HashMap::new();
    for r in &records {
        let (hqc_claude_jsonl::Record::User(m) | hqc_claude_jsonl::Record::Assistant(m)) = r else {
            continue;
        };
        if !m.is_main() {
            continue;
        }
        for i in 0..m.blocks().len().max(1) {
            found
                .entry(engine::record_event_id(&m.uuid, i))
                .or_insert(main.len());
        }
        main.push(m.uuid);
    }
    let i = at.iter().find_map(|e| found.get(e).copied())?;
    Some((main[i], main[..=i].to_vec()))
}

impl ClaudeCodeAdapter {
    /// The claude session uuid: `runtime_session_id` from the hub, or the hqc session id
    /// when the hub sent none.
    fn runtime_uuid(&self, spec: &StartSpec) -> anyhow::Result<(Uuid, bool)> {
        match &spec.runtime_session_id {
            Some(s) => Ok((
                s.parse()
                    .with_context(|| format!("runtime_session_id {s:?} is not a uuid"))?,
                false,
            )),
            None => Ok((spec.session_id, true)),
        }
    }

    /// The CLI command line (SPEC §8.1). Besides the protocol flags:
    /// - `--thinking-display summarized`, as the VS Code extension passes it: without it the
    ///   thinking blocks of newer models (opus 5.5 on 2.1.312) come with empty text;
    /// - `--setting-sources user,local` and `--permission-mode` (the ask mode, `auto` or
    ///   `bypassPermissions`, T38; with `[permissions] allow_all` and without `restricted`
    ///   also `--allow-dangerously-skip-permissions`, so that a switch to `allow_all` takes): a
    ///   repository's own `.claude/settings.json` (hooks, `defaultMode`) is not applied;
    ///   `local` stays for the rules `always` writes; `--setting-sources user,project,local`
    ///   when the environment has `project_settings` (the owner trusts every repository in
    ///   its folders, SPEC §14);
    /// - `restricted` → `--restricted` (no command-running tools, file tools confined to the
    ///   working directory, no settings files; SPEC §8.4);
    /// - the allow/deny rules of the session's environment (`[permissions]` merged with the
    ///   environment's, T43) → `--settings`, in restricted mode too (the CLI applies
    ///   `--settings` there, checked on 2.1.308);
    /// - [`NATIVE_MESSAGING_TOOLS`] → `--disallowedTools`, by [`NativeMessaging`]: none with
    ///   `allow_native_messaging`; with `allow_subagent_messages` only `ListAgents` (both in
    ///   restricted mode) and `"crossSessionInbound": "refuse"` in `--settings`;
    /// - `--strict-mcp-config` unless the environment has `allow_local_mcp` (the hqc server
    ///   is in `--mcp-config` either way); [`crate::config::Environment::process_env`];
    ///   `SSH_AUTH_SOCK` only as `ssh_agent`, the socket of the process's own ssh-agent (T44);
    ///   `HQC_SECRETS` only as `secrets`, the folder of the process's secrets (SPEC 0.147);
    /// - `disallowed_tools` → `--disallowedTools`, `system_prompt` →
    ///   `--append-system-prompt`. Nothing of `session.start` is silently dropped.
    pub fn command(
        &self,
        spec: &StartSpec,
        uuid: &Uuid,
        launch: Launch,
        ssh_agent: Option<&std::path::Path>,
        secrets: Option<&std::path::Path>,
    ) -> CommandSpec {
        let mut args: Vec<String> = [
            "-p",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--include-partial-messages",
            "--replay-user-messages",
            "--permission-prompts",
            "host",
            "--permission-prompt-tool",
            "stdio",
            "--thinking-display",
            "summarized",
        ]
        .map(String::from)
        .into();
        if !spec.environment.allow_local_mcp {
            args.push("--strict-mcp-config".into());
        }
        if let Some(m) = &self.opts.mcp {
            args.extend([
                "--mcp-config".into(),
                m.config(&spec.session_token, secrets.is_some()),
            ]);
        }
        args.extend(["--allowedTools".into(), "mcp__hqc".into()]);
        // SPEC 0.148: Chrome only for a session its owner gave it; never by the CLI's own
        // default (`/chrome`'s "enabled by default").
        args.push(
            if spec.browser {
                "--chrome"
            } else {
                "--no-chrome"
            }
            .into(),
        );
        let mut disallowed: Vec<String> = spec.disallowed_tools.clone();
        if spec.browser {
            for t in BROWSER_DISALLOWED {
                if !disallowed.iter().any(|d| d == t) {
                    disallowed.push(t.to_string());
                }
            }
        }
        for t in self.opts.native_messaging.disallowed(spec.restricted) {
            if !disallowed.iter().any(|d| d == t) {
                disallowed.push(t.to_string());
            }
        }
        if !disallowed.is_empty() {
            // Variadic: the entries run up to the next flag.
            args.push("--disallowedTools".into());
            args.extend(disallowed);
        }
        let mut settings = spec.environment.permissions.settings();
        if self.opts.native_messaging == NativeMessaging::Subagents {
            settings["crossSessionInbound"] = "refuse".into();
        }
        args.extend(["--settings".into(), settings.to_string()]);
        if spec.restricted {
            args.push("--restricted".into());
        } else {
            let sources = if spec.environment.project_settings {
                PROJECT_SETTING_SOURCES
            } else {
                SETTING_SOURCES
            };
            args.extend(["--setting-sources".into(), sources.into()]);
        }
        args.extend([
            "--permission-mode".into(),
            match spec.permission_mode {
                PermissionMode::Ask => self.opts.ask_mode.clone(),
                PermissionMode::Auto => "auto".into(),
                PermissionMode::AllowAll => "bypassPermissions".into(),
            },
        ]);
        // Lets a running process be switched to `bypassPermissions` (the CLI refuses the switch
        // otherwise), only where the worker owner allowed `allow_all`; `--restricted` refuses
        // the flag (CLI 2.1.312).
        if spec.environment.permissions.allow_all && !spec.restricted {
            args.push("--allow-dangerously-skip-permissions".into());
        }
        if let Some(p) = spec.system_prompt.as_deref().filter(|p| !p.is_empty()) {
            args.extend(["--append-system-prompt".into(), p.to_string()]);
        }
        match launch {
            Launch::New => args.extend(["--session-id".into(), uuid.to_string()]),
            Launch::Resume => args.extend(["--resume".into(), uuid.to_string()]),
            // A new transcript `uuid` with the source's records up to `at` (SPEC §8.1).
            Launch::Fork { at } => args.extend([
                "--resume".into(),
                fork_copy_path(uuid).display().to_string(),
                "--fork-session".into(),
                "--session-id".into(),
                uuid.to_string(),
                "--resume-session-at".into(),
                at.to_string(),
            ]),
        }
        args.push("--verbose".into());
        // A provider has one model, and every model name goes to it (SPEC §8.1).
        let model = match &spec.provider {
            Some(p) => Some(&p.model),
            None => spec.model.as_ref(),
        };
        if let Some(m) = model {
            args.extend(["--model".into(), m.clone()]);
        }
        args.extend(self.opts.extra_args.iter().cloned());
        CommandSpec {
            program: self
                .opts
                .claude_bin
                .clone()
                .unwrap_or_else(|| PathBuf::from("claude")),
            args,
            cwd: spec.workdir.clone(),
            env: {
                let mut env = spec.environment.process_env();
                if let Some(p) = &spec.provider {
                    // The worker's own Anthropic credentials never reach another endpoint.
                    env.retain(|(k, _)| !PROVIDER_DROPS.contains(&k.as_str()));
                    for (k, v) in provider_env(p) {
                        env.retain(|(x, _)| *x != k);
                        env.push((k, v));
                    }
                }
                // The worker's own come last.
                env.retain(|(k, _)| k != "CLAUDE_CODE_ENTRYPOINT" && k != "MCP_TOOL_TIMEOUT");
                if let Some(sock) = ssh_agent {
                    env.push((crate::ssh::SSH_AUTH_SOCK.into(), sock.display().to_string()));
                }
                if let Some(dir) = secrets {
                    env.push((
                        crate::secrets::SECRETS_ENV.into(),
                        dir.display().to_string(),
                    ));
                }
                if let Some(e) = &self.opts.entrypoint {
                    env.push(("CLAUDE_CODE_ENTRYPOINT".into(), e.clone()));
                }
                env.push((
                    "MCP_TOOL_TIMEOUT".into(),
                    mcp_tool_timeout_ms(spec.ask_timeout_s).to_string(),
                ));
                env.sort();
                env
            },
        }
    }

    /// The fork of `f` for transcript `uuid` (SPEC §8.1): the source transcript in the
    /// session's own folder, cut at the record of the first of `f.at_event_ids` in it, and
    /// written up to that record to [`fork_copy_path`], which the CLI resumes. The records the fork
    /// copies are registered as the adapter's, so the mirror does not take them for another
    /// writer's.
    fn fork(
        &self,
        spec: &StartSpec,
        f: &ForkFrom,
        uuid: Uuid,
    ) -> anyhow::Result<(Launch, RemoveOnDrop)> {
        let from: Uuid = f
            .runtime_session_id
            .parse()
            .with_context(|| format!("fork source {:?} is not a uuid", f.runtime_session_id))?;
        let dir = self
            .opts
            .config_dir
            .as_deref()
            .context("no Claude Code config folder to fork a transcript in")?;
        let path = paths::transcript_path(dir, &spec.workdir, &from);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("fork source {}", path.display()))?;
        let (at, copied) = fork_point(&text, &f.at_event_ids)
            .with_context(|| format!("the message to fork at is not in {}", path.display()))?;
        let end = line_end(&text, &at)
            .with_context(|| format!("the record {at} is not a line of {}", path.display()))?;
        let copy = RemoveOnDrop(fork_copy_path(&uuid));
        // The conversation in a shared temporary folder: 0600, as the transcript itself; a
        // copy left by a worker that died is replaced.
        let _ = std::fs::remove_file(&copy.0);
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        o.open(&copy.0)
            .and_then(|mut f| std::io::Write::write_all(&mut f, &text.as_bytes()[..end]))
            .with_context(|| format!("fork copy {}", copy.0.display()))?;
        for u in copied {
            self.opts.own_records.add(uuid, u);
        }
        Ok((Launch::Fork { at }, copy))
    }

    async fn spawn(&mut self, spec: &StartSpec, why: &str) -> anyhow::Result<()> {
        if self.proc.is_some() {
            self.kill().await?;
        }
        spec.recheck_workdir()?;
        let (uuid, generated) = self.runtime_uuid(spec)?;
        let transcript = self
            .opts
            .config_dir
            .as_deref()
            .map(|d| paths::transcript_path(d, &spec.workdir, &uuid));
        let resume = transcript.as_ref().is_some_and(|p| p.exists());
        // A fork is made once, by the first start: after it the session has its transcript.
        // The fork copy goes with the process, like its ssh-agent below.
        let (launch, fork_copy) = match (&spec.fork, resume) {
            (_, true) => (Launch::Resume, None),
            (Some(f), false) => {
                let (launch, copy) = self.fork(spec, f, uuid)?;
                (launch, Some(copy))
            }
            (None, false) => (Launch::New, None),
        };
        // T44: an ssh-agent of this process's own; it stops when the process has exited
        // (the monitor drops it), or right here if the spawn fails.
        // SPEC 0.147: a session that may have secrets always gets one, with the keys kept for
        // its workspace too, and a folder for its secrets that goes with the process.
        let mut key_files = self.opts.ssh.files(&spec.environment.ssh_keys);
        if let Some(w) = spec.secrets.as_ref().and_then(|s| s.workspace.as_ref()) {
            key_files.extend(
                w.files()
                    .context("the ssh keys kept for the workspace")?
                    .into_iter()
                    .map(|(_, p)| p),
            );
        }
        let ssh_agent = if key_files.is_empty() && spec.secrets.is_none() {
            None
        } else {
            Some(
                self.opts
                    .ssh
                    .start_agent(&key_files)
                    .await
                    .context("the session's ssh keys")?,
            )
        };
        let secrets = match (&spec.secrets, &ssh_agent) {
            (Some(s), Some(a)) => Some(Arc::new(
                s.stores
                    .process(a.socket().to_path_buf())
                    .context("the folder of the session's secrets")?,
            )),
            _ => None,
        };
        let cmd = self.command(
            spec,
            &uuid,
            launch,
            ssh_agent.as_ref().map(|a| a.socket()),
            secrets.as_ref().map(|s| s.dir()),
        );
        let spawned = self
            .spawner
            .spawn(&cmd)
            .with_context(|| format!("spawning {}", cmd.program.display()))?;
        tracing::info!(
            session_id = %self.session_id,
            runtime_session_id = %uuid,
            mode = match launch {
                Launch::New => "new",
                Launch::Resume => "resume",
                Launch::Fork { .. } => "fork",
            },
            why,
            cwd = %spec.workdir.display(),
            environment = %spec.environment.name,
            ssh_keys = ?spec.environment.ssh_keys,
            browser = spec.browser,
            "claude started"
        );
        // A provider the hub asked for that this worker does not allow: the agent runs on the
        // runtime's own endpoint, and the feed says why (SPEC §8.1).
        if let Some(why) = &spec.browser_refused {
            tracing::warn!(session_id = %self.session_id, reason = %why, "browser refused; the agent starts without it");
            let ev = crate::adapter::new_event(
                self.session_id,
                Author::System,
                EventKind::System {
                    text: format!("{why}: the agent runs without the browser"),
                    code: BROWSER_REFUSED.into(),
                    call_id: None,
                    status: None,
                    task_id: None,
                    fork: None,
                },
            );
            let _ = self.events.send(AdapterEvent::Event(ev)).await;
        }
        if let Some(why) = &spec.provider_refused {
            tracing::warn!(session_id = %self.session_id, reason = %why, "provider refused; the agent starts without it");
            let ev = crate::adapter::new_event(
                self.session_id,
                Author::System,
                EventKind::System {
                    text: format!("{why}: the agent runs on Claude Code's own endpoint"),
                    code: PROVIDER_REFUSED.into(),
                    call_id: None,
                    status: None,
                    task_id: None,
                    fork: None,
                },
            );
            let _ = self.events.send(AdapterEvent::Event(ev)).await;
        }
        // The session talked before, but its transcript is gone (Claude Code deletes those
        // older than its `cleanupPeriodDays`, 30 days by default): the agent starts without the
        // conversation, and the feed says so (SPEC §8.1).
        if matches!(launch, Launch::New) && spec.has_history {
            tracing::warn!(session_id = %self.session_id, runtime_session_id = %uuid, transcript = ?transcript, "the session's transcript is gone; the agent starts without its conversation");
            let ev = crate::adapter::new_event(
                self.session_id,
                Author::System,
                EventKind::System {
                    text: TRANSCRIPT_MISSING_TEXT.into(),
                    code: TRANSCRIPT_MISSING.into(),
                    call_id: None,
                    status: None,
                    task_id: None,
                    fork: None,
                },
            );
            let _ = self.events.send(AdapterEvent::Event(ev)).await;
        }

        // The CLI carries the session's cost over `--resume` from the transcript's last
        // `cost-state`; per-turn costs are counted from there (T27).
        let cost_baseline = match transcript.as_ref().filter(|_| resume) {
            Some(p) => tokio::fs::read_to_string(p)
                .await
                .ok()
                .and_then(|t| hqc_claude_jsonl::last_cost_total(&t)),
            None => None,
        };
        let mut core = Core::new(self.session_id, spec.restricted)
            .with_policy(Arc::new(Policy::new(&spec.environment.permissions)))
            .with_cost_baseline(cost_baseline);
        if let Some(rules) = &spec.folder_rules {
            core = core.with_folder(rules.clone(), spec.workspace.clone());
        }
        let initialize = core.initialize();
        let core = Arc::new(Mutex::new(core));
        let io = Arc::new(Io {
            session_id: self.session_id,
            stdin: tokio::sync::Mutex::new(spawned.stdin),
            events: self.events.clone(),
            signal: spawned.signal,
            transcript: Mutex::new(transcript),
            title: Mutex::new(None),
            runtime: Mutex::new(uuid),
            own: self.opts.own_records.clone(),
        });
        let pid = spawned.pid;
        let live = Live::new(self.opts.own_records.clone(), uuid, pid);
        let killing = Arc::new(AtomicBool::new(false));
        let (exit_tx, exited) = watch::channel(None);
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        let stderr_task = spawned
            .stderr
            .map(|err| tokio::spawn(drain_stderr(err, stderr_tail.clone(), self.session_id)));
        let exit = spawned.exit;
        tokio::spawn(async move {
            let info = match exit.await {
                Ok(i) => i,
                Err(e) => {
                    tracing::warn!("waiting for claude failed: {e}");
                    ExitInfo {
                        code: None,
                        signal: None,
                    }
                }
            };
            let _ = exit_tx.send(Some(info));
        });
        let (started, gate) = watch::channel(false);
        let monitor = tokio::spawn(monitor(
            spawned.stdout,
            core.clone(),
            io.clone(),
            killing.clone(),
            exited.clone(),
            gate,
            stderr_tail.clone(),
            live,
            ssh_agent,
            secrets.clone(),
            fork_copy,
        ));
        tokio::spawn(rate::meter(core.clone(), io.clone(), exited.clone()));
        let mut proc = Proc {
            core,
            io,
            killing,
            exited,
            started,
            monitor,
            pid,
            secrets: secrets.as_ref().map(Arc::downgrade),
        };
        drop(secrets);

        // Bad flags, an unknown `--resume` id or a logged-out CLI end the process at once.
        let early =
            tokio::time::timeout(self.opts.early_exit, proc.exited.wait_for(Option::is_some))
                .await
                .ok()
                .and_then(|r| r.ok().and_then(|i| *i));
        if let Some(info) = early {
            proc.killing.store(true, Ordering::SeqCst);
            let _ = proc.started.send(true);
            let _ = proc.monitor.await;
            if let Some(t) = stderr_task {
                let _ = tokio::time::timeout(Duration::from_secs(1), t).await;
            }
            let tail = stderr_tail
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            anyhow::bail!("claude exited right after start ({info}): {}", tail.trim());
        }
        let _ = proc.started.send(true);
        // The first line of its stdin, before any input can come (SPEC §8.1): the CLI takes
        // `initialize` only before the first user message.
        let init = proc.io.run(vec![initialize]).await;
        self.proc = Some(proc);
        init?;
        if generated {
            let _ = self
                .events
                .send(AdapterEvent::RuntimeSessionId(uuid.to_string()))
                .await;
        }
        Ok(())
    }

    fn running(&self) -> anyhow::Result<&Proc> {
        match &self.proc {
            Some(p) if p.exited.borrow().is_none() => Ok(p),
            _ => anyhow::bail!("claude is not running"),
        }
    }
}

/// Image types the Messages API takes as an image block, and the largest image sent inline;
/// bigger ones reach the agent by path only.
const IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];
pub const MAX_INLINE_IMAGE: u64 = 5 * 1024 * 1024;

/// Content blocks `{type: image, source: {type: base64, media_type, data}}` for the image
/// attachments of an input (SPEC §8.1). A file that cannot be read is skipped: its path is
/// in the text anyway.
pub async fn image_blocks(files: &[InboxFile]) -> Vec<serde_json::Value> {
    use base64::Engine;
    let mut out = Vec::new();
    for f in files {
        if f.kind != hqc_proto::event::ArtifactKind::Image
            || !IMAGE_TYPES.contains(&f.mime.as_str())
            || f.size > MAX_INLINE_IMAGE
        {
            continue;
        }
        match tokio::fs::read(&f.path).await {
            Ok(b) => out.push(serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": f.mime,
                    "data": base64::engine::general_purpose::STANDARD.encode(b),
                },
            })),
            Err(e) => {
                tracing::warn!(path = %f.path.display(), "image attachment not readable: {e}")
            }
        }
    }
    out
}

#[async_trait]
impl Adapter for ClaudeCodeAdapter {
    async fn start(&mut self, spec: &StartSpec) -> anyhow::Result<()> {
        self.spawn(spec, "start").await
    }

    async fn resume(&mut self, spec: &StartSpec) -> anyhow::Result<()> {
        self.spawn(spec, "resume").await
    }

    async fn input(&mut self, input: &SessionInput) -> anyhow::Result<()> {
        self.input_with(input, &[]).await
    }

    async fn input_with(
        &mut self,
        input: &SessionInput,
        files: &[InboxFile],
    ) -> anyhow::Result<()> {
        let images = image_blocks(files).await;
        let p = self.running()?;
        let actions = lock(&p.core).input_with_images(input, images);
        // Before the write: the mirror may read the prompt's record before its echo.
        p.io.own.sent(p.io.runtime(), &input.text);
        tracing::info!(session_id = %self.session_id, event_id = %input.event_id, "input sent to claude");
        p.io.run(actions).await
    }

    fn can_inject(&self) -> bool {
        self.running().is_ok_and(|p| lock(&p.core).can_inject())
    }

    fn runs_own_turn(&self) -> bool {
        self.running().is_ok_and(|p| lock(&p.core).runs_own_turn())
    }

    async fn inject_with(
        &mut self,
        input: &SessionInput,
        files: &[InboxFile],
    ) -> anyhow::Result<bool> {
        let images = image_blocks(files).await;
        let p = self.running()?;
        let Some(actions) = lock(&p.core).inject_with_images(input, images) else {
            return Ok(false);
        };
        p.io.own.sent(p.io.runtime(), &input.text);
        tracing::info!(session_id = %self.session_id, event_id = %input.event_id, "input sent to claude during the turn");
        p.io.run(actions).await?;
        Ok(true)
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        let Ok(p) = self.running() else {
            return Ok(());
        };
        let Some((req, turn)) = lock(&p.core).interrupt() else {
            return Ok(());
        };
        tracing::info!(session_id = %self.session_id, "interrupting the turn");
        p.io.run(vec![req]).await?;
        let (core, io, grace) = (p.core.clone(), p.io.clone(), self.opts.stop_grace);
        let mut exited = p.exited.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(grace) => {}
                _ = exited.wait_for(Option::is_some) => return,
            }
            if !lock(&core).turn_running(turn) {
                return;
            }
            tracing::warn!(session_id = %io.session_id, "no result after interrupt; sending SIGINT");
            io.signal(Sig::Interrupt);
            // A process that runs the turn on, stuck in a tool call that SIGINT does not reach,
            // goes with the commands it started: the group, as `kill` does.
            for (wait, sig) in [(grace, Sig::Terminate), (KILL_GRACE, Sig::Kill)] {
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = exited.wait_for(Option::is_some) => return,
                }
                if !lock(&core).turn_running(turn) {
                    return;
                }
                tracing::warn!(session_id = %io.session_id, signal = ?sig, "the turn runs on after SIGINT; signalling the process group");
                io.signal(sig);
            }
        });
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        self.proc.as_ref().and_then(|p| p.pid)
    }

    fn secrets(&self) -> Option<Arc<crate::secrets::ProcessSecrets>> {
        let p = self.running().ok()?;
        p.secrets.as_ref()?.upgrade()
    }

    async fn kill(&mut self) -> anyhow::Result<()> {
        let Some(mut p) = self.proc.take() else {
            return Ok(());
        };
        p.killing.store(true, Ordering::SeqCst);
        let alive = p.exited.borrow().is_none();
        if alive {
            p.io.signal(Sig::Terminate);
            if !wait_exit(&mut p.exited, KILL_GRACE).await {
                tracing::warn!(session_id = %self.session_id, "claude ignored SIGTERM; sending SIGKILL");
                p.io.signal(Sig::Kill);
                wait_exit(&mut p.exited, KILL_GRACE).await;
            }
        }
        if tokio::time::timeout(KILL_GRACE, &mut p.monitor)
            .await
            .is_err()
        {
            p.monitor.abort();
        }
        tracing::info!(session_id = %self.session_id, "claude killed");
        Ok(())
    }

    async fn set_permission_mode(&mut self, mode: PermissionMode) -> anyhow::Result<()> {
        let Ok(p) = self.running() else {
            return Ok(());
        };
        tracing::info!(session_id = %self.session_id, mode = mode.as_str(), "switching the permission mode");
        let action = lock(&p.core).set_permission_mode(mode);
        p.io.run(vec![action]).await
    }

    async fn set_model(&mut self, model: &str) -> anyhow::Result<()> {
        let Ok(p) = self.running() else {
            return Ok(());
        };
        tracing::info!(session_id = %self.session_id, model, "switching the model");
        let action = lock(&p.core).set_model(model);
        // The CLI writes the switch into the transcript as a typed `/model <name>`; it is ours,
        // not a prompt of someone else for the mirror to import.
        p.io.own.sent(p.io.runtime(), &format!("/model {model}"));
        p.io.run(vec![action]).await
    }

    async fn background(&mut self, call_id: &str) -> anyhow::Result<()> {
        let Ok(p) = self.running() else {
            return Ok(());
        };
        tracing::info!(session_id = %self.session_id, ?call_id, "moving a call to the background");
        let action = lock(&p.core).background(call_id);
        p.io.run(vec![action]).await
    }

    async fn stop_task(&mut self, task_id: &str) -> anyhow::Result<()> {
        let Ok(p) = self.running() else {
            return Ok(());
        };
        tracing::info!(session_id = %self.session_id, ?task_id, "stopping a task");
        let action = lock(&p.core).stop_task(task_id);
        p.io.run(vec![action]).await
    }

    async fn answer_approval(
        &mut self,
        request_id: &str,
        option_id: &str,
        message: Option<&str>,
    ) -> anyhow::Result<()> {
        let p = self.running()?;
        let actions = lock(&p.core).answer_approval(request_id, option_id, message)?;
        p.io.run(actions).await
    }

    async fn answer_clarify(
        &mut self,
        request_id: &str,
        answers: &BTreeMap<String, ClarifyAnswer>,
    ) -> anyhow::Result<()> {
        let p = self.running()?;
        let actions = lock(&p.core).answer_clarify(request_id, answers)?;
        p.io.run(actions).await
    }
}

async fn wait_exit(rx: &mut watch::Receiver<Option<ExitInfo>>, d: Duration) -> bool {
    tokio::time::timeout(d, rx.wait_for(Option::is_some))
        .await
        .is_ok_and(|r| r.is_ok())
}

/// Reads stdout until EOF (or shortly after the process exited), then reports the exit.
#[allow(clippy::too_many_arguments)]
async fn monitor(
    stdout: Pin<Box<dyn AsyncRead + Send>>,
    core: Arc<Mutex<Core>>,
    io: Arc<Io>,
    killing: Arc<AtomicBool>,
    mut exited: watch::Receiver<Option<ExitInfo>>,
    mut gate: watch::Receiver<bool>,
    stderr_tail: Arc<Mutex<String>>,
    mut live: Live,
    ssh_agent: Option<crate::ssh::SshAgent>,
    secrets: Option<Arc<crate::secrets::ProcessSecrets>>,
    fork_copy: Option<RemoveOnDrop>,
) {
    let mut rd = BufReader::new(stdout);
    let mut buf = Vec::new();
    let mut clearing: Option<Clearing> = None;
    let mut drain = exited.clone();
    let after_exit = async move {
        let _ = drain.wait_for(Option::is_some).await;
        tokio::time::sleep(EXIT_DRAIN).await;
    };
    tokio::pin!(after_exit);
    loop {
        tokio::select! {
            r = rd.read_until(b'\n', &mut buf) => match r {
                Ok(0) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf).into_owned();
                    buf.clear();
                    let (actions, seen, cleared) = {
                        let mut c = lock(&core);
                        let a = c.on_line(&line);
                        (a, c.take_seen(), c.clearing())
                    };
                    // `/clear`: the CLI begins the new transcript before the `init` that names
                    // it (~70 ms, `w1-clear`); the mirror is not to take it for someone else's.
                    if cleared && clearing.is_none() {
                        clearing = io
                            .transcript()
                            .as_deref()
                            .and_then(Path::parent)
                            .map(|d| Clearing::new(io.own.clone(), d.to_path_buf()));
                    }
                    // After `/clear` the process writes the new transcript: its records from
                    // this line on are that one's, and the old one is no longer written.
                    for a in &actions {
                        if let Action::Emit(AdapterEvent::RuntimeSessionId(r)) = a {
                            if let Ok(new) = r.parse::<Uuid>() {
                                io.moved_to(new);
                                live = Live::new(io.own.clone(), new, live.2);
                            }
                        }
                    }
                    // Unmarked once the new transcript is live.
                    if !cleared {
                        clearing = None;
                    }
                    for u in seen {
                        io.own.add(io.runtime(), u);
                    }
                    if let Err(e) = io.run(actions).await {
                        tracing::warn!(session_id = %io.session_id, "{e:#}");
                    }
                }
                Err(e) => {
                    tracing::warn!(session_id = %io.session_id, "reading claude stdout: {e}");
                    break;
                }
            },
            _ = &mut after_exit => break,
        }
    }
    let info = exited.wait_for(Option::is_some).await.ok().and_then(|i| *i);
    // The process wrote its last record: the mirror may treat what follows as another
    // writer's.
    drop(live);
    drop(clearing);
    // Its ssh-agent goes with it (T44), its secrets (SPEC 0.147) and the fork copy it resumed
    // (SPEC §8.1).
    drop(secrets);
    drop(ssh_agent);
    drop(fork_copy);
    let actions = lock(&core).on_exit();
    let _ = io.run(actions).await;
    let _ = gate.wait_for(|started| *started).await;
    if killing.load(Ordering::SeqCst) {
        return;
    }
    let error = match info {
        Some(i) if i.success() => None,
        Some(i) => {
            let tail = stderr_tail
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let last = tail.trim().lines().last().unwrap_or("").to_owned();
            Some(if last.is_empty() {
                i.to_string()
            } else {
                format!("{i}: {last}")
            })
        }
        None => Some("exit status unknown".into()),
    };
    tracing::info!(session_id = %io.session_id, ?info, "claude exited");
    let _ = io.events.send(AdapterEvent::Exited { error }).await;
}

/// Keeps the last [`STDERR_TAIL`] bytes for error reports; lines are logged at debug.
async fn drain_stderr(
    err: Pin<Box<dyn AsyncRead + Send>>,
    tail: Arc<Mutex<String>>,
    session_id: Uuid,
) {
    let mut lines = BufReader::new(err).lines();
    while let Ok(Some(l)) = lines.next_line().await {
        tracing::debug!(%session_id, "claude stderr: {l}");
        let mut t = tail.lock().unwrap_or_else(|e| e.into_inner());
        t.push_str(&l);
        t.push('\n');
        if t.len() > STDERR_TAIL {
            let mut cut = t.len() - STDERR_TAIL;
            while !t.is_char_boundary(cut) {
                cut += 1;
            }
            t.drain(..cut);
        }
    }
}

#[cfg(test)]
mod tests;
