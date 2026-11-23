//! Reading a secret from Vaultwarden (Bitwarden) through this machine's `rbw` (a `secret.put`
//! with `vault`, only with `vault = true` and `session_secrets` in `worker.toml`). The value
//! goes from `rbw` into the agent's secret file or ssh-agent here: it does not pass through the
//! hub, and nothing but the entry's name is ever logged or put into an error.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context};
use hqc_proto::frame::MAX_SECRET;
use tokio::process::Command;

/// How long all the `rbw` calls of one read may take together: a master-password reprompt window
/// holds `rbw-agent`'s lock, so even `rbw unlocked` can block; and a read must leave the hub's
/// 30 s wait for `secret.put` room for an ssh-add after it.
const READ_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest piece of `rbw`'s stderr an error carries, in characters.
const MAX_STDERR: usize = 300;
/// The `type` of a list line whose `get` prints the public key; its private half is a field.
const SSH_KEY: &str = "SSH Key";

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
/// whose name or id is exactly `entry`, as `rbw get` prints it without its final line break.
/// `rbw get <name>` would fall back to a substring of a name or a site address and could read
/// another entry than the one the owner confirmed, so the entry is looked up in `rbw list` first
/// and read by its id. Errors name why: `rbw` is missing, the vault is locked, no entry or
/// several are so named, the value is empty, not UTF-8 or over [`MAX_SECRET`]; an error carries
/// only counts and the first line of `rbw`'s stderr, never its stdout. All the calls share
/// [`READ_TIMEOUT`].
pub async fn read(rbw: &Path, entry: &str) -> anyhow::Result<String> {
    read_within(rbw, entry, READ_TIMEOUT).await
}

async fn read_within(rbw: &Path, entry: &str, limit: Duration) -> anyhow::Result<String> {
    tokio::time::timeout(limit, read_entry(rbw, entry))
        .await
        .map_err(|_| anyhow::anyhow!("rbw did not answer in time"))?
}

/// An `rbw` call that is killed if its future is dropped (the timeout).
fn rbw_cmd(rbw: &Path) -> Command {
    let mut c = Command::new(rbw);
    c.stdin(Stdio::null()).kill_on_drop(true);
    c
}

async fn read_entry(rbw: &Path, entry: &str) -> anyhow::Result<String> {
    let unlocked = rbw_cmd(rbw)
        .arg("unlocked")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|_| anyhow::anyhow!("rbw is not installed on this machine"))?;
    if !unlocked.success() {
        bail!("Vaultwarden is locked on this machine: unlock rbw here, or type the value");
    }
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
        // answers "unknown field type".
        if line.contains("unknown field") {
            bail!("this machine's rbw is older than 1.15: update it, or type the value");
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
    let mut get = rbw_cmd(rbw);
    get.arg("get");
    if kind == SSH_KEY {
        get.args(["--field", "private_key"]);
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
    let mut value = out.stdout;
    if value.ends_with(b"\n") {
        value.pop();
        if value.ends_with(b"\r") {
            value.pop();
        }
    }
    if value.is_empty() {
        bail!("the Vaultwarden entry has an empty value");
    }
    if value.len() > MAX_SECRET {
        bail!(
            "the Vaultwarden entry is longer than {} KiB",
            MAX_SECRET / 1024
        );
    }
    String::from_utf8(value).map_err(|_| anyhow::anyhow!("the Vaultwarden entry is not UTF-8 text"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A fake `rbw` in `dir` that runs `body`, a shell script over the subcommand in `$1`.
    fn fake(dir: &Path, body: &str) -> std::path::PathBuf {
        let path = dir.join("rbw");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
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
        assert_eq!(read(&rbw, "home-wifi").await.unwrap(), "correct horse");
        // An id given directly.
        assert_eq!(read(&rbw, "id-1").await.unwrap(), "correct horse");
    }

    #[tokio::test]
    async fn a_name_that_is_only_a_substring_is_no_entry() {
        let dir = tempfile::tempdir().unwrap();
        let rbw = vault(dir.path(), "id-1\\tprod-db-root\\tLogin\\n", "echo leaked");
        let e = read(&rbw, "db").await.unwrap_err().to_string();
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
        let e = read(&rbw, "db").await.unwrap_err().to_string();
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
        assert_eq!(read(&rbw, "id-2").await.unwrap(), "two");
    }

    #[tokio::test]
    async fn an_ssh_key_is_read_as_its_private_field() {
        let dir = tempfile::tempdir().unwrap();
        let rbw = vault(
            dir.path(),
            "id-9\\tpi-lab\\tSSH Key\\n",
            r#"[ "$2" = --field ] && [ "$3" = private_key ] && [ "$4" = -- ] && [ "$5" = id-9 ] && printf 'PRIVATE\n' || printf 'PUBLIC\n'"#,
        );
        assert_eq!(read(&rbw, "pi-lab").await.unwrap(), "PRIVATE");
    }

    #[tokio::test]
    async fn the_value_loses_exactly_one_line_break() {
        let dir = tempfile::tempdir().unwrap();
        let list = "id-1\\tx\\tLogin\\n";
        let crlf = vault(dir.path(), list, r"printf 'a b\r\n'");
        assert_eq!(read(&crlf, "x").await.unwrap(), "a b");
        let two = vault(dir.path(), list, r"printf 'a b\n\n'");
        assert_eq!(read(&two, "x").await.unwrap(), "a b\n");
    }

    #[tokio::test]
    async fn a_locked_vault_and_a_missing_rbw_are_told() {
        let dir = tempfile::tempdir().unwrap();
        let rbw = fake(dir.path(), r#"[ "$1" = unlocked ] && exit 1; echo leaked"#);
        let e = read(&rbw, "x").await.unwrap_err().to_string();
        assert!(e.contains("locked") && !e.contains("leaked"), "{e}");
        let e = read(&dir.path().join("nope"), "x").await.unwrap_err();
        assert!(e.to_string().contains("not installed"), "{e}");
    }

    #[tokio::test]
    async fn an_rbw_older_than_1_15_is_told() {
        let dir = tempfile::tempdir().unwrap();
        let rbw = fake(
            dir.path(),
            r#"case "$1" in unlocked) exit 0;; list) echo "rbw list: unknown field type" >&2; exit 1;; esac"#,
        );
        let e = read(&rbw, "x").await.unwrap_err().to_string();
        assert!(e.contains("older than 1.15"), "{e}");
    }

    #[tokio::test]
    async fn a_failed_get_carries_stderr_and_never_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let rbw = vault(
            dir.path(),
            "id-1\\tx\\tLogin\\n",
            r#"echo "the-secret-value"; echo "no entry found" >&2; echo "second line" >&2; exit 1"#,
        );
        let e = read(&rbw, "x").await.unwrap_err().to_string();
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
        assert!(read(&empty, "x").await.is_err());
        let bin = vault(dir.path(), list, r"printf '\377\376'");
        assert!(read(&bin, "x")
            .await
            .unwrap_err()
            .to_string()
            .contains("UTF-8"));
    }

    #[tokio::test]
    async fn a_hanging_rbw_ends_the_read_at_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        // `unlocked` never answers (a reprompt window holds the agent's lock).
        let rbw = fake(
            dir.path(),
            r#"[ "$1" = unlocked ] && exec sleep 60; exit 1"#,
        );
        let start = std::time::Instant::now();
        let e = read_within(&rbw, "x", Duration::from_millis(300))
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("did not answer"), "{e}");
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
