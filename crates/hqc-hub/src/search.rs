//! Search (SPEC §15): one query behind `GET /api/search` and `hqc_search_history`.
//!
//! - What is searched: `events.search_text` (the text of `user.message`, `agent.message`,
//!   `peer.message` and `system`) with its tsvector `search_words`, generated columns of
//!   migration `0016`, and session titles.
//! - Whose: the owner's sessions, archived ones included (and marked), the workers' service
//!   sessions left out. Until grants (iteration 6) nobody else's.
//! - How: exact words over a `simple` tsvector (`websearch_to_tsquery`: `"phrase"`, `or`,
//!   `-word`); `fuzzy` adds trigram word similarity (`<%`), which finds word forms and typos
//!   the `simple` configuration does not.
//! - Order: newest first by `(ts, id)`, paged by a cursor on that pair; a title hit stands at
//!   the session's creation time.
//! - Cost (SPEC §20): a 5 s `statement_timeout`; per owner `Limits::searches_per_owner_min`
//!   searches a minute, `Limits::fuzzy_searches_per_owner_min` of them fuzzy (a fuzzy query on
//!   common trigrams costs about a second, T33); `Limits::search_concurrency` at once on the
//!   hub ([`capped`]).

use chrono::{DateTime, Utc};
use hqc_proto::api::{SearchHit, SearchPage};
use sqlx::PgPool;
use uuid::Uuid;

/// Longest query, in characters.
pub const MAX_QUERY: usize = 200;
/// Shortest fuzzy query, in characters: shorter ones match nearly every word.
pub const MIN_FUZZY: usize = 3;
/// `pg_trgm.word_similarity_threshold` of a fuzzy query. The default 0.6 misses one typo in
/// a short word ("pinapple" in "pineapple" is 0.58).
const WORD_SIMILARITY: f32 = 0.5;
/// Longest a search may run (`statement_timeout`); beyond it the query is refused.
pub const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Hits per page by default and at most.
pub const PAGE: usize = 20;
pub const MAX_PAGE: usize = 50;
/// Characters of a snippet, and of context before the match in it.
const SNIPPET: usize = 160;
const BEFORE: usize = 50;

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub text: String,
    /// Only this project of the owner.
    pub project_id: Option<Uuid>,
    pub fuzzy: bool,
    pub cursor: Option<String>,
    pub limit: usize,
}

/// A query the search refuses; the message is for the person or the agent.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("{0}")]
    Invalid(String),
    /// Over the owner's searches per minute (SPEC §20).
    #[error("{0}")]
    Limited(String),
    /// The hub runs as many searches as it takes at once.
    #[error("the hub is busy with other searches; try again in a moment")]
    Busy,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// [`search`] within the caps of SPEC §20: the owner's searches per minute (fuzzy ones
/// counted on their own as well) and the searches running at once on the hub.
pub async fn capped(
    app: &crate::app::AppState,
    owner: Uuid,
    q: &Query,
) -> Result<SearchPage, SearchError> {
    let limits = &app.config.limits;
    let _permit = app
        .searches
        .clone()
        .try_acquire_owned()
        .map_err(|_| SearchError::Busy)?;
    let minute = chrono::TimeDelta::minutes(1);
    let now = Utc::now();
    let key = owner.to_string();
    if q.fuzzy {
        let max = limits.fuzzy_searches_per_owner_min as u64;
        if !app
            .budget
            .spend("fuzzy_searches", &key, 1, max, minute, now)
        {
            return Err(SearchError::Limited(format!(
                "at most {max} fuzzy searches a minute; search exact words or wait"
            )));
        }
    }
    let max = limits.searches_per_owner_min as u64;
    if !app.budget.spend("searches", &key, 1, max, minute, now) {
        return Err(SearchError::Limited(format!(
            "at most {max} searches a minute; wait a little"
        )));
    }
    search(&app.db, owner, q).await
}

#[derive(sqlx::FromRow)]
struct Row {
    session_id: Uuid,
    title: String,
    project_slug: String,
    slug: String,
    archived: bool,
    seq: Option<i64>,
    kind: Option<String>,
    ts: DateTime<Utc>,
    id: Uuid,
    text: String,
}

/// The owner's hits for `q`, newest first.
pub async fn search(db: &PgPool, owner: Uuid, q: &Query) -> Result<SearchPage, SearchError> {
    let text = q.text.trim();
    let chars = text.chars().count();
    if chars == 0 {
        return Err(SearchError::Invalid("the query is empty".into()));
    }
    if chars > MAX_QUERY {
        return Err(SearchError::Invalid(format!(
            "the query is longer than {MAX_QUERY} characters"
        )));
    }
    if q.fuzzy && chars < MIN_FUZZY {
        return Err(SearchError::Invalid(format!(
            "a fuzzy query needs at least {MIN_FUZZY} characters"
        )));
    }
    let after = q.cursor.as_deref().map(parse_cursor).transpose()?;
    let limit = q.limit.clamp(1, MAX_PAGE);
    // `$2` is the query; `words` is the tsvector of `text`. The indexes of migration 0016
    // serve both forms.
    let matches = |words: &str, text: &str| {
        let exact = format!("{words} @@ websearch_to_tsquery('simple', $2)");
        if q.fuzzy {
            format!("({exact} OR $2 <% {text})")
        } else {
            exact
        }
    };
    let sql = format!(
        "WITH s AS (
             SELECT s.id, s.title, s.slug, s.archived, s.created_at, p.slug AS project_slug
             FROM sessions s JOIN projects p ON p.id = s.project_id
             WHERE s.owner_id = $1 AND NOT s.system AND ($3::uuid IS NULL OR s.project_id = $3)
         )
         SELECT * FROM (
             SELECT s.id AS session_id, s.title, s.project_slug, s.slug, s.archived,
                    e.seq, e.type AS kind, e.ts, e.event_id AS id, e.search_text AS text
             FROM events e JOIN s ON s.id = e.session_id
             WHERE e.search_text IS NOT NULL AND {events}
             UNION ALL
             SELECT s.id, s.title, s.project_slug, s.slug, s.archived,
                    NULL, NULL, s.created_at, s.id, s.title
             FROM s WHERE {titles}
         ) hit
         WHERE $4::timestamptz IS NULL OR (hit.ts, hit.id) < ($4, $5)
         ORDER BY hit.ts DESC, hit.id DESC
         LIMIT $6",
        events = matches("e.search_words", "e.search_text"),
        titles = matches("to_tsvector('simple', s.title)", "s.title"),
    );
    let mut tx = db.begin().await?;
    sqlx::query("SELECT set_config('statement_timeout', $1, true)")
        .bind(format!("{}ms", TIMEOUT.as_millis()))
        .execute(&mut *tx)
        .await?;
    if q.fuzzy {
        sqlx::query("SELECT set_config('pg_trgm.word_similarity_threshold', $1, true)")
            .bind(WORD_SIMILARITY.to_string())
            .execute(&mut *tx)
            .await?;
    }
    let mut rows: Vec<Row> = sqlx::query_as(&sql)
        .bind(owner)
        .bind(text)
        .bind(q.project_id)
        .bind(after.map(|a| a.0))
        .bind(after.map(|a| a.1))
        .bind(limit as i64 + 1)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d) if d.code().as_deref() == Some("57014") => {
                SearchError::Invalid(format!(
                    "the search took longer than {} s: use rarer words, exact words or a project",
                    TIMEOUT.as_secs()
                ))
            }
            _ => e.into(),
        })?;
    tx.commit().await?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = more
        .then(|| rows.last().map(|r| cursor(r.ts, r.id)))
        .flatten();
    let results = rows
        .into_iter()
        .map(|r| SearchHit {
            session_id: r.session_id,
            address: format!("{}/{}", r.project_slug, r.slug),
            title: r.title,
            archived: r.archived,
            seq: r.seq.map(|s| s.max(0) as u64),
            kind: r.kind,
            ts: r.ts,
            snippet: snippet(&r.text, text),
        })
        .collect();
    Ok(SearchPage {
        results,
        next_cursor,
    })
}

/// The owner's project by id or slug.
pub async fn project_of(db: &PgPool, owner: Uuid, p: &str) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar(
        "SELECT id FROM projects WHERE owner_id = $1 AND (id::text = $2 OR slug = $2)
         ORDER BY id::text = $2 DESC LIMIT 1",
    )
    .bind(owner)
    .bind(p)
    .fetch_optional(db)
    .await
}

/// `<ts in µs since the epoch>.<id>`: opaque to clients.
fn cursor(ts: DateTime<Utc>, id: Uuid) -> String {
    format!("{}.{id}", ts.timestamp_micros())
}

fn parse_cursor(c: &str) -> Result<(DateTime<Utc>, Uuid), SearchError> {
    let bad = || SearchError::Invalid("the cursor is not one this search returned".into());
    let (ts, id) = c.split_once('.').ok_or_else(bad)?;
    let ts = DateTime::from_timestamp_micros(ts.parse().map_err(|_| bad())?).ok_or_else(bad)?;
    Ok((ts, id.parse().map_err(|_| bad())?))
}

/// About [`SNIPPET`] characters of `text` on one line, starting a little before the first
/// word of `query` found in it (ignoring case), or at the start; `…` marks a cut.
pub fn snippet(text: &str, query: &str) -> String {
    let chars: Vec<char> = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect();
    let lower: Vec<char> = chars.iter().map(|c| lower(*c)).collect();
    let at = words(query)
        .filter_map(|w| {
            find(&lower, &w).or_else(|| {
                // A fuzzy hit: the word's beginning may still be there.
                let head: Vec<char> = w.iter().copied().take(MIN_FUZZY + 1).collect();
                (head.len() < w.len())
                    .then(|| find(&lower, &head))
                    .flatten()
            })
        })
        .min()
        .unwrap_or(0);
    let mut start = at.saturating_sub(BEFORE);
    if chars.len() <= SNIPPET {
        start = 0;
    } else {
        start = start.min(chars.len() - SNIPPET);
        // Do not start in the middle of a word.
        if start > 0 {
            if let Some(sp) = chars[start..at.max(start)].iter().position(|c| *c == ' ') {
                start += sp + 1;
            }
        }
    }
    let end = (start + SNIPPET).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// The query's words, lowercased, without the `websearch` syntax.
fn words(query: &str) -> impl Iterator<Item = Vec<char>> + '_ {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !w.eq_ignore_ascii_case("or"))
        .map(|w| w.chars().map(lower).collect())
}

fn find(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets() {
        assert_eq!(snippet("Run  the\ntests", "tests"), "Run the tests");
        let long = format!("{} needle {}", "a ".repeat(100), "b ".repeat(100));
        let s = snippet(&long, "NEEDLE");
        assert!(s.starts_with('…') && s.ends_with('…'), "{s}");
        assert!(s.contains("needle"));
        assert!(s.chars().count() <= SNIPPET + 2);
        // Cyrillic, case-insensitive.
        let ru = format!("{} Миграция индекса", "слово ".repeat(40));
        assert!(snippet(&ru, "миграция").contains("Миграция"));
        // No word found (a fuzzy hit on another form): the beginning.
        assert!(snippet(&long, "zzz").starts_with("a a"));
        // A fuzzy hit on another ending: found by the word's beginning.
        let s = snippet(&format!("{} deployment", "x ".repeat(100)), "deploying");
        assert!(s.contains("deployment"), "{s}");
    }

    #[test]
    fn cursors() {
        let ts = DateTime::from_timestamp_micros(1_729_443_731_123_456).unwrap();
        let id = Uuid::now_v7();
        assert_eq!(parse_cursor(&cursor(ts, id)).unwrap(), (ts, id));
        for bad in ["", "x", "1.2", "abc.0199f3a2-7c10-7000-8000-000000000002"] {
            assert!(parse_cursor(bad).is_err(), "{bad}");
        }
    }
}
