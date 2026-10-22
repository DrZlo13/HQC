-- SPEC 0.11. `boot_id` from the worker's latest `hello`: only a change means the worker
-- restarted (SPEC §5.2, §17.3). `start_sent`: `session.start` has been sent for the session
-- at least once, so the next one carries `mode: resume` (SPEC §5.3).
ALTER TABLE workers ADD COLUMN boot_id uuid;
ALTER TABLE sessions ADD COLUMN start_sent boolean NOT NULL DEFAULT false;
