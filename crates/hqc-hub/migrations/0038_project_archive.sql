-- A project put away from the project bar (SPEC §3 Project): since when; NULL while it is not.
-- Its sessions stay as they are.
ALTER TABLE projects ADD COLUMN archived_at timestamptz;
