# T46: decisions

Worktree workspaces, SPEC 0.59. Built on T45's flow: the same `workspace.create` with `kind:
worktree`, the same `creating` → `ready` | `failed` states, the resend after `hello`, the fail on
a new `boot_id`, `workspace.inspect` and the deletion dialog.

## Protocol

- **`workspace.create {kind: worktree, path, origin, branch, new_branch?}`.** `origin` is the
  origin's canonical path, not its id: the worker knows folders, not hub records.
- **The hub sends the origin's `workspace.rules` right before every worktree `workspace.create`**
  (also when it repeats it after `hello`). The worker accepts an origin only if it is a workspace
  it knows, and a `dir` workspace added since the last `hello` with no session yet is not known
  to it: `workspace.rules` is the existing frame that makes it known (same trust as a
  `session.start`: a folder inside `allowed_roots` the hub calls a workspace), so no new frame.
- **`workspace.delete` gets `force`** (a struct of its own, `WorkspaceDelete`; `workspace.inspect`
  keeps `WorkspacePath`). It only changes a worktree's removal; a clone's folder is deleted
  either way, as in T45.
- **No base for a new branch.** Neither the task nor the spec has one: a new branch is made from
  the origin's `HEAD`.
- A worker before 0.59 refuses the frame (`unknown variant worktree`), which fails the
  workspace with that message (T45's rule).

## Worker

- **Origin checks:** in `known` (folders from `session.start`/`workspace.rules`,
  `known_workspaces.json`) or in its own `created_workspaces.json` and not still being created;
  canonicalizes to itself inside `allowed_roots`; `git rev-parse --show-toplevel` is the origin
  itself (so a subfolder of a repository is refused). A worktree as origin is not refused by the
  worker (git allows it); the hub refuses it.
- **Branch checks** (in the origin's environment, read-only `git` calls before the folder is
  made): `check_branch`, then `git check-ref-format --branch`.
  - `new_branch`: the branch must not exist (`-b <branch> ... HEAD`).
  - existing: a local branch that `git worktree list --porcelain` does not show checked out
    anywhere ("the branch X is already checked out at <path>"; git's own message would also
    refuse, ours is clear and does not depend on git's wording), or, with no local branch, a
    branch of exactly one remote, passed explicitly as `--track -b <branch> ...
    refs/remotes/<remote>/<branch>` so git cannot fall back to a tag or a commit of that name
    and detach. Several remotes or none: refused with what to do.
- **`git worktree add` runs in the origin with the environment of the new path** (its `env`,
  and an ssh-agent with its keys like a clone, as asked, although the command needs no
  network), into the empty folder the worker created (git accepts an existing empty folder). Same
  prompt rules, process group and `clone_timeout` as a clone; `git_clone` became `git_run`, one
  runner for both.
- **The list entry keeps the origin** (`origin` in `created_workspaces.json`). Deletion runs `git
  worktree remove [--force] -- <path>` in that recorded origin, never in one the hub names, then
  `git worktree prune`; the branch is never deleted. git itself refuses a path that is not a
  worktree of that repository. A worktree folder already gone: pruned, `removed: false`. A failed
  or unfinished (restart) worktree: the folder is removed and the origin pruned.
- **Inspection of a linked worktree** (`.git` is a file) lists unpushed commits of its own
  checked-out branch only; the repository's other branches are shared with the origin and would
  be noise in the dialog (none when `HEAD` is detached).
- **Residual risk, written into §14:** a worktree shares the origin's `.git` (config, hooks,
  filters). If the origin and the worktree are bound to different environments, what a session
  plants in one can run in the other's environment (hooks or filters of `git worktree add`, or of
  any later git there). Making a folder a worktree of another does not separate environments. Hooks
  are not disabled for `git worktree add` (git-lfs and similar rely on `post-checkout`).
- Known edge: if `git worktree add` is killed by the timeout, git may leave its admin entry
  locked ("initializing"), which `prune` skips; a later worktree of the same folder name gets a
  suffixed admin name. Harmless; `git worktree prune` after `git worktree unlock` cleans it.

## Hub

- **Origin rules** (`400 invalid_origin`, also for a missing `origin_workspace_id`): the user's
  workspace, same project and worker as the request's `project_id` and `worker_id` (both still
  required: they tell which project and worker the client means and are checked against the
  origin), `ready`, and **not itself a worktree** (decided here: one level only; git would allow a
  worktree of a worktree, but the client tree stays two levels and "delete the worktrees first"
  stays simple). Whether the origin is a git repository only the worker can tell; a plain folder
  gives a `failed` worktree with the worker's reason.
- **Default path** `<origin-path>-<branch with / as ->`, then the same checks as a clone (T45's
  path code became `new_folder`, shared). `path` may be omitted in the API (T47 will use that);
  the client shows the default and sends a path only when the user edited it.
- **Migration `0029`**: `origin_workspace_id uuid REFERENCES workspaces (id)` (no action on
  delete: checked at the end of the statement, so deleting a project or a worker as a whole, one
  statement, takes origin and worktrees together), `new_branch`, and `clone_branch` renamed to
  `branch` (one column for the clone's optional branch and the worktree's branch; the workspace
  object shows it as `branch`).
- **`409 workspace_in_use` for an origin with worktree records in any state**, checked before
  `remove_folder` asks the worker (so the folder is never deleted while the record would stay)
  and again inside the delete transaction, which now takes the per-worker advisory lock that new
  workspaces take. A failed worktree record also holds its origin until it is removed.
- `workspace_creating` and `workspace_not_ready` messages say "making" instead of "cloning".

## Client

- "New worktree" on every ready workspace that is not a worktree: the client cannot tell a git
  folder; the worker's refusal shows on the new row. The form opens under the row (round 2 card,
  the clone form's field layout): branch, "New, from the current commit" / "Existing branch",
  the folder (the default, following the branch until edited). No folder tree: the default is
  next to the origin and the field is editable.
- Worktrees are listed right under their origin, indented with an accent border, with their
  branch as a tag; "Adding worktree…" / "Worktree failed" on the row like a clone's.
- The dialog of a worktree says it goes with `git worktree remove` and the branch and its
  commits stay; "Delete folder" sends `force=true` only when the dialog listed uncommitted files
  (the user saw them). The dialog is now keyed by the workspace: closing one and opening another
  in the same render showed the previous inspection.
- The hub's `workspace_in_use` message is shown as is (it says what to do for sessions and for
  worktrees); the client's "Archive these sessions first." suffix repeated the hub's text and was
  wrong for worktrees.

## Left for the hand check (iteration criterion)

Checked in tests: a worktree made through the API on the fake worker (`tests/t46.rs`), the
worker's add/remove on real git repositories in `tmp/` (`workspaces::tests`,
`tests/worker.rs::worktree_through_frames`), the client flow on the mock hub. Not checked here,
needs the phone and the running worker: make a worktree of a project's workspace from the phone,
start a session in it whose environment (a binding of the worktree's folder with `ssh_keys`)
reaches one ssh host other sessions are refused, let it commit on its branch, and delete the
worktree from the phone, seeing the unpushed commit in the dialog before the folder goes.
