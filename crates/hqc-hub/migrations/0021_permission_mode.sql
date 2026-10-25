-- SPEC 0.42 (T38, a permission mode per session, SPEC §3 Session, §5.3, §5.4).

-- The mode hqc asks the worker for: null — the worker's `[permissions] mode`.
ALTER TABLE sessions ADD COLUMN permission_mode text
    CHECK (permission_mode IN ('ask', 'auto'));
-- The mode the worker last reported the agent runs in (`session.state.permission_mode`).
ALTER TABLE sessions ADD COLUMN effective_permission_mode text
    CHECK (effective_permission_mode IN ('ask', 'auto'));
