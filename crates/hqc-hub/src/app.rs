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
    /// `(what, owner)` -> a lock held while a cap is counted and the row it counts is
    /// written ([`AppState::owner_lock`]).
    owner_locks: Mutex<HashMap<(&'static str, Uuid), OwnerLock>>,
}

impl Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

/// What client connections need to hear about.
#[derive(Debug)]
pub enum Notice {
    /// A stored event (with `seq`) or an `agent.delta` (without), as clients get it.
    Event {
        session_id: Uuid,
        seq: Option<i64>,
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
            owner_locks: Mutex::new(HashMap::new()),
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
