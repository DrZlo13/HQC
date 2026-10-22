//! Spawning the CLI behind a trait, so tests can stand in for `claude` with a script that
//! talks over in-memory pipes ([`super::scripted`]).

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use tokio::io::{AsyncRead, AsyncWrite};

/// What to run; built by the adapter, inspected by tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The whole environment of the process: nothing is inherited from the worker.
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sig {
    Interrupt,
    Terminate,
    Kill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl ExitInfo {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

impl fmt::Display for ExitInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.code, self.signal) {
            (Some(c), _) => write!(f, "exit code {c}"),
            (None, Some(s)) => write!(f, "killed by signal {s}"),
            (None, None) => write!(f, "exited"),
        }
    }
}

pub type SignalFn = Box<dyn Fn(Sig) -> io::Result<()> + Send + Sync>;

pub struct Spawned {
    pub stdin: Pin<Box<dyn AsyncWrite + Send>>,
    pub stdout: Pin<Box<dyn AsyncRead + Send>>,
    pub stderr: Option<Pin<Box<dyn AsyncRead + Send>>>,
    /// Signals the process (its whole process group for the real CLI). A no-op once it
    /// has exited.
    pub signal: SignalFn,
    /// Resolves when the process has exited.
    pub exit: BoxFuture<'static, io::Result<ExitInfo>>,
}

pub trait Spawner: Send + Sync + 'static {
    fn spawn(&self, cmd: &CommandSpec) -> io::Result<Spawned>;
}

/// The real thing: a child process in its own process group, so signals reach the shells
/// it started and a Ctrl-C at the worker's terminal does not reach it.
pub struct OsSpawner;

impl Spawner for OsSpawner {
    fn spawn(&self, cmd: &CommandSpec) -> io::Result<Spawned> {
        let mut c = tokio::process::Command::new(&cmd.program);
        c.args(&cmd.args)
            .current_dir(&cmd.cwd)
            .env_clear()
            .envs(cmd.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        c.process_group(0);
        let mut child = c.spawn()?;
        let pid = child.id();
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let exited = Arc::new(AtomicBool::new(false));
        let flag = exited.clone();
        let exit = Box::pin(async move {
            let st = child.wait().await;
            flag.store(true, Ordering::SeqCst);
            let st = st?;
            #[cfg(unix)]
            let signal = std::os::unix::process::ExitStatusExt::signal(&st);
            #[cfg(not(unix))]
            let signal = None;
            Ok(ExitInfo {
                code: st.code(),
                signal,
            })
        });
        let signal: SignalFn = Box::new(move |s| {
            if exited.load(Ordering::SeqCst) {
                return Ok(());
            }
            match pid {
                Some(pid) => signal_group(pid, s),
                None => Ok(()),
            }
        });
        Ok(Spawned {
            stdin: Box::pin(stdin),
            stdout: Box::pin(stdout),
            stderr: Some(Box::pin(stderr)),
            signal,
            exit,
        })
    }
}

#[cfg(unix)]
fn signal_group(pid: u32, s: Sig) -> io::Result<()> {
    let sig = match s {
        Sig::Interrupt => libc::SIGINT,
        Sig::Terminate => libc::SIGTERM,
        Sig::Kill => libc::SIGKILL,
    };
    let pgid = i32::try_from(pid).map_err(|_| io::Error::other("pid out of range"))?;
    // SAFETY: kill(2) with a negative pid signals the process group we created with
    // `process_group(0)`; it touches no memory.
    let r = unsafe { libc::kill(-pgid, sig) };
    if r == 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(e)
}

#[cfg(not(unix))]
fn signal_group(_pid: u32, _s: Sig) -> io::Result<()> {
    Err(io::Error::other("signals are only supported on unix"))
}
