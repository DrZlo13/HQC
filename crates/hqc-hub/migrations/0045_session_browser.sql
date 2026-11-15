-- SPEC 0.148 (iteration 6, the browser in a session): the owner gave the session's agent their
-- Chrome; the worker's worker.toml has browser = true, from its hello.
ALTER TABLE sessions ADD COLUMN browser boolean NOT NULL DEFAULT false;
ALTER TABLE workers ADD COLUMN browser boolean NOT NULL DEFAULT false;
