//! HTTP bodies shared by the hub, the worker and the client (SPEC §5.1, §6).
//!
//! Requests ignore unknown fields, so an older hub accepts a newer worker's body.
//! Wire examples: `examples/wire/api.*.json`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::client_frame::SessionSummary;
use crate::event::SessionState;
use crate::frame::{HeldBy, LockedBy, ProcessState, Runtime, TitleSource};

/// A session as `GET /api/sessions/{id}`, the session list and `POST .../sessions` return
/// it (SPEC §3 Session, §6). `state`, `queue_len`, `last_read_seq` and `unread` are computed
/// by the hub for the requesting user (SPEC §17.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub project_id: Uuid,
    pub owner_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<Uuid>,
    pub cwd: String,
    pub title: String,
    /// Who set `title`; absent from older hubs, read as `default`.
    #[serde(default)]
    pub title_source: TitleSource,
    pub slug: String,
    pub agent: Agent,
    pub auto_approve: bool,
    pub wake_on_send: bool,
    pub locked_by: LockedBy,
    pub last_seq: u64,
    pub archived: bool,
    pub state: SessionState,
    pub queue_len: u64,
    pub last_read_seq: u64,
    /// Same rule as [`SessionSummary::unread`].
    pub unread: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The service session of worker `worker_id` (SPEC §10.2): no agent (`runtime: none`),
    /// no input, not archived; the client lists it under «Воркеры». Sent only when true.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub system: bool,
    /// A live process outside hqc holds the transcript open (SPEC §9.3): the session is
    /// `locked_vscode` and cannot be taken over until it is closed there. Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_by: Option<HeldBy>,
}

impl Session {
    /// The line of `summary` / `session.summary` (SPEC §6).
    pub fn summary(&self) -> SessionSummary {
        SessionSummary {
            session_id: self.id,
            project_id: self.project_id,
            title: self.title.clone(),
            state: self.state,
            queue_len: self.queue_len,
            unread: self.unread,
            last_seq: self.last_seq,
            last_read_seq: self.last_read_seq,
            held_by: self.held_by.clone(),
        }
    }
}

/// The agent embedded in a session (SPEC §3 Agent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub runtime: Runtime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_session_id: Option<String>,
    pub process_state: ProcessState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
    pub restricted: bool,
    pub start_failures: u32,
}

/// `GET /api/projects/{id}/sessions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionList {
    pub sessions: Vec<Session>,
}

/// `POST /api/projects/{id}/sessions`. Without `workspace_id` the project's
/// `default_workspace_id`; with only `worker_id` a workspace of the project on that worker
/// (SPEC §6, §13.4). `cwd` is relative to the workspace, `.` by default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub auto_approve: bool,
}

/// A project (SPEC §3 Project, §6): `GET /api/projects/{id}`, an element of the list, the
/// reply to `POST` and `PATCH`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    /// Unique per owner, kept on rename.
    pub slug: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_workspace_id: Option<Uuid>,
    /// The user's implicit project: neither deleted nor renamed.
    pub is_personal: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `GET /api/projects`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectList {
    pub projects: Vec<Project>,
}

/// `POST /api/projects`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateProject {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `PATCH /api/projects/{id}`: only the fields present change. `default_workspace_id` must
/// be a workspace of this project.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PatchProject {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_workspace_id: Option<Uuid>,
}

/// A workspace (SPEC §3 Workspace): a folder on a worker that belongs to one project.
/// `path` is the canonical path the worker reported in `fs.stat`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: Uuid,
    pub project_id: Uuid,
    pub worker_id: Uuid,
    pub path: String,
    #[serde(default)]
    pub label: String,
    pub environment: String,
    /// `dir` | `clone` | `worktree`; only `dir` for now.
    pub kind: String,
    pub ephemeral: bool,
    pub created_by_hqc: bool,
    pub mirror_enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `GET /api/workspaces?project_id=`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceList {
    pub workspaces: Vec<Workspace>,
}

/// `POST /api/workspaces`: an existing folder (`kind: dir`). The hub checks it with
/// `fs.stat` on the worker, which must be online (`409 worker_offline`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateWorkspace {
    pub project_id: Uuid,
    pub worker_id: Uuid,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// `PATCH /api/workspaces/{id}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PatchWorkspace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// `409 workspace_conflict`: on one worker a folder belongs to one project, so the new path
/// may not equal, lie inside or contain an existing workspace (SPEC §3 Workspace).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceConflict {
    /// `workspace_conflict`.
    pub error: String,
    pub message: String,
    pub existing_workspace_id: Uuid,
    pub existing_project_id: Uuid,
    pub existing_path: String,
}

/// `GET /api/workers/{id}/dirs?path=`: the worker's `fs.list_dirs` (SPEC §5.3, §6).
pub type DirList = crate::frame::FsListDirsResult;

/// `GET /api/workers/{id}/candidates`: the worker's `workspace.list_candidates` (SPEC §9.1).
pub type CandidateList = crate::frame::CandidatesResult;

/// `POST /api/workspaces` as the hub reads it (SPEC §9.1): [`CreateWorkspace`] plus
/// `mirror_enabled`, which the import from Claude Code sets. With it the hub sends the worker
/// `mirror.set` and the worker imports the folder's transcripts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateWorkspaceImport {
    #[serde(flatten)]
    pub workspace: CreateWorkspace,
    #[serde(default)]
    pub mirror_enabled: bool,
}

/// `GET /api/sessions/{id}/events`: stored events as raw JSON, oldest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventPage {
    pub events: Vec<serde_json::Value>,
    pub last_seq: u64,
}

/// `POST /api/sessions/{id}/input`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputRequest {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub artifact_ids: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_op_id: Option<String>,
}

/// Reply to `POST /api/sessions/{id}/input`: the stored `user.message` (201), or for a
/// repeated `client_op_id` the event stored the first time (200).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputResponse {
    pub event: serde_json::Value,
}

/// An artifact as `POST /api/artifacts` and `GET /api/artifacts/{id}` return it (SPEC §3
/// Artifact, §6, §12). Events carry only `id`; the content is fetched by a signed link.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: Uuid,
    pub kind: crate::event::ArtifactKind,
    pub name: String,
    pub mime: String,
    pub size: u64,
    /// Hex sha256 of the content.
    pub sha256: String,
    pub created_at: DateTime<Utc>,
}

/// `GET /api/artifacts/{id}/url`: a signed link valid until `expires_at` (SPEC §12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactUrl {
    pub url: String,
    pub expires_at: DateTime<Utc>,
}

/// Error body of every REST endpoint, with the matching HTTP status (SPEC §6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
    pub message: String,
}

/// `GET /api/push/vapid`; `null` when the hub has no VAPID keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VapidKey {
    pub public_key: Option<String>,
}

/// `POST /api/push/subscribe`: the browser's `PushSubscription.toJSON()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushSubscription {
    pub endpoint: String,
    #[serde(
        default,
        rename = "expirationTime",
        skip_serializing_if = "Option::is_none"
    )]
    pub expiration_time: Option<f64>,
    pub keys: PushKeys,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushKeys {
    pub p256dh: String,
    pub auth: String,
}

/// The Web Push message body (SPEC §16). `payload` is the event of SPEC §4 as stored; the
/// hub leaves it out when the message would not fit a push (about 4 KB). The service worker
/// derives `title` / `body` from `type` and `payload` when they are absent and shows
/// `approval.request` / `clarify.request` with `requireInteraction`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushMessage {
    pub session_id: Uuid,
    /// The event type.
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Notifications with the same tag replace each other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

/// `POST /api/workers/enroll/confirm`; case and the dash of `code` do not matter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollConfirmRequest {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// "The same machine": the user's offline or revoked worker that the enrolling one
    /// replaces, keeping its id, sessions and workspaces (SPEC §5.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_worker_id: Option<Uuid>,
}

/// `GET /api/workers/enroll/{code}`: what the worker that printed an unconfirmed code said
/// about itself, shown before the user confirms (SPEC §5.1, §6). `country` is Cloudflare's
/// `CF-IPCountry` of the enrolling request, when there was one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollInfo {
    pub name: String,
    pub os: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// The user's workers the enrolling one may replace: offline or revoked (SPEC §5.1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaceable: Vec<ReplaceableWorker>,
}

/// A worker of [`EnrollInfo::replaceable`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplaceableWorker {
    pub id: Uuid,
    pub name: String,
    pub os: String,
    /// `offline` | `revoked`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<DateTime<Utc>>,
    /// Sessions on the worker that are neither archived nor its service session.
    pub sessions_count: i64,
}

/// Reply to `POST /api/workers/enroll/confirm`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollConfirmed {
    pub worker_id: Uuid,
    pub name: String,
    /// The name the replaced worker had (with `replace_worker_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced: Option<String>,
}

/// `POST /worker/enroll` (SPEC §5.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollRequest {
    pub name: String,
    #[serde(default)]
    pub os: String,
    /// The worker's [`crate::PROTOCOL_VERSION`]; informational, `hello` decides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u32>,
}

/// Reply to `POST /worker/enroll`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollStarted {
    /// `XXXX-XXXX`, shown to the user; confirmed with `POST /api/workers/enroll/confirm`.
    pub code: String,
    /// The worker's secret for polling; the hub keeps only its hash.
    pub poll_token: String,
    pub expires_at: DateTime<Utc>,
    pub poll_interval_s: u32,
}

/// `POST /worker/enroll/poll`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollPollRequest {
    pub poll_token: String,
}

/// Reply to `POST /worker/enroll/poll`; the token is handed out exactly once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EnrollPoll {
    Pending,
    Confirmed {
        worker_id: Uuid,
        token: String,
        /// The name of the worker this enrollment replaced (SPEC §5.1); its id is `worker_id`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replaced: Option<String>,
    },
    Expired,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use serde_json::Value;

    fn roundtrip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(src: &str) -> T {
        let wire: Value = serde_json::from_str(src).unwrap();
        let v: T = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&v).unwrap(), wire, "wire form changed");
        v
    }

    #[test]
    fn enroll_examples_roundtrip() {
        let r: EnrollRequest = roundtrip(include_str!("../examples/wire/api.enroll.request.json"));
        assert_eq!(r.protocol_version, Some(crate::PROTOCOL_VERSION));
        roundtrip::<EnrollStarted>(include_str!("../examples/wire/api.enroll.json"));
        roundtrip::<EnrollPollRequest>(include_str!(
            "../examples/wire/api.enroll.poll.request.json"
        ));
        let p: EnrollPoll = roundtrip(include_str!("../examples/wire/api.enroll.poll.json"));
        assert!(matches!(p, EnrollPoll::Confirmed { replaced: None, .. }));
        let p: EnrollPoll = roundtrip(include_str!(
            "../examples/wire/api.enroll.poll.replace.json"
        ));
        assert!(matches!(
            p,
            EnrollPoll::Confirmed {
                replaced: Some(_),
                ..
            }
        ));
        let r: EnrollConfirmRequest = roundtrip(include_str!(
            "../examples/wire/api.enroll.confirm.replace.request.json"
        ));
        assert!(r.replace_worker_id.is_some());
        let r: EnrollInfo = roundtrip(include_str!(
            "../examples/wire/api.enroll.info.replace.json"
        ));
        assert_eq!(r.replaceable.len(), 2);
        let r: EnrollConfirmed = roundtrip(include_str!(
            "../examples/wire/api.enroll.confirm.replace.json"
        ));
        assert!(r.replaced.is_some());
        assert_eq!(
            serde_json::from_str::<EnrollPoll>(r#"{"status":"pending"}"#).unwrap(),
            EnrollPoll::Pending
        );
    }

    #[test]
    fn rest_examples_roundtrip() {
        let s: Session = roundtrip(include_str!("../examples/wire/api.session.json"));
        let summary: SessionSummary = serde_json::from_value(
            serde_json::from_str::<Value>(include_str!(
                "../examples/wire/client.session.summary.json"
            ))
            .unwrap()["session"]
                .clone(),
        )
        .unwrap();
        assert_eq!(
            s.summary(),
            summary,
            "summary is the session's compact form"
        );
        let held: Session = roundtrip(include_str!("../examples/wire/api.session.vscode.json"));
        assert_eq!(held.summary().held_by, held.held_by);
        assert_eq!(held.held_by.unwrap().entrypoint, "claude-vscode");
        assert!(s.held_by.is_none());
        roundtrip::<EventPage>(include_str!("../examples/wire/api.events.json"));
        roundtrip::<InputRequest>(include_str!("../examples/wire/api.input.request.json"));
        let r: InputResponse = roundtrip(include_str!("../examples/wire/api.input.json"));
        let ev: crate::event::Event = serde_json::from_value(r.event).unwrap();
        assert!(ev.client_op_id.is_some() && ev.seq.is_some());
        roundtrip::<ApiError>(include_str!("../examples/wire/api.error.json"));
        roundtrip::<VapidKey>(include_str!("../examples/wire/api.vapid.json"));
        roundtrip::<PushSubscription>(include_str!("../examples/wire/api.push.subscribe.json"));
        let p: PushMessage = roundtrip(include_str!("../examples/wire/api.push.json"));
        let ev: crate::event::Event = serde_json::from_value(p.payload.unwrap()).unwrap();
        assert_eq!(ev.session_id, p.session_id);
        roundtrip::<EnrollConfirmRequest>(include_str!(
            "../examples/wire/api.enroll.confirm.request.json"
        ));
        roundtrip::<EnrollConfirmed>(include_str!("../examples/wire/api.enroll.confirm.json"));
        roundtrip::<EnrollInfo>(include_str!("../examples/wire/api.enroll.info.json"));
        assert!(!s.system);
        let sys: Session = roundtrip(include_str!("../examples/wire/api.session.system.json"));
        assert!(sys.system && sys.workspace_id.is_none());
        assert_eq!(sys.agent.runtime, Runtime::None);
    }

    #[test]
    fn project_and_workspace_examples_roundtrip() {
        let p: Project = roundtrip(include_str!("../examples/wire/api.project.json"));
        let l: ProjectList = roundtrip(include_str!("../examples/wire/api.projects.json"));
        assert_eq!(l.projects[1], p);
        assert!(l.projects[0].is_personal && !p.is_personal);
        roundtrip::<CreateProject>(include_str!("../examples/wire/api.project.request.json"));
        roundtrip::<PatchProject>(include_str!(
            "../examples/wire/api.project.patch.request.json"
        ));
        let w: Workspace = roundtrip(include_str!("../examples/wire/api.workspace.json"));
        assert_eq!(p.default_workspace_id, Some(w.id));
        assert_eq!(w.project_id, p.id);
        let wl: WorkspaceList = roundtrip(include_str!("../examples/wire/api.workspaces.json"));
        assert_eq!(wl.workspaces, std::slice::from_ref(&w));
        let c: CreateWorkspace =
            roundtrip(include_str!("../examples/wire/api.workspace.request.json"));
        assert_eq!((c.project_id, c.worker_id), (w.project_id, w.worker_id));
        roundtrip::<PatchWorkspace>(include_str!(
            "../examples/wire/api.workspace.patch.request.json"
        ));
        let conflict: WorkspaceConflict =
            roundtrip(include_str!("../examples/wire/api.workspace.conflict.json"));
        assert_eq!(conflict.error, "workspace_conflict");
        assert_eq!(conflict.existing_workspace_id, w.id);
        // The conflict is also a plain error body.
        let plain: ApiError =
            serde_json::from_str(include_str!("../examples/wire/api.workspace.conflict.json"))
                .unwrap();
        assert_eq!(plain.error, "workspace_conflict");
        roundtrip::<DirList>(include_str!("../examples/wire/api.dirs.json"));
        let a: Artifact = roundtrip(include_str!("../examples/wire/api.artifact.json"));
        assert_eq!(a.kind, crate::event::ArtifactKind::Image);
        roundtrip::<ArtifactUrl>(include_str!("../examples/wire/api.artifact.url.json"));
        let cl: CandidateList = roundtrip(include_str!("../examples/wire/api.candidates.json"));
        assert_eq!(cl.candidates.len(), 2);
        let i: CreateWorkspaceImport = roundtrip(include_str!(
            "../examples/wire/api.workspace.import.request.json"
        ));
        assert!(i.mirror_enabled);
        // A plain `CreateWorkspace` body is an import without the mirror.
        let plain: CreateWorkspaceImport =
            serde_json::from_str(include_str!("../examples/wire/api.workspace.request.json"))
                .unwrap();
        assert_eq!((plain.workspace, plain.mirror_enabled), (c, false));
        let s: CreateSessionRequest =
            roundtrip(include_str!("../examples/wire/api.session.request.json"));
        assert_eq!(s.workspace_id, Some(w.id));
        assert_eq!(
            serde_json::from_str::<CreateSessionRequest>("{}").unwrap(),
            CreateSessionRequest::default()
        );
    }

    #[test]
    fn push_subscription_accepts_browser_null() {
        let s: PushSubscription = serde_json::from_str(
            r#"{"endpoint":"https://p/x","expirationTime":null,"keys":{"p256dh":"a","auth":"b"}}"#,
        )
        .unwrap();
        assert_eq!(s.expiration_time, None);
    }

    #[test]
    fn vapid_key_is_null_without_push() {
        assert_eq!(
            serde_json::to_string(&VapidKey { public_key: None }).unwrap(),
            r#"{"public_key":null}"#
        );
    }

    #[test]
    fn enroll_request_ignores_unknown_fields() {
        let r: EnrollRequest =
            serde_json::from_str(r#"{"name":"w","os":"linux","arch":"arm64"}"#).unwrap();
        assert_eq!(r.protocol_version, None);
    }
}
