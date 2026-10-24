//! Session titles (T21, T24, SPEC §3 Session).
//!
//! After the first final `agent.message` of a session whose title is still the default
//! «Сессия N» (from a worker or from the mirror: an imported session the same way, by its
//! first records), the hub sends `title.request` to the session's worker in the background:
//! the first prompt and the first answer, each cut to [`MAX_TITLE_INPUT`] characters. The
//! worker asks Claude Code on its subscription and answers with a title of at most
//! [`MAX_TITLE`] characters. When the worker is offline, answers `error` or an empty title, or
//! does not answer within `timing.title_request` (30 s; only logged), the title is the first
//! line of the first prompt without markdown, cut at a word to [`MAX_TITLE`] characters with
//! «…». Either way `title_source` becomes `runtime`, and only a `default` title is replaced:
//! a user's or runtime's title is never overwritten.

use hqc_proto::frame::{HubFrame, ResultBody, TitleRequest, MAX_TITLE, MAX_TITLE_INPUT};
use uuid::Uuid;

use crate::{app::AppState, db, worker_ws::RequestError};

/// The first final answer of session `s` was stored: derive its title in the background if
/// it still has the default one. Once per session per hub process.
pub fn after_first_answer(app: &AppState, s: &db::SessionRow) {
    if s.title_source != "default" || s.system {
        return;
    }
    if !app.titling.lock().unwrap().insert(s.id) {
        return;
    }
    let (app, session_id, worker_id) = (app.clone(), s.id, s.worker_id);
    tokio::spawn(async move {
        if let Err(e) = derive(&app, session_id, worker_id).await {
            tracing::warn!(%session_id, "session title: {e:#}");
        }
    });
}

async fn derive(app: &AppState, session_id: Uuid, worker_id: Option<Uuid>) -> anyhow::Result<()> {
    let Some(prompt) = db::first_text(&app.db, session_id, "user.message").await? else {
        return Ok(());
    };
    let answer = db::first_text(&app.db, session_id, "agent.message")
        .await?
        .unwrap_or_default();
    let title = match from_worker(app, session_id, worker_id, &prompt, &answer).await {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::info!(%session_id, "no title from the worker, title from the prompt: {e}");
            None
        }
    };
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

/// `title.request` to the session's worker; its title as one line of at most [`MAX_TITLE`].
async fn from_worker(
    app: &AppState,
    session_id: Uuid,
    worker_id: Option<Uuid>,
    prompt: &str,
    answer: &str,
) -> Result<String, String> {
    let conn = worker_id
        .and_then(|w| app.workers.get(w))
        .ok_or("the worker is offline")?;
    let build = |request_id| {
        HubFrame::TitleRequest(TitleRequest {
            request_id,
            session_id,
            user_text: cut(prompt, MAX_TITLE_INPUT),
            agent_text: cut(answer, MAX_TITLE_INPUT),
        })
    };
    match conn.request(build, app.config.timing.title_request).await {
        Ok(ResultBody::Title(t)) => {
            let line = t.title.split_whitespace().collect::<Vec<_>>().join(" ");
            shorten(&line, MAX_TITLE).ok_or_else(|| "the worker returned an empty title".into())
        }
        Ok(other) => Err(format!("unexpected result {other:?}")),
        Err(RequestError::Offline) => Err("the worker went offline".into()),
        Err(RequestError::Timeout) => Err("the worker did not answer in time".into()),
        Err(RequestError::Worker(e)) => Err(format!("{}: {}", e.code, e.message)),
    }
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
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
}
