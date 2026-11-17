-- SPEC 0.160 (a session is paused and resumed; SPEC §3 Session, §17.3): when the session was
-- paused, then, once the hub itself stops its agent for the pause, when it did; null while it is
-- not paused.
ALTER TABLE sessions ADD COLUMN paused_at timestamptz;
