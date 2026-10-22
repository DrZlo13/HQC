//! `hello.capabilities` (SPEC §3 Worker, §5.4): `{claude_code?: {version}, acp_agents: []}`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

/// Runs `claude --version` (no model call) on `claude_bin` or `claude` from `PATH`.
/// Without a working binary `claude_code` is left out.
pub async fn detect(claude_bin: Option<&Path>) -> Value {
    let bin = claude_bin
        .map(Path::to_path_buf)
        .or_else(|| find_in_path("claude"));
    let version = match bin {
        Some(bin) => claude_version(&bin).await,
        None => None,
    };
    build(version.as_deref())
}

pub fn build(claude_version: Option<&str>) -> Value {
    let mut caps = json!({ "acp_agents": [] });
    if let Some(v) = claude_version {
        caps["claude_code"] = json!({ "version": v });
    }
    caps
}

async fn claude_version(bin: &Path) -> Option<String> {
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(bin)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await;
    match out {
        Ok(Ok(o)) if o.status.success() => {
            let v = parse_version(&String::from_utf8_lossy(&o.stdout));
            if v.is_none() {
                tracing::warn!(bin = %bin.display(), "unrecognised `claude --version` output");
            }
            v
        }
        Ok(Ok(o)) => {
            tracing::warn!(bin = %bin.display(), status = %o.status, "`claude --version` failed");
            None
        }
        Ok(Err(e)) => {
            tracing::info!(bin = %bin.display(), "Claude Code not available: {e}");
            None
        }
        Err(_) => {
            tracing::warn!(bin = %bin.display(), "`claude --version` timed out");
            None
        }
    }
}

/// `2.1.307 (Claude Code)` -> `2.1.307`: the first token made of digits and dots.
pub fn parse_version(s: &str) -> Option<String> {
    s.split_whitespace()
        .find(|t| t.contains('.') && t.chars().all(|c| c.is_ascii_digit() || c == '.'))
        .map(String::from)
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(
            parse_version("2.1.307 (Claude Code)\n").as_deref(),
            Some("2.1.307")
        );
        assert_eq!(parse_version("nope"), None);
        assert_eq!(build(None), json!({"acp_agents": []}));
        assert_eq!(build(Some("1.2"))["claude_code"]["version"], "1.2");
    }

    #[tokio::test]
    async fn missing_binary_is_left_out() {
        let caps = detect(Some(Path::new("/nonexistent/claude"))).await;
        assert!(caps.get("claude_code").is_none());
    }
}
