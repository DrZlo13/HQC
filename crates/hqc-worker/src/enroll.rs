//! `hqc-worker enroll <hub_url>` (SPEC §5.1).
//!
//! 1. `POST {hub}/worker/enroll` `{name, os, protocol_version}` ->
//!    `{code, poll_token, expires_at, poll_interval_s}`;
//! 2. the code and the link `{hub}/#/enroll?code=XXXX-XXXX` are printed; the user confirms
//!    in the web app (`POST /api/workers/enroll/confirm {code}`);
//! 3. `POST {hub}/worker/enroll/poll` `{poll_token}` until `{status: confirmed, worker_id,
//!    token, replaced?}`; `pending` means wait, `expired` (or HTTP 404/410) ends enrollment.
//!    `replaced` — the user confirmed this as the same machine as an existing worker, whose
//!    id this one now has.
//!
//! `poll_token` is a secret between this process and the hub, sent in the body rather than
//! the URL so that it does not end up in access logs. The token is never printed or logged.

use std::io::Write;
use std::time::Duration;

use anyhow::{bail, Context};
use chrono::Utc;
use hqc_proto::api::{EnrollPoll, EnrollPollRequest, EnrollRequest, EnrollStarted};
use uuid::Uuid;

use crate::config::check_hub_url;
use crate::token::{Secret, TokenStore};

const MIN_POLL: Duration = Duration::from_millis(50);

/// Enrolls with `hub_url` as `name` (see [`default_name`]), saves the token to `store` and
/// returns the worker id. Human-readable progress goes to `out`. `hub_url` is https unless
/// it is a loopback host or `insecure_http` (see [`check_hub_url`]). `store` is probed
/// before the hub is asked: the hub hands the token out once, so a store that cannot keep
/// it (a locked Keychain over ssh) would burn it.
pub async fn enroll(
    hub_url: &str,
    name: &str,
    insecure_http: bool,
    store: &TokenStore,
    out: &mut (impl Write + Send),
) -> anyhow::Result<Uuid> {
    check_hub_url(hub_url, insecure_http)?;
    store.probe()?;
    let hub = hub_url.trim_end_matches('/');
    // A redirect would re-send the body (with `poll_token`) to another origin.
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let resp = http
        .post(format!("{hub}/worker/enroll"))
        .json(&EnrollRequest {
            name: name.to_string(),
            os: std::env::consts::OS.into(),
            protocol_version: Some(hqc_proto::PROTOCOL_VERSION),
            // The hub then offers replacing the worker of this machine (T21, SPEC §5.1).
            machine_id: crate::machine::fingerprint(crate::config::hqc_home().ok().as_deref()),
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
                    EnrollPoll::Confirmed {
                        worker_id,
                        token,
                        replaced,
                    } => {
                        if token.is_empty() {
                            bail!("hub confirmed enrollment without a token");
                        }
                        store.save(&Secret::new(token))?;
                        match replaced {
                            // The user chose "the same machine": its sessions and workspaces
                            // stay with this worker id (SPEC §5.1).
                            Some(old) => {
                                writeln!(out, "Enrolled as worker {worker_id} (replaced {old}).")?
                            }
                            None => writeln!(out, "Enrolled as worker {worker_id}.")?,
                        }
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

/// The worker name when `--name` is not given: the host name without its domain
/// (`home-mac.tail201738.ts.net` -> `home-mac`).
pub fn default_name() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| short_host(&s))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "worker".into())
}

fn short_host(s: &str) -> String {
    s.trim().split('.').next().unwrap_or_default().to_string()
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

    #[test]
    fn short_host() {
        assert_eq!(
            super::short_host("home-mac.tail201738.ts.net\n"),
            "home-mac"
        );
        assert_eq!(super::short_host("llm-server"), "llm-server");
    }
}
