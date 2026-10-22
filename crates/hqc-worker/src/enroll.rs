//! `hqc-worker enroll <hub_url>` (SPEC §5.1).
//!
//! The worker side of enrollment is not fixed by the spec yet; this build uses (see
//! docs/notes/t3-questions.md):
//! 1. `POST {hub}/worker/enroll` `{name, os, protocol_version}` ->
//!    `{enroll_id, code, poll_interval_s?, expires_at?}`;
//! 2. the code is printed; the user confirms it in the web app
//!    (`POST /api/workers/enroll/confirm {code}`);
//! 3. `POST {hub}/worker/enroll/poll` `{enroll_id}` until
//!    `{status: "confirmed", token, worker_id}`; `pending` means wait, anything else
//!    (`expired`, `rejected`, 404, 410) ends enrollment.
//!
//! `enroll_id` is a secret between this process and the hub, sent in the body rather than
//! the URL so that it does not end up in access logs. The token is never printed or logged.

use std::io::Write;
use std::time::Duration;

use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::token::{Secret, TokenStore};

#[derive(Deserialize)]
struct Started {
    enroll_id: String,
    code: String,
    #[serde(default)]
    poll_interval_s: Option<f64>,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct Poll {
    status: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    worker_id: Option<Uuid>,
}

const DEFAULT_POLL: Duration = Duration::from_secs(2);
const DEFAULT_WAIT: Duration = Duration::from_secs(15 * 60);

/// Enrolls with `hub_url`, saves the token to `store` and returns the worker id.
/// Human-readable progress goes to `out`.
pub async fn enroll(
    hub_url: &str,
    store: &TokenStore,
    out: &mut (impl Write + Send),
) -> anyhow::Result<Option<Uuid>> {
    let hub = hub_url.trim_end_matches('/');
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let resp = http
        .post(format!("{hub}/worker/enroll"))
        .json(&json!({
            "name": hostname(),
            "os": std::env::consts::OS,
            "protocol_version": hqc_proto::PROTOCOL_VERSION,
        }))
        .send()
        .await
        .context("contacting the hub")?;
    let status = resp.status();
    if !status.is_success() {
        bail!("hub refused enrollment: HTTP {status}");
    }
    let started: Started = resp.json().await.context("enrollment response")?;
    writeln!(
        out,
        "Enrollment code: {}\nConfirm it in the hqc web app (Workers) at {hub}; waiting...",
        started.code
    )?;
    out.flush()?;

    let interval = started
        .poll_interval_s
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(Duration::from_secs_f64)
        .unwrap_or(DEFAULT_POLL);
    let give_up = started
        .expires_at
        .and_then(|t| (t - Utc::now()).to_std().ok())
        .unwrap_or(DEFAULT_WAIT);
    let deadline = tokio::time::Instant::now() + give_up;
    loop {
        let resp = http
            .post(format!("{hub}/worker/enroll/poll"))
            .json(&json!({ "enroll_id": started.enroll_id }))
            .send()
            .await;
        match resp {
            Err(e) => tracing::warn!("polling enrollment: {e}"),
            Ok(r) if r.status().is_success() => {
                let p: Poll = r.json().await.context("enrollment poll response")?;
                match p.status.as_str() {
                    "pending" => {}
                    "confirmed" => {
                        let token = p
                            .token
                            .filter(|t| !t.is_empty())
                            .context("hub confirmed enrollment without a token")?;
                        store.save(&Secret::new(token))?;
                        match p.worker_id {
                            Some(id) => writeln!(out, "Enrolled as worker {id}.")?,
                            None => writeln!(out, "Enrolled.")?,
                        }
                        return Ok(p.worker_id);
                    }
                    other => bail!("enrollment {other}"),
                }
            }
            Ok(r) if r.status() == 404 || r.status() == 410 => {
                bail!("enrollment expired or unknown (HTTP {})", r.status())
            }
            Ok(r) => tracing::warn!("polling enrollment: HTTP {}", r.status()),
        }
        if tokio::time::Instant::now() + interval > deadline {
            bail!("enrollment was not confirmed in time");
        }
        tokio::time::sleep(interval).await;
    }
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "worker".into())
}
