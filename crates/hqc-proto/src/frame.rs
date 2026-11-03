//! Worker <-> hub frames, SPEC §5. Every frame is `{id, type, payload}`; a reply
//! (`ack`, `hello.ack`, `error`) reuses the `id` of the frame it answers.
//!
//! Frames are strict (SPEC §5.2): the protocol version is agreed in `hello`, and a frame
//! that does not parse is answered with `error` [`error_code::INVALID_FRAME`].
//!
//! Typed here: the frames of iteration 1 (docs/ITERATION-1.md, T1) and the folder requests
//! `fs.stat` / `fs.list_dirs` with their `result` (T14), confirmations outside a session
//! `worker.approval.request` / `worker.approval.response` (T17), `title.request` with its
//! `result` (T24), the MCP calls `mcp.call` / `mcp.cancel` and `mcp.result` (T30), the
//! workspaces hqc creates and deletes `workspace.create` / `workspace.created`,
//! `workspace.inspect`, `workspace.delete` (T45). Later iterations: hub -> worker
//! `session.steer`, `model.proxy`.

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
    /// `mirror.event` of a new transcript over the worker's daily limit of imported sessions
    /// (SPEC §9.1, §20); retryable: the worker reads the transcript again later.
    pub const MIRROR_QUOTA: &str = "mirror_quota";
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
        /// Slash commands the runtime offers, without the `/` (Claude Code: `slash_commands`
        /// of `system/init`, SPEC §8.1). Absent — not known yet; the worker repeats the last
        /// list in every following `session.state`, like `title`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commands: Option<Vec<String>>,
        /// The permission mode the session's agent runs in (T38), as the worker decided it
        /// and the runtime confirmed it; repeated in every following `session.state`.
        /// Absent — not known yet.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_mode: Option<PermissionMode>,
        /// The input whose `permission_cap` the agent runs its current turn under (since
        /// SPEC 0.43, T39): the session's own mode is `auto`, the runtime confirmed `ask` for
        /// this turn. Absent — no cap applies now.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capped_input: Option<Uuid>,
    },
    /// Reply to a hub request (`fs.stat`, `fs.list_dirs`, `title.request`, ...): same `id` as
    /// the request.
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
    /// A tool call of an agent's hqc MCP server (SPEC §5.2, §7); answered with `mcp.result`.
    #[serde(rename = "mcp.call")]
    McpCall(McpCall),
    /// The runtime gave up a call (`notifications/cancelled`) or the MCP process exited with
    /// it open (SPEC §5.2, §7.2); answered with `ack {}`.
    #[serde(rename = "mcp.cancel")]
    McpCancel(McpCancel),
    /// The outcome of `workspace.create` (T45, SPEC §5.4); answered with `ack {}`. Sent when
    /// the clone ends and again whenever the hub repeats the `workspace.create`.
    #[serde(rename = "workspace.created")]
    WorkspaceCreated(WorkspaceCreated),
    #[serde(rename = "ack")]
    Ack(Ack),
    #[serde(rename = "error")]
    Error(WireError),
}

/// `mcp.call {request_id, session_token, tool, args}` (SPEC §5.2, §7). `request_id` is a
/// uuid the worker makes up per call; `session_token` is the one of the session's latest
/// `session.start`, which the hub checks together with the worker that runs the session. A
/// fast tool is answered at once with `mcp.result` under the same frame `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpCall {
    pub request_id: String,
    pub session_token: String,
    /// One of [`crate::mcp::TOOLS`].
    pub tool: String,
    /// The tool's arguments as the agent gave them, at most [`crate::mcp::MAX_ARGS`] bytes.
    pub args: serde_json::Value,
    /// The `event_id` of the input whose turn made the call, as the worker handed it to the
    /// runtime (since SPEC 0.39). The hub takes the call's chain from it when it is the
    /// session's input not older than its last delivered one (an `ack` lost in a reconnect
    /// leaves the hub a turn behind, SPEC §7.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_input: Option<Uuid>,
}

/// `mcp.cancel {request_id, reason}` (SPEC §5.2, §7.2): one of [`crate::mcp::cancel_reason`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpCancel {
    pub request_id: String,
    pub reason: String,
}

/// `mcp.result {request_id, result | error}` (SPEC §5.2, §5.3): exactly one of the two.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpResult {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<McpError>,
}

impl McpResult {
    pub fn ok(request_id: impl Into<String>, result: serde_json::Value) -> Self {
        Self {
            request_id: request_id.into(),
            result: Some(result),
            error: None,
        }
    }

    pub fn err(request_id: impl Into<String>, code: &str, message: impl Into<String>) -> Self {
        Self {
            request_id: request_id.into(),
            result: None,
            error: Some(McpError::new(code, message)),
        }
    }

    /// The result, or the error (also for a frame that carries neither).
    pub fn outcome(self) -> Result<serde_json::Value, McpError> {
        match (self.result, self.error) {
            (_, Some(e)) => Err(e),
            (Some(r), None) => Ok(r),
            (None, None) => Err(McpError::new(
                crate::mcp::error_code::INTERNAL,
                "mcp.result without result or error",
            )),
        }
    }
}

/// The one shape of every tool error (SPEC §7): `code` is one of
/// [`crate::mcp::error_code`], the list is open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpError {
    pub code: String,
    pub message: String,
}

impl McpError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
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
    /// Iteration 4 (SPEC §13.1, §13.2).
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
    /// The user switched the session's permission mode (T38): a running agent switches
    /// at once, a stopped one starts in it next time. Answered with `ack`; the mode the
    /// agent runs comes back in `session.state`.
    #[serde(rename = "session.permission_mode")]
    SessionPermissionMode {
        session_id: Uuid,
        permission_mode: PermissionMode,
    },
    #[serde(rename = "approval.response")]
    ApprovalResponse {
        session_id: Uuid,
        request_id: String,
        option_id: String,
        /// The user's text with a deny option (T26): the adapter gives it to the runtime as
        /// the reason of the denial.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
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
    /// Title a session on the worker's Claude Code subscription (T24, SPEC §3 Session, §5.3).
    #[serde(rename = "title.request")]
    TitleRequest(TitleRequest),
    /// hqc's allow rules of a workspace (T48, SPEC §5.3).
    #[serde(rename = "workspace.rules")]
    WorkspaceRules(WorkspaceRules),
    /// The rules of a workspace and a check, for the rules screen (T48, SPEC §5.3).
    #[serde(rename = "permissions.inspect")]
    PermissionsInspect(PermissionsInspect),
    /// The answer to `mcp.call` (SPEC §5.2): under the call's frame `id` for a tool answered
    /// at once; a deferred one (`hqc_ask`) comes as a frame of its own, which the worker acks.
    #[serde(rename = "mcp.result")]
    McpResult(McpResult),
    /// Make a new folder for a workspace (T45, SPEC §5.3, §13.1): acked at once, the outcome
    /// comes later as `workspace.created`.
    #[serde(rename = "workspace.create")]
    WorkspaceCreate(WorkspaceCreate),
    /// Uncommitted changes and unpushed commits of a folder, before it is deleted (T45).
    #[serde(rename = "workspace.inspect")]
    WorkspaceInspect(WorkspacePath),
    /// Delete a folder the worker created (T45, SPEC §13.2); a worktree with `git worktree
    /// remove` (T46).
    #[serde(rename = "workspace.delete")]
    WorkspaceDelete(WorkspaceDelete),
    #[serde(rename = "ack")]
    Ack(Ack),
    #[serde(rename = "error")]
    Error(WireError),
}

/// `workspace.create {request_id, kind, path, ...}` -> `ack`, then `workspace.created` (T45,
/// SPEC §5.3). `request_id` is the workspace's id, not the frame's: the hub repeats the frame
/// after every `hello` while the workspace is `creating`, and the worker answers a request it
/// knows with its outcome instead of starting it again. `path` is the new folder: the worker
/// checks it itself (inside `allowed_roots` after resolving its parent, not existing, not
/// overlapping a workspace it knows) and creates it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCreate {
    pub request_id: String,
    pub path: String,
    #[serde(flatten)]
    pub source: WorkspaceSource,
}

/// What a new workspace is made from (`kind`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkspaceSource {
    /// `git clone [--branch <branch>] -- <url> <path>` in the environment of `path`; `url` as
    /// [`check_clone_url`] allows it.
    Clone {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    /// `git worktree add` in `origin`, a git workspace the worker knows (T46): `branch` checked
    /// out at `path`, or with `new_branch` created there from the origin's `HEAD`. Runs in the
    /// environment of `path`.
    Worktree {
        /// The origin workspace's canonical path.
        origin: String,
        branch: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        new_branch: bool,
    },
}

/// `workspace.created {request_id, canonical | error}` (T45, SPEC §5.4): the folder is ready at
/// `canonical`, or it was not made (`error`, a message for the user; the worker removed what it
/// had created).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCreated {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `workspace.inspect {request_id, path}` -> `result` [`WorkspaceInspectResult`] (T45, SPEC
/// §5.3). `request_id` equals the frame `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePath {
    pub request_id: String,
    pub path: String,
}

/// `workspace.delete {request_id, path, force?}` -> `result` [`WorkspaceDeleteResult`] (T45,
/// SPEC §5.3). `request_id` equals the frame `id`. `force` (T46): the user confirmed the
/// uncommitted files, so a worktree is removed with `git worktree remove --force`; a clone's
/// folder is deleted either way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDelete {
    pub request_id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
}

/// Result of `workspace.inspect`: what deleting the folder would lose.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInspectResult {
    /// The folder is a git work tree (it has a `.git`); otherwise the lists are empty.
    pub git: bool,
    /// Paths with uncommitted changes, untracked ones included (`git status --porcelain`), at
    /// most [`MAX_DIRTY`].
    pub dirty: Vec<String>,
    /// All of them, also those beyond [`MAX_DIRTY`].
    pub dirty_count: u32,
    /// Local branches with commits that are on no remote branch, at most [`MAX_UNPUSHED`]; in
    /// a linked worktree (T46) only the branch it has checked out: the others are not its own.
    pub unpushed: Vec<Unpushed>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unpushed {
    pub branch: String,
    pub commits: u32,
}

/// Most dirty paths one `workspace.inspect` lists.
pub const MAX_DIRTY: usize = 200;
/// Most branches one `workspace.inspect` lists.
pub const MAX_UNPUSHED: usize = 100;

/// Result of `workspace.delete`: `removed: false` when the folder was gone already.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDeleteResult {
    pub removed: bool,
}

/// Longest clone URL.
pub const MAX_CLONE_URL: usize = 2048;

/// Where [`check_clone_url`] says a clone comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloneFrom {
    /// `https://`, `ssh://` or scp-like `[user@]host:path`.
    Remote,
    /// `file:///<path>`: the worker clones it only from inside its `allowed_roots`.
    File(String),
}

/// A clone URL hqc passes to `git clone` (T45, SPEC §5.3): `https://`, `ssh://`, scp-like
/// `[user@]host:path` and `file:///<absolute path>`. Refused: other schemes (`http`, `git`,
/// `ext::` and every `<transport>::` remote helper, which can run commands), plain local
/// paths, a leading `-`, a host starting with `-`, whitespace and control characters.
pub fn check_clone_url(url: &str) -> Result<CloneFrom, String> {
    if url.is_empty() || url.len() > MAX_CLONE_URL {
        return Err(format!("the URL must be 1..{MAX_CLONE_URL} bytes"));
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the URL contains whitespace or control characters".into());
    }
    if url.starts_with('-') {
        return Err("the URL starts with -".into());
    }
    let host_ok = |authority: &str| {
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        !host.is_empty() && !host.starts_with('-')
    };
    if let Some((scheme, rest)) = url.split_once("://") {
        return match scheme.to_ascii_lowercase().as_str() {
            "https" | "ssh" => {
                let authority = rest.split('/').next().unwrap_or("");
                if host_ok(authority) {
                    Ok(CloneFrom::Remote)
                } else {
                    Err("the URL has no host, or one starting with -".into())
                }
            }
            "file" if rest.starts_with('/') && !rest.contains('%') => {
                Ok(CloneFrom::File(rest.to_string()))
            }
            "file" => Err("a file URL is file:///<absolute path>, without %".into()),
            s => Err(format!(
                "{s}:// is not allowed: use https://, ssh:// or user@host:path"
            )),
        };
    }
    if url.contains("::") {
        return Err("<transport>::<address> URLs are not allowed".into());
    }
    // scp-like: a `:` before any `/`, as git reads it.
    match url.split_once(':') {
        Some((authority, path))
            if !authority.contains('/') && !path.is_empty() && host_ok(authority) =>
        {
            Ok(CloneFrom::Remote)
        }
        _ => Err("not a clone URL: use https://, ssh:// or user@host:path".into()),
    }
}

/// A branch name for `git clone --branch` (T45) and `git worktree add` (T46): letters, digits and `._/-`, not starting
/// with `-` or `/`, without `..` or `//`, not ending with `/` or `.lock`, at most 255 bytes.
pub fn check_branch(b: &str) -> Result<(), String> {
    let ok = (1..=255).contains(&b.len())
        && b.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'/' | b'-'))
        && !b.starts_with(['-', '/'])
        && !b.ends_with('/')
        && !b.ends_with(".lock")
        && !b.contains("..")
        && !b.contains("//");
    if ok {
        Ok(())
    } else {
        Err(format!("{b:?} is not a branch name"))
    }
}

/// `title.request {request_id, session_id, user_text, agent_text}` -> `result {request_id,
/// title}` [`TitleResult`] or `error` (T24, SPEC §3 Session, §5.3): the first prompt and the
/// first final answer of the session, each cut by the hub to [`MAX_TITLE_INPUT`] characters.
/// `request_id` equals the frame `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TitleRequest {
    pub request_id: String,
    pub session_id: Uuid,
    pub user_text: String,
    pub agent_text: String,
}

/// Result of `title.request`: one line of at most [`MAX_TITLE`] characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TitleResult {
    pub title: String,
}

/// Longest `title.request.user_text` / `agent_text`, in characters.
pub const MAX_TITLE_INPUT: usize = 2000;
/// Longest title the hub derives or the worker returns, in characters.
pub const MAX_TITLE: usize = 60;

/// `fs.stat {request_id, path}` -> `result` [`FsStatResult`]. `request_id` equals the frame
/// `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsStat {
    pub request_id: String,
    /// Absolute path on the worker.
    pub path: String,
    /// Make the folder, with its missing parents, when it is missing and would be inside
    /// `allowed_roots` (since SPEC 0.64: the user asked for it); the result is then the made
    /// folder's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub create: bool,
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
    Title(TitleResult),
    Inspect(InspectResult),
    WorkspaceInspect(WorkspaceInspectResult),
    WorkspaceDelete(WorkspaceDeleteResult),
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
    /// The environments of `worker.toml` (since SPEC 0.56, T43), without the values of their
    /// `env`; `default` is implied. A display for the hub: the worker resolves a session's
    /// environment itself.
    #[serde(default)]
    pub environments: Vec<EnvironmentInfo>,
    #[serde(default)]
    pub env_bindings: Vec<EnvBinding>,
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

/// The environment of a folder with no binding (SPEC §14).
pub const DEFAULT_ENVIRONMENT: &str = "default";

/// An environment of the worker's `worker.toml` as `hello` reports it (SPEC §5.1, §14): its
/// name, whether local MCP servers are allowed and its permission rules; never its `env`.
/// A plain name (`"default"`, as workers before 0.56 send) reads as an environment without
/// rules.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "EnvironmentWire")]
pub struct EnvironmentInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_local_mcp: bool,
    #[serde(default)]
    pub permissions: EnvPermissions,
}

/// `[environments.<name>.permissions]` (SPEC §14).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvPermissions {
    /// Rules of the worker's `deny` lifted for this environment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unblock: Vec<String>,
    /// Added to the worker's `allow`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// Added to the worker's `deny`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EnvironmentWire {
    Name(String),
    Full {
        name: String,
        #[serde(default)]
        allow_local_mcp: bool,
        #[serde(default)]
        permissions: EnvPermissions,
    },
}

impl From<EnvironmentWire> for EnvironmentInfo {
    fn from(w: EnvironmentWire) -> Self {
        match w {
            EnvironmentWire::Name(name) => Self {
                name,
                ..Self::default()
            },
            EnvironmentWire::Full {
                name,
                allow_local_mcp,
                permissions,
            } => Self {
                name,
                allow_local_mcp,
                permissions,
            },
        }
    }
}

/// `[[env_bindings]]` (SPEC §14): the folders of an environment. `path` is canonical on the
/// worker when it existed at start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvBinding {
    pub path: String,
    pub environment: String,
}

/// The environment of a workspace path (SPEC §14): the binding with the longest path that
/// equals or contains it, compared by path components (`/a/b` does not contain `/a/bc`);
/// [`DEFAULT_ENVIRONMENT`] when none does. The worker decides with this for its sessions, the
/// hub only for display.
pub fn environment_for<'a>(bindings: &'a [EnvBinding], path: &str) -> &'a str {
    binding_for(bindings, path).map_or(DEFAULT_ENVIRONMENT, |b| b.environment.as_str())
}

/// The binding [`environment_for`] goes by.
pub fn binding_for<'a>(bindings: &'a [EnvBinding], path: &str) -> Option<&'a EnvBinding> {
    let path = std::path::Path::new(path);
    bindings
        .iter()
        .filter(|b| path.starts_with(&b.path))
        .max_by_key(|b| std::path::Path::new(&b.path).components().count())
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

/// A session's permission mode (SPEC §3 Session, §8.1, T38): what the runtime does with a
/// tool call no rule of the worker's `[permissions]` decides. The worker's `deny` stops a
/// call in both.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Ask the worker owner (`approval.request`).
    #[default]
    Ask,
    /// Claude Code's classifier decides and never asks (`--permission-mode auto`).
    Auto,
}

impl PermissionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Auto => "auto",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ask" => Some(Self::Ask),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
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
/// ("Session N"), `runtime` — the agent (`session.state.title`), `user` — `PATCH`. The hub
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
    /// The mode hqc asks for (T38); absent: the worker's `[permissions] mode`. The worker
    /// may run `ask` instead (`allow_auto = false`) and reports what it runs in
    /// `session.state.permission_mode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    /// hqc's allow rules of the session's workspace (since SPEC 0.52, T48), as
    /// [`crate::rules`] writes them; later changes come as `workspace.rules`. Absent when
    /// empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_rules: Vec<String>,
}

/// `workspace.rules {workspace_path, allow_rules[]}` -> `ack` (since SPEC 0.52, T48): the
/// whole current list of hqc's allow rules of a workspace. The hub sends it when the list
/// changes and, for every workspace of the worker, after `hello`; running sessions in the
/// folder use it at their next permission check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRules {
    pub workspace_path: String,
    #[serde(default)]
    pub allow_rules: Vec<String>,
}

/// `permissions.inspect {request_id, path, command?, tool?}` -> `result` [`InspectResult`]
/// (since SPEC 0.52, T48): the rules that decide tool calls in a workspace, and optionally
/// how the worker would decide one command or tool. `request_id` equals the frame `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionsInspect {
    pub request_id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

/// Where a rule comes from (`InspectResult.sources[].source`, `CheckResult.source`).
pub mod rule_source {
    pub const WORKER: &str = "worker.toml";
    pub const USER: &str = "~/.claude/settings.json";
    pub const LOCAL: &str = ".claude/settings.local.json";
    pub const HQC: &str = "hqc";

    /// The rules of an environment (since SPEC 0.56, T43): `worker.toml [environments.infra]`.
    pub fn environment(name: &str) -> String {
        format!("{WORKER} [environments.{name}]")
    }
}

/// Result of `permissions.inspect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectResult {
    /// `[permissions] mode` of the worker: the mode of a session that asks for none.
    pub mode: PermissionMode,
    pub allow_auto: bool,
    /// `worker.toml`, `~/.claude/settings.json`, `.claude/settings.local.json` of the folder,
    /// in this order; a file that is absent has empty lists.
    pub sources: Vec<RuleSource>,
    /// hqc's rules the worker holds for the folder (the last `workspace.rules`).
    #[serde(default)]
    pub hqc: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckResult>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSource {
    /// One of [`rule_source`].
    pub source: String,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
    /// The file could not be read or parsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Not run, not asked about.
    Deny,
    /// Run without asking.
    Allow,
    /// Asked about in `ask`, the classifier decides in `auto`.
    Ask,
}

/// How the worker's matcher decides a command or tool in the folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub verdict: Verdict,
    /// The deciding rule and its source: the deny rule, or the allow rule of a command of one
    /// part. Absent for `ask` and for a compound command every part of which is allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Simple parts of the command (1 for a tool); 0: the matcher could not split it.
    pub parts: u32,
    /// Parts no allow rule covers.
    #[serde(default)]
    pub uncovered: u32,
    /// hqc rules for the uncovered parts, with `ask`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<crate::event::RuleSuggestion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInput {
    pub session_id: Uuid,
    /// `event_id` of the `user.message` / `peer.message` being delivered.
    pub event_id: Uuid,
    pub text: String,
    #[serde(default)]
    pub artifact_ids: Vec<Uuid>,
    /// Buffered `send` messages (SPEC §7.1).
    #[serde(default)]
    pub peer_prefix: Vec<PeerPrefix>,
    /// For a `peer.message`: who sent it and how, for the header the worker puts before
    /// `text` (SPEC §7.1, since 0.34); absent for a `user.message`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<PeerHeader>,
    /// `permission_cap` of the input's chain (since SPEC 0.43, T39): with `ask`, a session
    /// running in `auto` runs this turn in `ask`. The worker only ever lowers the mode for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_cap: Option<PermissionMode>,
}

/// `session.input.peer` (SPEC §5.3, §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerHeader {
    /// `<project-slug>/<session-slug>` of the sender.
    pub from: String,
    pub mode: crate::event::PeerMode,
    /// Only for `mode: ask`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// One buffered `send` message in `session.input.peer_prefix` (SPEC §5.3, §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerPrefix {
    /// `<project-slug>/<session-slug>` of the sender.
    pub from: String,
    pub text: String,
    pub ts: DateTime<Utc>,
    /// The message's `event_id` (since 0.46, T41): its files go to `.hqc/inbox/<event_id>/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<Uuid>,
    /// The message's files (since 0.46, T41), downloaded like the input's own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_ids: Vec<Uuid>,
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
        "session.permission_mode",
        "approval.response",
        "clarify.response",
        "fs.stat",
        "fs.list_dirs",
        "worker.approval.response",
        "workspace.list_candidates",
        "mirror.set",
        "session.cleanup",
        "title.request",
        "workspace.rules",
        "permissions.inspect",
        "mcp.result",
        "workspace.create",
        "workspace.inspect",
        "workspace.delete",
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
        "mcp.call",
        "mcp.cancel",
        "workspace.created",
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
            "session.input",
            include_str!("../examples/wire/hub.session.input.peer.json"),
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
            "session.permission_mode",
            include_str!("../examples/wire/hub.session.permission_mode.json"),
        ),
        (
            "approval.response",
            include_str!("../examples/wire/hub.approval.response.json"),
        ),
        (
            "approval.response",
            include_str!("../examples/wire/hub.approval.response.message.json"),
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
        (
            "title.request",
            include_str!("../examples/wire/hub.title.request.json"),
        ),
        (
            "workspace.rules",
            include_str!("../examples/wire/hub.workspace.rules.json"),
        ),
        (
            "permissions.inspect",
            include_str!("../examples/wire/hub.permissions.inspect.json"),
        ),
        (
            "mcp.result",
            include_str!("../examples/wire/hub.mcp.result.json"),
        ),
        (
            "mcp.result",
            include_str!("../examples/wire/hub.mcp.result.error.json"),
        ),
        (
            "workspace.create",
            include_str!("../examples/wire/hub.workspace.create.json"),
        ),
        (
            "workspace.create",
            include_str!("../examples/wire/hub.workspace.create.worktree.json"),
        ),
        (
            "workspace.inspect",
            include_str!("../examples/wire/hub.workspace.inspect.json"),
        ),
        (
            "workspace.delete",
            include_str!("../examples/wire/hub.workspace.delete.json"),
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
            "session.state",
            include_str!("../examples/wire/worker.session.state.capped.json"),
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
            "result",
            include_str!("../examples/wire/worker.result.title.json"),
        ),
        (
            "result",
            include_str!("../examples/wire/worker.result.permissions.inspect.json"),
        ),
        (
            "result",
            include_str!("../examples/wire/worker.result.workspace.inspect.json"),
        ),
        (
            "result",
            include_str!("../examples/wire/worker.result.workspace.delete.json"),
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
        (
            "mcp.call",
            include_str!("../examples/wire/worker.mcp.call.json"),
        ),
        (
            "mcp.cancel",
            include_str!("../examples/wire/worker.mcp.cancel.json"),
        ),
        (
            "workspace.created",
            include_str!("../examples/wire/worker.workspace.created.json"),
        ),
        (
            "workspace.created",
            include_str!("../examples/wire/worker.workspace.created.error.json"),
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
            HubFrame::SessionPermissionMode { .. } => "session.permission_mode",
            HubFrame::ApprovalResponse { .. } => "approval.response",
            HubFrame::ClarifyResponse { .. } => "clarify.response",
            HubFrame::FsStat(_) => "fs.stat",
            HubFrame::FsListDirs(_) => "fs.list_dirs",
            HubFrame::WorkerApprovalResponse { .. } => "worker.approval.response",
            HubFrame::WorkspaceListCandidates(_) => "workspace.list_candidates",
            HubFrame::MirrorSet(_) => "mirror.set",
            HubFrame::SessionCleanup(_) => "session.cleanup",
            HubFrame::TitleRequest(_) => "title.request",
            HubFrame::WorkspaceRules(_) => "workspace.rules",
            HubFrame::PermissionsInspect(_) => "permissions.inspect",
            HubFrame::McpResult(_) => "mcp.result",
            HubFrame::WorkspaceCreate(_) => "workspace.create",
            HubFrame::WorkspaceInspect(_) => "workspace.inspect",
            HubFrame::WorkspaceDelete(_) => "workspace.delete",
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
            WorkerFrame::McpCall(_) => "mcp.call",
            WorkerFrame::McpCancel(_) => "mcp.cancel",
            WorkerFrame::WorkspaceCreated(_) => "workspace.created",
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

    /// T43: the longest binding wins, by path components; no binding is `default`.
    #[test]
    fn environment_lookup() {
        let b = |path: &str, environment: &str| EnvBinding {
            path: path.into(),
            environment: environment.into(),
        };
        let bindings = [b("/a/b", "infra"), b("/a", "lab"), b("/a/b/pub", "default")];
        let env = |p: &str| environment_for(&bindings, p);
        assert_eq!(env("/a/b"), "infra");
        assert_eq!(env("/a/b/c"), "infra");
        assert_eq!(env("/a/bc"), "lab", "/a/b does not contain /a/bc");
        assert_eq!(env("/a"), "lab");
        assert_eq!(
            env("/a/b/pub/x"),
            "default",
            "a binding may give a subfolder back"
        );
        assert_eq!(env("/ab"), DEFAULT_ENVIRONMENT);
        assert_eq!(environment_for(&[], "/a"), DEFAULT_ENVIRONMENT);
    }

    /// Workers before 0.56 send environment names only.
    #[test]
    fn hello_environments_of_older_workers() {
        let e: Vec<EnvironmentInfo> = serde_json::from_value(json!(["default"])).unwrap();
        assert_eq!(
            e,
            [EnvironmentInfo {
                name: "default".into(),
                ..EnvironmentInfo::default()
            }]
        );
    }

    /// T45: the clone frame carries its kind, and its outcome is a path or an error.
    #[test]
    fn workspace_create_and_created() {
        let c: Envelope<HubFrame> =
            serde_json::from_str(include_str!("../examples/wire/hub.workspace.create.json"))
                .unwrap();
        let HubFrame::WorkspaceCreate(c) = c.body else {
            panic!("not workspace.create");
        };
        let WorkspaceSource::Clone { url, branch } = &c.source else {
            panic!("not a clone");
        };
        assert_eq!(check_clone_url(url), Ok(CloneFrom::Remote));
        assert!(check_branch(branch.as_deref().unwrap()).is_ok());
        // T46: a worktree names its origin by path.
        let w: Envelope<HubFrame> = serde_json::from_str(include_str!(
            "../examples/wire/hub.workspace.create.worktree.json"
        ))
        .unwrap();
        let HubFrame::WorkspaceCreate(w) = w.body else {
            panic!("not workspace.create");
        };
        let WorkspaceSource::Worktree {
            origin,
            branch,
            new_branch,
        } = &w.source
        else {
            panic!("not a worktree");
        };
        assert!(check_branch(branch).is_ok() && *new_branch);
        assert!(w.path.starts_with(&format!("{origin}-")));
        let d: Envelope<HubFrame> =
            serde_json::from_str(include_str!("../examples/wire/hub.workspace.delete.json"))
                .unwrap();
        let HubFrame::WorkspaceDelete(d) = d.body else {
            panic!("not workspace.delete");
        };
        assert!(d.force);
        let plain: WorkspaceDelete =
            serde_json::from_value(json!({"request_id": "h-1", "path": "/a"})).unwrap();
        assert!(!plain.force, "force is off unless the hub says so");
        for (src, ok) in [
            (
                include_str!("../examples/wire/worker.workspace.created.json"),
                true,
            ),
            (
                include_str!("../examples/wire/worker.workspace.created.error.json"),
                false,
            ),
        ] {
            let f: Envelope<WorkerFrame> = serde_json::from_str(src).unwrap();
            let WorkerFrame::WorkspaceCreated(r) = f.body else {
                panic!("not workspace.created");
            };
            assert_eq!(r.request_id, c.request_id);
            assert_eq!((r.canonical.is_some(), r.error.is_none()), (ok, ok));
        }
    }

    #[test]
    fn clone_urls() {
        for ok in [
            "https://github.com/drzlo/hqc.git",
            "HTTPS://github.com/drzlo/hqc",
            "https://user:token@git.example.com/a/b.git",
            "ssh://git@github.com/drzlo/hqc.git",
            "ssh://git@[::1]:2222/x.git",
            "git@github.com:drzlo/hqc.git",
            "github.com:drzlo/hqc.git",
        ] {
            assert_eq!(check_clone_url(ok), Ok(CloneFrom::Remote), "{ok}");
        }
        assert_eq!(
            check_clone_url("file:///Users/me/work/repos/a.git"),
            Ok(CloneFrom::File("/Users/me/work/repos/a.git".into()))
        );
        for bad in [
            "",
            "ext::sh -c touch% /tmp/pwned",
            "ext::sh",
            "fd::7",
            "http://github.com/a/b",
            "git://github.com/a/b",
            "ftp://h/a",
            "/Users/me/work/repo",
            "./repo",
            "../repo",
            "repo",
            "-uhttps://x/y",
            "ssh://-oProxyCommand=touch/x",
            "ssh://git@-oProxyCommand=x/y",
            "-oProxyCommand=x:path",
            "git@-oProxy:path",
            "git@host:",
            "file://relative/path",
            "file:///a%2e%2e/b",
            "https://",
            "https://host/a b",
            "https://host/a\nb",
            "a/b:c",
        ] {
            assert!(check_clone_url(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(check_clone_url(&format!("https://h/{}", "a".repeat(MAX_CLONE_URL))).is_err());
    }

    #[test]
    fn branch_names() {
        for ok in ["main", "hqc/fix-backlight", "v1.2.3", "release_2026"] {
            assert!(check_branch(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "-b",
            "--upload-pack=x",
            "/a",
            "a/",
            "a..b",
            "a//b",
            "a.lock",
            "a b",
            "a~1",
            "a:b",
        ] {
            assert!(check_branch(bad).is_err(), "{bad:?}");
        }
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
            (
                include_str!("../examples/wire/hub.title.request.json"),
                include_str!("../examples/wire/worker.result.title.json"),
            ),
            (
                include_str!("../examples/wire/hub.permissions.inspect.json"),
                include_str!("../examples/wire/worker.result.permissions.inspect.json"),
            ),
            (
                include_str!("../examples/wire/hub.workspace.inspect.json"),
                include_str!("../examples/wire/worker.result.workspace.inspect.json"),
            ),
            (
                include_str!("../examples/wire/hub.workspace.delete.json"),
                include_str!("../examples/wire/worker.result.workspace.delete.json"),
            ),
        ];
        for (req, res) in pairs {
            let req: Envelope<HubFrame> = serde_json::from_str(req).unwrap();
            let res: Envelope<WorkerFrame> = serde_json::from_str(res).unwrap();
            let (request_id, kind) = match &req.body {
                HubFrame::FsStat(r) => (&r.request_id, 0),
                HubFrame::FsListDirs(r) => (&r.request_id, 1),
                HubFrame::WorkspaceListCandidates(r) => (&r.request_id, 2),
                HubFrame::TitleRequest(r) => (&r.request_id, 3),
                HubFrame::PermissionsInspect(r) => (&r.request_id, 4),
                HubFrame::WorkspaceInspect(r) => (&r.request_id, 5),
                HubFrame::WorkspaceDelete(r) => (&r.request_id, 6),
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
                ResultBody::Title(_) => 3,
                ResultBody::Inspect(_) => 4,
                ResultBody::WorkspaceInspect(_) => 5,
                ResultBody::WorkspaceDelete(_) => 6,
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

    /// T30: a call and its answer share the frame `id` and `request_id`; an error has the one
    /// shape `{code, message}`; the call names a tool of the list.
    #[test]
    fn mcp_call_and_result() {
        let call: Envelope<WorkerFrame> =
            serde_json::from_str(include_str!("../examples/wire/worker.mcp.call.json")).unwrap();
        let WorkerFrame::McpCall(c) = &call.body else {
            panic!("not mcp.call");
        };
        assert!(crate::mcp::TOOLS.iter().any(|t| t.name == c.tool));
        assert!(c.request_id.parse::<Uuid>().is_ok());
        for (src, ok) in [
            (include_str!("../examples/wire/hub.mcp.result.json"), true),
            (
                include_str!("../examples/wire/hub.mcp.result.error.json"),
                false,
            ),
        ] {
            let res: Envelope<HubFrame> = serde_json::from_str(src).unwrap();
            let HubFrame::McpResult(r) = res.body else {
                panic!("not mcp.result");
            };
            assert_eq!((&res.id, &r.request_id), (&call.id, &c.request_id));
            assert_eq!(r.outcome().is_ok(), ok);
        }
        let neither = McpResult {
            request_id: "x".into(),
            result: None,
            error: None,
        };
        assert_eq!(
            neither.outcome().unwrap_err().code,
            crate::mcp::error_code::INTERNAL
        );
        let cancel: Envelope<WorkerFrame> =
            serde_json::from_str(include_str!("../examples/wire/worker.mcp.cancel.json")).unwrap();
        let WorkerFrame::McpCancel(x) = cancel.body else {
            panic!("not mcp.cancel");
        };
        assert!(crate::mcp::cancel_reason::ALL.contains(&x.reason.as_str()));
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
        // `commands` (T27): additive, absent in older frames.
        let commands = |f: &Envelope<WorkerFrame>| match &f.body {
            WorkerFrame::SessionState { commands, .. } => commands.clone(),
            other => panic!("not session.state: {other:?}"),
        };
        assert_eq!(commands(&old).unwrap()[0], "compact");
        assert_eq!(commands(&held), None);
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
