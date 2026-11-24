//! Reading a secret from Vaultwarden (Bitwarden) through this machine's `rbw`. Two callers: a
//! card's `secret.put` with `vault` (only with `vault = true` and `session_secrets` in
//! `worker.toml`), and a provider's `api_key = { vault = ... }` at every start of an agent. The
//! value goes from `rbw` to the caller here: it does not pass through the hub, and nothing but
//! the entry's and the field's names is ever logged or put into an error. An error is the
//! reason and what to do about `rbw`; what the caller can offer instead (typing the value) is
//! the caller's to add.

use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{bail, Context};
use hqc_proto::frame::MAX_SECRET;
use tokio::process::Command;

/// How long all the `rbw` calls of one read may take together. A locked `rbw` asks its owner
/// to unlock it (`rbw list` runs `rbw unlock`, and `rbw-agent` asks for the master password
/// through the `pinentry` configured on this machine), so the time includes a person answering
/// the prompt; and a reprompt window holds `rbw-agent`'s lock, so even `rbw unlocked` can
/// block. It also covers the wait for another read's unlock ([`READS`]). An ssh-add after a
/// card's read is normally instant; the hub's 30 s wait for `secret.put` is the cap.
const READ_TIMEOUT: Duration = Duration::from_secs(25);
/// One read at a time on this machine: `rbw-agent` opens a pinentry window for each connection
/// that finds the vault locked, so parallel reads would each open one. The reads that wait get
/// the unlock the first one made.
static READS: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
/// Longest piece of `rbw`'s stderr an error carries, in characters.
const MAX_STDERR: usize = 300;
/// The `type` of a list line whose `get` prints the public key; its private half is a field.
const SSH_KEY: &str = "SSH Key";

/// A refusal that typing the value would not get past: an entry of type SSH Key asked for as
/// a plain secret, an entry over [`MAX_SECRET`]. The caller does not offer to type the value
/// for it.
#[derive(Debug)]
pub struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// The first line of `rbw`'s stderr, cut: all an error carries of what it printed.
fn first_line(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(MAX_STDERR)
        .collect()
}

/// The password (or the secret note, or an ssh key's private half) of the Vaultwarden entry
/// whose name or id is exactly `entry`, as `rbw get` prints it without its final line break;
/// or, with `field`, that field of the entry (see [`pick_field`]). `rbw get <name>` would fall
/// back to a substring of a name or a site address and could read another entry than the one
/// the owner confirmed, so the entry is looked up in `rbw list` first and read by its id. A
/// locked `rbw` is not refused: `rbw list` unlocks it, asking on this machine, and a prompt
/// nobody answers stays open there after the read gave up. One read runs at a time. Errors name
/// why: `rbw` is missing, the vault is locked and was not unlocked, no entry or several are so
/// named, the field is missing, ambiguous or empty, the value is empty, not UTF-8 or over
/// [`MAX_SECRET`]; an error carries only names, counts and the first line of `rbw`'s stderr,
/// never its stdout. All the calls, and the wait for another read, share [`READ_TIMEOUT`].
/// The two errors that typing the value would not help with are a [`Refused`].
///
/// `as_ssh_key`: the value is given as an ssh key, which goes into the agent's ssh-agent only. An
/// entry of type SSH Key given as anything else (a plain secret, a provider's key) is refused,
/// whatever the field, before anything is read: a plain secret becomes a file the agent reads,
/// and the key's private half would be in reach of the model.
pub async fn read_for(
    rbw: &Path,
    entry: &str,
    field: Option<&str>,
    as_ssh_key: bool,
) -> anyhow::Result<String> {
    read_within(rbw, entry, field, as_ssh_key, READ_TIMEOUT).await
}

/// [`read_for`] as an ssh key.
#[cfg(test)]
pub async fn read(rbw: &Path, entry: &str, field: Option<&str>) -> anyhow::Result<String> {
    read_within(rbw, entry, field, true, READ_TIMEOUT).await
}

async fn read_within(
    rbw: &Path,
    entry: &str,
    field: Option<&str>,
    as_ssh_key: bool,
    limit: Duration,
) -> anyhow::Result<String> {
    // Set by the first call, so that a timeout later knows whether it waited for an unlock.
    let was_locked = AtomicBool::new(false);
    // Until this read holds [`READS`]: a timeout before that waited for another read.
    let queued = AtomicBool::new(true);
    let read = async {
        let _one_at_a_time = READS.lock().await;
        queued.store(false, Ordering::Relaxed);
        read_entry(rbw, entry, field, as_ssh_key, &was_locked).await
    };
    tokio::time::timeout(limit, read).await.map_err(|_| {
        if queued.load(Ordering::Relaxed) {
            anyhow::anyhow!(
                "another read of Vaultwarden on this machine waits for its unlock: answer rbw's \
                 prompt there"
            )
        } else if was_locked.load(Ordering::Relaxed) {
            anyhow::anyhow!(
                "Vaultwarden is locked on this machine and its unlock was not answered in \
                     time: answer rbw's prompt there"
            )
        } else {
            anyhow::anyhow!("rbw did not answer in time")
        }
    })?
}

/// An `rbw` call that is killed if its future is dropped (the timeout).
fn rbw_cmd(rbw: &Path) -> Command {
    let mut c = Command::new(rbw);
    c.stdin(Stdio::null()).kill_on_drop(true);
    c
}

async fn read_entry(
    rbw: &Path,
    entry: &str,
    field: Option<&str>,
    as_ssh_key: bool,
    was_locked: &AtomicBool,
) -> anyhow::Result<String> {
    // Only to learn whether `rbw list` below is going to ask for an unlock.
    let unlocked = rbw_cmd(rbw)
        .arg("unlocked")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|_| anyhow::anyhow!("rbw is not installed on this machine"))?;
    was_locked.store(!unlocked.success(), Ordering::Relaxed);
    let list = rbw_cmd(rbw)
        .args(["list", "--fields", "id,name,type"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running rbw list")?;
    if !list.status.success() {
        let line = first_line(&list.stderr);
        // `--fields type` (and `get --field private_key`) came with rbw 1.15.0; an older one
        // answers "unknown field type" before it asks to unlock, and `rbw unlocked` fails for it
        // too, so this is checked first.
        if line.contains("unknown field") {
            bail!("this machine's rbw is older than 1.15: update it");
        }
        if was_locked.load(Ordering::Relaxed) {
            bail!("Vaultwarden is locked on this machine and rbw could not unlock it: {line}");
        }
        bail!("rbw list failed: {line}");
    }
    // Tab-separated `id, name, type`, one entry per line: the lines named, or numbered, `entry`.
    let listing = String::from_utf8_lossy(&list.stdout);
    let found: Vec<(&str, &str)> = listing
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (id, name) = (f.next()?, f.next()?);
            (name == entry || id == entry).then(|| (id, f.next().unwrap_or("")))
        })
        .collect();
    let (id, kind) = match found.as_slice() {
        [] => bail!("no Vaultwarden entry is named {entry}"),
        [one] => *one,
        many => bail!(
            "{} Vaultwarden entries are named {entry}: give the id of one",
            many.len()
        ),
    };
    if kind == SSH_KEY && !as_ssh_key {
        return Err(anyhow::Error::new(Refused(format!(
            "{entry} is an SSH key in Vaultwarden: ask for it as an ssh key, so that it goes into \
             the agent's ssh-agent and not into a file the agent reads"
        ))));
    }
    let mut get = rbw_cmd(rbw);
    get.arg("get");
    match field {
        // `get --field` matches a custom field by a substring of its name and could print
        // another one than the field asked for, so the whole entry is read and picked here.
        Some(_) => {
            get.arg("--raw");
        }
        None if kind == SSH_KEY => {
            get.args(["--field", "private_key"]);
        }
        None => {}
    }
    let out = get
        .args(["--", id])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running rbw get")?;
    if !out.status.success() {
        bail!("rbw get failed: {}", first_line(&out.stderr));
    }
    let value = match field {
        Some(f) => pick_field(&out.stdout, f)?.into_bytes(),
        None => {
            let mut value = out.stdout;
            if value.ends_with(b"\n") {
                value.pop();
                if value.ends_with(b"\r") {
                    value.pop();
                }
            }
            value
        }
    };
    if value.is_empty() {
        bail!("the Vaultwarden entry has an empty value");
    }
    if value.len() > MAX_SECRET {
        return Err(anyhow::Error::new(Refused(format!(
            "the Vaultwarden entry is longer than {} KiB",
            MAX_SECRET / 1024
        ))));
    }
    String::from_utf8(value).map_err(|_| anyhow::anyhow!("the Vaultwarden entry is not UTF-8 text"))
}

/// The field `field` of the entry `rbw get --raw` printed as JSON `{notes, data, fields: [{name,
/// value}]}`, by its exact, case-sensitive name: `notes` is the entry's notes; else a key of
/// `data` (its shape depends on the entry's type: a login's `username`, `password`, `totp`; a
/// card's `number`, `code`; an identity's names; an ssh key's `public_key`, `fingerprint`,
/// `private_key`) whose value is text; else the one custom field of that name. `totp` is the
/// key as stored, not a code. The error names the field and counts, never the JSON.
fn pick_field(raw: &[u8], field: &str) -> anyhow::Result<String> {
    use serde_json::Value;
    let entry: Value = serde_json::from_slice(raw)
        .map_err(|_| anyhow::anyhow!("rbw get --raw did not print JSON"))?;
    let text = |v: &Value| match v {
        Value::String(s) if !s.is_empty() => Ok(s.clone()),
        Value::Null | Value::String(_) => Err(anyhow::anyhow!(
            "the field {field} of the Vaultwarden entry is empty"
        )),
        _ => Err(anyhow::anyhow!(
            "the field {field} of the Vaultwarden entry has no text value"
        )),
    };
    if field == "notes" {
        return text(&entry["notes"]);
    }
    let in_data = entry["data"].get(field);
    if let Some(Value::String(_)) = in_data {
        return text(&in_data.cloned().unwrap_or_default());
    }
    let custom: Vec<&Value> = entry["fields"]
        .as_array()
        .map(|a| a.iter().filter(|f| f["name"] == field).collect())
        .unwrap_or_default();
    match custom.as_slice() {
        [one] => text(&one["value"]),
        [] => match in_data {
            Some(v) => text(v),
            None => bail!("the Vaultwarden entry has no field {field}"),
        },
        many => bail!(
            "{} fields of the Vaultwarden entry are named {field}",
            many.len()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Held for the whole of a test that keeps [`READS`] for long (a hanging or a slow `rbw`):
    /// another such test would otherwise wait for it inside its own limit.
    static HANGS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// A fake `rbw` in `dir` that runs `body`, a shell script over the subcommand in `$1`.
    fn fake(dir: &Path, body: &str) -> std::path::PathBuf {
        let path = dir.join("rbw");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Another test's fork may still hold the write handle (`ETXTBSY`): run the script once
        // until it can be run, as `read` would report a missing `rbw` for it.
        for _ in 0..100 {
            match std::process::Command::new(&path).arg("--probe").output() {
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => break,
            }
        }
        path
    }

    /// A fake `rbw` that is unlocked, lists `listing` (tab-separated lines) and answers
    /// `get` with `get_body`.
    fn vault(dir: &Path, listing: &str, get_body: &str) -> std::path::PathBuf {
        fake(
            dir,
            &format!(
                "case \"$1\" in unlocked) exit 0;; list) printf '{listing}';; get) {get_body};; esac"
            ),
        )
    }

    #[tokio::test]
    async fn the_value_is_read_by_the_id_of_the_exact_name() {
        let dir = tempfile::tempdir().unwrap();
        // The substring `home` and the site-like name are not the entry.
        let rbw = vault(
            dir.path(),
            "id-1\\thome-wifi\\tLogin\\nid-2\\thome-wifi-guest\\tLogin\\n",
            r#"[ "$2" = "--" ] && [ "$3" = id-1 ] && printf 'correct horse\n'"#,
        );
        assert_eq!(
            read(&rbw, "home-wifi", None).await.unwrap(),
            "correct horse"
        );
        // An id given directly.
        assert_eq!(read(&rbw, "id-1", None).await.unwrap(), "correct horse");
    }

    #[tokio::test]
    async fn a_name_that_is_only_a_substring_is_no_entry() {
        let dir = tempfile::tempdir().unwrap();
        let rbw = vault(dir.path(), "id-1\\tprod-db-root\\tLogin\\n", "echo leaked");
        let e = read(&rbw, "db", None).await.unwrap_err().to_string();
        assert!(e.contains("no Vaultwarden entry is named db"), "{e}");
        assert!(!e.contains("leaked") && !e.contains("prod-db-root"), "{e}");
    }

    #[tokio::test]
    async fn two_entries_of_one_name_ask_for_an_id() {
        let dir = tempfile::tempdir().unwrap();
        let rbw = vault(
            dir.path(),
            "id-1\\tdb\\tLogin\\nid-2\\tdb\\tLogin\\n",
            "echo leaked",
        );
        let e = read(&rbw, "db", None).await.unwrap_err().to_string();
        assert!(
            e.contains("2 Vaultwarden entries are named db: give the id of one"),
            "{e}"
        );
        // By the id of one it reads.
        let rbw = vault(
            dir.path(),
            "id-1\\tdb\\tLogin\\nid-2\\tdb\\tLogin\\n",
            r#"[ "$3" = id-2 ] && printf 'two\n'"#,
        );
        assert_eq!(read(&rbw, "id-2", None).await.unwrap(), "two");
    }

    #[tokio::test]
    async fn an_ssh_key_is_read_as_its_private_field() {
        let dir = tempfile::tempdir().unwrap();
        let rbw = vault(
            dir.path(),
            "id-9\\tpi-lab\\tSSH Key\\n",
            r#"[ "$2" = --field ] && [ "$3" = private_key ] && [ "$4" = -- ] && [ "$5" = id-9 ] && printf 'PRIVATE\n' || printf 'PUBLIC\n'"#,
        );
        assert_eq!(read(&rbw, "pi-lab", None).await.unwrap(), "PRIVATE");
        // As a plain secret it would be a file the agent reads: refused, nothing read, and a
        // field of it as well.
        for field in [None, Some("public_key")] {
            let e = read_for(&rbw, "pi-lab", field, false)
                .await
                .unwrap_err()
                .to_string();
            assert!(e.contains("is an SSH key"), "{e}");
        }
    }

    #[tokio::test]
    async fn the_value_loses_exactly_one_line_break() {
        let dir = tempfile::tempdir().unwrap();
        let list = "id-1\\tx\\tLogin\\n";
        let crlf = vault(dir.path(), list, r"printf 'a b\r\n'");
        assert_eq!(read(&crlf, "x", None).await.unwrap(), "a b");
        let two = vault(dir.path(), list, r"printf 'a b\n\n'");
        assert_eq!(read(&two, "x", None).await.unwrap(), "a b\n");
    }

    #[tokio::test]
    async fn a_missing_rbw_is_told() {
        let dir = tempfile::tempdir().unwrap();
        let e = read(&dir.path().join("nope"), "x", None).await.unwrap_err();
        assert!(e.to_string().contains("not installed"), "{e}");
    }

    #[tokio::test]
    async fn a_locked_vault_is_unlocked_by_the_list_and_read() {
        let dir = tempfile::tempdir().unwrap();
        // `unlocked` says locked; `list` (which unlocks) and `get` work.
        let rbw = fake(
            dir.path(),
            r#"case "$1" in unlocked) exit 1;; list) printf 'id-1\tx\tLogin\n';; get) printf 'opened\n';; esac"#,
        );
        assert_eq!(read(&rbw, "x", None).await.unwrap(), "opened");
    }

    #[tokio::test]
    async fn a_locked_vault_that_stays_locked_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let rbw = fake(
            dir.path(),
            r#"case "$1" in unlocked) exit 1;; list) echo "pinentry cancelled" >&2; echo "second line" >&2; echo leaked; exit 1;; esac"#,
        );
        let e = read(&rbw, "x", None).await.unwrap_err().to_string();
        assert!(
            e == "Vaultwarden is locked on this machine and rbw could not unlock it: pinentry cancelled",
            "{e}"
        );
        assert!(!e.contains("second line") && !e.contains("leaked"), "{e}");
    }

    #[tokio::test]
    async fn an_unanswered_unlock_prompt_ends_the_read_at_the_limit() {
        let _hang = HANGS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let ran = dir.path().join("ran");
        // Locked, and `list` waits for a pinentry nobody answers.
        let rbw = fake(
            dir.path(),
            &format!(
                r#"case "$1" in unlocked) exit 1;; list) touch {ran}; exec sleep 60;; esac"#,
                ran = ran.display()
            ),
        );
        let start = std::time::Instant::now();
        let e = read_within(&rbw, "x", None, true, Duration::from_secs(2))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            e == "Vaultwarden is locked on this machine and its unlock was not answered in time: answer rbw's prompt there",
            "{e}"
        );
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(ran.exists(), "rbw list did not run");
    }

    #[tokio::test]
    async fn an_rbw_older_than_1_15_is_told() {
        let dir = tempfile::tempdir().unwrap();
        let rbw = fake(
            dir.path(),
            r#"case "$1" in unlocked) exit 0;; list) echo "rbw list: unknown field type" >&2; exit 1;; esac"#,
        );
        let e = read(&rbw, "x", None).await.unwrap_err().to_string();
        assert_eq!(e, "this machine's rbw is older than 1.15: update it");
        // `rbw unlocked` fails too when rbw-agent is not running; the old rbw is told, not a
        // locked vault.
        let rbw = fake(
            dir.path(),
            r#"case "$1" in unlocked) exit 1;; list) echo "rbw list: unknown field type" >&2; exit 1;; esac"#,
        );
        let e = read(&rbw, "x", None).await.unwrap_err().to_string();
        assert_eq!(e, "this machine's rbw is older than 1.15: update it");
    }

    #[tokio::test]
    async fn reads_wait_for_one_another_so_only_one_unlock_is_asked() {
        let _hang = HANGS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        // `list` takes a while, as an unlock a person answers does.
        let rbw = fake(
            dir.path(),
            &format!(
                r#"case "$1" in unlocked) exit 0;; list) echo start >> {log}; sleep 0.3; echo end >> {log}; printf 'id-1\tx\tLogin\n';; get) printf 'v\n';; esac"#,
                log = log.display()
            ),
        );
        let (a, b) = tokio::join!(read(&rbw, "x", None), read(&rbw, "x", None));
        assert_eq!((a.unwrap(), b.unwrap()), ("v".to_string(), "v".to_string()));
        let lines = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            lines.lines().collect::<Vec<_>>(),
            ["start", "end", "start", "end"]
        );
    }

    #[tokio::test]
    async fn a_failed_get_carries_stderr_and_never_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let rbw = vault(
            dir.path(),
            "id-1\\tx\\tLogin\\n",
            r#"echo "the-secret-value"; echo "no entry found" >&2; echo "second line" >&2; exit 1"#,
        );
        let e = read(&rbw, "x", None).await.unwrap_err().to_string();
        assert!(e.contains("no entry found"), "{e}");
        assert!(
            !e.contains("the-secret-value") && !e.contains("second line"),
            "{e}"
        );
    }

    #[tokio::test]
    async fn an_empty_or_binary_value_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let list = "id-1\\tx\\tLogin\\n";
        let empty = vault(dir.path(), list, "echo");
        assert!(read(&empty, "x", None).await.is_err());
        let bin = vault(dir.path(), list, r"printf '\377\376'");
        assert!(read(&bin, "x", None)
            .await
            .unwrap_err()
            .to_string()
            .contains("UTF-8"));
    }

    #[tokio::test]
    async fn a_hanging_rbw_ends_the_read_at_the_limit() {
        let _hang = HANGS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let ran = dir.path().join("ran");
        // `unlocked` never answers (a reprompt window holds the agent's lock).
        let rbw = fake(
            dir.path(),
            &format!(
                r#"[ "$1" = unlocked ] && {{ touch {ran}; exec sleep 60; }}; exit 1"#,
                ran = ran.display()
            ),
        );
        let start = std::time::Instant::now();
        // 2 s, not less: the read may first wait on `READS` behind a short test's read.
        let e = read_within(&rbw, "x", None, true, Duration::from_secs(2))
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("did not answer"), "{e}");
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(ran.exists(), "rbw unlocked did not run");
    }

    #[tokio::test]
    async fn a_read_that_waits_for_another_says_so_and_runs_no_rbw() {
        let _hang = HANGS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let ran = dir.path().join("ran");
        // Any call of it leaves the marker; `fake` itself ran it once as a probe.
        let rbw = fake(dir.path(), &format!("touch {}; exit 1", ran.display()));
        let _ = std::fs::remove_file(&ran);
        // Another read holds the one-at-a-time lock for the whole limit.
        let _other = READS.lock().await;
        let e = read_within(&rbw, "x", None, true, Duration::from_millis(300))
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(
            e,
            "another read of Vaultwarden on this machine waits for its unlock: answer rbw's prompt there"
        );
        assert!(!ran.exists(), "rbw ran while the read was queued");
    }

    #[tokio::test]
    async fn only_the_refusals_typing_cannot_get_past_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = vault(dir.path(), "id-1\\tk\\tSSH Key\\n", "echo leaked");
        let e = read_for(&ssh, "k", None, false).await.unwrap_err();
        assert!(e.is::<Refused>(), "{e}");
        let big = vault(
            dir.path(),
            "id-1\\tb\\tLogin\\n",
            &format!("head -c {} /dev/zero | tr '\\0' a", MAX_SECRET + 1),
        );
        let e = read(&big, "b", None).await.unwrap_err();
        assert!(e.to_string().contains("longer than"), "{e}");
        assert!(e.is::<Refused>(), "{e}");
        let locked = fake(
            dir.path(),
            r#"case "$1" in unlocked) exit 1;; list) exit 1;; esac"#,
        );
        let e = read(&locked, "x", None).await.unwrap_err();
        assert!(!e.is::<Refused>(), "{e}");
    }

    /// What `rbw get --raw` prints for a login with custom fields.
    const RAW_LOGIN: &str = r#"{"id":"id-1","folder":null,"name":"x","data":{"username":"alice","password":"pw","totp":"JBSWY3DP","uris":[{"uri":"https://a.test"}]},"fields":[{"name":"api-key-old","value":"old","type":"text"},{"name":"api-key","value":"new","type":"hidden"},{"name":"twin","value":"1","type":"text"},{"name":"twin","value":"2","type":"text"},{"name":"blank","value":null,"type":"linked"}],"notes":"a note","history":[]}"#;

    /// A fake `rbw` that lists one login and prints `raw` for `get --raw -- id-1` only.
    fn raw_vault(dir: &Path, kind: &str, raw: &str) -> std::path::PathBuf {
        vault(
            dir,
            &format!("id-1\\tx\\t{kind}\\n"),
            &format!(
                r#"[ "$2" = --raw ] && [ "$3" = -- ] && [ "$4" = id-1 ] && printf '%s\n' '{raw}'"#
            ),
        )
    }

    #[tokio::test]
    async fn a_field_of_the_entry_is_picked_from_its_raw_form() {
        let dir = tempfile::tempdir().unwrap();
        let rbw = raw_vault(dir.path(), "Login", RAW_LOGIN);
        let get = |f: &'static str| {
            let rbw = rbw.clone();
            async move { read(&rbw, "x", Some(f)).await }
        };
        assert_eq!(get("username").await.unwrap(), "alice");
        assert_eq!(get("password").await.unwrap(), "pw");
        assert_eq!(get("totp").await.unwrap(), "JBSWY3DP");
        assert_eq!(get("notes").await.unwrap(), "a note");
        // The name is exact: `api-key` is not `api-key-old`, which comes first and contains it.
        assert_eq!(get("api-key").await.unwrap(), "new");
        assert_eq!(get("api-key-old").await.unwrap(), "old");
    }

    #[tokio::test]
    async fn an_ssh_keys_public_key_is_a_field() {
        let dir = tempfile::tempdir().unwrap();
        let raw = r#"{"id":"id-1","name":"x","data":{"public_key":"ssh-ed25519 AAAA","fingerprint":"SHA256:z","private_key":"PRIVATE"},"fields":[],"notes":null}"#;
        let rbw = raw_vault(dir.path(), "SSH Key", raw);
        assert_eq!(
            read(&rbw, "x", Some("public_key")).await.unwrap(),
            "ssh-ed25519 AAAA"
        );
    }

    #[tokio::test]
    async fn a_field_that_is_missing_ambiguous_or_empty_is_told() {
        let dir = tempfile::tempdir().unwrap();
        let rbw = raw_vault(dir.path(), "Login", RAW_LOGIN);
        let err = |f: &'static str| {
            let rbw = rbw.clone();
            async move { read(&rbw, "x", Some(f)).await.unwrap_err().to_string() }
        };
        assert!(err("nope").await.contains("has no field nope"));
        assert!(err("twin")
            .await
            .contains("2 fields of the Vaultwarden entry are named twin"));
        assert!(err("blank")
            .await
            .contains("field blank of the Vaultwarden entry is empty"));
        assert!(err("uris")
            .await
            .contains("field uris of the Vaultwarden entry has no text value"));
        // A note that is null.
        let raw = r#"{"data":{"password":"pw"},"fields":[],"notes":null}"#;
        let rbw = raw_vault(dir.path(), "Login", raw);
        let e = read(&rbw, "x", Some("notes"))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("field notes of the Vaultwarden entry is empty"),
            "{e}"
        );
        // A secure note has `data: null`; a field is still found among the custom ones.
        let raw = r#"{"data":null,"fields":[{"name":"k","value":"v"}],"notes":"n"}"#;
        let rbw = raw_vault(dir.path(), "Secure Note", raw);
        assert_eq!(read(&rbw, "x", Some("k")).await.unwrap(), "v");
        // What is not JSON is told without what was printed.
        let rbw = raw_vault(dir.path(), "Login", "leaked, not json");
        let e = read(&rbw, "x", Some("notes"))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("did not print JSON") && !e.contains("leaked"),
            "{e}"
        );
    }

    #[tokio::test]
    async fn the_entrys_own_data_wins_over_a_custom_field_of_the_same_name() {
        let dir = tempfile::tempdir().unwrap();
        // A custom `password` beside `data.password`: the data wins.
        let raw = r#"{"data":{"password":"real"},"fields":[{"name":"password","value":"custom"}],"notes":null}"#;
        let rbw = raw_vault(dir.path(), "Login", raw);
        assert_eq!(read(&rbw, "x", Some("password")).await.unwrap(), "real");
        // A `data.totp` that is null beside a custom `totp`: the custom field is the one.
        let raw = r#"{"data":{"password":"p","totp":null},"fields":[{"name":"totp","value":"JBSWY3DP"}],"notes":null}"#;
        let rbw = raw_vault(dir.path(), "Login", raw);
        assert_eq!(read(&rbw, "x", Some("totp")).await.unwrap(), "JBSWY3DP");
    }
}
