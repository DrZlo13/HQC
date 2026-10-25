-- SPEC 0.39 (T36, hardening of the MCP surface, SPEC §7.1, §16, §20).

-- An input whose `peer_prefix` was taken when it was first sent: a later send of the same
-- input (after a reconnect) carries exactly the messages carried by it then, never newer
-- ones, so the input's `ack` cannot clear a message the agent never saw.
CREATE TABLE input_prefix (
    event_id uuid PRIMARY KEY REFERENCES events (event_id) ON DELETE CASCADE
);

-- Peer messages are counted per chain (SPEC §20: 50 per chain).
CREATE INDEX events_peer_chain ON events ((chain ->> 'chain_id'))
    WHERE type = 'peer.message';

-- A schedule an agent created (`hqc_schedule`) keeps the chain of the call, moved on like a
-- send; its runs carry it, so a turn started by the schedule continues that chain instead of
-- starting a human's one. NULL for schedules of the API (a person's).
ALTER TABLE scheduled_messages ADD COLUMN chain jsonb;
