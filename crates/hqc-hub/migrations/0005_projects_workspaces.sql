-- SPEC 0.16 (T14): projects and several workspaces per project. A workspace is a hub object
-- {project_id, worker_id, path, label}; the worker no longer announces one in
-- `hello.capabilities.workspace_path`. Iteration 1 data is carried over from the copy of the
-- worker's last `hello` kept in `workers.capabilities`.

-- A folder belongs to one project: on one worker a path is unique (nesting is checked by the
-- hub when a workspace is added, SPEC §3 Workspace).
ALTER TABLE workspaces DROP CONSTRAINT workspaces_project_id_worker_id_path_key;

-- 1. The iteration 1 workspace of every worker that announced one, in its owner's personal
-- project (the hub created it on `hello`; this covers a row that is missing).
INSERT INTO workspaces (id, project_id, owner_id, worker_id, path)
SELECT gen_random_uuid(), u.personal_project_id, w.owner_id, w.id, w.capabilities->>'workspace_path'
FROM workers w
JOIN users u ON u.id = w.owner_id
WHERE coalesce(w.capabilities->>'workspace_path', '') <> ''
  AND u.personal_project_id IS NOT NULL
  AND NOT EXISTS (
      SELECT 1 FROM workspaces x
      WHERE x.worker_id = w.id AND x.path = w.capabilities->>'workspace_path');

-- 2. Sessions without a workspace go to the announced workspace of their worker.
UPDATE sessions s SET workspace_id = x.id
FROM workers w, workspaces x
WHERE s.workspace_id IS NULL
  AND s.worker_id = w.id
  AND x.worker_id = w.id
  AND x.project_id = s.project_id
  AND x.path = w.capabilities->>'workspace_path';

-- 3. Workspaces of earlier `hello`s (the configured path changed) that no session uses.
DELETE FROM workspaces x
WHERE NOT EXISTS (SELECT 1 FROM sessions s WHERE s.workspace_id = x.id)
  AND x.path IS DISTINCT FROM (
      SELECT w.capabilities->>'workspace_path' FROM workers w WHERE w.id = x.worker_id);

ALTER TABLE workspaces ADD CONSTRAINT workspaces_worker_path_key UNIQUE (worker_id, path);
CREATE INDEX workspaces_project ON workspaces (project_id);
CREATE INDEX sessions_workspace ON sessions (workspace_id);

-- 4. Sessions whose folder is unknown cannot run any more: archived (SPEC §17.4) with a
-- `system` event saying why; their pending input fails, open requests are closed.
CREATE TEMPORARY TABLE t14_lost ON COMMIT DROP AS
SELECT id FROM sessions WHERE workspace_id IS NULL;

UPDATE events SET delivery = 'failed'
WHERE delivery = 'pending' AND session_id IN (SELECT id FROM t14_lost);

UPDATE pending_requests SET closed_at = now(), updated_at = now()
WHERE closed_at IS NULL AND session_id IN (SELECT id FROM t14_lost);

WITH lost AS (
    UPDATE sessions SET archived = true, turn_open = false, last_seq = last_seq + 1,
        updated_at = now()
    WHERE id IN (SELECT id FROM t14_lost)
    RETURNING id, last_seq
)
INSERT INTO events (event_id, session_id, seq, ts, author, type, payload)
SELECT gen_random_uuid(), id, last_seq, now(), '{"kind": "system"}', 'system',
    jsonb_build_object(
        'code', 'workspace_unknown',
        'text', 'Archived on upgrade: the session has no workspace and its worker never '
             || 'announced one. Add a workspace to the project and start a new session.')
FROM lost;

-- 5. The first workspace of a project is its default (SPEC §3 Project, §13.4).
UPDATE projects p SET default_workspace_id = (
    SELECT x.id FROM workspaces x WHERE x.project_id = p.id ORDER BY x.created_at, x.id LIMIT 1)
WHERE p.default_workspace_id IS NULL;

ALTER TABLE projects
    ADD CONSTRAINT projects_default_workspace_fk
    FOREIGN KEY (default_workspace_id) REFERENCES workspaces (id) ON DELETE SET NULL;
