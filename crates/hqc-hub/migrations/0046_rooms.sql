-- SPEC 0.171 (the agents' room; SPEC §3 Session, §7.1.1).
-- A room is a session with no worker and no agent (`runtime = 'none'`) whose `room_mode` is set;
-- its agents are sessions whose `room_id` is the room, deleted by the hub with the room.
ALTER TABLE sessions
    ADD COLUMN room_mode text CHECK (room_mode IN ('conversation', 'council')),
    ADD COLUMN room_id uuid REFERENCES sessions (id) ON DELETE SET NULL;
