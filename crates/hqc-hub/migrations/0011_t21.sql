-- SPEC 0.23 (T21).
-- The mirror of Claude Code sessions is on for every workspace by default (SPEC §9.2); the
-- user switches it with PATCH /api/workspaces/{id}.
ALTER TABLE workspaces ALTER COLUMN mirror_enabled SET DEFAULT true;
UPDATE workspaces SET mirror_enabled = true WHERE NOT mirror_enabled;

-- Machine fingerprint (SPEC §5.1): hex sha256 of a stable machine id with the salt `hqc`, from
-- `POST /worker/enroll` and every `hello`. Replaceable workers of the same machine come first.
ALTER TABLE workers ADD COLUMN machine_hash text;
ALTER TABLE enroll_codes ADD COLUMN machine_hash text;
