-- SPEC 0.52 (T50, archive mode and the custom order of the session list, SPEC §3 Session, §6,
-- §17.4).

-- When the session was archived; NULL for live sessions and for those archived before this
-- migration (the client then shows no time).
ALTER TABLE sessions ADD COLUMN archived_at timestamptz;

-- The order of a project's session list for one user: the chosen mode and, for `custom`, the
-- sessions in the user's order. Ids are not foreign keys: archived and deleted sessions are
-- left out when the record is read and written.
CREATE TABLE session_orders (
    user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    project_id  uuid NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    mode        text NOT NULL DEFAULT 'state' CHECK (mode IN ('state', 'recent', 'custom')),
    session_ids uuid[] NOT NULL DEFAULT '{}',
    updated_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, project_id)
);
