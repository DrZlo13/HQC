-- SPEC 0.19 (T15, mirror and takeover of Claude Code sessions, SPEC §9; merged after T16/T17).
-- When the hub last received a mirrored event the worker marked `recent` (the transcript
-- was written within 30 s): takeover needs 30 s of silence after it (SPEC §9.3).
ALTER TABLE sessions ADD COLUMN vscode_active_at timestamptz;

-- `mirror.event` finds its session by the worker and the transcript's uuid.
CREATE INDEX sessions_worker_runtime ON sessions (worker_id, runtime_session_id);
