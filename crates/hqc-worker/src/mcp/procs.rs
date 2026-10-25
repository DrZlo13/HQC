//! Process ancestry for the MCP socket (SPEC §2.1, §7.2): a connection is accepted for a
//! session only from a process that descends from the agent process the worker started for
//! that session. Another session's agent, same OS user, can read the token from the agent's
//! command line, but its own processes descend from its own agent.

/// The parent of `pid`; `None` if it is gone or cannot be read.
#[cfg(target_os = "macos")]
pub fn parent(pid: u32) -> Option<u32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    (n == size).then_some(info.pbi_ppid)
}

/// The parent of `pid`; `None` if it is gone or cannot be read.
#[cfg(target_os = "linux")]
pub fn parent(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `pid (comm) state ppid ...`; `comm` may contain spaces and parentheses.
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// Elsewhere the parent is not known, and no connection is accepted.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn parent(_pid: u32) -> Option<u32> {
    None
}

/// Longest chain of parents walked from a peer to its agent.
const MAX_DEPTH: usize = 32;

/// `pid` is `ancestor` or one of its descendants.
pub fn descends_from(pid: u32, ancestor: u32) -> bool {
    let mut p = pid;
    for _ in 0..MAX_DEPTH {
        if p == ancestor {
            return true;
        }
        match parent(p) {
            Some(next) if next != p && next > 1 => p = next,
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestry() {
        let me = std::process::id();
        assert!(descends_from(me, me));
        let parent_pid = parent(me).expect("our parent");
        assert!(descends_from(me, parent_pid));
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let c = child.id();
        assert_eq!(parent(c), Some(me));
        assert!(descends_from(c, me), "a child descends from us");
        assert!(!descends_from(me, c), "we do not descend from our child");
        assert!(!descends_from(me, 1), "launchd / init is never walked into");
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
