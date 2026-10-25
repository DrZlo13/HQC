# T31: questions and decisions

## 1. Who reads the file: the daemon

`hqc-worker mcp` runs inside the agent's process tree and holds only the session token; the
daemon holds the worker token, `allowed_roots` and the session's folders from its
`session.start`. So the daemon does the whole file part (`crates/hqc-worker/src/mcp/attach.rs`):
the MCP server forwards `hqc_attach` like any other call, the daemon resolves and uploads,
then sends `mcp.call {artifact_id, caption?}` (`hqc_proto::mcp::AttachCall`). The hub never
gets a path, so it cannot steer the worker to a file (SPEC §2.1). The MCP server's
`tools/list` shows the agent's arguments (`path, name?, kind?, caption?`); the hub's registry
takes the worker's (`artifact_id, caption?`).

## 2. The path rules

- Relative to the session's **working directory** (`<workspace>/<cwd>`), not the workspace
  root: that is where the agent's Bash and file tools stand, and an agent naming
  `report.txt` means the one it sees. The task text says "relative to the session's
  workspace"; the two are the same for `cwd = "."`, which is every session so far.
- The canonical path (symlinks resolved) must be inside the session's workspace **and** inside
  `allowed_roots`. The workspace check is the stricter one in practice; the roots check is the
  invariant of CLAUDE.md and also catches a session without a workspace (its scratch folder is
  not in the roots), whose `hqc_attach` therefore fails until iteration 4 decides otherwise.
- Regular files only. The file is opened with `O_NOFOLLOW | O_NONBLOCK` and checked with
  `fstat` on the handle: a symlink swapped in after `canonicalize` fails the open, a FIFO
  cannot hang it, a directory or device is refused. What is not closed: swapping a directory
  component between `canonicalize` and `open` (an agent racing its own worker inside its own
  workspace; it can read those files with Bash anyway). `openat` walking with `O_NOFOLLOW`
  per component would close it; not worth it while the agent has Bash.
- 100 MB (`inbox::MAX_ARTIFACT_BYTES`, the hub's limit) checked on the handle; the body is
  exactly that many bytes (`take(size)` with `Content-Length`), so a file growing meanwhile is
  cut, and one shrinking fails the upload.
- `name` absent: the last component of the path as the agent wrote it (a symlink
  `latest.png` shows as `latest.png`, not as its target); the hub cleans it as for the client.
- `content` is not in the schema and is refused as an unknown field (`invalid_args`).

## 3. `PUT /worker/artifacts`

The body is the raw file, not multipart: the worker has nothing else to send, and the name
and kind fit headers the download already uses (`x-hqc-artifact-name` percent-encoded,
`x-hqc-artifact-kind`). The session is `x-hqc-session-id`; the hub accepts a session of this
worker only (another worker's, an unknown one and the service session are `404`). The owner is
the session's owner; dedup `(owner, sha256, name)` and the `kind` rules are the client's
(`artifacts::save`, shared with `POST /api/artifacts`). The axum default body limit is off on
this route; the stream is counted against `max_bytes` like the multipart one, after an early
`413` on a `Content-Length` over it.

The upload links the artifact to the session in `artifact_sessions`. That row is what
`hqc_attach` checks on the hub (an artifact of the owner that was not uploaded for this
session is `not_found`), and it lets the session's worker download it back, which is harmless.

An upload whose `mcp.call` never comes (the worker restarted, the runtime cancelled) leaves an
unreferenced artifact; the daily cleanup (§12) removes it after the retention period.

## 4. A repeated `mcp.call` (open question 7.3 of T30)

T30 sends a call again after a reconnect when its answer was lost; the hub runs it again. For
`hqc_attach` the side effect is the `attachment` event, and events are already unique by
`event_id`. So the tool derives the event's id from the call:
`uuid v5(NS, "hqc_attach\n<calling session id>\n<request_id>")` (`mcp::once_id`). A call first
looks the event up; found, it answers from it (`{event_id, artifact_id, name, kind}` are all in
the event), so a repeat gets the same result and nothing is written twice. Two copies running
at once both try the insert; the loser gets `Duplicate` and reads the winner's event. The
calling session is part of the key, so another session's call with the same `request_id`
(only a broken or hostile worker would send one) is a different call, not a way to read
someone's event.

Rejected: a generic `mcp_results (request_id, result)` table. It needs a migration and a second
write per call, and for this tool the event already is the remembered result. A tool whose
effect is not one row it writes itself (T32's deferred ask) may need the table; `once_id` is
there for the ones that fit.

The registry's `Handler` got the call's `request_id` as a parameter for this.

## 5. Cancel during the upload

The upload runs before there is an `mcp.call`, so T30's `mcp.cancel` path does not cover it.
The daemon keeps the connection's calls being prepared with their task's abort handle; a
`notifications/cancelled` or EOF aborts the upload and nothing is sent to the hub (test
`a_call_cancelled_during_its_upload_is_not_sent`, with a fake hub that answers late).

## 6. The client

Nothing to change: `attachment` events have rendered since T16 (image inline, diff, cards),
whoever the author. The hub writes the event while the call is open, so it sits between the
call's `tool.call` and `tool.result` (seen in the live run); the feed closes the tool group
before it and draws the call with its result, then the file. Test: "the agent's hqc_attach
(T31)" in `client/tests/attachments.test.tsx`. The wire example `event.attachment.json` now
has the agent as author, as the hub writes it.

## 7. Live run

2026-10-25, home-mac, Claude Code 2.1.308, `claude-haiku-5-5`, `--max-turns 2`, one run:
`crates/hqc-hub/tests/live_attach.rs`. The agent's first model turn was `ToolSearch`
(`select:mcp__hqc__hqc_attach`), the second `hqc_attach {"path":"report.txt","caption":"live"}`;
the daemon uploaded 24 bytes, the hub wrote `attachment` (seq 6, between `tool.call` 5 and
`tool.result` 7) authored by the agent, kind `text`, and the signed link served the file's
content. The turn then ended with `error_max_turns`, as in T30.

## 8. Open questions

1. A session without a workspace cannot attach (its scratch folder is outside
   `allowed_roots`). Iteration 4 (bare model, `content`) is the place to decide whether the
   scratch folder counts.
2. `caption` has no limit of its own beyond the 64 KB of `args`. The client shows it in one
   line; a cap can come when an agent abuses it.
