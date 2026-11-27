-- SPEC 0.265: what a worktree's new branch starts from (a branch, a tag or a commit the worker
-- resolves) instead of the origin's HEAD; repeated in `workspace.create` after `hello` while the
-- worktree is `creating`. NULL: the origin's HEAD, as before.
ALTER TABLE workspaces
    ADD COLUMN base text;
