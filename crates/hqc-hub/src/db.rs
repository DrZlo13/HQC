//! Postgres access (sqlx, runtime queries). Migrations: `crates/hqc-hub/migrations`.

use chrono::{DateTime, Utc};
use hqc_proto::{
    api::{ProjectColor, ProjectIcon, WorkspaceState},
    frame::{PermissionMode, TitleSource},
};
use serde_json::{Map, Value};
use sqlx::{postgres::PgPoolOptions, FromRow, PgPool};
use uuid::Uuid;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(url: &str) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .connect(url)
        .await?;
    MIGRATOR.run(&pool).await?;
    Ok(pool)
}

pub fn new_id() -> Uuid {
    Uuid::now_v7()
}

// ---------------------------------------------------------------- users, projects

#[derive(Debug, Clone, FromRow)]
pub struct UserRow {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub timezone: String,
    pub personal_project_id: Option<Uuid>,
    /// `warm` | `graphite` | `contrast` (T49).
    pub theme: String,
}

#[derive(Debug, Clone, FromRow, serde::Serialize)]
pub struct ProjectRow {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub default_workspace_id: Option<Uuid>,
    pub is_personal: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// NULL: by a hash of the id (T49).
    pub color: Option<String>,
    /// `{kind: emoji|letters, value}` or `{kind: image, v}` (T49).
    pub icon: Option<Value>,
}

/// Find the user by the verified email, creating it and its personal project on first login
/// (SPEC §3 User).
pub async fn ensure_user(db: &PgPool, email: &str) -> sqlx::Result<UserRow> {
    let email = email.trim().to_lowercase();
    if let Some(u) = sqlx::query_as::<_, UserRow>("SELECT * FROM users WHERE email = $1")
        .bind(&email)
        .fetch_optional(db)
        .await?
    {
        if u.personal_project_id.is_some() {
            return Ok(u);
        }
    }
    let mut tx = db.begin().await?;
    let display_name = email.split('@').next().unwrap_or(&email).to_string();
    sqlx::query(
        "INSERT INTO users (id, email, display_name) VALUES ($1, $2, $3) ON CONFLICT (email) DO NOTHING",
    )
    .bind(new_id())
    .bind(&email)
    .bind(&display_name)
    .execute(&mut *tx)
    .await?;
    let u = sqlx::query_as::<_, UserRow>("SELECT * FROM users WHERE email = $1 FOR UPDATE")
        .bind(&email)
        .fetch_one(&mut *tx)
        .await?;
    if u.personal_project_id.is_some() {
        tx.commit().await?;
        return Ok(u);
    }
    let pid = new_id();
    sqlx::query(
        "INSERT INTO projects (id, owner_id, name, slug, is_personal) VALUES ($1, $2, 'Personal', 'personal', true)",
    )
    .bind(pid)
    .bind(u.id)
    .execute(&mut *tx)
    .await?;
    let u = sqlx::query_as::<_, UserRow>(
        "UPDATE users SET personal_project_id = $2, updated_at = now() WHERE id = $1 RETURNING *",
    )
    .bind(u.id)
    .bind(pid)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(u)
}

/// An OIDC sign-in (SPEC §3 User, §6): the user bound to `sub`; else the user of `email`
/// ([`ensure_user`]), bound to `sub` now if it has no subject yet. `None`: the user of `email`
/// is bound to another subject.
pub async fn ensure_oidc_user(
    db: &PgPool,
    email: &str,
    sub: &str,
) -> sqlx::Result<Option<UserRow>> {
    if let Some(u) = sqlx::query_as::<_, UserRow>("SELECT * FROM users WHERE oidc_sub = $1")
        .bind(sub)
        .fetch_optional(db)
        .await?
    {
        return Ok(Some(u));
    }
    let u = ensure_user(db, email).await?;
    // `oidc_sub = $2`: a concurrent first sign-in of the same subject bound it meanwhile.
    sqlx::query_as::<_, UserRow>(
        "UPDATE users SET oidc_sub = $2, updated_at = now()
         WHERE id = $1 AND (oidc_sub IS NULL OR oidc_sub = $2) RETURNING *",
    )
    .bind(u.id)
    .bind(sub)
    .fetch_optional(db)
    .await
}

/// In the user's order (`project_order`), the projects missing from it last, oldest first.
pub async fn projects_of(db: &PgPool, owner: Uuid) -> sqlx::Result<Vec<ProjectRow>> {
    sqlx::query_as(
        "SELECT p.* FROM projects p JOIN users u ON u.id = p.owner_id
         WHERE p.owner_id = $1
         ORDER BY array_position(u.project_order, p.id) NULLS LAST, p.created_at",
    )
    .bind(owner)
    .fetch_all(db)
    .await
}

/// Keeps the user's own projects of `ids`, each once, in their order.
pub async fn put_project_order(db: &PgPool, owner: Uuid, ids: &[Uuid]) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE users SET project_order = ARRAY(
            SELECT t.id FROM unnest($2::uuid[]) WITH ORDINALITY AS t(id, n)
            WHERE t.id IN (SELECT id FROM projects WHERE owner_id = $1)
            GROUP BY t.id ORDER BY min(t.n))
         WHERE id = $1",
    )
    .bind(owner)
    .bind(ids)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn project(db: &PgPool, id: Uuid) -> sqlx::Result<Option<ProjectRow>> {
    sqlx::query_as("SELECT * FROM projects WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
}

impl ProjectRow {
    /// `favorite_at`: the requesting user's (`project_favorites`).
    pub fn to_api(&self, favorite_at: Option<DateTime<Utc>>) -> hqc_proto::api::Project {
        hqc_proto::api::Project {
            id: self.id,
            owner_id: self.owner_id,
            name: self.name.clone(),
            slug: self.slug.clone(),
            description: self.description.clone(),
            default_workspace_id: self.default_workspace_id,
            is_personal: self.is_personal,
            color: self
                .color
                .as_deref()
                .and_then(ProjectColor::parse)
                .unwrap_or_else(|| ProjectColor::of_id(self.id)),
            icon: self.icon.as_ref().and_then(|v| self.icon_api(v)),
            created_at: self.created_at,
            updated_at: self.updated_at,
            favorite_at,
        }
    }

    /// The stored icon as the API gives it: an image gets the URL its `v` versions.
    fn icon_api(&self, v: &Value) -> Option<ProjectIcon> {
        let value = || v.get("value")?.as_str().map(String::from);
        match v.get("kind")?.as_str()? {
            "emoji" => Some(ProjectIcon::Emoji { value: value()? }),
            "letters" => Some(ProjectIcon::Letters { value: value()? }),
            "image" => Some(ProjectIcon::Image {
                url: format!("/api/projects/{}/icon?v={}", self.id, v.get("v")?.as_str()?),
            }),
            _ => None,
        }
    }
}

/// A new project with a slug unique for its owner (SPEC §3 Project).
pub async fn create_project(
    db: &PgPool,
    owner: Uuid,
    name: &str,
    description: &str,
) -> sqlx::Result<ProjectRow> {
    let base = slugify_or(name, "project");
    for i in 1.. {
        let slug = if i == 1 {
            base.clone()
        } else {
            format!("{base}-{i}")
        };
        let row = sqlx::query_as(
            "INSERT INTO projects (id, owner_id, name, slug, description) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (owner_id, slug) DO NOTHING RETURNING *",
        )
        .bind(new_id())
        .bind(owner)
        .bind(name)
        .bind(&slug)
        .bind(description)
        .fetch_optional(db)
        .await?;
        if let Some(row) = row {
            return Ok(row);
        }
    }
    unreachable!()
}

/// The fields `PATCH /api/projects/{id}` changes; `None` keeps the stored value.
#[derive(Debug, Default)]
pub struct ProjectUpdate<'a> {
    pub name: Option<&'a str>,
    pub description: Option<&'a str>,
    pub default_workspace_id: Option<Uuid>,
    pub color: Option<ProjectColor>,
    /// An emoji or letters (stored as `{kind, value}`); a stored image goes with it.
    pub icon: Option<Value>,
}

/// `PATCH /api/projects/{id}`: only the given fields change; the slug stays.
pub async fn update_project(
    db: &PgPool,
    id: Uuid,
    u: ProjectUpdate<'_>,
) -> sqlx::Result<ProjectRow> {
    let mut tx = db.begin().await?;
    if u.icon.is_some() {
        sqlx::query("DELETE FROM project_icons WHERE project_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    let row = sqlx::query_as(
        "UPDATE projects SET name = coalesce($2, name), description = coalesce($3, description),
            default_workspace_id = coalesce($4, default_workspace_id),
            color = coalesce($5, color), icon = coalesce($6, icon), updated_at = now()
         WHERE id = $1 RETURNING *",
    )
    .bind(id)
    .bind(u.name)
    .bind(u.description)
    .bind(u.default_workspace_id)
    .bind(u.color.map(ProjectColor::as_str))
    .bind(u.icon)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// A project's icon image (T49): its checked type and bytes.
#[derive(Debug, Clone, FromRow)]
pub struct ProjectIconRow {
    pub mime: String,
    pub data: Vec<u8>,
    pub sha256: String,
}

/// Stores the image and makes it the project's icon.
pub async fn set_project_icon_image(
    db: &PgPool,
    id: Uuid,
    mime: &str,
    data: &[u8],
    sha256: &str,
) -> sqlx::Result<ProjectRow> {
    let mut tx = db.begin().await?;
    sqlx::query(
        "INSERT INTO project_icons (project_id, mime, data, sha256) VALUES ($1, $2, $3, $4)
         ON CONFLICT (project_id) DO UPDATE
            SET mime = $2, data = $3, sha256 = $4, updated_at = now()",
    )
    .bind(id)
    .bind(mime)
    .bind(data)
    .bind(sha256)
    .execute(&mut *tx)
    .await?;
    let row = sqlx::query_as(
        "UPDATE projects SET icon = jsonb_build_object('kind', 'image', 'v', $2::text),
            updated_at = now()
         WHERE id = $1 RETURNING *",
    )
    .bind(id)
    .bind(&sha256[..16])
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

pub async fn project_icon_image(db: &PgPool, id: Uuid) -> sqlx::Result<Option<ProjectIconRow>> {
    sqlx::query_as("SELECT mime, data, sha256 FROM project_icons WHERE project_id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
}

/// Deletes the project with its workspaces and sessions (their events, cursors and requests
/// go with them, SPEC §17.4). Returns the sessions that were on a worker, for `session.kill`.
pub async fn delete_project(db: &PgPool, id: Uuid) -> sqlx::Result<Vec<(Uuid, Uuid)>> {
    let mut tx = db.begin().await?;
    let killed: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, worker_id FROM sessions WHERE project_id = $1 AND worker_id IS NOT NULL",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM sessions WHERE project_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM projects WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(killed)
}

// ---------------------------------------------------------------- workers

#[derive(Debug, Clone, FromRow)]
pub struct WorkerRow {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub os: String,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_seen: Option<DateTime<Utc>>,
    pub protocol_version: Option<i32>,
    pub capabilities: Value,
    pub environments: Value,
    pub env_bindings: Value,
    pub allowed_roots: Value,
    pub allow_foreign_sessions: bool,
    pub share_local_models: bool,
    pub local_models: Value,
    /// The worker's service session (SPEC §10.2); set at enrollment, by migration 0006 for
    /// older workers, and by [`ensure_system_session`] if still missing.
    pub system_session_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The machine fingerprint of its last `hello` or enrollment (SPEC §5.1, T21).
    pub machine_hash: Option<String>,
}

const WORKER_COLS: &str = "id, owner_id, name, os, revoked_at, last_seen, protocol_version, \
    capabilities, environments, env_bindings, allowed_roots, allow_foreign_sessions, \
    share_local_models, local_models, system_session_id, created_at, updated_at, machine_hash";

pub async fn worker(db: &PgPool, id: Uuid) -> sqlx::Result<Option<WorkerRow>> {
    sqlx::query_as(&format!("SELECT {WORKER_COLS} FROM workers WHERE id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

pub async fn worker_by_token_hash(db: &PgPool, hash: &str) -> sqlx::Result<Option<WorkerRow>> {
    sqlx::query_as(&format!(
        "SELECT {WORKER_COLS} FROM workers WHERE token_hash = $1 AND revoked_at IS NULL"
    ))
    .bind(hash)
    .fetch_optional(db)
    .await
}

pub async fn workers_of(db: &PgPool, owner: Uuid) -> sqlx::Result<Vec<WorkerRow>> {
    sqlx::query_as(&format!(
        "SELECT {WORKER_COLS} FROM workers WHERE owner_id = $1 AND revoked_at IS NULL ORDER BY created_at"
    ))
    .bind(owner)
    .fetch_all(db)
    .await
}

/// Store the copy of the worker's local config from `hello` (display only, SPEC §3 Worker)
/// and its `boot_id`; returns the `boot_id` of the previous `hello` (SPEC §5.2, §17.3).
/// `None`: the worker was revoked after its token was checked.
pub async fn worker_hello(
    db: &PgPool,
    id: Uuid,
    h: &hqc_proto::frame::Hello,
) -> sqlx::Result<Option<Option<Uuid>>> {
    sqlx::query_scalar(
        "WITH old AS (SELECT boot_id FROM workers WHERE id = $1)
         UPDATE workers SET protocol_version = $2, capabilities = $3, environments = $4,
            env_bindings = $5, allowed_roots = $6, allow_foreign_sessions = $7,
            share_local_models = $8, local_models = $9, boot_id = $10, last_seen = now(),
            machine_hash = COALESCE($11, machine_hash), updated_at = now()
         WHERE id = $1 AND revoked_at IS NULL
         RETURNING (SELECT boot_id FROM old)",
    )
    .bind(id)
    .bind(h.protocol_version as i32)
    .bind(&h.capabilities)
    .bind(serde_json::to_value(&h.environments).unwrap_or_default())
    .bind(serde_json::to_value(&h.env_bindings).unwrap_or_default())
    .bind(serde_json::to_value(&h.allowed_roots).unwrap_or_default())
    .bind(h.allow_foreign_sessions)
    .bind(h.share_local_models)
    .bind(serde_json::to_value(&h.local_models).unwrap_or_default())
    .bind(h.boot_id)
    .bind(h.machine_id.as_deref().and_then(machine_hash))
    .fetch_optional(db)
    .await
}

/// Creates the service session of a worker that has none (SPEC §10.2): in the owner's
/// personal project, titled with the worker's name, no workspace, `runtime: none`. Returns
/// its id; `None` if the owner has no personal project (cannot happen after login).
async fn create_system_session(
    tx: &mut sqlx::PgConnection,
    worker_id: Uuid,
) -> sqlx::Result<Option<Uuid>> {
    let row: Option<(Option<Uuid>, Uuid, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT w.system_session_id, w.owner_id, w.name, u.personal_project_id
         FROM workers w JOIN users u ON u.id = w.owner_id WHERE w.id = $1 FOR UPDATE OF w",
    )
    .bind(worker_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((existing, owner, name, project)) = row else {
        return Ok(None);
    };
    if existing.is_some() {
        return Ok(existing);
    }
    let Some(project) = project else {
        return Ok(None);
    };
    let id = new_id();
    sqlx::query(
        "INSERT INTO sessions (id, project_id, owner_id, worker_id, title, title_source, slug,
            runtime, system)
         VALUES ($1, $2, $3, $4, $5, 'user', $6, 'none', true)",
    )
    .bind(id)
    .bind(project)
    .bind(owner)
    .bind(worker_id)
    .bind(&name)
    .bind(format!("worker-{worker_id}"))
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE workers SET system_session_id = $2, updated_at = now() WHERE id = $1")
        .bind(worker_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(Some(id))
}

/// The worker's service session, created if it has none yet (SPEC §10.2).
pub async fn ensure_system_session(db: &PgPool, worker_id: Uuid) -> sqlx::Result<Option<Uuid>> {
    let existing: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT system_session_id FROM workers WHERE id = $1")
            .bind(worker_id)
            .fetch_optional(db)
            .await?;
    match existing {
        None => Ok(None),
        Some(Some(id)) => Ok(Some(id)),
        Some(None) => {
            let mut tx = db.begin().await?;
            let id = create_system_session(&mut tx, worker_id).await?;
            tx.commit().await?;
            Ok(id)
        }
    }
}

/// The worker restarted: none of its agents runs any more (SPEC §17.3).
pub async fn worker_restarted(db: &PgPool, worker_id: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE sessions SET process_state = 'stopped', permission_capped_by = NULL, subagents = 0,
            locked_by = CASE WHEN locked_by = 'hqc' THEN 'none' ELSE locked_by END
         WHERE worker_id = $1",
    )
    .bind(worker_id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn worker_seen(db: &PgPool, id: Uuid) -> sqlx::Result<()> {
    sqlx::query("UPDATE workers SET last_seen = now() WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Revoke: the token stops working, the row stays for history (SPEC §5.1, §20).
pub async fn revoke_worker(db: &PgPool, id: Uuid, owner: Uuid) -> sqlx::Result<bool> {
    let r = sqlx::query(
        "UPDATE workers SET token_hash = NULL, revoked_at = now(), updated_at = now()
         WHERE id = $1 AND owner_id = $2 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(owner)
    .execute(db)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// The owner's workers, revoked ones too, oldest first.
pub async fn all_workers_of(db: &PgPool, owner: Uuid) -> sqlx::Result<Vec<WorkerRow>> {
    sqlx::query_as(&format!(
        "SELECT {WORKER_COLS} FROM workers WHERE owner_id = $1 ORDER BY created_at"
    ))
    .bind(owner)
    .fetch_all(db)
    .await
}

const LIVE_SESSIONS_OF_WORKER: &str = "SELECT count(*) FROM sessions
    WHERE NOT system AND NOT archived AND (worker_id = $1
        OR workspace_id IN (SELECT id FROM workspaces WHERE worker_id = $1))";

/// Sessions on the worker that still work there: neither archived nor its service
/// session. They keep a worker from being deleted (SPEC §6).
pub async fn live_sessions_of_worker(db: &PgPool, worker_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar(LIVE_SESSIONS_OF_WORKER)
        .bind(worker_id)
        .fetch_one(db)
        .await
}

pub enum WorkerPurge {
    Deleted,
    /// Sessions that are not archived still run there.
    HasSessions(i64),
}

/// Deletes a revoked worker with its service session and workspace records (SPEC §6,
/// §17.4). Refused while a session that is not archived works on it; archived ones lose
/// their worker and workspace and stay as history. A deleted default workspace is replaced
/// by the project's oldest remaining one.
pub async fn purge_worker(db: &PgPool, id: Uuid) -> sqlx::Result<WorkerPurge> {
    let mut tx = db.begin().await?;
    sqlx::query("SELECT id FROM workers WHERE id = $1 FOR UPDATE")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let live: i64 = sqlx::query_scalar(LIVE_SESSIONS_OF_WORKER)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    if live > 0 {
        return Ok(WorkerPurge::HasSessions(live));
    }
    let projects: Vec<Uuid> =
        sqlx::query_scalar("SELECT DISTINCT project_id FROM workspaces WHERE worker_id = $1")
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;
    sqlx::query("DELETE FROM sessions WHERE worker_id = $1 AND system")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM workspaces WHERE worker_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM workers WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE projects p SET default_workspace_id = (
            SELECT id FROM workspaces WHERE project_id = p.id ORDER BY created_at, id LIMIT 1),
            updated_at = now()
         WHERE p.id = ANY($1) AND p.default_workspace_id IS NULL",
    )
    .bind(&projects)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(WorkerPurge::Deleted)
}

/// Archives the service session of a revoked worker (SPEC §10.2), so the "Workers" group
/// leaves it out; its history stays.
pub async fn archive_system_session(db: &PgPool, session_id: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE sessions SET archived = true, archived_at = now(), turn_open = false,
            updated_at = now()
         WHERE id = $1 AND system",
    )
    .bind(session_id)
    .execute(db)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------- enrollment

#[derive(Debug, Clone, FromRow)]
pub struct EnrollRow {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub os: String,
    pub expires_at: DateTime<Utc>,
    pub confirmed_by: Option<Uuid>,
    pub worker_id: Option<Uuid>,
}

pub struct NewEnroll<'a> {
    pub code: &'a str,
    pub poll_token_hash: &'a str,
    pub name: &'a str,
    pub os: &'a str,
    pub expires_at: DateTime<Utc>,
    pub country: Option<&'a str>,
    pub client_ip: &'a str,
    pub machine_hash: Option<&'a str>,
}

/// A machine fingerprint as stored: 64 lowercase hex digits (sha256), else nothing.
pub fn machine_hash(v: &str) -> Option<String> {
    let v = v.trim().to_ascii_lowercase();
    (v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit())).then_some(v)
}

pub async fn enroll_create(db: &PgPool, e: NewEnroll<'_>) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM enroll_codes WHERE expires_at < now()")
        .execute(db)
        .await?;
    sqlx::query(
        "INSERT INTO enroll_codes (id, code, poll_token_hash, name, os, expires_at, country,
            client_ip, machine_hash)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(new_id())
    .bind(e.code)
    .bind(e.poll_token_hash)
    .bind(e.name)
    .bind(e.os)
    .bind(e.expires_at)
    .bind(e.country)
    .bind(e.client_ip)
    .bind(e.machine_hash)
    .execute(db)
    .await?;
    Ok(())
}

/// Unconfirmed live codes: `(in total, from client_ip)`.
pub async fn enroll_active_count(db: &PgPool, client_ip: &str) -> sqlx::Result<(i64, i64)> {
    sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE client_ip = $1) FROM enroll_codes
         WHERE expires_at > now() AND confirmed_by IS NULL",
    )
    .bind(client_ip)
    .fetch_one(db)
    .await
}

/// A live unconfirmed code, for the confirmation screen (SPEC §6), with the machine
/// fingerprint the enrolling worker sent.
pub async fn enroll_info(
    db: &PgPool,
    code: &str,
) -> sqlx::Result<Option<(hqc_proto::api::EnrollInfo, Option<String>)>> {
    type Row = (
        String,
        String,
        DateTime<Utc>,
        Option<String>,
        Option<String>,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT name, os, created_at, country, machine_hash FROM enroll_codes
         WHERE code = $1 AND expires_at > now() AND confirmed_by IS NULL",
    )
    .bind(code)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(name, os, created_at, country, machine)| {
        (
            hqc_proto::api::EnrollInfo {
                name,
                os,
                created_at,
                country,
                replaceable: vec![],
            },
            machine,
        )
    }))
}

pub enum EnrollConfirm {
    /// No live unconfirmed code.
    NoCode,
    /// `replace_worker_id` is not a worker of the user.
    UnknownWorker,
    /// Another confirmed code is already replacing that worker.
    ReplaceBusy,
    Confirmed {
        worker_id: Uuid,
        name: String,
        /// With a replacement: the old name and the service session (SPEC §5.1).
        replaced: Option<(String, Option<Uuid>)>,
    },
}

/// Confirm a code. Without `replace`: creates the worker row (without a token yet) owned by
/// `user`, with its service session. With `replace` (the user's worker, checked offline by
/// the caller): the code gets that worker's id; its token stops working now, the name and
/// OS become the enrolling worker's, a revocation is lifted and the service session is
/// unarchived and renamed; sessions and workspaces stay (SPEC §5.1). The new token goes to
/// the row on poll.
pub async fn enroll_confirm(
    db: &PgPool,
    code: &str,
    user: Uuid,
    name: Option<&str>,
    replace: Option<Uuid>,
) -> sqlx::Result<EnrollConfirm> {
    let mut tx = db.begin().await?;
    let Some(e) = sqlx::query_as::<_, EnrollRow>(
        "SELECT id, code, name, os, expires_at, confirmed_by, worker_id FROM enroll_codes
         WHERE code = $1 AND expires_at > now() AND confirmed_by IS NULL FOR UPDATE",
    )
    .bind(code)
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(EnrollConfirm::NoCode);
    };
    let name = name.map(str::to_string).unwrap_or(e.name);
    let (wid, replaced) = match replace {
        None => {
            let wid = new_id();
            sqlx::query(
                "INSERT INTO workers (id, owner_id, name, os, machine_hash)
                 SELECT $1, $2, $3, $4, machine_hash FROM enroll_codes WHERE id = $5",
            )
            .bind(wid)
            .bind(user)
            .bind(&name)
            .bind(&e.os)
            .bind(e.id)
            .execute(&mut *tx)
            .await?;
            create_system_session(&mut tx, wid).await?;
            (wid, None)
        }
        Some(wid) => {
            let Some(old_name) = sqlx::query_scalar::<_, String>(
                "SELECT name FROM workers WHERE id = $1 AND owner_id = $2 FOR UPDATE",
            )
            .bind(wid)
            .bind(user)
            .fetch_optional(&mut *tx)
            .await?
            else {
                return Ok(EnrollConfirm::UnknownWorker);
            };
            let busy: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM enroll_codes
                    WHERE worker_id = $1 AND confirmed_by IS NOT NULL AND expires_at > now())",
            )
            .bind(wid)
            .fetch_one(&mut *tx)
            .await?;
            if busy {
                return Ok(EnrollConfirm::ReplaceBusy);
            }
            // `boot_id` NULL: the new process's first `hello` counts as a restart (SPEC §5.2).
            sqlx::query(
                "UPDATE workers SET token_hash = NULL, name = $2, os = $3, revoked_at = NULL,
                    boot_id = NULL, updated_at = now(),
                    machine_hash = COALESCE(
                        (SELECT machine_hash FROM enroll_codes WHERE id = $4), machine_hash)
                 WHERE id = $1",
            )
            .bind(wid)
            .bind(&name)
            .bind(&e.os)
            .bind(e.id)
            .execute(&mut *tx)
            .await?;
            let system = create_system_session(&mut tx, wid).await?;
            sqlx::query(
                "UPDATE sessions SET archived = false, archived_at = NULL, title = $2,
                    updated_at = now()
                 WHERE id = $1",
            )
            .bind(system)
            .bind(&name)
            .execute(&mut *tx)
            .await?;
            (wid, Some((old_name, system)))
        }
    };
    sqlx::query(
        "UPDATE enroll_codes SET confirmed_by = $2, confirmed_at = now(), worker_id = $3,
            replaced_name = $4
         WHERE id = $1",
    )
    .bind(e.id)
    .bind(user)
    .bind(wid)
    .bind(replaced.as_ref().map(|(n, _)| n))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(EnrollConfirm::Confirmed {
        worker_id: wid,
        name,
        replaced,
    })
}

/// The user's workers an enrolling one may replace, with their live session counts (SPEC
/// §5.1); the caller drops the online ones.
pub async fn replace_candidates(db: &PgPool, owner: Uuid) -> sqlx::Result<Vec<(WorkerRow, i64)>> {
    let mut out = vec![];
    for w in all_workers_of(db, owner).await? {
        let n = live_sessions_of_worker(db, w.id).await?;
        out.push((w, n));
    }
    Ok(out)
}

pub enum EnrollPoll {
    Pending,
    /// Token issued; the enrollment row is gone.
    Confirmed {
        worker_id: Uuid,
        /// The replaced worker's former name (SPEC §5.1).
        replaced: Option<String>,
    },
    Gone,
}

/// The worker polls with its secret; once confirmed, the hub stores `token_hash` and the
/// enrollment is consumed, so the token is handed out exactly once.
pub async fn enroll_poll(
    db: &PgPool,
    poll_token_hash: &str,
    token_hash: &str,
) -> sqlx::Result<EnrollPoll> {
    let mut tx = db.begin().await?;
    let Some(e) = sqlx::query_as::<_, EnrollRow>(
        "SELECT id, code, name, os, expires_at, confirmed_by, worker_id FROM enroll_codes
         WHERE poll_token_hash = $1 FOR UPDATE",
    )
    .bind(poll_token_hash)
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(EnrollPoll::Gone);
    };
    let Some(worker_id) = e.worker_id else {
        if e.expires_at < Utc::now() {
            sqlx::query("DELETE FROM enroll_codes WHERE id = $1")
                .bind(e.id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return Ok(EnrollPoll::Gone);
        }
        return Ok(EnrollPoll::Pending);
    };
    let replaced: Option<String> =
        sqlx::query_scalar("SELECT replaced_name FROM enroll_codes WHERE id = $1")
            .bind(e.id)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query("UPDATE workers SET token_hash = $2, updated_at = now() WHERE id = $1")
        .bind(worker_id)
        .bind(token_hash)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM enroll_codes WHERE id = $1")
        .bind(e.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(EnrollPoll::Confirmed {
        worker_id,
        replaced,
    })
}

// ---------------------------------------------------------------- workspaces

#[derive(Debug, Clone, FromRow)]
pub struct WorkspaceRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub owner_id: Uuid,
    pub worker_id: Uuid,
    pub path: String,
    pub label: String,
    pub environment: String,
    /// A worktree `hqc_spawn` made for its session (T47).
    pub ephemeral: bool,
    /// Ephemeral, ready and without a session that is not archived (T47): computed.
    pub marked_for_deletion: bool,
    pub created_by_hqc: bool,
    pub kind: String,
    pub mirror_enabled: bool,
    /// How many hqc rules the folder has (T48).
    pub hqc_rules: i32,
    /// `creating` | `ready` | `failed` (T45).
    pub state: String,
    /// Why the worker could not make the folder (`failed`).
    pub error: Option<String>,
    /// What `workspace.create` clones (T45), repeated after `hello` while `creating`.
    pub clone_url: Option<String>,
    /// The clone's branch, or the worktree's (T46).
    pub branch: Option<String>,
    /// A worktree's origin workspace (T46).
    pub origin_workspace_id: Option<Uuid>,
    /// The worktree creates `branch` (T46).
    pub new_branch: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const WORKSPACE_COLS: &str = "id, project_id, owner_id, worker_id, path, label, environment, \
    ephemeral, created_by_hqc, kind, mirror_enabled, \
    (ephemeral AND state = 'ready' AND NOT EXISTS (SELECT 1 FROM sessions s \
        WHERE s.workspace_id = workspaces.id AND NOT s.archived)) AS marked_for_deletion, \
    (SELECT count(*)::int FROM workspace_rules r WHERE r.workspace_id = workspaces.id) AS hqc_rules, \
    state, error, clone_url, branch, origin_workspace_id, new_branch, created_at, updated_at";

impl WorkspaceRow {
    pub fn state(&self) -> WorkspaceState {
        WorkspaceState::parse(&self.state).unwrap_or_default()
    }

    pub fn to_api(&self) -> hqc_proto::api::Workspace {
        hqc_proto::api::Workspace {
            id: self.id,
            project_id: self.project_id,
            worker_id: self.worker_id,
            path: self.path.clone(),
            label: self.label.clone(),
            environment: self.environment.clone(),
            kind: self.kind.clone(),
            origin_workspace_id: self.origin_workspace_id,
            branch: self.branch.clone(),
            ephemeral: self.ephemeral,
            marked_for_deletion: self.marked_for_deletion,
            created_by_hqc: self.created_by_hqc,
            mirror_enabled: self.mirror_enabled,
            hqc_rules: self.hqc_rules.max(0) as u32,
            state: self.state(),
            error: self.error.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// The worker's workspaces whose folder exists (`ready`), oldest first.
pub async fn workspaces_of_worker(db: &PgPool, worker_id: Uuid) -> sqlx::Result<Vec<WorkspaceRow>> {
    sqlx::query_as(&format!(
        "SELECT {WORKSPACE_COLS} FROM workspaces WHERE worker_id = $1 AND state = 'ready'
         ORDER BY created_at, id"
    ))
    .bind(worker_id)
    .fetch_all(db)
    .await
}

/// The worker's clones that wait for `workspace.created` (T45).
pub async fn creating_workspaces(db: &PgPool, worker_id: Uuid) -> sqlx::Result<Vec<WorkspaceRow>> {
    sqlx::query_as(&format!(
        "SELECT {WORKSPACE_COLS} FROM workspaces WHERE worker_id = $1 AND state = 'creating'
         ORDER BY created_at, id"
    ))
    .bind(worker_id)
    .fetch_all(db)
    .await
}

/// The folder is ready at `canonical` (`workspace.created`); `None` if the workspace is not
/// `creating` (any more).
pub async fn workspace_created(
    db: &PgPool,
    id: Uuid,
    canonical: &str,
) -> sqlx::Result<Option<WorkspaceRow>> {
    sqlx::query_as(&format!(
        "UPDATE workspaces SET state = 'ready', path = $2, error = NULL, updated_at = now()
         WHERE id = $1 AND state = 'creating' RETURNING {WORKSPACE_COLS}"
    ))
    .bind(id)
    .bind(canonical)
    .fetch_optional(db)
    .await
}

/// The worker could not make the folder; `None` if the workspace is not `creating`.
pub async fn workspace_failed(
    db: &PgPool,
    id: Uuid,
    error: &str,
) -> sqlx::Result<Option<WorkspaceRow>> {
    sqlx::query_as(&format!(
        "UPDATE workspaces SET state = 'failed', error = $2, updated_at = now()
         WHERE id = $1 AND state = 'creating' RETURNING {WORKSPACE_COLS}"
    ))
    .bind(id)
    .bind(error)
    .fetch_optional(db)
    .await
}

/// One of hqc's allow rules of a workspace (T48).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RuleRow {
    pub id: Uuid,
    pub rule: String,
    pub created_at: DateTime<Utc>,
}

impl RuleRow {
    pub fn to_api(&self) -> hqc_proto::api::WorkspaceRule {
        hqc_proto::api::WorkspaceRule {
            id: self.id,
            rule: self.rule.clone(),
            created_at: self.created_at,
        }
    }
}

/// The workspace's hqc rules, newest first.
pub async fn workspace_rules(db: &PgPool, workspace_id: Uuid) -> sqlx::Result<Vec<RuleRow>> {
    sqlx::query_as(
        "SELECT id, rule, created_at FROM workspace_rules WHERE workspace_id = $1
         ORDER BY created_at DESC, id DESC",
    )
    .bind(workspace_id)
    .fetch_all(db)
    .await
}

/// The rule texts in the order the worker gets them (oldest first).
pub async fn workspace_rule_texts(db: &PgPool, workspace_id: Uuid) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT rule FROM workspace_rules WHERE workspace_id = $1 ORDER BY created_at, id",
    )
    .bind(workspace_id)
    .fetch_all(db)
    .await
}

/// Add rules the workspace does not have yet; `false` (and nothing added) when the folder
/// would have more than `max`. Serialized per workspace by locking its row.
pub async fn add_workspace_rules(
    db: &PgPool,
    workspace_id: Uuid,
    rules: &[String],
    max: usize,
) -> sqlx::Result<bool> {
    let mut tx = db.begin().await?;
    sqlx::query("SELECT 1 FROM workspaces WHERE id = $1 FOR UPDATE")
        .bind(workspace_id)
        .execute(&mut *tx)
        .await?;
    let have: Vec<String> =
        sqlx::query_scalar("SELECT rule FROM workspace_rules WHERE workspace_id = $1")
            .bind(workspace_id)
            .fetch_all(&mut *tx)
            .await?;
    let mut new: Vec<&String> = Vec::new();
    for r in rules {
        if !have.contains(r) && !new.contains(&r) {
            new.push(r);
        }
    }
    if have.len() + new.len() > max {
        return Ok(false);
    }
    for r in new {
        sqlx::query("INSERT INTO workspace_rules (id, workspace_id, rule) VALUES ($1, $2, $3)")
            .bind(new_id())
            .bind(workspace_id)
            .bind(r)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(true)
}

/// `false`: the workspace has no such rule.
pub async fn delete_workspace_rule(
    db: &PgPool,
    workspace_id: Uuid,
    rule_id: Uuid,
) -> sqlx::Result<bool> {
    let r = sqlx::query("DELETE FROM workspace_rules WHERE workspace_id = $1 AND id = $2")
        .bind(workspace_id)
        .bind(rule_id)
        .execute(db)
        .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn workspace(db: &PgPool, id: Uuid) -> sqlx::Result<Option<WorkspaceRow>> {
    sqlx::query_as(&format!(
        "SELECT {WORKSPACE_COLS} FROM workspaces WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Oldest first: the order the user added them in.
pub async fn workspaces_of_project(db: &PgPool, project: Uuid) -> sqlx::Result<Vec<WorkspaceRow>> {
    sqlx::query_as(&format!(
        "SELECT {WORKSPACE_COLS} FROM workspaces WHERE project_id = $1 ORDER BY created_at, id"
    ))
    .bind(project)
    .fetch_all(db)
    .await
}

pub async fn workspaces_of_owner(db: &PgPool, owner: Uuid) -> sqlx::Result<Vec<WorkspaceRow>> {
    sqlx::query_as(&format!(
        "SELECT {WORKSPACE_COLS} FROM workspaces WHERE owner_id = $1 ORDER BY created_at, id"
    ))
    .bind(owner)
    .fetch_all(db)
    .await
}

/// `workers.env_bindings` as stored from `hello`; anything unreadable is no binding.
fn env_bindings(v: Value) -> Vec<hqc_proto::frame::EnvBinding> {
    serde_json::from_value(v).unwrap_or_default()
}

/// Sets `environment` of the worker's workspaces from the bindings of its `hello` (SPEC §3
/// Workspace, §14: a display; the worker resolves sessions' environments itself). Returns how
/// many changed.
pub async fn set_workspace_environments(
    db: &PgPool,
    worker_id: Uuid,
    bindings: &[hqc_proto::frame::EnvBinding],
) -> sqlx::Result<usize> {
    let rows: Vec<(Uuid, String, String)> =
        sqlx::query_as("SELECT id, path, environment FROM workspaces WHERE worker_id = $1")
            .bind(worker_id)
            .fetch_all(db)
            .await?;
    let mut changed = 0;
    for (id, path, old) in rows {
        let env = hqc_proto::frame::environment_for(bindings, &path);
        if env != old {
            sqlx::query("UPDATE workspaces SET environment = $2, updated_at = now() WHERE id = $1")
                .bind(id)
                .bind(env)
                .execute(db)
                .await?;
            changed += 1;
        }
    }
    Ok(changed)
}

/// Two folders overlap when they are the same or one lies inside the other (compared by
/// path components, so `/a/b` and `/a/bc` do not).
pub fn paths_overlap(a: &str, b: &str) -> bool {
    let (a, b) = (std::path::Path::new(a), std::path::Path::new(b));
    a.starts_with(b) || b.starts_with(a)
}

pub struct NewWorkspace<'a> {
    pub project_id: Uuid,
    pub owner_id: Uuid,
    pub worker_id: Uuid,
    /// Canonical, as the worker reported it in `fs.stat`.
    pub path: &'a str,
    pub label: &'a str,
    /// On by default since T21 (SPEC §9.2).
    pub mirror_enabled: bool,
    pub source: Source<'a>,
    /// The id of an ephemeral worktree of `hqc_spawn`, derived from the call so a repeat finds
    /// it (T47); `None` for every other workspace.
    pub ephemeral: Option<Uuid>,
}

/// What a new workspace is: an existing folder, or one hqc makes, `creating` and
/// `created_by_hqc` until the worker answers.
#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    Dir,
    /// T45.
    Clone {
        url: &'a str,
        branch: Option<&'a str>,
    },
    /// T46.
    Worktree {
        origin: Uuid,
        branch: &'a str,
        new_branch: bool,
    },
}

pub enum WorkspaceInsert {
    Created(WorkspaceRow),
    /// The path equals, lies inside or contains this workspace of the same worker.
    Conflict(WorkspaceRow),
}

/// Adds a workspace unless it overlaps one of the same worker (SPEC §3 Workspace: a folder
/// belongs to one project). Additions per worker are serialized by an advisory lock, so two
/// concurrent requests cannot both pass the check. The project's first workspace becomes
/// its `default_workspace_id`.
pub async fn create_workspace(db: &PgPool, n: NewWorkspace<'_>) -> sqlx::Result<WorkspaceInsert> {
    let mut tx = db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 14))")
        .bind(n.worker_id)
        .execute(&mut *tx)
        .await?;
    let existing: Vec<WorkspaceRow> = sqlx::query_as(&format!(
        "SELECT {WORKSPACE_COLS} FROM workspaces WHERE worker_id = $1 ORDER BY created_at, id"
    ))
    .bind(n.worker_id)
    .fetch_all(&mut *tx)
    .await?;
    if let Some(w) = existing
        .into_iter()
        .find(|w| paths_overlap(&w.path, n.path))
    {
        return Ok(WorkspaceInsert::Conflict(w));
    }
    let bindings: Option<Value> =
        sqlx::query_scalar("SELECT env_bindings FROM workers WHERE id = $1")
            .bind(n.worker_id)
            .fetch_optional(&mut *tx)
            .await?;
    let bindings = env_bindings(bindings.unwrap_or_default());
    let (kind, url, branch, origin, new_branch) = match n.source {
        Source::Dir => ("dir", None, None, None, false),
        Source::Clone { url, branch } => ("clone", Some(url), branch, None, false),
        Source::Worktree {
            origin,
            branch,
            new_branch,
        } => ("worktree", None, Some(branch), Some(origin), new_branch),
    };
    let made = !matches!(n.source, Source::Dir);
    let row: WorkspaceRow = sqlx::query_as(&format!(
        "INSERT INTO workspaces (id, project_id, owner_id, worker_id, path, label, mirror_enabled,
            environment, kind, created_by_hqc, state, clone_url, branch, origin_workspace_id,
            new_branch, ephemeral)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
         RETURNING {WORKSPACE_COLS}"
    ))
    .bind(n.ephemeral.unwrap_or_else(new_id))
    .bind(n.project_id)
    .bind(n.owner_id)
    .bind(n.worker_id)
    .bind(n.path)
    .bind(n.label)
    .bind(n.mirror_enabled)
    .bind(hqc_proto::frame::environment_for(&bindings, n.path))
    .bind(kind)
    .bind(made)
    .bind(
        if made {
            WorkspaceState::Creating
        } else {
            WorkspaceState::Ready
        }
        .as_str(),
    )
    .bind(url)
    .bind(branch)
    .bind(origin)
    .bind(new_branch)
    .bind(n.ephemeral.is_some())
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE projects SET default_workspace_id = $2, updated_at = now()
         WHERE id = $1 AND default_workspace_id IS NULL",
    )
    .bind(n.project_id)
    .bind(row.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(WorkspaceInsert::Created(row))
}

pub async fn set_workspace_label(db: &PgPool, id: Uuid, label: &str) -> sqlx::Result<WorkspaceRow> {
    sqlx::query_as(&format!(
        "UPDATE workspaces SET label = $2, updated_at = now() WHERE id = $1 RETURNING {WORKSPACE_COLS}"
    ))
    .bind(id)
    .bind(label)
    .fetch_one(db)
    .await
}

/// Sessions that are not archived in the workspace.
pub async fn live_sessions_in_workspace(db: &PgPool, id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM sessions WHERE workspace_id = $1 AND NOT archived")
        .bind(id)
        .fetch_one(db)
        .await
}

/// The sessions that work in the workspace, not archived, oldest first.
pub async fn live_session_ids_in_workspace(db: &PgPool, id: Uuid) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "SELECT id FROM sessions WHERE workspace_id = $1 AND NOT archived ORDER BY created_at, id",
    )
    .bind(id)
    .fetch_all(db)
    .await
}

/// Worktree records made from the workspace (T46), in any state.
pub async fn worktrees_of(db: &PgPool, id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM workspaces WHERE origin_workspace_id = $1")
        .bind(id)
        .fetch_one(db)
        .await
}

pub enum WorkspaceDelete {
    Deleted,
    /// Sessions that are not archived still run there.
    InUse(i64),
    /// Worktrees of it still exist (T46).
    HasWorktrees(i64),
}

/// Deletes the record only; the folder is not touched (SPEC §13.2). Refused while a
/// session that is not archived uses it (archived ones lose their workspace) and while
/// worktrees of it exist. A default workspace is replaced by the project's oldest remaining
/// one.
pub async fn delete_workspace(db: &PgPool, ws: &WorkspaceRow) -> sqlx::Result<WorkspaceDelete> {
    let mut tx = db.begin().await?;
    // Serialized with new workspaces of the worker, so no worktree of it appears meanwhile.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 14))")
        .bind(ws.worker_id)
        .execute(&mut *tx)
        .await?;
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sessions WHERE workspace_id = $1 AND NOT archived",
    )
    .bind(ws.id)
    .fetch_one(&mut *tx)
    .await?;
    if live > 0 {
        return Ok(WorkspaceDelete::InUse(live));
    }
    let worktrees: i64 =
        sqlx::query_scalar("SELECT count(*) FROM workspaces WHERE origin_workspace_id = $1")
            .bind(ws.id)
            .fetch_one(&mut *tx)
            .await?;
    if worktrees > 0 {
        return Ok(WorkspaceDelete::HasWorktrees(worktrees));
    }
    sqlx::query("DELETE FROM workspaces WHERE id = $1")
        .bind(ws.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE projects SET default_workspace_id = (
            SELECT id FROM workspaces WHERE project_id = $1 ORDER BY created_at, id LIMIT 1),
            updated_at = now()
         WHERE id = $1 AND default_workspace_id IS NULL",
    )
    .bind(ws.project_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(WorkspaceDelete::Deleted)
}

// ---------------------------------------------------------------- sessions

#[derive(Debug, Clone, FromRow)]
pub struct SessionRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub owner_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub worker_id: Option<Uuid>,
    pub cwd: String,
    pub title: String,
    pub title_source: String,
    pub slug: String,
    pub auto_approve: bool,
    pub wake_on_send: bool,
    pub locked_by: String,
    pub last_seq: i64,
    pub archived: bool,
    /// When it was archived (T50); null for a live session and for one archived before
    /// migration `0026`.
    pub archived_at: Option<DateTime<Utc>>,
    pub runtime: String,
    pub acp_agent: Option<String>,
    pub model: Option<String>,
    pub runtime_session_id: Option<String>,
    pub process_state: String,
    pub system_prompt: Option<String>,
    pub disallowed_tools: Value,
    pub restricted: bool,
    pub start_failures: i32,
    pub turn_open: bool,
    pub last_turn_error: bool,
    pub start_sent: bool,
    /// The service session of `worker_id` (SPEC §10.2).
    pub system: bool,
    /// The last mirrored event written while VS Code was active (SPEC §9.3).
    pub vscode_active_at: Option<DateTime<Utc>>,
    /// `{entrypoint, status}` of a live process outside hqc holding the transcript open
    /// (SPEC §9.3), and since when.
    pub held_by: Option<Value>,
    pub vscode_held_at: Option<DateTime<Utc>>,
    /// The runtime's slash commands (`session.state.commands`, SPEC §5.4): a JSON array.
    pub commands: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The session whose `hqc_spawn` started this one (SPEC §3 Session); null when that
    /// session was deleted.
    pub spawned_by: Option<Uuid>,
    /// `ask` | `auto`: the mode hqc asks the worker for (T38); null — the worker's default.
    pub permission_mode: Option<String>,
    /// The mode the worker last reported (`session.state.permission_mode`).
    pub effective_permission_mode: Option<String>,
    /// The address whose `ask` caps the current turn (T39, `session.state.capped_input`).
    pub permission_capped_by: Option<String>,
    /// The subagents the agent runs now (`session.state.subagents`).
    pub subagents: i32,
}

const SESSION_COLS: &str = "s.id, s.project_id, s.owner_id, s.workspace_id, s.worker_id, s.cwd, \
    s.title, s.title_source, s.slug, s.auto_approve, s.wake_on_send, s.locked_by, s.last_seq, s.archived, s.archived_at, \
    s.runtime, s.acp_agent, s.model, s.runtime_session_id, s.process_state, s.system_prompt, \
    s.disallowed_tools, s.restricted, s.start_failures, s.turn_open, s.last_turn_error, \
    s.start_sent, s.system, s.vscode_active_at, s.held_by, s.vscode_held_at, s.commands, s.created_at, \
    s.updated_at, s.spawned_by, s.permission_mode, s.effective_permission_mode, \
    s.permission_capped_by, s.subagents";

pub async fn session(db: &PgPool, id: Uuid) -> sqlx::Result<Option<SessionRow>> {
    sqlx::query_as(&format!(
        "SELECT {SESSION_COLS} FROM sessions s WHERE s.id = $1"
    ))
    .bind(id)
    .fetch_optional(db)
    .await
}

pub struct NewSession<'a> {
    pub project_id: Uuid,
    pub owner_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub worker_id: Option<Uuid>,
    pub cwd: &'a str,
    /// `None`: the default title "Session N", N = the session's number in the project (the
    /// service sessions of workers do not count, SPEC §10.2).
    pub title: Option<&'a str>,
    pub model: Option<&'a str>,
    pub auto_approve: bool,
    /// `None`: the worker's `[permissions] mode` (T38).
    pub permission_mode: Option<PermissionMode>,
}

/// What `hqc_spawn` records on the session it creates.
#[derive(Debug, Clone, Copy)]
pub struct Spawn {
    /// Derived from the call (`mcp::once_id`): a repeated call finds the session.
    pub id: Uuid,
    pub by: Uuid,
    pub chain_id: Uuid,
    /// The N of "Session N" chosen before the session, when its worktree's branch was named
    /// after the slug (T47); `None`: counted now.
    pub number: Option<i64>,
}

/// Creates the session with a slug unique in the project (SPEC §3 Session) and a
/// hub-assigned `runtime_session_id` for Claude Code (SPEC §8.1).
pub async fn create_session(db: &PgPool, n: NewSession<'_>) -> sqlx::Result<Uuid> {
    create_spawned_session(db, n, None).await
}

/// [`create_session`], recording `hqc_spawn`'s [`Spawn`] when given. A session with the
/// spawn's id already there is a unique violation.
pub async fn create_spawned_session(
    db: &PgPool,
    n: NewSession<'_>,
    spawn: Option<Spawn>,
) -> sqlx::Result<Uuid> {
    let (title, source, base) = match n.title {
        Some(t) => (t.to_string(), TitleSource::User, slugify(t)),
        None => {
            let num = match spawn.and_then(|s| s.number) {
                Some(num) => num,
                None => next_session_number(db, n.project_id).await?,
            };
            (
                format!("Session {num}"),
                TitleSource::Default,
                format!("session-{num}"),
            )
        }
    };
    for i in 1.. {
        let slug = if i == 1 {
            base.clone()
        } else {
            format!("{base}-{i}")
        };
        let id = spawn.map_or_else(new_id, |s| s.id);
        let r = sqlx::query(
            "INSERT INTO sessions (id, project_id, owner_id, workspace_id, worker_id, cwd, title, slug,
                model, auto_approve, runtime, runtime_session_id, title_source, spawned_by,
                spawn_chain_id, permission_mode)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'claude_code', $11, $12, $13, $14,
                $15)
             ON CONFLICT (project_id, slug) DO NOTHING",
        )
        .bind(id)
        .bind(n.project_id)
        .bind(n.owner_id)
        .bind(n.workspace_id)
        .bind(n.worker_id)
        .bind(n.cwd)
        .bind(&title)
        .bind(&slug)
        .bind(n.model)
        .bind(n.auto_approve)
        .bind(Uuid::new_v4().to_string())
        .bind(source.as_str())
        .bind(spawn.map(|s| s.by))
        .bind(spawn.map(|s| s.chain_id))
        .bind(n.permission_mode.map(PermissionMode::as_str))
        .execute(db)
        .await?;
        if r.rows_affected() == 1 {
            return Ok(id);
        }
    }
    unreachable!()
}

/// N of the default title "Session N": the session's number in the project, the workers'
/// service sessions left out (SPEC §3 Session).
pub async fn next_session_number(db: &PgPool, project_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT count(*) + 1 FROM sessions WHERE project_id = $1 AND NOT system")
        .bind(project_id)
        .fetch_one(db)
        .await
}

pub async fn sessions_of_owner(db: &PgPool, owner: Uuid) -> sqlx::Result<Vec<SessionRow>> {
    sqlx::query_as(&format!(
        "SELECT {SESSION_COLS} FROM sessions s WHERE s.owner_id = $1 ORDER BY s.updated_at DESC"
    ))
    .bind(owner)
    .fetch_all(db)
    .await
}

pub async fn sessions_of_project(db: &PgPool, project: Uuid) -> sqlx::Result<Vec<SessionRow>> {
    sqlx::query_as(&format!(
        "SELECT {SESSION_COLS} FROM sessions s WHERE s.project_id = $1 ORDER BY s.updated_at DESC"
    ))
    .bind(project)
    .fetch_all(db)
    .await
}

/// The sessions a project's list shows (T50): unarchived, not a worker's service session,
/// the newest first.
pub async fn live_session_ids(db: &PgPool, project: Uuid) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "SELECT id FROM sessions WHERE project_id = $1 AND NOT archived AND NOT system
         ORDER BY created_at DESC, id DESC",
    )
    .bind(project)
    .fetch_all(db)
    .await
}

/// The user's order record of the project's list: `(mode, session_ids)` as stored.
pub async fn session_order(
    db: &PgPool,
    user: Uuid,
    project: Uuid,
) -> sqlx::Result<Option<(String, Vec<Uuid>)>> {
    sqlx::query_as(
        "SELECT mode, session_ids FROM session_orders WHERE user_id = $1 AND project_id = $2",
    )
    .bind(user)
    .bind(project)
    .fetch_optional(db)
    .await
}

/// Sets the mode, the order or both; a field left `None` keeps its stored value.
pub async fn put_session_order(
    db: &PgPool,
    user: Uuid,
    project: Uuid,
    mode: Option<&str>,
    session_ids: Option<&[Uuid]>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO session_orders (user_id, project_id, mode, session_ids)
         VALUES ($1, $2, COALESCE($3, 'state'), COALESCE($4, '{}'))
         ON CONFLICT (user_id, project_id) DO UPDATE SET
            mode = COALESCE($3, session_orders.mode),
            session_ids = COALESCE($4, session_orders.session_ids),
            updated_at = now()",
    )
    .bind(user)
    .bind(project)
    .bind(mode)
    .bind(session_ids)
    .execute(db)
    .await?;
    Ok(())
}

/// A title from the user (`PATCH`): the runtime's never replaces it.
pub async fn set_user_title(db: &PgPool, id: Uuid, title: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE sessions SET title = $2, title_source = 'user', updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(title)
    .execute(db)
    .await?;
    Ok(())
}

/// The runtime's slash commands (`session.state.commands`, T27); written only when they
/// changed, so a `session.state` without a new list does not touch `updated_at`.
pub async fn set_commands(db: &PgPool, id: Uuid, commands: &[String]) -> sqlx::Result<()> {
    let v = serde_json::to_value(commands).unwrap_or(Value::Null);
    sqlx::query(
        "UPDATE sessions SET commands = $2
         WHERE id = $1 AND commands IS DISTINCT FROM $2",
    )
    .bind(id)
    .bind(v)
    .execute(db)
    .await?;
    Ok(())
}

/// The mode the worker reports the session's agent runs in (T38).
pub async fn set_effective_permission_mode(
    db: &PgPool,
    id: Uuid,
    mode: PermissionMode,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE sessions SET effective_permission_mode = $2
         WHERE id = $1 AND effective_permission_mode IS DISTINCT FROM $2",
    )
    .bind(id)
    .bind(mode.as_str())
    .execute(db)
    .await?;
    Ok(())
}

/// The subagents the worker reports the session's agent runs now; written only when the
/// count changed.
pub async fn set_subagents(db: &PgPool, id: Uuid, n: u32) -> sqlx::Result<()> {
    sqlx::query("UPDATE sessions SET subagents = $2 WHERE id = $1 AND subagents <> $2")
        .bind(id)
        .bind(i32::try_from(n).unwrap_or(i32::MAX))
        .execute(db)
        .await?;
    Ok(())
}

/// Who caps the session's current turn (T39): `capped_by` of the chain of the session's
/// input `capped_input`, as the worker reports it, or of the send buffer it carried when its
/// own chain has no `ask` cap (T40, `input_caps`); `None` clears it.
pub async fn set_permission_capped_by(
    db: &PgPool,
    id: Uuid,
    capped_input: Option<Uuid>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE sessions s SET permission_capped_by = c.by
         FROM (SELECT CASE WHEN $2::uuid IS NULL THEN NULL ELSE
                 COALESCE((SELECT CASE WHEN e.chain ->> 'permission_cap' = 'ask'
                                       THEN e.chain ->> 'capped_by' ELSE i.capped_by END
                           FROM events e LEFT JOIN input_caps i ON i.event_id = e.event_id
                           WHERE e.event_id = $2 AND e.session_id = $1), '?') END AS by) c
         WHERE s.id = $1 AND s.permission_capped_by IS DISTINCT FROM c.by",
    )
    .bind(id)
    .bind(capped_input)
    .execute(db)
    .await?;
    Ok(())
}

/// The mode hqc asks the worker for (T38, `PATCH /api/sessions/{id}`).
pub async fn set_permission_mode(db: &PgPool, id: Uuid, mode: PermissionMode) -> sqlx::Result<()> {
    sqlx::query("UPDATE sessions SET permission_mode = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(mode.as_str())
        .execute(db)
        .await?;
    Ok(())
}

/// The session's model (`PATCH /api/sessions/{id}`); `None` — the runtime's default.
pub async fn set_model(db: &PgPool, id: Uuid, model: Option<&str>) -> sqlx::Result<()> {
    sqlx::query("UPDATE sessions SET model = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(model)
        .execute(db)
        .await?;
    Ok(())
}

/// A title from the runtime (`session.state.title`), applied unless the user set one.
pub async fn set_runtime_title(db: &PgPool, id: Uuid, title: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE sessions SET title = $2, title_source = 'runtime', updated_at = now()
         WHERE id = $1 AND title_source <> 'user' AND (title <> $2 OR title_source <> 'runtime')",
    )
    .bind(id)
    .bind(title)
    .execute(db)
    .await?;
    Ok(())
}

/// A title the hub derived itself (T21, SPEC §3 Session): only over the default "Session N",
/// never over the user's or the runtime's. `false` if the session has another title by now.
pub async fn set_generated_title(db: &PgPool, id: Uuid, title: &str) -> sqlx::Result<bool> {
    let r = sqlx::query(
        "UPDATE sessions SET title = $2, title_source = 'runtime', updated_at = now()
         WHERE id = $1 AND title_source = 'default'",
    )
    .bind(id)
    .bind(title)
    .execute(db)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Text of the session's first event of `kind` (with `final: true` for `agent.message`).
pub async fn first_text(db: &PgPool, id: Uuid, kind: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT payload->>'text' FROM events
         WHERE session_id = $1 AND type = $2
           AND ($2 <> 'agent.message' OR payload->>'final' = 'true')
           AND coalesce(payload->>'text', '') <> ''
         ORDER BY seq LIMIT 1",
    )
    .bind(id)
    .bind(kind)
    .fetch_optional(db)
    .await
}

/// The session's inbox folders on the worker (`session.cleanup`, T21): those of its
/// `user.message` and `peer.message` events that carried artifacts (T41: a buffered message's
/// files have a folder of their own), and those of the replies with files to its asks, named
/// by the ask's `request_id` (T41).
pub async fn inbox_events(db: &PgPool, id: Uuid) -> sqlx::Result<Vec<Uuid>> {
    let mut ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT event_id FROM events
         WHERE session_id = $1 AND type IN ('user.message', 'peer.message')
           AND jsonb_typeof(payload->'artifact_ids') = 'array'
           AND jsonb_array_length(payload->'artifact_ids') > 0
         ORDER BY seq",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    let asks: Vec<String> = sqlx::query_scalar(
        "SELECT request_id FROM pending_requests
         WHERE session_id = $1 AND kind = 'ask'
           AND jsonb_typeof(result->'payload'->'result'->'files') = 'array'
         ORDER BY closed_at",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    // The worker names the folder by its call's id, a uuid.
    ids.extend(asks.iter().filter_map(|r| r.parse::<Uuid>().ok()));
    Ok(ids)
}

pub fn slugify(title: &str) -> String {
    slugify_or(title, "session")
}

/// Lowercase ASCII letters and digits joined by `-`, at most ~48 characters; `fallback`
/// when nothing is left (a title in Cyrillic, say).
pub fn slugify_or(title: &str, fallback: &str) -> String {
    let mut s = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.is_empty() && !s.ends_with('-') {
            s.push('-');
        }
        if s.len() >= 48 {
            break;
        }
    }
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() {
        fallback.into()
    } else {
        s
    }
}

pub async fn sessions_of_worker(db: &PgPool, worker_id: Uuid) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar("SELECT id FROM sessions WHERE worker_id = $1")
        .bind(worker_id)
        .fetch_all(db)
        .await
}

/// Sessions of the worker with input waiting for delivery (SPEC §5.2: re-sent after `hello`).
pub async fn sessions_with_pending_input(db: &PgPool, worker_id: Uuid) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "SELECT s.id FROM sessions s WHERE s.worker_id = $1 AND NOT s.archived
           AND EXISTS (SELECT 1 FROM events e WHERE e.session_id = s.id AND e.delivery = 'pending')
         ORDER BY s.id",
    )
    .bind(worker_id)
    .fetch_all(db)
    .await
}

/// Sessions that get `session.start` (`mode: resume`) after a reconnect: pending input, an
/// open approval / clarify, or an answer the worker has not acknowledged (SPEC §5.2).
pub async fn sessions_to_restart(db: &PgPool, worker_id: Uuid) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "SELECT s.id FROM sessions s WHERE s.worker_id = $1 AND NOT s.archived
           AND s.locked_by <> 'vscode'
           AND (EXISTS (SELECT 1 FROM events e WHERE e.session_id = s.id AND e.delivery = 'pending')
             OR EXISTS (SELECT 1 FROM pending_requests p WHERE p.session_id = s.id
                          AND p.kind IN ('approval', 'clarify')
                          AND (p.closed_at IS NULL
                               OR (p.result IS NOT NULL AND p.result_delivered_at IS NULL))))
         ORDER BY s.id",
    )
    .bind(worker_id)
    .fetch_all(db)
    .await
}

/// Marks that `session.start` was sent; returns whether one had been sent before (SPEC §5.3).
pub async fn mark_start_sent(db: &PgPool, id: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "WITH old AS (SELECT start_sent FROM sessions WHERE id = $1)
         UPDATE sessions SET start_sent = true WHERE id = $1
         RETURNING (SELECT start_sent FROM old)",
    )
    .bind(id)
    .fetch_one(db)
    .await
}

/// A new token for the session's next `session.start`; the one before stays valid for the
/// `mcp.call`s already on their way (SPEC §5.2).
pub async fn set_session_token_hash(db: &PgPool, id: Uuid, hash: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE sessions SET prev_session_token_hash = session_token_hash, session_token_hash = $2
         WHERE id = $1",
    )
    .bind(id)
    .bind(hash)
    .execute(db)
    .await?;
    Ok(())
}

/// The session of `worker_id` whose latest or previous token hashes to `hash` (SPEC §5.2,
/// `mcp.call`); the service session has no agent and never matches.
pub async fn session_by_token_hash(
    db: &PgPool,
    worker_id: Uuid,
    hash: &str,
) -> sqlx::Result<Option<SessionRow>> {
    sqlx::query_as(&format!(
        "SELECT {SESSION_COLS} FROM sessions s
         WHERE (s.session_token_hash = $2 OR s.prev_session_token_hash = $2)
           AND s.worker_id = $1 AND NOT s.system"
    ))
    .bind(worker_id)
    .bind(hash)
    .fetch_optional(db)
    .await
}

pub async fn bump_start_failures(db: &PgPool, id: Uuid) -> sqlx::Result<i32> {
    sqlx::query_scalar(
        "UPDATE sessions SET start_failures = start_failures + 1, updated_at = now()
         WHERE id = $1 RETURNING start_failures",
    )
    .bind(id)
    .fetch_one(db)
    .await
}

pub async fn reset_start_failures(db: &PgPool, id: Uuid) -> sqlx::Result<()> {
    sqlx::query("UPDATE sessions SET start_failures = 0 WHERE id = $1 AND start_failures <> 0")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_turn(db: &PgPool, id: Uuid, open: bool, error: bool) -> sqlx::Result<()> {
    sqlx::query("UPDATE sessions SET turn_open = $2, last_turn_error = $3, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(open)
        .bind(error)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn clear_turn_error(db: &PgPool, id: Uuid) -> sqlx::Result<()> {
    sqlx::query("UPDATE sessions SET last_turn_error = false WHERE id = $1 AND last_turn_error")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Sessions whose computed state is `running` apart from worker presence (SPEC §17.1): a
/// turn is open and nothing ranks above it. The caller checks that the worker is online.
/// A session waiting for the answer to its `hqc_ask` is not silent but waiting, and is left
/// out too (SPEC §7.1, §17.3).
pub async fn running_turns(db: &PgPool) -> sqlx::Result<Vec<(Uuid, Option<Uuid>)>> {
    sqlx::query_as(
        "SELECT s.id, s.worker_id FROM sessions s
         WHERE s.turn_open AND NOT s.archived AND NOT s.last_turn_error AND s.locked_by <> 'vscode'
           AND NOT EXISTS (SELECT 1 FROM pending_requests p WHERE p.session_id = s.id
                           AND p.closed_at IS NULL AND p.kind IN ('approval', 'clarify', 'ask'))
           AND NOT EXISTS (SELECT 1 FROM events e WHERE e.session_id = s.id AND e.delivery = 'failed')",
    )
    .fetch_all(db)
    .await
}

/// The final `agent.message` answering input `input_id` is stored already.
pub async fn turn_answered(db: &PgPool, session_id: Uuid, input_id: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM events WHERE session_id = $1 AND type = 'agent.message'
            AND payload->>'final' = 'true' AND payload->>'in_reply_to' = $2)",
    )
    .bind(session_id)
    .bind(input_id.to_string())
    .fetch_one(db)
    .await
}

pub async fn set_process_state(
    db: &PgPool,
    id: Uuid,
    process_state: &str,
    runtime_session_id: Option<&str>,
    locked_by: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE sessions SET process_state = $2,
            runtime_session_id = COALESCE($3, runtime_session_id), locked_by = $4,
            vscode_active_at = CASE WHEN $4 = 'vscode' AND locked_by <> 'vscode'
                THEN now() ELSE vscode_active_at END,
            updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(process_state)
    .bind(runtime_session_id)
    .bind(locked_by)
    .execute(db)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------- mirror (SPEC §9)

/// The session of transcript `runtime_session_id` on this worker.
pub async fn session_by_runtime(
    db: &PgPool,
    worker_id: Uuid,
    runtime_session_id: &str,
) -> sqlx::Result<Option<SessionRow>> {
    sqlx::query_as(&format!(
        "SELECT {SESSION_COLS} FROM sessions s
         WHERE s.worker_id = $1 AND s.runtime_session_id = $2
         ORDER BY s.created_at LIMIT 1"
    ))
    .bind(worker_id)
    .bind(runtime_session_id)
    .fetch_optional(db)
    .await
}

pub async fn workspace_by_path(
    db: &PgPool,
    worker_id: Uuid,
    path: &str,
) -> sqlx::Result<Option<WorkspaceRow>> {
    sqlx::query_as(&format!(
        "SELECT {WORKSPACE_COLS} FROM workspaces WHERE worker_id = $1 AND path = $2"
    ))
    .bind(worker_id)
    .bind(path)
    .fetch_optional(db)
    .await
}

pub async fn mirrored_workspaces(db: &PgPool, worker_id: Uuid) -> sqlx::Result<Vec<WorkspaceRow>> {
    sqlx::query_as(&format!(
        "SELECT {WORKSPACE_COLS} FROM workspaces WHERE worker_id = $1 AND mirror_enabled
         AND state = 'ready' ORDER BY created_at, id"
    ))
    .bind(worker_id)
    .fetch_all(db)
    .await
}

pub async fn set_workspace_mirror(db: &PgPool, id: Uuid, on: bool) -> sqlx::Result<WorkspaceRow> {
    sqlx::query_as(&format!(
        "UPDATE workspaces SET mirror_enabled = $2, updated_at = now() WHERE id = $1
         RETURNING {WORKSPACE_COLS}"
    ))
    .bind(id)
    .bind(on)
    .fetch_one(db)
    .await
}

/// Most sessions one `mirror.set` lists (the most recently active first), so that the frame
/// stays well under the WebSocket message limit (SPEC §20).
pub const MAX_MIRROR_SINCE: i64 = 5000;

/// `mirror.set.sessions` (T21, SPEC §9.1): the workspace's sessions with a transcript and the
/// time of the last event the hub has of each.
pub async fn mirror_since(
    db: &PgPool,
    workspace_id: Uuid,
) -> sqlx::Result<Vec<(String, DateTime<Utc>)>> {
    sqlx::query_as(
        "SELECT s.runtime_session_id, max(e.ts) AS since FROM sessions s
         JOIN events e ON e.session_id = s.id
         WHERE s.workspace_id = $1 AND s.runtime_session_id IS NOT NULL AND NOT s.system
         GROUP BY s.runtime_session_id ORDER BY since DESC LIMIT $2",
    )
    .bind(workspace_id)
    .bind(MAX_MIRROR_SINCE)
    .fetch_all(db)
    .await
}

/// A session found by the mirror: it runs transcript `runtime_session_id`; `locked_by` and,
/// for a live transcript, `vscode_active_at` by SPEC §9.1.
pub async fn adopt_transcript(
    db: &PgPool,
    id: Uuid,
    runtime_session_id: &str,
    locked_by: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE sessions SET runtime_session_id = $2, locked_by = $3, imported = true,
            vscode_active_at = CASE WHEN $3 = 'vscode' THEN now() END, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(runtime_session_id)
    .bind(locked_by)
    .execute(db)
    .await?;
    Ok(())
}

/// Sessions the mirror created: of the worker in the last 24 h, and in the workspace in
/// total (SPEC §20).
pub async fn imported_counts(
    db: &PgPool,
    worker_id: Uuid,
    workspace_id: Uuid,
) -> sqlx::Result<(i64, i64)> {
    sqlx::query_as(
        "SELECT
            (SELECT count(*) FROM sessions WHERE imported AND worker_id = $1
                AND created_at > now() - interval '24 hours'),
            (SELECT count(*) FROM sessions WHERE imported AND workspace_id = $2)",
    )
    .bind(worker_id)
    .bind(workspace_id)
    .fetch_one(db)
    .await
}

/// `SET` clause for a new `locked_by` in `$2`: a session that becomes VS Code's gets
/// `vscode_active_at`, which takeover counts its 30 s from (SPEC §9.3).
const STAMP_VSCODE: &str = "vscode_active_at = CASE WHEN $2 = 'vscode' AND locked_by <> 'vscode'
    THEN now() ELSE vscode_active_at END";

pub async fn set_locked_by(db: &PgPool, id: Uuid, locked_by: &str) -> sqlx::Result<()> {
    sqlx::query(&format!(
        "UPDATE sessions SET locked_by = $2, {STAMP_VSCODE}, updated_at = now() WHERE id = $1"
    ))
    .bind(id)
    .bind(locked_by)
    .execute(db)
    .await?;
    Ok(())
}

/// The holder of the session's transcript (SPEC §9.3); `vscode_held_at` keeps the time the
/// transcript was first held.
pub async fn set_holder(db: &PgPool, id: Uuid, held_by: Option<&Value>) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE sessions SET held_by = $2,
            vscode_held_at = CASE WHEN $2::jsonb IS NULL THEN NULL
                                  ELSE COALESCE(vscode_held_at, now()) END,
            updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(held_by)
    .execute(db)
    .await?;
    Ok(())
}

/// On `hello` the worker reports its holders again; what it reported before is forgotten
/// (the lock stays). Returns the sessions that had a holder.
pub async fn forget_holders(db: &PgPool, worker_id: Uuid) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "UPDATE sessions SET held_by = NULL, vscode_held_at = NULL
         WHERE worker_id = $1 AND held_by IS NOT NULL RETURNING id",
    )
    .bind(worker_id)
    .fetch_all(db)
    .await
}

/// The worker reads Claude Code's registry of live sessions
/// (`capabilities.claude_code.sessions_registry`, SPEC §5.4, §9.3).
pub async fn worker_has_registry(db: &PgPool, worker_id: Uuid) -> sqlx::Result<bool> {
    let v: Option<bool> = sqlx::query_scalar(
        "SELECT COALESCE((capabilities->'claude_code'->>'sessions_registry')::boolean, false)
         FROM workers WHERE id = $1",
    )
    .bind(worker_id)
    .fetch_optional(db)
    .await?;
    Ok(v.unwrap_or(false))
}

pub async fn touch_vscode(db: &PgPool, id: Uuid) -> sqlx::Result<()> {
    sqlx::query("UPDATE sessions SET vscode_active_at = now() WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------- events

#[derive(Debug, Clone, FromRow)]
pub struct EventRow {
    pub event_id: Uuid,
    pub session_id: Uuid,
    pub seq: i64,
    pub ts: DateTime<Utc>,
    pub author: Value,
    #[sqlx(rename = "type")]
    pub kind: String,
    pub payload: Value,
    pub chain: Option<Value>,
    pub client_op_id: Option<String>,
    pub delivery: Option<String>,
    pub delivered_at: Option<DateTime<Utc>>,
}

const EVENT_COLS: &str = "event_id, session_id, seq, ts, author, type, payload, chain, \
    client_op_id, delivery, delivered_at";

impl EventRow {
    /// The event as clients get it (SPEC §3 Event): stored `author` and `payload` verbatim.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert(
            "session_id".into(),
            Value::String(self.session_id.to_string()),
        );
        m.insert("event_id".into(), Value::String(self.event_id.to_string()));
        m.insert("seq".into(), Value::from(self.seq));
        m.insert(
            "ts".into(),
            serde_json::to_value(self.ts).unwrap_or_default(),
        );
        m.insert("author".into(), self.author.clone());
        m.insert("type".into(), Value::String(self.kind.clone()));
        m.insert("payload".into(), self.payload.clone());
        if let Some(c) = &self.chain {
            m.insert("chain".into(), c.clone());
        }
        if let Some(c) = &self.client_op_id {
            m.insert("client_op_id".into(), Value::String(c.clone()));
        }
        if let Some(d) = &self.delivery {
            m.insert("delivery".into(), Value::String(d.clone()));
        }
        if let Some(t) = self.delivered_at {
            m.insert(
                "delivered_at".into(),
                serde_json::to_value(t).unwrap_or_default(),
            );
        }
        Value::Object(m)
    }
}

/// Events as clients get them ([`EventRow::to_json`]), a final `agent.message` that answered an
/// `hqc_ask` with `reply_delivered_at` once the asking worker acknowledged it (SPEC §6, T51).
pub async fn events_for_clients(db: &PgPool, rows: &[EventRow]) -> sqlx::Result<Vec<Value>> {
    let replies: Vec<String> = rows
        .iter()
        .filter(|r| {
            r.kind == "agent.message"
                && r.payload["final"] == Value::Bool(true)
                && r.payload["in_reply_to"].is_string()
        })
        .map(|r| r.event_id.to_string())
        .collect();
    let delivered: std::collections::HashMap<Uuid, DateTime<Utc>> = if replies.is_empty() {
        Default::default()
    } else {
        sqlx::query_as::<_, (Uuid, DateTime<Utc>)>(
            "SELECT (payload ->> 'reply_event_id')::uuid,
                    (payload ->> 'reply_delivered_at')::timestamptz
             FROM pending_requests
             WHERE kind = 'ask' AND payload ->> 'reply_event_id' = ANY($1)
               AND payload ? 'reply_delivered_at'",
        )
        .bind(&replies)
        .fetch_all(db)
        .await?
        .into_iter()
        .collect()
    };
    Ok(rows
        .iter()
        .map(|r| {
            let mut v = r.to_json();
            if let Some(t) = delivered.get(&r.event_id) {
                v["reply_delivered_at"] = serde_json::to_value(t).unwrap_or_default();
            }
            v
        })
        .collect())
}

#[derive(Debug, Clone)]
pub struct NewEvent {
    pub event_id: Uuid,
    pub session_id: Uuid,
    pub ts: DateTime<Utc>,
    pub author: Value,
    pub kind: String,
    pub payload: Value,
    pub chain: Option<Value>,
    pub client_op_id: Option<String>,
    pub delivery: Option<String>,
    pub delivered_at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
pub enum Inserted {
    New(Box<EventRow>),
    /// Same `event_id` already stored (SPEC §5.2): its `seq`.
    Duplicate(i64),
    UnknownSession,
}

/// Whether `e` is the database refusing the data itself (it would fail the same way on every
/// retry) rather than the database being unreachable or busy: SQLSTATE classes 22 (data
/// exception: encoding, value out of range, string too long), 23 (integrity constraint) and
/// 54 (program limit: index row or value too big), or a value the driver could not encode.
pub fn is_data_error(e: &anyhow::Error) -> bool {
    e.chain()
        .filter_map(|c| c.downcast_ref::<sqlx::Error>())
        .any(|e| match e {
            sqlx::Error::Database(d) => d
                .code()
                .is_some_and(|c| c.starts_with("22") || c.starts_with("23") || c.starts_with("54")),
            sqlx::Error::Encode(_) => true,
            _ => false,
        })
}

/// Append an event, assigning the next `seq` of its session (SPEC §5.2). The session row
/// lock serializes appends per session; the unique `event_id` drops duplicates.
pub async fn insert_event(db: &PgPool, e: &NewEvent) -> sqlx::Result<Inserted> {
    Ok(insert(db, e, None)
        .await?
        .expect("an event that closes no request is never refused"))
}

/// [`insert_event`] of a worker's cancel of its open request `request_id` of `kind`, closed
/// in the same transaction: the cancel is stored if and only if it closed the request (SPEC
/// §10.1). `None` when the request was not open and this cancel is not stored.
pub async fn insert_worker_cancel(
    db: &PgPool,
    e: &NewEvent,
    kind: &str,
    request_id: &str,
) -> sqlx::Result<Option<Inserted>> {
    insert(db, e, Some((kind, request_id))).await
}

async fn insert(
    db: &PgPool,
    e: &NewEvent,
    closes: Option<(&str, &str)>,
) -> sqlx::Result<Option<Inserted>> {
    if let Some(seq) = existing_seq(db, e.event_id).await? {
        return Ok(Some(Inserted::Duplicate(seq)));
    }
    let mut tx = db.begin().await?;
    if let Some((kind, request_id)) = closes {
        let r = sqlx::query(
            "UPDATE pending_requests SET closed_at = now(), updated_at = now()
             WHERE session_id = $1 AND kind = $2 AND request_id = $3 AND closed_at IS NULL",
        )
        .bind(e.session_id)
        .bind(kind)
        .bind(request_id)
        .execute(&mut *tx)
        .await?;
        if r.rows_affected() == 0 {
            tx.rollback().await?;
            // The same cancel, sent again, may have closed it meanwhile.
            return Ok(existing_seq(db, e.event_id).await?.map(Inserted::Duplicate));
        }
    }
    let Some(seq): Option<i64> = sqlx::query_scalar(
        "UPDATE sessions SET last_seq = last_seq + 1, updated_at = now() WHERE id = $1 RETURNING last_seq",
    )
    .bind(e.session_id)
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(Some(Inserted::UnknownSession));
    };
    let row = sqlx::query_as::<_, EventRow>(&format!(
        "INSERT INTO events ({EVENT_COLS}) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         ON CONFLICT (event_id) DO NOTHING RETURNING {EVENT_COLS}"
    ))
    .bind(e.event_id)
    .bind(e.session_id)
    .bind(seq)
    .bind(e.ts)
    .bind(&e.author)
    .bind(&e.kind)
    .bind(&e.payload)
    .bind(&e.chain)
    .bind(&e.client_op_id)
    .bind(&e.delivery)
    .bind(e.delivered_at)
    .fetch_optional(&mut *tx)
    .await?;
    match row {
        Some(row) => {
            tx.commit().await?;
            Ok(Some(Inserted::New(Box::new(row))))
        }
        None => {
            tx.rollback().await?;
            Ok(Some(
                existing_seq(db, e.event_id)
                    .await?
                    .map_or(Inserted::UnknownSession, Inserted::Duplicate),
            ))
        }
    }
}

pub async fn event(db: &PgPool, event_id: Uuid) -> sqlx::Result<Option<EventRow>> {
    sqlx::query_as(&format!(
        "SELECT {EVENT_COLS} FROM events WHERE event_id = $1"
    ))
    .bind(event_id)
    .fetch_optional(db)
    .await
}

async fn existing_seq(db: &PgPool, event_id: Uuid) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar("SELECT seq FROM events WHERE event_id = $1")
        .bind(event_id)
        .fetch_optional(db)
        .await
}

pub async fn event_by_client_op(
    db: &PgPool,
    session_id: Uuid,
    client_op_id: &str,
) -> sqlx::Result<Option<EventRow>> {
    sqlx::query_as(&format!(
        "SELECT {EVENT_COLS} FROM events WHERE session_id = $1 AND client_op_id = $2"
    ))
    .bind(session_id)
    .bind(client_op_id)
    .fetch_optional(db)
    .await
}

pub async fn event_by_id(db: &PgPool, event_id: Uuid) -> sqlx::Result<Option<EventRow>> {
    sqlx::query_as(&format!(
        "SELECT {EVENT_COLS} FROM events WHERE event_id = $1"
    ))
    .bind(event_id)
    .fetch_optional(db)
    .await
}

/// The events with these ids, in no particular order.
pub async fn events_by_ids(db: &PgPool, ids: &[Uuid]) -> sqlx::Result<Vec<EventRow>> {
    sqlx::query_as(&format!(
        "SELECT {EVENT_COLS} FROM events WHERE event_id = ANY($1)"
    ))
    .bind(ids)
    .fetch_all(db)
    .await
}

/// Events with `seq > after_seq`, oldest first.
pub async fn events_after(
    db: &PgPool,
    session_id: Uuid,
    after_seq: i64,
    limit: i64,
) -> sqlx::Result<Vec<EventRow>> {
    sqlx::query_as(&format!(
        "SELECT {EVENT_COLS} FROM events WHERE session_id = $1 AND seq > $2 ORDER BY seq LIMIT $3"
    ))
    .bind(session_id)
    .bind(after_seq)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// The last `limit` events with `seq < before_seq`, oldest first.
pub async fn events_before(
    db: &PgPool,
    session_id: Uuid,
    before_seq: i64,
    limit: i64,
) -> sqlx::Result<Vec<EventRow>> {
    let mut v: Vec<EventRow> = sqlx::query_as(&format!(
        "SELECT {EVENT_COLS} FROM events WHERE session_id = $1 AND seq < $2 ORDER BY seq DESC LIMIT $3"
    ))
    .bind(session_id)
    .bind(before_seq)
    .bind(limit)
    .fetch_all(db)
    .await?;
    v.reverse();
    Ok(v)
}

#[derive(Debug, Clone, FromRow)]
pub struct InputRow {
    pub event_id: Uuid,
    pub seq: i64,
    pub payload: Value,
    pub chain: Option<Value>,
}

/// `pending` input of a session in `seq` order (SPEC §5.2).
pub async fn pending_inputs(db: &PgPool, session_id: Uuid) -> sqlx::Result<Vec<InputRow>> {
    sqlx::query_as(
        "SELECT event_id, seq, payload, chain FROM events
         WHERE session_id = $1 AND delivery = 'pending' ORDER BY seq",
    )
    .bind(session_id)
    .fetch_all(db)
    .await
}

/// Mark delivered; `None` if it was not pending (already delivered, failed, unknown).
pub async fn mark_delivered(db: &PgPool, event_id: Uuid) -> sqlx::Result<Option<EventRow>> {
    sqlx::query_as(&format!(
        "UPDATE events SET delivery = 'delivered', delivered_at = now()
         WHERE event_id = $1 AND delivery = 'pending' RETURNING {EVENT_COLS}"
    ))
    .bind(event_id)
    .fetch_optional(db)
    .await
}

/// Move all input of a session from `from` to `to` delivery.
pub async fn move_inputs(
    db: &PgPool,
    session_id: Uuid,
    from: &str,
    to: &str,
) -> sqlx::Result<Vec<EventRow>> {
    sqlx::query_as(&format!(
        "UPDATE events SET delivery = $3 WHERE session_id = $1 AND delivery = $2 RETURNING {EVENT_COLS}"
    ))
    .bind(session_id)
    .bind(from)
    .bind(to)
    .fetch_all(db)
    .await
}

pub async fn mark_input_failed(db: &PgPool, event_id: Uuid) -> sqlx::Result<Option<EventRow>> {
    sqlx::query_as(&format!(
        "UPDATE events SET delivery = 'failed' WHERE event_id = $1 AND delivery = 'pending'
         RETURNING {EVENT_COLS}"
    ))
    .bind(event_id)
    .fetch_optional(db)
    .await
}

// ---------------------------------------------------------------- pending requests

#[derive(Debug, Clone, FromRow)]
pub struct PendingRow {
    pub id: Uuid,
    pub kind: String,
    pub session_id: Uuid,
    pub request_id: String,
    pub payload: Value,
    pub expires_at: Option<DateTime<Utc>>,
    pub worker_id: Option<Uuid>,
    pub result: Option<Value>,
    pub closed_at: Option<DateTime<Utc>>,
}

const PENDING_COLS: &str =
    "id, kind, session_id, request_id, payload, expires_at, worker_id, result, closed_at";

/// Stores an open request; `false` if one with the same `(session, kind, request_id)` exists.
pub async fn open_request(
    db: &PgPool,
    kind: &str,
    session_id: Uuid,
    request_id: &str,
    payload: &Value,
    expires_at: Option<DateTime<Utc>>,
    worker_id: Option<Uuid>,
) -> sqlx::Result<bool> {
    let r = sqlx::query(
        "INSERT INTO pending_requests (id, kind, session_id, request_id, payload, expires_at, worker_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (session_id, kind, request_id) DO NOTHING",
    )
    .bind(new_id())
    .bind(kind)
    .bind(session_id)
    .bind(request_id)
    .bind(payload)
    .bind(expires_at)
    .bind(worker_id)
    .execute(db)
    .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn find_open_request(
    db: &PgPool,
    session_id: Uuid,
    request_id: &str,
) -> sqlx::Result<Option<PendingRow>> {
    sqlx::query_as(&format!(
        "SELECT {PENDING_COLS} FROM pending_requests
         WHERE session_id = $1 AND request_id = $2 AND closed_at IS NULL
           AND kind IN ('approval', 'clarify', 'worker_approval')"
    ))
    .bind(session_id)
    .bind(request_id)
    .fetch_optional(db)
    .await
}

/// A request of `kind` with this `request_id` in the session, open or closed.
pub async fn request_by_id(
    db: &PgPool,
    session_id: Uuid,
    kind: &str,
    request_id: &str,
) -> sqlx::Result<Option<PendingRow>> {
    sqlx::query_as(&format!(
        "SELECT {PENDING_COLS} FROM pending_requests
         WHERE session_id = $1 AND kind = $2 AND request_id = $3"
    ))
    .bind(session_id)
    .bind(kind)
    .bind(request_id)
    .fetch_optional(db)
    .await
}

/// Answers to `worker.approval.request` the worker has not acknowledged (SPEC §10.2):
/// sent again after `hello`, since no `session.start` precedes them.
pub async fn undelivered_worker_results(
    db: &PgPool,
    worker_id: Uuid,
) -> sqlx::Result<Vec<PendingRow>> {
    sqlx::query_as(&format!(
        "SELECT {PENDING_COLS} FROM pending_requests
         WHERE kind = 'worker_approval' AND worker_id = $1
           AND result IS NOT NULL AND result_delivered_at IS NULL
         ORDER BY closed_at"
    ))
    .bind(worker_id)
    .fetch_all(db)
    .await
}

/// Open `worker_approval` requests of a worker (SPEC §20).
pub async fn open_worker_requests(db: &PgPool, worker_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT count(*) FROM pending_requests
         WHERE kind = 'worker_approval' AND worker_id = $1 AND closed_at IS NULL",
    )
    .bind(worker_id)
    .fetch_one(db)
    .await
}

pub async fn request_exists(db: &PgPool, session_id: Uuid, request_id: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pending_requests WHERE session_id = $1 AND request_id = $2)",
    )
    .bind(session_id)
    .bind(request_id)
    .fetch_one(db)
    .await
}

/// Close an open request; `false` if someone closed it first (first answer wins, SPEC §10.1).
pub async fn close_request(db: &PgPool, id: Uuid, result: Option<&Value>) -> sqlx::Result<bool> {
    let r = sqlx::query(
        "UPDATE pending_requests SET closed_at = now(), result = $2, updated_at = now()
         WHERE id = $1 AND closed_at IS NULL",
    )
    .bind(id)
    .bind(result)
    .execute(db)
    .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn open_requests_of_session(
    db: &PgPool,
    session_id: Uuid,
) -> sqlx::Result<Vec<PendingRow>> {
    sqlx::query_as(&format!(
        "SELECT {PENDING_COLS} FROM pending_requests
         WHERE session_id = $1 AND closed_at IS NULL AND kind IN ('approval', 'clarify')"
    ))
    .bind(session_id)
    .fetch_all(db)
    .await
}

pub async fn open_requests_of_worker(
    db: &PgPool,
    worker_id: Uuid,
) -> sqlx::Result<Vec<PendingRow>> {
    sqlx::query_as(&format!(
        "SELECT {PENDING_COLS} FROM pending_requests p
         WHERE p.closed_at IS NULL AND p.kind IN ('approval', 'clarify', 'worker_approval')
           AND p.session_id IN (SELECT id FROM sessions WHERE worker_id = $1)"
    ))
    .bind(worker_id)
    .fetch_all(db)
    .await
}

pub async fn expired_requests(db: &PgPool) -> sqlx::Result<Vec<PendingRow>> {
    sqlx::query_as(&format!(
        "SELECT {PENDING_COLS} FROM pending_requests
         WHERE closed_at IS NULL AND expires_at IS NOT NULL AND expires_at <= now()
           AND kind IN ('approval', 'clarify', 'worker_approval')"
    ))
    .fetch_all(db)
    .await
}

/// Results (answers, hub cancellations) not yet acknowledged by the worker.
pub async fn undelivered_results(db: &PgPool, worker_id: Uuid) -> sqlx::Result<Vec<PendingRow>> {
    sqlx::query_as(&format!(
        "SELECT {PENDING_COLS} FROM pending_requests p
         WHERE p.result IS NOT NULL AND p.result_delivered_at IS NULL
           AND p.session_id IN (SELECT id FROM sessions WHERE worker_id = $1)
         ORDER BY p.closed_at"
    ))
    .bind(worker_id)
    .fetch_all(db)
    .await
}

pub async fn result_delivered(db: &PgPool, id: Uuid) -> sqlx::Result<()> {
    sqlx::query("UPDATE pending_requests SET result_delivered_at = now() WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// The worker restarted: answers it never acknowledged went to a process that is gone.
pub async fn drop_undelivered_results(db: &PgPool, worker_id: Uuid) -> sqlx::Result<u64> {
    let r = sqlx::query(
        "UPDATE pending_requests SET result_delivered_at = now()
         WHERE result IS NOT NULL AND result_delivered_at IS NULL
           AND session_id IN (SELECT id FROM sessions WHERE worker_id = $1)",
    )
    .bind(worker_id)
    .execute(db)
    .await?;
    Ok(r.rows_affected())
}

pub async fn pending_by_id(db: &PgPool, id: Uuid) -> sqlx::Result<Option<PendingRow>> {
    sqlx::query_as(&format!(
        "SELECT {PENDING_COLS} FROM pending_requests WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(db)
    .await
}

// ---------------------------------------------------------------- favorites

/// What a user marks as a favorite: a table `<kind>_favorites (user_id, <kind>_id,
/// created_at)`.
#[derive(Debug, Clone, Copy)]
pub enum Favorite {
    Session,
    Project,
}

impl Favorite {
    fn table_and_column(self) -> (&'static str, &'static str) {
        match self {
            Favorite::Session => ("session_favorites", "session_id"),
            Favorite::Project => ("project_favorites", "project_id"),
        }
    }
}

/// Makes `id` one of the user's favorites and returns since when; a repeated call keeps the
/// first time.
pub async fn add_favorite(
    db: &PgPool,
    kind: Favorite,
    user: Uuid,
    id: Uuid,
) -> sqlx::Result<DateTime<Utc>> {
    let (table, column) = kind.table_and_column();
    sqlx::query_scalar(&format!(
        "INSERT INTO {table} (user_id, {column}) VALUES ($1, $2)
         ON CONFLICT (user_id, {column}) DO UPDATE SET created_at = {table}.created_at
         RETURNING created_at"
    ))
    .bind(user)
    .bind(id)
    .fetch_one(db)
    .await
}

/// `id` is no longer the user's favorite; nothing happens if it was not one.
pub async fn remove_favorite(
    db: &PgPool,
    kind: Favorite,
    user: Uuid,
    id: Uuid,
) -> sqlx::Result<()> {
    let (table, column) = kind.table_and_column();
    sqlx::query(&format!(
        "DELETE FROM {table} WHERE user_id = $1 AND {column} = $2"
    ))
    .bind(user)
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// Those of `projects` the user made favorites, with since when.
pub async fn project_favorites(
    db: &PgPool,
    user: Uuid,
    projects: &[Uuid],
) -> sqlx::Result<Vec<(Uuid, DateTime<Utc>)>> {
    sqlx::query_as(
        "SELECT project_id, created_at FROM project_favorites
         WHERE user_id = $1 AND project_id = ANY($2)",
    )
    .bind(user)
    .bind(projects)
    .fetch_all(db)
    .await
}

// ---------------------------------------------------------------- read cursors, push

pub async fn set_read_cursor(
    db: &PgPool,
    user: Uuid,
    session: Uuid,
    seq: i64,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO read_cursors (user_id, session_id, last_read_seq) VALUES ($1, $2, $3)
         ON CONFLICT (user_id, session_id) DO UPDATE
           SET last_read_seq = GREATEST(read_cursors.last_read_seq, EXCLUDED.last_read_seq),
               updated_at = now()
         RETURNING last_read_seq",
    )
    .bind(user)
    .bind(session)
    .bind(seq)
    .fetch_one(db)
    .await
}

#[derive(Debug, Clone, FromRow)]
pub struct PushSubRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

pub enum PushUpsert {
    Stored(Uuid),
    /// The endpoint belongs to another user.
    Foreign,
    /// The user already has `max` other subscriptions.
    Limit,
}

/// Store a subscription, or refresh the keys of the user's own one. Another user's endpoint
/// is never taken over (SPEC §20).
pub async fn upsert_push_sub(
    db: &PgPool,
    user: Uuid,
    endpoint: &str,
    p256dh: &str,
    auth: &str,
    max: i64,
) -> sqlx::Result<PushUpsert> {
    let mut tx = db.begin().await?;
    // Serializes subscribing of one user, so the limit holds under concurrent requests.
    sqlx::query("SELECT 1 FROM users WHERE id = $1 FOR UPDATE")
        .bind(user)
        .execute(&mut *tx)
        .await?;
    let others: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM push_subscriptions WHERE user_id = $1 AND endpoint <> $2",
    )
    .bind(user)
    .bind(endpoint)
    .fetch_one(&mut *tx)
    .await?;
    if others >= max {
        return Ok(PushUpsert::Limit);
    }
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO push_subscriptions (id, user_id, endpoint, p256dh, auth) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (endpoint) DO UPDATE
           SET p256dh = EXCLUDED.p256dh, auth = EXCLUDED.auth, updated_at = now()
           WHERE push_subscriptions.user_id = EXCLUDED.user_id
         RETURNING id",
    )
    .bind(new_id())
    .bind(user)
    .bind(endpoint)
    .bind(p256dh)
    .bind(auth)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id.map_or(PushUpsert::Foreign, PushUpsert::Stored))
}

pub async fn delete_push_sub(
    db: &PgPool,
    user: Option<Uuid>,
    endpoint: &str,
) -> sqlx::Result<bool> {
    let r = sqlx::query(
        "DELETE FROM push_subscriptions WHERE endpoint = $1 AND ($2::uuid IS NULL OR user_id = $2)",
    )
    .bind(endpoint)
    .bind(user)
    .execute(db)
    .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn push_subs_of(db: &PgPool, user: Uuid) -> sqlx::Result<Vec<PushSubRow>> {
    sqlx::query_as(
        "SELECT id, user_id, endpoint, p256dh, auth FROM push_subscriptions WHERE user_id = $1",
    )
    .bind(user)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("Fix the Build!"), "fix-the-build");
        assert_eq!(slugify("  "), "session");
        assert_eq!(slugify("Привет"), "session");
        assert_eq!(slugify("a--b"), "a-b");
    }
}
