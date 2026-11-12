//! Scheduled messages and the hub cron (SPEC §3 ScheduledMessage, §16): `hqc_schedule`,
//! `/api/schedules` and the loop that sends them.
//!
//! - A schedule sends `message` into its session as a `user.message` of the owner, with
//!   `schedule_id` in the payload (the client marks it), through the normal delivery path:
//!   `pending`, then `session.input` when the worker is there.
//! - When: once at `at` (RFC 3339), or by `cron` (5 fields, [`croner`]) in `timezone` (IANA,
//!   [`chrono_tz`]; the owner's by default). DST follows Vixie cron: a fixed time in the
//!   skipped hour runs right after the gap, one in the repeated hour runs once.
//! - `next_run` lives in the table, so the loop survives a restart. A run found more than
//!   [`MISSED_AFTER`] late was missed (the hub was down): with `catch_up` it is sent once,
//!   otherwise skipped; either way the next run is the first one after now.
//! - A run is sent at most once: the loop locks the row while it sends, and the event's id is
//!   derived from the schedule and the run time ([`run_event_id`]), so a run repeated after a
//!   crash stores nothing. A paused (`enabled: false`) or deleted schedule never fires; a
//!   deleted session takes its schedules with it (`ON DELETE CASCADE`).
//! - A target that is archived, or driven from VS Code, takes no input (SPEC §6, §9.3): the
//!   run is skipped.
//! - Limits (SPEC §20): [`MAX_ACTIVE_PER_OWNER`] (counted and inserted under the owner's
//!   lock), [`MIN_INTERVAL`], [`MAX_MESSAGE`]; runs sent per owner and hour
//!   (`Limits::schedule_runs_per_owner_hour`, more are skipped).
//! - A schedule an agent creates keeps the chain of its call, moved on like a send (SPEC
//!   §7.1); its runs carry it, so a turn they start continues that chain: an agent cannot
//!   keep itself running for ever by scheduling its next turn. Each run is capped by the
//!   lower of the chain's cap and its creator's mode at run time (T40, [`run_chain`]).

use std::{str::FromStr, sync::Mutex, time::Duration};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, TimeDelta, Utc};
use chrono_tz::Tz;
use croner::{
    parser::{CronParser, Seconds, Year},
    Cron,
};
use hqc_proto::{
    api::{CreateSchedule, PatchSchedule, Schedule, ScheduleList},
    event::Chain,
    frame::{McpCall, McpError, PermissionMode},
    mcp::error_code,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::CurrentUser,
    db::{self, SessionRow},
    delivery,
    error::{ApiError, ApiResult},
    events,
    mcp::{args, internal, Answer},
};

/// Schedules of one owner that may still run (`next_run` set, paused ones included).
pub const MAX_ACTIVE_PER_OWNER: i64 = 100;
/// The shortest gap between two runs of a `cron`.
pub const MIN_INTERVAL: TimeDelta = TimeDelta::minutes(5);
/// A run found this late was missed: the hub was down at its time.
pub const MISSED_AFTER: TimeDelta = TimeDelta::minutes(2);
/// Longest `message`, in bytes.
pub const MAX_MESSAGE: usize = 16 * 1024;
/// How often the loop looks for due runs.
pub const TICK: Duration = Duration::from_secs(10);
/// Runs of a `cron` looked at by the [`MIN_INTERVAL`] check.
const INTERVAL_SAMPLE: usize = 100;

/// The hub's clock for schedules: the system time, or a time a test set.
#[derive(Default)]
pub struct Clock(Mutex<Option<DateTime<Utc>>>);

impl Clock {
    pub fn now(&self) -> DateTime<Utc> {
        self.0.lock().unwrap().unwrap_or_else(Utc::now)
    }

    /// From now on the clock shows `t`, and [`cron_loop`] leaves the runs to the test, which
    /// calls [`run_due`] itself. Set it before creating schedules.
    pub fn set(&self, t: DateTime<Utc>) {
        *self.0.lock().unwrap() = Some(t);
    }

    fn is_set(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }
}

// ---------------------------------------------------------------- when

/// The 5-field parser: no seconds, no year.
fn parse_cron(pattern: &str) -> Result<Cron, String> {
    CronParser::builder()
        .seconds(Seconds::Disallowed)
        .year(Year::Disallowed)
        .build()
        .parse(pattern.trim())
        .map_err(|e| format!("cron {pattern:?}: {e}"))
}

pub fn parse_tz(name: &str) -> Result<Tz, String> {
    Tz::from_str(name.trim()).map_err(|_| format!("unknown time zone {name:?}"))
}

/// The first run of `cron` in `tz` strictly after `after`.
pub fn next_cron(cron: &Cron, tz: Tz, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    cron.find_next_occurrence(&after.with_timezone(&tz), false)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// `cron` checked for [`MIN_INTERVAL`] over its next runs; the first run after `now`.
fn check_cron(cron: &Cron, tz: Tz, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let first = next_cron(cron, tz, now).ok_or("the cron pattern never runs")?;
    let mut prev = first;
    for _ in 1..INTERVAL_SAMPLE {
        let Some(next) = next_cron(cron, tz, prev) else {
            break;
        };
        if next - prev < MIN_INTERVAL {
            return Err(format!(
                "runs {} minutes apart; the minimum is {} minutes",
                (next - prev).num_minutes(),
                MIN_INTERVAL.num_minutes()
            ));
        }
        prev = next;
    }
    Ok(first)
}

/// What a new schedule asks for, before the checks.
struct NewSchedule {
    session_id: Uuid,
    /// The chain of the `hqc_schedule` call; `None` for a person's schedule.
    chain: Option<Chain>,
    /// The session whose agent made it; `None` for a person's schedule.
    created_by: Option<Uuid>,
    cron: Option<String>,
    at: Option<DateTime<Utc>>,
    timezone: String,
    message: String,
    catch_up: bool,
}

enum Refused {
    Invalid(String),
    Limit(String),
    Internal(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for Refused {
    fn from(e: E) -> Self {
        Refused::Internal(e.into())
    }
}

impl Refused {
    fn mcp(self) -> McpError {
        match self {
            Refused::Invalid(m) => McpError::new(error_code::INVALID_ARGS, m),
            Refused::Limit(m) => McpError::new(error_code::LIMIT_EXCEEDED, m),
            Refused::Internal(e) => internal(e),
        }
    }

    fn api(self) -> ApiError {
        match self {
            Refused::Invalid(m) => ApiError::BadRequest(m),
            Refused::Limit(m) => ApiError::Conflict("schedule_limit", m),
            Refused::Internal(e) => ApiError::Internal(e),
        }
    }
}

/// Checks `n` and stores it under `id` for `owner`; a schedule with that id already there
/// (a repeated `mcp.call`) is returned as it is.
async fn create(
    app: &AppState,
    owner: Uuid,
    id: Uuid,
    n: NewSchedule,
) -> Result<ScheduleRow, Refused> {
    if let Some(row) = row(app, id).await? {
        return Ok(row);
    }
    if n.message.trim().is_empty() {
        return Err(Refused::Invalid("message is empty".into()));
    }
    if n.message.len() > MAX_MESSAGE {
        return Err(Refused::Invalid(format!(
            "message is longer than {MAX_MESSAGE} bytes"
        )));
    }
    let tz = parse_tz(&n.timezone).map_err(Refused::Invalid)?;
    let now = app.clock.now();
    let next_run = match (&n.cron, n.at) {
        (Some(c), None) => {
            let cron = parse_cron(c).map_err(Refused::Invalid)?;
            check_cron(&cron, tz, now).map_err(Refused::Invalid)?
        }
        (None, Some(at)) if at > now => at,
        (None, Some(at)) => {
            return Err(Refused::Invalid(format!(
                "at {} is not in the future",
                at.to_rfc3339()
            )))
        }
        _ => return Err(Refused::Invalid("give exactly one of at and cron".into())),
    };
    let _lock = app.owner_lock("schedule", owner).await;
    if let Some(row) = row(app, id).await? {
        return Ok(row);
    }
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM scheduled_messages WHERE owner_id = $1 AND next_run IS NOT NULL",
    )
    .bind(owner)
    .fetch_one(&app.db)
    .await?;
    if active >= MAX_ACTIVE_PER_OWNER {
        return Err(Refused::Limit(format!(
            "at most {MAX_ACTIVE_PER_OWNER} schedules; delete some in the hqc client"
        )));
    }
    sqlx::query(
        "INSERT INTO scheduled_messages
           (id, owner_id, session_id, cron, at, timezone, message, catch_up, next_run, chain,
            created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(owner)
    .bind(n.session_id)
    .bind(n.cron.as_deref().map(str::trim))
    .bind(n.at)
    .bind(tz.name())
    .bind(&n.message)
    .bind(n.catch_up)
    .bind(next_run)
    .bind(n.chain.as_ref().map(serde_json::to_value).transpose()?)
    .bind(n.created_by)
    .execute(&app.db)
    .await?;
    tracing::info!(%id, session_id = %n.session_id, %next_run, "schedule created");
    row(app, id)
        .await?
        .ok_or_else(|| Refused::Internal(anyhow::anyhow!("schedule {id} vanished")))
}

// ---------------------------------------------------------------- rows

#[derive(Debug, Clone, FromRow)]
pub struct ScheduleRow {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub session_id: Uuid,
    pub cron: Option<String>,
    pub at: Option<DateTime<Utc>>,
    pub timezone: String,
    pub message: String,
    pub enabled: bool,
    pub catch_up: bool,
    pub next_run: Option<DateTime<Utc>>,
    pub last_run: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The chain an agent's schedule carries (SPEC §7.1); `None` for a person's.
    pub chain: Option<Value>,
    /// The session whose agent made it (T40); `None` for a person's, and for an agent's
    /// whose creator was deleted or that predates 0.44.
    pub created_by: Option<Uuid>,
}

impl ScheduleRow {
    pub fn to_api(&self) -> Schedule {
        Schedule {
            id: self.id,
            owner_id: self.owner_id,
            session_id: self.session_id,
            cron: self.cron.clone(),
            at: self.at,
            timezone: self.timezone.clone(),
            message: self.message.clone(),
            enabled: self.enabled,
            catch_up: self.catch_up,
            next_run: self.next_run,
            last_run: self.last_run,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }

    /// The first run strictly after `t`; `None` for an `at` or a pattern that broke.
    fn next_after(&self, t: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let cron = parse_cron(self.cron.as_deref()?).ok()?;
        next_cron(&cron, parse_tz(&self.timezone).ok()?, t)
    }
}

async fn row(app: &AppState, id: Uuid) -> sqlx::Result<Option<ScheduleRow>> {
    sqlx::query_as("SELECT * FROM scheduled_messages WHERE id = $1")
        .bind(id)
        .fetch_optional(&app.db)
        .await
}

async fn owner_timezone(app: &AppState, owner: Uuid) -> sqlx::Result<String> {
    sqlx::query_scalar("SELECT timezone FROM users WHERE id = $1")
        .bind(owner)
        .fetch_one(&app.db)
        .await
}

// ---------------------------------------------------------------- the tool

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleArgs {
    session: String,
    message: String,
    at: Option<String>,
    cron: Option<String>,
    timezone: Option<String>,
    #[serde(default)]
    catch_up: bool,
}

/// `hqc_schedule(session, at|cron, message, timezone?, catch_up?)`: `{schedule_id, session,
/// next_run, timezone}`. Once per `request_id`: the schedule's id is the call's.
pub async fn tool(app: &AppState, caller: &SessionRow, c: &McpCall) -> Result<Answer, McpError> {
    let a: ScheduleArgs = args(c.args.clone())?;
    let id = crate::mcp::once_id(hqc_proto::mcp::SCHEDULE, caller.id, &c.request_id);
    let existing = row(app, id).await.map_err(internal)?;
    // The chain moves on as for a send; one at its limit schedules nothing (SPEC §7.1).
    let chain = match &existing {
        Some(_) => None,
        None => Some(crate::peers::outgoing_chain(app, caller, c, false).await?),
    };
    let (session_id, to) = match &existing {
        Some(r) => {
            let t = db::session(&app.db, r.session_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| McpError::new(error_code::NOT_FOUND, "the session is gone"))?;
            (
                t.id,
                crate::peers::address(app, &t).await.map_err(internal)?,
            )
        }
        None => {
            let (t, to) = crate::peers::target(app, caller, &a.session).await?;
            (t.id, to)
        }
    };
    let at = match a.at.as_deref().map(str::trim) {
        None => None,
        Some(s) => Some(
            DateTime::parse_from_rfc3339(s)
                .map_err(|e| {
                    McpError::new(
                        error_code::INVALID_ARGS,
                        format!("at {s:?} is not an RFC 3339 time with an offset: {e}"),
                    )
                })?
                .with_timezone(&Utc),
        ),
    };
    let timezone = match a.timezone {
        Some(t) => t,
        None => owner_timezone(app, caller.owner_id)
            .await
            .map_err(internal)?,
    };
    let r = create(
        app,
        caller.owner_id,
        id,
        NewSchedule {
            session_id,
            chain,
            created_by: Some(caller.id),
            cron: a.cron,
            at,
            timezone,
            message: a.message,
            catch_up: a.catch_up,
        },
    )
    .await
    .map_err(Refused::mcp)?;
    Ok(Answer::Now(json!({
        "schedule_id": r.id,
        "session": to,
        "next_run": r.next_run,
        "timezone": r.timezone,
    })))
}

// ---------------------------------------------------------------- the API

async fn own_session(app: &AppState, user: &CurrentUser, id: Uuid) -> ApiResult<SessionRow> {
    match db::session(&app.db, id).await? {
        Some(s) if s.owner_id == user.id && !s.system => Ok(s),
        _ => Err(ApiError::NotFound),
    }
}

async fn own_schedule(app: &AppState, user: &CurrentUser, id: Uuid) -> ApiResult<ScheduleRow> {
    match row(app, id).await? {
        Some(r) if r.owner_id == user.id => Ok(r),
        _ => Err(ApiError::NotFound),
    }
}

#[derive(Deserialize)]
pub struct ListQuery {
    session_id: Option<Uuid>,
}

/// `GET /api/schedules?session_id=`: the user's schedules (of one session), the next due
/// first, finished ones last.
pub async fn list(
    State(app): State<AppState>,
    user: CurrentUser,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<ScheduleList>> {
    let rows: Vec<ScheduleRow> = sqlx::query_as(
        "SELECT * FROM scheduled_messages
         WHERE owner_id = $1 AND ($2::uuid IS NULL OR session_id = $2)
         ORDER BY next_run ASC NULLS LAST, created_at",
    )
    .bind(user.id)
    .bind(q.session_id)
    .fetch_all(&app.db)
    .await?;
    Ok(Json(ScheduleList {
        schedules: rows.iter().map(ScheduleRow::to_api).collect(),
    }))
}

/// `POST /api/schedules` → `201` and the schedule.
pub async fn create_api(
    State(app): State<AppState>,
    user: CurrentUser,
    Json(b): Json<CreateSchedule>,
) -> ApiResult<(StatusCode, Json<Schedule>)> {
    let s = own_session(&app, &user, b.session_id).await?;
    if s.archived {
        return Err(ApiError::Conflict(
            "archived",
            "the session is archived".into(),
        ));
    }
    let r = create(
        &app,
        user.id,
        db::new_id(),
        NewSchedule {
            session_id: s.id,
            chain: None,
            created_by: None,
            cron: b.cron,
            at: b.at,
            timezone: b.timezone.unwrap_or(user.timezone),
            message: b.message,
            catch_up: b.catch_up,
        },
    )
    .await
    .map_err(Refused::api)?;
    Ok((StatusCode::CREATED, Json(r.to_api())))
}

/// `PATCH /api/schedules/{id}` {enabled?}: pause or resume. A `cron` resumed runs next
/// after now: the runs it missed while paused are not sent.
pub async fn patch(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<PatchSchedule>,
) -> ApiResult<Json<Schedule>> {
    let r = own_schedule(&app, &user, id).await?;
    if let Some(enabled) = b.enabled.filter(|e| *e != r.enabled) {
        let next_run = match (&r.cron, enabled) {
            (Some(_), true) => r.next_after(app.clock.now()),
            _ => r.next_run,
        };
        sqlx::query(
            "UPDATE scheduled_messages SET enabled = $2, next_run = $3, updated_at = now()
             WHERE id = $1",
        )
        .bind(id)
        .bind(enabled)
        .bind(next_run)
        .execute(&app.db)
        .await?;
        tracing::info!(%id, enabled, "schedule changed");
    }
    Ok(Json(own_schedule(&app, &user, id).await?.to_api()))
}

/// `DELETE /api/schedules/{id}` → `204`.
pub async fn delete(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    own_schedule(&app, &user, id).await?;
    sqlx::query("DELETE FROM scheduled_messages WHERE id = $1")
        .bind(id)
        .execute(&app.db)
        .await?;
    tracing::info!(%id, "schedule deleted");
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- the loop

/// Looks for due runs every [`TICK`].
pub async fn cron_loop(app: AppState) {
    let mut tick = tokio::time::interval(TICK);
    loop {
        tick.tick().await;
        if let Err(e) = pass(&app, None).await {
            tracing::error!(error = %e, "hub cron");
        }
    }
}

/// Sends (or skips) every run due by [`Clock::now`]; the number sent.
pub async fn run_due(app: &AppState) -> anyhow::Result<usize> {
    pass(app, Some(app.clock.now())).await
}

/// One pass at `now`; `None`: the loop's, on the system clock, which leaves the runs to a
/// test once it has set the clock (a pass already under way too: [`fire`] checks again).
async fn pass(app: &AppState, now: Option<DateTime<Utc>>) -> anyhow::Result<usize> {
    let from_loop = now.is_none();
    if from_loop && app.clock.is_set() {
        return Ok(0);
    }
    let now = now.unwrap_or_else(Utc::now);
    let due: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM scheduled_messages WHERE enabled AND next_run <= $1
         ORDER BY next_run LIMIT 500",
    )
    .bind(now)
    .fetch_all(&app.db)
    .await?;
    let mut sent = 0;
    for id in due {
        match fire(app, id, now, from_loop).await {
            Ok(Some(session_id)) => {
                sent += 1;
                delivery::kick(app, session_id, false).await;
            }
            Ok(None) => {}
            Err(e) => tracing::error!(%id, error = %e, "scheduled run failed"),
        }
    }
    Ok(sent)
}

/// The `event_id` of the run of schedule `id` due at `run`.
pub fn run_event_id(id: Uuid, run: DateTime<Utc>) -> Uuid {
    crate::mcp::once_id("schedule.run", id, &run.timestamp().to_string())
}

/// One due run of schedule `id`: sent or skipped, then the next run set. The row stays
/// locked meanwhile, so a pause or a delete waits for it and the loop does not run it twice.
/// The session the message went to, if it went.
async fn fire(
    app: &AppState,
    id: Uuid,
    now: DateTime<Utc>,
    from_loop: bool,
) -> anyhow::Result<Option<Uuid>> {
    let mut tx = app.db.begin().await?;
    let r: Option<ScheduleRow> = sqlx::query_as(
        "SELECT * FROM scheduled_messages WHERE id = $1 AND enabled AND next_run <= $2
         FOR UPDATE",
    )
    .bind(id)
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(r) = r else {
        return Ok(None);
    };
    if from_loop && app.clock.is_set() {
        // A test set the clock before it created this row: the run is the test's.
        return Ok(None);
    }
    let due = r.next_run.unwrap_or(now);
    let missed = now - due > MISSED_AFTER;
    let mut sent = None;
    if missed && !r.catch_up {
        tracing::warn!(%id, %due, "scheduled run missed, skipped");
    } else if !app.budget.spend(
        "schedule_runs",
        &r.owner_id.to_string(),
        1,
        app.config.limits.schedule_runs_per_owner_hour as u64,
        TimeDelta::hours(1),
        now,
    ) {
        tracing::warn!(%id, owner_id = %r.owner_id, %due, "scheduled run skipped: the owner's runs per hour are used up");
    } else {
        sent = send(app, &r, due).await?;
    }
    sqlx::query(
        "UPDATE scheduled_messages SET next_run = $2, last_run = COALESCE($3, last_run),
         updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(r.next_after(now))
    .bind(sent.map(|_| now))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(sent)
}

/// The run's `user.message` in the session, unless the session takes no input now.
async fn send(app: &AppState, r: &ScheduleRow, due: DateTime<Utc>) -> anyhow::Result<Option<Uuid>> {
    let Some(s) = db::session(&app.db, r.session_id).await? else {
        return Ok(None);
    };
    if s.archived || s.system || s.locked_by == "vscode" {
        tracing::warn!(id = %r.id, session_id = %s.id, archived = s.archived, locked_by = %s.locked_by,
            "scheduled run skipped: the session takes no input");
        return Ok(None);
    }
    db::clear_turn_error(&app.db, s.id).await?;
    let chain = run_chain(app, r).await?;
    let payload: Value = json!({"text": r.message, "artifact_ids": [], "schedule_id": r.id});
    let stored = events::store_hub_event_with_id(
        app,
        &s,
        run_event_id(r.id, due),
        events::user_author(r.owner_id),
        "user.message",
        payload,
        Some("pending"),
        chain,
    )
    .await?;
    if stored.is_some() {
        tracing::info!(id = %r.id, session_id = %s.id, %due, "scheduled message sent");
    }
    Ok(Some(s.id))
}

/// `capped_by` of a run whose schedule's creator is gone (deleted, or not stored before 0.44).
pub const GONE_CREATOR: &str = "a deleted session";

/// The chain a run of `r` carries: none for a person's schedule; for an agent's, the stored
/// one capped with the lower of its stored cap and the creator's effective mode now (T40,
/// SPEC §16), a creator that is gone counting as `ask`.
async fn run_chain(app: &AppState, r: &ScheduleRow) -> anyhow::Result<Option<Value>> {
    let Some(stored) = &r.chain else {
        return Ok(None);
    };
    let mut c: Chain = serde_json::from_value(stored.clone())?;
    let stored_cap = c.permission_cap.unwrap_or(PermissionMode::AllowAll);
    if stored_cap == PermissionMode::Ask {
        return Ok(Some(stored.clone()));
    }
    let creator = match r.created_by {
        Some(id) => db::session(&app.db, id).await?,
        None => None,
    };
    let lower = match &creator {
        None => Some((PermissionMode::Ask, GONE_CREATOR.to_string())),
        Some(s) => {
            let mode = crate::peers::sender_mode(s);
            if mode < stored_cap {
                Some((mode, crate::peers::address(app, s).await?))
            } else {
                None
            }
        }
    };
    match (lower, &creator) {
        (Some((mode, by)), _) => {
            c.permission_cap = Some(mode);
            c.capped_by = Some(by);
        }
        // An `auto` cap stored before 0.129 names no session: its creator stands for it.
        (None, Some(s)) if stored_cap < PermissionMode::AllowAll && c.capped_by.is_none() => {
            c.capped_by = Some(crate::peers::address(app, s).await?);
        }
        _ => {}
    }
    Ok(Some(serde_json::to_value(c)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn runs(pattern: &str, tz: &str, from: &str, n: usize) -> Vec<String> {
        let cron = parse_cron(pattern).unwrap();
        let tz = parse_tz(tz).unwrap();
        let mut t = utc(from);
        (0..n)
            .map(|_| {
                t = next_cron(&cron, tz, t).unwrap();
                t.to_rfc3339()
            })
            .collect()
    }

    #[test]
    fn five_fields_only() {
        assert!(parse_cron("0 9 * * 1-5").is_ok());
        assert!(parse_cron("@daily").is_ok());
        assert!(parse_cron("0 0 9 * * *").is_err(), "seconds");
        assert!(parse_cron("0 9 * *").is_err());
        assert!(parse_cron("61 * * * *").is_err());
        assert!(parse_tz("Europe/Berlin").is_ok());
        assert!(parse_tz("Mars/Olympus").is_err());
    }

    #[test]
    fn a_daily_time_keeps_the_wall_clock_across_dst() {
        // Berlin: CET → CEST on 2026-03-29, CEST → CET on 2026-10-25.
        assert_eq!(
            runs("0 9 * * *", "Europe/Berlin", "2026-03-27T12:00:00Z", 3),
            [
                "2026-03-28T08:00:00+00:00",
                "2026-03-29T07:00:00+00:00",
                "2026-03-30T07:00:00+00:00"
            ]
        );
        assert_eq!(
            runs("0 9 * * *", "Europe/Berlin", "2026-10-24T12:00:00Z", 2),
            ["2026-10-25T08:00:00+00:00", "2026-10-26T08:00:00+00:00"]
        );
    }

    #[test]
    fn the_skipped_and_the_repeated_hour() {
        // 02:30 does not exist on 2026-03-29: right after the gap, 03:00 CEST.
        assert_eq!(
            runs("30 2 * * *", "Europe/Berlin", "2026-03-28T12:00:00Z", 2),
            ["2026-03-29T01:00:00+00:00", "2026-03-30T00:30:00+00:00"]
        );
        // 02:30 happens twice on 2026-10-25: once, the first time (CEST).
        assert_eq!(
            runs("30 2 * * *", "Europe/Berlin", "2026-10-24T12:00:00Z", 2),
            ["2026-10-25T00:30:00+00:00", "2026-10-26T01:30:00+00:00"]
        );
    }

    #[test]
    fn minimum_interval() {
        let tz = parse_tz("UTC").unwrap();
        let now = utc("2026-10-25T12:00:00Z");
        let check = |p: &str| check_cron(&parse_cron(p).unwrap(), tz, now);
        assert!(check("*/5 * * * *").is_ok());
        assert!(check("*/4 * * * *").is_err());
        assert!(check("* * * * *").is_err());
        // Close runs once a day are found too.
        assert!(check("0,1 9 * * *").is_err());
        assert!(check("0 0 30 2 *").is_err(), "never runs");
        assert_eq!(check("0 13 * * *").unwrap(), utc("2026-10-25T13:00:00Z"));
    }

    /// Cron input from an agent (T36): sparse and long patterns are checked in bounded time.
    #[test]
    fn hostile_patterns_are_cheap() {
        let tz = parse_tz("Pacific/Chatham").unwrap();
        let now = utc("2026-10-25T12:00:00Z");
        let long = (0..60).map(|m| m.to_string()).collect::<Vec<_>>().join(",");
        for p in [
            "0 0 29 2 1".to_string(),
            "0 0 31 4 *".to_string(),
            format!("{long} 0 1 1 *"),
            "59 23 31 12 0".to_string(),
        ] {
            let t = std::time::Instant::now();
            let _ = parse_cron(&p).map(|c| check_cron(&c, tz, now));
            assert!(
                t.elapsed() < Duration::from_secs(1),
                "{p}: {:?}",
                t.elapsed()
            );
        }
        assert!(parse_cron(&"1,".repeat(30_000)).is_err());
    }

    #[test]
    fn fake_clock() {
        let c = Clock::default();
        assert!((Utc::now() - c.now()).num_seconds().abs() < 5);
        c.set(utc("2026-01-01T00:00:00Z"));
        assert_eq!(c.now(), utc("2026-01-01T00:00:00Z"));
    }
}
