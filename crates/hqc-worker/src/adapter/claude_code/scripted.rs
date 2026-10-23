//! A stand-in for `claude` in tests: plays a captured stream-json stdout over in-memory
//! pipes. It waits for the first stdin line (the prompt) before playing, and after each
//! `control_request` waits for one more stdin line (the answer), like the real CLI. After
//! the script it stays "running" until signalled. Every stdin line, command and signal is
//! recorded.

use std::io;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::watch;

use super::process::{CommandSpec, ExitInfo, Sig, Spawned, Spawner};

#[derive(Default)]
struct State {
    stdin: Vec<Value>,
    commands: Vec<CommandSpec>,
    signals: Vec<Sig>,
}

#[derive(Clone)]
pub struct ScriptedClaude {
    script: Arc<Vec<String>>,
    state: Arc<Mutex<State>>,
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
        }
    }

    pub fn stdin(&self) -> Vec<Value> {
        self.lock().stdin.clone()
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
            let record = |line: &str| {
                if let Ok(v) = serde_json::from_str(line) {
                    state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .stdin
                        .push(v);
                }
            };
            let play = async {
                let mut line = String::new();
                if rd.read_line(&mut line).await.ok()? == 0 {
                    return None::<()>;
                }
                record(&line);
                for out in script.iter() {
                    script_out.write_all(out.as_bytes()).await.ok()?;
                    script_out.write_all(b"\n").await.ok()?;
                    if out.contains("\"control_request\"") {
                        line.clear();
                        if rd.read_line(&mut line).await.ok()? == 0 {
                            return None;
                        }
                        record(&line);
                    }
                }
                // Idle: keep recording stdin (e.g. an interrupt) until EOF.
                loop {
                    line.clear();
                    if rd.read_line(&mut line).await.ok()? == 0 {
                        return None;
                    }
                    record(&line);
                }
            };
            tokio::select! {
                _ = play => {}
                _ = stop.wait_for(Option::is_some) => {}
            }
            // Dropping `script_out` closes stdout.
        });
        let state = self.state.clone();
        let signal = Box::new(move |s: Sig| {
            state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .signals
                .push(s);
            let _ = sig_tx.send(Some(s));
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
