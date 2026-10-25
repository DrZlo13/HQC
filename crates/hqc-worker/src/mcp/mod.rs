//! The hqc MCP server and its path to the hub (SPEC §5.2, §7, §7.2).
//!
//! ```text
//! runtime --stdio JSON-RPC--> `hqc-worker mcp` --unix socket--> daemon --mcp.call--> hub
//! ```
//!
//! - [`server`]: `hqc-worker mcp`, the stdio MCP server the runtime starts itself. It takes
//!   the session token only from `HQC_SESSION_TOKEN` and keeps no state but its open calls.
//! - [`daemon`]: the daemon's end of the socket `$HQC_HOME/mcp.sock` (0600, one per daemon).
//!   A connection names its session by the token; the daemon accepts only the token of a
//!   session whose agent process it runs ([`McpCalls::session_for`]).
//! - [`McpCalls`]: the sessions' tokens and the calls waiting for `mcp.result`, shared by
//!   the socket, the session tasks and the hub connection. A call goes to the hub through
//!   the outbox (SPEC §5.2) without its token, which is added when the frame is sent:
//!   a call whose session or caller is gone (a worker restart) is dropped instead of sent.
//!
//! Tokens. The hub issues a fresh token with every `session.start`, also for an agent that
//! keeps running (a reconnect). The MCP process keeps the token its agent was started with
//! (the "process token", what it authenticates with on the socket), while `mcp.call`
//! carries the session's latest one (the "hub token").

pub mod attach;
pub mod daemon;
pub mod procs;
pub mod server;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hqc_proto::frame::McpError;
use hqc_proto::mcp::error_code;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::token::Secret;

/// The variable the MCP process reads its session token from (SPEC §7.2).
pub const TOKEN_ENV: &str = "HQC_SESSION_TOKEN";

/// The daemon's socket in `$HQC_HOME`.
pub fn socket_path(hqc_home: &Path) -> PathBuf {
    hqc_home.join("mcp.sock")
}

/// Longest line on the socket: the arguments of one call plus the envelope.
pub const MAX_LINE: usize = hqc_proto::mcp::MAX_ARGS + 4096;

/// What the MCP process sends the daemon, one JSON object per line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToDaemon {
    /// First line: the session token; the daemon answers [`FromDaemon::Ready`] or
    /// [`FromDaemon::Refused`] and closes.
    Hello {
        token: String,
        /// The MCP protocol version agreed with the runtime and the runtime's
        /// `clientInfo`, for the daemon's log.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol_version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client: Option<Value>,
    },
    /// `id` is the connection's own number for the call.
    Call { id: u64, tool: String, args: Value },
    /// The runtime cancelled call `id` (`notifications/cancelled`).
    Cancel { id: u64 },
}

/// What the daemon answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromDaemon {
    Ready,
    Refused {
        error: McpError,
    },
    Result {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<McpError>,
    },
}

pub type Outcome = Result<Value, McpError>;

/// The sessions' tokens and the open calls (see the module doc).
#[derive(Default)]
pub struct McpCalls {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    sessions: HashMap<Uuid, Tokens>,
    calls: HashMap<Uuid, Open>,
}

struct Tokens {
    process: Secret,
    hub: Secret,
    /// Where the session's agent works, for `hqc_attach`.
    folders: attach::Folders,
    /// The agent's process: the MCP server must descend from it.
    pid: AgentPid,
    /// The input of the turn the agent runs (or ran last), sent with its calls.
    turn: Option<Uuid>,
}

/// What the daemon knows of a session's agent process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentPid {
    /// The agent is being started; its MCP server may connect before the start returns.
    Starting,
    /// Started, but its runtime reports no process: its MCP server is refused.
    Unknown,
    Known(u32),
}

struct Open {
    session_id: Uuid,
    tx: oneshot::Sender<Outcome>,
}

impl McpCalls {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The session's agent process starts with `token` in `folders`: its MCP server
    /// authenticates with the token, and calls carry it until the next `session.start`.
    pub fn bind(&self, session_id: Uuid, token: &Secret, folders: attach::Folders) {
        self.lock().sessions.insert(
            session_id,
            Tokens {
                process: token.clone(),
                hub: token.clone(),
                folders,
                pid: AgentPid::Starting,
                turn: None,
            },
        );
    }

    /// The folders of the session whose agent runs.
    pub fn folders(&self, session_id: Uuid) -> Option<attach::Folders> {
        self.lock()
            .sessions
            .get(&session_id)
            .map(|t| t.folders.clone())
    }

    /// The session's agent started as `pid` (`None`: its runtime knows no process).
    pub fn set_pid(&self, session_id: Uuid, pid: Option<u32>) {
        if let Some(t) = self.lock().sessions.get_mut(&session_id) {
            t.pid = pid.map_or(AgentPid::Unknown, AgentPid::Known);
        }
    }

    /// The agent's process; `None` if the agent does not run.
    pub fn pid(&self, session_id: Uuid) -> Option<AgentPid> {
        self.lock().sessions.get(&session_id).map(|t| t.pid)
    }

    /// The agent was given input `event_id`: the turn its calls belong to (SPEC §7.1).
    pub fn set_turn(&self, session_id: Uuid, event_id: Uuid) {
        if let Some(t) = self.lock().sessions.get_mut(&session_id) {
            t.turn = Some(event_id);
        }
    }

    /// The input of the session's current turn.
    pub fn turn(&self, session_id: Uuid) -> Option<Uuid> {
        self.lock().sessions.get(&session_id).and_then(|t| t.turn)
    }

    /// `session.start` for an agent that keeps running: the hub knows only `token` now.
    pub fn set_hub_token(&self, session_id: Uuid, token: &Secret) {
        if let Some(t) = self.lock().sessions.get_mut(&session_id) {
            t.hub = token.clone();
        }
    }

    /// The agent process stopped: its MCP server is gone with it, its open calls fail.
    pub fn unbind(&self, session_id: Uuid) {
        let mut g = self.lock();
        if g.sessions.remove(&session_id).is_none() {
            return;
        }
        let ids: Vec<Uuid> = g
            .calls
            .iter()
            .filter(|(_, c)| c.session_id == session_id)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(c) = g.calls.remove(&id) {
                let _ = c.tx.send(Err(McpError::new(
                    error_code::UNAVAILABLE,
                    "the session's agent stopped",
                )));
            }
        }
    }

    /// The session whose running agent was started with `token`.
    pub fn session_for(&self, token: &str) -> Option<Uuid> {
        self.lock()
            .sessions
            .iter()
            .find(|(_, t)| same(t.process.expose(), token))
            .map(|(id, _)| *id)
    }

    /// A new call of `session_id`; `None` if its agent no longer runs.
    pub fn open(&self, session_id: Uuid) -> Option<(Uuid, oneshot::Receiver<Outcome>)> {
        let mut g = self.lock();
        if !g.sessions.contains_key(&session_id) {
            return None;
        }
        let id = Uuid::now_v7();
        let (tx, rx) = oneshot::channel();
        g.calls.insert(id, Open { session_id, tx });
        Some((id, rx))
    }

    /// The token to send call `request_id` with; `None` if the call is not open any more
    /// (answered, cancelled, from a previous run of the worker).
    pub fn token_for(&self, request_id: Uuid) -> Option<Secret> {
        let g = self.lock();
        let c = g.calls.get(&request_id)?;
        g.sessions.get(&c.session_id).map(|t| t.hub.clone())
    }

    /// Hands the outcome to the caller; `false` if nobody waits for it.
    pub fn finish(&self, request_id: Uuid, outcome: Outcome) -> bool {
        match self.lock().calls.remove(&request_id) {
            Some(c) => c.tx.send(outcome).is_ok(),
            None => false,
        }
    }

    /// Forgets the call; `false` if it was not open.
    pub fn forget(&self, request_id: Uuid) -> bool {
        self.lock().calls.remove(&request_id).is_some()
    }

    pub fn is_open(&self, request_id: Uuid) -> bool {
        self.lock().calls.contains_key(&request_id)
    }
}

/// Equality that does not stop at the first differing byte.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_calls() {
        let m = McpCalls::default();
        let sid = Uuid::now_v7();
        let first = Secret::new("hqcs_first");
        assert!(m.open(sid).is_none(), "no agent, no call");
        let folders = attach::Folders {
            workspace: "/w".into(),
            workdir: "/w/sub".into(),
        };
        m.bind(sid, &first, folders.clone());
        assert_eq!(m.folders(sid), Some(folders));
        assert_eq!(m.pid(sid), Some(AgentPid::Starting));
        m.set_pid(sid, Some(42));
        assert_eq!(m.pid(sid), Some(AgentPid::Known(42)));
        m.set_pid(sid, None);
        assert_eq!(m.pid(sid), Some(AgentPid::Unknown));
        assert_eq!(m.turn(sid), None);
        let input = Uuid::now_v7();
        m.set_turn(sid, input);
        assert_eq!(m.turn(sid), Some(input));
        assert_eq!(m.session_for("hqcs_first"), Some(sid));
        assert_eq!(m.session_for("hqcs_firsT"), None);
        assert_eq!(m.session_for(""), None);

        let (rid, mut rx) = m.open(sid).unwrap();
        assert_eq!(m.token_for(rid), Some(first.clone()));
        // A reconnect's `session.start`: the process keeps its token, calls carry the new.
        m.set_hub_token(sid, &Secret::new("hqcs_second"));
        assert_eq!(m.session_for("hqcs_first"), Some(sid));
        assert_eq!(m.session_for("hqcs_second"), None);
        assert_eq!(m.token_for(rid).unwrap().expose(), "hqcs_second");

        assert!(m.finish(rid, Ok(Value::Null)));
        assert_eq!(rx.try_recv().unwrap(), Ok(Value::Null));
        assert!(!m.finish(rid, Ok(Value::Null)), "answered once");
        assert_eq!(m.token_for(rid), None);

        let (rid, mut rx) = m.open(sid).unwrap();
        m.unbind(sid);
        assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "unavailable");
        assert!(!m.is_open(rid));
        assert_eq!(m.session_for("hqcs_first"), None);
        assert_eq!(m.pid(sid), None);
        assert_eq!(m.turn(sid), None);
    }
}
