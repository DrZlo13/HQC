//! The worker's own warn/error log lines in its service session (SPEC §10.2).
//!
//! [`layer`] is a `tracing` layer that copies every `WARN` / `ERROR` event into a bounded
//! channel; [`forward`] turns the lines into `system` events with `code: worker_log` and a
//! `level` field in the payload and queues them in the outbox like any other event (SPEC
//! §5.2). The session id comes from `hello.ack.system_session_id`; lines logged before the
//! first `hello.ack` wait in memory.
//!
//! Rate limit ([`RateLimit`]): the same text at most once a second, at most
//! [`PER_MINUTE`] lines a minute in total; what is dropped is counted and reported by one
//! line "skipped N" as soon as the limit allows.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use hqc_proto::event::{Author, EventKind};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;
use uuid::Uuid;

use crate::link::HubLink;

/// `system.code` of a log line (SPEC §4, §10.2).
pub const CODE: &str = "worker_log";
/// Lines a minute, the "skipped N" line included.
pub const PER_MINUTE: u32 = 60;
/// The same text is sent at most once per this interval.
pub const SAME_TEXT_INTERVAL: Duration = Duration::from_secs(1);
const WINDOW: Duration = Duration::from_secs(60);
/// Lines waiting for the forwarder; more are counted as dropped.
const CHANNEL: usize = 256;
/// Longest line sent, in characters.
const MAX_TEXT: usize = 2000;

/// This module logs about its own failures; those lines must not come back here.
const OWN_TARGET: &str = module_path!();

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// `warn` or `error`.
    pub level: &'static str,
    pub text: String,
    pub ts: DateTime<Utc>,
}

/// The sending side: what [`LogLayer`] feeds. Lines that do not fit the channel are counted.
#[derive(Clone)]
pub struct LogSender {
    tx: mpsc::Sender<LogLine>,
    lost: Arc<AtomicU64>,
}

impl LogSender {
    pub fn send(&self, line: LogLine) {
        if self.tx.try_send(line).is_err() {
            self.lost.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub struct LogReceiver {
    rx: mpsc::Receiver<LogLine>,
    lost: Arc<AtomicU64>,
}

pub fn channel() -> (LogSender, LogReceiver) {
    let (tx, rx) = mpsc::channel(CHANNEL);
    let lost = Arc::new(AtomicU64::new(0));
    (
        LogSender {
            tx,
            lost: lost.clone(),
        },
        LogReceiver { rx, lost },
    )
}

/// The layer for the worker's subscriber and the receiver for [`forward`].
pub fn layer() -> (LogLayer, LogReceiver) {
    let (tx, rx) = channel();
    (LogLayer { tx }, rx)
}

pub struct LogLayer {
    tx: LogSender,
}

impl<S: Subscriber> Layer<S> for LogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let m = event.metadata();
        let level = match *m.level() {
            Level::ERROR => "error",
            Level::WARN => "warn",
            _ => return,
        };
        if m.target().starts_with(OWN_TARGET) {
            return;
        }
        let mut v = Fields::default();
        event.record(&mut v);
        let mut text = v.message;
        if !v.rest.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&v.rest);
        }
        self.tx.send(LogLine {
            level,
            text: format!("{}: {text}", m.target()),
            ts: Utc::now(),
        });
    }
}

#[derive(Default)]
struct Fields {
    message: String,
    rest: String,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            self.sep();
            let _ = write!(self.rest, "{}={value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            self.sep();
            let _ = write!(self.rest, "{}={value:?}", field.name());
        }
    }
}

impl Fields {
    fn sep(&mut self) {
        if !self.rest.is_empty() {
            self.rest.push(' ');
        }
    }
}

/// The rate limit of log lines (module docs). Pure: time comes from the caller.
#[derive(Debug)]
pub struct RateLimit {
    /// When each text was last let through.
    last: HashMap<String, Instant>,
    window_start: Option<Instant>,
    in_window: u32,
    /// Dropped since the last "skipped N".
    pub suppressed: u64,
}

impl Default for RateLimit {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimit {
    pub fn new() -> Self {
        Self {
            last: HashMap::new(),
            window_start: None,
            in_window: 0,
            suppressed: 0,
        }
    }

    fn roll(&mut self, now: Instant) {
        match self.window_start {
            Some(s) if now.duration_since(s) < WINDOW => {}
            _ => {
                self.window_start = Some(now);
                self.in_window = 0;
            }
        }
    }

    /// Whether `text` may go now; a refused line is counted.
    pub fn check(&mut self, text: &str, now: Instant) -> bool {
        self.roll(now);
        if self
            .last
            .get(text)
            .is_some_and(|t| now.duration_since(*t) < SAME_TEXT_INTERVAL)
            || self.in_window >= PER_MINUTE
        {
            self.suppressed += 1;
            return false;
        }
        self.in_window += 1;
        if self.last.len() > PER_MINUTE as usize * 2 {
            self.last
                .retain(|_, t| now.duration_since(*t) < SAME_TEXT_INTERVAL);
        }
        self.last.insert(text.to_string(), now);
        true
    }

    /// `Some(N)` when "skipped N" is due and fits the limit; the count starts again.
    pub fn summary(&mut self, now: Instant) -> Option<u64> {
        self.roll(now);
        if self.suppressed == 0 || self.in_window >= PER_MINUTE {
            return None;
        }
        self.in_window += 1;
        Some(std::mem::take(&mut self.suppressed))
    }
}

/// The `system` event of a log line, as JSON: `level` is not a field of the typed
/// [`EventKind::System`], so it is added to the payload here (SPEC §4: unknown payload fields
/// are kept and ignored by readers that do not know them).
pub fn event(session_id: Uuid, line: &LogLine) -> serde_json::Value {
    let text: String = hqc_proto::replace_nul(&line.text)
        .chars()
        .take(MAX_TEXT)
        .collect();
    let mut ev = crate::adapter::new_event(
        session_id,
        Author::System,
        EventKind::System {
            text,
            code: CODE.into(),
            call_id: None,
            status: None,
            task_id: None,
            fork: None,
        },
    );
    ev.ts = line.ts;
    let mut v = serde_json::to_value(&ev).expect("events serialize");
    v["payload"]["level"] = line.level.into();
    v
}

fn skipped(n: u64) -> LogLine {
    LogLine {
        level: "warn",
        text: format!("skipped {n}"),
        ts: Utc::now(),
    }
}

/// Lines kept while the service session is unknown (before the first `hello.ack`).
const EARLY: usize = PER_MINUTE as usize;

/// Runs until `stop`: rate-limits the lines and queues them as events of the service
/// session in the outbox.
pub async fn forward(mut logs: LogReceiver, link: HubLink, stop: CancellationToken) {
    let mut session = link.system_session();
    let mut limit = RateLimit::new();
    let mut early: VecDeque<LogLine> = VecDeque::new();
    let mut tick = tokio::time::interval(SAME_TEXT_INTERVAL);
    let emit =
        |early: &mut VecDeque<LogLine>, limit: &mut RateLimit, sid: Option<Uuid>, line| match sid {
            Some(sid) => {
                let ev = event(sid, &line);
                if let Err(e) = link.event_json(&ev) {
                    tracing::error!("worker log line not queued: {e:#}");
                }
            }
            None if early.len() < EARLY => early.push_back(line),
            None => limit.suppressed += 1,
        };
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            line = logs.rx.recv() => {
                let Some(line) = line else { break };
                if limit.check(&line.text, Instant::now()) {
                    let sid = *session.borrow();
                    emit(&mut early, &mut limit, sid, line);
                }
            }
            _ = tick.tick() => {
                limit.suppressed += logs.lost.swap(0, Ordering::Relaxed);
                if let Some(n) = limit.summary(Instant::now()) {
                    let sid = *session.borrow();
                    emit(&mut early, &mut limit, sid, skipped(n));
                }
            }
            r = session.changed() => {
                if r.is_err() {
                    break;
                }
                let sid = *session.borrow_and_update();
                if sid.is_some() {
                    for line in early.drain(..).collect::<Vec<_>>() {
                        emit(&mut early, &mut limit, sid, line);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_text_once_a_second() {
        let t0 = Instant::now();
        let mut l = RateLimit::new();
        assert!(l.check("a", t0));
        assert!(!l.check("a", t0 + Duration::from_millis(500)));
        assert!(l.check("b", t0 + Duration::from_millis(500)));
        assert!(l.check("a", t0 + Duration::from_millis(1000)));
        assert_eq!(l.suppressed, 1);
        assert_eq!(l.summary(t0 + Duration::from_millis(1100)), Some(1));
        assert_eq!(l.summary(t0 + Duration::from_millis(1200)), None);
    }

    #[test]
    fn sixty_a_minute_then_one_summary() {
        let t0 = Instant::now();
        let mut l = RateLimit::new();
        let mut passed = 0;
        for i in 0..100u64 {
            if l.check(&format!("line {i}"), t0 + Duration::from_millis(i * 10)) {
                passed += 1;
            }
        }
        assert_eq!(passed, PER_MINUTE);
        // No room for the summary in this minute.
        assert_eq!(l.summary(t0 + Duration::from_secs(30)), None);
        assert!(!l.check("late", t0 + Duration::from_secs(59)));
        // The next minute starts with "skipped N", which counts against it.
        assert_eq!(l.summary(t0 + Duration::from_secs(61)), Some(41));
        for i in 0..100u64 {
            l.check(&format!("next {i}"), t0 + Duration::from_secs(62));
        }
        assert_eq!(l.suppressed, 100 - (PER_MINUTE as u64 - 1));
    }

    #[test]
    fn event_shape() {
        let sid = Uuid::now_v7();
        let ev = event(
            sid,
            &LogLine {
                level: "error",
                text: "x\0y".into(),
                ts: Utc::now(),
            },
        );
        assert_eq!(ev["session_id"], sid.to_string());
        assert_eq!(ev["author"], serde_json::json!({"kind": "system"}));
        assert_eq!(ev["type"], "system");
        assert_eq!(
            ev["payload"],
            serde_json::json!({"text": "x\u{FFFD}y", "code": "worker_log", "level": "error"})
        );
        // Still a valid event for the typed side.
        let _: hqc_proto::event::Event = serde_json::from_value(ev).unwrap();
    }

    #[tokio::test]
    async fn layer_takes_warn_and_error_only() {
        use tracing_subscriber::layer::SubscriberExt;
        let (layer, mut rx) = layer();
        let sub = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(sub, || {
            // This module's own lines are skipped: the test logs as another one.
            tracing::info!(target: "hqc_worker::connection", "not this");
            tracing::warn!(target: "hqc_worker::connection", attempts = 3, "hub error");
            tracing::error!(target: "hqc_worker::connection", "broken");
            tracing::error!(target: "hqc_worker::worker_log", "own");
        });
        let a = rx.rx.try_recv().unwrap();
        assert_eq!(a.level, "warn");
        assert_eq!(a.text, "hqc_worker::connection: hub error attempts=3");
        let b = rx.rx.try_recv().unwrap();
        assert_eq!(b.level, "error");
        assert_eq!(b.text, "hqc_worker::connection: broken");
        assert!(rx.rx.try_recv().is_err());
    }
}
