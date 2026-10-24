//! Worker <-> hub frames, SPEC §5. Every frame is `{id, type, payload}`; a reply
//! (`ack`, `hello.ack`, `error`) reuses the `id` of the frame it answers.
//!
//! Frames are strict (SPEC §5.2): the protocol version is agreed in `hello`, and a frame
//! that does not parse is answered with `error` [`error_code::INVALID_FRAME`].
//!
//! Typed here: the frames of iteration 1 (docs/ITERATION-1.md, T1) and the folder requests
//! `fs.stat` / `fs.list_dirs` with their `result` (T14), confirmations outside a session
//! `worker.approval.request` / `worker.approval.response` (T17). Later iterations: hub ->
//! worker `session.steer`, `workspace.*`, `mirror.set`, `model.proxy`, `mcp.result`; worker ->
//! hub `mirror.event`, `mcp.call`, `mcp.cancel`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event::{ClarifyAnswer, Event};

/// `{id, type, payload}`; `body` supplies `type` and `payload`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub id: String,
    #[serde(flatten)]
    pub body: T,
}

impl<T> Envelope<T> {
    pub fn new(id: impl Into<String>, body: T) -> Self {
        Self {
            id: id.into(),
            body,
        }
    }

    /// A reply to this frame: same `id`, new body.
    pub fn reply<U>(&self, body: U) -> Envelope<U> {
        Envelope {
            id: self.id.clone(),
            body,
        }
    }
}

/// `ack {event_id?, seq?}` (SPEC §5, §5.2).
/// - to `event`: `{event_id, seq}`; `seq` of the stored event for a duplicate, absent when
///   the event was dropped for an unknown session;
/// - to `session.input`: `{event_id}` of the delivered input;
/// - otherwise `{}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

/// `error {code, message, retryable, details?}` (SPEC §5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireError {
    /// One of [`error_code`]; the list is open, an unknown code is treated as `internal`.
    pub code: String,
    pub message: String,
    pub retryable: bool,
    /// `{min_version}` for `protocol_version_unsupported`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Initial error codes (SPEC §5).
pub mod error_code {
    pub const PROTOCOL_VERSION_UNSUPPORTED: &str = "protocol_version_unsupported";
    pub const UNAUTHORIZED: &str = "unauthorized";
    pub const UNKNOWN_SESSION: &str = "unknown_session";
    pub const INVALID_FRAME: &str = "invalid_frame";
    pub const AGENT_START_FAILED: &str = "agent_start_failed";
    pub const INTERNAL: &str = "internal";
}

/// Frames sent by the worker (SPEC §5.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum WorkerFrame {
    #[serde(rename = "hello")]
    Hello(Hello),
    /// `event.session_id` identifies the session; it is not repeated in the frame (SPEC §5.4).
    #[serde(rename = "event")]
    Event { event: Event },
    #[serde(rename = "session.state")]
    SessionState {
        session_id: Uuid,
        process_state: ProcessState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        runtime_session_id: Option<String>,
        locked_by: LockedBy,
        /// A title the runtime gave the session (Claude Code: the `ai-title` record of the
        /// transcript, SPEC §8.1). The hub applies it only while the user has not renamed
        /// the session (`title_source`, SPEC §3 Session).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        /// Who outside hqc holds the transcript open (SPEC §9.3): absent — the worker has
        /// no registry data, `null` — nobody, an object — a live process. With the nil
        /// `session_id` the frame is only a holder report: the hub finds the session by
        /// `runtime_session_id` and ignores `process_state` and `locked_by`.
        #[serde(default, skip_serializing_if = "Option::is_none", with = "maybe_null")]
        held_by: Option<Option<HeldBy>>,
    },
    /// Reply to a hub request (`fs.stat`, `fs.list_dirs`): same `id` as the request.
    #[serde(rename = "result")]
    Result(RequestResult),
    /// A confirmation the worker needs from its owner outside any session (SPEC §10.2); the
    /// hub acks it and answers later with `worker.approval.response`.
    #[serde(rename = "worker.approval.request")]
    WorkerApprovalRequest(WorkerApprovalRequest),
    /// An event read from a Claude Code transcript by the mirror (SPEC §9.1, §9.2); goes
    /// through the outbox and is answered like `event`.
    #[serde(rename = "mirror.event")]
    MirrorEvent(MirrorEvent),
    #[serde(rename = "ack")]
    Ack(Ack),
    #[serde(rename = "error")]
    Error(WireError),
}

/// `worker.approval.request {request_id, kind, description, expires_at}` (SPEC §5.4, §10.2).
/// The hub writes it into the worker's service session as `approval.request` with the
/// options [`worker_approval::ONCE`] / [`worker_approval::DENY`]. Sent again with the same
/// `request_id` after a reconnect while the worker still waits; the hub stores it once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerApprovalRequest {
    pub request_id: String,
    /// One of [`worker_approval`]'s kinds; the list is open.
    pub kind: String,
    pub description: String,
    pub expires_at: DateTime<Utc>,
}

/// Kinds and options of `worker.approval.*` (SPEC §10.2).
pub mod worker_approval {
    /// `session.start` in a folder the worker has not run sessions in yet, for a session
    /// the worker owner did not start (SPEC §10.2).
    pub const WORKSPACE_USE: &str = "workspace.use";
    /// Iteration 5 (SPEC §13.1, §13.2).
    pub const WORKSPACE_CREATE: &str = "workspace.create";
    pub const WORKSPACE_DELETE: &str = "workspace.delete";
    /// Option ids of the `approval.request` the hub writes, and of the answer.
    pub const ONCE: &str = "once";
    pub const DENY: &str = "deny";
}

/// `mirror.event {workspace_path, runtime_session_id, cwd, event, title?, recent}` (SPEC
/// §5.4, §9.2). The hub finds the session by the worker and `runtime_session_id` (the
/// transcript's uuid) and creates it in the workspace's project when there is none; the
/// worker does not know hqc session ids, so `event.session_id` is ignored (the worker sends
/// the nil uuid). `author` is replaced by the hub too: the workspace owner for
/// `user.message`, the session's agent otherwise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MirrorEvent {
    /// The mirrored workspace (`mirror.set`) the transcript's `cwd` lies in.
    pub workspace_path: String,
    pub runtime_session_id: String,
    /// Absolute working directory from the transcript's records.
    pub cwd: String,
    pub event: Event,
    /// The transcript's last `ai-title`, if any (SPEC §8.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The transcript was modified less than [`MIRROR_RECENT_S`] seconds before the worker
    /// read this record: someone writes it now (SPEC §9.1, §9.3). Old history (an import)
    /// comes with `false`.
    #[serde(default)]
    pub recent: bool,
    /// As in `session.state`: absent without registry data, `null` when no process
    /// outside hqc holds the transcript.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "maybe_null")]
    pub held_by: Option<Option<HeldBy>>,
}

/// The quiet period of SPEC §9.1/§9.3: a transcript written within it is live in VS Code.
pub const MIRROR_RECENT_S: u64 = 30;

/// A live Claude Code process outside hqc that holds a transcript open (SPEC §9.3): an entry
/// of `<claude config>/sessions/<pid>.json` whose process exists and is not the worker's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldBy {
    /// `entrypoint` of the entry: `claude-vscode` for the VS Code extension, `cli`,
    /// `sdk-cli` and others; the list is open.
    pub entrypoint: String,
    /// `idle` | `busy`; the list is open.
    pub status: String,
}

/// `Option<Option<T>>` for a field that may be absent (`None`), `null` (`Some(None)`) or a
/// value: serde alone reads `null` as absent.
mod maybe_null {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer, T: Serialize>(
        v: &Option<Option<T>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match v {
            Some(inner) => inner.serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        d: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(d).map(Some)
    }
}

/// `workspace.list_candidates {request_id}` -> `result` [`CandidatesResult`] (SPEC §5.3,
/// §9.1). `request_id` equals the frame `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListCandidates {
    pub request_id: String,
}

/// `mirror.set {workspace_path, enabled}` -> `ack` (SPEC §5.3, §9.2). The hub sends it for
/// every workspace with `mirror_enabled` after `hello` and when one is created; the worker
/// mirrors only paths inside its `allowed_roots`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorSet {
    pub workspace_path: String,
    pub enabled: bool,
    /// The hub's sessions of this workspace with a transcript (T21, SPEC §9.1): the worker's
    /// first pass over such a transcript skips records not newer than `since_ts`, the last
    /// event the hub has, so enabling the mirror on a workspace with hqc sessions brings no
    /// duplicates. Absent when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<MirrorSince>,
}

/// One element of [`MirrorSet::sessions`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorSince {
    pub runtime_session_id: String,
    pub since_ts: DateTime<Utc>,
}

/// `session.cleanup {session_id, workspace_path | null, cwd, event_ids[]}` -> `ack` (T21, SPEC
/// §5.3, §17.4): the session was archived or deleted. The worker removes
/// `<workspace>/<cwd>/.hqc/inbox/<event_id>` of the listed inputs (only inside `allowed_roots`
/// and the workspace), or, for a session without workspace (`null`), its scratch folder
/// `<scratch_root>/<session_id>`. Nothing else is deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCleanup {
    pub session_id: Uuid,
    pub workspace_path: Option<String>,
    pub cwd: String,
    /// Inputs of the session that carried artifacts (their inbox folders).
    #[serde(default)]
    pub event_ids: Vec<Uuid>,
}

/// Result of `workspace.list_candidates`: working directories (from the records' `cwd`)
/// that have Claude Code transcripts, inside the worker's `allowed_roots`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidatesResult {
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub path: String,
    pub sessions_count: u32,
    /// Modification time of the newest transcript.
    pub last_active: DateTime<Utc>,
}

/// Frames sent by the hub (SPEC §5.3).
///
/// There is no `approval.cancel` frame: when the hub closes a request itself (timeout,
/// answer from another device, archiving) it writes the `approval.cancel` event and sends
/// `approval.response` with the option whose `canonical` is `deny` (SPEC §5.3, §10.1).
/// A clarify request is closed the same way: `clarify.cancel` event plus `clarify.response`
/// with empty `answers`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum HubFrame {
    /// Reply to `hello`; a rejected `hello` gets `error` `protocol_version_unsupported`.
    #[serde(rename = "hello.ack")]
    HelloAck {
        protocol_version: u32,
        worker_id: Uuid,
        server_time: DateTime<Utc>,
        /// The worker's service session (SPEC §10.2): its log lines go there as `system`
        /// events. Absent from hubs before T17.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_session_id: Option<Uuid>,
    },
    #[serde(rename = "session.start")]
    SessionStart(SessionStart),
    #[serde(rename = "session.input")]
    SessionInput(SessionInput),
    /// Abort the current turn.
    #[serde(rename = "session.stop")]
    SessionStop { session_id: Uuid },
    /// Terminate the agent process.
    #[serde(rename = "session.kill")]
    SessionKill { session_id: Uuid },
    #[serde(rename = "approval.response")]
    ApprovalResponse {
        session_id: Uuid,
        request_id: String,
        option_id: String,
    },
    /// Empty `answers` means the hub cancelled the request; the adapter answers the
    /// runtime with `deny` and the turn goes on without answers.
    #[serde(rename = "clarify.response")]
    ClarifyResponse {
        session_id: Uuid,
        request_id: String,
        answers: BTreeMap<String, ClarifyAnswer>,
    },
    /// Check a folder before it becomes a workspace (SPEC §5.3, §13.1).
    #[serde(rename = "fs.stat")]
    FsStat(FsStat),
    /// Subfolders for the folder picker (SPEC §5.3).
    #[serde(rename = "fs.list_dirs")]
    FsListDirs(FsListDirs),
    /// The owner's answer to `worker.approval.request`, or `deny` when the hub cancelled it
    /// (timeout). The worker acks it, also when nothing waits for it any more.
    #[serde(rename = "worker.approval.response")]
    WorkerApprovalResponse {
        request_id: String,
        option_id: String,
    },
    /// Folders with Claude Code sessions, for the import (SPEC §5.3, §9.1).
    #[serde(rename = "workspace.list_candidates")]
    WorkspaceListCandidates(ListCandidates),
    /// Mirror the transcripts of a workspace, or stop (SPEC §5.3, §9.2).
    #[serde(rename = "mirror.set")]
    MirrorSet(MirrorSet),
    /// Remove the session's inbox folders or scratch folder (T21, SPEC §5.3, §17.4).
    #[serde(rename = "session.cleanup")]
    SessionCleanup(SessionCleanup),
    #[serde(rename = "ack")]
    Ack(Ack),
    #[serde(rename = "error")]
    Error(WireError),
}

/// `fs.stat {request_id, path}` -> `result` [`FsStatResult`]. `request_id` equals the frame
/// `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsStat {
    pub request_id: String,
    /// Absolute path on the worker.
    pub path: String,
}

/// `fs.list_dirs {request_id, path | null}` -> `result` [`FsListDirsResult`]: `null` lists
/// `allowed_roots`, a path its subfolders. A path outside `allowed_roots` is answered with
/// `error {invalid_frame}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsListDirs {
    pub request_id: String,
    pub path: Option<String>,
}

/// `result {request_id, ...}`: the body depends on the request it answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestResult {
    pub request_id: String,
    #[serde(flatten)]
    pub body: ResultBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResultBody {
    FsStat(FsStatResult),
    FsListDirs(FsListDirsResult),
    Candidates(CandidatesResult),
}

/// Result of `fs.stat`. `canonical` is the path with symlinks resolved, present when it
/// exists; the hub stores it as the workspace path. Outside `allowed_roots` the worker does
/// not tell whether the path exists: `{exists: false, is_dir: false,
/// inside_allowed_roots: false}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsStatResult {
    pub exists: bool,
    pub is_dir: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical: Option<String>,
    pub inside_allowed_roots: bool,
}

/// Result of `fs.list_dirs`: folders only, no hidden ones, sorted by name, at most
/// [`MAX_DIR_ENTRIES`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsListDirsResult {
    pub entries: Vec<DirEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    /// Absolute path, to pass back as `fs.list_dirs.path` or as a workspace path.
    pub path: String,
}

/// Most entries one `fs.list_dirs` returns.
pub const MAX_DIR_ENTRIES: usize = 500;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    /// A fresh uuid per worker process (SPEC §5.4, §17.3). The hub treats only a change of
    /// `boot_id` as a worker restart; the same `boot_id` again is a reconnect.
    pub boot_id: Uuid,
    /// `{claude_code?: {version}, acp_agents: []}`. Workspaces are hub objects (SPEC §3);
    /// the worker only checks them against `allowed_roots`.
    pub capabilities: serde_json::Value,
    #[serde(default)]
    pub environments: Vec<String>,
    #[serde(default)]
    pub env_bindings: Vec<serde_json::Value>,
    #[serde(default)]
    pub allowed_roots: Vec<String>,
    #[serde(default)]
    pub allow_foreign_sessions: bool,
    #[serde(default)]
    pub share_local_models: bool,
    #[serde(default)]
    pub local_models: Vec<serde_json::Value>,
    /// Fingerprint of the machine (T21, SPEC §5.1): hex sha256 of a stable machine id with
    /// the salt `hqc`; the id itself never leaves the machine. Absent from older workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    ClaudeCode,
    Acp,
    Bare,
    /// No agent: the worker's service session (SPEC §10.2); never started.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Stopped,
    Starting,
    Running,
    Stopping,
}

/// Who set the session title (SPEC §3 Session): `default` — the hub at creation
/// («Сессия N»), `runtime` — the agent (`session.state.title`), `user` — `PATCH`. The hub
/// replaces a `default` or `runtime` title with the runtime's, never a `user` one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TitleSource {
    #[default]
    Default,
    Runtime,
    User,
}

impl TitleSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Runtime => "runtime",
            Self::User => "user",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockedBy {
    None,
    Hqc,
    Vscode,
}

/// `session.start.mode` (SPEC §5.3): `new` if the hub has never sent `session.start` for the
/// session, else `resume`. The worker calls `start` or `resume` of the adapter accordingly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartMode {
    New,
    Resume,
}

/// `session.start` (SPEC §5.3). Optional fields are omitted when absent;
/// `workspace_path` is explicitly `null` for a session without workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStart {
    pub session_id: Uuid,
    pub mode: StartMode,
    pub runtime: Runtime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_agent: Option<String>,
    /// Absent: the runtime's default model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub workspace_path: Option<String>,
    pub cwd: String,
    pub environment: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
    pub restricted: bool,
    pub auto_approve: bool,
    pub session_token: String,
    pub owner_is_worker_owner: bool,
    pub ask_timeout_s: u32,
    /// The workspace was added by the worker owner: starting a session in it needs no
    /// confirmation of a new folder (SPEC §10.2). Sent only when true.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub requested_by_owner: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInput {
    pub session_id: Uuid,
    /// `event_id` of the `user.message` / `peer.message` being delivered.
    pub event_id: Uuid,
    pub text: String,
    #[serde(default)]
    pub artifact_ids: Vec<Uuid>,
    /// Buffered `send` messages (SPEC §7.1); always empty in iteration 1.
    #[serde(default)]
    pub peer_prefix: Vec<PeerPrefix>,
}

/// One buffered `send` message in `session.input.peer_prefix` (SPEC §5.3, §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerPrefix {
    /// `<project-slug>/<session-slug>` of the sender.
    pub from: String,
    pub text: String,
    pub ts: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use serde_json::{json, Value};

    /// Frame `type` values of iteration 1, as written in SPEC §5.3 / §5.4.
    const SPEC_HUB_TYPES: &[&str] = &[
        "hello.ack",
        "session.start",
        "session.input",
        "session.stop",
        "session.kill",
        "approval.response",
        "clarify.response",
        "fs.stat",
        "fs.list_dirs",
        "worker.approval.response",
        "workspace.list_candidates",
        "mirror.set",
        "session.cleanup",
        "ack",
        "error",
    ];
    const SPEC_WORKER_TYPES: &[&str] = &[
        "hello",
        "event",
        "session.state",
        "result",
        "worker.approval.request",
        "mirror.event",
        "ack",
        "error",
    ];

    /// Wire examples shared with the TS client (`examples/wire/{hub,worker}.<type>[.<case>].json`).
    const HUB_EXAMPLES: &[(&str, &str)] = &[
        (
            "hello.ack",
            include_str!("../examples/wire/hub.hello.ack.json"),
        ),
        (
            "session.start",
            include_str!("../examples/wire/hub.session.start.json"),
        ),
        (
            "session.input",
            include_str!("../examples/wire/hub.session.input.json"),
        ),
        (
            "session.stop",
            include_str!("../examples/wire/hub.session.stop.json"),
        ),
        (
            "session.kill",
            include_str!("../examples/wire/hub.session.kill.json"),
        ),
        (
            "approval.response",
            include_str!("../examples/wire/hub.approval.response.json"),
        ),
        (
            "clarify.response",
            include_str!("../examples/wire/hub.clarify.response.json"),
        ),
        (
            "clarify.response",
            include_str!("../examples/wire/hub.clarify.response.cancel.json"),
        ),
        ("fs.stat", include_str!("../examples/wire/hub.fs.stat.json")),
        (
            "fs.list_dirs",
            include_str!("../examples/wire/hub.fs.list_dirs.json"),
        ),
        (
            "fs.list_dirs",
            include_str!("../examples/wire/hub.fs.list_dirs.roots.json"),
        ),
        (
            "worker.approval.response",
            include_str!("../examples/wire/hub.worker.approval.response.json"),
        ),
        (
            "workspace.list_candidates",
            include_str!("../examples/wire/hub.workspace.list_candidates.json"),
        ),
        (
            "mirror.set",
            include_str!("../examples/wire/hub.mirror.set.json"),
        ),
        (
            "mirror.set",
            include_str!("../examples/wire/hub.mirror.set.sessions.json"),
        ),
        (
            "session.cleanup",
            include_str!("../examples/wire/hub.session.cleanup.json"),
        ),
        (
            "session.cleanup",
            include_str!("../examples/wire/hub.session.cleanup.scratch.json"),
        ),
        ("ack", include_str!("../examples/wire/hub.ack.json")),
        (
            "ack",
            include_str!("../examples/wire/hub.ack.unknown-session.json"),
        ),
        ("error", include_str!("../examples/wire/hub.error.json")),
    ];
    const WORKER_EXAMPLES: &[(&str, &str)] = &[
        ("hello", include_str!("../examples/wire/worker.hello.json")),
        (
            "hello",
            include_str!("../examples/wire/worker.hello.machine.json"),
        ),
        ("event", include_str!("../examples/wire/worker.event.json")),
        (
            "session.state",
            include_str!("../examples/wire/worker.session.state.json"),
        ),
        (
            "session.state",
            include_str!("../examples/wire/worker.session.state.held.json"),
        ),
        (
            "session.state",
            include_str!("../examples/wire/worker.session.state.released.json"),
        ),
        (
            "result",
            include_str!("../examples/wire/worker.result.fs.stat.json"),
        ),
        (
            "result",
            include_str!("../examples/wire/worker.result.fs.list_dirs.json"),
        ),
        (
            "result",
            include_str!("../examples/wire/worker.result.candidates.json"),
        ),
        (
            "worker.approval.request",
            include_str!("../examples/wire/worker.worker.approval.request.json"),
        ),
        (
            "mirror.event",
            include_str!("../examples/wire/worker.mirror.event.json"),
        ),
        (
            "mirror.event",
            include_str!("../examples/wire/worker.mirror.event.forked.json"),
        ),
        ("ack", include_str!("../examples/wire/worker.ack.json")),
        ("error", include_str!("../examples/wire/worker.error.json")),
    ];

    /// Exhaustive on purpose: a new variant does not compile until it gets a spec name here.
    fn hub_type(f: &HubFrame) -> &'static str {
        match f {
            HubFrame::HelloAck { .. } => "hello.ack",
            HubFrame::SessionStart(_) => "session.start",
            HubFrame::SessionInput(_) => "session.input",
            HubFrame::SessionStop { .. } => "session.stop",
            HubFrame::SessionKill { .. } => "session.kill",
            HubFrame::ApprovalResponse { .. } => "approval.response",
            HubFrame::ClarifyResponse { .. } => "clarify.response",
            HubFrame::FsStat(_) => "fs.stat",
            HubFrame::FsListDirs(_) => "fs.list_dirs",
            HubFrame::WorkerApprovalResponse { .. } => "worker.approval.response",
            HubFrame::WorkspaceListCandidates(_) => "workspace.list_candidates",
            HubFrame::MirrorSet(_) => "mirror.set",
            HubFrame::SessionCleanup(_) => "session.cleanup",
            HubFrame::Ack(_) => "ack",
            HubFrame::Error(_) => "error",
        }
    }

    fn worker_type(f: &WorkerFrame) -> &'static str {
        match f {
            WorkerFrame::Hello(_) => "hello",
            WorkerFrame::Event { .. } => "event",
            WorkerFrame::SessionState { .. } => "session.state",
            WorkerFrame::Result(_) => "result",
            WorkerFrame::WorkerApprovalRequest(_) => "worker.approval.request",
            WorkerFrame::MirrorEvent(_) => "mirror.event",
            WorkerFrame::Ack(_) => "ack",
            WorkerFrame::Error(_) => "error",
        }
    }

    fn check_examples<T>(examples: &[(&str, &str)], spec: &[&str], type_of: fn(&T) -> &'static str)
    where
        T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let mut names: Vec<&str> = examples.iter().map(|(n, _)| *n).collect();
        names.dedup();
        assert_eq!(names, spec);
        for (name, src) in examples {
            let wire: Value = serde_json::from_str(src).unwrap();
            let keys: Vec<&String> = wire.as_object().unwrap().keys().collect();
            assert_eq!(keys, ["id", "payload", "type"], "{name}: envelope keys");
            let f: Envelope<T> =
                serde_json::from_value(wire.clone()).unwrap_or_else(|err| panic!("{name}: {err}"));
            assert_eq!(type_of(&f.body), *name);
            let back = serde_json::to_value(&f).unwrap();
            assert_eq!(back, wire, "{name}: wire form changed");
            assert_eq!(serde_json::from_value::<Envelope<T>>(back).unwrap(), f);
        }
    }

    #[test]
    fn hub_examples_roundtrip_exactly() {
        check_examples::<HubFrame>(HUB_EXAMPLES, SPEC_HUB_TYPES, hub_type);
    }

    #[test]
    fn worker_examples_roundtrip_exactly() {
        check_examples::<WorkerFrame>(WORKER_EXAMPLES, SPEC_WORKER_TYPES, worker_type);
    }

    #[test]
    fn frames_are_strict() {
        let unknown = json!({"id": "x", "type": "approval.cancel", "payload": {}});
        assert!(serde_json::from_value::<Envelope<HubFrame>>(unknown.clone()).is_err());
        assert!(serde_json::from_value::<Envelope<WorkerFrame>>(unknown).is_err());
    }

    #[test]
    fn reply_reuses_id() {
        let req = Envelope::new(
            "f-17",
            HubFrame::SessionStop {
                session_id: Uuid::nil(),
            },
        );
        let ack = req.reply(WorkerFrame::Ack(Ack::default()));
        assert_eq!(
            serde_json::to_value(&ack).unwrap(),
            json!({"id": "f-17", "type": "ack", "payload": {}})
        );
    }

    #[test]
    fn acks_answer_their_requests() {
        let input: Envelope<HubFrame> =
            serde_json::from_str(include_str!("../examples/wire/hub.session.input.json")).unwrap();
        let ack: Envelope<WorkerFrame> =
            serde_json::from_str(include_str!("../examples/wire/worker.ack.json")).unwrap();
        let HubFrame::SessionInput(si) = &input.body else {
            panic!("not session.input");
        };
        assert_eq!(ack.id, input.id);
        assert_eq!(
            ack.body,
            WorkerFrame::Ack(Ack {
                event_id: Some(si.event_id),
                seq: None
            })
        );

        let ev: Envelope<WorkerFrame> =
            serde_json::from_str(include_str!("../examples/wire/worker.event.json")).unwrap();
        let ack: Envelope<HubFrame> =
            serde_json::from_str(include_str!("../examples/wire/hub.ack.json")).unwrap();
        let WorkerFrame::Event { event } = &ev.body else {
            panic!("not an event frame");
        };
        assert_eq!(event.seq, None, "seq is assigned by the hub (SPEC §5.2)");
        assert_eq!(ack.id, ev.id);
        let HubFrame::Ack(a) = &ack.body else {
            panic!("not an ack");
        };
        assert_eq!(a.event_id, Some(event.event_id));
        assert!(a.seq.is_some());
    }

    #[test]
    fn results_answer_their_requests() {
        let pairs = [
            (
                include_str!("../examples/wire/hub.fs.stat.json"),
                include_str!("../examples/wire/worker.result.fs.stat.json"),
            ),
            (
                include_str!("../examples/wire/hub.fs.list_dirs.json"),
                include_str!("../examples/wire/worker.result.fs.list_dirs.json"),
            ),
            (
                include_str!("../examples/wire/hub.workspace.list_candidates.json"),
                include_str!("../examples/wire/worker.result.candidates.json"),
            ),
        ];
        for (req, res) in pairs {
            let req: Envelope<HubFrame> = serde_json::from_str(req).unwrap();
            let res: Envelope<WorkerFrame> = serde_json::from_str(res).unwrap();
            let (request_id, kind) = match &req.body {
                HubFrame::FsStat(r) => (&r.request_id, 0),
                HubFrame::FsListDirs(r) => (&r.request_id, 1),
                HubFrame::WorkspaceListCandidates(r) => (&r.request_id, 2),
                other => panic!("not a request: {other:?}"),
            };
            assert_eq!(&req.id, request_id, "request_id is the frame id");
            let WorkerFrame::Result(r) = &res.body else {
                panic!("not a result");
            };
            assert_eq!((&res.id, &r.request_id), (&req.id, &req.id));
            let got = match r.body {
                ResultBody::FsStat(_) => 0,
                ResultBody::FsListDirs(_) => 1,
                ResultBody::Candidates(_) => 2,
            };
            assert_eq!(got, kind);
        }
        // The roots are asked for with an explicit `null`.
        let roots = serde_json::to_value(HubFrame::FsListDirs(FsListDirs {
            request_id: "h-1".into(),
            path: None,
        }))
        .unwrap();
        assert!(roots["payload"].as_object().unwrap().contains_key("path"));
        assert_eq!(roots["payload"]["path"], Value::Null);
    }

    #[test]
    fn worker_approval_round_trip() {
        let req: Envelope<WorkerFrame> = serde_json::from_str(include_str!(
            "../examples/wire/worker.worker.approval.request.json"
        ))
        .unwrap();
        let res: Envelope<HubFrame> = serde_json::from_str(include_str!(
            "../examples/wire/hub.worker.approval.response.json"
        ))
        .unwrap();
        let WorkerFrame::WorkerApprovalRequest(r) = &req.body else {
            panic!("not worker.approval.request");
        };
        let HubFrame::WorkerApprovalResponse {
            request_id,
            option_id,
        } = &res.body
        else {
            panic!("not worker.approval.response");
        };
        assert_eq!(request_id, &r.request_id);
        assert!([worker_approval::ONCE, worker_approval::DENY].contains(&option_id.as_str()));
        assert_eq!(r.kind, worker_approval::WORKSPACE_USE);
    }

    #[test]
    fn t17_fields_are_optional() {
        // A hub before T17 sends no `system_session_id`; `requested_by_owner` is sent only
        // when true.
        let ack: Envelope<HubFrame> =
            serde_json::from_str(include_str!("../examples/wire/hub.hello.ack.json")).unwrap();
        assert!(matches!(
            ack.body,
            HubFrame::HelloAck {
                system_session_id: None,
                ..
            }
        ));
        let start: Envelope<HubFrame> =
            serde_json::from_str(include_str!("../examples/wire/hub.session.start.json")).unwrap();
        let HubFrame::SessionStart(mut s) = start.body else {
            panic!("not session.start");
        };
        assert!(!s.requested_by_owner);
        s.requested_by_owner = true;
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["requested_by_owner"], true);
        assert_eq!(serde_json::to_value(Runtime::None).unwrap(), "none");
    }

    #[test]
    fn null_seq_reads_as_absent() {
        let a: Ack = serde_json::from_value(json!({"event_id": Uuid::nil(), "seq": null})).unwrap();
        assert_eq!(a.seq, None);
    }

    /// `held_by` (T20): absent (no registry data), `null` (nobody) and a holder are three
    /// different things, and a frame of an older worker still parses.
    #[test]
    fn held_by_is_absent_null_or_a_holder() {
        let old: Envelope<WorkerFrame> =
            serde_json::from_str(include_str!("../examples/wire/worker.session.state.json"))
                .unwrap();
        let held: Envelope<WorkerFrame> = serde_json::from_str(include_str!(
            "../examples/wire/worker.session.state.held.json"
        ))
        .unwrap();
        let gone: Envelope<WorkerFrame> = serde_json::from_str(include_str!(
            "../examples/wire/worker.session.state.released.json"
        ))
        .unwrap();
        let of = |f: &Envelope<WorkerFrame>| match &f.body {
            WorkerFrame::SessionState { held_by, .. } => held_by.clone(),
            other => panic!("not session.state: {other:?}"),
        };
        assert_eq!(of(&old), None);
        assert_eq!(of(&gone), Some(None));
        assert_eq!(
            of(&held),
            Some(Some(HeldBy {
                entrypoint: "claude-vscode".into(),
                status: "idle".into()
            }))
        );
        let m: Envelope<WorkerFrame> =
            serde_json::from_str(include_str!("../examples/wire/worker.mirror.event.json"))
                .unwrap();
        let WorkerFrame::MirrorEvent(m) = m.body else {
            panic!("not mirror.event");
        };
        assert_eq!(m.held_by, None);
        let f: Envelope<WorkerFrame> = serde_json::from_str(include_str!(
            "../examples/wire/worker.mirror.event.forked.json"
        ))
        .unwrap();
        let WorkerFrame::MirrorEvent(f) = f.body else {
            panic!("not mirror.event");
        };
        let crate::event::EventKind::System { code, fork, .. } = &f.event.kind else {
            panic!("not system");
        };
        assert_eq!(code, crate::event::SESSION_FORKED);
        assert_eq!(fork.as_ref().unwrap().orphaned_turns, 1);
    }
}
