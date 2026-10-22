//! `session` approvals (SPEC §8.1, §10.1): rules the adapter keeps until the process ends.
//!
//! A rule is remembered from the request the owner answered with `session`: the exact
//! `(tool_name, input)` pair always, plus whatever the CLI suggested in
//! `permission_suggestions[].rules` that the adapter can match safely:
//! - `{toolName}` without `ruleContent` — the whole tool;
//! - Bash `ruleContent` `"<prefix> *"` or `"<prefix>:*"` — commands equal to the prefix or
//!   starting with `"<prefix> "`, and only commands without shell operators, so
//!   `touch x; rm -rf ~` does not ride on a `touch x *` rule;
//! - Bash `ruleContent` without `*` — that exact command.
//!
//! Other `ruleContent` forms (path globs of file tools, `domain:` of WebFetch, ...) are not
//! interpreted here; for them only the exact input matches.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
enum Rule {
    Exact { tool: String, input: Value },
    Tool(String),
    BashPrefix(String),
    BashExact(String),
}

#[derive(Debug, Default)]
pub struct SessionRules(Vec<Rule>);

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
            if s["toolName"].as_str() != Some(tool) {
                continue;
            }
            let rule = match s.get("ruleContent").and_then(Value::as_str) {
                None => Some(Rule::Tool(tool.into())),
                Some(c) if tool == "Bash" => bash_rule(c),
                Some(_) => None,
            };
            if let Some(r) = rule {
                self.push(r);
            }
        }
    }

    pub fn matches(&self, tool: &str, input: &Value) -> bool {
        let command = input["command"].as_str();
        self.0.iter().any(|r| match r {
            Rule::Exact { tool: t, input: i } => t == tool && i == input,
            Rule::Tool(t) => t == tool,
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
