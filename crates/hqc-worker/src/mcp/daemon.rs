//! The daemon's end of the MCP socket (SPEC §7.2): `$HQC_HOME/mcp.sock`, mode 0600, one
//! per daemon.
//!
//! - A peer of another OS user is refused before reading a byte (the mode already keeps
//!   them out; this also covers the moment between `bind` and `chmod`).
//! - The first line is [`ToDaemon::Hello`]: a token that is not the process token of a
//!   session whose agent runs here gets [`FromDaemon::Refused`] and the connection closes.
//! - So does a peer that does not descend from that session's agent process
//!   ([`super::procs::descends_from`]): the token is on the agent's command line, which every
//!   process of the OS user can read, the agents of the worker's other sessions included
//!   (SPEC §2.1). The peer's pid is the kernel's (`LOCAL_PEEREPID` / `SO_PEERCRED`), taken
//!   when the connection is accepted.
//! - Every [`ToDaemon::Call`] is checked again (the agent may have been replaced since) and
//!   goes to the hub as `mcp.call` ([`HubLink::mcp_call`]); its `mcp.result` comes back as
//!   [`FromDaemon::Result`].
//! - `hqc_attach` first reads and uploads the file ([`Attacher::prepare`]); only then does
//!   the `mcp.call` go, with the artifact id instead of the path. A cancel or EOF during the
//!   upload stops it, and no `mcp.call` is sent.
//! - [`ToDaemon::Cancel`] and EOF with calls open send `mcp.cancel` for each of them.
//! - The token is never logged.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use hqc_proto::frame::McpError;
use hqc_proto::mcp::{cancel_reason, error_code, MAX_ARGS};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::attach::Attacher;
use super::{FromDaemon, ToDaemon, MAX_LINE};
use crate::link::HubLink;

/// A connection that sends no hello within this is closed.
const HELLO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// How long a hello waits for the agent's pid: the CLI may start its MCP servers before the
/// worker has finished starting it.
const PID_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The bound socket; the file is removed when this is dropped.
pub struct McpSocket {
    listener: UnixListener,
    path: PathBuf,
}

impl McpSocket {
    /// Binds `path` with mode 0600. A socket file left by a daemon that is gone is replaced;
    /// one a live daemon answers on is not.
    pub fn bind(path: &Path) -> anyhow::Result<Self> {
        if path.exists() {
            if std::os::unix::net::UnixStream::connect(path).is_ok() {
                anyhow::bail!(
                    "another worker daemon serves {}; one daemon per HQC_HOME",
                    path.display()
                );
            }
            std::fs::remove_file(path)
                .with_context(|| format!("removing the stale {}", path.display()))?;
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let listener =
            UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod {}", path.display()))?;
        Ok(Self {
            listener,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Serves connections until `stop`.
    pub async fn run(self, link: HubLink, attacher: Attacher, stop: CancellationToken) {
        let uid = unsafe { libc::geteuid() };
        loop {
            let stream = tokio::select! {
                _ = stop.cancelled() => break,
                r = self.listener.accept() => match r {
                    Ok((s, _)) => s,
                    Err(e) => {
                        tracing::warn!("mcp socket accept: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        continue;
                    }
                },
            };
            let peer_pid = match stream.peer_cred() {
                Ok(c) if c.uid() == uid => c.pid().and_then(|p| u32::try_from(p).ok()),
                Ok(c) => {
                    tracing::warn!(
                        peer_uid = c.uid(),
                        "mcp socket: a peer of another user refused"
                    );
                    continue;
                }
                Err(e) => {
                    tracing::warn!("mcp socket: peer credentials: {e}");
                    continue;
                }
            };
            tokio::spawn(serve(
                stream,
                peer_pid,
                link.clone(),
                attacher.clone(),
                stop.clone(),
            ));
        }
    }
}

impl Drop for McpSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn refused(code: &str, message: impl Into<String>) -> FromDaemon {
    FromDaemon::Refused {
        error: McpError::new(code, message),
    }
}

/// The session's agent process, once the worker knows it; `None` if the agent stopped or
/// the pid did not come in time.
async fn agent_pid(link: &HubLink, sid: Uuid) -> Option<u32> {
    let deadline = tokio::time::Instant::now() + PID_TIMEOUT;
    loop {
        match link.mcp().pid(sid) {
            Some(super::AgentPid::Known(pid)) => return Some(pid),
            Some(super::AgentPid::Starting) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            _ => return None,
        }
    }
}

/// Whether `peer` (the connecting process) runs inside the agent of session `sid`.
async fn from_the_agent(link: &HubLink, sid: Uuid, peer: Option<u32>) -> bool {
    let Some(peer) = peer else {
        tracing::warn!(session_id = %sid, "mcp socket: the peer's pid is unknown");
        return false;
    };
    let Some(agent) = agent_pid(link, sid).await else {
        tracing::warn!(session_id = %sid, "mcp socket: the agent's process is not known");
        return false;
    };
    let ok = super::procs::descends_from(peer, agent);
    if !ok {
        tracing::warn!(session_id = %sid, peer, agent, "mcp socket: a peer outside the session's agent refused");
    }
    ok
}

/// One MCP process.
async fn serve(
    stream: UnixStream,
    peer_pid: Option<u32>,
    link: HubLink,
    attacher: Attacher,
    stop: CancellationToken,
) {
    let (rd, mut wr) = stream.into_split();
    let mut lines = BufReader::new(rd);
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<FromDaemon>();
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            let mut line = serde_json::to_vec(&m).expect("daemon messages serialize");
            line.push(b'\n');
            if wr.write_all(&line).await.is_err() {
                break;
            }
        }
    });

    let mut buf = Vec::new();
    let hello = tokio::time::timeout(HELLO_TIMEOUT, read_line(&mut lines, &mut buf))
        .await
        .unwrap_or(None);
    let token = match hello {
        Some(Ok(ToDaemon::Hello {
            token,
            protocol_version,
            client,
        })) => match link.mcp().session_for(&token) {
            Some(sid) if !from_the_agent(&link, sid, peer_pid).await => {
                let _ = out_tx.send(refused(
                    error_code::UNAUTHORIZED,
                    "this process is not part of the session's agent",
                ));
                None
            }
            Some(sid) => {
                tracing::info!(session_id = %sid, ?protocol_version, client = %client.unwrap_or_default(), "mcp server connected");
                let _ = out_tx.send(FromDaemon::Ready);
                Some(token)
            }
            None => {
                tracing::warn!("mcp socket: a token of no running session refused");
                let _ = out_tx.send(refused(
                    error_code::UNAUTHORIZED,
                    "the session token is not one of a session this worker runs",
                ));
                None
            }
        },
        Some(Ok(_)) => {
            let _ = out_tx.send(refused(error_code::INVALID_ARGS, "hello expected"));
            None
        }
        Some(Err(why)) => {
            let _ = out_tx.send(refused(error_code::INVALID_ARGS, why));
            None
        }
        None => None,
    };
    let Some(token) = token else {
        drop(out_tx);
        let _ = writer.await;
        return;
    };

    // The connection's call ids -> `request_id`s of open calls.
    let open: Arc<std::sync::Mutex<HashMap<u64, Uuid>>> = Arc::default();
    // Calls still being prepared on the worker (an upload): no `mcp.call` yet.
    let preparing: Arc<std::sync::Mutex<HashMap<u64, tokio::task::AbortHandle>>> = Arc::default();
    loop {
        let msg = tokio::select! {
            _ = stop.cancelled() => break,
            m = read_line(&mut lines, &mut buf) => m,
        };
        match msg {
            None => break,
            Some(Err(why)) => {
                tracing::warn!("mcp socket: {why}");
                break;
            }
            Some(Ok(ToDaemon::Hello { .. })) => {
                tracing::warn!("mcp socket: a second hello");
                break;
            }
            Some(Ok(ToDaemon::Cancel { id })) => {
                if let Some(h) = preparing.lock().unwrap().remove(&id) {
                    tracing::info!(
                        call = id,
                        "mcp call cancelled by the runtime before it was sent"
                    );
                    h.abort();
                }
                let rid = open.lock().unwrap().remove(&id);
                if let Some(rid) = rid {
                    tracing::info!(request_id = %rid, "mcp call cancelled by the runtime");
                    link.mcp_cancel(rid, cancel_reason::CANCELLED);
                }
            }
            Some(Ok(ToDaemon::Call { id, tool, args })) => {
                let fail = |code: &str, message: String| {
                    let _ = out_tx.send(FromDaemon::Result {
                        id,
                        result: None,
                        error: Some(McpError::new(code, message)),
                    });
                };
                let Some(sid) = link.mcp().session_for(&token) else {
                    fail(
                        error_code::UNAUTHORIZED,
                        "the session's agent no longer runs".into(),
                    );
                    continue;
                };
                let size = serde_json::to_vec(&args).map(|v| v.len()).unwrap_or(0);
                if size > MAX_ARGS {
                    fail(
                        error_code::INVALID_ARGS,
                        format!("arguments are {size} bytes, more than {MAX_ARGS}"),
                    );
                    continue;
                }
                if tool == hqc_proto::mcp::ATTACH {
                    let Some(folders) = link.mcp().folders(sid) else {
                        fail(
                            error_code::UNAVAILABLE,
                            "the session's agent no longer runs".into(),
                        );
                        continue;
                    };
                    let (link, attacher, out) = (link.clone(), attacher.clone(), out_tx.clone());
                    let (open, prep) = (open.clone(), preparing.clone());
                    // Held until the task is registered, so that it cannot finish first.
                    let mut g = preparing.lock().unwrap();
                    let task = tokio::spawn(async move {
                        let args = attacher.prepare(sid, &folders, args).await;
                        // Held until the call is in `open`: an EOF meanwhile then sees it.
                        let mut p = prep.lock().unwrap();
                        if p.remove(&id).is_none() {
                            return;
                        }
                        match args {
                            Ok(args) => send_call(&link, &out, &open, sid, id, &tool, args),
                            Err(e) => {
                                let _ = out.send(FromDaemon::Result {
                                    id,
                                    result: None,
                                    error: Some(e),
                                });
                            }
                        }
                    });
                    g.insert(id, task.abort_handle());
                    continue;
                }
                send_call(&link, &out_tx, &open, sid, id, &tool, args);
            }
        }
    }
    for (_, h) in preparing.lock().unwrap().drain() {
        h.abort();
    }
    // EOF (the MCP process exited) or stop: the calls still open die with it.
    let left: Vec<Uuid> = open.lock().unwrap().drain().map(|(_, r)| r).collect();
    for rid in left {
        tracing::info!(request_id = %rid, "mcp server gone with the call open");
        link.mcp_cancel(rid, cancel_reason::EOF);
    }
    drop(out_tx);
    let _ = writer.await;
}

/// Sends `mcp.call` for the connection's call `id` and hands its `mcp.result` back.
fn send_call(
    link: &HubLink,
    out_tx: &mpsc::UnboundedSender<FromDaemon>,
    open: &Arc<std::sync::Mutex<HashMap<u64, Uuid>>>,
    sid: Uuid,
    id: u64,
    tool: &str,
    args: serde_json::Value,
) {
    let fail = |code: &str, message: &str| {
        let _ = out_tx.send(FromDaemon::Result {
            id,
            result: None,
            error: Some(McpError::new(code, message)),
        });
    };
    let (rid, rx) = match link.mcp_call(sid, tool, args) {
        Ok(Some(c)) => c,
        Ok(None) => {
            return fail(
                error_code::UNAVAILABLE,
                "the session's agent no longer runs",
            )
        }
        Err(e) => {
            tracing::error!(session_id = %sid, "mcp call not queued: {e:#}");
            return fail(error_code::INTERNAL, "the worker could not queue the call");
        }
    };
    tracing::info!(session_id = %sid, request_id = %rid, %tool, "mcp call");
    open.lock().unwrap().insert(id, rid);
    let (out, open) = (out_tx.clone(), open.clone());
    tokio::spawn(async move {
        // A cancelled call's sender is dropped: no answer goes back (MCP).
        let Ok(outcome) = rx.await else { return };
        if open.lock().unwrap().remove(&id).is_none() {
            return;
        }
        let (result, error) = match outcome {
            Ok(r) => (Some(r), None),
            Err(e) => (None, Some(e)),
        };
        let _ = out.send(FromDaemon::Result { id, result, error });
    });
}

/// The next message; `None` at EOF, `Err` for a line too long or not a message.
async fn read_line<R: tokio::io::AsyncBufRead + Unpin>(
    r: &mut R,
    buf: &mut Vec<u8>,
) -> Option<Result<ToDaemon, String>> {
    buf.clear();
    let mut limited = (&mut *r).take(MAX_LINE as u64 + 1);
    match limited.read_until(b'\n', buf).await {
        Ok(0) => None,
        Ok(_) if buf.len() > MAX_LINE => Some(Err(format!("a line over {MAX_LINE} bytes"))),
        Ok(_) => Some(serde_json::from_slice(buf).map_err(|e| format!("not a message: {e}"))),
        Err(e) => Some(Err(format!("read: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn one_socket_per_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home/mcp.sock");
        let s = McpSocket::bind(&path).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let err = McpSocket::bind(&path).err().unwrap().to_string();
        assert!(err.contains("another worker daemon"), "{err}");
        drop(s);
        assert!(!path.exists(), "removed when the daemon stops");
        // A file left by a daemon that died is replaced.
        std::fs::write(&path, b"").unwrap();
        let _s = McpSocket::bind(&path).unwrap();
    }
}
