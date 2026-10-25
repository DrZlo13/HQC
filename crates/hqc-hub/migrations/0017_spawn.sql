-- SPEC 0.36 (T34, `hqc_spawn`, SPEC §3 Session, §7).
-- A session started by an agent's `hqc_spawn` records the spawning session (`spawned_by`,
-- cleared when that session is deleted; the client links to it) and the chain of the call
-- (`spawn_chain_id`, kept: the limits of SPEC §20 count spawned sessions per chain and per
-- owner per hour).
ALTER TABLE sessions
    ADD COLUMN spawned_by uuid REFERENCES sessions (id) ON DELETE SET NULL,
    ADD COLUMN spawn_chain_id uuid;
CREATE INDEX sessions_spawn_chain ON sessions (spawn_chain_id)
    WHERE spawn_chain_id IS NOT NULL;
CREATE INDEX sessions_spawned_recently ON sessions (owner_id, created_at)
    WHERE spawn_chain_id IS NOT NULL;
