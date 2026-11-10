-- SPEC 0.104: how many background tasks the runtime lists for the session's agent now, as its
-- worker last reported it (`session.state.background_tasks`); 0 while it is stopped.
ALTER TABLE sessions ADD COLUMN background_tasks integer NOT NULL DEFAULT 0;
