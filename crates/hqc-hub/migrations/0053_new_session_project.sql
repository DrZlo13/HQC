-- SPEC 0.191 (the project a phone's floating button opens a new session in; SPEC §3 User): null
-- until the user picks one, and again when the picked project is deleted.
ALTER TABLE users
    ADD COLUMN new_session_project_id uuid REFERENCES projects (id) ON DELETE SET NULL;
