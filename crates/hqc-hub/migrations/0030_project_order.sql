-- SPEC 0.64: the order of the user's projects in the project bar. Ids are not foreign keys:
-- deleted projects are skipped when the list is read, and only the user's own are written.
ALTER TABLE users ADD COLUMN project_order uuid[] NOT NULL DEFAULT '{}';
