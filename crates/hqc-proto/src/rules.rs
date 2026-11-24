//! hqc's own allow rules of a workspace (T48, SPEC §3 Workspace, §14): standing answers of
//! the worker owner that the worker applies in `can_use_tool`, after its `deny`.
//!
//! The syntax is a strict subset of Claude Code's, so the worker's matcher reads every rule
//! exactly and conservatively:
//! - `Tool` — a whole tool (`Edit`, `WebFetch`), or a whole MCP server (`mcp__srv`) or one of
//!   its tools (`mcp__srv__tool`); never a tool that runs commands ([`EXEC_TOOLS`]);
//! - `Bash(<words>)` — exactly that command;
//! - `Bash(<words> *)` or `Bash(<words>:*)` — the command, with any arguments after it.
//!
//! `<words>` are words of letters, digits and `-_./=+,@%:~` separated by single spaces: no
//! quotes, no shell operators, no `*` but the final one.

/// Most hqc rules one workspace has (SPEC §20).
pub const MAX_RULES: usize = 200;
/// Longest rule, in bytes (SPEC §20, as for `worker.toml`).
pub const MAX_RULE_LEN: usize = 256;

/// Tools that run commands or code: never allowed as a whole tool by an hqc rule.
pub const EXEC_TOOLS: &[&str] = &["Bash", "PowerShell", "BashOutput", "KillShell", "KillBash"];

/// A parsed hqc rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowRule {
    /// A whole tool, or a whole MCP server (`mcp__srv`).
    Tool(String),
    /// `Bash(<words>)`.
    BashExact(Vec<String>),
    /// `Bash(<words> *)`, `Bash(<words>:*)`.
    BashPrefix(Vec<String>),
}

/// The server of an MCP tool `mcp__<server>__<tool>`: the part between `mcp__` and the next
/// `__`.
pub fn mcp_server(tool: &str) -> Option<&str> {
    tool.strip_prefix("mcp__")?
        .split_once("__")
        .map(|(server, _)| server)
        .filter(|s| !s.is_empty())
}

/// [`mcp_server`] when no other server's tool can have the same name, the server whose all
/// tools an answer of the owner may allow. Claude Code splits a tool's name at the first `__`
/// as [`mcp_server`] does, so `mcp__hqc___x` is the tool `_x` of `hqc` or `x` of `hqc_`, and
/// `mcp__a__b__x` the tool `b__x` of `a` or `x` of `a__b`; a rule `mcp__hqc` or `mcp__a` made
/// for one would cover the other. None when the tool part starts with `_` or has `__`, and, as
/// a precaution, when the server part starts or ends with `_` (with the split at the first `__`
/// it never ends with `_`, and a leading `_` reads only one way). A rule `mcp__hqc` made for
/// the real `hqc` still covers `mcp__hqc___x`, as Claude Code's own rule does: [`mcp_server`]
/// is how a rule matches.
pub fn unambiguous_mcp_server(tool: &str) -> Option<&str> {
    let server = mcp_server(tool)?;
    let rest = &tool["mcp__".len() + server.len() + "__".len()..];
    (!server.starts_with('_')
        && !server.ends_with('_')
        && !rest.starts_with('_')
        && !rest.contains("__"))
    .then_some(server)
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || "-_./=+,@%:~".contains(c)
}

/// Parse an hqc rule; the error is a sentence for the user.
pub fn parse_allow_rule(rule: &str) -> Result<AllowRule, String> {
    if rule.is_empty() {
        return Err("the rule is empty".into());
    }
    if rule.len() > MAX_RULE_LEN {
        return Err(format!("the rule is longer than {MAX_RULE_LEN} bytes"));
    }
    if rule != rule.trim() || rule.chars().any(char::is_control) {
        return Err("the rule has spaces around it or control characters".into());
    }
    let Some((name, rest)) = rule.split_once('(') else {
        if !rule.starts_with(|c: char| c.is_ascii_alphabetic())
            || !rule
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(
                "not a valid rule: use Bash(command *), a tool name like Edit, or mcp__server"
                    .into(),
            );
        }
        if EXEC_TOOLS.contains(&rule) {
            return Err(format!(
                "{rule} as a whole would allow every command: use {rule}(command *)"
            ));
        }
        return Ok(AllowRule::Tool(rule.into()));
    };
    if name != "Bash" {
        return Err("only Bash takes a pattern in an hqc rule: use the tool name alone".into());
    }
    let Some(content) = rest.strip_suffix(')') else {
        return Err("no closing parenthesis at the end".into());
    };
    let (body, prefix) = if let Some(b) = content.strip_suffix(":*") {
        (b, true)
    } else if let Some(b) = content.strip_suffix(" *") {
        (b, true)
    } else {
        (content, false)
    };
    let words: Vec<&str> = body.split(' ').collect();
    if body.is_empty() || words.iter().any(|w| w.is_empty()) {
        return Err("the command must be words separated by single spaces".into());
    }
    if let Some(w) = words.iter().find(|w| !w.chars().all(word_char)) {
        return Err(format!(
            "{w:?}: a rule cannot have quotes, shell operators or a * other than the final one"
        ));
    }
    let words = words.into_iter().map(String::from).collect();
    Ok(if prefix {
        AllowRule::BashPrefix(words)
    } else {
        AllowRule::BashExact(words)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax() {
        let w = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(
            parse_allow_rule("Bash(python3 *)"),
            Ok(AllowRule::BashPrefix(w("python3")))
        );
        assert_eq!(
            parse_allow_rule("Bash(git fetch:*)"),
            Ok(AllowRule::BashPrefix(w("git fetch")))
        );
        assert_eq!(
            parse_allow_rule("Bash(npm test)"),
            Ok(AllowRule::BashExact(w("npm test")))
        );
        assert_eq!(parse_allow_rule("Edit"), Ok(AllowRule::Tool("Edit".into())));
        assert_eq!(
            parse_allow_rule("mcp__hqc"),
            Ok(AllowRule::Tool("mcp__hqc".into()))
        );
        // Claude Code's server names are letters, digits, `_` and `-`.
        assert_eq!(
            parse_allow_rule("mcp__claude-in-chrome"),
            Ok(AllowRule::Tool("mcp__claude-in-chrome".into()))
        );
        assert!(parse_allow_rule("Bash(./tools/conv.py --out=x *)").is_ok());
        for bad in [
            "",
            "Bash",
            "PowerShell",
            "Bash()",
            "Bash(*)",
            "Bash( *)",
            "Bash(a  b)",
            "Bash(a; b *)",
            "Bash(a && b)",
            "Bash(a | b)",
            "Bash(echo $(id))",
            "Bash(echo `id`)",
            "Bash(rm -rf * x)",
            "Bash(\"a\" *)",
            "Bash(a > f)",
            "Bash(python3 *",
            "Read(./src/**)",
            "WebFetch(domain:x)",
            " Edit",
            "1Tool",
            "mcp__srv.x",
        ] {
            assert!(parse_allow_rule(bad).is_err(), "{bad:?}");
        }
        assert!(parse_allow_rule(&format!("Bash({} *)", "a".repeat(260))).is_err());
    }

    #[test]
    fn a_server_is_whole_only_when_its_name_is_not_ambiguous() {
        for (tool, server) in [
            ("mcp__hqc__hqc_send", Some("hqc")),
            ("mcp__claude-in-chrome__navigate", Some("claude-in-chrome")),
            ("mcp__my_srv__get_x", Some("my_srv")),
            ("mcp__hqc___x", None),
            ("mcp___a__x", None),
            ("mcp__a__b__x", None),
            ("mcp__a__x_", Some("a")),
            ("Bash", None),
            ("mcp__srv", None),
        ] {
            assert_eq!(unambiguous_mcp_server(tool), server, "{tool}");
        }
        // The server of a rule is still read as Claude Code reads it.
        assert_eq!(mcp_server("mcp__hqc___x"), Some("hqc"));
    }
}
