-- SPEC 0.147 (iteration 6, secrets in a session): the worker's worker.toml has
-- session_secrets = true, from its hello. The hub keeps no secret: it only passes them on.
ALTER TABLE workers ADD COLUMN session_secrets boolean NOT NULL DEFAULT false;
