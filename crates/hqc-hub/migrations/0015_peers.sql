-- SPEC 0.34 (T32, `hqc_send` and `hqc_ask`, SPEC §7.1).
-- The send buffer: `peer.message`s with `mode: send` to a session that does not wake on
-- them. They go to the agent in `peer_prefix` of the session's next `session.input`
-- (`carried_by`: the input whose frame carries them) and leave the buffer with that input's
-- `ack`, or as the oldest beyond the limits (50 messages / 64 KB).
CREATE TABLE peer_buffer (
    event_id   uuid PRIMARY KEY REFERENCES events (event_id) ON DELETE CASCADE,
    session_id uuid NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    seq        bigint NOT NULL,
    bytes      integer NOT NULL,
    carried_by uuid
);
CREATE INDEX peer_buffer_session ON peer_buffer (session_id, seq);

-- An open `hqc_ask` is closed by the target's final `agent.message` whose `in_reply_to` is
-- the `peer.message` it delivered (`payload.event_id`).
CREATE INDEX pending_requests_open_asks ON pending_requests ((payload ->> 'event_id'))
    WHERE kind = 'ask' AND closed_at IS NULL;
