//! `hqc-worker enroll <hub_url>` (SPEC §5.1).
//!
//! 1. `POST {hub}/worker/enroll` `{name, os, protocol_version}` ->
//!    `{code, poll_token, expires_at, poll_interval_s}`;
//! 2. the code and the link `{hub}/#/enroll?code=XXXX-XXXX` are printed; the user confirms
//!    in the web app (`POST /api/workers/enroll/confirm {code}`);
//! 3. `POST {hub}/worker/enroll/poll` `{poll_token}` until `{status: confirmed, worker_id,
//!    token}`; `pending` means wait, `expired` (or HTTP 404/410) ends enrollment.
//!
//! `poll_token` is a secret between this process and the hub, sent in the body rather than
//! the URL so that it does not end up in access logs. The token is never printed or logged.

use std::io::Write;
use std::time::Duration;

use anyhow::{bail, Context};
use chrono::Utc;
use hqc_proto::api::{EnrollPoll, EnrollPollRequest, EnrollRequest, EnrollStarted};
use uuid::Uuid;

use crate::token::{Secret, TokenStore};

const MIN_POLL: Duration = Duration::from_millis(50);

/// Enrolls with `hub_url`, saves the token to `store` and returns the worker id.
/// Human-readable progress goes to `out`.
pub async fn enroll(
    hub_url: &str,
    store: &TokenStore,
    out: &mut (impl Write + Send),
) -> anyhow::Result<Uuid> {
    let hub = hub_url.trim_end_matches('/');
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let resp = http
        .post(format!("{hub}/worker/enroll"))
        .json(&EnrollRequest {
            name: hostname(),
            os: std::env::consts::OS.into(),
            protocol_version: Some(hqc_proto::PROTOCOL_VERSION),
        })
        .send()
        .await
        .context("contacting the hub")?;
    let status = resp.status();
    if !status.is_success() {
        bail!("hub refused enrollment: HTTP {status}");
    }
    let started: EnrollStarted = resp.json().await.context("enrollment response")?;
    writeln!(
        out,
        "Enrollment code: {code}\nConfirm it in the hqc web app: {}\nWaiting...",
        enroll_link(hub, &started.code),
        code = started.code,
    )?;
    out.flush()?;

    let interval = Duration::from_secs(started.poll_interval_s.into()).max(MIN_POLL);
    let give_up = (started.expires_at - Utc::now())
        .to_std()
        .unwrap_or_default();
    let deadline = tokio::time::Instant::now() + give_up;
    let poll = EnrollPollRequest {
        poll_token: started.poll_token,
    };
    loop {
        let resp = http
            .post(format!("{hub}/worker/enroll/poll"))
            .json(&poll)
            .send()
            .await;
        match resp {
            Err(e) => tracing::warn!("polling enrollment: {e}"),
            Ok(r) if r.status().is_success() => {
                match r.json().await.context("enrollment poll response")? {
                    EnrollPoll::Pending => {}
                    EnrollPoll::Confirmed { worker_id, token } => {
                        if token.is_empty() {
                            bail!("hub confirmed enrollment without a token");
                        }
                        store.save(&Secret::new(token))?;
                        writeln!(out, "Enrolled as worker {worker_id}.")?;
                        return Ok(worker_id);
                    }
                    EnrollPoll::Expired => bail!("enrollment code expired"),
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

/// The web client's enrollment screen with the code filled in.
pub fn enroll_link(hub: &str, code: &str) -> String {
    format!("{}/#/enroll?code={code}", hub.trim_end_matches('/'))
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

#[cfg(test)]
mod tests {
    #[test]
    fn link() {
        assert_eq!(
            super::enroll_link("https://hub.example/", "K7QM-3XPW"),
            "https://hub.example/#/enroll?code=K7QM-3XPW"
        );
    }
}
