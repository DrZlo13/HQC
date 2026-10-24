//! Session titles the hub derives itself (T21, SPEC §3 Session).
//!
//! After the first final `agent.message` of a session whose title is still the default
//! «Сессия N» (from a worker or from the mirror: an imported session the same way, by its
//! first records), the hub asks the title model ([`crate::llm`], `HQC_TITLE_MODEL`) in the
//! background for a title of at most [`MAX_TITLE`] characters in the user's language, from
//! the first prompt and the first answer (each cut to [`MAX_INPUT`] characters). Without a
//! model, or when the call fails or times out (15 s; only logged), the title is the first
//! line of the first prompt without markdown, cut at a word to [`MAX_TITLE`] characters with
//! «…». Either way `title_source` becomes `runtime`, and only a `default` title is replaced:
//! a user's or runtime's title is never overwritten.

use uuid::Uuid;

use crate::{app::AppState, db};

/// Longest derived title, in characters.
pub const MAX_TITLE: usize = 60;
/// How much of the first prompt and the first answer the model sees, in characters.
pub const MAX_INPUT: usize = 2000;

const SYSTEM: &str = "You name chat sessions between a user and a coding agent. Reply with the \
    title only: at most 60 characters, in the language of the user's message, no quotes, no \
    markdown, no trailing period.";

/// The first final answer of `session_id` was stored: derive its title in the background if
/// it still has the default one. Once per session per hub process.
pub fn after_first_answer(app: &AppState, session_id: Uuid, title_source: &str, system: bool) {
    if title_source != "default" || system {
        return;
    }
    if !app.titling.lock().unwrap().insert(session_id) {
        return;
    }
    let app = app.clone();
    tokio::spawn(async move {
        if let Err(e) = derive(&app, session_id).await {
            tracing::warn!(%session_id, "session title: {e:#}");
        }
    });
}

async fn derive(app: &AppState, session_id: Uuid) -> anyhow::Result<()> {
    let Some(prompt) = db::first_text(&app.db, session_id, "user.message").await? else {
        return Ok(());
    };
    let answer = db::first_text(&app.db, session_id, "agent.message")
        .await?
        .unwrap_or_default();
    let mut title = None;
    if let Some(llm) = &app.titles {
        let user = format!(
            "First message of the user:\n{}\n\nFirst answer of the agent:\n{}",
            cut(&prompt, MAX_INPUT),
            cut(&answer, MAX_INPUT)
        );
        match llm.complete(SYSTEM, &user, 64).await {
            Ok(t) => title = from_model(&t),
            Err(e) => {
                tracing::warn!(%session_id, model = %llm.config().model, "title model failed, title from the prompt: {e:#}")
            }
        }
    }
    let Some(title) = title.or_else(|| from_prompt(&prompt)) else {
        return Ok(());
    };
    let title = hqc_proto::replace_nul(&title).into_owned();
    if db::set_generated_title(&app.db, session_id, &title).await? {
        tracing::info!(%session_id, %title, "session titled");
        app.session_changed(session_id);
    }
    Ok(())
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// The model's answer as a title: its first non-empty line without quotes and markdown.
pub fn from_model(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line
        .strip_prefix("Title:")
        .or_else(|| line.strip_prefix("Заголовок:"))
        .unwrap_or(line);
    let line = plain(line);
    let line = line
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '«' | '»' | '“' | '”' | '`'))
        .trim()
        .trim_end_matches('.')
        .trim();
    shorten(line, MAX_TITLE)
}

/// The fallback: the first non-empty line of the prompt without markdown, cut at a word.
pub fn from_prompt(prompt: &str) -> Option<String> {
    let line = prompt
        .lines()
        .map(plain)
        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with("```"))?;
    shorten(line.trim(), MAX_TITLE)
}

/// Markdown of one line removed: heading, quote and list marks, emphasis, inline code marks,
/// links (their text stays); whitespace collapsed.
fn plain(line: &str) -> String {
    let mut s = line.trim_start();
    loop {
        let t = s
            .trim_start_matches('#')
            .trim_start_matches('>')
            .trim_start();
        let t = ["- ", "* ", "+ ", "• "]
            .iter()
            .find_map(|m| t.strip_prefix(m))
            .unwrap_or(t);
        let t = match t.split_once(". ") {
            Some((n, rest))
                if !n.is_empty() && n.len() <= 3 && n.bytes().all(|b| b.is_ascii_digit()) =>
            {
                rest
            }
            _ => t,
        };
        if t.len() == s.len() {
            break;
        }
        s = t;
    }
    // `[text](url)` -> `text`.
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find("](") {
            Some(j) if !after[..j].contains('[') => match after[j + 2..].find(')') {
                Some(k) => {
                    out.push_str(&after[..j]);
                    rest = &after[j + 2 + k + 1..];
                }
                None => {
                    out.push('[');
                    rest = after;
                }
            },
            _ => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    let out = out.replace("**", "").replace("__", "").replace('`', "");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// At most `max` characters: cut at the last space before the limit (or hard, for one long
/// word) and end with «…».
pub fn shorten(s: &str, max: usize) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s.chars().count() <= max {
        return Some(s.to_string());
    }
    let head: String = s.chars().take(max - 1).collect();
    let cut = match head.rfind(' ') {
        Some(i) if i > 0 => head[..i].trim_end().to_string(),
        _ => head,
    };
    let cut = cut
        .trim_end_matches([',', ';', ':', '.', '-', '—'])
        .trim_end();
    Some(format!("{cut}…"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_titles() {
        assert_eq!(
            from_prompt("Fix the build").as_deref(),
            Some("Fix the build")
        );
        assert_eq!(
            from_prompt("\n\n## **Почини** сборку `cargo`\nдальше текст").as_deref(),
            Some("Почини сборку cargo")
        );
        assert_eq!(
            from_prompt("- see [the docs](https://x.y/z) please").as_deref(),
            Some("see the docs please")
        );
        assert_eq!(from_prompt("1. first item").as_deref(), Some("first item"));
        assert_eq!(from_prompt("```\ncode\n```").as_deref(), Some("code"));
        assert_eq!(from_prompt("   \n "), None);
        let long =
            "Сделай кронштейн для крепления камеры на трубу диаметром двадцать пять миллиметров";
        let t = from_prompt(long).unwrap();
        assert!(t.chars().count() <= MAX_TITLE, "{t}");
        assert!(
            t.ends_with('…') && long.starts_with(t.trim_end_matches('…')),
            "{t}"
        );
        assert!(!t.trim_end_matches('…').ends_with(' '));
        let word = "a".repeat(100);
        let t = from_prompt(&word).unwrap();
        assert_eq!(t.chars().count(), MAX_TITLE);
    }

    #[test]
    fn model_titles() {
        assert_eq!(
            from_model("\"Починка сборки\"\n").as_deref(),
            Some("Починка сборки")
        );
        assert_eq!(from_model("Title: **Fix CI.**").as_deref(), Some("Fix CI"));
        assert_eq!(
            from_model("  \n«Кронштейн камеры»\nexplanation").as_deref(),
            Some("Кронштейн камеры")
        );
        assert_eq!(from_model(" \n "), None);
        assert!(from_model(&"word ".repeat(40)).unwrap().chars().count() <= MAX_TITLE);
    }
}
