-- SPEC 0.23 (T22, hardening before going public, SPEC §20).

-- Sessions the mirror created from a transcript (SPEC §9.1): a worker may create at most so
-- many per day and per workspace. Sessions imported before this migration are not marked;
-- the per-day window is long gone for them, the per-workspace total misses them.
ALTER TABLE sessions ADD COLUMN imported boolean NOT NULL DEFAULT false;
CREATE INDEX sessions_imported ON sessions (worker_id, created_at) WHERE imported;

-- The turn watchdog (SPEC §17.3) looks only at sessions with an open turn.
CREATE INDEX sessions_turn_open ON sessions (id) WHERE turn_open;

-- `locked_by: vscode` without a time counts as written just now for takeover (SPEC §9.3);
-- old rows get one, so the 30 s of silence start from the upgrade instead of never.
UPDATE sessions SET vscode_active_at = now()
 WHERE locked_by = 'vscode' AND vscode_active_at IS NULL;
