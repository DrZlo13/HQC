//! `hqc-worker env` (T43, SPEC §14): the environments and bindings of `worker.toml`, and
//! `env which <path>` with the environment and the effective rules of a folder. Values of
//! `env` are printed as `***`, ssh keys by name; editing stays in `worker.toml`. `env add-key
//! <name> <file>` puts a key into the store (T44).

use std::fmt::Write;
use std::path::Path;

use anyhow::Context;
use hqc_proto::frame::{rule_source, DEFAULT_ENVIRONMENT};

use crate::config::{Config, Environment, LocalMcpTransport, McpValue};
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

/// A value of a local MCP server as `hqc-worker env` shows it: never the value itself.
fn shown(v: &McpValue) -> String {
    match v {
        McpValue::Plain(_) => "***".into(),
        McpValue::Vault(r) => match &r.field {
            Some(f) => format!("<Vaultwarden {}, field {f}>", r.entry),
            None => format!("<Vaultwarden {}>", r.entry),
        },
    }
}

/// `[environments.<name>.mcp_servers.*]` (iteration 9), without values.
fn servers(out: &mut String, e: &Environment) {
    if e.mcp_servers.is_empty() {
        let _ = writeln!(out, "    mcp_servers: none");
        return;
    }
    let _ = writeln!(out, "    mcp_servers:");
    for s in &e.mcp_servers {
        let pin = if s.pin {
            "tools pinned"
        } else {
            "pin = false: tools not pinned"
        };
        match &s.transport {
            LocalMcpTransport::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                let mut line = format!("      {}: stdio {command}", s.name);
                for a in args {
                    line.push(' ');
                    line.push_str(a);
                }
                let _ = writeln!(out, "{line} ({pin})");
                if !env.is_empty() {
                    let vars: Vec<String> = env
                        .iter()
                        .map(|(k, v)| format!("{k}={}", shown(v)))
                        .collect();
                    let _ = writeln!(out, "        env: {}", vars.join(" "));
                }
                let _ = writeln!(
                    out,
                    "        runs in: {}",
                    cwd.as_ref()
                        .map_or("the session's working folder".into(), |c| {
                            c.display().to_string()
                        })
                );
            }
            LocalMcpTransport::Http { url, headers } => {
                let _ = writeln!(out, "      {}: http {url} ({pin})", s.name);
                if !headers.is_empty() {
                    let h: Vec<String> = headers
                        .iter()
                        .map(|(k, v)| format!("{k}: {}", shown(v)))
                        .collect();
                    let _ = writeln!(out, "        headers: {}", h.join(", "));
                }
            }
        }
    }
}

/// The warning of an environment with `allow_local_mcp` (iteration 9).
const ALLOW_LOCAL_MCP_WARNING: &str = "every MCP server this machine's Claude Code knows, the \
     account's claude.ai connectors (mail, drive, calendar...) included; name the servers in \
     [environments.<name>.mcp_servers] instead";

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
        if e.allow_local_mcp {
            let _ = writeln!(out, "    allow_local_mcp: true: {ALLOW_LOCAL_MCP_WARNING}");
        } else {
            let _ = writeln!(out, "    allow_local_mcp: false");
        }
        servers(&mut out, e);
        let _ = writeln!(out, "    project_settings: {}", e.project_settings);
        let _ = writeln!(out, "    hub_mcp: {}", e.hub_mcp);
        let _ = writeln!(out, "    hub_skills: {}", e.hub_skills);
        list(&mut out, "unblock", &e.rules.unblock);
        list(&mut out, "allow", &e.rules.allow);
        list(&mut out, "deny", &e.rules.deny);
        for (host, rules) in &e.rules.remote_unblock {
            list(&mut out, &format!("remote_unblock (ssh {host})"), rules);
        }
    }
    out.push_str("bindings:\n");
    if cfg.env_bindings.is_empty() {
        out.push_str("  none\n");
    }
    for b in &cfg.env_bindings {
        let _ = writeln!(out, "  {} -> {}", b.path, b.environment);
    }
    // SPEC 0.144: a session runs on one only when the hub describes it with this base_url.
    out.push_str("providers:\n");
    if cfg.providers.is_empty() {
        out.push_str("  none\n");
    }
    for p in &cfg.providers {
        let key = match (&p.api_key, &p.api_key_vault) {
            (_, Some(v)) => match &v.field {
                Some(f) => format!("api_key from Vaultwarden entry {}, field {f}", v.entry),
                None => format!("api_key from Vaultwarden entry {}", v.entry),
            },
            (Some(_), None) => "api_key ***".to_string(),
            (None, None) => "no api_key".to_string(),
        };
        let _ = writeln!(out, "  {}: {} ({key})", p.name, p.base_url);
    }
    // SPEC 0.147: secrets and keys from the chat, and the keys kept for workspaces.
    let _ = writeln!(
        out,
        "session_secrets: {}",
        if cfg.session_secrets { "on" } else { "off" }
    );
    let _ = writeln!(out, "vault: {}", if cfg.vault { "on" } else { "off" });
    let _ = writeln!(
        out,
        "browser: {}",
        if cfg.browser {
            "on (a session its owner gives it runs with --chrome)"
        } else {
            "off"
        }
    );
    let _ = writeln!(
        out,
        "task_tools: {}",
        if cfg.task_tools { "on" } else { "off" }
    );
    let _ = writeln!(
        out,
        "long_run_note: {}",
        match cfg.long_run_note {
            Some(every) => format!("{} s", every.as_secs()),
            None => "off".into(),
        }
    );
    out.push_str("workspace keys:\n");
    match cfg.secrets.all() {
        Ok(all) if all.is_empty() => out.push_str("  none\n"),
        Ok(all) => {
            for (path, keys) in all {
                let _ = writeln!(out, "  {path}:");
                for k in keys {
                    let _ = writeln!(out, "    {} {}", k.name, k.fingerprint);
                }
            }
        }
        Err(e) => {
            let _ = writeln!(out, "  unreadable: {e:#}");
        }
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
    let kept = cfg.secrets.workspace(&path).list().unwrap_or_default();
    if e.ssh_keys.is_empty() && kept.is_empty() && !cfg.session_secrets {
        out.push_str("    ssh keys: none (no SSH_AUTH_SOCK)\n");
    }
    for k in &kept {
        let _ = writeln!(
            out,
            "    kept for this workspace: {} {}",
            k.name, k.fingerprint
        );
    }
    if e.allow_local_mcp {
        let _ = writeln!(
            out,
            "    the machine's MCP servers: all (allow_local_mcp, no --strict-mcp-config): {ALLOW_LOCAL_MCP_WARNING}"
        );
    } else {
        let _ = writeln!(
            out,
            "    the machine's MCP servers: only those of mcp_servers (--strict-mcp-config)"
        );
    }
    servers(&mut out, &e);
    let _ = writeln!(
        out,
        "    project settings: {}",
        if e.project_settings {
            "read (--setting-sources user,project,local)"
        } else {
            "off (--setting-sources user,local)"
        }
    );
    let _ = writeln!(
        out,
        "    the hub's MCP servers: {}",
        if e.hub_mcp {
            "taken (hub_mcp), each as a relay to the hub"
        } else {
            "off"
        }
    );
    let _ = writeln!(
        out,
        "    the hub's skills: {}",
        if e.hub_skills {
            "taken (hub_skills), as the plugin hqc (--plugin-dir)"
        } else {
            "off"
        }
    );
    let p = &cfg.permissions;
    let _ = writeln!(
        out,
        "mode: {} by default (allow_auto: {}, allow_all: {})",
        p.mode.as_str(),
        p.allow_auto,
        p.allow_all
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
    fn providers_are_listed_without_keys() {
        let c = Config::parse(
            "hub_url = \"https://h\"\nallowed_roots = [\"/a\"]\n\
             [providers.zai]\nbase_url = \"https://api.z.ai/api/anthropic\"\napi_key = \"sk-zai-secret\"\n",
        )
        .unwrap();
        let out = summary(&c);
        assert!(
            out.contains("zai: https://api.z.ai/api/anthropic (api_key ***)"),
            "{out}"
        );
        assert!(!out.contains("sk-zai-secret"));
    }

    #[test]
    fn a_vault_key_is_listed_by_its_entry() {
        let c = Config::parse(
            "hub_url = \"https://h\"\nallowed_roots = [\"/a\"]\n\
             [providers.zai]\nbase_url = \"https://api.z.ai/api/anthropic\"\n\
             api_key = { vault = \"zai key\" }\n\
             [providers.kimi]\nbase_url = \"https://api.kimi.example\"\n\
             api_key = { vault = \"kimi\", field = \"token\" }\n",
        )
        .unwrap();
        let out = summary(&c);
        assert!(
            out.contains(
                "zai: https://api.z.ai/api/anthropic (api_key from Vaultwarden entry zai key)"
            ),
            "{out}"
        );
        assert!(
            out.contains(
                "kimi: https://api.kimi.example (api_key from Vaultwarden entry kimi, field token)"
            ),
            "{out}"
        );
    }

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
    fn local_mcp_servers_are_listed_without_values() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let c = Config::parse(&format!(
            "hub_url = \"https://h\"\nallowed_roots = [\"{r}\"]\n\
             [environments.firmware]\nallow_local_mcp = true\n\
             [environments.firmware.mcp_servers.figma]\ncommand = \"npx\"\n\
             args = [\"-y\", \"figma-developer-mcp\", \"--stdio\"]\n\
             env = {{ FIGMA_API_KEY = {{ vault = \"figma token\" }}, PLAIN = \"s3cret-value\" }}\n\
             [environments.firmware.mcp_servers.figma-dev]\ntype = \"http\"\n\
             url = \"http://127.0.0.1:3845/mcp\"\npin = false\n\
             headers = {{ Authorization = \"Bearer s3cret-header\" }}\n\
             [[env_bindings]]\npath = \"{r}\"\nenvironment = \"firmware\"\n",
            r = root.display()
        ))
        .unwrap();
        for out in [summary(&c), which(&c, &root).unwrap()] {
            assert!(
                out.contains("figma: stdio npx -y figma-developer-mcp --stdio (tools pinned)"),
                "{out}"
            );
            assert!(
                out.contains("env: FIGMA_API_KEY=<Vaultwarden figma token> PLAIN=***"),
                "{out}"
            );
            assert!(
                out.contains("runs in: the session's working folder"),
                "{out}"
            );
            assert!(
                out.contains(
                    "figma-dev: http http://127.0.0.1:3845/mcp (pin = false: tools not pinned)"
                ),
                "{out}"
            );
            assert!(out.contains("headers: Authorization: ***"), "{out}");
            assert!(out.contains("claude.ai connectors"), "{out}");
            assert!(!out.contains("s3cret"), "{out}");
        }
        assert!(summary(
            &Config::parse("hub_url = \"https://h\"\nallowed_roots = [\"/a\"]\n[environments.x]\n")
                .unwrap()
        )
        .contains("mcp_servers: none"));
    }

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
