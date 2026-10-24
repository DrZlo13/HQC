# T17 — the worker's service session and non-session confirmations: decisions, questions

Task: the worker's service session (§10.2), frames `worker.approval.request` /
`worker.approval.response`, worker logs in the service session, confirming a new folder for
someone else's session. SPEC 0.17 (§3 Worker, §10.2, §16, §23); task: `docs/ITERATION-2.md`.
Live claude runs: zero.

## Decisions made along the way (not covered by the task statement)

- **How the worker learns its service session.** Logs go as regular `event`s through the
  outbox and need a `session_id`. The hub reports it in `hello.ack` (optional
  `system_session_id`, added without changing the protocol version). Lines before the first
  `hello.ack` wait in memory (up to 60); if the worker stops without ever reaching the hub,
  they are lost. The id could be stored in `$HQC_HOME` to write to the outbox right away — I
  did not: if the hub changed, the id would be stale.
- **`requested_by_owner` in `session.start`** is read as "the session's workspace was added by
  the worker owner" (`workspaces.owner_id` = the worker owner). Since T14 only the owner can
  add a workspace on a worker, so for now the flag is set on all sessions, and the new-folder
  confirmation never fires in practice — until sharing (iteration 6) or `workspace.create`
  from a colleague (iteration 5). The field is sent only as `true`, so as not to touch the
  example `hub.session.start.json`, shared with parallel tasks.
- **What "for the first time" means.** The cache `$HQC_HOME/known_workspaces.json` holds the
  canonical paths where the owner agreed (`once`) or where a session of the owner / a session
  in the owner's workspace ran. The comparison is exact: a subfolder of a known folder is a
  new folder (workspaces do not nest, §3 Workspace, so in practice this is not a problem). A
  refusal (`deny`) is not remembered: the next `session.start` asks again.
- **Waiting for a confirmation holds `session.start` unanswered**, rather than answering with
  an error and a retry: otherwise the hub would count failed starts (§5.2) and mark the input
  `failed` after three. The hub has no timeout for the answer to `session.start`, so this is
  safe; after a reconnect the hub sends a new `session.start` for the same session — it joins
  the same request. A refusal is `invalid_frame`, `retryable: false`: the hub marks the
  `pending` input `failed` with `system agent_start_failed` ("The agent did not start…: the
  worker owner did not allow sessions in …"). There is no separate error code: I did not
  touch the §5 code list.
- **The worker's own timeout**: `expires_at` (10 minutes) + 30 s without an answer means a
  refusal. Needed in case the hub is unreachable all that time.
- **The service session is stored like a regular one** (`sessions.system = true`,
  `runtime = 'none'`, the worker's `worker_id`), so that events, `seq`, subscriptions, unread
  counts and `PendingRequest` work without special paths. The special places: `delivery::kick`
  does not start it; input, stop, retry, archiving and deletion are `409 system_session`;
  `auto_approve` is `403`; "Session N" numbering does not count it. Renaming (`PATCH title`)
  is not forbidden — no runtime title will arrive there, and the hub does not sync the worker
  name (there is no worker renaming yet).
- **`worker_log` lines do not count as unread** (otherwise the badge would light up on every
  reconnect warning). This is an exception to the §6 rule, described in §10.2.
- **Logs carry a `target`** (`hqc_worker::connection: hub connection lost …`) and `key=value`
  fields; the level is the `level` field in the `system` payload (the typed
  `EventKind::System` was not changed, so as not to break constructors in parallel branches;
  the worker builds the event as JSON and puts it into the outbox through
  `HubLink::event_json`). Lines of the forwarder itself (`hqc_worker::worker_log`) are not
  forwarded — otherwise an outbox write error would spawn new lines.
- **Rate limiting**: a 60 s window from the first line (not sliding); "skipped N" is emitted
  as soon as the window has room (for repeats of one line — within a second), and itself
  counts toward the 60. An overflow of the layer's channel (256 lines) also counts toward
  "skipped".
- **The worker request's options** are fixed: `once` / `deny` (labels `Allow` / `Deny`; the
  client labels them by `canonical`). The event's `tool` = the request's `kind`; the client
  shows `workspace.use` as "sessions in a new folder".
- **Migration `0006`**: service sessions also for revoked workers (the history stays). The
  number `0006` may clash with a T15/T16 migration — rename on merge (the migration is
  independent of its neighbours). Resolved in T18: `0006` stayed with T17, T16 — `0007`,
  T15 — `0008`.

## Questions

1. Who can answer worker requests once grants appear (iteration 6)? Now only the worker
   owner, and the service session is visible only to them (it is in the personal project,
   which is not shared). If a "trusted" second person is needed, that is a separate policy.
2. Should a refusal be remembered for a folder (`deny_always`)? Now every new `session.start`
   of someone else's session in a refused folder asks again — with a persistent colleague
   that is request spam (limited only by the fact that waiting starts for one folder share
   one request).
3. Should `info`-level lifecycle lines (connected, restarted) be written to the service
   session? Now only warn/error, as in the task statement.
4. Move the new fields into §4, §5.3, §5.4 and §6 after the merge with T15/T16 — in T17 those
   sections were not edited per the task's terms; the facts are collected in §10.2 (the list
   is in §23).
   Resolved in T18: moved to §3, §4, §5.1, §5.3, §5.4, §6, §16 (SPEC 0.19).

## Not checked

- Not run on the live home-mac worker: the `tracing` layer in `main.rs` was checked by a unit
  test on the subscriber, forwarding by an integration test with a channel instead of the
  layer.
- Push for a worker request was checked on a recording `PushSender`, not in a real browser.
