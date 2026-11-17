//! A stand-in for `claude` in tests: plays a captured stream-json stdout over in-memory
//! pipes. It waits for the first stdin line (the prompt) before playing, and after each
//! `control_request` waits for one more stdin line (the answer), like the real CLI. After
//! the script it stays "running" until signalled (a signal it [`ScriptedClaude::ignoring`]
//! is recorded and survived). Every stdin line, command and signal is recorded; a
//! `control_request` `initialize` is no prompt or answer, like the real CLI's handshake, and is
//! recorded apart ([`ScriptedClaude::initialize`]).

use std::io;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::watch;

use super::process::{CommandSpec, ExitInfo, Sig, Spawned, Spawner};

#[derive(Default)]
struct State {
    stdin: Vec<Value>,
    /// Each `initialize` request with the number of [`State::stdin`] lines before it.
    initialize: Vec<(usize, Value)>,
    commands: Vec<CommandSpec>,
    signals: Vec<Sig>,
}

/// Reads stdin up to the next line that is not an `initialize` request and records what it
/// read; `None` at EOF.
async fn next_line(
    rd: &mut BufReader<tokio::io::DuplexStream>,
    state: &Mutex<State>,
) -> Option<()> {
    loop {
        let mut line = String::new();
        if rd.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            return Some(());
        };
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        if v["type"] == "control_request" && v["request"]["subtype"] == "initialize" {
            let before = s.stdin.len();
            s.initialize.push((before, v));
            continue;
        }
        s.stdin.push(v);
        return Some(());
    }
}

#[derive(Clone)]
pub struct ScriptedClaude {
    script: Arc<Vec<String>>,
    state: Arc<Mutex<State>>,
    /// Signals the process survives, like a CLI stuck in a tool call that ignores SIGINT.
    ignored: Arc<Vec<Sig>>,
}

impl ScriptedClaude {
    /// `stream`: stream-json stdout, one JSON object per line.
    pub fn new(stream: &str) -> Self {
        Self {
            script: Arc::new(
                stream
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(str::to_owned)
                    .collect(),
            ),
            state: Arc::default(),
            ignored: Arc::default(),
        }
    }

    /// The same, surviving `signals`.
    pub fn ignoring(mut self, signals: &[Sig]) -> Self {
        self.ignored = Arc::new(signals.to_vec());
        self
    }

    pub fn stdin(&self) -> Vec<Value> {
        self.lock().stdin.clone()
    }

    /// The `initialize` requests read, each with the number of [`ScriptedClaude::stdin`] lines
    /// read before it.
    pub fn initialize(&self) -> Vec<(usize, Value)> {
        self.lock().initialize.clone()
    }

    pub fn commands(&self) -> Vec<CommandSpec> {
        self.lock().commands.clone()
    }

    pub fn signals(&self) -> Vec<Sig> {
        self.lock().signals.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Spawner for ScriptedClaude {
    fn spawn(&self, cmd: &CommandSpec) -> io::Result<Spawned> {
        self.lock().commands.push(cmd.clone());
        let (stdin, script_in) = tokio::io::duplex(1 << 20);
        let (mut script_out, stdout) = tokio::io::duplex(1 << 20);
        let (sig_tx, sig_rx) = watch::channel::<Option<Sig>>(None);
        let script = self.script.clone();
        let state = self.state.clone();
        let mut stop = sig_rx.clone();
        tokio::spawn(async move {
            let mut rd = BufReader::new(script_in);
            let play = async {
                next_line(&mut rd, &state).await?;
                for out in script.iter() {
                    script_out.write_all(out.as_bytes()).await.ok()?;
                    script_out.write_all(b"\n").await.ok()?;
                    if out.contains("\"control_request\"") {
                        next_line(&mut rd, &state).await?;
                    }
                }
                // Idle: keep recording stdin (e.g. an interrupt) until EOF.
                while next_line(&mut rd, &state).await.is_some() {}
                None::<()>
            };
            tokio::select! {
                _ = play => {}
                _ = stop.wait_for(Option::is_some) => {}
            }
            // Dropping `script_out` closes stdout.
        });
        let state = self.state.clone();
        let ignored = self.ignored.clone();
        let signal = Box::new(move |s: Sig| {
            state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .signals
                .push(s);
            if !ignored.contains(&s) {
                let _ = sig_tx.send(Some(s));
            }
            Ok(())
        });
        let mut rx = sig_rx;
        let exit = Box::pin(async move {
            let s = rx
                .wait_for(Option::is_some)
                .await
                .map(|s| *s)
                .unwrap_or(Some(Sig::Kill));
            Ok(ExitInfo {
                code: None,
                signal: Some(match s {
                    Some(Sig::Interrupt) => 2,
                    Some(Sig::Terminate) => 15,
                    _ => 9,
                }),
            })
        });
        Ok(Spawned {
            pid: None,
            stdin: Box::pin(stdin),
            stdout: Box::pin(stdout),
            stderr: None,
            signal,
            exit,
        })
    }
}
