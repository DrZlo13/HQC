# T33: questions and decisions

## 1. The index: stored generated columns

Migration `0016` adds two stored generated columns to `events`:
- `search_text`: `payload ->> 'text'` for `user.message`, `agent.message`, `peer.message`
  and `system`, otherwise null (an empty text is null too);
- `search_words`: `to_tsvector('simple', <the same expression>)` (a generated column cannot
  refer to another one, so the expression is repeated).

Indexes: GIN on `search_words` and GIN `gin_trgm_ops` on `search_text` (both partial, `WHERE
search_text IS NOT NULL`), and expression GIN indexes on `sessions.title` (both kinds).

Why generated columns: Postgres computes them on every insert, so "kept up to date on
insert" needs no code in the hub's insert path, and `ALTER TABLE ... ADD COLUMN ...
GENERATED ... STORED` rewrites the table, which is the backfill (test
`the_migration_indexes_old_events`). Events are append-only (only `delivery` changes), so
nothing else ever needs recomputing.

Why a stored tsvector and not an expression index on `to_tsvector(search_text)`: the GIN
index stores no word positions, so a phrase query (`"tunnel hub"`) rechecks every row that
has all the words, and with an expression index that recheck recomputes `to_tsvector` per
row. Measured over 100k events (section 4): a phrase took 542 ms (20-word corpus) and 237 ms
(natural corpus) with the expression index, 11–15 ms with the stored column. The price is the
column itself: 20–27 MB per 60k texts, about the size of the text.

Session titles stay as expression indexes: titles change (rename, the runtime's title), there
are few sessions, and a stored column on `sessions` would buy nothing.

What is not indexed: `tool.call`/`tool.result` (output blobs, the task says so),
`agent.thinking` (not shown by default), `approval.request` descriptions and `clarify.request`
questions (structured, mostly tool text), `attachment` captions. Easy to add to the `CASE` if
someone misses them; it costs a migration (the generated expression cannot be altered in place,
the column has to be dropped and added again).

## 2. `pg_trgm` and the tests

`pg_trgm` is part of Postgres contrib and trusted since Postgres 13, so the migration's
`CREATE EXTENSION IF NOT EXISTS pg_trgm` works as the database owner (RUNBOOK §1.1 updated).
The e2e script's fresh database (`createdb`) got it that way.

The test harness puts every hub into its own schema and drops it with `CASCADE`. An
extension is one per database and lives in one schema: created by the first test's migration
inside its schema, it would vanish with that schema under the feet of the others. So the
harness creates it once in `public` (retrying the race of concurrent creates) and every
test's `search_path` is `<schema>,public`; the migration then finds it and does nothing.

## 3. The query

One function, `crates/hqc-hub/src/search.rs::search`, behind `GET /api/search` and
`hqc_search_history`:
- the owner's sessions joined to their projects (for the address), `NOT system`, optionally
  one project; events of those sessions with a match, `UNION ALL` the sessions whose title
  matches;
- exact: `search_words @@ websearch_to_tsquery('simple', q)`: words, `"phrases"`, `or`,
  `-word`; case-insensitive (`simple` lowercases), Russian and English alike (the database is
  UTF-8 with an `en_US.UTF-8` ctype on home-mac; a `C` ctype would not lowercase Cyrillic);
- fuzzy: `exact OR q <% search_text` (trigram word similarity: the query against the best
  matching stretch of the text). The default threshold 0.6 missed one typo in a short word
  (`pinapple` vs `pineapple` is 0.58, `tunnle` vs `tunnel` 0.57), so the query runs in a
  transaction with `set_config('pg_trgm.word_similarity_threshold', '0.5', true)`; fuzzy
  needs at least 3 characters (shorter ones are a trigram or two that match nearly every
  word);
- newest first by `(ts, id)`, `LIMIT n + 1` to know whether there is a next page; the cursor
  is `<ts in µs>.<id>` of the last hit, and the next page takes `(ts, id) < cursor`. Recency
  instead of relevance (`ts_rank`): a chat history is read by time, the order is stable
  under paging, and ranking would need every match scored before the first page;
- the title hit's time is the session's `created_at` (stable, unlike `updated_at`, so the
  cursor holds);
- the snippet is cut in Rust (`snippet`): about 160 characters on one line from a little
  before the first query word found (ignoring case; for a fuzzy hit the word's first four
  characters are tried too), else the beginning. `ts_headline` would reparse every text and
  does not know fuzzy matches;
- `statement_timeout` 5 s for the transaction; over it the query is refused with a message
  (`400 bad_request`, `invalid_args` for the tool) suggesting rarer words or a project.
  Further limits on query cost (rate per owner) are T36's.

The REST endpoint takes `project` as an id or a slug (the client sends nothing yet; the agent
sends slugs), `limit` 1–50 (20 by default). The tool has a fixed 20 per call and takes
`cursor` so an agent can page; the spec signature `hqc_search_history(query, project?,
fuzzy?)` got `cursor?`. Its hits have no `session_id`: the address is what the other tools
take, and fewer tokens.

`hqc_search_history` writes nothing, so `once_id` does not apply: a repeated call runs the
query again.

## 4. Timing over 100k events

`crates/hqc-hub/tests/search.rs::timing_over_100k_events` (ignored by default; run with
`cargo test --release -p hqc-hub --test search -- --ignored --nocapture`). 2026-10-25,
home-mac (Apple Silicon, macOS 26.6), Postgres 17.11 (Homebrew, default settings), through
the HTTP endpoint of a test hub on loopback, best of 5 runs (warm cache). Each corpus: one
owner, 100 sessions × 1000 events, 60 % with text (user and agent messages of 12–41 words),
40 % tool calls and results (not indexed); every 10 000th text has the rare word `quokka`.

Corpus A, 20 words in all (Russian and English): every word is in most texts, the worst case
for common words. Column and index sizes: `search_text` 16 MB, `search_words` 20 MB, words
index 5.6 MB, trigram index 17 MB.

| query | hits on the page | time |
|---|---|---|
| `quokka` | 10 | 1.8 ms |
| `quokka`, fuzzy | 10 | 4.5 ms |
| `deploy` (in most texts) | 20 | 19 ms |
| `миграция` (in most texts) | 20 | 18 ms |
| `"tunnel hub"` (phrase; both words in most texts, never adjacent) | 0 | 11 ms |
| `миграцию`, fuzzy | 20 | 945 ms |
| `tunel`, fuzzy | 20 | 883 ms |
| `nothinglikethis` | 0 | 1.5 ms |
| `nothinglikethis`, fuzzy | 0 | 3.1 ms |

Corpus B, 5000 words per language with a log-uniform distribution (word k about 1/k, as in
natural text). `search_words` 27 MB, words index 10 MB, trigram index 20 MB.

| query | hits on the page | time |
|---|---|---|
| `quokka` | 10 | 1.2 ms |
| `word1` (the most common word) | 20 | 19 ms |
| `word2000` | 20 | 1.6 ms |
| `слово30` | 20 | 2.8 ms |
| `"word1 word2"` | 20 | 15 ms |
| `wrd2000`, fuzzy | 20 | 86 ms |
| `слово`, fuzzy (a prefix of every Russian word) | 20 | 1.09 s |
| `nothinglikethis`, fuzzy | 0 | 3.9 ms |

Reading: exact queries are 1–20 ms whatever the word; fuzzy is cheap for rare trigrams and
costs about a second when the query's trigrams are in most rows (every candidate row is
rechecked with `word_similarity`). That is the case the 5 s limit is for. Seeding 100k events
took 7–9 s (the generated columns and three GIN indexes on insert); the migration's backfill
of an existing table of that size should be of the same order, not measured separately.

## 5. The client

- Desktop: the left column's field keeps its instant filter of the project's sessions by
  title and path and, from 2 characters and 300 ms after the last keystroke, asks the hub;
  the hits are a group "In messages" below the sessions: title, `archived`, time, the snippet
  with the query's words marked, the address; "Similar words" switches fuzzy (from 3
  characters), "More" loads the next page. A stale answer (the query changed meanwhile) is
  dropped.
- Phone: the search button of the list (it used to show the filter field in the header) opens
  `#/search`: back, the field, the sessions of all projects matching by title or path, then the
  same hits. The filter by path stays, on that screen.
- A hit is a link `#/s/<session>?seq=<seq>`; `Chat` gets `focusSeq`. The chat already loads
  the whole history page by page on open (`ChatSession.open`, since T5), so "load history
  around the seq" needs no new request: once `lastSeq` reaches the seq, the block that holds
  the event is found by `data-seq` on the feed's blocks (each block's first event; the last
  block starting at or before the seq, which covers events the feed hides or groups), scrolled
  to the middle, marked for 2.4 s, and the "stick to the bottom" of the feed is released. Each
  block is now wrapped in `<div class="feed-block">`; no style depended on the blocks being
  the feed's direct children.
- A partial history window (load only the pages around the seq for a long session) would be
  worth it if opening long sessions gets slow; today the whole history loads anyway.

## 6. Open questions

1. Grants (iteration 6): the query filters by `owner_id`; with grants it becomes "sessions the
   user can read", one more join.
2. A database with a `C` ctype would not lowercase Cyrillic in `to_tsvector('simple')` or in
   trigrams (`pg_trgm` treats non-ASCII letters by the locale). The deploy's database is
   created by `createdb` with the cluster's default locale; worth checking `\l` on the
   server before relying on Russian search there.
3. No live run: the tool is a plain query with no Claude Code specific behaviour; the T30
   pipe it uses is covered live.
4. Relevance ranking, prefix matching while typing (`deplo` finds nothing exact) and indexing
   requests or thinking: not asked for; fuzzy covers most of the prefix case.
