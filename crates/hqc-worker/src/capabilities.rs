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

/// The `--permission-mode` value that means "ask for everything not allowed by a rule":
/// `default` up to CLI 2.1.307, listed as `manual` by the `--help` of 2.1.308 (where
/// `default` is not among the choices). Read from `claude --help` (no model call); `default`
/// if the help cannot be read.
pub async fn claude_permission_mode(claude_bin: Option<&Path>) -> String {
    let bin = claude_bin
        .map(Path::to_path_buf)
        .or_else(|| find_in_path("claude"));
    let help = match bin {
        Some(bin) => tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new(&bin)
                .arg("--help")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned()),
        None => None,
    };
    help.as_deref()
        .map(permission_mode_from_help)
        .unwrap_or("default")
        .to_string()
}

/// `manual` if the choices of `--permission-mode` in `claude --help` name it and not
/// `default`, else `default`.
pub fn permission_mode_from_help(help: &str) -> &'static str {
    let Some(start) = help.find("--permission-mode") else {
        return "default";
    };
    let rest = &help[start..];
    // The option's text runs up to the next option line.
    let end = rest[2..].find("\n  --").map_or(rest.len(), |i| i + 2);
    let text = &rest[..end];
    if text.contains("\"manual\"") && !text.contains("\"default\"") {
        "manual"
    } else {
        "default"
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

    #[test]
    fn permission_mode_by_help() {
        // `claude --help` of 2.1.308.
        let new = r#"
  --permission-mode <mode>              Permission mode to use for the session
                                        (choices: "acceptEdits", "auto",
                                        "bypassPermissions", "manual",
                                        "dontAsk", "plan")
  --permission-prompts <target>         Who answers permission prompts with
                                        --print: "host" (the SDK host or
"#;
        assert_eq!(permission_mode_from_help(new), "manual");
        let old = r#"
  --permission-mode <mode>              Permission mode to use for the session
                                        (choices: "acceptEdits",
                                        "bypassPermissions", "default", "plan")
  --plugin-dir <paths...>               Load plugins
"#;
        assert_eq!(permission_mode_from_help(old), "default");
        assert_eq!(permission_mode_from_help("nothing"), "default");
    }

    #[tokio::test]
    async fn missing_binary_is_left_out() {
        let caps = detect(Some(Path::new("/nonexistent/claude"))).await;
        assert!(caps.get("claude_code").is_none());
    }
}
