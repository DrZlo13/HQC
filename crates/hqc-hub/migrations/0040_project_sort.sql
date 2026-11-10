-- The project bar's order (SPEC §3 User): `custom` (the user's own, `project_order`), `name` or
-- `recent`, the same on every device of the user.
ALTER TABLE users
    ADD COLUMN project_sort text NOT NULL DEFAULT 'custom'
        CHECK (project_sort IN ('custom', 'name', 'recent'));
