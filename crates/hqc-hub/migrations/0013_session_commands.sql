-- SPEC 0.28 (T27, slash command completion, SPEC §5.4, §6).
-- The slash commands of the session's runtime as the worker last reported them in
-- `session.state.commands` (Claude Code: `slash_commands` of `system/init`): a JSON array of
-- names without `/`. NULL until a process of the session has reported a list.
ALTER TABLE sessions ADD COLUMN commands jsonb;
