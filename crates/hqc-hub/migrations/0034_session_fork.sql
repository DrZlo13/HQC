-- SPEC 0.80: a session forked from another one before a message of the user. `fork` is what
-- `session.start.fork` sends ({runtime_session_id, at_event_ids}); the worker forks the
-- conversation at the agent's first start. NULL: no fork.
ALTER TABLE sessions ADD COLUMN fork jsonb;
