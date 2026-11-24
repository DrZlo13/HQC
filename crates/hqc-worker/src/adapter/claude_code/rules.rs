//! `session` approvals (SPEC §8.1, §10.1): rules the adapter keeps until the process ends.
//!
//! A rule is remembered from the request the owner answered with `session`: the exact
//! `(tool_name, input)` pair always, plus whatever the CLI suggested in
//! `permission_suggestions[].rules` that the adapter can match safely:
//! - `{toolName}` without `ruleContent` — the whole tool, except for tools that run commands
//!   (`Bash`, `PowerShell`, ...): for them such a suggestion is ignored;
//! - Bash `ruleContent` `"<prefix> *"` or `"<prefix>:*"` — commands equal to the prefix or
//!   starting with `"<prefix> "`, and only commands without shell operators, so
//!   `touch x; rm -rf ~` does not ride on a `touch x *` rule;
//! - Bash `ruleContent` without `*` — that exact command.
//!
//! The answer "all tools of this MCP server" adds a rule for every `mcp__<server>__*` tool.
//!
//! Other `ruleContent` forms (path globs of file tools, `domain:` of WebFetch, ...) are not
//! interpreted here; for them only the exact input matches.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
enum Rule {
    Exact {
        tool: String,
        input: Value,
    },
    Tool(String),
    /// Every tool of an MCP server: `mcp__<server>__*`.
    McpServer(String),
    BashPrefix(String),
    BashExact(String),
}

#[derive(Debug, Default)]
pub struct SessionRules(Vec<Rule>);

/// Tools that run commands or code: never approved as a whole tool.
const EXEC_TOOLS: &[&str] = &["Bash", "PowerShell", "BashOutput", "KillShell", "KillBash"];

/// Characters that chain, substitute or redirect in a shell command.
const SHELL_META: &[char] = &[';', '&', '|', '`', '$', '(', ')', '<', '>', '\n', '\r'];

impl SessionRules {
    /// Remember `tool` + `input` and the matchable rules among `suggested` (entries of
    /// `permission_suggestions[].rules` with `toolName` == `tool`).
    pub fn add(&mut self, tool: &str, input: &Value, suggested: &[Value]) {
        self.push(Rule::Exact {
            tool: tool.into(),
            input: input.clone(),
        });
        for s in suggested {
            if let Some(r) = taken(tool, s) {
                self.push(r);
            }
        }
    }

    /// Remember every tool of the MCP server `server`.
    pub fn add_server(&mut self, server: &str) {
        self.push(Rule::McpServer(server.into()));
    }

    pub fn matches(&self, tool: &str, input: &Value) -> bool {
        let command = input["command"].as_str();
        self.0.iter().any(|r| match r {
            Rule::Exact { tool: t, input: i } => t == tool && i == input,
            Rule::Tool(t) => t == tool,
            Rule::McpServer(srv) => hqc_proto::rules::mcp_server(tool) == Some(srv.as_str()),
            Rule::BashPrefix(p) => {
                tool == "Bash"
                    && command.is_some_and(|c| {
                        !c.contains(SHELL_META)
                            && (c == p
                                || c.strip_prefix(p.as_str())
                                    .is_some_and(|r| r.starts_with(' ')))
                    })
            }
            Rule::BashExact(e) => tool == "Bash" && command == Some(e.as_str()),
        })
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn push(&mut self, r: Rule) {
        if !self.0.contains(&r) {
            self.0.push(r);
        }
    }
}

/// The rule `add` takes from one suggestion for `tool`, if it takes any.
fn taken(tool: &str, suggestion: &Value) -> Option<Rule> {
    if suggestion["toolName"].as_str() != Some(tool) {
        return None;
    }
    match suggestion.get("ruleContent").and_then(Value::as_str) {
        // A whole command-running tool for the rest of the session would approve
        // every command; only the exact input is remembered then.
        None if EXEC_TOOLS.contains(&tool) => None,
        None => Some(Rule::Tool(tool.into())),
        Some(c) if tool == "Bash" => bash_rule(c),
        Some(_) => None,
    }
}

/// The rules `add` takes from `suggested`, as the CLI writes a rule: `toolName` or
/// `toolName(ruleContent)`. Not the exact input, which `add` always remembers.
pub fn shown(tool: &str, suggested: &[Value]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for rule in suggested.iter().filter_map(|s| taken(tool, s)) {
        let text = match rule {
            // `taken` never makes a server rule; it comes only from the server answers.
            Rule::Exact { .. } | Rule::McpServer(_) => continue,
            Rule::Tool(t) => t,
            Rule::BashPrefix(p) => format!("Bash({p} *)"),
            Rule::BashExact(e) => format!("Bash({e})"),
        };
        if !out.contains(&text) {
            out.push(text);
        }
    }
    out
}

/// A rule of a suggestion, `{toolName, ruleContent?}`, as the CLI writes it.
pub fn rule_text(rule: &Value) -> Option<String> {
    let tool = rule["toolName"].as_str()?;
    Some(match rule.get("ruleContent").and_then(Value::as_str) {
        Some(c) => format!("{tool}({c})"),
        None => tool.to_owned(),
    })
}

fn bash_rule(content: &str) -> Option<Rule> {
    let prefix = content
        .strip_suffix(":*")
        .or_else(|| content.strip_suffix(" *"))
        .map(str::trim_end);
    match prefix {
        Some(p) if !p.is_empty() && !p.contains(SHELL_META) && !p.contains('*') => {
            Some(Rule::BashPrefix(p.into()))
        }
        Some(_) => None,
        None if !content.contains('*') && !content.is_empty() => {
            Some(Rule::BashExact(content.into()))
        }
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bash(c: &str) -> Value {
        json!({"command": c, "description": "x"})
    }

    #[test]
    fn bash_prefix_rule() {
        let mut r = SessionRules::default();
        r.add(
            "Bash",
            &bash("touch hqc-marker"),
            &[json!({"toolName": "Bash", "ruleContent": "touch hqc-marker *"})],
        );
        assert!(r.matches("Bash", &bash("touch hqc-marker")));
        assert!(r.matches("Bash", &bash("touch hqc-marker -c")));
        assert!(!r.matches("Bash", &bash("touch hqc-marker2")));
        assert!(!r.matches("Bash", &bash("touch hqc-marker; rm -rf /")));
        assert!(!r.matches("Bash", &bash("touch hqc-marker && curl x")));
        assert!(!r.matches("Bash", &bash("touch hqc-marker $(id)")));
        assert!(!r.matches("Bash", &bash("rm hqc-marker")));
        assert!(!r.matches("Write", &bash("touch hqc-marker")));
    }

    #[test]
    fn exact_and_tool_rules() {
        let mut r = SessionRules::default();
        let input = json!({"file_path": "/w/a.txt", "content": "x"});
        r.add(
            "Write",
            &input,
            &[json!({"toolName": "Write", "ruleContent": "/w/**"})],
        );
        assert!(r.matches("Write", &input));
        assert!(!r.matches("Write", &json!({"file_path": "/w/b.txt", "content": "x"})));

        r.add(
            "WebSearch",
            &json!({"query": "a"}),
            &[json!({"toolName": "WebSearch"})],
        );
        assert!(r.matches("WebSearch", &json!({"query": "b"})));
        // A suggestion for another tool is not taken.
        r.add(
            "Read",
            &json!({"file_path": "/a"}),
            &[json!({"toolName": "Bash"})],
        );
        assert!(!r.matches("Bash", &bash("ls")));
    }

    #[test]
    fn no_whole_tool_rule_for_command_tools() {
        let mut r = SessionRules::default();
        r.add("Bash", &bash("ls"), &[json!({"toolName": "Bash"})]);
        assert!(r.matches("Bash", &bash("ls")), "the exact input still");
        assert!(!r.matches("Bash", &bash("rm -rf ~")));
        let ps = json!({"command": "dir"});
        r.add("PowerShell", &ps, &[json!({"toolName": "PowerShell"})]);
        assert!(!r.matches("PowerShell", &json!({"command": "del x"})));
    }

    #[test]
    fn shown_lists_what_add_takes() {
        let bash_forms = [
            json!({"toolName": "Bash", "ruleContent": "git status:*"}),
            json!({"toolName": "Bash", "ruleContent": "touch hqc-marker *"}),
            json!({"toolName": "Bash", "ruleContent": "npm test"}),
            json!({"toolName": "Bash", "ruleContent": "*"}),
            json!({"toolName": "Bash", "ruleContent": "a * b"}),
            json!({"toolName": "Bash", "ruleContent": "a | b *"}),
            json!({"toolName": "Bash"}),
            json!({"toolName": "Read", "ruleContent": "/w/**"}),
        ];
        assert_eq!(
            shown("Bash", &bash_forms),
            [
                "Bash(git status *)",
                "Bash(touch hqc-marker *)",
                "Bash(npm test)"
            ]
        );
        // What is shown is what a session rule then allows, besides the exact call.
        let mut r = SessionRules::default();
        r.add("Bash", &bash("ls"), &bash_forms);
        assert_eq!(r.len(), 1 + 3);

        let file_forms = [
            json!({"toolName": "Write", "ruleContent": "/w/**"}),
            json!({"toolName": "Bash", "ruleContent": "ls *"}),
        ];
        assert!(shown("Write", &file_forms).is_empty());
        assert_eq!(
            shown("WebSearch", &[json!({"toolName": "WebSearch"})]),
            ["WebSearch"]
        );
        assert!(shown("PowerShell", &[json!({"toolName": "PowerShell"})]).is_empty());
        let mut r = SessionRules::default();
        r.add("Write", &json!({}), &file_forms);
        assert_eq!(r.len(), 1);
        r.add("WebSearch", &json!({}), &[json!({"toolName": "WebSearch"})]);
        assert_eq!(r.len(), 1 + 2);
    }

    #[test]
    fn rule_text_forms() {
        assert_eq!(
            rule_text(&json!({"toolName": "Bash", "ruleContent": "npm test:*"})).as_deref(),
            Some("Bash(npm test:*)")
        );
        assert_eq!(
            rule_text(&json!({"toolName": "WebSearch"})).as_deref(),
            Some("WebSearch")
        );
        assert_eq!(rule_text(&json!({"ruleContent": "x"})), None);
    }

    #[test]
    fn bash_rule_forms() {
        assert_eq!(
            bash_rule("git status:*"),
            Some(Rule::BashPrefix("git status".into()))
        );
        assert_eq!(
            bash_rule("npm test"),
            Some(Rule::BashExact("npm test".into()))
        );
        assert_eq!(bash_rule("*"), None);
        assert_eq!(bash_rule("a * b"), None);
        assert_eq!(bash_rule("a | b *"), None);
    }
}
