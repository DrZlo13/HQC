-- SPEC 0.51 (T49, the app theme and project appearance, SPEC §3 User, §3 Project, §6).

-- The base palette of the client, the same on every device of the user.
ALTER TABLE users
    ADD COLUMN theme text NOT NULL DEFAULT 'warm'
        CHECK (theme IN ('warm', 'graphite', 'contrast'));

-- NULL color: the API picks one by a hash of the id. NULL icon: the first letters of the
-- name. `icon` is {kind: emoji|letters, value} or {kind: image, v} with `v` the start of
-- the image's sha256 (the cache key in its URL).
ALTER TABLE projects
    ADD COLUMN color text
        CHECK (color IN ('teal', 'plum', 'blue', 'rose', 'moss', 'coral', 'cyan', 'slate')),
    ADD COLUMN icon jsonb;

-- The uploaded icon image: one per project, at most 256 KB, checked by its magic bytes.
-- Not an artifact: it is not in any session, the daily cleanup must not touch it and it
-- goes with the project.
CREATE TABLE project_icons (
    project_id uuid PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    mime       text NOT NULL,
    data       bytea NOT NULL,
    sha256     text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);
