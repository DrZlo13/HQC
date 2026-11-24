-- SPEC 0.223 (the agent's task list, SPEC §4, §6): a subscriber past the latest `tasks` event of
-- a session gets that event once more, so the hub looks for the newest one of the session; the
-- index holds only these events and stays small.
CREATE INDEX events_tasks ON events (session_id, seq) WHERE type = 'tasks';
