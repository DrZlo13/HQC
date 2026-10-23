-- SPEC 0.17 (T17): every worker has a service session (SPEC §10.2) in its owner's personal
-- project: no workspace, no agent (`runtime = 'none'`), no input, never archived. Requests
-- of the worker outside sessions (`worker.approval.request`) and its warn/error log lines
-- are written there.

ALTER TABLE sessions ADD COLUMN system boolean NOT NULL DEFAULT false;
ALTER TABLE workers ADD COLUMN system_session_id uuid REFERENCES sessions (id) ON DELETE SET NULL;

-- Workers registered before T17 get theirs now (revoked ones too: their history stays).
CREATE TEMPORARY TABLE t17_system ON COMMIT DROP AS
SELECT w.id AS worker_id, gen_random_uuid() AS session_id, w.owner_id, w.name,
       u.personal_project_id AS project_id
FROM workers w
JOIN users u ON u.id = w.owner_id
WHERE w.system_session_id IS NULL AND u.personal_project_id IS NOT NULL;

INSERT INTO sessions (id, project_id, owner_id, worker_id, title, title_source, slug, runtime,
                      system)
SELECT session_id, project_id, owner_id, worker_id, name, 'user', 'worker-' || worker_id,
       'none', true
FROM t17_system;

UPDATE workers w SET system_session_id = t.session_id
FROM t17_system t WHERE w.id = t.worker_id;

CREATE UNIQUE INDEX sessions_system_worker ON sessions (worker_id) WHERE system;
