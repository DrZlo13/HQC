-- SPEC 0.15. Who set the session title: `default` (the hub, «Сессия N»), `runtime`
-- (`session.state.title`, e.g. Claude Code's ai-title) or `user` (PATCH). A runtime title
-- never replaces a user one (SPEC §3 Session).
ALTER TABLE sessions ADD COLUMN title_source text NOT NULL DEFAULT 'default';
