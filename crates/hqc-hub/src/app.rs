//! Shared hub state: database, auth, the registry of connected workers and the in-process
//! bus that fans events out to client WebSockets.

use std::{
    collections::HashMap,
    ops::Deref,
    sync::{Arc, Mutex},
};

use tokio::time::Instant;

use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::{broadcast, Semaphore};
use uuid::Uuid;

use crate::{
    auth::Auth,
    config::Config,
    push::PushSender,
    security::{Budget, RateLimiter},
    worker_ws::WorkerConn,
};

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

type OwnerLock = Arc<tokio::sync::Mutex<()>>;

pub struct Inner {
    pub db: PgPool,
    pub config: Config,
    pub auth: Auth,
    pub workers: WorkerRegistry,
    pub bus: broadcast::Sender<Arc<Notice>>,
    pub push: Arc<dyn PushSender>,
    /// `(user_id, session_id)` -> number of client WebSockets subscribed (SPEC §16: no push
    /// while the user is watching the session).
    pub watchers: Mutex<HashMap<(Uuid, Uuid), usize>>,
    /// Per-address limits of `/worker/*` (SPEC §20).
    pub limiter: RateLimiter,
    /// Token lookups of `/worker/ws` in flight (SPEC §20).
    pub ws_auth: Arc<Semaphore>,
    /// Session -> the last sign of life of its turn: an event from the worker, or the moment
    /// the watchdog first saw the session `running` (SPEC §17.3, [`crate::watchdog`]).
    pub turn_activity: Mutex<HashMap<Uuid, Instant>>,
    /// Worker -> the last push on one of its own requests (SPEC §10.2, §20: one per 10 s).
    pub worker_pushes: Mutex<HashMap<Uuid, Instant>>,
    /// Sessions whose title the hub has started to derive in this process ([`crate::titles`]).
    pub titling: Mutex<std::collections::HashSet<Uuid>>,
    /// The clock of the hub cron ([`crate::schedules`]); tests set it.
    pub clock: crate::schedules::Clock,
    /// The caps of the MCP surface and of worker uploads (SPEC §20).
    pub budget: Budget,
    /// Searches running at once (SPEC §15, §20).
    pub searches: Arc<Semaphore>,
    /// Worker -> the moves the owner allowed whose result the previous hub process did not
    /// store, found on start and carried out once that worker says `hello` ([`crate::moves`]).
    pub left_moves: Mutex<HashMap<Uuid, Vec<crate::db::PendingRow>>>,
    /// Worker -> the usage limits of its account as it last reported them (`worker.limits`,
    /// SPEC §5.4), and when; in memory: the next turn on the worker reports them again.
    pub limits: Mutex<HashMap<Uuid, (hqc_proto::frame::Limits, chrono::DateTime<chrono::Utc>)>>,
    /// `(what, owner)` -> a lock held while a cap is counted and the row it counts is
    /// written ([`AppState::owner_lock`]).
    owner_locks: Mutex<HashMap<(&'static str, Uuid), OwnerLock>>,
    /// The text of the answer being written ([`AppState::live_text`]).
    live: Mutex<LiveText>,
    /// Session -> its latest `subagents.live`, unless that list was empty, and when it came
    /// ([`AppState::live_agents`]).
    live_agents: Mutex<HashMap<Uuid, (Value, Instant)>>,
    /// Session -> its latest `tokens.rate`, unless it was 0 ([`AppState::token_rate`]).
    token_rates: Mutex<HashMap<Uuid, u32>>,
}

/// A session's `agent.delta` text since its last `agent.message` or `status` other than
/// `running`, so a client that subscribes mid-answer sees all of it (SPEC §6). Deltas are
/// numbered across sessions: a subscription skips the ones its copy of the text holds.
#[derive(Default)]
struct LiveText {
    next: u64,
    /// Session -> the first delta of the text (its envelope for the copy) and the text;
    /// `None` once the text outgrew [`MAX_LIVE_TEXT`].
    texts: HashMap<Uuid, Option<(Value, String)>>,
}

/// Over this, a subscription mid-answer gets only the deltas after it, as before.
const MAX_LIVE_TEXT: usize = 512 * 1024;

impl Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

/// What client connections need to hear about.
#[derive(Debug)]
pub enum Notice {
    /// A stored event, as clients get it.
    Event {
        session_id: Uuid,
        seq: i64,
        event: Value,
    },
    /// A stored event sent again with a field the hub adds for clients, now known
    /// (`reply_delivered_at`, SPEC §6): only to subscriptions that already had it.
    Resent {
        session_id: Uuid,
        seq: i64,
        event: Value,
    },
    /// An `agent.delta`, numbered for [`AppState::live_text`].
    Delta {
        session_id: Uuid,
        at: u64,
        event: Value,
    },
    /// A `subagents.live` ([`AppState::live_agents`]).
    LiveAgents { session_id: Uuid, event: Value },
    /// `delivery` of an input changed (SPEC §3 Event).
    Delivery {
        session_id: Uuid,
        event_id: Uuid,
        seq: i64,
        delivery: String,
        delivered_at: Option<chrono::DateTime<chrono::Utc>>,
        /// The input went into the turn already running (SPEC §17.2).
        joined: bool,
    },
    /// Something that feeds `state`, `queue_len` or unread changed (SPEC §6 `subscribe_summary`).
    SessionChanged { session_id: Uuid },
    /// An open request of the user's sessions, or an `hqc_ask` between them, opened or
    /// closed (SPEC §6 `requests`).
    RequestsChanged { owner_id: Uuid },
}

impl AppState {
    pub fn new(db: PgPool, config: Config, push: Arc<dyn PushSender>) -> Self {
        let auth = Auth::from_config(&config.auth);
        let (bus, _) = broadcast::channel(1024);
        AppState(Arc::new(Inner {
            db,
            auth,
            workers: WorkerRegistry::default(),
            bus,
            push,
            watchers: Mutex::new(HashMap::new()),
            limiter: RateLimiter::default(),
            ws_auth: Arc::new(Semaphore::new(config.limits.ws_auth_concurrency)),
            turn_activity: Mutex::new(HashMap::new()),
            worker_pushes: Mutex::new(HashMap::new()),
            titling: Mutex::new(Default::default()),
            clock: Default::default(),
            budget: Budget::default(),
            searches: Arc::new(Semaphore::new(config.limits.search_concurrency)),
            left_moves: Mutex::new(HashMap::new()),
            limits: Mutex::new(HashMap::new()),
            owner_locks: Mutex::new(HashMap::new()),
            live: Mutex::default(),
            live_agents: Mutex::default(),
            token_rates: Mutex::default(),
            config,
        }))
    }

    /// Serializes the creations of `what` (spawned sessions, schedules) of one owner, so that
    /// a cap counted before an insert holds for parallel calls (SPEC §20). The hub is one
    /// process (SPEC §2), so a lock in memory is enough.
    pub async fn owner_lock(
        &self,
        what: &'static str,
        owner: Uuid,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let m = self
            .owner_locks
            .lock()
            .unwrap()
            .entry((what, owner))
            .or_default()
            .clone();
        m.lock_owned().await
    }

    pub fn notify(&self, n: Notice) {
        // No receivers is fine: nobody is watching.
        let _ = self.bus.send(Arc::new(n));
    }

    /// Fan an `agent.delta` out to clients and add it to the session's text. Under the lock
    /// of the text, so the bus carries deltas in the order of their numbers.
    pub fn delta(&self, session_id: Uuid, event: Value, text: &str) {
        let mut live = self.live.lock().unwrap();
        let at = live.next;
        live.next += 1;
        let slot = live
            .texts
            .entry(session_id)
            .or_insert_with(|| Some((event.clone(), String::new())));
        if let Some((_, t)) = slot {
            if t.len() + text.len() > MAX_LIVE_TEXT {
                *slot = None;
            } else {
                t.push_str(text);
            }
        }
        self.notify(Notice::Delta {
            session_id,
            at,
            event,
        });
    }

    /// The session's answer was stored or its turn ended: the text is done. Called before
    /// the event is stored, so that a subscription that missed the event in its backfill
    /// cannot get a copy of the text after it.
    pub fn end_live_text(&self, session_id: Uuid) {
        self.live.lock().unwrap().texts.remove(&session_id);
    }

    /// The session's answer is being written: a delta came and the text is not done yet
    /// (also once it outgrew the copy).
    pub fn text_open(&self, session_id: Uuid) -> bool {
        self.live.lock().unwrap().texts.contains_key(&session_id)
    }

    /// The session's text as one `agent.delta` (`None` if there is none), and the number of
    /// the first delta not in it.
    pub fn live_text(&self, session_id: Uuid) -> (Option<Value>, u64) {
        let live = self.live.lock().unwrap();
        let copy = match live.texts.get(&session_id) {
            Some(Some((envelope, text))) if !text.is_empty() => {
                let mut ev = envelope.clone();
                ev["payload"] = serde_json::json!({ "text": text });
                Some(ev)
            }
            _ => None,
        };
        (copy, live.next)
    }

    /// Fan a `subagents.live` out to clients and keep it as the session's list, or drop the
    /// list when it is `empty`. Under the lock of the lists, so the last one on the bus is the
    /// one kept.
    pub fn live_agents_changed(&self, session_id: Uuid, event: Value, empty: bool) {
        let mut lists = self.live_agents.lock().unwrap();
        if empty {
            lists.remove(&session_id);
        } else {
            lists.insert(session_id, (event.clone(), Instant::now()));
        }
        self.notify(Notice::LiveAgents { session_id, event });
    }

    /// The session's agent runs no subagent any more, though no empty list from the worker
    /// may say so (SPEC §4): its worker restarted, its `session.state` counts none, or it was
    /// deleted. A kept list is dropped and the session's subscribers get an empty
    /// `subagents.live`, as from the worker.
    pub fn drop_live_agents(&self, session_id: Uuid) {
        let mut lists = self.live_agents.lock().unwrap();
        if lists.remove(&session_id).is_some() {
            self.notify(Notice::LiveAgents {
                session_id,
                event: no_live_agents(session_id),
            });
        }
    }

    /// The session's latest `subagents.live`, unless it was empty, its run times counted now.
    pub fn live_agents(&self, session_id: Uuid) -> Option<Value> {
        let lists = self.live_agents.lock().unwrap();
        let (event, at) = lists.get(&session_id)?;
        Some(aged(event.clone(), at.elapsed()))
    }

    /// Keep a `tokens.rate` as the session's rate, or drop the rate at 0; whether it changed.
    pub fn token_rate_changed(&self, session_id: Uuid, output_per_s: u32) -> bool {
        let mut rates = self.token_rates.lock().unwrap();
        let before = if output_per_s == 0 {
            rates.remove(&session_id)
        } else {
            rates.insert(session_id, output_per_s)
        };
        before.unwrap_or(0) != output_per_s
    }

    /// The session's latest `tokens.rate`, 0 without one.
    pub fn token_rate(&self, session_id: Uuid) -> u32 {
        let rates = self.token_rates.lock().unwrap();
        rates.get(&session_id).copied().unwrap_or(0)
    }

    /// The worker sent something for the session (any event, `agent.delta` included).
    pub fn worker_activity(&self, session_id: Uuid) {
        self.turn_activity
            .lock()
            .unwrap()
            .insert(session_id, Instant::now());
    }

    pub fn session_changed(&self, session_id: Uuid) {
        self.notify(Notice::SessionChanged { session_id });
    }

    pub fn requests_changed(&self, owner_id: Uuid) {
        self.notify(Notice::RequestsChanged { owner_id });
    }

    pub fn is_watching(&self, user: Uuid, session: Uuid) -> bool {
        self.watchers
            .lock()
            .unwrap()
            .get(&(user, session))
            .is_some_and(|n| *n > 0)
    }

    pub fn watch(&self, user: Uuid, session: Uuid, on: bool) {
        let mut w = self.watchers.lock().unwrap();
        let n = w.entry((user, session)).or_insert(0);
        if on {
            *n += 1;
        } else {
            *n = n.saturating_sub(1);
            if *n == 0 {
                w.remove(&(user, session));
            }
        }
    }
}

/// An empty `subagents.live` of the session, as its worker sends one when the agent stops.
pub fn no_live_agents(session_id: Uuid) -> Value {
    serde_json::json!({
        "session_id": session_id,
        "event_id": crate::db::new_id(),
        "ts": chrono::Utc::now(),
        "author": {"kind": "agent", "id": session_id},
        "type": "subagents.live",
        "payload": {"agents": []},
    })
}

/// `event`, a `subagents.live`, with `age` added to the `duration_ms` of each subagent.
fn aged(mut event: Value, age: std::time::Duration) -> Value {
    let age = u64::try_from(age.as_millis()).unwrap_or(u64::MAX);
    let agents = event
        .get_mut("payload")
        .and_then(|p| p.get_mut("agents"))
        .and_then(Value::as_array_mut);
    for a in agents.into_iter().flatten() {
        if let Some(d) = a.get("duration_ms").and_then(Value::as_u64) {
            a["duration_ms"] = d.saturating_add(age).into();
        }
    }
    event
}

/// Connected workers by id; at most one live connection per worker.
#[derive(Default)]
pub struct WorkerRegistry {
    inner: Mutex<HashMap<Uuid, Arc<WorkerConn>>>,
}

impl WorkerRegistry {
    pub fn get(&self, worker_id: Uuid) -> Option<Arc<WorkerConn>> {
        self.inner.lock().unwrap().get(&worker_id).cloned()
    }

    pub fn is_online(&self, worker_id: Uuid) -> bool {
        self.inner.lock().unwrap().contains_key(&worker_id)
    }

    /// Register a connection, returning the one it replaces.
    pub fn insert(&self, conn: Arc<WorkerConn>) -> Option<Arc<WorkerConn>> {
        self.inner.lock().unwrap().insert(conn.worker_id, conn)
    }

    /// Remove the connection only if it is still the registered one.
    pub fn remove(&self, worker_id: Uuid, conn_id: Uuid) -> bool {
        let mut m = self.inner.lock().unwrap();
        if m.get(&worker_id).is_some_and(|c| c.conn_id == conn_id) {
            m.remove(&worker_id);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The copy a subscription gets counts the time since the list came; a run time at the
    /// top of the range stays there.
    #[test]
    fn a_kept_list_ages() {
        let event = json!({"type": "subagents.live", "payload": {"agents": [
            {"task_id": "a", "description": "A", "duration_ms": 120_000},
            {"task_id": "b", "description": "B", "duration_ms": u64::MAX},
        ]}});
        let got = aged(event, std::time::Duration::from_secs(480));
        assert_eq!(got["payload"]["agents"][0]["duration_ms"], 600_000);
        assert_eq!(got["payload"]["agents"][1]["duration_ms"], u64::MAX);
        assert_eq!(got["payload"]["agents"][0]["description"], "A");
    }
}
