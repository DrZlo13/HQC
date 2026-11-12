-- SPEC 0.129 (Allow all, a third permission mode; SPEC §3 Session, §7.1).

-- `allow_all`: Claude Code's `bypassPermissions`, for the mode hqc asks for and the one the
-- worker reports.
ALTER TABLE sessions DROP CONSTRAINT sessions_permission_mode_check;
ALTER TABLE sessions ADD CONSTRAINT sessions_permission_mode_check
    CHECK (permission_mode IN ('ask', 'auto', 'allow_all'));
ALTER TABLE sessions DROP CONSTRAINT sessions_effective_permission_mode_check;
ALTER TABLE sessions ADD CONSTRAINT sessions_effective_permission_mode_check
    CHECK (effective_permission_mode IN ('ask', 'auto', 'allow_all'));

-- The cap the buffered messages put on the input (T40): the lowest of theirs, `ask` or, since
-- an `auto` cap holds an `allow_all` session down, `auto`. Rows from before are `ask` caps.
ALTER TABLE input_caps ADD COLUMN permission_cap text NOT NULL DEFAULT 'ask'
    CHECK (permission_cap IN ('ask', 'auto'));
