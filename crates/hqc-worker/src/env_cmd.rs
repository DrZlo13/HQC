//! `hqc-worker env` (T43, SPEC §14): the environments and bindings of `worker.toml`, and
//! `env which <path>` with the environment and the effective rules of a folder. Values of
//! `env` are printed as `***`; editing stays in `worker.toml`.

use std::fmt::Write;
use std::path::Path;

use anyhow::Context;
use hqc_proto::frame::{rule_source, DEFAULT_ENVIRONMENT};

use crate::config::{Config, Environment};

fn vars(out: &mut String, e: &Environment) {
    let names: Vec<String> = e.env.names().map(|n| format!("{n}=***")).collect();
    if !names.is_empty() {
        let _ = writeln!(out, "    env: {}", names.join(" "));
    }
}

fn list(out: &mut String, label: &str, rules: &[String]) {
    if !rules.is_empty() {
        let _ = writeln!(out, "    {label}: {}", rules.join(", "));
    }
}

/// `hqc-worker env`.
pub fn summary(cfg: &Config) -> String {
    let mut out = String::from("environments:\n");
    let _ = writeln!(
        out,
        "  {DEFAULT_ENVIRONMENT}: folders without a binding; [permissions] only, no local MCP"
    );
    for e in &cfg.environments {
        let _ = writeln!(out, "  {}:", e.name);
        vars(&mut out, e);
        let _ = writeln!(out, "    allow_local_mcp: {}", e.allow_local_mcp);
        list(&mut out, "unblock", &e.rules.unblock);
        list(&mut out, "allow", &e.rules.allow);
        list(&mut out, "deny", &e.rules.deny);
    }
    out.push_str("bindings:\n");
    if cfg.env_bindings.is_empty() {
        out.push_str("  none\n");
    }
    for b in &cfg.env_bindings {
        let _ = writeln!(out, "  {} -> {}", b.path, b.environment);
    }
    out
}

/// `hqc-worker env which <path>`: as a `session.start` in that folder would resolve it.
pub fn which(cfg: &Config, path: &Path) -> anyhow::Result<String> {
    let path = path
        .canonicalize()
        .with_context(|| format!("{}", path.display()))?;
    let e = cfg.environment(&path);
    let mut out = format!("path: {}\n", path.display());
    if !cfg.inside_roots(&path) {
        out.push_str("  (outside allowed_roots: no session can run here)\n");
    }
    match cfg.binding(&path) {
        Some(b) => {
            let _ = writeln!(out, "environment: {} (binding {})", e.name, b.path);
        }
        None => {
            let _ = writeln!(out, "environment: {} (no binding)", e.name);
        }
    }
    vars(&mut out, &e);
    let _ = writeln!(
        out,
        "    local MCP servers: {}",
        if e.allow_local_mcp {
            "allowed"
        } else {
            "off (--strict-mcp-config)"
        }
    );
    let p = &cfg.permissions;
    let _ = writeln!(
        out,
        "mode: {} by default (allow_auto: {})",
        p.mode.as_str(),
        p.allow_auto
    );
    let env_source = rule_source::environment(&e.name);
    let source = |r: &String, own: &[String]| {
        if own.contains(r) {
            env_source.as_str()
        } else {
            rule_source::WORKER
        }
    };
    out.push_str("deny (never runs, never asks):\n");
    for r in &e.permissions.deny {
        let _ = writeln!(out, "  {r}  [{}]", source(r, &e.rules.deny));
    }
    for r in &e.rules.unblock {
        let _ = writeln!(out, "  (lifted) {r}  [{env_source}]");
    }
    out.push_str("allow (runs without asking):\n");
    for r in &e.permissions.allow {
        let _ = writeln!(out, "  {r}  [{}]", source(r, &e.rules.allow));
    }
    out.push_str(
        "Claude Code's settings files and hqc's folder rules come on top (the rules screen shows them).\n",
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_never_printed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("infra/sub")).unwrap();
        let cfg = Config::parse(&format!(
            r#"hub_url = "https://h"
allowed_roots = [{root:?}]
[environments.infra]
env = {{ GITHUB_TOKEN = "ghp_secret_value" }}
[environments.infra.permissions]
unblock = ["Bash(ssh:*)"]
allow = ["Bash(ssh debian@192.168.1.20:*)"]
deny = ["Bash(git push:*)"]
[[env_bindings]]
path = "{}"
environment = "infra"
"#,
            root.join("infra").display()
        ))
        .unwrap();
        let s = summary(&cfg);
        assert!(s.contains("GITHUB_TOKEN=***"), "{s}");
        assert!(s.contains(&format!("{} -> infra", root.join("infra").display())));
        let w = which(&cfg, &root.join("infra/sub")).unwrap();
        assert!(w.contains("environment: infra (binding"), "{w}");
        assert!(w.contains("GITHUB_TOKEN=***"), "{w}");
        assert!(
            w.contains("Bash(git push:*)  [worker.toml [environments.infra]]"),
            "{w}"
        );
        assert!(w.contains("Bash(sudo:*)  [worker.toml]"), "{w}");
        assert!(w.contains("(lifted) Bash(ssh:*)"), "{w}");
        assert!(!w.contains("  Bash(ssh:*)  ["), "unblocked: {w}");
        let d = which(&cfg, &root).unwrap();
        assert!(d.contains("environment: default (no binding)"), "{d}");
        assert!(d.contains("Bash(ssh:*)  [worker.toml]"), "{d}");
        for out in [s, w, d, format!("{cfg:?}")] {
            assert!(!out.contains("ghp_secret_value"), "{out}");
        }
    }
}
