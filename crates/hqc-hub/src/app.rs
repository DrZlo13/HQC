//! Shared hub state: database, auth, the registry of connected workers and the in-process
//! bus that fans events out to client WebSockets.

use std::{
    collections::HashMap,
    ops::Deref,
    sync::{Arc, Mutex},
    time::Instant,
};

use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{auth::Auth, config::Config, push::PushSender, worker_ws::WorkerConn};

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

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
    /// Timestamps of recent `/worker/enroll` calls (SPEC §20: rate limit on enroll).
    pub enroll_calls: Mutex<Vec<Instant>>,
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
            config,
            auth,
            workers: WorkerRegistry::default(),
            bus,
            push,
            watchers: Mutex::new(HashMap::new()),
            enroll_calls: Mutex::new(Vec::new()),
        }))
    }

    pub fn notify(&self, n: Notice) {
        // No receivers is fine: nobody is watching.
        let _ = self.bus.send(Arc::new(n));
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
