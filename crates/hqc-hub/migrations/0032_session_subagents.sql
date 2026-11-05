-- SPEC 0.68: how many subagents the session's agent runs now, as its worker last reported
-- it (`session.state.subagents`); 0 while it is stopped.
ALTER TABLE sessions ADD COLUMN subagents integer NOT NULL DEFAULT 0;
