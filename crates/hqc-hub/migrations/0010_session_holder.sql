-- SPEC 0.22 (T20, takeover without forks, SPEC §9.3).
-- A live Claude Code process outside hqc that holds the session's transcript open, as the
-- worker read it from Claude Code's registry of live sessions: `{entrypoint, status}`, and
-- since when it holds it. While set the session is `locked_by: vscode` and takeover is
-- refused (`409 vscode_open`).
ALTER TABLE sessions ADD COLUMN held_by jsonb, ADD COLUMN vscode_held_at timestamptz;
