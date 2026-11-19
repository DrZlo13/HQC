//! The sign-in of the worker's own Claude Code, driven from hqc (SPEC §5.3, §14, since 0.185).
//!
//! - `claude auth status` in the worker's environment says how the CLI is signed in
//!   ([`status`]). The worker runs in the user's session (launchd on macOS), where the CLI
//!   reaches its credentials in the Keychain; an ssh shell does not, which is why a sign-in
//!   over ssh left them in a file the agents never read.
//! - [`Logins`]: `claude auth login --claudeai` runs with its stdin and output piped; the page it
//!   prints to sign in on goes to the hub, the code that page gives comes back and is written to
//!   the CLI's stdin. The CLI stores what it gets where the agents read it. One sign-in at a time,
//!   at most [`LOGIN_TTL`] long. The code is never logged; nothing of the CLI's credentials passes
//!   through the worker.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context};
use hqc_proto::frame::{ClaudeAuthStatus, ClaudeLogin, ClaudeLoginDone};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};

/// How long `claude auth status` may take.
const STATUS_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the CLI may take to print the page to sign in on.
const URL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the CLI may take to take the code.
const CODE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a sign-in waits for its code.
pub const LOGIN_TTL: Duration = Duration::from_secs(600);
/// The most of the CLI's words a failed sign-in reports.
const MESSAGE_LIMIT: usize = 300;

/// The CLI's environment: the agents' allowlist (no `HQC_*`), and a browser that opens
/// nothing on the machine's screen, where nobody signs in (**[verify]** that the CLI honours
/// `BROWSER`; at worst a tab opens there).
fn command(bin: &PathBuf) -> Command {
    let mut c = Command::new(bin);
    c.env_clear()
        .envs(crate::adapter::claude_code::child_env(std::env::vars()))
        .env("BROWSER", "true")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    c
}

/// `claude auth status`: its JSON (`loggedIn`, `authMethod`, `email`, `orgName`,
/// `subscriptionType`, `loginExpiresAt`), signed out or not; a CLI that cannot be run or says
/// nothing readable gives `error`.
pub async fn status(bin: &PathBuf) -> ClaudeAuthStatus {
    let mut c = command(bin);
    c.args(["auth", "status"]);
    let out = match tokio::time::timeout(STATUS_TIMEOUT, c.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return failed(format!("running claude: {e}")),
        Err(_) => return failed("claude auth status did not answer in time".into()),
    };
    match serde_json::from_slice::<Value>(&out.stdout) {
        Ok(v) => parse_status(&v),
        Err(_) => failed(format!(
            "claude auth status said nothing readable ({})",
            String::from_utf8_lossy(&out.stderr)
                .trim()
                .chars()
                .take(200)
                .collect::<String>()
        )),
    }
}

fn failed(error: String) -> ClaudeAuthStatus {
    ClaudeAuthStatus {
        error: Some(error),
        ..Default::default()
    }
}

/// The status of `claude auth status --json`'s object.
pub fn parse_status(v: &Value) -> ClaudeAuthStatus {
    let s = |k: &str| {
        v[k].as_str()
            .filter(|s| !s.is_empty() && *s != "none")
            .map(str::to_string)
    };
    let logged_in = v["loggedIn"].as_bool().unwrap_or(false);
    ClaudeAuthStatus {
        logged_in,
        auth_method: s("authMethod").filter(|_| logged_in),
        email: s("email"),
        org_name: s("orgName"),
        subscription_type: s("subscriptionType"),
        expires_at: s("loginExpiresAt"),
        error: None,
    }
}

/// The page to sign in on in the CLI's words: the first `https://` address on `claude.com`,
/// `claude.ai` or a subdomain of them.
pub fn login_url(text: &str) -> Option<String> {
    text.split_whitespace()
        .filter(|w| w.starts_with("https://"))
        .find(|w| {
            let host = w["https://".len()..]
                .split(['/', '?', '#'])
                .next()
                .unwrap_or("");
            ["claude.com", "claude.ai"]
                .iter()
                .any(|d| host == *d || host.ends_with(&format!(".{d}")))
        })
        .map(str::to_string)
}

/// A sign-in waiting for its code.
struct Login {
    child: Child,
    stdin: ChildStdin,
    /// What the CLI printed, stdout and stderr together.
    out: Arc<Mutex<String>>,
}

/// The sign-ins under way, by id: at most one.
#[derive(Default, Clone)]
pub struct Logins {
    map: Arc<tokio::sync::Mutex<HashMap<String, Login>>>,
}

fn read_into(mut r: impl tokio::io::AsyncRead + Unpin + Send + 'static, out: Arc<Mutex<String>>) {
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        while let Ok(n) = r.read(&mut buf).await {
            if n == 0 {
                break;
            }
            out.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });
}

impl Logins {
    /// Starts `claude auth login --claudeai` and returns the page it prints. A sign-in that
    /// waits already is refused (`login_running`); it ends after [`LOGIN_TTL`] by itself.
    pub async fn start(&self, bin: &PathBuf) -> anyhow::Result<ClaudeLogin> {
        let mut map = self.map.lock().await;
        map.retain(|_, l| matches!(l.child.try_wait(), Ok(None)));
        if !map.is_empty() {
            bail!("login_running: a sign-in of this worker's Claude Code is under way");
        }
        let mut c = command(bin);
        c.args(["auth", "login", "--claudeai"])
            .stdin(Stdio::piped());
        let mut child = c.spawn().context("starting claude auth login")?;
        let stdin = child.stdin.take().context("no stdin")?;
        let out = Arc::new(Mutex::new(String::new()));
        read_into(child.stdout.take().context("no stdout")?, out.clone());
        read_into(child.stderr.take().context("no stderr")?, out.clone());
        let deadline = tokio::time::Instant::now() + URL_TIMEOUT;
        let url = loop {
            let text = out.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(u) = login_url(&text) {
                break u;
            }
            if !matches!(child.try_wait(), Ok(None)) {
                bail!(
                    "claude auth login ended without a page to sign in on: {}",
                    tail(&text)
                );
            }
            if tokio::time::Instant::now() > deadline {
                bail!("claude auth login printed no page to sign in on in time");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let id = uuid::Uuid::now_v7().to_string();
        map.insert(id.clone(), Login { child, stdin, out });
        drop(map);
        tracing::info!(login_id = %id, "Claude Code sign-in started");
        let this = self.clone();
        let expired = id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(LOGIN_TTL).await;
            if this.map.lock().await.remove(&expired).is_some() {
                tracing::info!(login_id = %expired, "Claude Code sign-in expired");
            }
        });
        Ok(ClaudeLogin { login_id: id, url })
    }

    /// Writes `code` to the sign-in's CLI and waits for its verdict; the status after it.
    pub async fn code(
        &self,
        bin: &PathBuf,
        id: &str,
        code: &str,
    ) -> anyhow::Result<ClaudeLoginDone> {
        let code = code.trim();
        if code.is_empty() || code.len() > 2048 || code.chars().any(|c| c.is_control()) {
            bail!("the code is one line of at most 2048 bytes");
        }
        let Some(mut l) = self.map.lock().await.remove(id) else {
            bail!(
                "no_login: no such sign-in, or it ended (they last {} minutes)",
                LOGIN_TTL.as_secs() / 60
            );
        };
        let start = l.out.lock().unwrap_or_else(|e| e.into_inner()).len();
        l.stdin
            .write_all(format!("{code}\n").as_bytes())
            .await
            .context("writing the code to claude auth login")?;
        let _ = l.stdin.flush().await;
        let deadline = tokio::time::Instant::now() + CODE_TIMEOUT;
        let (ok, said) = loop {
            let text = l.out.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let after = text.get(start..).unwrap_or("").to_string();
            if after.contains("Login successful") {
                break (true, after);
            }
            let ended = !matches!(l.child.try_wait(), Ok(None));
            if ended || tokio::time::Instant::now() > deadline {
                break (false, after);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let _ = l.child.start_kill();
        tracing::info!(login_id = %id, ok, "Claude Code sign-in ended");
        Ok(ClaudeLoginDone {
            ok,
            message: (!ok).then(|| {
                let t = tail(&said);
                if t.is_empty() {
                    "claude auth login did not take the code".into()
                } else {
                    t
                }
            }),
            status: status(bin).await,
        })
    }

    /// Ends sign-in `id`; `false` if there is none.
    pub async fn cancel(&self, id: &str) -> bool {
        match self.map.lock().await.remove(id) {
            Some(mut l) => {
                let _ = l.child.start_kill();
                tracing::info!(login_id = %id, "Claude Code sign-in cancelled");
                true
            }
            None => false,
        }
    }
}

/// The last words of the CLI, one line, at most [`MESSAGE_LIMIT`] bytes.
fn tail(text: &str) -> String {
    let one = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let start = one.len().saturating_sub(MESSAGE_LIMIT);
    let mut i = start;
    while !one.is_char_boundary(i) {
        i += 1;
    }
    one[i..].to_string()
}

/// A runtime's own reply that says its Claude Code is signed out (SPEC §8.1): the CLI's words
/// for a sign-in that expired, is missing or was refused.
pub fn says_signed_out(text: &str) -> bool {
    const SIGNS: &[&str] = &[
        "OAuth session expired",
        "OAuth token has expired",
        "Not logged in",
        "Please run /login",
        "Invalid API key",
        "Failed to authenticate",
    ];
    SIGNS.iter().any(|s| text.contains(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_status_is_read_from_the_clis_json() {
        let on = parse_status(
            &json!({"loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty",
            "email": "a@b.c", "orgName": "Org", "subscriptionType": "team", "loginExpiresAt": "2026-12-12T20:53:03.852Z"}),
        );
        assert!(on.logged_in);
        assert_eq!(on.auth_method.as_deref(), Some("claude.ai"));
        assert_eq!(on.subscription_type.as_deref(), Some("team"));
        let off = parse_status(&json!({"loggedIn": false, "authMethod": "none"}));
        assert!(!off.logged_in && off.auth_method.is_none());
    }

    #[test]
    fn the_page_to_sign_in_on_is_on_claude_only() {
        let said = "Opening browser to sign in…\nIf the browser didn't open, visit: https://claude.com/cai/oauth/authorize?code=true&state=x\nPaste code here if prompted >";
        assert_eq!(
            login_url(said).as_deref(),
            Some("https://claude.com/cai/oauth/authorize?code=true&state=x")
        );
        assert!(login_url("visit: https://evil.example/claude.com/x").is_none());
        assert!(login_url("visit: https://claude.com.evil.example/x").is_none());
        assert!(login_url("visit: http://claude.com/x").is_none());
        assert!(login_url("visit: https://platform.claude.com/oauth").is_some());
    }

    #[test]
    fn a_signed_out_reply_is_known() {
        assert!(says_signed_out(
            "Failed to authenticate: OAuth session expired and could not be refreshed"
        ));
        assert!(!says_signed_out("API Error: 529 overloaded"));
    }

    /// A fake `claude` that prints a sign-in page and takes a code: `ok` signs in.
    #[tokio::test]
    async fn a_sign_in_takes_the_code() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        std::fs::write(
            &bin,
            "#!/bin/sh\nif [ \"$2\" = status ]; then echo '{\"loggedIn\": true, \"authMethod\": \"claude.ai\"}'; exit 0; fi\n\
             echo 'If the browser did not open, visit: https://claude.com/cai/oauth/authorize?state=s'\n\
             printf 'Paste code here if prompted > '\nread code\n\
             if [ \"$code\" = ok ]; then echo 'Login successful.'; else echo 'Invalid code'; exit 1; fi\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let logins = Logins::default();
        let l = logins.start(&bin).await.unwrap();
        assert!(l.url.starts_with("https://claude.com/"));
        assert!(logins
            .start(&bin)
            .await
            .unwrap_err()
            .to_string()
            .starts_with("login_running"));
        let done = logins.code(&bin, &l.login_id, "ok").await.unwrap();
        assert!(done.ok && done.status.logged_in, "{done:?}");
        // A bad code: not ok, the CLI's words.
        let l = logins.start(&bin).await.unwrap();
        let done = logins.code(&bin, &l.login_id, "nope").await.unwrap();
        assert!(!done.ok && done.message.unwrap().contains("Invalid code"));
        // Cancel, and a code for a sign-in that is gone.
        let l = logins.start(&bin).await.unwrap();
        assert!(logins.cancel(&l.login_id).await);
        assert!(logins
            .code(&bin, &l.login_id, "ok")
            .await
            .unwrap_err()
            .to_string()
            .starts_with("no_login"));
    }
}
