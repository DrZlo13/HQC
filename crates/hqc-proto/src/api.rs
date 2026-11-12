//! HTTP bodies shared by the hub, the worker and the client (SPEC §5.1, §6).
//!
//! Requests ignore unknown fields, so an older hub accepts a newer worker's body.
//! Wire examples: `examples/wire/api.*.json`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::client_frame::SessionSummary;
use crate::event::SessionState;
use crate::frame::{HeldBy, LockedBy, PermissionMode, ProcessState, Runtime, TitleSource};

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
    /// When the session was archived (T50); absent for a live session and for one archived
    /// before hub migration `0026`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<DateTime<Utc>>,
    pub state: SessionState,
    pub queue_len: u64,
    pub last_read_seq: u64,
    /// Same rule as [`SessionSummary::unread`].
    pub unread: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The service session of worker `worker_id` (SPEC §10.2): no agent (`runtime: none`),
    /// no input, not archived; the client lists it under "Workers". Sent only when true.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub system: bool,
    /// A live process outside hqc holds the transcript open (SPEC §9.3): the session is
    /// `locked_vscode` and cannot be taken over until it is closed there. Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_by: Option<HeldBy>,
    /// Slash commands of the session's runtime, without the `/`, as the worker last reported
    /// them (`session.state.commands`, SPEC §5.4); the client completes `/` with them. Absent
    /// until a process of the session has reported a list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<String>>,
    /// The session whose agent started this one with `hqc_spawn` (SPEC §7), while it exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawned_by: Option<SpawnedBy>,
    /// The permission mode hqc asks the worker for (T38, SPEC §3 Session): set on creation
    /// or by `PATCH`; absent — the worker's `[permissions] mode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    /// The mode the session's agent runs in, as its worker last reported it
    /// (`session.state.permission_mode`); what the client shows. Absent until reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_permission_mode: Option<PermissionMode>,
    /// While the current turn runs under a peer message's cap (T39, SPEC §10.1): the
    /// address of the session up the chain whose `ask` lowered this `auto` session to `ask`
    /// for the turn, as the worker reported it (`session.state.capped_input`). Absent
    /// otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_capped_by: Option<String>,
    /// How many subagents the session's agent runs now, as its worker last reported it
    /// (`session.state.subagents`, since SPEC 0.68). Absent — 0.
    #[serde(default, skip_serializing_if = "crate::frame::is_zero")]
    pub subagents: u32,
    /// How many background tasks the runtime lists for the session's agent now, as its
    /// worker last reported it (`session.state.background_tasks`, since SPEC 0.104). Absent
    /// — 0.
    #[serde(default, skip_serializing_if = "crate::frame::is_zero")]
    pub background_tasks: u32,
    /// The output tokens per second of the session's agent and its subagents, as its worker
    /// last sent them (`tokens.rate`, kept in the hub's memory). Absent — 0.
    #[serde(default, skip_serializing_if = "crate::frame::is_zero")]
    pub output_per_s: u32,
    /// The calls the session's agent runs in the foreground now and can move to the background
    /// (`POST .../background`), by `call_id`, as its worker last reported them
    /// (`session.state.foreground_calls`, since SPEC 0.125). Absent — none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreground_calls: Vec<String>,
    /// When the requesting user made the session a favorite (`PUT .../favorite`); favorites
    /// are per user. Absent — not a favorite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favorite_at: Option<DateTime<Utc>>,
}

/// `spawned_by` of a session: the spawning session and its address
/// (`<project-slug>/<session-slug>`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpawnedBy {
    pub session_id: Uuid,
    pub address: String,
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
            permission_mode: self.permission_mode,
            effective_permission_mode: self.effective_permission_mode,
            permission_capped_by: self.permission_capped_by.clone(),
            subagents: self.subagents,
            background_tasks: self.background_tasks,
            output_per_s: self.output_per_s,
            foreground_calls: self.foreground_calls.clone(),
            favorite_at: self.favorite_at,
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

/// The order of a project's session list (T50, SPEC §6): `GET` and `PUT
/// /api/projects/{id}/session_order`, per user and project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOrderMode {
    /// Needs you / Running / Idle.
    #[default]
    State,
    /// Today / Yesterday / Earlier.
    Recent,
    /// `session_ids` as the user arranged them.
    Custom,
}

impl SessionOrderMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionOrderMode::State => "state",
            SessionOrderMode::Recent => "recent",
            SessionOrderMode::Custom => "custom",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::State, Self::Recent, Self::Custom]
            .into_iter()
            .find(|m| m.as_str() == s)
    }
}

/// The reply of `GET`/`PUT /api/projects/{id}/session_order`: every unarchived session of
/// the project (the service sessions aside) in the user's order, those not placed yet first,
/// newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionOrder {
    pub mode: SessionOrderMode,
    pub session_ids: Vec<Uuid>,
}

/// `PUT /api/projects/{id}/session_order`: either field or both. Ids that are not unarchived
/// sessions of the project are ignored; the project's sessions missing from the list go first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PutSessionOrder {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<SessionOrderMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ids: Option<Vec<Uuid>>,
}

/// `PUT /api/me/project_order` (since SPEC 0.64): the user's projects in the order of the
/// project bar. Ids that are not the user's projects are ignored; projects missing from the
/// list go last, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PutProjectOrder {
    pub project_ids: Vec<Uuid>,
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
    /// Absent: the worker's `[permissions] mode` (T38). `auto` only from the worker owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
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
    /// The stored color, or one picked by a hash of `id` (T49).
    pub color: ProjectColor,
    /// Absent: the client shows the first letters of the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<ProjectIcon>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// As in the session object: when the requesting user made the project a favorite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favorite_at: Option<DateTime<Utc>>,
    /// Since when the project is archived: not in `GET /api/projects`, only with `?archived=1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<DateTime<Utc>>,
}

/// A project's color (SPEC §3 Project, T49): the names of the client's `theme.ts`. No
/// yellows: amber means "waiting for you" in every project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectColor {
    Teal,
    Plum,
    Blue,
    Rose,
    Moss,
    Coral,
    Cyan,
    Slate,
}

impl ProjectColor {
    pub const ALL: [ProjectColor; 8] = [
        ProjectColor::Teal,
        ProjectColor::Plum,
        ProjectColor::Blue,
        ProjectColor::Rose,
        ProjectColor::Moss,
        ProjectColor::Coral,
        ProjectColor::Cyan,
        ProjectColor::Slate,
    ];

    /// The default of a project that has none: FNV-1a over the id's bytes, modulo 8.
    pub fn of_id(id: Uuid) -> Self {
        let mut h: u32 = 0x811c_9dc5;
        for b in id.as_bytes() {
            h ^= u32::from(*b);
            h = h.wrapping_mul(0x0100_0193);
        }
        Self::ALL[(h % 8) as usize]
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ProjectColor::Teal => "teal",
            ProjectColor::Plum => "plum",
            ProjectColor::Blue => "blue",
            ProjectColor::Rose => "rose",
            ProjectColor::Moss => "moss",
            ProjectColor::Coral => "coral",
            ProjectColor::Cyan => "cyan",
            ProjectColor::Slate => "slate",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

/// A project's icon (SPEC §3 Project, T49). `image` is uploaded with `PUT
/// /api/projects/{id}/icon`; its `url` (`/api/projects/{id}/icon?v=<hash>`) changes with the
/// image, so it may be cached for good.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectIcon {
    Emoji {
        value: String,
    },
    /// 1–2 characters, upper case.
    Letters {
        value: String,
    },
    Image {
        #[serde(default)]
        url: String,
    },
}

/// The app's base palette (SPEC §3 User, T49), the same on every device of the user: three
/// dark ones and their light counterparts (since 0.88).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    Warm,
    Graphite,
    Contrast,
    WarmLight,
    GraphiteLight,
    ContrastLight,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::Warm => "warm",
            Theme::Graphite => "graphite",
            Theme::Contrast => "contrast",
            Theme::WarmLight => "warm_light",
            Theme::GraphiteLight => "graphite_light",
            Theme::ContrastLight => "contrast_light",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Theme::Warm,
            Theme::Graphite,
            Theme::Contrast,
            Theme::WarmLight,
            Theme::GraphiteLight,
            Theme::ContrastLight,
        ]
        .into_iter()
        .find(|t| t.as_str() == s)
    }
}

/// How dense the client draws everything (SPEC §3 User), the same on every device of the user:
/// `compact` — the feed across the chat column in one column, the system font, a line per
/// session and the projects' names on the project bar; `spacious` — none of that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Look {
    Compact,
    #[default]
    Spacious,
}

impl Look {
    pub fn as_str(self) -> &'static str {
        match self {
            Look::Compact => "compact",
            Look::Spacious => "spacious",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Look::Compact, Look::Spacious]
            .into_iter()
            .find(|l| l.as_str() == s)
    }
}

/// The order of the project bar (SPEC §3 User), the same on every device of the user, the
/// favorite projects first in each: `custom` — the user's own (`PUT /api/me/project_order`);
/// `name` — by the shown name; `recent` — by the latest activity of the project's sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectSort {
    #[default]
    Custom,
    Name,
    Recent,
}

impl ProjectSort {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectSort::Custom => "custom",
            ProjectSort::Name => "name",
            ProjectSort::Recent => "recent",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [ProjectSort::Custom, ProjectSort::Name, ProjectSort::Recent]
            .into_iter()
            .find(|o| o.as_str() == s)
    }
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
    /// T49; the personal project may change it too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<ProjectColor>,
    /// `emoji` or `letters`; either drops a stored image. An `image` is set only by `PUT
    /// /api/projects/{id}/icon` (here: `422`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<ProjectIcon>,
}

/// A workspace (SPEC §3 Workspace): a folder on a worker that belongs to one project.
/// `path` is the canonical path the worker reported in `fs.stat` (for a clone: in
/// `workspace.created`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: Uuid,
    pub project_id: Uuid,
    pub worker_id: Uuid,
    pub path: String,
    #[serde(default)]
    pub label: String,
    pub environment: String,
    /// `dir` | `clone` (since SPEC 0.58, T45) | `worktree` (since 0.59, T46).
    pub kind: String,
    /// A worktree's origin: the git workspace of the same project and worker it was made from
    /// (T46).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_workspace_id: Option<Uuid>,
    /// The branch a clone was asked for, or the worktree's branch (T46).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// A worktree `hqc_spawn` made for the session it started (T47).
    pub ephemeral: bool,
    /// An ephemeral workspace that is ready and has no session that is not archived: shown
    /// for deletion, never deleted automatically (T47, SPEC §13.2). Computed by the hub.
    #[serde(default)]
    pub marked_for_deletion: bool,
    pub created_by_hqc: bool,
    pub mirror_enabled: bool,
    /// How many hqc allow rules the folder has (since SPEC 0.52, T48).
    #[serde(default)]
    pub hqc_rules: u32,
    /// `ready`, or for a folder hqc makes (T45) `creating` until the worker answers and
    /// `failed` when it could not (the reason in `error`).
    #[serde(default)]
    pub state: WorkspaceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `Workspace.state` (T45, SPEC §3 Workspace).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceState {
    /// The worker is making the folder (a clone runs).
    Creating,
    #[default]
    Ready,
    /// The worker could not make it; only the record is left.
    Failed,
}

impl WorkspaceState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "creating" => Some(Self::Creating),
            "ready" => Some(Self::Ready),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// `GET /api/workspaces/{id}/inspect` (T45): the worker's `workspace.inspect` of a folder hqc
/// created, for the confirmation before it is deleted.
pub type WorkspaceInspection = crate::frame::WorkspaceInspectResult;

/// One of hqc's allow rules of a workspace (T48, SPEC §3 Workspace).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceRule {
    pub id: Uuid,
    /// As [`crate::rules`] reads it.
    pub rule: String,
    pub created_at: DateTime<Utc>,
}

/// `GET /api/workspaces/{id}/rules`, and the answer to `POST` (the whole list, newest first).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceRuleList {
    pub rules: Vec<WorkspaceRule>,
}

/// `POST /api/workspaces/{id}/rules`: rules to add; ones the folder has already are kept as
/// they are.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AddWorkspaceRules {
    pub rules: Vec<String>,
}

/// `GET /api/workspaces/{id}/permissions?command=|tool=`: the worker's
/// `permissions.inspect` (SPEC §5.3, §6).
pub type WorkspacePermissions = crate::frame::InspectResult;

/// `GET /api/workspaces?project_id=`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceList {
    pub workspaces: Vec<Workspace>,
}

/// `POST /api/workspaces`: an existing folder (`kind: dir`, the default), a new one the
/// worker clones into (`kind: clone` with `url` and `branch?`, T45) or a worktree of a git
/// workspace (`kind: worktree` with `origin_workspace_id`, `branch`, `new_branch`, T46; `path`
/// may be left empty for `<origin-parent>/<origin-name>-<branch>`). The hub checks the path
/// (for a new folder: its parent) with `fs.stat` on the worker, which must be online (`409
/// worker_offline`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateWorkspace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub project_id: Uuid,
    pub worker_id: Uuid,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_workspace_id: Option<Uuid>,
    /// A worktree on a branch it creates from the origin's `HEAD` (T46).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub new_branch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// A `dir` that is missing is made by the worker (since SPEC 0.64; `fs.stat` `create`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub create: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// `PATCH /api/workspaces/{id}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PatchWorkspace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Switch the mirror of the workspace's Claude Code sessions (T21, SPEC §9.2); the hub
    /// sends the worker `mirror.set`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_enabled: Option<bool>,
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
/// `mirror_enabled`, on by default since T21 (`false` turns it off). With it the hub sends the
/// worker `mirror.set` and the worker imports the folder's transcripts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateWorkspaceImport {
    #[serde(flatten)]
    pub workspace: CreateWorkspace,
    #[serde(default = "mirror_by_default")]
    pub mirror_enabled: bool,
}

/// New workspaces are mirrored unless the request says otherwise (T21).
fn mirror_by_default() -> bool {
    true
}

/// `GET /api/sessions/{id}/events`: stored events as raw JSON, oldest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventPage {
    pub events: Vec<serde_json::Value>,
    pub last_seq: u64,
}

/// `GET /api/search` (SPEC §15): hits best first; `next_cursor` fetches the next page
/// (`&cursor=`), absent on the last one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchPage {
    pub results: Vec<SearchHit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// The words of this page's hits the query found besides the words typed: their other
    /// forms, the similar words of a fuzzy query; lowercased, most frequent first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<String>,
    /// The hits of the whole query by kind, on the first page only (without `cursor`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<SearchCounts>,
}

/// The hits and sessions of a query (SPEC §15): all of them (title hits too) and of each
/// kind, whatever `kind` the query asked for.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchCounts {
    pub all: SearchCount,
    pub message: SearchCount,
    pub command: SearchCount,
    pub output: SearchCount,
    pub answer: SearchCount,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchCount {
    pub hits: u64,
    pub sessions: u64,
}

/// One search hit: an event's text, or the session's title (no `seq`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub session_id: Uuid,
    /// `<project-slug>/<session-slug>` (SPEC §7).
    pub address: String,
    pub title: String,
    pub archived: bool,
    /// The event to open the session at; absent for a title hit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// The event's type; absent for a title hit.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The event's time, or the session's creation for a title hit.
    pub ts: DateTime<Utc>,
    /// A short piece of the text around the first match.
    pub snippet: String,
    /// The hits of the query (with the same filters) in this hit's session.
    pub session_hits: u64,
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

/// An open request of the user (SPEC §6 `GET /api/requests`, §10.1, §10.2): an `approval`
/// or `clarify` of a session, or a worker's own request (`worker_approval`, in its service
/// session), with where it is and who waits on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenRequest {
    pub session_id: Uuid,
    pub request_id: String,
    /// `approval` | `clarify` | `worker_approval` | `move` (an agent's `hqc_move`, SPEC §7) |
    /// `follow` (a move after the transcript, SPEC §9.2).
    pub kind: String,
    /// `<project-slug>/<session-slug>` of the request's session.
    pub address: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<Uuid>,
    /// The worker's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_name: Option<String>,
    /// The request is in a worker's service session (SPEC §10.2).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub system: bool,
    /// The `approval.request` / `clarify.request` event as stored (SPEC §4).
    pub event: serde_json::Value,
    /// The sessions that wait on this one through open `hqc_ask`s (SPEC §7.1), the nearest
    /// first: the last is the top of the chain. Empty when nobody waits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting: Vec<WaitingSession>,
}

/// A session waiting on another through an open `hqc_ask`, directly or along a chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitingSession {
    pub session_id: Uuid,
    pub address: String,
    pub title: String,
}

/// `GET /api/requests`: every open request of the user, oldest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestList {
    pub requests: Vec<OpenRequest>,
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
    /// The session a click opens when it is not `session_id`: for a request of a session
    /// that other sessions wait on, the top of the chain (SPEC §16).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_session_id: Option<Uuid>,
}

/// A scheduled message (SPEC §3 ScheduledMessage, §16) as `/api/schedules` returns it.
/// Exactly one of `cron` and `at` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub session_id: Uuid,
    /// 5 fields, in `timezone`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<DateTime<Utc>>,
    /// IANA name, e.g. `Europe/Berlin`.
    pub timezone: String,
    pub message: String,
    pub enabled: bool,
    pub catch_up: bool,
    /// When it is due next; absent when it will not run again (an `at` that ran).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `GET /api/schedules?session_id=`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleList {
    pub schedules: Vec<Schedule>,
}

/// `POST /api/schedules`: `cron` or `at`; `timezone` defaults to the user's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateSchedule {
    pub session_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    pub message: String,
    #[serde(default)]
    pub catch_up: bool,
}

/// `PATCH /api/schedules/{id}`: pause (`enabled: false`) or resume.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchSchedule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// `GET /api/me`, and the reply to `PATCH /api/me`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Me {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub timezone: String,
    pub personal_project_id: Uuid,
    /// The app's base palette (T49); `warm` from hubs before it.
    #[serde(default)]
    pub theme: Theme,
    /// The client's look; `spacious` from hubs before it.
    #[serde(default)]
    pub look: Look,
    /// The project bar's order; `custom` from hubs before it.
    #[serde(default)]
    pub project_sort: ProjectSort,
}

/// `PATCH /api/me`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchMe {
    /// IANA name; the default zone of the user's schedules (SPEC §16).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// T49: `warm` | `graphite` | `contrast` | `warm_light` | `graphite_light` |
    /// `contrast_light`; anything else `422`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<Theme>,
    /// `compact` | `spacious`; anything else `422`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look: Option<Look>,
    /// `custom` | `name` | `recent`; anything else `422`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_sort: Option<ProjectSort>,
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
    /// The worker last reported the same machine fingerprint as the enrolling one (T21, SPEC
    /// §5.1): such workers come first, and the client preselects replacing one. Sent only
    /// when true.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub same_machine: bool,
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
    /// Machine fingerprint, as in `hello.machine_id` (T21, SPEC §5.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
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
        assert!(r.replaceable[0].same_machine && !r.replaceable[1].same_machine);
        let m: EnrollRequest = roundtrip(include_str!(
            "../examples/wire/api.enroll.request.machine.json"
        ));
        assert_eq!(m.machine_id.unwrap().len(), 64);
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
        assert_eq!(s.spawned_by.as_ref().unwrap().address, "hqc/planner");
        assert!(
            held.spawned_by.is_none(),
            "absent unless hqc_spawn started it"
        );
        assert_eq!(s.commands.as_deref().map(<[_]>::len), Some(4));
        assert_eq!(
            held.commands, None,
            "absent until a process reported a list"
        );
        assert_eq!(summary.favorite_at, s.favorite_at);
        assert!(s.favorite_at.is_some() && held.favorite_at.is_none());
        roundtrip::<EventPage>(include_str!("../examples/wire/api.events.json"));
        let sp: SearchPage = roundtrip(include_str!("../examples/wire/api.search.json"));
        assert!(sp.results[0].seq.is_some() && sp.results[1].seq.is_none());
        assert!(sp.results[1].archived && sp.next_cursor.is_some());
        assert_eq!(sp.results[0].session_hits, 3);
        assert_eq!(sp.also, ["migrations", "migrate"]);
        let c = sp.counts.unwrap();
        assert_eq!((c.all.hits, c.all.sessions, c.command.hits), (24, 2, 4));
        roundtrip::<InputRequest>(include_str!("../examples/wire/api.input.request.json"));
        let r: InputResponse = roundtrip(include_str!("../examples/wire/api.input.json"));
        let ev: crate::event::Event = serde_json::from_value(r.event).unwrap();
        assert!(ev.client_op_id.is_some() && ev.seq.is_some());
        roundtrip::<ApiError>(include_str!("../examples/wire/api.error.json"));
        roundtrip::<VapidKey>(include_str!("../examples/wire/api.vapid.json"));
        roundtrip::<PushSubscription>(include_str!("../examples/wire/api.push.subscribe.json"));
        let rl: RequestList = roundtrip(include_str!("../examples/wire/api.requests.json"));
        assert_eq!(rl.requests.len(), 2);
        let ev: crate::event::Event = serde_json::from_value(rl.requests[0].event.clone()).unwrap();
        assert_eq!(ev.session_id, rl.requests[0].session_id);
        assert_eq!(rl.requests[0].waiting.len(), 2);
        assert!(rl.requests[1].system && rl.requests[1].waiting.is_empty());
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
        let patch: PatchProject = roundtrip(include_str!(
            "../examples/wire/api.project.patch.request.json"
        ));
        assert_eq!(patch.color, Some(ProjectColor::Plum));
        assert!(matches!(patch.icon, Some(ProjectIcon::Emoji { .. })));
        assert!(matches!(p.icon, Some(ProjectIcon::Image { .. })));
        assert!(l.projects[0].icon.is_none(), "absent: the first letters");
        assert!(p.favorite_at.is_some() && l.projects[0].favorite_at.is_none());
        assert!(l.projects.iter().all(|x| x.archived_at.is_none()));
        let archived: ProjectList =
            roundtrip(include_str!("../examples/wire/api.projects.archived.json"));
        assert!(archived.projects[0].archived_at.is_some());
        let letters: PatchProject =
            serde_json::from_str(r#"{"icon":{"kind":"letters","value":"PM"}}"#).unwrap();
        assert_eq!(
            letters.icon,
            Some(ProjectIcon::Letters { value: "PM".into() })
        );
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
        let pm: PatchWorkspace = roundtrip(include_str!(
            "../examples/wire/api.workspace.patch.mirror.request.json"
        ));
        assert_eq!(pm.mirror_enabled, Some(false));
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
        // T45: a clone, its outcome on the workspace, and the inspection before deletion.
        let cr: CreateWorkspace = roundtrip(include_str!(
            "../examples/wire/api.workspace.clone.request.json"
        ));
        assert_eq!(cr.kind.as_deref(), Some("clone"));
        assert!(crate::frame::check_clone_url(cr.url.as_deref().unwrap()).is_ok());
        let cw: Workspace = roundtrip(include_str!("../examples/wire/api.workspace.clone.json"));
        assert_eq!((cw.kind.as_str(), cw.created_by_hqc), ("clone", true));
        assert_eq!(cw.state, WorkspaceState::Failed);
        assert!(cw.error.is_some() && w.error.is_none());
        assert_eq!(w.state, WorkspaceState::Ready);
        let old: Workspace = serde_json::from_value({
            let mut v: Value =
                serde_json::from_str(include_str!("../examples/wire/api.workspace.json")).unwrap();
            v.as_object_mut().unwrap().remove("state");
            v
        })
        .unwrap();
        assert_eq!(
            old.state,
            WorkspaceState::Ready,
            "a hub before T45 sends no state"
        );
        // T46: a worktree of the clone, by the origin's id; the path is the hub's default.
        let wr: CreateWorkspace = roundtrip(include_str!(
            "../examples/wire/api.workspace.worktree.request.json"
        ));
        assert_eq!(
            (wr.kind.as_deref(), wr.origin_workspace_id, wr.new_branch),
            (Some("worktree"), Some(cw.id), true)
        );
        assert!(wr.path.is_empty());
        let ww: Workspace = roundtrip(include_str!("../examples/wire/api.workspace.worktree.json"));
        assert_eq!(ww.origin_workspace_id, Some(cw.id));
        assert_eq!(ww.branch, wr.branch);
        assert_eq!(ww.path, format!("{}-hqc-fix-backlight", cw.path));
        assert_eq!(ww.state, WorkspaceState::Creating);
        assert!(!ww.ephemeral && !ww.marked_for_deletion);
        // T47: the worktree of a spawned session, whose sessions are all archived.
        let we: Workspace = roundtrip(include_str!(
            "../examples/wire/api.workspace.ephemeral.json"
        ));
        assert_eq!(we.origin_workspace_id, Some(cw.id));
        assert!(we.ephemeral && we.marked_for_deletion);
        assert_eq!(we.branch.as_deref(), Some("hqc/session-7"));
        let wi: WorkspaceInspection =
            roundtrip(include_str!("../examples/wire/api.workspace.inspect.json"));
        assert_eq!(wi.dirty.len() as u32, wi.dirty_count);
        let rl: WorkspaceRuleList =
            roundtrip(include_str!("../examples/wire/api.workspace.rules.json"));
        assert_eq!(rl.rules.len() as u32, w.hqc_rules);
        roundtrip::<AddWorkspaceRules>(include_str!(
            "../examples/wire/api.workspace.rules.request.json"
        ));
        let wp: WorkspacePermissions = roundtrip(include_str!(
            "../examples/wire/api.workspace.permissions.json"
        ));
        assert_eq!(wp.sources.len(), 3);
        assert_eq!(
            wp.check.map(|c| c.verdict),
            Some(crate::frame::Verdict::Deny)
        );
        let a: Artifact = roundtrip(include_str!("../examples/wire/api.artifact.json"));
        assert_eq!(a.kind, crate::event::ArtifactKind::Image);
        roundtrip::<ArtifactUrl>(include_str!("../examples/wire/api.artifact.url.json"));
        let cl: CandidateList = roundtrip(include_str!("../examples/wire/api.candidates.json"));
        assert_eq!(cl.candidates.len(), 2);
        let i: CreateWorkspaceImport = roundtrip(include_str!(
            "../examples/wire/api.workspace.import.request.json"
        ));
        assert!(i.mirror_enabled);
        // A plain `CreateWorkspace` body is mirrored too (T21).
        let plain: CreateWorkspaceImport =
            serde_json::from_str(include_str!("../examples/wire/api.workspace.request.json"))
                .unwrap();
        assert_eq!((plain.workspace, plain.mirror_enabled), (c, true));
        let s: CreateSessionRequest =
            roundtrip(include_str!("../examples/wire/api.session.request.json"));
        assert_eq!(s.workspace_id, Some(w.id));
        // The model, as the session object shows it in `agent`.
        let created: Session = roundtrip(include_str!("../examples/wire/api.session.json"));
        assert_eq!(s.model.as_deref(), created.agent.model.as_deref());
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
    fn archive_and_order_examples_roundtrip() {
        let a: Session = roundtrip(include_str!("../examples/wire/api.session.archived.json"));
        assert!(a.archived && a.archived_at.is_some());
        let o: SessionOrder = roundtrip(include_str!("../examples/wire/api.session_order.json"));
        assert_eq!(o.mode, SessionOrderMode::Custom);
        let put: PutSessionOrder = roundtrip(include_str!(
            "../examples/wire/api.session_order.request.json"
        ));
        assert_eq!(put.session_ids.as_deref(), Some(&o.session_ids[..]));
        let mode: PutSessionOrder = serde_json::from_str(r#"{"mode":"recent"}"#).unwrap();
        assert_eq!(mode.mode, Some(SessionOrderMode::Recent));
        assert!(serde_json::from_str::<PutSessionOrder>(r#"{"mode":"name"}"#).is_err());
        let p: PutProjectOrder = roundtrip(include_str!(
            "../examples/wire/api.project_order.request.json"
        ));
        assert_eq!(p.project_ids.len(), 2);
    }

    #[test]
    fn schedule_examples_roundtrip() {
        let s: Schedule = roundtrip(include_str!("../examples/wire/api.schedule.json"));
        assert!(s.cron.is_some() && s.at.is_none());
        let l: ScheduleList = roundtrip(include_str!("../examples/wire/api.schedules.json"));
        assert!(l.schedules[0].at.is_some() && l.schedules[0].last_run.is_none());
        roundtrip::<CreateSchedule>(include_str!("../examples/wire/api.schedule.request.json"));
        roundtrip::<PatchSchedule>(include_str!(
            "../examples/wire/api.schedule.patch.request.json"
        ));
        let m: PatchMe = roundtrip(include_str!("../examples/wire/api.me.patch.request.json"));
        assert_eq!(m.theme, Some(Theme::Graphite));
        assert_eq!(m.look, Some(Look::Compact));
        let me: Me = roundtrip(include_str!("../examples/wire/api.me.json"));
        assert_eq!(me.theme, Theme::Contrast);
        assert_eq!(me.look, Look::Compact);
        assert!(serde_json::from_str::<PatchMe>(r#"{"look":"dense"}"#).is_err());
        for l in [Look::Compact, Look::Spacious] {
            assert_eq!(Look::parse(l.as_str()), Some(l));
            assert_eq!(serde_json::to_value(l).unwrap(), l.as_str());
        }
        assert_eq!(m.project_sort, Some(ProjectSort::Name));
        assert_eq!(me.project_sort, ProjectSort::Recent);
        assert!(serde_json::from_str::<PatchMe>(r#"{"project_sort":"Name"}"#).is_err());
        for o in [ProjectSort::Custom, ProjectSort::Name, ProjectSort::Recent] {
            assert_eq!(ProjectSort::parse(o.as_str()), Some(o));
            assert_eq!(serde_json::to_value(o).unwrap(), o.as_str());
        }
    }

    #[test]
    fn theme_names_agree_on_the_wire_and_in_the_database() {
        // The JSON name (serde) is what the hub stores (`as_str`) and reads back (`parse`).
        for t in [
            Theme::Warm,
            Theme::Graphite,
            Theme::Contrast,
            Theme::WarmLight,
            Theme::GraphiteLight,
            Theme::ContrastLight,
        ] {
            assert_eq!(serde_json::to_value(t).unwrap(), t.as_str());
            assert_eq!(Theme::parse(t.as_str()), Some(t));
        }
    }

    #[test]
    fn project_color_by_id_is_stable_and_spread() {
        let id: Uuid = "0199f3a2-7c10-7000-8000-0000000000d2".parse().unwrap();
        assert_eq!(ProjectColor::of_id(id), ProjectColor::of_id(id));
        let seen: std::collections::HashSet<_> = (0..64u128)
            .map(|i| ProjectColor::of_id(Uuid::from_u128(i)))
            .collect();
        assert_eq!(seen.len(), 8, "every color is reachable");
        for c in ProjectColor::ALL {
            assert_eq!(ProjectColor::parse(c.as_str()), Some(c));
            assert_eq!(serde_json::to_value(c).unwrap(), c.as_str());
        }
    }

    #[test]
    fn enroll_request_ignores_unknown_fields() {
        let r: EnrollRequest =
            serde_json::from_str(r#"{"name":"w","os":"linux","arch":"arm64"}"#).unwrap();
        assert_eq!(r.protocol_version, None);
    }
}
