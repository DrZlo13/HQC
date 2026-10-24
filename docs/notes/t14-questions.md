# T14 — projects and multiple workspaces: decisions, questions, what was not checked

Task: remove the iteration 1 simplification "one workspace from the worker config". SPEC 0.16
(§3, §4, §5.3, §5.4, §6, §13, §14, §19, §23); task: `docs/ITERATION-2.md`. Live claude runs:
zero.

## Decisions made along the way (not covered by the task statement)

- **The old `[workspace]` block in `worker.toml` does not break startup.** The task says "the
  block is removed"; with a strict config (`deny_unknown_fields`) the home-mac worker would
  simply fail to start after the update. The block is read and ignored; at startup a warning
  says "remove it". It never imposed restrictions (the boundary is `allowed_roots`), so
  tolerating it weakens nothing. If a hard refusal is wanted, it is one line in `config.rs`.
- **`fs.stat` outside `allowed_roots` reveals nothing about the path**: `{exists: false,
  is_dir: false, inside_allowed_roots: false}` without `canonical`, including for a symlink
  from inside the roots to the outside. Otherwise a compromised hub could probe whether any
  path exists on the machine. For a nonexistent path, "inside" is decided by the nearest
  existing ancestor, resolved through symlinks.
- **The hub does not accept a "canonical" path with `.`/`..`** (`400 invalid_path`): the
  nesting check compares path components and needs an already resolved path.
- **Deleting a workspace with live sessions is `409 workspace_in_use`.** Sessions have an FK
  `ON DELETE SET NULL`, and a session without a workspace does not start; silently breaking
  live sessions is worse than asking to archive them. Archived ones lose `workspace_id`;
  unarchiving such a session is `409 no_workspace`.
- **Deleting a project cascades**: workspace records and sessions with their events,
  `session.kill` to the agents; the client asks for confirmation showing the number of
  sessions. The alternative is `409` while the project has sessions; I chose the cascade
  because the client has no way to delete sessions one by one.
- **The personal project cannot be renamed or deleted through the API either**
  (`409 personal_project`), not only in the UI; its description and `default_workspace_id`
  can be changed.
- **`worker_id` without `workspace_id`** when creating a session: the project's workspace on
  that worker (the default one if it is there, otherwise the earliest). The iteration 1 rule
  "default worker" (`409 no_worker`) is removed entirely: with workspaces it is not needed,
  and protection against a "phishing" worker comes from the fact that a new worker has no
  workspace until the user adds one. The test `a_fresh_worker_is_not_the_default` was
  rewritten accordingly.
- **There is no owner confirmation through the service session (§13.1) for `dir`**: only the
  worker's owner can add a workspace on it (a worker has no other users until iteration 6).
  When grants appear, the §13.1 check is needed here.
- **The timeout for a worker's answer to `fs.*` is 10 s, a constant** (`504 worker_timeout`),
  not in the hub config. If the worker answers `error`, it is `400 invalid_path` with its text.
- **`result` correlation**: `request_id` equals the frame's `id`; the hub looks up the pending
  request by the reply frame's `id`, then by `request_id`. When the connection drops, pending
  requests get `409 worker_offline` immediately rather than a timeout.
- **New ids in the migration are `gen_random_uuid()` (v4)**, not v7: Postgres 17 has no
  `uuidv7()`. Time ordering of these records does not matter for anything.
- **Client**: the personal project has no "Rename" and "Delete project" buttons; a workspace
  label can be set when adding it, there is no label editing in the UI (the API has `PATCH`).
  Folder tree: clicking a folder enters it, "Choose this folder" fills in the path, a double
  click on a folder fills it in too; "↑ Up" from a root leads to the list of roots.

## Migration 0005: what it does and does not do

- For each worker with `capabilities.workspace_path` in its last `hello` it creates (if not
  there yet) a workspace in the owner's personal project; sessions on that worker without
  `workspace_id` are bound to it; the project's first workspace becomes the default.
- Workspaces from earlier `hello`s (the path in the config changed) that no session refers to
  are deleted. Those that sessions refer to **stay** — even if they are nested in the current
  one or contain it. The invariant "paths on a worker are not nested" may be violated for such
  historical records; new paths are checked against all records, so there will be no new
  violations. **Question:** merge such records (sessions move to another path, and `--resume`
  of their transcripts will no longer find the jsonl) or leave them as is?
- Sessions whose folder is unknown (the worker never reported `workspace_path`, or the worker
  is gone) are archived with `system workspace_unknown`. Their `pending` inputs become
  `failed`, open `pending_requests` are closed — **without** `approval.cancel` /
  `clarify.cancel` events and without frames to the client (the migration runs before the hub
  starts). If a client has a card open, after a reload it stays in the archived session
  unanswered.
- The migration was not run on a real database (the production hub, `hqc_dev`): it was checked
  by the test `migration_carries_iteration1_sessions_over` on a seeded schema after migrations
  0001–0004. Back up before updating the production hub (RUNBOOK §1).

## What was not checked

- **Live e2e** (`tools/e2e-local.sh` with real claude) was not run — it costs the
  subscription. The rehearsal `E2E_CLAUDE_BIN=/usr/bin/false` gives `PASS (rehearsal)`; it
  also includes a new check that the failure comes from the binary and not from the workspace
  path. The live scenario differs only in the agent's turn; the workspace path and `cwd` are
  the same as in the rehearsal.
- **`tools/dev.sh up`** was not run (it starts a launchd job in the GUI domain and real
  claude). The workspace step is the same command `e2e-local.py workspace` that passed in the
  rehearsal; on a repeated `up` it sees the existing workspace and does nothing, and if the
  folder was moved to another project it says so and does not fail.
- **The client on a phone** was not checked: the layout of the left column (projects as chips
  at the top, sessions below) and of the Workspaces screen was checked only by tests in
  happy-dom.
- **Windows workers**: the nesting check in the hub compares paths by Unix rules (the hub's
  `std::path::Path` on Linux); `C:\...` will need its own parsing when Windows workers appear.

## Deferred

- `PATCH /api/workspaces/{id}` for `mirror_enabled` (the mirror is in the next iteration 2
  tasks).
- `clone`, `worktree`, `ephemeral`, the session template (§13) — iteration 5.
- Sessions without a workspace (§8.4) through REST and the client.
- Moving a workspace to another project (for now: delete the record and add it again).
