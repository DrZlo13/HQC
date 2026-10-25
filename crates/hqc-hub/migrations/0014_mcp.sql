-- SPEC 0.33 (T30, the hqc MCP pipe, SPEC §5.2, §7.2).
-- `mcp.call` names its session by the token of a `session.start`. The hub issues a new token
-- with every start, also to an agent that keeps running (a reconnect), and the worker sends
-- the latest one; a call already on its way with the previous token must still pass, so the
-- token before the latest is kept as well.
ALTER TABLE sessions ADD COLUMN prev_session_token_hash text;
CREATE INDEX sessions_token_hash ON sessions (session_token_hash)
    WHERE session_token_hash IS NOT NULL;
CREATE INDEX sessions_prev_token_hash ON sessions (prev_session_token_hash)
    WHERE prev_session_token_hash IS NOT NULL;
