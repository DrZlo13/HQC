# T41: questions and decisions

Files between sessions (SPEC 0.46). Before this, `hqc_send` and `hqc_ask` carried text only,
and an agent on home-mac fell back to pasting a file into the message text (2026-10-27).

## 1. The path of a file

```text
agent A: hqc_ask {text, session, files: ["out/board.png", "<artifact id>"]}
  A's daemon: resolve + read + PUT /worker/artifacts (the hqc_attach rules) for each path
  mcp.call {text, session, artifact_ids: [...]}          (no path reaches the hub)
hub: owner's + linked to A? -> peer.message.artifact_ids, linked to B
  B's worker: session.input.artifact_ids -> GET /worker/artifacts -> B's .hqc/inbox
B's turn: hqc_attach report.txt -> attachment event; final agent.message
hub: reply files = B's attachment events between the question and the final message,
     owner's only, linked to A -> mcp.result {from, reply, files: [{artifact_id, name, kind}]}
  A's daemon: GET /worker/artifacts -> A's .hqc/inbox/<request_id>/ -> result with paths
```

Both halves reuse what exists: the upload is `Attacher` of T31 (`upload_path`, shared with
`hqc_attach`), the downloads are `inbox` of T16 (`fetch_one`, shared by inputs, buffered
messages and replies), and the hub's permission to download is the `artifact_sessions` link
that T31 already checks on `GET /worker/artifacts`. No migration, no new route, no new frame.

## 2. The sender's side (`crates/hqc-worker/src/mcp/attach.rs`)

- `files` is a list of at most 32 strings. A string that parses as a uuid is an artifact id
  and passes as it is (an `hqc_attach` result, or an artifact the session got before); anything
  else is a path, checked exactly as `hqc_attach` checks one (inside the workspace as written,
  then canonical inside the workspace and `allowed_roots`, `O_NOFOLLOW`, a regular file, 100 MB,
  exactly the size seen on the handle). A file literally named like a uuid cannot be sent by
  that name; `./<uuid>` works.
- The daemon prepares the call like `hqc_attach` (the same "preparing" task, so a cancel or EOF
  during an upload stops it and nothing is sent) and sends `artifact_ids` instead of `files`.
  The hub's argument types have no `files`: a path can never reach it.
- The first refusal fails the whole call; files uploaded before it stay unreferenced and go
  with the daily artifact cleanup (§12), as for an `hqc_attach` whose call never came.
- `files: []` and no `files` send the arguments unchanged.

## 3. The hub (`crates/hqc-hub/src/peers.rs`)

- `checked_files`: at most 32 distinct ids, each an artifact of the caller's owner **and**
  linked to the calling session in `artifact_sessions` (uploaded for it, given to it in an input,
  or linked to it by a reply). Another owner's artifact, an unknown id and the owner's artifact of
  another session are all `not_found`; nothing is stored. Checked before the chain and the
  caps, like the target.
- The ids are linked to the target before the `peer.message` is stored, so that the input,
  which can go out the moment the event exists, never reaches a worker that may not yet
  download its files.
- The payload has `artifact_ids` only when there are files (old clients and the wire examples
  stay as they were).
- A repeat (§5.2) finds the stored event first and answers from it, as before; the files were
  linked by the first call.

## 4. The files of a reply

The hub, not the worker, decides which files belong to the reply: it already matches the final
`agent.message` to the question. `reply_files` takes the `attachment` events of the target
between the question's `seq` and the final message's `seq` (the final message's row is passed
to `on_final`), oldest first, each artifact once, at most 32.

- Only artifacts of the target's owner. A worker may write `attachment` events itself (only
  `user.message`, `approval.response`, `clarify.response` and `peer.message` are hub-only), and
  an `attachment` naming another owner's artifact would otherwise be linked to the asking
  session and downloadable by its worker. Test: `files_go_with_the_question_and_come_back_with_
  the_reply` writes such an event from B's worker and checks it is left out and stays `404`.
- "Files of its final message" in the task text: an `agent.message` has no files in the event
  schema, and Claude Code has no other way to attach than `hqc_attach`. So the reply's files are
  the turn's `hqc_attach`es; the tool descriptions tell the target so ("a file attached during a
  turn that answers an hqc_ask goes back to the asker").
- Files attached before the question (an earlier turn) are not part of the reply (test
  `a_reply_without_files_and_files_before_the_question`).
- The ids go into the stored `mcp.result` (`result.files: [{artifact_id, name, kind}]`), so a
  resend after `hello` and a repeated call carry them too; the asking worker downloads again
  into the same folder (a rename over the same names).

## 5. The asker's side (`daemon.rs`, `inbox::fetch_reply`)

- The connection acks the deferred `mcp.result` frame as before; the daemon's task that waits
  for the outcome downloads the files and only then answers the MCP call. The agent never sees an
  artifact id of a reply, only `{name, kind, path, size}` or `{name, kind, error}`.
- The folder is `<workdir>/.hqc/inbox/<request_id>/`: the call's id is a uuid the worker made, it
  is unique per call, and the hub knows it, so `session.cleanup` can name it
  (`db::inbox_events` adds the `request_id`s of the session's asks whose stored result has
  `files`). The sweep at start covers it after 14 days like any inbox folder.
- `path` is absolute (the inbox block of an input is relative to the working directory, but the
  agent reads a tool result, and Claude Code's `Read` wants absolute paths).
- Limits are those of an input: 100 MB per file, 500 MB per reply. A failed download (the hub
  refuses, over a limit, a disk error, a `.hqc` that is a symlink) is reported for that file and
  the reply text still comes back. A reply that arrives after the agent stopped is not
  downloaded at all (the call is gone).

## 6. The target's side

- A direct `peer.message` with files: nothing new on the worker. The hub's input frame already
  took `artifact_ids` from the payload, and the worker builds header + escaped text + the
  `Attachments:` block (SPEC §7.1, §12).
- A buffered message (`peer_prefix`): each entry now has its `event_id` and `artifact_ids`
  (additive, absent when empty). Its files go to `.hqc/inbox/<its event_id>/` and its block
  follows its text inside the "Messages from other sessions" block; the 500 MB of an input count
  the buffered files together with the input's own; a failure removes every folder this input
  created and fails the input as before. A folder per message, rather than the carrying input's
  folder, because the hub can name it for `session.cleanup` (`peer.message` with files is in
  `inbox_events` now) and two buffered files with the same name do not collide.
- Escaping (`[hqc peer`) applies to the whole text including the block, as for user input.

## 7. The client

A `peer.message` shows its files under its text, as a `user.message` does (`AttachmentView`).
The asker's feed shows the reply's files through B's `attachment` events in B's feed and the
paths in A's `tool.result`; nothing else was added.

## 8. Tool descriptions

`hqc_send` and `hqc_ask` now say: list files in `files` (paths in your workspace, 100 MB each,
or artifact ids from hqc_attach), never paste a file's content into `text`, the other agent gets
each file as a local path; `hqc_ask` adds that files the other agent attaches while answering
come back in `files` with a local `path` (or an `error`). `hqc_attach` says its `artifact_id`
can go in `files` and that a file attached while answering an ask goes back to the asker.

## 9. Live run

2026-10-27, home-mac, Claude Code 2.1.308, `claude-haiku-5-5`, `--max-turns 2`, one run:
`crates/hqc-hub/tests/live_files.rs` (folder `tmp/spike-t41-live-<uuid>`, removed after).
Sessions `live/asker` (workspace `ws-a`) and `live/helper` (`ws-b`, with `report.txt`, 53
bytes). A called `mcp__hqc__hqc_ask {"session": "live/helper", "text": "Call the hqc_attach
tool once with path report.txt, then reply with exactly one word: done"}`; B called
`hqc_attach` (the daemon uploaded 53 bytes, the hub wrote `attachment` seq 4), then answered
`done` (seq 6, `in_reply_to` the question); the hub answered the ask with one file; A's
`tool.result` was `{"files": [{"kind": "text", "name": "report.txt", "path":
".../ws-a/.hqc/inbox/<request_id>/report.txt", "size": 53}], "from": "live/helper", "reply":
"done"}` and the file there had the same content.

B needs two model turns (the attach, then the answer). To stay within `--max-turns 2` the test
starts the CLI through a wrapper with `ENABLE_TOOL_SEARCH=false`: on 2.1.308 this loads the hqc
tools up front, and neither agent spent a turn on `ToolSearch` (B's first model turn was the
`hqc_attach` call). Production is unchanged (no wrapper, no `--max-turns`).

## 10. Open questions

1. A file named like a uuid is taken as an artifact id (section 2). Prefixing `./` is the way
   around it; an explicit `{artifact_id}` object form would remove the ambiguity if it bites.
2. The reply's files are only `hqc_attach`es of the answering turn. If Claude Code ever gives
   the final message attachments of its own, they would join here.
3. The hub's cap of 32 files on a reply drops the rest with a warning in the hub's log only; the
   asker does not learn that files were left out.
