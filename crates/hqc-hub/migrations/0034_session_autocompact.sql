-- A session's auto-compaction (`session.start.autocompact`): null — the runtime's default,
-- 0 — off, otherwise the context window in tokens the runtime compacts in.
ALTER TABLE sessions ADD COLUMN autocompact integer
    CHECK (autocompact = 0 OR autocompact BETWEEN 100000 AND 1000000);
