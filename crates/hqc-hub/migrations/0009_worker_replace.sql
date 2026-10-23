-- SPEC 0.21 (T19): enrollment that replaces an existing worker (SPEC §5.1), revoked
-- workers out of the «Воркеры» group (SPEC §10.2).

-- The name the replaced worker had, set when a code is confirmed with
-- `replace_worker_id` (then `worker_id` is the replaced worker); the poll reports it.
ALTER TABLE enroll_codes ADD COLUMN replaced_name text;

-- Revoking now archives the worker's service session; workers revoked before get the same.
UPDATE sessions s SET archived = true, updated_at = now()
FROM workers w
WHERE s.id = w.system_session_id AND w.revoked_at IS NOT NULL AND NOT s.archived;
