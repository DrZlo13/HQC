# T47: decisions

Ephemeral worktrees for spawned sessions, SPEC 0.61. Built on T46's flow: `hqc_spawn` with
`isolation: "worktree"` calls the hub's `create_worktree` (the same origin rules, default path,
`workspace.create {kind: worktree}`, states, resend after `hello`, fail on a new `boot_id`), and
the deletion is T45/T46's inspection and dialog. No second flow, no new frame, no migration, no
worker code change (one worker test).

## The call waits

- **`Answer::Wait`, not a deferred answer.** The hub handles a worker's frames one by one, and
  `mcp.call` used to be answered inside that loop. Waiting for the worktree there would block
  the very `fs.stat` answers and `workspace.created` it waits for. A tool can now answer with a
  future: the connection spawns it and sends its `mcp.result` under the call's frame id when it
  is done (the worker matches it by frame id as for a fast tool). Everything after the argument
  checks runs in it, including the two `fs.stat`s of the new folder. If the connection closes
  first, the task is dropped and nothing is sent; the call is still in the worker's outbox and
  comes again after `hello`. `hqc_ask`'s deferred path (`ack`, `PendingRequest`, resend of the
  stored result) was not used: a spawn has nothing to store until the session exists, and a
  repeat finds everything by its keys.
- **Once per `request_id`:** the worktree's id is `once_id("hqc_spawn worktree", caller,
  request_id)` (the session's stays `once_id("hqc_spawn", ...)`), so a repeat finds the worktree
  and waits for it again, or finds the session and answers at once.
- **The hub polls the workspace row** every 200 ms while it waits (a few seconds for `git
  worktree add`). A notification from `workspace.created` would save the polls but adds a
  registry for one caller.
- **15 minutes at most** (the worker's default `clone_timeout`, which bounds `git worktree add`,
  plus a margin): then the call fails with `unavailable` and no session; the worktree stays and
  shows as marked once it is ready (it has no session). This covers a target worker that went
  offline in the middle.
- **A failed worktree is removed.** The call's error carries the worker's reason, so the record
  would only be something for the user to clean up. Consequence: a repeat of a call whose
  failure was lost makes a new attempt. A worktree that fails while nobody waits (the spawner's
  agent ended) stays as "Worktree failed" until the user removes it.

## Origin and branch

- **The origin is the workspace the spawn would use today** (`workspace` or the project's
  default), checked by `create_worktree`: a ready workspace that is not a worktree (T46, one
  level), on a worker of the owner that is not revoked and is online (`fs.stat`). A plain folder
  is refused by the worker, so the call fails with its reason.
- **The branch is always new.** The default is `hqc/<slug>` of the session to be. An explicit
  `branch` is new too (checked by `check_branch` on the hub and by the worker, refused if it
  exists): a helper never takes over a branch someone else works on. "Checked like T46's" is
  read as T46's checks for `new_branch: true`.
- **The slug is chosen before the session exists.** The branch needs the slug, the session
  needs the worktree. The hub picks N under the owner's spawn lock: the project's next "Session
  N", moved past slugs already taken and branches `hqc/session-N` of the project's workspaces
  (so parallel spawns, whose worktrees have no session yet, get their own). The session is then
  created with that N (`db::Spawn::number`, read back from the default branch, so a repeat gets
  it too). A session created by hand meanwhile could take the slug first; the spawned one then
  gets `session-N-2` and the branch keeps `hqc/session-N`. Rare and harmless.
- **A branch outlives its worktree** (T46 keeps it on deletion), and a slug can come back after
  its session was deleted (N counts sessions): the worker refuses the existing branch, the call
  fails with that reason, and the agent can give `branch`.
- The ephemeral worktree has no label and the mirror off: nobody chose to follow its Claude
  Code sessions, and hqc's own session there is not a VS Code one.

## Limits

- **No limit of their own.** A worktree is made only together with a spawn, so the spawn limits
  (§20) cover it. While a worktree is being made it has no session yet: the limit check counts
  the owner's ephemeral workspaces still `creating` in both limits, in every chain (the chain is
  not stored on the workspace; counting it against another chain of the owner for a few seconds
  is on the safe side). The check and the insert hold the owner's spawn lock, as before, and the
  prompt counts as a peer message at that point; the session's creation after the wait does not
  count again.

## The mark

- **`marked_for_deletion` is computed, not stored:** ephemeral, `ready`, and no session that is
  not archived. Archiving the last session marks it, unarchiving (or the user starting a session
  there) clears it, and nothing can drift. A deleted session counts as gone. No migration:
  `ephemeral` has been in the schema since `0001`.
- Client: the tag "spawned" on every ephemeral row; when marked, "for deletion", a line saying
  the sessions are archived and the worktree stays until deleted, and "Delete" (instead of
  "Remove") opening the T45/T46 dialog, which adds that hqc made the worktree for a session an
  agent started. The project settings read the list when opened, so the mark shows the state of
  then.

## Live run

`crates/hqc-hub/tests/live_worktree.rs` (behind `HQC_LIVE_CLAUDE=1`, `claude-haiku-5-5`,
`--max-turns 2`, two Claude Code runs, `tmp/spike-t47-live-*`): session A calls `hqc_spawn` with
isolation in a git workspace; B, in `ws-hqc-session-2`, reports its working folder; the test
checks it is that path, on `refs/heads/hqc/session-2`, with the common git directory of A's
repository, and listed by `git worktree list` there. Passed on 2026-11-02 (Claude Code on
home-mac).
