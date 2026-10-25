-- SPEC 0.36 (T33, search, SPEC §15).
-- One index for the client's search bar and `hqc_search_history`: the text a human reads
-- (`user.message`, `agent.message`, `peer.message`, `system`; no tool output) and session
-- titles, as a `simple` tsvector for exact words and trigrams for fuzzy matches.
--
-- Both columns are stored generated columns: Postgres fills them on every insert, and adding
-- them rewrites the table, which backfills the existing rows. The tsvector is stored rather
-- than an expression index because a phrase query rechecks every candidate row, and
-- recomputing `to_tsvector` there cost 0.2–0.5 s over 100k events (docs/notes/t33-questions.md).
-- pg_trgm is a trusted extension (Postgres 13+): the database owner may create it.
CREATE EXTENSION IF NOT EXISTS pg_trgm;

ALTER TABLE events ADD COLUMN search_text text GENERATED ALWAYS AS (
    CASE WHEN type IN ('user.message', 'agent.message', 'peer.message', 'system')
         THEN NULLIF(payload ->> 'text', '')
    END
) STORED;
-- The same expression: a generated column cannot refer to another one.
ALTER TABLE events ADD COLUMN search_words tsvector GENERATED ALWAYS AS (
    to_tsvector('simple',
        CASE WHEN type IN ('user.message', 'agent.message', 'peer.message', 'system')
             THEN NULLIF(payload ->> 'text', '')
        END)
) STORED;

CREATE INDEX events_search_words ON events USING gin (search_words)
    WHERE search_text IS NOT NULL;
CREATE INDEX events_search_trgm ON events USING gin (search_text gin_trgm_ops)
    WHERE search_text IS NOT NULL;
CREATE INDEX sessions_title_words ON sessions USING gin (to_tsvector('simple', title));
CREATE INDEX sessions_title_trgm ON sessions USING gin (title gin_trgm_ops);
