//! Live Claude Code processes outside hqc and the transcripts they hold open (SPEC §9.3).
//!
//! Claude Code keeps a registry of its running sessions in `<config>/sessions/<pid>.json`
//! (2.1.308): `pid`, `sessionId`, `cwd`, `entrypoint` (`claude-vscode` for the VS Code
//! extension, `sdk-cli` for `claude -p` and our adapter, others), `kind`, `status`
//! (`idle`/`busy`), `updatedAt` (ms since the epoch), `version` and more. Next to them lie
//! `<pid>.<hash>.key` files, which are not read. An entry is live while its process exists
//! (`kill -0`); a process that crashed leaves its file behind.
//!
//! The mirror reads the folder once per poll. Entries of the worker's own agent processes
//! are left out by pid (and process group: the adapter starts the CLI as a group leader);
//! an `sdk-cli` entry for a transcript the worker's own process runs is left out too, in
//! case the CLI runs under a wrapper with another pid. Unknown fields and unreadable files
//! are skipped.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use hqc_proto::frame::HeldBy;
use serde_json::Value;
use uuid::Uuid;

/// One live entry of the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    pub entrypoint: String,
    pub status: String,
    pub updated_at: Option<DateTime<Utc>>,
    pub cwd: Option<String>,
}

impl Holder {
    pub fn held_by(&self) -> HeldBy {
        HeldBy {
            entrypoint: self.entrypoint.clone(),
            status: self.status.clone(),
        }
    }
}

/// What the registry says about processes, injectable for tests.
pub trait Processes {
    fn alive(&self, pid: u32) -> bool;
    /// Process group of `pid`, if it can be read.
    fn group(&self, pid: u32) -> Option<u32>;
}

/// The real processes of this machine.
pub struct System;

impl Processes for System {
    #[cfg(unix)]
    fn alive(&self, pid: u32) -> bool {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        if pid <= 0 {
            return false;
        }
        // SAFETY: kill(2) with signal 0 only checks that the process exists.
        let r = unsafe { libc::kill(pid, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(not(unix))]
    fn alive(&self, _pid: u32) -> bool {
        false
    }

    #[cfg(unix)]
    fn group(&self, pid: u32) -> Option<u32> {
        let pid = i32::try_from(pid).ok()?;
        // SAFETY: getpgid(2) reads the process group of another process; no memory involved.
        let g = unsafe { libc::getpgid(pid) };
        u32::try_from(g).ok()
    }

    #[cfg(not(unix))]
    fn group(&self, _pid: u32) -> Option<u32> {
        None
    }
}

/// Holders by transcript uuid: live entries of `dir` that are not the worker's own.
/// `own_pids`: pids of the worker's agent processes; `own_live`: transcripts one of them
/// runs now. Of several entries for one transcript a `busy` one wins, then the newest.
pub fn read(
    dir: &Path,
    own_pids: &HashSet<u32>,
    own_live: impl Fn(Uuid) -> bool,
    procs: &dyn Processes,
) -> HashMap<Uuid, Holder> {
    let mut out: HashMap<Uuid, Holder> = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let path = e.path();
        // `<pid>.json` only: `<pid>.<hash>.key` and anything else is not ours to read.
        let is_entry = path.extension().is_some_and(|x| x == "json")
            && path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
        if !is_entry {
            continue;
        }
        let Some((session, h)) = std::fs::read(&path).ok().and_then(|b| parse(&b)) else {
            tracing::debug!(path = %path.display(), "unreadable session registry entry");
            continue;
        };
        if own_pids.contains(&h.pid)
            || procs.group(h.pid).is_some_and(|g| own_pids.contains(&g))
            || (h.entrypoint == "sdk-cli" && own_live(session))
            || !procs.alive(h.pid)
        {
            continue;
        }
        match out.get(&session) {
            Some(cur) if !better(&h, cur) => {}
            _ => {
                out.insert(session, h);
            }
        }
    }
    out
}

fn better(a: &Holder, b: &Holder) -> bool {
    match (a.status == "busy", b.status == "busy") {
        (true, false) => true,
        (false, true) => false,
        _ => a.updated_at > b.updated_at,
    }
}

/// One registry file; `None` without a pid or a session uuid.
pub fn parse(bytes: &[u8]) -> Option<(Uuid, Holder)> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    // The pid is a number on 2.1.308 and a string in entries seen elsewhere.
    let pid = match &v["pid"] {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .and_then(|p| u32::try_from(p).ok())
    .filter(|p| *p > 0)?;
    let session: Uuid = v["sessionId"].as_str()?.parse().ok()?;
    let text = |k: &str| v[k].as_str().map(str::to_string);
    let updated_at = match &v["updatedAt"] {
        Value::Number(n) => n.as_i64().and_then(DateTime::from_timestamp_millis),
        Value::String(s) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|t| t.with_timezone(&Utc)),
        _ => None,
    };
    Some((
        session,
        Holder {
            pid,
            entrypoint: text("entrypoint").unwrap_or_else(|| "unknown".into()),
            status: text("status").unwrap_or_else(|| "idle".into()),
            updated_at,
            cwd: text("cwd"),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Processes 100 (group 100), 200 (group 77: a child of the worker's agent 77) and 300.
    struct Fake;

    impl Processes for Fake {
        fn alive(&self, pid: u32) -> bool {
            [100, 200, 300, 77].contains(&pid)
        }
        fn group(&self, pid: u32) -> Option<u32> {
            match pid {
                200 => Some(77),
                p if self.alive(p) => Some(p),
                _ => None,
            }
        }
    }

    fn entry(pid: Value, session: Uuid, entrypoint: &str, status: &str) -> String {
        serde_json::json!({
            "pid": pid, "sessionId": session, "cwd": "/w/a", "entrypoint": entrypoint,
            "kind": "interactive", "status": status, "updatedAt": 1792779848554u64,
            "version": "2.1.308", "peerFeatures": ["notify_idle"], "somethingNew": {"x": 1}
        })
        .to_string()
    }

    #[test]
    fn live_entries_except_our_own() {
        let dir = tempfile::tempdir().unwrap();
        let (vscode, ours, child, dead, mine, sdk) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let w = |name: &str, s: String| std::fs::write(dir.path().join(name), s).unwrap();
        // VS Code, its pid as a string the way the extension's entries were seen.
        w(
            "100.json",
            entry(Value::from("100"), vscode, "claude-vscode", "idle"),
        );
        w("100.abcdef.key", "secret".into());
        // The worker's own agent and a process in its group.
        w("77.json", entry(Value::from(77), ours, "sdk-cli", "busy"));
        w(
            "200.json",
            entry(Value::from(200), child, "sdk-cli", "busy"),
        );
        // A crashed process left its file.
        w(
            "400.json",
            entry(Value::from(400), dead, "claude-vscode", "busy"),
        );
        // `claude -p` for a transcript our process runs: taken for ours.
        w("300.json", entry(Value::from(300), mine, "sdk-cli", "busy"));
        // Garbage and half-written files.
        w("500.json", "{\"pid\": 5".into());
        w("600.json", "[]".into());
        w("notes.json", entry(Value::from(100), sdk, "cli", "idle"));

        let own: HashSet<u32> = [77].into();
        let got = read(dir.path(), &own, |s| s == mine, &Fake);
        assert_eq!(got.len(), 1, "{got:?}");
        let h = &got[&vscode];
        assert_eq!(h.pid, 100);
        assert_eq!(
            h.held_by(),
            HeldBy {
                entrypoint: "claude-vscode".into(),
                status: "idle".into()
            }
        );
        assert_eq!(h.cwd.as_deref(), Some("/w/a"));
        assert_eq!(
            h.updated_at.unwrap().to_rfc3339(),
            "2026-10-23T18:24:08.554+00:00"
        );

        // Our process gone: the `claude -p` of the same transcript is someone else's.
        let got = read(dir.path(), &own, |_| false, &Fake);
        assert!(got.contains_key(&mine));
        assert!(!got.contains_key(&ours) && !got.contains_key(&child));
        assert!(!got.contains_key(&dead));
    }

    #[test]
    fn busy_wins_then_newest() {
        let dir = tempfile::tempdir().unwrap();
        let s = Uuid::new_v4();
        std::fs::write(
            dir.path().join("100.json"),
            entry(Value::from(100), s, "claude-vscode", "idle"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("300.json"),
            entry(Value::from(300), s, "cli", "busy"),
        )
        .unwrap();
        let got = read(dir.path(), &HashSet::new(), |_| false, &Fake);
        assert_eq!(got[&s].pid, 300);
    }

    #[test]
    fn no_folder_no_holders() {
        let got = read(
            Path::new("/nonexistent/hqc"),
            &HashSet::new(),
            |_| false,
            &System,
        );
        assert!(got.is_empty());
    }

    #[test]
    fn this_process_is_alive() {
        assert!(System.alive(std::process::id()));
        assert!(!System.alive(0));
    }
}
