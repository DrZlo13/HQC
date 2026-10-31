# Iteration 4: tasks

Goal (SPEC §19.4): workspaces beyond an existing folder, and per-folder environments on the
worker. Criterion: from a phone, make a worktree of a project's workspace on a worker and start
a session in it; the session's environment lets it reach one ssh host that other sessions on
the same worker are refused; the session commits on a branch, and deleting the worktree from
the phone warns about the unpushed commit before it removes the folder.

Simplifications for this iteration: grants are iteration 6, so only the worker owner creates,
uses and deletes workspaces on a worker, and the confirmations of §13.1 and §13.2 are the
owner's own dialog in the client, not a request in the service session. `launch` is `process`
only; `network` (the bash sandbox) and the environment's `roots` are left out until someone
needs them. ACP agents and `acp_agents` are iteration 5. The project session template (§13.3)
moves to `docs/BACKLOG.md` (2026-10-31: sessions in a shared folder create their subprojects
themselves).

The UX tasks (T48 on) run first; the user adds to them before the workspace tasks start.

Each task is self-contained: it can be handed to a separate agent together with `CLAUDE.md`
and the listed sections of `docs/SPEC.md`. Dependencies are stated; T44 and T45 can run in
parallel after T43; T46 needs T45, T47 needs T46.

## T48. Permission rules in hqc: a rules screen and patterns from the card

Sections: §2.1, §3 Workspace, §5.3, §6, §8.1, §10.1, §14. Runs first. The rules live in
`worker.toml`, `~/.claude/settings.json` and the folder's `.claude/settings.local.json`, none
of them visible in hqc, and "Always allow" saves the exact command, so it rarely matches again
(`docs/BACKLOG.md`, "Permissions managed from hqc", items 1 and 2, moved here).
- hqc's own allow rules, per workspace (decided 2026-10-31: a folder, not a project or a
  worker), stored on the hub (a migration), owner only. The worker gets them with
  `session.start` and in a new frame when they change, and applies them itself in
  `can_use_tool` (allow without asking) after its `deny`, so a new rule works in a running
  session too; `deny` of the worker always wins. A compound Bash command (`&&`, `;`, `|`,
  subshells) is allowed by an hqc rule only if every part is; anything the matcher does not
  understand is asked. This keeps §2.1: the hub can answer permission requests already, a rule
  is a standing answer within the owner's policy, and it cannot lift the worker's `deny`.
- The card: next to "Always allow" (the exact command, as now) an "Allow in this folder" with
  a pattern hqc suggests from the command (`Bash(python3 *)` for `python3 tools/conv.py x`,
  the tool name alone for non-Bash tools), editable before confirming; the call is allowed and
  the rule saved.
- The rules screen, from the worker's settings (pick a folder) and from the session header
  (that session's folder): the default mode and the session's mode; "Check a command" (the
  rule that decides, or "asks"); then three groups in the order they act — denied (never runs,
  never asks), allowed (no asking), the rest (Ask: a card, Auto: the classifier) — each rule
  with its source (`worker.toml`, `~/.claude/settings.json`, `.claude/settings.local.json`,
  hqc). hqc's rules are added and removed there; the others are read-only. The data comes from
  the worker on request (`permissions.inspect {path}` → the rules of the three files for that
  workspace, parsed, with sources; the check runs on the worker with its own matcher).
Acceptance: the common checks; worker tests: hqc rules applied in `can_use_tool` and never
over `deny`, compound commands, a rule update reaching a running session, `permissions.inspect`
of the three sources; hub tests: rules CRUD (owner only, per workspace), sent with
`session.start` and on change; client tests: the card's pattern, the screen's groups and the
check; by hand: allow `python3 *` from a card on the phone, the next `python3` call in that
folder runs without a card, a session in another folder still asks.

## T43. Environments and bindings on the worker

Sections: §2.1, §3 Workspace, §5.1 (`hello`), §5.3 (`session.start`), §8.1, §14. Today the
worker knows only `default`, and `[permissions]` applies to every session on the machine, so one
folder cannot get `ssh` without unblocking it for all (2026-10-26: `ssh debian@192.168.1.20`
refused in `~/work/lab/infra`; moved here from `docs/BACKLOG.md`).
- `worker.toml`:
  ```toml
  [environments.infra]
  env = { GITHUB_TOKEN = "..." }        # variables on top of the child allowlist
  allow_local_mcp = false
  [environments.infra.permissions]
  unblock = ["Bash(ssh:*)"]             # lift worker-wide deny rules here only
  allow = ["Bash(ssh debian@192.168.1.20:*)"]
  deny = ["Bash(git push:*)"]           # added to the worker's deny

  [[env_bindings]]
  path = "/Users/drzlo/work/lab/infra"
  environment = "infra"
  ```
  A binding is matched by the workspace path, the longest match wins, else `default` (§14).
  `unblock` names only rules that are in the worker's `deny` (a config error otherwise); the
  effective rules are the worker's `deny` minus `unblock`, plus the environment's `deny`, then
  the worker's and the environment's `allow`, then the session's mode, as in §14. `settings`
  of the spec is replaced by this block (the only use it had was `permissions`).
- `allow_local_mcp = true`: Claude Code without `--strict-mcp-config` (the hqc server stays).
- The worker resolves the environment itself from the canonical workspace path on every
  `session.start` and never takes it from the hub; the hub's `environment` stays a display. The
  hub learns `environments` and `env_bindings` from `hello` (names, paths and the permission
  rules, never the values of `env`) and shows the environment of each workspace in its settings
  and in the session header.
- `hqc-worker env` lists the environments and bindings, and `hqc-worker env which <path>` prints
  the environment and the effective rules for a path; values of `env` are shown as `***`.
  Editing stays in `worker.toml`.
Acceptance: the common checks; worker unit tests: binding lookup (longest match, `/a/b` vs
`/a/bc`, symlinks), `unblock` of a rule not in `deny` is a config error, the merged rules in
`--settings`, `env` on top of the child allowlist and never in logs or `hello`; hub test: the
workspace shows the environment from `hello` and follows a new `hello`; a live run: in a bound
folder `ssh -o BatchMode=yes <host> true` is asked about or run, in another folder it is denied
by the worker.

## T44. ssh keys of an environment

Sections: §2.1, §14, §20. Depends on T43. Today a session either has no ssh (the default
`deny`) or the user's whole `~/.ssh` through `SSH_AUTH_SOCK`.
- `[environments.infra] ssh_keys = ["infra_ed25519"]`: names of keys in the worker's store
  `$HQC_HOME/keys/` (0700, files 0600; the worker refuses to start with wider modes; files, not
  Keychain, decided 2026-10-31). For a
  session with keys the worker starts its own `ssh-agent` on a socket in `$HQC_HOME/agents/`
  (0700), loads the keys with `ssh-add`, passes `SSH_AUTH_SOCK` of that socket to the agent
  process only, and stops it with the process. Without keys `SSH_AUTH_SOCK` is not passed at all
  (decided 2026-10-31): an agent the worker runs no longer gets the user's agent, so `git` over
  ssh works only in a folder whose environment has a key. Processes the user starts (VS Code,
  a terminal) are not touched.
- `hqc-worker env add-key <name> <file>` copies a key into the store with the right mode.
- The limitation of §14 stays in the spec: without a separate OS user the agent can still read
  `~/.ssh`; the user moves a project's keys out of it.
Acceptance: the common checks; worker tests: the socket lives and dies with the process, two
sessions of one environment do not share a socket, a session without keys has no
`SSH_AUTH_SOCK`, wrong modes refuse to start; a live run: a session in the bound folder runs `ssh
-o BatchMode=yes` to a test host with the store's key while `~/.ssh` has no key for it.

## T45. Clone workspaces, and deleting what hqc created

Sections: §3 Workspace, §5.3, §5.4, §6, §13.1, §13.2, §20. Depends on T43 (the clone runs in the
environment of the target path, so a private repository gets its keys from T44 once that is in).
- `POST /api/workspaces {kind: clone, project_id, worker_id, url, path, branch?, label?}`:
  `path` is the new folder, inside `allowed_roots`, not existing and not overlapping another
  workspace (§3). The hub stores the workspace as `creating` and sends `workspace.create
  {request_id, kind: clone, url, path, branch?}`; the worker runs `git clone` with the
  environment of `path` and a timeout, then answers `workspace.created {request_id, canonical}`
  or `error`. The workspace becomes ready or is deleted with the error shown; `created_by_hqc:
  true`. A clone in progress survives a hub reconnect (the worker resends the answer).
- Deleting a workspace with `created_by_hqc: true` asks the worker first: `workspace.inspect
  {path}` → `{dirty: [files], unpushed: [branch: n]}`; the client shows them in the confirmation
  dialog, and `DELETE /api/workspaces/{id}?remove_folder=true` sends `workspace.delete`; the
  worker deletes the folder only if it is the canonical path it created (it keeps its own list
  in `$HQC_HOME/created_workspaces.json`, the hub cannot name another folder).
- Client: "Clone a repository" next to "Add a folder", with the URL, the parent folder from the
  tree and a name; progress and errors on the workspace row.
Acceptance: the common checks; worker tests: clone into a new folder, refusals (outside roots,
existing, overlapping), the timeout, delete only of a folder from its own list, dirty and
unpushed detection; hub tests: `creating` → ready and → failed, reconnect during a clone,
deletion with `remove_folder`; client tests for the form and the dialog.

## T46. Worktree workspaces

Sections: §3 Workspace (`origin_workspace_id`), §13.1, §13.2. Depends on T45.
- `POST /api/workspaces {kind: worktree, origin_workspace_id, branch, new_branch: bool, path?}`:
  the origin is a git workspace of the same project and worker; `path` defaults to
  `<origin-parent>/<origin-name>-<branch>` (slashes in the branch become `-`). The worker runs
  `git worktree add` in the origin. A migration adds `origin_workspace_id` (on deleting the
  origin: `409 workspace_in_use` while worktrees exist).
- Deletion goes through T45's inspect and dialog and runs `git worktree remove` (with `--force`
  only after the dialog confirmed dirty files), then `git worktree prune`; the branch is kept.
- Client: "New worktree" on a workspace row, and the worktree shown under its origin.
Acceptance: the common checks; worker tests: add on a new and an existing branch, a branch
already checked out elsewhere is refused, remove with and without changes; hub tests: origin
rules and the migration; the iteration criterion by hand.

## T47. Ephemeral worktrees for spawned sessions

Sections: §3 Workspace (`ephemeral`), §7 (`hqc_spawn`), §13.2, §17.4. Depends on T46.
- `hqc_spawn(..., isolation: "worktree", branch?)`: the hub creates an ephemeral worktree of the
  target workspace (branch `hqc/<session-slug>` by default) and starts the session there, so a
  helper does not edit the spawner's checkout. The result carries the worktree's path and branch.
- An ephemeral workspace is never deleted automatically (§13.2): when its last session is
  archived it is marked for deletion, and the project shows it with the T45 dialog.
Acceptance: the common checks; hub tests: spawn with isolation creates and links the worktree,
archiving the last session marks it; worker test for the branch name; a live run (haiku, two
turns): a spawned session reports a path that is a worktree of the spawner's workspace.
