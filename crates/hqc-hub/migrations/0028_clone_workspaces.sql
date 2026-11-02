-- SPEC 0.58 (T45): workspaces hqc clones. `state` is `creating` until the worker answers
-- `workspace.created`, then `ready`, or `failed` with the worker's `error`. `clone_url` and
-- `clone_branch` go into `workspace.create`, which the hub repeats after every `hello` while
-- the workspace is `creating`. Existing workspaces are folders that exist: `ready`.
ALTER TABLE workspaces
    ADD COLUMN state text NOT NULL DEFAULT 'ready'
        CHECK (state IN ('creating', 'ready', 'failed')),
    ADD COLUMN error text,
    ADD COLUMN clone_url text,
    ADD COLUMN clone_branch text;
