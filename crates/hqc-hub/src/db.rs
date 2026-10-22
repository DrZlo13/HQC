//! Postgres access (sqlx, runtime queries). Migrations: `crates/hqc-hub/migrations`.

use chrono::{DateTime, Utc};
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

pub async fn projects_of(db: &PgPool, owner: Uuid) -> sqlx::Result<Vec<ProjectRow>> {
    sqlx::query_as("SELECT * FROM projects WHERE owner_id = $1 ORDER BY created_at")
        .bind(owner)
        .fetch_all(db)
        .await
}

pub async fn project(db: &PgPool, id: Uuid) -> sqlx::Result<Option<ProjectRow>> {
    sqlx::query_as("SELECT * FROM projects WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const WORKER_COLS: &str = "id, owner_id, name, os, revoked_at, last_seen, protocol_version, \
    capabilities, environments, env_bindings, allowed_roots, allow_foreign_sessions, \
    share_local_models, local_models, created_at, updated_at";

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
pub async fn worker_hello(
    db: &PgPool,
    id: Uuid,
    h: &hqc_proto::frame::Hello,
) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar(
        "WITH old AS (SELECT boot_id FROM workers WHERE id = $1)
         UPDATE workers SET protocol_version = $2, capabilities = $3, environments = $4,
            env_bindings = $5, allowed_roots = $6, allow_foreign_sessions = $7,
            share_local_models = $8, local_models = $9, boot_id = $10, last_seen = now(),
            updated_at = now()
         WHERE id = $1
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
    .fetch_one(db)
    .await
}

/// The worker restarted: none of its agents runs any more (SPEC §17.3).
pub async fn worker_restarted(db: &PgPool, worker_id: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE sessions SET process_state = 'stopped',
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

pub async fn enroll_create(
    db: &PgPool,
    code: &str,
    poll_token_hash: &str,
    name: &str,
    os: &str,
    expires_at: DateTime<Utc>,
) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM enroll_codes WHERE expires_at < now()")
        .execute(db)
        .await?;
    sqlx::query(
        "INSERT INTO enroll_codes (id, code, poll_token_hash, name, os, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(new_id())
    .bind(code)
    .bind(poll_token_hash)
    .bind(name)
    .bind(os)
    .bind(expires_at)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn enroll_active_count(db: &PgPool) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT count(*) FROM enroll_codes WHERE expires_at > now() AND confirmed_by IS NULL",
    )
    .fetch_one(db)
    .await
}

/// Confirm a code: creates the worker row (without a token yet) owned by `user`.
pub async fn enroll_confirm(
    db: &PgPool,
    code: &str,
    user: Uuid,
    name: Option<&str>,
) -> sqlx::Result<Option<(Uuid, String)>> {
    let mut tx = db.begin().await?;
    let Some(e) = sqlx::query_as::<_, EnrollRow>(
        "SELECT id, code, name, os, expires_at, confirmed_by, worker_id FROM enroll_codes
         WHERE code = $1 AND expires_at > now() AND confirmed_by IS NULL FOR UPDATE",
    )
    .bind(code)
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(None);
    };
    let name = name.map(str::to_string).unwrap_or(e.name);
    let wid = new_id();
    sqlx::query("INSERT INTO workers (id, owner_id, name, os) VALUES ($1, $2, $3, $4)")
        .bind(wid)
        .bind(user)
        .bind(&name)
        .bind(&e.os)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE enroll_codes SET confirmed_by = $2, confirmed_at = now(), worker_id = $3 WHERE id = $1",
    )
    .bind(e.id)
    .bind(user)
    .bind(wid)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some((wid, name)))
}

pub enum EnrollPoll {
    Pending,
    /// Token issued; the enrollment row is gone.
    Confirmed {
        worker_id: Uuid,
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
    Ok(EnrollPoll::Confirmed { worker_id })
}

// ---------------------------------------------------------------- workspaces

#[derive(Debug, Clone, FromRow)]
pub struct WorkspaceRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub worker_id: Uuid,
    pub path: String,
    pub environment: String,
}

/// Iteration 1: the worker announces its single workspace in `hello`; it lives in the
/// owner's personal project.
pub async fn ensure_workspace(
    db: &PgPool,
    project_id: Uuid,
    owner_id: Uuid,
    worker_id: Uuid,
    path: &str,
) -> sqlx::Result<Uuid> {
    sqlx::query_scalar(
        "INSERT INTO workspaces (id, project_id, owner_id, worker_id, path)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (project_id, worker_id, path) DO UPDATE SET updated_at = now()
         RETURNING id",
    )
    .bind(new_id())
    .bind(project_id)
    .bind(owner_id)
    .bind(worker_id)
    .bind(path)
    .fetch_one(db)
    .await
}

/// The most recently announced workspace of a worker in a project.
pub async fn latest_workspace(
    db: &PgPool,
    project_id: Uuid,
    worker_id: Uuid,
) -> sqlx::Result<Option<WorkspaceRow>> {
    sqlx::query_as(
        "SELECT id, project_id, worker_id, path, environment FROM workspaces
         WHERE project_id = $1 AND worker_id = $2 ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(project_id)
    .bind(worker_id)
    .fetch_optional(db)
    .await
}

pub async fn workspace(db: &PgPool, id: Uuid) -> sqlx::Result<Option<WorkspaceRow>> {
    sqlx::query_as(
        "SELECT id, project_id, worker_id, path, environment FROM workspaces WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
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
    pub slug: String,
    pub auto_approve: bool,
    pub wake_on_send: bool,
    pub locked_by: String,
    pub last_seq: i64,
    pub archived: bool,
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const SESSION_COLS: &str = "s.id, s.project_id, s.owner_id, s.workspace_id, s.worker_id, s.cwd, \
    s.title, s.slug, s.auto_approve, s.wake_on_send, s.locked_by, s.last_seq, s.archived, \
    s.runtime, s.acp_agent, s.model, s.runtime_session_id, s.process_state, s.system_prompt, \
    s.disallowed_tools, s.restricted, s.start_failures, s.turn_open, s.last_turn_error, \
    s.start_sent, s.created_at, s.updated_at";

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
    pub title: &'a str,
    pub model: Option<&'a str>,
    pub auto_approve: bool,
}

/// Creates the session with a slug unique in the project (SPEC §3 Session) and a
/// hub-assigned `runtime_session_id` for Claude Code (SPEC §8.1).
pub async fn create_session(db: &PgPool, n: NewSession<'_>) -> sqlx::Result<Uuid> {
    let base = slugify(n.title);
    for i in 1.. {
        let slug = if i == 1 {
            base.clone()
        } else {
            format!("{base}-{i}")
        };
        let id = new_id();
        let r = sqlx::query(
            "INSERT INTO sessions (id, project_id, owner_id, workspace_id, worker_id, cwd, title, slug,
                model, auto_approve, runtime, runtime_session_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'claude_code', $11)
             ON CONFLICT (project_id, slug) DO NOTHING",
        )
        .bind(id)
        .bind(n.project_id)
        .bind(n.owner_id)
        .bind(n.workspace_id)
        .bind(n.worker_id)
        .bind(n.cwd)
        .bind(n.title)
        .bind(&slug)
        .bind(n.model)
        .bind(n.auto_approve)
        .bind(Uuid::new_v4().to_string())
        .execute(db)
        .await?;
        if r.rows_affected() == 1 {
            return Ok(id);
        }
    }
    unreachable!()
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

pub fn slugify(title: &str) -> String {
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
        "session".into()
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

pub async fn set_session_token_hash(db: &PgPool, id: Uuid, hash: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE sessions SET session_token_hash = $2 WHERE id = $1")
        .bind(id)
        .bind(hash)
        .execute(db)
        .await?;
    Ok(())
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

pub async fn set_process_state(
    db: &PgPool,
    id: Uuid,
    process_state: &str,
    runtime_session_id: Option<&str>,
    locked_by: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE sessions SET process_state = $2,
            runtime_session_id = COALESCE($3, runtime_session_id), locked_by = $4, updated_at = now()
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

/// Append an event, assigning the next `seq` of its session (SPEC §5.2). The session row
/// lock serializes appends per session; the unique `event_id` drops duplicates.
pub async fn insert_event(db: &PgPool, e: &NewEvent) -> sqlx::Result<Inserted> {
    if let Some(seq) = existing_seq(db, e.event_id).await? {
        return Ok(Inserted::Duplicate(seq));
    }
    let mut tx = db.begin().await?;
    let Some(seq): Option<i64> = sqlx::query_scalar(
        "UPDATE sessions SET last_seq = last_seq + 1, updated_at = now() WHERE id = $1 RETURNING last_seq",
    )
    .bind(e.session_id)
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(Inserted::UnknownSession);
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
            Ok(Inserted::New(Box::new(row)))
        }
        None => {
            tx.rollback().await?;
            Ok(existing_seq(db, e.event_id)
                .await?
                .map_or(Inserted::UnknownSession, Inserted::Duplicate))
        }
    }
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
}

/// `pending` input of a session in `seq` order (SPEC §5.2).
pub async fn pending_inputs(db: &PgPool, session_id: Uuid) -> sqlx::Result<Vec<InputRow>> {
    sqlx::query_as(
        "SELECT event_id, seq, payload FROM events
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

pub async fn open_request(
    db: &PgPool,
    kind: &str,
    session_id: Uuid,
    request_id: &str,
    payload: &Value,
    expires_at: Option<DateTime<Utc>>,
    worker_id: Option<Uuid>,
) -> sqlx::Result<()> {
    sqlx::query(
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
    Ok(())
}

pub async fn find_open_request(
    db: &PgPool,
    session_id: Uuid,
    request_id: &str,
) -> sqlx::Result<Option<PendingRow>> {
    sqlx::query_as(&format!(
        "SELECT {PENDING_COLS} FROM pending_requests
         WHERE session_id = $1 AND request_id = $2 AND closed_at IS NULL
           AND kind IN ('approval', 'clarify')"
    ))
    .bind(session_id)
    .bind(request_id)
    .fetch_optional(db)
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

/// The worker closed the request itself (its own response or cancel event).
pub async fn close_request_by_worker(
    db: &PgPool,
    session_id: Uuid,
    kind: &str,
    request_id: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE pending_requests SET closed_at = now(), updated_at = now()
         WHERE session_id = $1 AND kind = $2 AND request_id = $3 AND closed_at IS NULL",
    )
    .bind(session_id)
    .bind(kind)
    .bind(request_id)
    .execute(db)
    .await?;
    Ok(())
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
         WHERE p.closed_at IS NULL AND p.kind IN ('approval', 'clarify')
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
           AND kind IN ('approval', 'clarify')"
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

pub async fn upsert_push_sub(
    db: &PgPool,
    user: Uuid,
    endpoint: &str,
    p256dh: &str,
    auth: &str,
) -> sqlx::Result<Uuid> {
    sqlx::query_scalar(
        "INSERT INTO push_subscriptions (id, user_id, endpoint, p256dh, auth) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (endpoint) DO UPDATE
           SET user_id = EXCLUDED.user_id, p256dh = EXCLUDED.p256dh, auth = EXCLUDED.auth, updated_at = now()
         RETURNING id",
    )
    .bind(new_id())
    .bind(user)
    .bind(endpoint)
    .bind(p256dh)
    .bind(auth)
    .fetch_one(db)
    .await
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
