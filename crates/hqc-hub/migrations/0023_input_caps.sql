-- SPEC 0.44 (T40, the cap for buffered sends and old schedules, SPEC §7.1, §16).

-- An input that carried, in `peer_prefix`, a buffered `peer.message` with an `ask` cap: its
-- turn runs capped although its own chain has no `ask` cap (a person's input has none).
-- `capped_by`: the session that set the cap of the first such message. Written when the
-- send buffer is first handed to the input, kept after its `ack` for the header and for
-- the messages its turn sends.
CREATE TABLE input_caps (
    event_id  uuid PRIMARY KEY REFERENCES events (event_id) ON DELETE CASCADE,
    capped_by text NOT NULL
);

-- The session whose agent made the schedule (`hqc_schedule`); each run is capped by its
-- mode at run time. NULL for a person's schedule (no `chain`), and for an agent's schedule
-- whose creator was deleted or that was made before 0.44: its runs count it as `ask`.
ALTER TABLE scheduled_messages
    ADD COLUMN created_by uuid REFERENCES sessions (id) ON DELETE SET NULL;
