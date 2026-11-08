//! `hqc-worker env` (T43, SPEC §14): the environments and bindings of `worker.toml`, and
//! `env which <path>` with the environment and the effective rules of a folder. Values of
//! `env` are printed as `***`, ssh keys by name; editing stays in `worker.toml`. `env add-key
//! <name> <file>` puts a key into the store (T44).

use std::fmt::Write;
use std::path::Path;

use anyhow::Context;
use hqc_proto::frame::{rule_source, DEFAULT_ENVIRONMENT};

use crate::config::{Config, Environment};
use crate::ssh::Store;

fn vars(out: &mut String, e: &Environment) {
    let names: Vec<String> = e.env.names().map(|n| format!("{n}=***")).collect();
    if !names.is_empty() {
        let _ = writeln!(out, "    env: {}", names.join(" "));
    }
    if !e.ssh_keys.is_empty() {
        let _ = writeln!(
            out,
            "    ssh_keys: {} (an ssh-agent of its own per session)",
            e.ssh_keys.join(", ")
        );
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
        "  {DEFAULT_ENVIRONMENT}: folders without a binding; [permissions] only, no ssh keys, no local MCP, no project settings"
    );
    let unbound = cfg.unbound_environments();
    for e in &cfg.environments {
        if unbound.contains(&e.name.as_str()) {
            let _ = writeln!(
                out,
                "  {}: (no binding names it: no folder runs in it)",
                e.name
            );
        } else {
            let _ = writeln!(out, "  {}:", e.name);
        }
        vars(&mut out, e);
        let _ = writeln!(out, "    allow_local_mcp: {}", e.allow_local_mcp);
        let _ = writeln!(out, "    project_settings: {}", e.project_settings);
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
    if e.ssh_keys.is_empty() {
        out.push_str("    ssh keys: none (no SSH_AUTH_SOCK)\n");
    }
    let _ = writeln!(
        out,
        "    local MCP servers: {}",
        if e.allow_local_mcp {
            "allowed"
        } else {
            "off (--strict-mcp-config)"
        }
    );
    let _ = writeln!(
        out,
        "    project settings: {}",
        if e.project_settings {
            "read (--setting-sources user,project,local)"
        } else {
            "off (--setting-sources user,local)"
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

/// `hqc-worker env add-key <name> <file>`: what was done and how to use it.
pub fn add_key(store: &Store, name: &str, file: &Path) -> anyhow::Result<String> {
    let dest = store.add_key(name, file)?;
    Ok(format!(
        "added {} (0600)\nuse it with ssh_keys = [\"{name}\"] in an [environments.<name>] of \
         worker.toml and restart the worker\n",
        dest.display()
    ))
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
        assert!(!s.contains("no binding names it"), "{s}");
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

    /// A permissions table under another environment's name makes an environment no folder
    /// runs in; the listing says so.
    #[test]
    fn unbound_environment_is_marked() {
        let cfg = Config::parse(
            "hub_url = \"https://h\"\nallowed_roots = [\"/a\"]\n[environments.infra]\n\
             allow_local_mcp = true\n[environments.homemac.permissions]\nunblock = [\"Bash(ssh:*)\"]\n\
             [[env_bindings]]\npath = \"/a/infra\"\nenvironment = \"infra\"\n",
        )
        .unwrap();
        let s = summary(&cfg);
        assert!(
            s.contains("  homemac: (no binding names it: no folder runs in it)"),
            "{s}"
        );
        assert!(s.contains("  infra:\n"), "{s}");
    }

    /// T44: keys are listed by name; `add-key` says where the key went, never what it is.
    #[test]
    fn ssh_keys_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("infra")).unwrap();
        let cfg = Config::parse(&format!(
            "hub_url = \"https://h\"\nallowed_roots = [{root:?}]\n[environments.infra]\n\
             ssh_keys = [\"infra_ed25519\"]\n[[env_bindings]]\npath = \"{}\"\nenvironment = \"infra\"\n",
            root.join("infra").display()
        ))
        .unwrap();
        let s = summary(&cfg);
        assert!(s.contains("ssh_keys: infra_ed25519"), "{s}");
        let w = which(&cfg, &root.join("infra")).unwrap();
        assert!(w.contains("ssh_keys: infra_ed25519"), "{w}");
        let d = which(&cfg, &root).unwrap();
        assert!(d.contains("ssh keys: none (no SSH_AUTH_SOCK)"), "{d}");

        let store = Store::in_home(&root.join("home"));
        std::fs::write(root.join("k"), "PRIVATE KEY MATERIAL").unwrap();
        let out = add_key(&store, "infra_ed25519", &root.join("k")).unwrap();
        assert!(out.contains("keys/infra_ed25519 (0600)"), "{out}");
        assert!(!out.contains("MATERIAL"), "{out}");
    }
}
