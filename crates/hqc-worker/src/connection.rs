//! WebSocket client to the hub's `/worker/ws` (SPEC §5, §5.1, §5.2).
//!
//! - Bearer token in the upgrade request; `hello` first, nothing else until `hello.ack`.
//! - `protocol_version_unsupported`: log "update needed" and retry every
//!   [`ConnOptions::version_retry`] (an hour, SPEC §5.1), doing nothing else meanwhile.
//! - Any other failure: reconnect with exponential backoff, 1 s doubling up to 60 s, reset
//!   after a successful `hello.ack`.
//! - Heartbeat: a WebSocket ping every 20 s; a connection silent for
//!   [`ConnOptions::dead_after`] is dropped.
//! - After `hello.ack` the whole outbox is sent in order, then new entries as they come;
//!   `ack {event_id}` deletes the entry. A non-retryable `error` on an event frame drops
//!   the event (it would fail the same way forever); a retryable one keeps it for the next
//!   connection, up to [`MAX_EVENT_ATTEMPTS`] such errors (SPEC §5.2).
//! - On stop, the outbox is flushed for up to [`ConnOptions::flush_timeout`]; what is left
//!   stays on disk for the next run.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use hqc_proto::frame::{error_code, Envelope, Hello, HubFrame, WorkerFrame};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::link::{Direct, HubLink};
use crate::supervisor::{wire_error, Inbound};
use crate::token::Secret;

/// Retryable hub errors on one event after which it is dropped: a hub that fails it on
/// every resend must not keep it in the outbox forever (SPEC §5.2).
pub const MAX_EVENT_ATTEMPTS: u32 = 10;

#[derive(Debug, Clone)]
pub struct ConnOptions {
    pub heartbeat: Duration,
    pub dead_after: Duration,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    pub version_retry: Duration,
    pub hello_timeout: Duration,
    pub flush_timeout: Duration,
}

impl Default for ConnOptions {
    fn default() -> Self {
        Self {
            heartbeat: Duration::from_secs(20),
            dead_after: Duration::from_secs(60),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            version_retry: Duration::from_secs(3600),
            hello_timeout: Duration::from_secs(30),
            flush_timeout: Duration::from_secs(5),
        }
    }
}

/// Connection status, for observers (tests, later the tray).
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Connecting,
    Connected {
        worker_id: Uuid,
    },
    /// The hub needs a newer worker; retrying every `version_retry`.
    UpdateNeeded {
        min_version: Option<u64>,
    },
    Unauthorized,
    Stopped,
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

enum Ended {
    Stopped,
    UpdateNeeded(Option<u64>),
    Lost { connected: bool, why: String },
}

pub struct Connection {
    pub hub_url: String,
    pub token: Secret,
    pub hello: Hello,
    pub link: HubLink,
    pub direct: mpsc::UnboundedReceiver<Direct>,
    pub inbound: mpsc::Sender<Inbound>,
    pub status: watch::Sender<Status>,
    pub opts: ConnOptions,
    next_id: u64,
}

/// `http(s)://host[/prefix]` -> `ws(s)://host[/prefix]/worker/ws`.
pub fn ws_url(hub_url: &str) -> String {
    let base = hub_url.trim_end_matches('/');
    let base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{base}/worker/ws")
}

impl Connection {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        hub_url: String,
        token: Secret,
        hello: Hello,
        link: HubLink,
        direct: mpsc::UnboundedReceiver<Direct>,
        inbound: mpsc::Sender<Inbound>,
        status: watch::Sender<Status>,
        opts: ConnOptions,
    ) -> Self {
        Self {
            hub_url,
            token,
            hello,
            link,
            direct,
            inbound,
            status,
            opts,
            next_id: 0,
        }
    }

    pub async fn run(mut self, stop: CancellationToken) {
        let mut backoff = self.opts.backoff_min;
        loop {
            self.status.send_replace(Status::Connecting);
            let wait = match self.session(&stop).await {
                Ended::Stopped => break,
                Ended::UpdateNeeded(min) => {
                    tracing::error!(
                        min_version = ?min,
                        our_version = hqc_proto::PROTOCOL_VERSION,
                        "hub rejected our protocol version: worker update needed; retrying in {:?}",
                        self.opts.version_retry
                    );
                    self.status
                        .send_replace(Status::UpdateNeeded { min_version: min });
                    backoff = self.opts.backoff_min;
                    self.opts.version_retry
                }
                Ended::Lost { connected, why } => {
                    if connected {
                        backoff = self.opts.backoff_min;
                    }
                    tracing::warn!("hub connection lost: {why}; reconnecting in {backoff:?}");
                    let w = backoff;
                    backoff = (backoff * 2).min(self.opts.backoff_max);
                    w
                }
            };
            if !self.idle(wait, &stop).await {
                break;
            }
        }
        self.status.send_replace(Status::Stopped);
    }

    /// Waits while disconnected, dropping direct frames. `false` if stopped.
    async fn idle(&mut self, d: Duration, stop: &CancellationToken) -> bool {
        let until = Instant::now() + d;
        loop {
            tokio::select! {
                _ = stop.cancelled() => return false,
                _ = tokio::time::sleep_until(until) => return true,
                d = self.direct.recv() => if d.is_none() { return false },
            }
        }
    }

    fn frame_id(&mut self) -> String {
        self.next_id += 1;
        format!("w-{}", self.next_id)
    }

    async fn session(&mut self, stop: &CancellationToken) -> Ended {
        let lost = |why: String| Ended::Lost {
            connected: false,
            why,
        };
        let mut req = match ws_url(&self.hub_url).into_client_request() {
            Ok(r) => r,
            Err(e) => return lost(format!("bad hub url: {e}")),
        };
        let bearer = match format!("Bearer {}", self.token.expose()).parse() {
            Ok(v) => v,
            Err(_) => return lost("token is not a valid header value".into()),
        };
        req.headers_mut().insert(AUTHORIZATION, bearer);
        let connect = tokio::time::timeout(
            self.opts.hello_timeout,
            tokio_tungstenite::connect_async(req),
        );
        let mut ws: Ws = tokio::select! {
            _ = stop.cancelled() => return Ended::Stopped,
            r = connect => match r {
                Err(_) => return lost("connect timed out".into()),
                Ok(Err(tokio_tungstenite::tungstenite::Error::Http(resp)))
                    if resp.status() == 401 || resp.status() == 403 =>
                {
                    self.unauthorized();
                    return lost(format!("hub refused the worker token ({})", resp.status()));
                }
                Ok(Err(e)) => return lost(format!("connect: {e}")),
                Ok(Ok((ws, _))) => ws,
            },
        };

        let hello_id = self.frame_id();
        let hello = Envelope::new(hello_id.clone(), WorkerFrame::Hello(self.hello.clone()));
        if let Err(e) = send_json(&mut ws, &hello).await {
            return lost(format!("sending hello: {e}"));
        }
        let worker_id = match self.await_hello_ack(&mut ws, &hello_id, stop).await {
            Ok(id) => id,
            Err(ended) => return ended,
        };
        tracing::info!(%worker_id, "connected to hub");
        self.status.send_replace(Status::Connected { worker_id });
        let _ = self.inbound.send(Inbound::Connected).await;

        let why = match self.connected(&mut ws, stop).await {
            Ok(()) => return Ended::Stopped,
            Err(why) => why,
        };
        let _ = ws.close(None).await;
        Ended::Lost {
            connected: true,
            why,
        }
    }

    fn unauthorized(&self) {
        tracing::error!("hub rejected the worker token; run `hqc-worker enroll` again");
        self.status.send_replace(Status::Unauthorized);
    }

    async fn await_hello_ack(
        &mut self,
        ws: &mut Ws,
        hello_id: &str,
        stop: &CancellationToken,
    ) -> Result<Uuid, Ended> {
        let lost = |why: String| Ended::Lost {
            connected: false,
            why,
        };
        let deadline = Instant::now() + self.opts.hello_timeout;
        loop {
            let msg = tokio::select! {
                _ = stop.cancelled() => return Err(Ended::Stopped),
                _ = tokio::time::sleep_until(deadline) => return Err(lost("no hello.ack".into())),
                m = ws.next() => m,
            };
            let text = match msg {
                None => return Err(lost("closed before hello.ack".into())),
                Some(Err(e)) => return Err(lost(format!("reading hello.ack: {e}"))),
                Some(Ok(Message::Text(t))) => t,
                Some(Ok(Message::Close(c))) => {
                    return Err(lost(format!("closed before hello.ack: {c:?}")))
                }
                Some(Ok(_)) => continue,
            };
            let f: Envelope<HubFrame> = match serde_json::from_str(&text) {
                Ok(f) => f,
                Err(e) => return Err(lost(format!("bad frame before hello.ack: {e}"))),
            };
            match f.body {
                HubFrame::HelloAck {
                    protocol_version,
                    worker_id,
                    ..
                } if f.id == hello_id => {
                    if protocol_version != hqc_proto::PROTOCOL_VERSION {
                        tracing::warn!(
                            protocol_version,
                            "hub acked hello with another protocol version"
                        );
                    }
                    return Ok(worker_id);
                }
                HubFrame::Error(e) if e.code == error_code::PROTOCOL_VERSION_UNSUPPORTED => {
                    let min = e
                        .details
                        .as_ref()
                        .and_then(|d| d.get("min_version"))
                        .and_then(Value::as_u64);
                    return Err(Ended::UpdateNeeded(min));
                }
                HubFrame::Error(e) => {
                    if e.code == error_code::UNAUTHORIZED {
                        self.unauthorized();
                    }
                    return Err(lost(format!("hello rejected: {} ({})", e.code, e.message)));
                }
                other => {
                    return Err(lost(format!(
                        "unexpected frame before hello.ack: {}",
                        frame_type(&other)
                    )))
                }
            }
        }
    }

    /// The connected phase. `Ok` when stopped (after flushing), `Err(why)` when lost.
    async fn connected(&mut self, ws: &mut Ws, stop: &CancellationToken) -> Result<(), String> {
        let mut sent_upto = 0i64;
        let mut in_flight: HashMap<String, Uuid> = HashMap::new();
        self.send_outbox(ws, &mut sent_upto, &mut in_flight).await?;
        let mut heartbeat = tokio::time::interval(self.opts.heartbeat);
        heartbeat.tick().await;
        let mut last_rx = Instant::now();
        let ready = self.link.outbox_ready.clone();
        loop {
            tokio::select! {
                _ = stop.cancelled() => {
                    self.flush(ws, &mut sent_upto, &mut in_flight).await;
                    return Ok(());
                }
                _ = ready.notified() => {
                    self.send_outbox(ws, &mut sent_upto, &mut in_flight).await?;
                }
                d = self.direct.recv() => match d {
                    Some(d) => self.send_direct(ws, d).await?,
                    None => return Ok(()),
                },
                m = ws.next() => {
                    last_rx = Instant::now();
                    match m {
                        None => return Err("closed by hub".into()),
                        Some(Err(e)) => return Err(format!("read: {e}")),
                        Some(Ok(Message::Text(t))) => self.on_text(ws, &t, &mut in_flight, true).await?,
                        Some(Ok(Message::Close(c))) => return Err(format!("closed by hub: {c:?}")),
                        Some(Ok(_)) => {}
                    }
                }
                _ = heartbeat.tick() => {
                    if last_rx.elapsed() > self.opts.dead_after {
                        return Err("heartbeat timeout".into());
                    }
                    ws.send(Message::Ping(Default::default())).await.map_err(|e| format!("ping: {e}"))?;
                }
            }
        }
    }

    /// Sends what is left of the direct queue and waits for the outbox to drain.
    async fn flush(
        &mut self,
        ws: &mut Ws,
        sent_upto: &mut i64,
        in_flight: &mut HashMap<String, Uuid>,
    ) {
        let deadline = Instant::now() + self.opts.flush_timeout;
        let r: Result<(), String> = async {
            while let Ok(d) = self.direct.try_recv() {
                self.send_direct(ws, d).await?;
            }
            self.send_outbox(ws, sent_upto, in_flight).await?;
            while !self.link.outbox.is_empty().map_err(|e| e.to_string())? {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => break,
                    m = ws.next() => match m {
                        Some(Ok(Message::Text(t))) => self.on_text(ws, &t, in_flight, false).await?,
                        Some(Ok(_)) => {}
                        _ => return Err("closed during flush".into()),
                    },
                }
            }
            Ok(())
        }
        .await;
        let left = self.link.outbox.len().unwrap_or(0);
        match r {
            Ok(()) if left == 0 => tracing::info!("outbox flushed"),
            Ok(()) => tracing::warn!(left, "outbox not fully acked; kept for the next run"),
            Err(e) => tracing::warn!(left, "flush interrupted: {e}"),
        }
        let _ = ws.close(None).await;
    }

    async fn send_outbox(
        &mut self,
        ws: &mut Ws,
        sent_upto: &mut i64,
        in_flight: &mut HashMap<String, Uuid>,
    ) -> Result<(), String> {
        let entries = self
            .link
            .outbox
            .after(*sent_upto)
            .map_err(|e| format!("reading outbox: {e:#}"))?;
        for e in entries {
            let id = self.frame_id();
            let frame = if e.frame == crate::outbox::FRAME_EVENT {
                json!({"id": id, "type": "event", "payload": {"event": e.event}})
            } else {
                json!({"id": id, "type": e.frame, "payload": e.event})
            };
            ws.send(Message::text(frame.to_string()))
                .await
                .map_err(|e| format!("send: {e}"))?;
            in_flight.insert(id, e.event_id);
            *sent_upto = e.seq;
        }
        Ok(())
    }

    async fn send_direct(&mut self, ws: &mut Ws, d: Direct) -> Result<(), String> {
        let id = d.id.unwrap_or_else(|| self.frame_id());
        send_json(ws, &Envelope::new(id, d.frame))
            .await
            .map_err(|e| format!("send: {e}"))
    }

    async fn on_text(
        &mut self,
        ws: &mut Ws,
        text: &str,
        in_flight: &mut HashMap<String, Uuid>,
        dispatch: bool,
    ) -> Result<(), String> {
        let f: Envelope<HubFrame> = match serde_json::from_str(text) {
            Ok(f) => f,
            Err(e) => {
                // Strict frames (SPEC §5.2): answer with the frame id if there is one.
                let id = serde_json::from_str::<Value>(text)
                    .ok()
                    .and_then(|v| v.get("id")?.as_str().map(String::from))
                    .unwrap_or_default();
                tracing::warn!(%id, "invalid frame from hub: {e}");
                let reply = Envelope::new(
                    id,
                    wire_error(error_code::INVALID_FRAME, e.to_string(), false),
                );
                return send_json(ws, &reply)
                    .await
                    .map_err(|e| format!("send: {e}"));
            }
        };
        match f.body {
            HubFrame::Ack(a) => {
                // Only outbox events are in flight. Acks of direct frames (`agent.delta`, never
                // stored, SPEC §5.2) come without `seq` by design and are not drops.
                if let Some(by_frame) = in_flight.remove(&f.id) {
                    let eid = a.event_id.unwrap_or(by_frame);
                    if let Err(e) = self.link.outbox.remove(eid) {
                        tracing::error!(event_id = %eid, "outbox delete failed: {e:#}");
                    }
                    if a.seq.is_none() {
                        tracing::info!(event_id = %eid, "hub dropped the event: unknown session");
                    }
                }
            }
            HubFrame::Error(e) => match in_flight.remove(&f.id) {
                Some(eid) if !e.retryable => {
                    tracing::error!(event_id = %eid, code = %e.code, "hub rejected an event, dropping it: {}", e.message);
                    let _ = self.link.outbox.remove(eid);
                }
                Some(eid) => match self.link.outbox.failed(eid) {
                    Ok(Some(n)) if n >= MAX_EVENT_ATTEMPTS => {
                        tracing::error!(event_id = %eid, code = %e.code, attempts = n, "hub keeps failing an event, dropping it: {}", e.message);
                        if let Err(e) = self.link.outbox.remove(eid) {
                            tracing::error!(event_id = %eid, "outbox delete failed: {e:#}");
                        }
                    }
                    Ok(n) => {
                        tracing::warn!(event_id = %eid, code = %e.code, attempts = ?n, "hub error on an event, will resend after reconnect: {}", e.message);
                    }
                    Err(err) => {
                        tracing::error!(event_id = %eid, "outbox update failed: {err:#}");
                    }
                },
                None => tracing::warn!(id = %f.id, code = %e.code, "hub error: {}", e.message),
            },
            HubFrame::HelloAck { .. } => tracing::warn!("unexpected hello.ack"),
            body if dispatch => {
                if self
                    .inbound
                    .send(Inbound::Frame(Envelope::new(f.id, body)))
                    .await
                    .is_err()
                {
                    tracing::debug!("supervisor is gone; frame dropped");
                }
            }
            body => tracing::debug!(frame = frame_type(&body), "frame ignored during shutdown"),
        }
        Ok(())
    }
}

async fn send_json<T: serde::Serialize>(
    ws: &mut Ws,
    v: &T,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    let text = serde_json::to_string(v).expect("frames serialize");
    ws.send(Message::text(text)).await
}

fn frame_type(f: &HubFrame) -> &'static str {
    match f {
        HubFrame::HelloAck { .. } => "hello.ack",
        HubFrame::SessionStart(_) => "session.start",
        HubFrame::SessionInput(_) => "session.input",
        HubFrame::SessionStop { .. } => "session.stop",
        HubFrame::SessionKill { .. } => "session.kill",
        HubFrame::ApprovalResponse { .. } => "approval.response",
        HubFrame::ClarifyResponse { .. } => "clarify.response",
        HubFrame::FsStat(_) => "fs.stat",
        HubFrame::FsListDirs(_) => "fs.list_dirs",
        HubFrame::WorkspaceListCandidates(_) => "workspace.list_candidates",
        HubFrame::MirrorSet(_) => "mirror.set",
        HubFrame::Ack(_) => "ack",
        HubFrame::Error(_) => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_urls() {
        assert_eq!(ws_url("https://h.example/"), "wss://h.example/worker/ws");
        assert_eq!(
            ws_url("http://127.0.0.1:9/hub"),
            "ws://127.0.0.1:9/hub/worker/ws"
        );
    }
}
