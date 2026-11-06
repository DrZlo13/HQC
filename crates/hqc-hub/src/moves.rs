//! A session moves to another folder of its worker, in another project (SPEC §3 Session,
//! §6): `POST /api/sessions/{id}/move` goes through [`move_session`].
//!
//! - Only a Claude Code session with a workspace, not archived, neither driven from VS Code
//!   (`locked_by: vscode`) nor held open by a live process outside hqc (`held_by`, SPEC
//!   §9.3), with no subagent running (`subagents`, a foreground one too), whose worker is
//!   online and reports `claude_code.moves` (SPEC §5.4); otherwise `409`.
//! - The worker checks the folder with `fs.stat` (`create`: it makes a missing one, before
//!   the overlap check below, so a refused move may leave it, as `POST /api/workspaces`
//!   does), with the codes of `POST /api/workspaces`; the canonical path it reports is the
//!   target.
//! - A workspace of the owner on that worker at exactly that path is taken with its project
//!   (`project`, when given, must be that one); a path that equals another owner's
//!   workspace, lies inside one or contains one is `409 workspace_conflict`, and no record is
//!   created.
//! - Otherwise the folder becomes a new `dir` workspace, `ready` and mirrored, of `project`
//!   (a slug or id of the owner's) or of a new project named after the folder's last
//!   component, whose id is derived from the session and the path ([`crate::mcp::once_id`]),
//!   so a repeat finds it. The project's first workspace becomes its default.
//! - A session already in that workspace is a repeat: the same result, nothing written.
//! - One UPDATE moves the session ([`db::move_session`]: `project_id`, `workspace_id`, `cwd =
//!   '.'`, the slug kept when free there); its old project's saved orders drop it; `system
//!   session_moved` goes into its feed; then the workspace's `mirror.set`, so its `since`
//!   counts the session.
//! - The connection's start state goes back to "not started" and a running agent gets a new
//!   `session.start` with the new folder at once, which the worker applies once no turn runs
//!   (SPEC §5.3); a stopped agent starts there with its next input. Nothing is queued.

use std::path::Path;

use uuid::Uuid;

use crate::{
    api,
    app::AppState,
    db::{self, SessionRow},
    delivery,
    error::{ApiError, ApiResult},
    events,
    worker_ws::StartState,
};

/// `system.code` written into the feed of a session that moved (SPEC §4).
pub const MOVED: &str = "session_moved";
/// What [`crate::mcp::once_id`] derives the id of a move's new project from.
const PROJECT: &str = "hqc_move project";

/// Where the session is now.
pub struct Moved {
    pub session: SessionRow,
    pub project: db::ProjectRow,
    pub workspace: db::WorkspaceRow,
}

fn unavailable(message: &str) -> ApiError {
    ApiError::Conflict("unavailable", message.into())
}

/// Moves session `s` into folder `path` of its worker, in `project` (a slug or id of the
/// owner's) or a new project named after the folder.
pub async fn move_session(
    app: &AppState,
    s: &SessionRow,
    path: &str,
    project: Option<&str>,
    create: bool,
) -> ApiResult<Moved> {
    if s.archived {
        return Err(ApiError::Conflict(
            "archived",
            "the session is archived".into(),
        ));
    }
    if s.system || s.runtime != "claude_code" {
        return Err(unavailable("only a Claude Code session can move"));
    }
    if s.workspace_id.is_none() {
        return Err(unavailable("the session has no folder to move from"));
    }
    if s.locked_by == "vscode" || s.held_by.is_some() {
        return Err(unavailable(
            "the session is driven or open outside hqc (VS Code): close it there first",
        ));
    }
    // A background subagent outlives the turn: the agent would wait in its old folder for it,
    // and the input meanwhile would run there. The hub sees only the count, so a foreground
    // one refuses too.
    if s.subagents > 0 {
        return Err(unavailable(
            "a subagent of the session is still running: move once it has ended",
        ));
    }
    let worker = match s.worker_id {
        Some(w) => db::worker(&app.db, w).await?,
        None => None,
    }
    .filter(|w| w.revoked_at.is_none())
    .ok_or_else(|| unavailable("the session has no worker"))?;
    if !app.workers.is_online(worker.id) {
        return Err(ApiError::Conflict(
            "worker_offline",
            "the worker is offline".into(),
        ));
    }
    if worker.capabilities["claude_code"]["moves"] != true {
        return Err(unavailable(
            "the session's worker cannot move a session; update it",
        ));
    }
    let path = api::stat_folder(app, worker.id, path, create).await?;
    // Held until the session is moved: a repeat that comes in parallel finds what this made.
    let _lock = app.owner_lock("move", s.owner_id).await;
    let s = db::session(&app.db, s.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let wanted = match project {
        Some(p) => Some(owner_project(app, s.owner_id, p).await?),
        None => None,
    };
    let ws = match db::overlapping_workspace(&app.db, worker.id, &path).await? {
        Some(w) if w.path == path && w.owner_id == s.owner_id => {
            if let Some(p) = wanted.as_ref().filter(|p| p.id != w.project_id) {
                return Err(ApiError::BadRequest(format!(
                    "{path} is a workspace of another project, not of {}",
                    p.slug
                )));
            }
            w
        }
        Some(x) => return Err(api::workspace_conflict(app, &path, &x).await?),
        None => {
            let project = match wanted {
                Some(p) => p,
                None => new_project(app, &s, &path).await?,
            };
            let ws = api::workspace_insert(
                app,
                db::NewWorkspace {
                    project_id: project.id,
                    owner_id: s.owner_id,
                    worker_id: worker.id,
                    path: &path,
                    label: "",
                    mirror_enabled: true,
                    source: db::Source::Dir,
                    ephemeral: None,
                },
            )
            .await?;
            tracing::info!(workspace_id = %ws.id, project_id = %ws.project_id, path = %ws.path, "workspace added for a move");
            ws
        }
    };
    let project = db::project(&app.db, ws.project_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if s.workspace_id == Some(ws.id) {
        // A repeat: the session is there already.
        return Ok(Moved {
            session: s,
            project,
            workspace: ws,
        });
    }
    let from = db::project(&app.db, s.project_id)
        .await?
        .map_or_else(|| "?".to_string(), |p| p.name);
    db::move_session(&app.db, &s, project.id, ws.id).await?;
    let moved = db::session(&app.db, s.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    events::system(
        app,
        &moved,
        MOVED,
        &format!(
            "The session moved from {from} to {} ({})",
            project.name, ws.path
        ),
    )
    .await?;
    tracing::info!(session_id = %s.id, project_id = %project.id, workspace_id = %ws.id, path = %ws.path, slug = %moved.slug, "session moved");
    if let Some(conn) = app.workers.get(worker.id) {
        // After the move: `since` of the workspace's sessions counts this one (SPEC §9.1).
        if ws.mirror_enabled {
            crate::mirror::enable(app, &conn, &ws).await?;
        }
        conn.st.lock().await.sessions.entry(s.id).or_default().start = StartState::NotStarted;
    }
    delivery::kick(app, s.id, moved.process_state != "stopped").await;
    app.session_changed(s.id);
    let session = db::session(&app.db, s.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Moved {
        session,
        project,
        workspace: ws,
    })
}

/// The owner's project by slug or id; another owner's is `404`, as a missing one.
async fn owner_project(app: &AppState, owner: Uuid, p: &str) -> ApiResult<db::ProjectRow> {
    let id = p.parse::<Uuid>().ok();
    db::projects_of(&app.db, owner)
        .await?
        .into_iter()
        .find(|x| Some(x.id) == id || x.slug == p)
        .ok_or(ApiError::NotFound)
}

/// The project a move of `s` to `path` makes, named after the folder; found again by its
/// derived id on a repeat.
async fn new_project(app: &AppState, s: &SessionRow, path: &str) -> ApiResult<db::ProjectRow> {
    let id = crate::mcp::once_id(PROJECT, s.id, path);
    if let Some(p) = db::project(&app.db, id).await? {
        if p.owner_id != s.owner_id {
            return Err(anyhow::anyhow!("project {id} is not the session owner's").into());
        }
        return Ok(p);
    }
    let name: String = Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
        .chars()
        .take(api::MAX_PROJECT_NAME)
        .collect();
    let p = db::create_project_as(&app.db, id, s.owner_id, &api::project_name(&name)?, "").await?;
    tracing::info!(project_id = %p.id, session_id = %s.id, "project created for a move");
    Ok(p)
}
