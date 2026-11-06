//! Claude Code adapter (SPEC §8.1, docs/ITERATION-1.md T4): one `claude -p` process per
//! session in stream-json mode, mapped to SPEC §4 events by [`engine::Core`].
//!
//! - `start`/`resume` spawn the CLI in `<workspace>/<cwd>`. The flag follows the transcript,
//!   not the call: `--resume <uuid>` if `<config>/projects/<encoded cwd>/<uuid>.jsonl`
//!   exists (the worker restarted, or the process ran before), else `--session-id <uuid>`
//!   (a process that never got a prompt left no transcript to resume).
//! - `stop` sends `control_request {subtype: interrupt}`; if no `result` follows within
//!   [`ClaudeOptions::stop_grace`], the process group gets SIGINT.
//! - `kill` sends SIGTERM to the process group, SIGKILL after 5 s.
//! - A process that exits by itself is reported as [`AdapterEvent::Exited`].

pub mod engine;
pub mod paths;
pub mod process;
pub mod rules;
pub mod scripted;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Claude Code's built-in cross-session tools: they find other local Claude sessions and
/// inject messages into them directly, so the hub never sees that input (SPEC §7 routes
/// agent-to-agent traffic through `hqc_send`/`hqc_ask` instead).
pub const NATIVE_MESSAGING_TOOLS: &[&str] = &["ListAgents", "SendMessage"];

use anyhow::Context;
use async_trait::async_trait;
use hqc_proto::event::ClarifyAnswer;
use hqc_proto::frame::{Runtime, SessionInput};
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

const KILL_GRACE: Duration = Duration::from_secs(5);
/// How long the output of an exited process is still read when its stdout stays open
/// (held by a grandchild).
const EXIT_DRAIN: Duration = Duration::from_secs(2);
const STDERR_TAIL: usize = 2048;

/// Settings files the CLI reads: the user's own and `.claude/settings.local.json` (where
/// `always` writes, SPEC §8.1), not the repository's `.claude/settings.json`.
pub const SETTING_SOURCES: &str = "user,local";

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
    /// does not apply: this, or `auto` for a session in auto (T38).
    pub ask_mode: String,
    /// Tools disallowed in every session on top of `session.start.disallowed_tools`. By
    /// default Claude Code's own cross-session messaging, which bypasses the hub
    /// (`allow_native_messaging` in the worker config keeps it).
    pub always_disallowed: Vec<String>,
    /// Interrupt → SIGINT fallback delay.
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
            always_disallowed: NATIVE_MESSAGING_TOOLS
                .iter()
                .map(|t| t.to_string())
                .collect(),
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
    /// inherits.
    pub fn config(&self, token: &crate::token::Secret) -> String {
        serde_json::json!({
            "mcpServers": {
                "hqc": {
                    "type": "stdio",
                    "command": self.program,
                    "args": ["mcp", "--socket", self.socket],
                    "env": { (crate::mcp::TOKEN_ENV): token.expose() },
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
                always_disallowed: if cfg.allow_native_messaging {
                    vec![]
                } else {
                    NATIVE_MESSAGING_TOOLS
                        .iter()
                        .map(|t| t.to_string())
                        .collect()
                },
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
}

struct Io {
    session_id: Uuid,
    stdin: tokio::sync::Mutex<Pin<Box<dyn AsyncWrite + Send>>>,
    events: EventSink,
    signal: SignalFn,
    /// The session's transcript, where the CLI writes `ai-title` (SPEC §8.1).
    transcript: Option<PathBuf>,
    /// The last title reported by this process.
    title: Mutex<Option<String>>,
    /// The transcript's uuid and the registry its records go to (SPEC §9.3).
    runtime: Uuid,
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

    /// After a turn: the last `ai-title` of the transcript, if there is one and it is new.
    /// The CLI writes it after the first prompt; a missing file or title is not an error.
    async fn report_title(&self) {
        let Some(path) = &self.transcript else {
            return;
        };
        let text = match tokio::fs::read_to_string(path).await {
            Ok(t) => t,
            Err(e) => {
                tracing::debug!(session_id = %self.session_id, "no transcript for ai-title: {e}");
                return;
            }
        };
        let Some(title) = hqc_claude_jsonl::last_ai_title(&text) else {
            return;
        };
        let title = hqc_proto::replace_nul(&title).into_owned();
        {
            let mut last = lock(&self.title);
            if last.as_deref() == Some(title.as_str()) {
                return;
            }
            *last = Some(title.clone());
        }
        let _ = self.events.send(AdapterEvent::Title(title)).await;
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
    /// - `--setting-sources user,local` and `--permission-mode` (the ask mode or `auto`,
    ///   T38): a repository's own
    ///   `.claude/settings.json` (hooks, `defaultMode`) is not applied; `local` stays for the
    ///   rules `always` writes;
    /// - `restricted` → `--restricted` (no command-running tools, file tools confined to the
    ///   working directory, no settings files; SPEC §8.4);
    /// - the allow/deny rules of the session's environment (`[permissions]` merged with the
    ///   environment's, T43) → `--settings`, in restricted mode too (the CLI applies
    ///   `--settings` there, checked on 2.1.308);
    /// - `--strict-mcp-config` unless the environment has `allow_local_mcp` (the hqc server
    ///   is in `--mcp-config` either way); [`crate::config::Environment::process_env`];
    ///   `SSH_AUTH_SOCK` only as `ssh_agent`, the socket of the process's own ssh-agent (T44);
    /// - `disallowed_tools` → `--disallowedTools`, `system_prompt` →
    ///   `--append-system-prompt`. Nothing of `session.start` is silently dropped.
    pub fn command(
        &self,
        spec: &StartSpec,
        uuid: &Uuid,
        resume: bool,
        ssh_agent: Option<&std::path::Path>,
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
            args.extend(["--mcp-config".into(), m.config(&spec.session_token)]);
        }
        args.extend(["--allowedTools".into(), "mcp__hqc".into()]);
        let mut disallowed: Vec<String> = spec.disallowed_tools.clone();
        for t in &self.opts.always_disallowed {
            if !disallowed.contains(t) {
                disallowed.push(t.clone());
            }
        }
        if !disallowed.is_empty() {
            // Variadic: the entries run up to the next flag.
            args.push("--disallowedTools".into());
            args.extend(disallowed);
        }
        if let Some(settings) = spec.environment.permissions.settings_json() {
            args.extend(["--settings".into(), settings]);
        }
        if spec.restricted {
            args.push("--restricted".into());
        } else {
            args.extend(["--setting-sources".into(), SETTING_SOURCES.into()]);
        }
        args.extend([
            "--permission-mode".into(),
            match spec.permission_mode {
                PermissionMode::Ask => self.opts.ask_mode.clone(),
                PermissionMode::Auto => "auto".into(),
            },
        ]);
        if let Some(p) = spec.system_prompt.as_deref().filter(|p| !p.is_empty()) {
            args.extend(["--append-system-prompt".into(), p.to_string()]);
        }
        args.extend([
            if resume { "--resume" } else { "--session-id" }.into(),
            uuid.to_string(),
        ]);
        args.push("--verbose".into());
        if let Some(m) = &spec.model {
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
                // The worker's own come last.
                env.retain(|(k, _)| k != "CLAUDE_CODE_ENTRYPOINT" && k != "MCP_TOOL_TIMEOUT");
                if let Some(sock) = ssh_agent {
                    env.push((crate::ssh::SSH_AUTH_SOCK.into(), sock.display().to_string()));
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
        // T44: an ssh-agent of this process's own; it stops when the process has exited
        // (the monitor drops it), or right here if the spawn fails.
        let ssh_agent = match spec.environment.ssh_keys.as_slice() {
            [] => None,
            keys => Some(
                self.opts
                    .ssh
                    .start_agent(keys)
                    .await
                    .context("the environment's ssh keys")?,
            ),
        };
        let cmd = self.command(spec, &uuid, resume, ssh_agent.as_ref().map(|a| a.socket()));
        let spawned = self
            .spawner
            .spawn(&cmd)
            .with_context(|| format!("spawning {}", cmd.program.display()))?;
        tracing::info!(
            session_id = %self.session_id,
            runtime_session_id = %uuid,
            mode = if resume { "resume" } else { "new" },
            why,
            cwd = %spec.workdir.display(),
            environment = %spec.environment.name,
            ssh_keys = ?spec.environment.ssh_keys,
            "claude started"
        );

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
        let core = Arc::new(Mutex::new(core));
        let io = Arc::new(Io {
            session_id: self.session_id,
            stdin: tokio::sync::Mutex::new(spawned.stdin),
            events: self.events.clone(),
            signal: spawned.signal,
            transcript,
            title: Mutex::new(None),
            runtime: uuid,
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
        ));
        let mut proc = Proc {
            core,
            io,
            killing,
            exited,
            started,
            monitor,
            pid,
        };

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
        self.proc = Some(proc);
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
        p.io.own.sent(p.io.runtime, &input.text);
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
        p.io.own.sent(p.io.runtime, &input.text);
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
            if lock(&core).turn_running(turn) {
                tracing::warn!(session_id = %io.session_id, "no result after interrupt; sending SIGINT");
                io.signal(Sig::Interrupt);
            }
        });
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        self.proc.as_ref().and_then(|p| p.pid)
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
    live: Live,
    ssh_agent: Option<crate::ssh::SshAgent>,
) {
    let mut rd = BufReader::new(stdout);
    let mut buf = Vec::new();
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
                    let (actions, seen) = {
                        let mut c = lock(&core);
                        let a = c.on_line(&line);
                        (a, c.take_seen())
                    };
                    for u in seen {
                        io.own.add(io.runtime, u);
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
    // Its ssh-agent goes with it (T44).
    drop(ssh_agent);
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
