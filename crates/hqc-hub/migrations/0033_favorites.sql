-- SPEC 0.74: a person's favorite sessions and projects, since when. Per user and apart from
-- the session row: a session may be shared with other users, and a write to `sessions`
-- moves its `updated_at`.
CREATE TABLE session_favorites (
    user_id    uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    session_id uuid NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, session_id)
);

CREATE TABLE project_favorites (
    user_id    uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    project_id uuid NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, project_id)
);
