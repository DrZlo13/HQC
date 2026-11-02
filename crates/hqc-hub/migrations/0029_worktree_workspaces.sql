-- SPEC 0.59 (T46): worktree workspaces. A worktree names its origin, a git workspace of the
-- same project and worker; an origin is not deleted while worktrees of it exist (the hub
-- answers `409 workspace_in_use`; the key, checked at the end of the statement, keeps a
-- project or worker deleted as a whole deletable). `branch` is the clone's optional branch
-- (T45's `clone_branch`) or the worktree's branch; `new_branch` says the worktree creates it.
-- Both go into `workspace.create`, which the hub repeats after every `hello` while the
-- workspace is `creating`.
ALTER TABLE workspaces RENAME COLUMN clone_branch TO branch;

ALTER TABLE workspaces
    ADD COLUMN origin_workspace_id uuid REFERENCES workspaces (id),
    ADD COLUMN new_branch boolean NOT NULL DEFAULT false;

CREATE INDEX workspaces_origin ON workspaces (origin_workspace_id)
    WHERE origin_workspace_id IS NOT NULL;
