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

## T31. `hqc_attach` (done, SPEC 0.34)

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

## T32. `hqc_send` and `hqc_ask` (done, SPEC 0.35)

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

## T33. Search: the index, `hqc_search_history` and the search bar (done, SPEC 0.36)

Sections: §15, §6.
- Hub: migration with `pg_trgm`, a `tsvector` (`simple`) over the event text and session
  titles; `GET /api/search?q=&project=&fuzzy=` and the tool `hqc_search_history(query,
  project?, fuzzy?)` over one query function, the owner's sessions only, paged, with the
  session address and the event's `seq` for a jump.
- Client: a search bar; a result opens the session at that event.
Acceptance: the common checks; hub tests: exact and fuzzy hits, Russian and English, another
owner's sessions never returned; client test: search and jump.

## T34. `hqc_spawn` (done, SPEC 0.37)

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

## T35. `hqc_schedule` and the hub cron (done, SPEC 0.38)

Sections: §16 (hub cron), §3 ScheduledMessage, §7.
- Hub: a `scheduled_messages` table; `hqc_schedule(session, at|cron, message)` and
  `GET/POST/PATCH/DELETE /api/schedules`; a cron loop that sends the message as the owner's
  `user.message` at `at` or by `cron` in the user's `timezone`; missed runs with `catch_up`
  once, otherwise skipped; disabled and deleted schedules never fire.
- Client: the session's schedules: list, pause, delete.
Acceptance: the common checks; hub tests with a fake clock: `at`, `cron` across a DST change,
catch-up and skip, a deleted target session.

## T36. Hardening of the MCP surface and the review backlog (done, SPEC 0.39; closes the iteration)

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
Done: every item with its status and test is in `docs/notes/t36-questions.md`. The
process-ancestry check of the MCP socket is **[verify]** in SPEC §7.2 (no live run was made).

## T37. Requests where the user is looking (done, SPEC 0.41)

Sections: §6, §7.1, §10.1, §10.2, §16. A request of a session that another session waits on
(an open `hqc_ask`, also along a chain) is shown and answered only in its own feed: the user
looks at the asking session and sees nothing but "Waiting for a reply".
- Hub: one query for the owner's open requests (`approval`, `clarify`, `worker_approval`) with
  the session's address, title and worker; `GET /api/requests`; live changes to the client
  (with the summary subscription, no polling). For a session, the open requests of the
  sessions it waits on through open asks, transitively.
- Client: in the asking session, under its "Waiting for a reply from …" line, the waited-on
  session's request as a card that answers it in place; the card names the session and the
  worker at the top and does not look like the session's own requests. A "Requests" list of
  all open requests (left column with a count on the desktop, a screen on the phone); each item
  answers in place or opens its session.
- Push: a request of a waited-on session opens the session where the user waits (the top of
  the chain); one notification per request.
- Answers go to the request's own session as now: the event, the audit and the permission
  policy (the worker's) stay where they are; nothing is approved automatically.
Acceptance: the common checks; hub tests: the owner's list, another owner's requests never
listed, the transitive waits, live changes; client tests: the card in the asking session, the
list, answering from both.

## T38. A permission mode per session (done, SPEC 0.42)

Sections: §2.1, §3 Session, §6, §8.1, §10.1, §14. Agents that write Python, grep, route boards
and convert files ask all the time, and "Always allow" saves the exact command, which rarely
matches again.
- A session's mode is "Ask" or "Auto" (Claude Code's classifier, `--permission-mode auto`),
  chosen when the session is created and switchable from its menu at any time; the worker's
  `deny` stays stronger in both. The default comes from `[permissions] mode` of the worker.
- Worker: `[permissions] allow_auto` (default `true`) — with `false` the worker refuses "Auto"
  from hqc and runs the session in "Ask"; the worker reports the mode it actually runs.
- Sessions started with `hqc_spawn` and sessions a peer message wakes do not inherit "Auto";
  `hqc_spawn` cannot ask for it.
- The client shows the mode in the session header; "Auto" is visibly marked.
Acceptance: the common checks; worker tests: the flag in the CLI arguments or the runtime
switch, `allow_auto = false`, the reported mode; hub tests: create with a mode, switch, spawn
without inheritance, another owner; client tests: the switch and the mark; a live run that
switches a running session and sees `permissionMode` change.
Done: the runtime switch is Claude Code's `set_permission_mode` control request (verified on
2.1.308, no restart); decisions and open questions in `docs/notes/t38-questions.md`.

## T39. A message cannot raise permissions (done, SPEC 0.43)

Sections: §7.1, §10.1, §16, §17.2. After T38 a turn woken by another session's message ran in
the target's mode, so a session in "Ask" could have work done without asking through a session
in "Auto". The rule (user decision, 2026-10-25): a turn woken by a peer message (`hqc_ask`,
`hqc_send` with `wake_on_send`, the prompt of `hqc_spawn`) or by an agent's schedule runs in
"Auto" only if the target and every session up the chain run in "Auto"; otherwise in "Ask".
A turn started by the user's own message runs in the session's mode, as now.
- Hub: a mode cap travels with the chain — the cap of a peer message is the lower of the sender
  session's effective mode and the cap of the input that started the sender's turn; an agent's
  schedule stores the cap with its chain. `session.input` carries the cap (additive).
- Worker: for an input with an "Ask" cap in a session running "Auto", switch to "Ask" before
  handing the input to the runtime and back after the turn ends (the T38 runtime switch); a
  queued user input after it runs in the session's own mode. The cap only lowers: the worker
  never raises a mode because of it.
- Client: a turn running under the cap shows "Ask (asked by <address>)" in the header.
Acceptance: the common checks; hub tests for the four combinations (Auto/Ask sender × target),
a three-session chain with "Ask" in the middle, an agent's schedule; worker tests: the switch
down and back, a user input queued after it, the cap never raising; a live run where A in "Ask"
asks B in "Auto" and B's command is asked about.
Done: the cap is a field of the chain (`permission_cap`, `capped_by`); the worker waits for the
runtime's confirmation of Ask before it hands over a capped input, or restarts the process in
Ask; the live run (`live_cap`, 2.1.308) saw B's `touch` asked about. Decisions and open
questions in `docs/notes/t39-questions.md`.

## T40. The cap for buffered sends and old schedules (done, SPEC 0.44)

Sections: §7.1, §10.1, §16. Two gaps T39 left (`docs/notes/t39-questions.md`, user decision
2026-10-26):
- A buffered `hqc_send` (no wake) puts its text in front of the target's next input. That input's
  turn now takes the lowest cap of the buffered messages it carries, together with its own (a
  user's input has none of its own): text from a session in "Ask" never runs in "Auto".
- An agent's schedule caps each run with the lower of the cap stored when it was made and its
  creator's effective mode at run time (a deleted creator counts as "Ask").
Acceptance: the common checks; hub tests: a user's input carrying a buffered send from "Ask"
runs capped, one from "Auto" does not; a schedule whose creator moved from "Auto" to "Ask", and
one whose creator was deleted; the client's header names the session that capped the turn.
Done: the cap a buffered message lends an input is decided when the hub first hands it the
buffer and kept in `input_caps` (migration `0023`), so the header names it and the turn's own
messages carry it on; a schedule stores its creator (`created_by`) and a run computes its cap
at run time, a creator that is gone (deleted, or a schedule from before 0.44) counting as
"Ask". The worker and the client are unchanged: the header already names
`permission_capped_by`, which now comes from either source. Tests: hub `tests/t40.rs`.

## T41. Files between sessions (done, SPEC 0.46)

Sections: §4 (peer.message), §5.3 (session.input), §7, §7.1, §12, §14, §20. `hqc_send` and
`hqc_ask` carry text only, so an agent cannot hand another session a file; the iteration's
criterion needs B's file to reach A (2026-10-27: an agent on home-mac fell back to pasting a
file into the message text).
- `hqc_send(text, session, files?)` and `hqc_ask(text, session, timeout_s?, files?)`: `files`
  are paths (or artifact ids from `hqc_attach` results). The sender's worker resolves and
  uploads paths with the `hqc_attach` rules (inside the workspace and `allowed_roots`, regular
  files, 100 MB each), then the call carries artifact ids. The `peer.message` keeps
  `artifact_ids`; the target's worker downloads them into its `.hqc/inbox` and lists the paths
  under the peer header, as for a user's attachments (§12).
- The reply to `hqc_ask` carries the files the target attached during the answering turn
  (`hqc_attach` and files of its final message): the asker's worker downloads them into its own
  `.hqc/inbox` before it returns the tool result, which lists the local paths.
- Owner's own sessions only; an artifact of another owner is `not_found`; the inbox caps of T36
  apply; a repeated call acts once (§5.2).
Acceptance: the common checks; worker tests: upload rules on the sender side, download into
the inbox on both sides, the result's paths; hub tests: artifacts on the message and on the
reply, another owner's artifact, a repeat; a live run: A asks B on another session, B attaches a
file, A's tool result has a local path to it with the same content.
Done: the sender's daemon uploads `files` with the `hqc_attach` rules and sends `artifact_ids`;
the hub links them to the target and, for a reply, links the target's `attachment`s of the
answering turn (owner's only) to the asker and puts them in the stored `mcp.result`; the asking
daemon downloads them into `.hqc/inbox/<request_id>/` before it answers. No migration. Tests: hub
`tests/t41.rs`; worker `tests/mcp.rs` (`message_files_are_uploaded_before_the_call`,
`the_files_of_a_reply_are_downloaded_before_the_result`), `tests/worker.rs`
(`peer_files_reach_the_inbox`); client `attachments.test.tsx`; live `live_files` (2.1.308, with
`ENABLE_TOOL_SEARCH=false`). Decisions and open questions in `docs/notes/t41-questions.md`.

## T42. Replies that come back as a send (after T41)

Sections: §4, §7, §7.1, §8.1. The 2026-10-31 run: A asked B with `hqc_ask` and `files`; B
changed the file and sent it back with `hqc_send` and `files` instead of `hqc_attach`, so the
ask's reply had no file and the send sat in A's buffer until the user's next message.
- Hub: while A waits on an open ask to B, a `hqc_send` from B to A (B's turn that answers it)
  goes into that ask's reply — its text after the final message, its files with the reply's
  files — instead of A's send buffer; the send's result says so (`delivery: "with_reply"`).
  A send from B to A outside the answering turn is unchanged.
- Worker: the ask header tells the target how to return files (`hqc_attach`, or `files` of
  `hqc_send`); the tool descriptions say the same.
- [verify] Whether a session whose transcript holds a ToolSearch result from before an update
  sees the new tool schema (A said `files` was not in its description, yet the call went
  through). Record the finding in SPEC §8.1 and the notes; fix only if hqc can do something.
Acceptance: the common checks; hub tests: a send from the target during the answering turn
joins the reply with its files, one outside it is buffered, another session's send to A is
buffered; a live run: A asks B with a file, B answers with `hqc_send` and `files`, A's tool
result has B's file.

