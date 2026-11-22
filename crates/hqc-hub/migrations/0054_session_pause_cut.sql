-- SPEC 0.191 (a pause stops the running turn at once; SPEC §3 Session, §17.3): the pause stopped
-- a turn that was running; the note on resume then tells the agent that the step it cut was the
-- pause's. False while the session is not paused.
ALTER TABLE sessions ADD COLUMN pause_cut_turn boolean NOT NULL DEFAULT false;
