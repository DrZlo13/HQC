//! `title.request` (T24, SPEC §3 Session, §8.1): the hub asks for a session title by its first
//! prompt and first answer; the worker asks Claude Code on its subscription.
//!
//! One `claude -p` per request, not through the session's adapter or in its process: the
//! same binary and environment as the adapter (without `CLAUDE_CODE_ENTRYPOINT`), a fixed
//! command line ([`args`]) with no tools, no MCP servers, no settings files and no
//! transcript (`--no-session-persistence`: nothing for the mirror or the VS Code history),
//! the texts on stdin. The working directory is an empty folder of the worker,
//! `$HQC_HOME/titles/<slot>`, created before and removed after the run; the slot numbers are
//! reused so that Claude Code's per-folder state stays bounded. At most [`MAX_CONCURRENT`]
//! requests run at once, more are refused at once (the hub falls back to the prompt); a run
//! is killed after [`TIMEOUT`]. The answer is cleaned ([`clean`]) to one line of at most
//! [`MAX_TITLE`] characters.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use hqc_proto::frame::MAX_TITLE;
use tokio::io::AsyncWriteExt;

use crate::adapter::claude_code::child_env;
use crate::config::Config;

/// `title.request` runs at once per worker.
pub const MAX_CONCURRENT: usize = 2;
/// The whole `claude -p` run.
pub const TIMEOUT: Duration = Duration::from_secs(25);
pub const MODEL: &str = "claude-haiku-5-5";
pub const SYSTEM_PROMPT: &str =
    "Write a short title for this conversation, at most 60 characters, \
    in the language the user writes in. Reply with the title only, without quotes or a \
    trailing period.";

/// The command line after the program; the texts go to stdin ([`input`]).
pub fn args() -> Vec<String> {
    [
        "-p",
        "--model",
        MODEL,
        "--max-turns",
        "1",
        "--no-session-persistence",
        "--strict-mcp-config",
        "--setting-sources",
        "",
        "--tools",
        "",
        "--system-prompt",
        SYSTEM_PROMPT,
        "--output-format",
        "json",
    ]
    .map(String::from)
    .into()
}

/// What the model reads: the first prompt and the first answer of the session.
pub fn input(user_text: &str, agent_text: &str) -> String {
    format!("User message:\n{user_text}\n\nAgent answer:\n{agent_text}\n")
}

/// The model's answer as a title: one line without quotes and a trailing period, at most
/// [`MAX_TITLE`] characters; `None` when nothing is left.
pub fn clean(raw: &str) -> Option<String> {
    const QUOTES: &[char] = &['"', '«', '»', '“', '”', '„'];
    let line = raw
        .replace(QUOTES, "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let line = line
        .trim_matches(|c: char| c == '\'' || c == '`' || c.is_whitespace())
        .trim_end_matches('.')
        .trim();
    let cut: String = line.chars().take(MAX_TITLE).collect();
    let cut = cut.trim_end();
    (!cut.is_empty()).then(|| cut.to_string())
}

/// `result` of `--output-format json`: `{"type": "result", "is_error": false, "result":
/// "<text>", ...}`.
pub fn parse_output(stdout: &[u8]) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_slice(stdout)
        .map_err(|e| format!("claude printed no JSON result: {e}"))?;
    let text = v["result"].as_str().unwrap_or_default();
    if v["is_error"].as_bool().unwrap_or(true) {
        return Err(format!(
            "claude failed ({}): {}",
            v["subtype"].as_str().unwrap_or("?"),
            text.chars().take(300).collect::<String>()
        ));
    }
    clean(text).ok_or_else(|| "claude returned an empty title".into())
}

#[derive(Debug, PartialEq, Eq)]
pub enum TitleError {
    /// [`MAX_CONCURRENT`] requests already run.
    Busy,
    Failed(String),
}

pub struct Titler {
    program: PathBuf,
    /// `$HQC_HOME/titles`: the run folders `<root>/<slot>`.
    root: PathBuf,
    slots: Mutex<[bool; MAX_CONCURRENT]>,
    timeout: Duration,
}

/// A taken slot; frees it and removes its folder on drop.
struct Slot<'a> {
    titler: &'a Titler,
    index: usize,
    dir: PathBuf,
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.dir) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(dir = %self.dir.display(), "title folder not removed: {e}");
            }
        }
        self.titler.slots.lock().unwrap()[self.index] = false;
    }
}

impl Titler {
    pub fn new(program: PathBuf, root: PathBuf, timeout: Duration) -> Self {
        Self {
            program,
            root,
            slots: Mutex::new([false; MAX_CONCURRENT]),
            timeout,
        }
    }

    /// `claude_bin` (else `claude` from `PATH`); the folders next to `scratch_root`, in
    /// `$HQC_HOME/titles`.
    pub fn from_config(cfg: &Config) -> Self {
        Self::new(
            cfg.claude_bin
                .clone()
                .unwrap_or_else(|| PathBuf::from("claude")),
            cfg.scratch_root.with_file_name("titles"),
            TIMEOUT,
        )
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn take_slot(&self) -> Result<Slot<'_>, TitleError> {
        let index = {
            let mut slots = self.slots.lock().unwrap();
            let i = slots.iter().position(|b| !b).ok_or(TitleError::Busy)?;
            slots[i] = true;
            i
        };
        let slot = Slot {
            titler: self,
            index,
            dir: self.root.join(index.to_string()),
        };
        // Whatever a crashed run left there goes first: the folder must be empty.
        let _ = std::fs::remove_dir_all(&slot.dir);
        create_private_dir(&slot.dir)
            .map_err(|e| TitleError::Failed(format!("{}: {e}", slot.dir.display())))?;
        Ok(slot)
    }

    /// One `claude -p` run: the title, or why there is none.
    pub async fn title(&self, user_text: &str, agent_text: &str) -> Result<String, TitleError> {
        let slot = self.take_slot()?;
        let mut env = child_env(std::env::vars());
        env.retain(|(k, _)| k != "CLAUDE_CODE_ENTRYPOINT");
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(args())
            .current_dir(&slot.dir)
            .env_clear()
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let failed = |e: String| TitleError::Failed(e);
        let mut child = cmd
            .spawn()
            .map_err(|e| failed(format!("spawning {}: {e}", self.program.display())))?;
        let pid = child.id();
        let mut stdin = child.stdin.take().expect("piped stdin");
        let text = input(user_text, agent_text);
        let run = async move {
            // A CLI that exits at once (not logged in, bad flags) closes its stdin before the
            // prompt is written: that broken pipe is not the reason, its stderr and exit
            // status are, so keep going and report those.
            if let Err(e) = stdin.write_all(text.as_bytes()).await {
                if e.kind() != std::io::ErrorKind::BrokenPipe {
                    return Err(e);
                }
            }
            drop(stdin);
            child.wait_with_output().await
        };
        let out = match tokio::time::timeout(self.timeout, run).await {
            Ok(r) => r.map_err(|e| failed(format!("running claude: {e}")))?,
            Err(_) => {
                // The child itself goes with `kill_on_drop`; whatever it started goes too.
                #[cfg(unix)]
                if let Some(pid) = pid {
                    // SAFETY: a plain syscall on the child's own group (`process_group(0)`).
                    unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
                }
                return Err(failed(format!("no answer in {:?}", self.timeout)));
            }
        };
        let r = parse_output(&out.stdout);
        if !out.status.success() && r.is_ok() {
            return Err(failed(format!("claude exited with {}", out.status)));
        }
        r.map_err(|e| {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let tail: String = stderr.trim().chars().take(300).collect();
            if tail.is_empty() {
                failed(e)
            } else {
                failed(format!("{e}; stderr: {tail}"))
            }
        })
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn command_line() {
        let a = args();
        let pairs = [
            ("--model", "claude-haiku-5-5"),
            ("--max-turns", "1"),
            ("--setting-sources", ""),
            ("--tools", ""),
            ("--system-prompt", SYSTEM_PROMPT),
            ("--output-format", "json"),
        ];
        for (flag, value) in pairs {
            let i = a
                .iter()
                .position(|x| x == flag)
                .unwrap_or_else(|| panic!("{flag}"));
            assert_eq!(a[i + 1], value, "{flag}");
        }
        assert_eq!(a[0], "-p");
        assert!(a.contains(&"--no-session-persistence".to_string()));
        assert!(a.contains(&"--strict-mcp-config".to_string()));
        // Nothing that would resume, persist or load anything else.
        for no in [
            "--resume",
            "--session-id",
            "--mcp-config",
            "--settings",
            "--verbose",
        ] {
            assert!(!a.contains(&no.to_string()), "{no}");
        }
        assert_eq!(a.len(), 15);
    }

    #[test]
    fn answers_are_cleaned() {
        assert_eq!(clean("«Починка сборки»").as_deref(), Some("Починка сборки"));
        assert_eq!(clean("\"Fix CI.\"\n").as_deref(), Some("Fix CI"));
        assert_eq!(
            clean(" Кронштейн\nкамеры\r\n").as_deref(),
            Some("Кронштейн камеры")
        );
        assert_eq!(clean("'quoted'").as_deref(), Some("quoted"));
        assert_eq!(clean(" \n \"\" "), None);
        let t = clean(&"слово ".repeat(30)).unwrap();
        assert_eq!(
            t.chars().count(),
            59,
            "60 characters, the trailing space trimmed"
        );
        assert_eq!(clean(&"я".repeat(100)).unwrap().chars().count(), 60);
    }

    #[test]
    fn output_is_parsed() {
        let ok =
            br#"{"type":"result","subtype":"success","is_error":false,"result":"\"Title\"\n"}"#;
        assert_eq!(parse_output(ok).unwrap(), "Title");
        let err = br#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":""}"#;
        assert!(parse_output(err).unwrap_err().contains("error_max_turns"));
        let empty = br#"{"type":"result","subtype":"success","is_error":false,"result":" "}"#;
        assert!(parse_output(empty).is_err());
        assert!(parse_output(b"Not logged in").is_err());
    }

    /// A fake `claude`: records its arguments, folder and stdin next to `log`, sleeps
    /// `HQC_FAKE_SLEEP` seconds and prints a result.
    fn fake_claude(dir: &Path, result: &str) -> PathBuf {
        let bin = dir.join("claude");
        let log = dir.join("log").display().to_string();
        let script = format!(
            "#!/bin/sh\n\
             n=$$\n\
             printf '%s\\n' \"$@\" > '{log}'.$n.args\n\
             pwd -P > '{log}'.$n.cwd\n\
             ls -A > '{log}'.$n.ls\n\
             cat > '{log}'.$n.stdin\n\
             env > '{log}'.$n.env\n\
             sleep \"${{HQC_FAKE_SLEEP:-0}}\"\n\
             printf '%s' '{result}'\n"
        );
        std::fs::write(&bin, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        bin
    }

    fn logs(dir: &Path, ext: &str) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                (p.extension().and_then(|x| x.to_str()) == Some(ext))
                    .then(|| std::fs::read_to_string(p).unwrap())
            })
            .collect();
        v.sort();
        v
    }

    const OK: &str =
        r#"{"type":"result","subtype":"success","is_error":false,"result":"«Починка сборки.»"}"#;

    #[tokio::test]
    async fn one_run_in_a_fresh_folder_that_goes() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = fake_claude(tmp.path(), OK);
        let root = tmp.path().join("home/titles");
        let t = Titler::new(bin, root.clone(), Duration::from_secs(10));
        let title = t.title("почини сборку", "готово").await.unwrap();
        assert_eq!(title, "Починка сборки");
        let seen = logs(tmp.path(), "args");
        assert_eq!(seen, [args().join("\n") + "\n"]);
        let cwd = logs(tmp.path(), "cwd");
        assert!(cwd[0].trim().ends_with("home/titles/0"), "{}", cwd[0]);
        assert_eq!(logs(tmp.path(), "ls"), [""], "an empty folder");
        assert_eq!(
            logs(tmp.path(), "stdin"),
            [input("почини сборку", "готово")]
        );
        assert!(!logs(tmp.path(), "env")[0].contains("CLAUDE_CODE_ENTRYPOINT="));
        // The folder is gone, the root stays.
        assert!(!root.join("0").exists());
        assert!(root.exists());
    }

    #[tokio::test]
    async fn at_most_two_at_once() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = fake_claude(tmp.path(), OK);
        let root = tmp.path().join("titles");
        // A slow fake claude: a run takes a second.
        let script = std::fs::read_to_string(&bin)
            .unwrap()
            .replace("${HQC_FAKE_SLEEP:-0}", "1");
        std::fs::write(&bin, script).unwrap();
        let t = Arc::new(Titler::new(bin, root.clone(), Duration::from_secs(10)));
        let run = |t: Arc<Titler>| tokio::spawn(async move { t.title("a", "b").await });
        let (a, b) = (run(t.clone()), run(t.clone()));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(root.join("0").is_dir() && root.join("1").is_dir());
        assert_eq!(t.title("c", "d").await, Err(TitleError::Busy));
        assert_eq!(a.await.unwrap().unwrap(), "Починка сборки");
        assert_eq!(b.await.unwrap().unwrap(), "Починка сборки");
        assert!(!root.join("0").exists() && !root.join("1").exists());
        // Free again.
        assert!(t.title("e", "f").await.is_ok());
    }

    #[tokio::test]
    async fn failures_and_timeout_clean_up() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("titles");
        // Not logged in: plain text and exit 1.
        let bin = tmp.path().join("false-claude");
        std::fs::write(&bin, "#!/bin/sh\necho 'Not logged in' >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let t = Titler::new(bin, root.clone(), Duration::from_secs(10));
        match t.title("a", "b").await {
            Err(TitleError::Failed(e)) => assert!(e.contains("Not logged in"), "{e}"),
            other => panic!("{other:?}"),
        }
        assert!(!root.join("0").exists());
        // Exits before reading a prompt larger than the pipe buffer: the broken pipe on
        // stdin must not hide the CLI's own error (this raced on CI).
        let t = Titler::new(
            tmp.path().join("false-claude"),
            root.clone(),
            Duration::from_secs(10),
        );
        let big = "x".repeat(1 << 20);
        match t.title(&big, &big).await {
            Err(TitleError::Failed(e)) => assert!(e.contains("Not logged in"), "{e}"),
            other => panic!("{other:?}"),
        }
        // No such binary.
        let t = Titler::new(
            tmp.path().join("nope"),
            root.clone(),
            Duration::from_secs(10),
        );
        assert!(matches!(
            t.title("a", "b").await,
            Err(TitleError::Failed(_))
        ));
        assert!(!root.join("0").exists());
        // Too slow: killed after the timeout.
        let bin = fake_claude(tmp.path(), OK);
        let script = std::fs::read_to_string(&bin)
            .unwrap()
            .replace("${HQC_FAKE_SLEEP:-0}", "30");
        std::fs::write(&bin, script).unwrap();
        let t = Titler::new(bin, root.clone(), Duration::from_millis(500));
        let started = std::time::Instant::now();
        match t.title("a", "b").await {
            Err(TitleError::Failed(e)) => assert!(e.contains("no answer"), "{e}"),
            other => panic!("{other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!root.join("0").exists());
    }
}
