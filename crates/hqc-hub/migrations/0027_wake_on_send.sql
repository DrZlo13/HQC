-- SPEC 0.55 (T52): a send wakes its target by default, existing sessions included.
ALTER TABLE sessions ALTER COLUMN wake_on_send SET DEFAULT true;
UPDATE sessions SET wake_on_send = true;
