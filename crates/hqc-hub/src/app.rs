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
    /// `(what, owner)` -> a lock held while a cap is counted and the row it counts is
    /// written ([`AppState::owner_lock`]).
    owner_locks: Mutex<HashMap<(&'static str, Uuid), OwnerLock>>,
    /// The text of the answer being written ([`AppState::live_text`]).
    live: Mutex<LiveText>,
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
    /// `delivery` of an input changed (SPEC §3 Event).
    Delivery {
        session_id: Uuid,
        event_id: Uuid,
        seq: i64,
        delivery: String,
        delivered_at: Option<chrono::DateTime<chrono::Utc>>,
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
            owner_locks: Mutex::new(HashMap::new()),
            live: Mutex::default(),
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
