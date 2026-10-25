# T34: questions and decisions

## 1. One creation path

`POST /api/projects/{id}/sessions` and `hqc_spawn` both call `api::new_session` (the body
of the old handler): model check, the workspace of §13.4 (`session_workspace`), the worker
of the owner, `cwd`, title, `session_created`, the first `kick` with `session.start`. The
handler keeps only `project_for` (the user's project) and the view. `hqc_spawn`
(`crates/hqc-hub/src/spawn.rs`) does what the API has no part in: resolving `project` and
`workspace` from the agent's strings, the limits, the spawner, the prompt.

`db::create_session` is unchanged for its callers (API tests, the mirror);
`db::create_spawned_session` takes an optional `Spawn {id, by, chain_id}` — the session's id
given by the caller, so that a repeated call finds it.

## 2. The prompt is a `peer.message`, `mode: send`, into the input queue

The task says "like `wake_on_send`": a send that starts a turn. So the message is stored
exactly as a send to a session with `wake_on_send` (`delivery: pending`, authored by the
spawning agent, `from` its address, the chain moved on), and the existing delivery path
carries it: `session.start` (from `new_session`'s kick), then the pending input with
`peer {from, mode: send}`. The new agent reads `[hqc peer from live/spawner | mode=send]`
above the prompt and can `hqc_send` back. No new mode (`spawn`): the agent's behaviour is that
of a send; the client already draws it ("Message from <address>").

`mode=ask` was rejected: it would make the spawner wait (that is `hqc_ask` on an existing
session, which the agent can do right after spawning).

The new session's `wake_on_send` stays `false`: later sends to it are buffered like for any
session.

## 3. Arguments

- `project`: slug or id of the owner's project (T30's `owner_project`); another owner's or an
  unknown one is `not_found`.
- `workspace`: id, label or path of a workspace **of that project**; a workspace of another
  project (even the owner's) is `not_found`. Labels are not unique; the first match wins
  (workspaces are listed oldest first). Absent: the project's `default_workspace_id`; none →
  `invalid_args` "give `workspace`" (`409 no_workspace` in REST). Not `worker`/`cwd`: the
  task's signature has neither, and a subfolder is the template's job (§13.3).
- `runtime`: only `claude_code` (iteration 4 brings the others); the schema lists the enum.
- `model`: absent → **the calling session's model**. The alternative (the CLI default) would
  let a haiku agent start an opus one without saying so. Present → `valid_model` as for the
  API.
- No `title`: "Session N", then the worker's title after the first answer (§3), as for a
  session from the UI without a title.
- `auto_approve` is never set: an agent cannot give another agent more autonomy than the
  default.

## 4. Limits (SPEC §20)

- **5 sessions per chain.** A human's turn that fans out to a few machines is the use case;
  five is more than the iteration's criterion needs and small enough that a runaway
  recursion (A spawns B spawns C …) stops after five sessions, well before the 20 hops.
- **20 per owner in the last hour** (sliding, `created_at`). Each spawned session costs a
  Claude Code process and the subscription; twenty in an hour is far beyond hand-driven use.

They are counted from `sessions.spawn_chain_id` (migration `0017`, partial indexes). Deleted
sessions no longer count; archived ones do. The count and the insert are not atomic: parallel
calls of one agent could pass the check together and overshoot by the number of calls in
flight. Acceptable for a guard against loops; T36 can take a per-owner advisory lock if needed.

### The chain of a human's turn (a change to T32)

§7.1 said "`chain_id` is created on the *first* ask/send from a turn started by a human", but
T32's code started a fresh chain for every call of such a turn. That made a per-chain spawn
limit useless exactly where fan-out happens (a human's turn), so `peers::turn_chain` now treats
a delivered input without a chain as a chain of its own with `depth` 0 and `hops` 0, its id
uuid v5 of the input's event id. `next_chain` gives the same `depth`/`hops` as before
(T32's tests unchanged); only the id is shared within the turn now.

## 5. Once per `request_id`

The session's id is `once_id("hqc_spawn", caller, request_id)`, the message's `event_id`
is `peers::message_id(caller, request_id)` (T32's key for peer messages). A repeat:
- finds the session → skips the checks and the limits (the first call passed them),
- stores the message — or finds it (events are unique by `event_id`),
- answers the same `{address}`.
A crash between the session and the message is completed by the repeat. Two copies at once:
the loser's insert fails on the primary key; it reads the winner's session. Not handled: a
repeat after the spawned session was deleted creates it again (the repeat comes within seconds
of a reconnect, a deletion in that window is not worth a tombstone).

## 6. Who started it: `spawned_by`

`sessions.spawned_by` (FK, `ON DELETE SET NULL`) and `spawn_chain_id` (kept for the limits).
The API session object gets `spawned_by? {session_id, address}`; the address is computed when
served (one subquery in `state::aggregates`), so it follows a project slug change and shows an
archived spawner as well. The client shows "started by <address>" in the chat header, a link to
`#/s/<id>`; a spawner the user cannot open (iteration 6) would still be linked — not possible
until grants.

## 7. Live run

`crates/hqc-hub/tests/live_spawn.rs`, 2026-10-25, home-mac, Claude Code 2.1.308,
`claude-haiku-5-5`, `--max-turns 2`, one run (14.9 s): session `live/spawner` called
`mcp__hqc__hqc_spawn {"project":"live","prompt":"Reply with exactly one word: pineapple"}`
and got `{"address": "live/session-2"}`; the hub started B in the same workspace (model
inherited: haiku), B's input was the `peer.message` (chain depth 0, hops 1), B's first turn
answered `pineapple` with `in_reply_to` = the prompt, and B's transcript has the line
`[hqc peer from live/spawner | mode=send]`. A's turn ended with `error_max_turns` as in T30.

## 8. Open questions

1. The spawner is not told when the spawned turn ends unless the new agent sends back. A
   `notify_on_final` (a send to the spawner with B's final message) would close the loop for
   the iteration's criterion without relying on B; for now the prompt has to ask for it, or A
   uses `hqc_ask` on the new address.
2. Spawning on another owner's worker (shared workers, iteration 6) and choosing a worker
   without a workspace (§8.4) are out of scope.
