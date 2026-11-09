//! Search (SPEC §15): one query behind `GET /api/search` and `hqc_search_history`.
//!
//! - What is searched: `events.search_words`, the weighted tsvector of migration `0037`: the
//!   text of `user.message`, `agent.message`, `peer.message`, `system` and the answers of
//!   `clarify.response` (weight A), a `tool.call`'s tool and arguments (B), a `tool.result`'s
//!   output (D); and session titles (A). `/` and `.` are read as spaces there and in the
//!   query, so a path or an address is a phrase of its parts.
//! - Whose: the owner's sessions, archived ones included (and marked), the workers' service
//!   sessions left out. Until grants (iteration 6) nobody else's.
//! - How: the `hqc` configuration stems Russian and English words, so a word finds its other
//!   forms. [`Parsed`] reads the query (words ANDed, `"phrase"`, `or`, `-word`, `word*`);
//!   Postgres reads each operand with `to_tsquery` ([`READ`]), and [`Parsed::assembled`] puts
//!   the query together. `word*` is also the unstemmed prefix and the shorter stems the
//!   stemmer gives it with common English endings; `fuzzy` widens each plain word to the most
//!   similar words of `search_vocab`, the words of the index (trigram similarity), which finds
//!   typos.
//! - Order: best match first (`ts_rank` by the query without its negated words), then newest;
//!   paged by an offset.
//! - Cost (SPEC §20): a 5 s `statement_timeout` on each statement of a search (reading the
//!   query, then the hits); per owner `Limits::searches_per_owner_min`
//!   searches a minute, `Limits::fuzzy_searches_per_owner_min` of them fuzzy;
//!   `Limits::search_concurrency` at once on the hub ([`capped`]).

use chrono::{DateTime, Utc};
use hqc_proto::api::{SearchHit, SearchPage};
use sqlx::PgPool;
use uuid::Uuid;

/// Longest query, in characters.
pub const MAX_QUERY: usize = 200;
/// Shortest fuzzy query, and shortest word a fuzzy query widens, in characters: shorter ones
/// are similar to nearly every word.
pub const MIN_FUZZY: usize = 3;
/// `pg_trgm.similarity_threshold` of a fuzzy query, between a word's stem and the words of
/// `search_vocab`.
const SIMILARITY: f32 = 0.4;
/// Longest each statement of a search may run (`statement_timeout`); beyond it the search is
/// refused.
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
    text: String,
}

/// The owner's hits for `q`, best first.
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
    let offset = q
        .cursor
        .as_deref()
        .map(parse_cursor)
        .transpose()?
        .unwrap_or(0);
    let limit = q.limit.clamp(1, MAX_PAGE);
    let parsed = Parsed::new(text);
    let page = (owner, q.project_id, offset, limit);
    let mut tx = begin(db).await?;
    let (mut rows, also) = ranked(&mut tx, &parsed, q.fuzzy, page)
        .await
        .map_err(refused)?;
    tx.commit().await?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = more.then(|| (offset + limit as u32).to_string());
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
            snippet: snippet(&r.text, text, &also),
        })
        .collect();
    Ok(SearchPage {
        results,
        next_cursor,
    })
}

/// SQLSTATE of a query cancelled by `statement_timeout`.
const TIMED_OUT: &str = "57014";

/// A transaction whose each statement runs under the search's `statement_timeout`.
async fn begin(db: &PgPool) -> sqlx::Result<sqlx::Transaction<'static, sqlx::Postgres>> {
    let mut tx = db.begin().await?;
    sqlx::query("SELECT set_config('statement_timeout', $1, true)")
        .bind(format!("{}ms", TIMEOUT.as_millis()))
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

fn refused(e: sqlx::Error) -> SearchError {
    match &e {
        sqlx::Error::Database(d) if d.code().as_deref() == Some(TIMED_OUT) => {
            SearchError::Invalid(format!(
                "the search took longer than {} s: use rarer words, exact words or a project",
                TIMEOUT.as_secs()
            ))
        }
        _ => e.into(),
    }
}

/// The page `(offset, limit)` of the hits of `parsed`, and the vocabulary's words a fuzzy query
/// added (for the snippet).
async fn ranked(
    tx: &mut sqlx::PgConnection,
    parsed: &Parsed,
    fuzzy: bool,
    page: (Uuid, Option<Uuid>, u32, usize),
) -> sqlx::Result<(Vec<Row>, Vec<String>)> {
    if fuzzy {
        sqlx::query("SELECT set_config('pg_trgm.similarity_threshold', $1, true)")
            .bind(SIMILARITY.to_string())
            .execute(&mut *tx)
            .await?;
    }
    let terms: Vec<String> = parsed.operands().map(|o| o.term.tsquery()).collect();
    let words: Vec<Option<&str>> = parsed
        .operands()
        .map(|o| o.fuzzy_word().filter(|_| fuzzy))
        .collect();
    let prefixes: Vec<Option<String>> = parsed
        .operands()
        .map(|o| {
            o.prefix_word()
                .filter(|p| p.chars().all(|c| c.is_ascii_alphabetic()))
                .map(str::to_ascii_lowercase)
        })
        .collect();
    let read: Vec<ReadOperand> = sqlx::query_as(READ)
        .bind(&terms)
        .bind(&words)
        .bind(&prefixes)
        .fetch_all(&mut *tx)
        .await?;
    let (tsq, rank) = (
        parsed.assembled(&read, true),
        parsed.assembled(&read, false),
    );
    let rows = hits(tx, (&tsq, &rank), page).await?;
    Ok((rows, read.into_iter().flat_map(|w| w.alts).collect()))
}

/// An operand as Postgres read it: its text through `to_tsquery`, the words of
/// `search_vocab` similar to it (for a fuzzy query), and the stems shorter than a prefix that
/// its word may have (for `word*`).
#[derive(Debug, sqlx::FromRow)]
struct ReadOperand {
    read: String,
    alts: Vec<String>,
    stems: Vec<String>,
}

/// For each operand (`$1`, its `to_tsquery` text, in order): that text as `to_tsquery` reads
/// it; for a word a fuzzy query widens (`$2`, NULL for the others) whose stem is one lexeme,
/// the words of `search_vocab` most similar to the stem; for a prefix of ASCII letters,
/// lowercased (`$3`, NULL for the others), the stems the stemmer gives it with common English
/// endings that are shorter than it, begin it, have 4 letters at least and are not its own
/// stem ("connecti" + "on" is "connect"; no "y", "ies", "ly", as "modely" is "mode").
const READ: &str = "SELECT to_tsquery('hqc', term)::text AS read,
         CASE WHEN cardinality(l) = 1 THEN ARRAY(
             SELECT v.word FROM search_vocab v WHERE v.word % l[1] AND v.word <> l[1]
             ORDER BY similarity(v.word, l[1]) DESC, v.word LIMIT 6)
         ELSE '{}' END AS alts,
         ARRAY(
             SELECT DISTINCT w
             FROM unnest(ARRAY['', 's', 'es', 'e', 'ed', 'd', 'ing', 'ion', 'ions', 'on', 'n',
                     'tion', 'ation', 'ment', 'ent', 'nt', 'ity', 'ive', 'al', 'er', 'ers',
                     'ness', 'able', 'ize', 'ate']) s,
                  unnest(tsvector_to_array(to_tsvector('hqc', typed || s))) w
             WHERE length(w) >= 4 AND length(w) < length(typed)
                 AND left(typed, length(w)) = w AND w <> ALL(p)
             ORDER BY w) AS stems
     FROM unnest($1::text[], $2::text[], $3::text[]) WITH ORDINALITY u(term, word, typed, n),
          tsvector_to_array(to_tsvector('hqc', coalesce(word, ''))) l,
          tsvector_to_array(to_tsvector('hqc', coalesce(typed, ''))) p
     ORDER BY n";

/// The page `(offset, limit)` of the hits among the sessions of `owner` (in one project) of
/// the tsquery text `tsq` (`$5`), ranked by the tsquery text `rank` (`$6`), best first. One
/// more hit than `limit` tells there is another page.
async fn hits(
    tx: &mut sqlx::PgConnection,
    (tsq, rank): (&str, &str),
    (owner, project, offset, limit): (Uuid, Option<Uuid>, u32, usize),
) -> sqlx::Result<Vec<Row>> {
    // `$5` and `$6` are cast once each: the statement is planned for its values (below).
    const SQL: &str = r#"WITH s AS (
             SELECT s.id, s.title, s.slug, s.archived, s.created_at, p.slug AS project_slug
             FROM sessions s JOIN projects p ON p.id = s.project_id
             WHERE s.owner_id = $1 AND NOT s.system AND ($2::uuid IS NULL OR s.project_id = $2)
         ), hit AS (
             SELECT e.session_id, e.seq, e.type AS kind, e.ts, e.event_id AS id,
                    ts_rank(e.search_words, $6::tsquery, 1) AS rank
             FROM events e JOIN s ON s.id = e.session_id
             WHERE e.search_words IS NOT NULL AND e.search_words @@ $5::tsquery
             UNION ALL
             SELECT s.id, NULL, NULL, s.created_at, s.id, ts_rank(
                    setweight(to_tsvector('hqc', translate(s.title, '/.', '  ')), 'A'),
                    $6::tsquery, 1)
             FROM s WHERE to_tsvector('hqc', translate(s.title, '/.', '  ')) @@ $5::tsquery
             ORDER BY rank DESC, ts DESC, id DESC
             OFFSET $3 LIMIT $4
         )
         SELECT hit.session_id, s.title, s.project_slug, s.slug, s.archived, hit.seq, hit.kind,
                hit.ts,
                CASE
                    WHEN hit.seq IS NULL THEN s.title
                    WHEN hit.kind = 'tool.call' THEN (e.payload ->> 'tool') || ' '
                        || regexp_replace(e.payload ->> 'args_preview', '\\[nrt]', ' ', 'g')
                    WHEN hit.kind = 'tool.result'
                        THEN regexp_replace(e.payload ->> 'result_preview', '\\[nrt]', ' ', 'g')
                    WHEN hit.kind = 'clarify.response' THEN (
                        SELECT string_agg(a, '; ') FROM jsonb_array_elements_text(
                            jsonb_path_query_array(e.payload -> 'answers', '$.*[*]')) a)
                    ELSE e.payload ->> 'text'
                END AS text
         FROM hit JOIN s ON s.id = hit.session_id LEFT JOIN events e ON e.event_id = hit.id
         ORDER BY hit.rank DESC, hit.ts DESC, hit.id DESC"#;
    // Not a prepared statement: planned for its values every time. A cached generic plan
    // does not know the owner's sessions and searches each one on its own (1.5 s instead of
    // 50 ms for "modbus" over 916k events).
    sqlx::query_as(SQL)
        .persistent(false)
        .bind(owner)
        .bind(project)
        .bind(offset as i64)
        .bind(limit as i64 + 1)
        .bind(tsq)
        .bind(rank)
        .fetch_all(tx)
        .await
}

/// A query read for `to_tsquery('hqc', …)`: groups ANDed, the operands of a group ORed, so
/// `a b or c` is `a & (b | c)`, as `websearch_to_tsquery` reads it.
#[derive(Debug, Default, PartialEq)]
struct Parsed(Vec<Vec<Operand>>);

#[derive(Debug, PartialEq)]
struct Operand {
    /// `-word` (four letters or more after the dash), `-"phrase"`.
    not: bool,
    term: Term,
}

#[derive(Debug, PartialEq)]
enum Term {
    /// `prefix` for `word*`.
    Word { text: String, prefix: bool },
    /// `"words in a row"`.
    Phrase(Vec<String>),
}

impl Parsed {
    fn new(query: &str) -> Self {
        let mut groups: Vec<Vec<Operand>> = Vec::new();
        let mut or = false;
        let mut chars = query.chars().peekable();
        loop {
            while chars.next_if(|c| c.is_whitespace()).is_some() {}
            if chars.peek().is_none() {
                break;
            }
            // `-` excludes a phrase or a word that starts with four letters; before anything
            // else it is part of the word, so a command pasted with its flags (`-k`, `-rf`,
            // `-lah`, `--force`) finds its call: the index reads them as the words `k`, `rf`,
            // `lah`, `force`.
            let mut ahead = chars.clone();
            let not = ahead.next() == Some('-')
                && (ahead.peek() == Some(&'"')
                    || ahead.take(4).filter(|c| c.is_alphabetic()).count() == 4);
            if not {
                chars.next();
            }
            let term = if chars.next_if_eq(&'"').is_some() {
                let words: String = chars.by_ref().take_while(|c| *c != '"').collect();
                let words = spaced(&words);
                let words: Vec<String> = words.split_whitespace().map(str::to_string).collect();
                if words.is_empty() {
                    continue;
                }
                Term::Phrase(words)
            } else {
                let mut word = String::new();
                while let Some(c) = chars.next_if(|c| !c.is_whitespace() && *c != '"') {
                    word.push(c);
                }
                if !not && word.eq_ignore_ascii_case("or") {
                    or = !groups.is_empty();
                    continue;
                }
                let text = word.trim_end_matches('*');
                let prefix = text.len() < word.len();
                let text = spaced(text);
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                Term::Word {
                    prefix,
                    text: text.to_string(),
                }
            };
            let operand = Operand { not, term };
            match groups.last_mut() {
                Some(g) if or => g.push(operand),
                _ => groups.push(vec![operand]),
            }
            or = false;
        }
        Parsed(groups)
    }

    fn operands(&self) -> impl Iterator<Item = &Operand> {
        self.0.iter().flatten()
    }

    /// The query from the text of each operand, by its position (`None` leaves it out): groups
    /// ANDed, the operands of a group ORed, `!` before a negated one. Without `negated` the
    /// negated operands are left out: that is the query to rank by, as `ts_rank` takes a
    /// negated word for one a hit should have and ranks every hit next to nothing.
    fn assemble(&self, negated: bool, text: impl Fn(usize, &Operand) -> Option<String>) -> String {
        let mut n = 0;
        let mut groups = Vec::new();
        for g in &self.0 {
            let mut ops = Vec::new();
            for o in g {
                match text(n, o) {
                    Some(t) if o.not => {
                        if negated {
                            ops.push(format!("!{t}"));
                        }
                    }
                    Some(t) => ops.push(t),
                    None => {}
                }
                n += 1;
            }
            match ops.len() {
                0 => {}
                1 => groups.extend(ops),
                _ => groups.push(format!("( {} )", ops.join(" | "))),
            }
        }
        groups.join(" & ")
    }

    /// The query as tsquery text to cast as it is, `negated` operands too or not
    /// ([`Parsed::assemble`]), from its operands as Postgres read them (`read`, by position).
    /// A prefix is ORed with the word unstemmed ("copy*" is also "copytoclipboard", which its
    /// stem "copi" is no prefix of) and with the shorter stems its word may have ([`READ`]), as
    /// whole lexemes: the stem of a longer word may be shorter than the prefix ("connecti*" is
    /// also "connect", the stem of "connection"), while "worker*" is not also "work". A fuzzy
    /// word is ORed with its similar words. Those are lexemes already: stemmed again, about one
    /// in nine would become another ("прощан" becomes "проща") and miss the words it stands
    /// for.
    fn assembled(&self, read: &[ReadOperand], negated: bool) -> String {
        self.assemble(negated, |n, o| {
            let r = read.get(n)?;
            let mut parts = Vec::new();
            if !r.read.is_empty() {
                parts.push(format!("( {} )", r.read));
            }
            if let Some(text) = o.prefix_word() {
                let raw = format!("{}:*", quote(&text.to_lowercase()));
                if raw != r.read {
                    parts.push(raw);
                }
            }
            parts.extend(r.stems.iter().map(|w| quote(w)));
            parts.extend(r.alts.iter().map(|a| quote(a)));
            match parts.len() {
                0 => None,
                1 => parts.pop(),
                _ => Some(format!("( {} )", parts.join(" | "))),
            }
        })
    }
}

impl Operand {
    /// The word a fuzzy query widens: not in a phrase, not negated, not a prefix, at least
    /// [`MIN_FUZZY`] characters after its leading dashes (`-rf` is not widened).
    fn fuzzy_word(&self) -> Option<&str> {
        match &self.term {
            Term::Word {
                text,
                prefix: false,
            } if !self.not && text.trim_start_matches('-').chars().count() >= MIN_FUZZY => {
                Some(text.as_str())
            }
            _ => None,
        }
    }

    /// The text of a prefix (`word*`) of one word, as typed.
    fn prefix_word(&self) -> Option<&str> {
        match &self.term {
            Term::Word { text, prefix: true } if !text.contains(' ') => Some(text.as_str()),
            _ => None,
        }
    }
}

impl Term {
    /// The `to_tsquery` text of the term, quoted, so nothing in it is read as an operator. A
    /// phrase is one quoted text: `to_tsquery` puts `<->` between its words itself, and a
    /// sign between them ("ls -la | grep") leaves no gap the text does not have.
    fn tsquery(&self) -> String {
        match self {
            Term::Word { text, prefix } => {
                format!("{}{}", quote(text), if *prefix { ":*" } else { "" })
            }
            Term::Phrase(words) => quote(&words.join(" ")),
        }
    }
}

/// `/` and `.` read as spaces, as the index reads them (migration `0037`): a path or an address
/// is a phrase of its parts ("docs/SPEC.md" is "docs SPEC md").
fn spaced(text: &str) -> String {
    text.replace(['/', '.'], " ")
}

/// `word` as a quoted tsquery operand: its quotes and backslashes doubled.
fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\\', "\\\\").replace('\'', "''"))
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

/// The cursor is the offset of the next page, opaque to clients.
fn parse_cursor(c: &str) -> Result<u32, SearchError> {
    c.parse()
        .map_err(|_| SearchError::Invalid("the cursor is not one this search returned".into()))
}

/// About [`SNIPPET`] characters of `text` on one line, starting a little before the first
/// word of `query` or of `also` (a fuzzy query's alternatives) found in it (ignoring case),
/// or at the start; `…` marks a cut.
pub fn snippet(text: &str, query: &str, also: &[String]) -> String {
    let chars: Vec<char> = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect();
    let lower: Vec<char> = chars.iter().map(|c| lower(*c)).collect();
    let at = words(query)
        .chain(also.iter().map(|w| w.chars().map(self::lower).collect()))
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

/// The query's words, lowercased, without its syntax.
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
        assert_eq!(snippet("Run  the\ntests", "tests", &[]), "Run the tests");
        let long = format!("{} needle {}", "a ".repeat(100), "b ".repeat(100));
        let s = snippet(&long, "NEEDLE", &[]);
        assert!(s.starts_with('…') && s.ends_with('…'), "{s}");
        assert!(s.contains("needle"));
        assert!(s.chars().count() <= SNIPPET + 2);
        // Cyrillic, case-insensitive.
        let ru = format!("{} Миграция индекса", "слово ".repeat(40));
        assert!(snippet(&ru, "миграция", &[]).contains("Миграция"));
        // No word found: the beginning.
        assert!(snippet(&long, "zzz", &[]).starts_with("a a"));
        // Another ending: found by the word's beginning.
        let s = snippet(
            &format!("{} deployment", "x ".repeat(100)),
            "deploying",
            &[],
        );
        assert!(s.contains("deployment"), "{s}");
        // A typo: found by a fuzzy query's alternative.
        let s = snippet(
            &long.replace("needle", "Modbus"),
            "modbas",
            &["modbus".into()],
        );
        assert!(s.contains("Modbus"), "{s}");
    }

    #[test]
    fn cursors() {
        assert_eq!(parse_cursor("40").unwrap(), 40);
        for bad in [
            "",
            "x",
            "1.2",
            "-20",
            "abc.0199f3a2-7c10-7000-8000-000000000002",
        ] {
            assert!(parse_cursor(bad).is_err(), "{bad}");
        }
    }

    /// The query's operands as `to_tsquery` texts, `negated` ones too or not.
    fn tsquery_of(q: &str, negated: bool) -> String {
        Parsed::new(q).assemble(negated, |_, o| Some(o.term.tsquery()))
    }

    fn tsquery(q: &str) -> String {
        tsquery_of(q, true)
    }

    #[test]
    fn queries() {
        assert_eq!(tsquery("launchctl kickstart"), "'launchctl' & 'kickstart'");
        assert_eq!(tsquery("\"connection refused\""), "'connection refused'");
        assert_eq!(tsquery("kube*"), "'kube':*");
        assert_eq!(tsquery("deploy -staging"), "'deploy' & !'staging'");
        assert_eq!(
            tsquery("hub tunnel or deploy OR ci worker"),
            "'hub' & ( 'tunnel' | 'deploy' | 'ci' ) & 'worker'"
        );
        assert_eq!(tsquery("-\"old plan\" переезд"), "!'old plan' & 'переезд'");
        // `or` at an end, `*`, `.` or empty quotes are no operands; a lone `-` is a word, of no
        // lexemes to Postgres.
        assert_eq!(tsquery("or tunnel or"), "'tunnel'");
        assert_eq!(tsquery("- * . \"\" \"  \" ok"), "'-' & 'ok'");
        assert_eq!(tsquery("-or"), "'-or'");
        // A dash excludes before a phrase or four letters; before anything else it is part of
        // the word, so a pasted command keeps its flags.
        assert_eq!(
            tsquery("launchctl kickstart -k gui/501/x"),
            "'launchctl' & 'kickstart' & '-k' & 'gui 501 x'"
        );
        assert_eq!(tsquery("rm -rf build"), "'rm' & '-rf' & 'build'");
        assert_eq!(
            tsquery("tar -xzf x ls -lah curl -sSL"),
            "'tar' & '-xzf' & 'x' & 'ls' & '-lah' & 'curl' & '-sSL'"
        );
        assert_eq!(
            tsquery("git push --force -- -1"),
            "'git' & 'push' & '--force' & '--' & '-1'"
        );
        assert_eq!(
            tsquery("-staging -переезд -test -\"a b\" -abc1"),
            "!'staging' & !'переезд' & !'test' & !'a b' & '-abc1'"
        );
        // An unclosed quote runs to the end.
        assert_eq!(tsquery("\"a b"), "'a b'");
        // `/` and `.` are spaces: a path or an address is a phrase of its parts.
        assert_eq!(
            tsquery("docs/SPEC.md 10.31.80.253 \"see src/search.rs\""),
            "'docs SPEC md' & '10 31 80 253' & 'see src search rs'"
        );
        // The query to rank by has no negated words.
        let q = "tunnel -restarted a or -bees -\"x y\"";
        assert_eq!(
            tsquery_of(q, true),
            "'tunnel' & !'restarted' & ( 'a' | !'bees' ) & !'x y'"
        );
        assert_eq!(tsquery_of(q, false), "'tunnel' & 'a'");
    }

    /// The query from its operands as Postgres read them: a prefix also unstemmed, the similar
    /// words of a fuzzy one as they are.
    #[test]
    fn assembled_queries() {
        let p = Parsed::new("modbas -relay \"x y\" ! Copy* kube*");
        let r = |read: &str, alts: &[&str]| ReadOperand {
            read: read.into(),
            alts: alts.iter().map(|a| a.to_string()).collect(),
            stems: Vec::new(),
        };
        let read = [
            r("'modba'", &["modbus", "it's"]),
            r("'relay'", &[]),
            r("'x' <-> 'y'", &[]),
            r("", &[]),
            r("'copi':*", &[]),
            r("'kube':*", &[]),
        ];
        assert_eq!(
            p.assembled(&read, true),
            "( ( 'modba' ) | 'modbus' | 'it''s' ) & !( 'relay' ) & ( 'x' <-> 'y' ) \
             & ( ( 'copi':* ) | 'copy':* ) & ( 'kube':* )"
        );
        assert_eq!(
            p.assembled(&read, false),
            "( ( 'modba' ) | 'modbus' | 'it''s' ) & ( 'x' <-> 'y' ) \
             & ( ( 'copi':* ) | 'copy':* ) & ( 'kube':* )"
        );
        // A prefix longer than the stem of its word: also the shorter stems its word may have.
        let p = Parsed::new("Connecti* worker*");
        let connecti = ReadOperand {
            stems: vec!["connect".into()],
            ..r("'connecti':*", &[])
        };
        assert_eq!(
            p.assembled(&[connecti, r("'worker':*", &[])], true),
            "( ( 'connecti':* ) | 'connect' ) & ( 'worker':* )"
        );
    }

    /// Whatever the query holds, every word stays one quoted operand: tsquery operators,
    /// quotes and backslashes in it are text.
    #[test]
    fn hostile_queries() {
        for (q, want) in [
            ("a'b", r"'a''b'"),
            (r"a\b", r"'a\\b'"),
            (r"x\", r"'x\\'"),
            (r"\'", r"'\\'''"),
            ("'", "''''"),
            ("a&b|c", "'a&b|c'"),
            ("!x", "'!x'"),
            ("(a", "'(a'"),
            ("a)", "'a)'"),
            ("a<->b", "'a<->b'"),
            ("a<2>b", "'a<2>b'"),
            ("a:*", "'a:':*"),
            ("a:AB", "'a:AB'"),
            ("x:* & y", "'x:':* & '&' & 'y'"),
            (
                "'; DROP TABLE events; --",
                "''';' & 'DROP' & 'TABLE' & 'events;' & '--'",
            ),
            ("\"a' | b\"", "'a'' | b'"),
        ] {
            assert_eq!(tsquery(q), want, "{q}");
        }
    }

    #[test]
    fn plain_words_of_a_fuzzy_query() {
        let p = Parsed::new("modbas \"in phrase\" -negated kube* ab переежд -rf --force");
        let words: Vec<&str> = p.operands().filter_map(Operand::fuzzy_word).collect();
        assert_eq!(words, ["modbas", "переежд", "--force"]);
    }
}
