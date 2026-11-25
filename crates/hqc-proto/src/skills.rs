//! Skills the owner keeps on the hub (SPEC 0.245, iteration 8): the rules the hub checks on
//! upload and the worker checks again before an agent gets a skill, since the worker does not
//! trust the hub for what runs outside its `deny` (SPEC §2.1).

use serde::{Deserialize, Serialize};

/// A skill unpacked: at most this many bytes.
pub const MAX_SKILL_BYTES: u64 = 4 * 1024 * 1024;
/// A skill: at most this many files.
pub const MAX_SKILL_FILES: usize = 500;
/// A path inside a skill: at most this many bytes.
pub const MAX_SKILL_PATH: usize = 200;
/// A skill's name: at most this many bytes.
pub const MAX_SKILL_NAME: usize = 64;
/// The file every skill has at its top.
pub const SKILL_MD: &str = "SKILL.md";

/// A skill of `session.start.skills` (SPEC §5.3): its name and the sha256 of its archive,
/// which the worker fetches once per hash (`GET /worker/skills/{sha256}`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SkillRef {
    pub name: String,
    pub sha256: String,
}

/// A skill's name is `[a-z0-9][a-z0-9_-]{0,63}`: the folder the CLI names it by (`hqc:<name>`),
/// so no `:`, `.` nor `/`.
pub fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SKILL_NAME
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A sha256 as hex, lower case.
pub fn valid_sha256(h: &str) -> bool {
    h.len() == 64
        && h.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A path inside a skill: relative, `/`-separated, each part a plain name (not empty, `.` nor
/// `..`), no `\`, no control character, at most [`MAX_SKILL_PATH`] bytes.
pub fn check_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > MAX_SKILL_PATH {
        return Err(format!("{path:?}: a path is 1 to {MAX_SKILL_PATH} bytes"));
    }
    if path.contains('\\') || path.chars().any(char::is_control) {
        return Err(format!(
            "{path:?}: no '\\' nor control characters in a path"
        ));
    }
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(format!("{path:?}: a path stays inside the skill"));
        }
    }
    Ok(())
}

/// What a `SKILL.md` says of itself, for the list in Settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    /// `allowed-tools`: what the agent runs without a card while the skill is active (the
    /// worker's `deny` still applies).
    pub allowed_tools: Vec<String>,
}

/// Checks a `SKILL.md` (any file of that name in a skill) and reads its frontmatter.
///
/// Refused: a `hooks` key, which would run commands outside the worker's `deny`; and, so that
/// no YAML form hides one, a frontmatter whose top-level lines are not plain `key:` (quotes,
/// `{`, `?`, `<<` merges), one not closed, and any line of the file at column 0 that is a
/// `hooks:` key, whatever a parser takes for the frontmatter.
pub fn check_skill_md(text: &str) -> Result<Frontmatter, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    for l in text.split(['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}']) {
        let k = l.trim_start_matches(['"', '\'']).to_ascii_lowercase();
        if let Some(rest) = k.strip_prefix("hooks") {
            if rest
                .trim_start_matches(['"', '\''])
                .trim_start()
                .starts_with(':')
            {
                return Err(
                    "a skill with hooks is not taken: hooks run outside the worker's deny".into(),
                );
            }
        }
    }
    let mut fm = Frontmatter::default();
    // The frontmatter as Claude Code finds it (2.1.328): one BOM stripped, then
    // `^---\s*\n([\s\S]*?)---` with JavaScript's `\s`, so it ends at the first `---` anywhere.
    let Some(after) = text.strip_prefix("---") else {
        return Ok(fm);
    };
    let ws = after.len() - after.trim_start_matches(js_space).len();
    let Some(nl) = after[..ws].rfind('\n') else {
        // No line break before the first non-space: the CLI sees no frontmatter, but a file
        // that looks like one is refused rather than guessed at.
        return Err("the first line of SKILL.md is --- with something after it".into());
    };
    if after[..nl]
        .chars()
        .any(|c| !matches!(c, ' ' | '\t' | '\r' | '\n'))
    {
        return Err("the first line of SKILL.md is --- with something after it".into());
    }
    let body = &after[nl + 1..];
    let Some(end) = body.find("---") else {
        return Err("the frontmatter of SKILL.md is not closed with ---".into());
    };
    let region = &body[..end];
    // Only `\n` (with an optional `\r` before it) breaks a line, and no other control or
    // space character than a tab, so that every YAML parser sees the lines checked here.
    let mut chars = region.chars().peekable();
    while let Some(c) = chars.next() {
        let ok = match c {
            '\n' | '\t' | ' ' => true,
            '\r' => chars.peek() == Some(&'\n'),
            c => {
                !c.is_control() && !js_space(c) && !matches!(c, '\u{85}' | '\u{200b}'..='\u{200f}')
            }
        };
        if !ok {
            return Err(format!(
                "the frontmatter of SKILL.md has the character U+{:04X}, which is not taken there",
                c as u32
            ));
        }
    }
    let lines: Vec<&str> = region
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .collect();
    // (key, value on its line, continuation lines)
    let mut entries: Vec<(String, String, Vec<String>)> = vec![];
    for (i, l) in lines.iter().enumerate() {
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        // A tab at the start is an error to YAML, which a lax parser might read otherwise.
        if l.starts_with('\t') {
            return Err(format!(
                "frontmatter line {} starts with a tab: indent with spaces",
                i + 2
            ));
        }
        if l.starts_with([' ', '-']) {
            match entries.last_mut() {
                Some(e) => e.2.push(l.trim().to_string()),
                None => return Err(format!("frontmatter line {}: no key before it", i + 2)),
            }
            continue;
        }
        let Some((key, value)) = l.split_once(':') else {
            return Err(format!(
                "frontmatter line {} is not a plain key: value",
                i + 2
            ));
        };
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || !(value.is_empty() || value.starts_with([' ', '\t']))
        {
            return Err(format!(
                "frontmatter line {} is not a plain key: value",
                i + 2
            ));
        }
        if key == "hooks" {
            return Err(
                "a skill with hooks is not taken: hooks run outside the worker's deny".into(),
            );
        }
        entries.push((key.to_string(), value.trim().to_string(), vec![]));
    }
    let unquote = |v: &str| -> String {
        let v = v.trim();
        for q in ['"', '\''] {
            if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
                return v[1..v.len() - 1].to_string();
            }
        }
        v.to_string()
    };
    for (key, value, more) in entries {
        match key.as_str() {
            "name" => fm.name = Some(unquote(&value)).filter(|v| !v.is_empty()),
            "description" => {
                let v = if value.is_empty() || value.starts_with(['>', '|']) {
                    more.join(" ")
                } else {
                    std::iter::once(unquote(&value))
                        .chain(more)
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                fm.description = Some(v.trim().to_string()).filter(|v| !v.is_empty());
            }
            "allowed-tools" | "allowed_tools" => {
                let mut items: Vec<String> = vec![];
                let flat = value.trim_start_matches('[').trim_end_matches(']');
                items.extend(split_tools(flat));
                for m in more {
                    items.extend(split_tools(m.trim_start_matches('-')));
                }
                fm.allowed_tools = items;
            }
            _ => {}
        }
    }
    Ok(fm)
}

/// JavaScript's `\s`: what the CLI's frontmatter pattern takes for a space.
fn js_space(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `Read, Bash(git add:*)` → its entries, commas inside parentheses kept.
fn split_tools(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if c == ',' && depth <= 0 {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out.into_iter()
        .map(|t| t.trim().trim_matches(['"', '\'']).trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_hashes() {
        for ok in ["probe-bench", "a", "x_1", "0day"] {
            assert!(valid_skill_name(ok), "{ok}");
        }
        for bad in ["", "-a", "_a", "A", "a.b", "a:b", "a/b", &"a".repeat(65)] {
            assert!(!valid_skill_name(bad), "{bad}");
        }
        assert!(valid_sha256(&"ab".repeat(32)));
        assert!(!valid_sha256(&"AB".repeat(32)));
        assert!(!valid_sha256("ab"));
    }

    #[test]
    fn paths_stay_inside() {
        for ok in [
            "SKILL.md",
            "scripts/run.sh",
            "references/a b.md",
            ".env.example",
        ] {
            assert!(check_path(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "a/./b",
            "a\\b",
            "a\nb",
        ] {
            assert!(check_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_frontmatter_is_read() {
        let fm = check_skill_md(
            "\u{feff}---\r\nname: probe-bench\r\ndescription: >\r\n  Runs the bench.\r\n  Twice.\r\n\
             allowed-tools: Read, Bash(git add:*, x), \"Grep\"\r\nmodel: haiku\r\n---\r\n# Body\r\n",
        )
        .unwrap();
        assert_eq!(fm.name.as_deref(), Some("probe-bench"));
        assert_eq!(fm.description.as_deref(), Some("Runs the bench. Twice."));
        assert_eq!(fm.allowed_tools, ["Read", "Bash(git add:*, x)", "Grep"]);
        let fm =
            check_skill_md("---\nname: 'x'\nallowed-tools:\n  - Read\n- Write\n---\n").unwrap();
        assert_eq!(fm.name.as_deref(), Some("x"));
        assert_eq!(fm.allowed_tools, ["Read", "Write"]);
        assert_eq!(
            check_skill_md("no frontmatter").unwrap(),
            Frontmatter::default()
        );
    }

    #[test]
    fn hooks_and_yaml_tricks_are_refused() {
        for bad in [
            "---\nname: x\nhooks:\n  PreToolUse: []\n---\n",
            "---\nname: x\n\"hooks\": {}\n---\n",
            "---\nname: x\n'hooks' : {}\n---\n",
            "---\n{name: x, hooks: {}}\n---\n",
            "---\nname: x\n<<: *a\n---\n",
            "---\n? hooks\n: {}\n---\n",
            "---\nname: x\nHOOKS:\n---\n",
            "---\nname: x\n--- \nhooks: {}\n",
            "---\nname: x\n",
            "---\nname:x\n---\n",
            // A lone CR is a line break to YAML (found in review: the hook ran on 2.1.328).
            "---\nname: cr\ndescription: d\rhooks: {Stop: []}\n---\nSay ok.\n",
            // `---` and U+FEFF: JavaScript's `\s` takes it, the CLI parses the frontmatter.
            "---\u{feff}\nname: x\n\"hooks\": {Stop: []}\n---\n",
            "---\u{a0}\nname: x\n\"hooks\": {}\n---\n",
            "---\nname: x\ndescription: d\u{2028}hooks: {}\n---\n",
            "---\nname: x\ndescription: d\u{85}hooks: {}\n---\n",
            "---\nname: x\ndescription: d\u{b}x\n---\n",
            "--- x\nname: x\n---\n",
            "---\nname: x\n...\n\"hooks\": {}\n---\n",
            "---\nname: x\n%YAML 1.2\n---\n",
            "---\nname: x\n!!str hooks: {}\n---\n",
            "---\nname: x\n\thooks: {Stop: []}\n---\n",
            "---\nname: x\ndescription: >\n\tlong\n---\n",
        ] {
            assert!(check_skill_md(bad).is_err(), "{bad:?}");
        }
        // Blank lines after the opener, CRLF, the frontmatter ending at the first `---` as the
        // CLI's does, and odd characters in the body are fine.
        let fm =
            check_skill_md("---\n\nname: x\r\ndescription: a---\nweird\u{2028}body\n").unwrap();
        assert_eq!(fm.description.as_deref(), Some("a"));
        // The word in a description or the body is fine.
        assert!(
            check_skill_md("---\ndescription: git hooks: set them up\n---\nUse hooks.\n").is_ok()
        );
    }
}
