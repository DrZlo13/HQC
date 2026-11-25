-- SPEC 0.237 (iteration 8): the skills an owner keeps on the hub. A session's agent gets the
-- skills of its project in a plugin folder its worker builds (`--plugin-dir`); the worker
-- fetches each archive once per hash (`GET /worker/skills/{sha256}`).
CREATE TABLE skills (
    id uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- The folder the CLI names it by (`hqc:<name>`).
    name text NOT NULL,
    -- From its SKILL.md, for the list in Settings.
    description text,
    allowed_tools text[] NOT NULL DEFAULT '{}',
    -- The archive the hub wrote (paths sorted, fixed times) and its sha256 in hex.
    archive bytea NOT NULL,
    sha256 text NOT NULL,
    -- Unpacked bytes and the number of files.
    size bigint NOT NULL,
    files integer NOT NULL,
    all_projects boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (owner_id, name)
);
CREATE INDEX skills_owner_sha256 ON skills (owner_id, sha256);

CREATE TABLE skill_projects (
    skill_id uuid NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    PRIMARY KEY (skill_id, project_id)
);
CREATE INDEX skill_projects_project ON skill_projects (project_id);
