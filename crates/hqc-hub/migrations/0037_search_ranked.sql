-- SPEC 0.97 (search, SPEC §15): word forms, tool calls and output, ranking, a vocabulary for
-- fuzzy search instead of trigrams over the text.
--
-- `hqc`: the `russian` configuration with snowball stemmers that have no stop words, so every
-- word stays searchable ("the", "и"); Latin words go to the English stemmer, Cyrillic ones to
-- the Russian one, the other tokens (numbers and the like) stay as they are.
CREATE TEXT SEARCH DICTIONARY hqc_ru (TEMPLATE = snowball, Language = russian);
CREATE TEXT SEARCH DICTIONARY hqc_en (TEMPLATE = snowball, Language = english);
CREATE TEXT SEARCH CONFIGURATION hqc (COPY = russian);
ALTER TEXT SEARCH CONFIGURATION hqc
    ALTER MAPPING FOR asciiword, asciihword, hword_asciipart WITH hqc_en;
ALTER TEXT SEARCH CONFIGURATION hqc ALTER MAPPING FOR word, hword, hword_part WITH hqc_ru;

DROP INDEX events_search_trgm;
DROP INDEX events_search_words;
DROP INDEX sessions_title_words;
DROP INDEX sessions_title_trgm;
ALTER TABLE events DROP COLUMN search_words;
ALTER TABLE events DROP COLUMN search_text;

-- Weights rank a hit: what people and agents wrote (A) over a tool call (B) over its output
-- (D). A call's arguments are JSON, and output often is: its `\n`, `\r`, `\t` become spaces,
-- so "\nmake" is the word "make", not "nmake"; the answers to questions are the JSON array of
-- their strings. Everywhere `/` and `.` are spaces too, so a path or an address is the words
-- of its parts in a row ("search" finds "src/search.rs"); the search reads its query the same
-- way. Adding the column rewrites the table, which fills it for the existing rows.
ALTER TABLE events ADD COLUMN search_words tsvector GENERATED ALWAYS AS (
    CASE
        WHEN type IN ('user.message', 'agent.message', 'peer.message', 'system')
            THEN setweight(to_tsvector('hqc', translate(payload ->> 'text', '/.', '  ')), 'A')
        WHEN type = 'clarify.response'
            THEN setweight(to_tsvector('hqc', translate(regexp_replace(
                jsonb_path_query_array(payload -> 'answers', '$.*[*]')::text,
                '\\[nrt]', ' ', 'g'), '/.', '  ')), 'A')
        WHEN type = 'tool.call'
            THEN setweight(to_tsvector('hqc', translate((payload ->> 'tool') || ' '
                || regexp_replace(payload ->> 'args_preview', '\\[nrt]', ' ', 'g'), '/.', '  ')),
                'B')
        WHEN type = 'tool.result'
            THEN setweight(to_tsvector('hqc', translate(
                regexp_replace(payload ->> 'result_preview', '\\[nrt]', ' ', 'g'), '/.', '  ')),
                'D')
    END
) STORED;

CREATE INDEX events_search_words ON events USING gin (search_words)
    WHERE search_words IS NOT NULL;
CREATE INDEX sessions_title_words ON sessions
    USING gin (to_tsvector('hqc', translate(title, '/.', '  ')));

-- The words of the index a fuzzy query widens a word to: lexemes of letters only, 3 to 40
-- long (numbers and hashes would only be noise among the similar words).
CREATE TABLE search_vocab (word text PRIMARY KEY);
INSERT INTO search_vocab
SELECT DISTINCT w FROM events, unnest(tsvector_to_array(search_words)) w
WHERE search_words IS NOT NULL AND w ~ '^[[:alpha:]]+$' AND length(w) BETWEEN 3 AND 40;
CREATE INDEX search_vocab_trgm ON search_vocab USING gin (word gin_trgm_ops);

-- A new event's words, in order, so that concurrent inserts take the keys in the same order
-- and cannot deadlock.
CREATE FUNCTION search_vocab_add() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO search_vocab (word)
    SELECT w FROM unnest(tsvector_to_array(NEW.search_words)) w
    WHERE w ~ '^[[:alpha:]]+$' AND length(w) BETWEEN 3 AND 40
    ORDER BY w
    ON CONFLICT DO NOTHING;
    RETURN NULL;
END
$$;
CREATE TRIGGER events_search_vocab AFTER INSERT ON events
    FOR EACH ROW WHEN (NEW.search_words IS NOT NULL) EXECUTE FUNCTION search_vocab_add();
