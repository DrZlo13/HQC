-- SPEC 0.156 (a fork is quick): the events a fork copies get their search words after the fork
-- has answered, from the hub in the background, and add nothing to `search_vocab`, whose words
-- they already are. Copying 14 545 events of a long session took 9-12 s, most of it the
-- vocabulary trigger and the words of each row.
--
-- The words of an event, as the generated column of 0037 computed them.
CREATE FUNCTION event_search_words(type text, payload jsonb) RETURNS tsvector
    LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
    SELECT CASE
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
$$;

-- A plain column, so a copy can go in without its words; the values stay, no rewrite.
ALTER TABLE events ALTER COLUMN search_words DROP EXPRESSION;

-- Every other event gets its words as it is written or changed, as before. A copy (a fork's,
-- `origin_id` set; only the fork's copy sets it) goes in without them: no words, no entry in
-- the index, and the vocabulary trigger, which needs words, skips it.
CREATE FUNCTION events_search_words() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' OR NEW.origin_id IS NULL THEN
        NEW.search_words := event_search_words(NEW.type, NEW.payload);
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER events_search_words BEFORE INSERT OR UPDATE OF type, payload ON events
    FOR EACH ROW EXECUTE FUNCTION events_search_words();
