# Iteration 3: tasks

Goal (SPEC §19.3): the hqc MCP server, so that an agent can attach files, message other sessions
and wait for their replies, start new sessions, list and search sessions, and schedule
messages. Criterion: from a phone, ask session A to have work done on another worker; A starts
session B there with `hqc_spawn` (or asks an existing one with `hqc_ask`), B does it, attaches
a file and replies, and A reports back in its own feed with that file.

Simplifications for this iteration: grants are iteration 6, so every tool works only on the
caller's owner's own sessions (a foreign target is `not_found`). The personal assistant
session moves to iteration 4, together with the bare model it defaults to: `hqc_send` and
`hqc_ask` need `session` until then.

Each task is self-contained: it can be handed to a separate agent together with `CLAUDE.md`
and the listed sections of `docs/SPEC.md`. Dependencies are stated; T31, T32, T33 and T35 can
run in parallel after T30; T36 closes the iteration.

## T30. The MCP pipe and `hqc_list_sessions` (done, SPEC 0.33)

Sections: §5.2 (MCP calls), §5.3, §5.4, §7, §7.2, §8.1, §2.1. The whole path of one tool call,
proven on the simplest tool.
- Worker: `hqc-worker mcp` is a stdio MCP server (JSON-RPC 2.0, `initialize`, `tools/list`,
  `tools/call`, `notifications/cancelled`); it reads `HQC_SESSION_TOKEN` from the environment
  and nothing else, talks to the daemon over a Unix socket in `$HQC_HOME` (0600, one per
  daemon), and keeps no state except the turn's `chain`. The daemon checks the token against
  the sessions it runs and forwards `mcp.call {request_id, session_token, tool, args}`; it
  routes `mcp.result` back and sends `mcp.cancel` on `notifications/cancelled` or EOF with an
  open call. The token never reaches arguments, results, logs or files.
- Claude Code adapter: `--mcp-config` as a JSON string with the server `hqc` (command: the
  worker's own binary, `mcp`), `HQC_SESSION_TOKEN` in that server's env, `MCP_TOOL_TIMEOUT =
  (ask_timeout_s + 60) * 1000` in the CLI's env. `--allowedTools mcp__hqc` stays.
- Hub: `mcp.call` is authenticated by `sessions.session_token_hash` and the worker that runs the
  session; unknown token or another worker's session → `mcp.result` with an error. A tool
  registry with argument validation (size cap per call), errors in one shape (`code`,
  `message`). `hqc_list_sessions(project?)`: address `<project-slug>/<session-slug>`, title,
  status of the owner's sessions, the service sessions excluded.
- Every other tool of §7 is listed in `tools/list` only once its task lands (no stubs).
Acceptance: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace` (worker: the MCP server against a scripted runtime over
stdio, the socket and the token check, cancel on EOF; hub: token auth, a foreign token, the
list tool); live `HQC_LIVE_CLAUDE=1` test: a Claude Code turn calls `hqc_list_sessions` and
sees its own session (`--max-turns 2`, haiku).

## T31. `hqc_attach` (after T30)

Sections: §7, §12, §4 `attachment`, §2.1, §14.
- Worker: `hqc_attach(path, name?, kind?, caption?)` reads `path` itself (relative to the
  session's workspace; the canonical path must be inside `allowed_roots`, symlinks resolved;
  100 MB cap, streamed), uploads it with `PUT /worker/artifacts` under the worker token; only
  then sends `mcp.call` with the artifact id. `content` is for the bare model (iteration 4) and
  is not accepted yet.
- Hub: `PUT /worker/artifacts` (the owner is the session owner, same dedup and `kind` rules as
  the client's upload, §12); the tool writes an `attachment` event authored by the agent.
- Client: an `attachment` event from the agent renders like a user's attachment (image inline,
  diff, file link).
Acceptance: the common checks; worker tests: a path outside the roots, through a symlink, a
directory, over the cap; hub tests: upload, dedup, the event; client test: rendering; live run
optional.

## T32. `hqc_send` and `hqc_ask` (done, SPEC 0.34)

Sections: §7.1 in full, §3 PendingRequest, §4 `peer.message`, `ask_cancelled`, §5.2, §17.2.
- Hub: `session` is required for both tools (no personal session until iteration 4); the
  target is an address or an id of the owner's session. A `peer.message` event in the target
  session; `mode=ask` goes into the input queue,
  `mode=send` into the send buffer (`peer_prefix`, 50 messages / 64 KB, eviction with a `system`
  event) or into the queue with `wake_on_send`. An ask is a `PendingRequest` (`kind: ask`,
  `worker_id`, `from_session_id`); the reply is the target's final `agent.message` with
  `in_reply_to`; timeout (`ask_timeout_s`, 10 min), `locked_vscode` target → error at once,
  the asking session itself → error at once; results kept for an offline worker and resent
  after `hello`; `mcp.cancel` → `system ask_cancelled` in the sender. Chains: `chain_id`,
  `depth` ≤ 3, `hops` ≤ 20, given to the MCP server with the first call's result.
- Worker: the header `[hqc peer from … | mode=… | request_id=…]` and the escaping of
  `[hqc peer` in all text passed to the runtime; the "Messages from other sessions" block;
  progress notifications at least once a minute while an ask waits; the adapter fills
  `in_reply_to` of the turn's final message.
- Client: `peer.message` in the feed with the sender's address; a waiting ask is visible in the
  sending session.
Acceptance: the common checks; hub tests for every rule above (queue, buffer and eviction,
reply matching, timeout, offline resend, cancel, chain limits, self-ask); worker tests for the
header and escaping; live `HQC_LIVE_CLAUDE=1`: session A asks session B, gets B's reply.

## T33. Search: the index, `hqc_search_history` and the search bar (after T30)

Sections: §15, §6.
- Hub: migration with `pg_trgm`, a `tsvector` (`simple`) over the event text and session
  titles; `GET /api/search?q=&project=&fuzzy=` and the tool `hqc_search_history(query,
  project?, fuzzy?)` over one query function, the owner's sessions only, paged, with the
  session address and the event's `seq` for a jump.
- Client: a search bar; a result opens the session at that event.
Acceptance: the common checks; hub tests: exact and fuzzy hits, Russian and English, another
owner's sessions never returned; client test: search and jump.

## T34. `hqc_spawn` (after T32)

Sections: §7 (`hqc_spawn`), §3 Session, §13.4, §7.1 (chains).
- Hub: `hqc_spawn(project, workspace?, runtime?, model?, prompt)` creates a session of the
  owner in that project (`workspace` required without `default_workspace_id`; runtime
  `claude_code` only until iteration 4), on the workspace's worker, and delivers `prompt` as a
  `peer.message` from the spawning session that starts the first turn (like `wake_on_send`),
  so the new agent can answer with `hqc_send`. Returns the address at once. The chain goes on:
  `hops` + 1. Limits: at most N sessions spawned per chain and per hour per owner (numbers in
  the spec), errors as in T30.
- Client: a spawned session shows who started it (a link to the spawning session).
Acceptance: the common checks; hub tests: a project and workspace of the owner only, a missing
workspace, the first turn starts, chain and spawn limits; live `HQC_LIVE_CLAUDE=1`: A spawns
B, B's first turn runs.

## T35. `hqc_schedule` and the hub cron (after T30)

Sections: §16 (hub cron), §3 ScheduledMessage, §7.
- Hub: a `scheduled_messages` table; `hqc_schedule(session, at|cron, message)` and
  `GET/POST/PATCH/DELETE /api/schedules`; a cron loop that sends the message as the owner's
  `user.message` at `at` or by `cron` in the user's `timezone`; missed runs with `catch_up`
  once, otherwise skipped; disabled and deleted schedules never fire.
- Client: the session's schedules: list, pause, delete.
Acceptance: the common checks; hub tests with a fake clock: `at`, `cron` across a DST change,
catch-up and skip, a deleted target session.

## T36. Hardening of the MCP surface and the review backlog (closes the iteration)

Sections: §2.1, §20, the 2026-10-23 security review (open items are in the project notes).
- The new paths: caps and rate limits on `mcp.call` per session and worker, open asks per
  session, peer messages per chain, spawned sessions, schedules per owner, search query cost;
  a compromised agent has the owner's rights but must not exhaust the hub.
- The old backlog: open `worker_approval` rows and their `expires_at`, `mirror.event` session
  creation and `runtime_session_id`/`cwd` validation, a worker writing any event type into its
  service session, a byte cap on artifact downloads into `.hqc/inbox` and its pruning, the
  client's `DiffView` fetching before `meta.size`, the minor items.
- An adversarial review of everything iteration 3 added, as before the first deploy.
Acceptance: the common checks; a test per cap; the review's findings fixed or recorded with a
reason.
