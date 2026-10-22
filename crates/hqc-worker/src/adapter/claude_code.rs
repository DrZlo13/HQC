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
use crate::permissions::{PermissionMode, Permissions, Policy};

const KILL_GRACE: Duration = Duration::from_secs(5);
/// How long the output of an exited process is still read when its stdout stays open
/// (held by a grandchild).
const EXIT_DRAIN: Duration = Duration::from_secs(2);
const STDERR_TAIL: usize = 2048;

/// Settings files the CLI reads: the user's own and `.claude/settings.local.json` (where
/// `always` writes, SPEC §8.1), not the repository's `.claude/settings.json`.
pub const SETTING_SOURCES: &str = "user,local";

/// The agent's environment: only these variables of the worker's (SPEC §14), never `HQC_*`
/// (the worker's token store, config) or other secrets the worker runs with. `SHELL` and
/// `LOGNAME` are what the CLI's Bash tool and the Keychain lookup need besides the list of
/// SPEC §14; variables of the workspace's environment come on top (iteration 5).
pub fn child_env(vars: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
    const NAMES: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "TERM",
        "TMPDIR",
        "SSH_AUTH_SOCK",
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
    /// `--mcp-config` JSON. Unset in iteration 1 (MCP hqc is iteration 3).
    pub mcp_config: Option<String>,
    /// Appended to the command line (tests: `--max-turns`).
    pub extra_args: Vec<String>,
    /// `--permission-mode`, always passed so that a `defaultMode` from settings
    /// (`bypassPermissions`, `acceptEdits`) does not apply: the CLI's "ask" mode, `default`
    /// or `manual` by version ([`crate::capabilities::claude_permission_mode`]), or `auto`
    /// with `[permissions] mode = "auto"`.
    pub permission_mode: String,
    /// Tools disallowed in every session on top of `session.start.disallowed_tools`. By
    /// default Claude Code's own cross-session messaging, which bypasses the hub
    /// (`allow_native_messaging` in the worker config keeps it).
    pub always_disallowed: Vec<String>,
    /// `[permissions]` of the worker config: the lists go to `--settings`, the adapter
    /// reads them for `policy_hint` ([`Policy`]).
    pub permissions: Permissions,
    /// Interrupt → SIGINT fallback delay.
    pub stop_grace: Duration,
    /// A process that dies this soon after spawning fails `start` instead of being
    /// reported as `Exited` (bad flags, unknown `--resume` id, not logged in).
    pub early_exit: Duration,
}

impl Default for ClaudeOptions {
    fn default() -> Self {
        Self {
            claude_bin: None,
            config_dir: paths::default_config_dir(),
            mcp_config: None,
            extra_args: vec![],
            permission_mode: "default".into(),
            always_disallowed: NATIVE_MESSAGING_TOOLS
                .iter()
                .map(|t| t.to_string())
                .collect(),
            permissions: Permissions::default(),
            stop_grace: Duration::from_secs(10),
            early_exit: Duration::from_millis(500),
        }
    }
}

/// Factory for [`ClaudeCodeAdapter`]s.
#[derive(Clone)]
pub struct ClaudeCodeAdapters {
    opts: Arc<ClaudeOptions>,
    policy: Arc<Policy>,
    spawner: Arc<dyn Spawner>,
}

impl ClaudeCodeAdapters {
    pub fn new(opts: ClaudeOptions, spawner: Arc<dyn Spawner>) -> Self {
        Self {
            policy: Arc::new(Policy::new(&opts.permissions)),
            opts: Arc::new(opts),
            spawner,
        }
    }

    /// `permission_mode`: the CLI's "ask" mode, used unless `[permissions] mode = "auto"`.
    pub fn from_config(cfg: &Config, permission_mode: String) -> Self {
        Self::new(
            ClaudeOptions {
                claude_bin: cfg.claude_bin.clone(),
                permission_mode: match cfg.permissions.mode {
                    PermissionMode::Ask => permission_mode,
                    PermissionMode::Auto => "auto".into(),
                },
                permissions: cfg.permissions.clone(),
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
            policy: self.policy.clone(),
            spawner: self.spawner.clone(),
            events,
            proc: None,
        })
    }
}

pub struct ClaudeCodeAdapter {
    session_id: Uuid,
    opts: Arc<ClaudeOptions>,
    policy: Arc<Policy>,
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
}

struct Io {
    session_id: Uuid,
    stdin: tokio::sync::Mutex<Pin<Box<dyn AsyncWrite + Send>>>,
    events: EventSink,
    signal: SignalFn,
}

impl Io {
    async fn run(&self, actions: Vec<Action>) -> anyhow::Result<()> {
        for a in actions {
            match a {
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

    fn signal(&self, s: Sig) {
        if let Err(e) = (self.signal)(s) {
            tracing::warn!(session_id = %self.session_id, ?s, "signalling claude failed: {e}");
        }
    }
}

fn lock(core: &Mutex<Core>) -> std::sync::MutexGuard<'_, Core> {
    core.lock().unwrap_or_else(|e| e.into_inner())
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
    /// - `--setting-sources user,local` and `--permission-mode default`: a repository's own
    ///   `.claude/settings.json` (hooks, `defaultMode`) is not applied; `local` stays for the
    ///   rules `always` writes;
    /// - `restricted` → `--restricted` (no command-running tools, file tools confined to the
    ///   working directory, no settings files; SPEC §8.4);
    /// - `[permissions]` allow/deny → `--settings`, in restricted mode too (the CLI applies
    ///   `--settings` there, checked on 2.1.308);
    /// - `disallowed_tools` → `--disallowedTools`, `system_prompt` →
    ///   `--append-system-prompt`. Nothing of `session.start` is silently dropped.
    pub fn command(&self, spec: &StartSpec, uuid: &Uuid, resume: bool) -> CommandSpec {
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
            "--strict-mcp-config",
        ]
        .map(String::from)
        .into();
        if let Some(m) = &self.opts.mcp_config {
            args.extend(["--mcp-config".into(), m.clone()]);
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
        if let Some(settings) = self.opts.permissions.settings_json() {
            args.extend(["--settings".into(), settings]);
        }
        if spec.restricted {
            args.push("--restricted".into());
        } else {
            args.extend(["--setting-sources".into(), SETTING_SOURCES.into()]);
        }
        args.extend([
            "--permission-mode".into(),
            self.opts.permission_mode.clone(),
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
            env: child_env(std::env::vars()),
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
        let cmd = self.command(spec, &uuid, resume);
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
            "claude started"
        );

        let core = Arc::new(Mutex::new(
            Core::new(self.session_id, spec.restricted).with_policy(self.policy.clone()),
        ));
        let io = Arc::new(Io {
            session_id: self.session_id,
            stdin: tokio::sync::Mutex::new(spawned.stdin),
            events: self.events.clone(),
            signal: spawned.signal,
        });
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
        ));
        let mut proc = Proc {
            core,
            io,
            killing,
            exited,
            started,
            monitor,
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

#[async_trait]
impl Adapter for ClaudeCodeAdapter {
    async fn start(&mut self, spec: &StartSpec) -> anyhow::Result<()> {
        self.spawn(spec, "start").await
    }

    async fn resume(&mut self, spec: &StartSpec) -> anyhow::Result<()> {
        self.spawn(spec, "resume").await
    }

    async fn input(&mut self, input: &SessionInput) -> anyhow::Result<()> {
        let p = self.running()?;
        let actions = lock(&p.core).input(input);
        tracing::info!(session_id = %self.session_id, event_id = %input.event_id, "input sent to claude");
        p.io.run(actions).await
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

    async fn answer_approval(&mut self, request_id: &str, option_id: &str) -> anyhow::Result<()> {
        let p = self.running()?;
        let actions = lock(&p.core).answer_approval(request_id, option_id)?;
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
async fn monitor(
    stdout: Pin<Box<dyn AsyncRead + Send>>,
    core: Arc<Mutex<Core>>,
    io: Arc<Io>,
    killing: Arc<AtomicBool>,
    mut exited: watch::Receiver<Option<ExitInfo>>,
    mut gate: watch::Receiver<bool>,
    stderr_tail: Arc<Mutex<String>>,
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
                    let actions = lock(&core).on_line(&line);
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
