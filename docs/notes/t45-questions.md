# T45: decisions

Clone workspaces, and deleting what hqc created, SPEC 0.58 (written on top of 0.56 while T44
takes 0.57 in parallel; the merge renumbers if needed).

## Protocol

- **`workspace.create` is acked at once; the outcome is a frame of its own,
  `workspace.created {request_id, canonical | error}`.** A clone takes minutes, longer than any
  request the hub waits for, and must survive a reconnect. The error travels inside
  `workspace.created` rather than as an `error` frame, so the deferred answer has one type the
  hub acks. An `error` frame answering `workspace.create` itself (a worker before 0.58 does not
  know the type) fails the workspace too.
- **`request_id` is the workspace's id.** The hub stores nothing extra to match the answer, and
  repeating the request after `hello` names the same clone.
- **"Resends the answer after hello" is pulled by the hub, not pushed by the worker.** The hub
  sends `workspace.create` again after every `hello` of the same worker process while the
  workspace is `creating`; the worker answers a request it knows with its outcome (acked, never
  started twice; a running one is only acked) and starts one it never got. So the worker needs
  no acks of its own frames (direct frames are not tracked on the worker), and a request lost on
  the way in is covered by the same rule. The worker keeps the outcomes in memory for the life
  of the process (a few hundred bytes per clone).
- **A worker restart fails the clone on the hub, by `boot_id`**, like a lost turn; the worker
  removes the folders of entries still marked as creating when it starts. Edge left open: a
  clone that finished just before a restart, with its `workspace.created` lost, is `failed` on
  the hub while its folder stays on the worker (in its list). Removing the failed record leaves
  that folder; adding it again as a folder works.
- **`workspace.inspect` and `workspace.delete` are request/`result` frames like `fs.*`** and
  count among the four `fs.*` requests in flight per worker. The hub waits 10 s for the
  inspection and 60 s for the deletion (a big checkout takes a while to remove); a timeout keeps
  the record, and deleting again is safe (a listed folder that is gone is `removed: false`).
- `requested_by_owner` and `origin_url`/`origin_path` of the old spec stubs are gone: in
  iteration 4 only the owner creates and deletes (the dialog is the confirmation), and T46 adds
  `kind: worktree` to the same internally tagged `kind` (`WorkspaceSource`).

## Worker

- **The worker creates the folder itself (`create_dir`, which fails if it exists) and clones
  into the empty folder**, then records it as creating before `git` runs. Nothing it did not
  create can end up in `created_workspaces.json`, even if something appears at the path between
  the check and the clone.
- **Overlap** is checked against the folders the worker knows: workspaces from `session.start`
  and `workspace.rules`, `known_workspaces.json` and its own created folders. The hub checks its
  own records before it sends anything (`409 workspace_conflict`).
- **URLs** (`hqc_proto::frame::check_clone_url`, shared by hub and worker): `https://`,
  `ssh://`, scp-like `user@host:path`, and `file:///…` only when the path resolves inside
  `allowed_roots` (the tests clone local bare repositories that way; a file URL elsewhere would
  let the hub copy a repository from outside the roots into them). `http://`, `git://`, `ext::`
  and every `<transport>::`, local paths, a leading `-` and a host starting with `-` are refused.
  `GIT_ALLOW_PROTOCOL` limits git to the same transports (also for redirects and anything git
  would follow). The URL goes after `--`; no shell.
- **No prompts:** `GIT_TERMINAL_PROMPT=0`, `GIT_ASKPASS=true` (credential helpers still answer;
  nothing opens a dialog), `GCM_INTERACTIVE=never`, `ssh -o BatchMode=yes` unless the
  environment's `env` sets `GIT_SSH_COMMAND`/`GIT_SSH`, stdin closed. On timeout the whole
  process group is killed and the folder removed.
- **One process environment:** `Environment::process_env` (the child allowlist plus the
  environment's `env`) is used by the Claude Code adapter and by every `git` for a workspace;
  T44's `SSH_AUTH_SOCK` of the environment's agent belongs there. The timeout test sets
  `GIT_SSH_COMMAND` in a bound environment, which also shows the clone runs in the environment
  of the new path.
- **Deletion** looks the path up in the list as written (canonical) and requires that it still
  resolves to itself inside `allowed_roots`: a folder swapped for a symlink is refused, a
  symlink to a listed folder is not that folder. A path that is not listed and one that does not
  exist get the same refusal, so the hub cannot probe paths with it.
- `workspace.inspect` lists dirty paths (status `-z`, renames counted once) and per local branch
  `rev-list --count <branch> --not --remotes`. A folder without `.git` is `git: false`; the
  inspection takes any folder inside the roots (it reads names only), the hub asks only for
  folders hqc created.

## Hub

- Migration `0028`: `state`, `error`, `clone_url`, `clone_branch` on `workspaces` (existing
  rows `ready`). `workspaces_of_worker` (used by `workspace.rules` after `hello`) and
  `mirrored_workspaces` return `ready` ones only; the clone is mirrored when it becomes ready.
- `POST` checks the parent with `fs.stat` (the existing codes) and the new path with a second
  `fs.stat` (`400 path_exists`), so the common mistakes are answered at once; the worker checks
  everything again. The stored path is the canonical parent plus the name, updated to the
  worker's `canonical` when it answers.
- Sessions refuse a workspace that is not ready (`409 workspace_not_ready`, also for
  `hqc_spawn` and schedules, which go through the same code); a `creating` workspace cannot be
  deleted (`409 workspace_creating`). `remove_folder=true` is only for a ready folder hqc
  created; a failed clone is removed as a record.
- No push to clients: the client reads the list again every second while a row is `creating`.

## Client

- "Clone a repository" sits next to "Add a workspace" (the mockups name the add button so; the
  task's "Add a folder" is that button). Both forms share the folder tree (`FolderTree`) and the
  worker select. The folder name follows the URL until it is edited; the branch is optional.
- The row: "Cloning…" with a note and no actions while creating; "Clone failed", the worker's
  error and "Remove" (record only) when failed; the normal row when ready.
- The deletion dialog uses the round 2 dialog look (scrim, header with a close cross, footer
  buttons; full screen on the phone) with its own small `confirm-*` classes; "Delete folder" is
  enabled once the worker's inspection arrived, "Remove from project only" always.

## Not done here

- No live check against a real remote: every clone in the tests is a local bare repository
  (`file://` inside the test roots). Cloning a private repository over ssh needs T44's keys.
