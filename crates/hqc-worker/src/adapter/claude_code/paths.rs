//! Where Claude Code keeps a session transcript: `<config>/projects/<encoded cwd>/<uuid>.jsonl`.
//!
//! The encoding follows the CLI (function `J0` in 2.1.308): every UTF-16 code unit of the
//! path that is not an ASCII letter or digit becomes `-`; a result longer than 200
//! characters is cut to 200 and gets `-` plus the base-36 of the absolute value of a 32-bit
//! string hash (`h = h * 31 + unit`). The CLI encodes `process.cwd()`, which is the real
//! path (`/tmp` on macOS is `/private/tmp`), so the working directory is canonicalized first.

use std::path::{Path, PathBuf};

use uuid::Uuid;

const MAX_ENCODED: usize = 200;

pub fn encode_project_dir(path: &str) -> String {
    let encoded: String = path
        .encode_utf16()
        .map(|u| match u8::try_from(u) {
            Ok(b) if b.is_ascii_alphanumeric() => b as char,
            _ => '-',
        })
        .collect();
    if encoded.len() <= MAX_ENCODED {
        return encoded;
    }
    format!(
        "{}-{}",
        &encoded[..MAX_ENCODED],
        base36(js_string_hash(path).unsigned_abs() as u64)
    )
}

/// `for (c of utf16) h = (h << 5) - h + c | 0`.
fn js_string_hash(s: &str) -> i32 {
    s.encode_utf16().fold(0i32, |h, u| {
        h.wrapping_shl(5).wrapping_sub(h).wrapping_add(u as i32)
    })
}

fn base36(mut n: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("ascii")
}

/// `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn default_config_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".claude"))
}

pub fn transcript_path(config_dir: &Path, workdir: &Path, session: &Uuid) -> PathBuf {
    let real = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    config_dir
        .join("projects")
        .join(encode_project_dir(&real.to_string_lossy()))
        .join(format!("{session}.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Directory names seen in `~/.claude/projects` on home-mac (T0 runs, 2.1.307).
    #[test]
    fn encodes_like_the_cli() {
        assert_eq!(
            encode_project_dir("/Users/home-mac/work/hqc/tmp/spike-once3"),
            "-Users-home-mac-work-hqc-tmp-spike-once3"
        );
        assert_eq!(encode_project_dir("/private/tmp"), "-private-tmp");
        assert_eq!(
            encode_project_dir("/Users/home-mac/work/hqc-t4"),
            "-Users-home-mac-work-hqc-t4"
        );
        assert_eq!(encode_project_dir("/a/b_c.d e"), "-a-b-c-d-e");
        // One UTF-16 unit each for BMP characters, two for the rest (JS regex without `u`).
        assert_eq!(encode_project_dir("/д"), "--");
        assert_eq!(encode_project_dir("/😀"), "---");
    }

    /// Reference values computed with the CLI's own function in node.
    #[test]
    fn long_paths_get_a_hash() {
        let long = format!("/{}", "x".repeat(250));
        assert_eq!(
            encode_project_dir(&long),
            format!("-{}-v734pd", "x".repeat(199))
        );
        assert_eq!(js_string_hash("hello"), 99162322);
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
    }

    #[test]
    fn transcript_path_shape() {
        let id = Uuid::nil();
        let p = transcript_path(Path::new("/c"), Path::new("/no/such/dir"), &id);
        assert_eq!(
            p,
            PathBuf::from("/c/projects/-no-such-dir/00000000-0000-0000-0000-000000000000.jsonl")
        );
    }
}
