-- SPEC 0.219 (a message taken back before the agent takes it; SPEC §3 Event, §6): when the hub
-- first handed an input to a worker (`session.input` sent). A worker keeps an input across a
-- reconnect and a hub restart, so an input that was ever handed over may still be held by one:
-- the hub alone never withdraws it. NULL: never handed, or not an input, or handed by a hub before
-- 0.219: such an input has no `payload.human` and is never taken back.
ALTER TABLE events ADD COLUMN handed_at timestamptz;
