# T30: questions and decisions

## 1. How the session token reaches the MCP process ([verify] in the task)

Candidates: the `env` of the server in the `--mcp-config` JSON (on the CLI's command line),
inheritance from the CLI's environment, Claude Code's `${VAR}` expansion in the MCP config, a
one-time secret over the socket.

What was checked on 2026-10-25, on home-mac (macOS 26.6, arm64), Claude Code **2.1.308** (the
installed CLI; `system/init.claude_code_version` and the `clientInfo` of `initialize` both say
2.1.308 — 2.1.310 is the VS Code extension of SPEC §23 0.30, not this CLI):

- **Another OS user, `ps`.** `ps` itself is not available to this agent, so the same kernel call
  was made directly (a Python `ctypes` script in `tmp/spike-t30-ps/procargs.py`, not committed):
  `sysctl({CTL_KERN, KERN_PROCARGS2, pid})`, which is where `ps` gets the command line and, with
  `-E`, the environment. For every pid 1..4999: own processes 69 of 69 readable; processes of
  other users (those `kill(pid, 0)` answers `EPERM`) 116 of 118 refused with `EINVAL`, the 2
  readable ones are `loginwindow` and a Safari helper of the login session. So on macOS a
  non-root user reads neither the argv nor the environment of our processes; root reads both.
  On Linux `/proc/<pid>/cmdline` is world-readable (unless `hidepid`), `/proc/<pid>/environ`
  is not; this was not run (no Linux worker runs Claude Code yet).
- **The agent's Bash.** Live test `crates/hqc-hub/tests/live_mcp.rs` (one run, haiku,
  `--max-turns 2`, two prompts in one process): the second prompt had the agent run
  `env | grep -c '^HQC_' || true`; the result was `0`. The CLI's environment is the worker's
  allowlist (SPEC §8.1) plus `MCP_TOOL_TIMEOUT`; `HQC_*` never passes, so there is nothing to
  inherit.
- **The transcript and other files.** Same run: the token (taken from the recorded command
  line) was not in the transcript `~/.claude/projects/<cwd>/<uuid>.jsonl`, `~/.claude.json`,
  the CLI's stream-json stdout, the hub's events or the worker's outbox. The transcript does
  contain the call (`hqc_list_sessions`) and its result.

Decision: **`env` in `--mcp-config`** (SPEC §7). Rejected:
- inheritance and `${VAR}` expansion: both need the variable in the CLI's environment, which
  every Bash call of the agent inherits;
- a config file: the token would be in a file (the task forbids it), and `--mcp-config <path>`
  would also outlive the process;
- a one-time secret over the socket: the only gain is on Linux (argv visible to other users),
  where the token is useless to them anyway (the socket is 0600 and only accepts peers of the
  same uid; the hub only accepts the token over the connection of the worker that runs the
  session). Worth doing if Claude Code ever runs on a shared Linux machine (question 7.1).

## 2. The MCP server: hand-written

`hqc-worker mcp` needs `initialize`, `ping`, `tools/list`, `tools/call` and
`notifications/cancelled` over line-delimited stdio JSON-RPC, plus `notifications/progress`
in T32: about 350 lines without the tests. The Rust MCP SDK (`rmcp`) would bring macros, its own
service/transport layers and a protocol version list to keep in step with Claude Code anyway,
for the same handful of methods; nothing in the server is hard to get right by hand. The
protocol versions it answers are `2025-11-25` (what 2.1.308 asks for), `2025-06-18`,
`2025-03-26`, `2024-11-05`; another version gets the newest.

## 3. `mcp.call` and the outbox

SPEC §5.2 put `mcp.call` in the outbox; the task says the token never reaches files. Both hold:
the outbox stores the call without `session_token`, the connection adds it when sending, from
memory. A call whose caller is gone (answered, cancelled, the worker restarted) is dropped
instead of sent: after a restart the MCP process died with the agent, so running its call
would be a side effect nobody sees. Across a dropped connection the call waits in the outbox
and is sent after `hello` (test `a_call_waits_in_the_outbox_across_a_reconnect`).

Consequence for T31+: when the answer to a call is lost (connection dropped after the hub ran
it), the call is sent again and runs again. `hqc_list_sessions` does not care; a tool with side
effects should remember `request_id` (open question 7.3).

## 4. Token rotation

The hub issues a fresh token on every `session.start`, also for an agent that keeps running
(every reconnect sends `session.start` before the next input). The MCP process cannot learn
the new token, so:
- the daemon keeps per session the *process token* (what its MCP process authenticates with on
  the socket) and the *hub token* (the latest from `session.start`), and sends the latter;
- the hub accepts the latest and the previous hash (`prev_session_token_hash`, migration
  `0014`), so a call sent just before the next `session.start` still passes. Two rotations
  while one call is on its way would fail it with `unauthorized`; that needs two reconnects in
  a few milliseconds.

## 5. The frames

- A fast tool's `mcp.result` is the reply under the call frame's `id` (like `result` to
  `fs.stat`), which also removes the call from the outbox.
- For T32's deferred `hqc_ask` the plan written into §5.2: the hub answers the call frame with
  `ack {}` (the outbox entry goes, the call stays open on the worker), and sends `mcp.result`
  later as a frame of its own, which the worker acks. The worker already handles both.
- `mcp.cancel` is sent directly (not through the outbox) for every call open when the runtime
  cancels it or the MCP process exits, fast calls included; the hub acks it. A call still in
  the outbox is removed from it.
- `error` replies of the hub to `mcp.call` (a refused frame) fail the call with `unavailable`
  instead of leaving the agent waiting until `MCP_TOOL_TIMEOUT`.

## 6. `hqc_list_sessions`

- Archived sessions and the workers' service sessions are left out; the result is ordered by
  the last activity, at most 200 (`truncated`).
- `current: true` marks the calling session, so the agent knows its own address (and T32 can
  refuse an ask to itself without the agent guessing).
- `status` is the computed `state` of SPEC §17.1; the caller's own session is `running` while
  it asks.
- A project is a slug or an id of the owner's projects; another owner's project is
  `not_found`, the same as an unknown one.

## 7. Open questions

1. **Linux.** If Claude Code runs on a Linux worker shared with other users, its command line
   (and so the token) is visible to them. The token alone is useless to them (section 1), but a
   one-time secret over the socket would close it. Not needed for home-mac.
2. **Agents of other sessions on the same worker** can read the CLI's command line of another
   session (same OS user) and call hqc tools with that session's token through their own MCP
   process: the daemon checks that the token belongs to a running session, not that the caller
   is that session's process. This is the soft boundary of SPEC §2.1/§14 and matters only for
   foreign sessions (`allow_foreign_sessions`). The daemon could bind a socket peer to the
   session by process ancestry (`LOCAL_PEERPID` / `SO_PEERCRED` → the parent is the session's
   CLI). Candidate for T36.
3. **Repeated `request_id`.** See section 3: tools with side effects (`hqc_attach`,
   `hqc_send`, `hqc_spawn`, `hqc_schedule`) should store their result by `request_id` and
   return it for a repeat.
4. **A turn that calls an hqc tool needs two model turns** (`ToolSearch` first). With the
   live tests' `--max-turns 2` the agent never gets to answer after the call (`error_max_turns`);
   the test checks the tool result. Production has no `--max-turns`.
5. `hqc_list_sessions` has no paging; 200 sessions is a guess at what fits an agent's context.
