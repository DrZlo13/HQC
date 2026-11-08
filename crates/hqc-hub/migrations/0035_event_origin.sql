-- SPEC 0.82: an event a fork copied keeps where it came from. `origin_id` is the event id the
-- runtime's record gave it first (SPEC §3: derived from the transcript record), `origin_runtime`
-- the transcript that record was written in. A fork of a fork cuts by them: the copy's own id is
-- in no transcript. NULL: the event is not a copy.
ALTER TABLE events ADD COLUMN origin_id uuid, ADD COLUMN origin_runtime text;
