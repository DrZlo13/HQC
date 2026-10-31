# T42: questions and decisions

Replies that come back as a send (SPEC 0.47). The 2026-10-31 run: A asked B with `hqc_ask`
and `files`; B changed the file and sent it back with `hqc_send` and `files` instead of
`hqc_attach`, so the ask's reply had no file and the send waited in A's buffer until the user's
next message.

## 1. Which send joins the reply

A send from B to A joins the reply when, at the time of the call, A has an open ask whose
question (`payload.event_id`) is the input of B's current turn. The current turn is found as
for the chain (`peers::call_input`, shared with `call_chain`): the input the worker names in
`turn_input` if it is valid, else B's last delivered input. So:

- a send from the turn on the question → with the reply (`delivery: with_reply`);
- the final message first, then a send (another turn, or the same process after the ask
  closed) → a normal send (buffered or queued);
- B busy with another turn when the question came (the ask is open but not B's current
  input) → a normal send;
- B's send to another session, or another session's send to A → unchanged.

No new table and no migration: the joined send is still a `peer.message` in A (so the feed and
the search show it, and its `event_id` keys the repeat as before), stored without `delivery`
and not put into `peer_buffer`; the ask's `payload.joined` lists it. The client already shows a
`peer.message` without `delivery` as a plain message; nothing changed there.

## 2. The race with the final message

B's send and B's final message are separate frames and can be handled concurrently. Both lock
the ask's row (`SELECT … FOR UPDATE`): the send from its check until the message is on
`payload.joined`; the reply (`on_final`) from reading `joined` until it closes the ask in the
same transaction. A send that loses finds the ask closed and takes the normal path. The send's
`peer.message` is stored through the pool while the row is held (the event insert does not
touch `pending_requests`, so there is no lock cycle).

Known gap: if the transaction's commit fails after the event was stored, the message exists in
A without a buffer row and without being joined; the call fails (`internal`), and its repeat
finds the event and answers `buffered` without buffering it again (re-buffering on a repeat
could deliver it twice after a delivery). It needs a failed commit between two statements.

## 3. The reply

- Text: the final message, then for each joined send in `seq` order the separator
  `\n\n--- sent with hqc_send while answering ---\n` and its text
  (`peers::JOINED_SEPARATOR`). A plain marker line the agent reads; no escaping is needed (the
  reply is a tool result, not an input).
- Files: the turn's `attachment`s as in T41, then the joined sends' `artifact_ids` in order,
  each once, at most 32, owner's only. They are linked to A when the send is stored (the send
  links its files to its target), so the asking worker downloads them into
  `.hqc/inbox/<request_id>/` by the T41 path, nothing new on the worker.
- A repeat of the send answers `with_reply` again (found through `payload.joined`), a repeat of
  the ask answers the stored result with the joined text and files.

## 4. An ask that gets no reply

A timeout, a lost target turn, `mcp.cancel` and the asking worker's restart close the ask
without a reply. The joined sends would then be lost although B was told `with_reply`, so they
go to A's send buffer at that moment (`peers::rebuffer`, with the usual eviction) and reach A's
agent with its next input. `close_ask` closes with `UPDATE … RETURNING payload`, which waits
for a send holding the row and then sees it. A target with `wake_on_send` still gets them
buffered, not queued: the case is rare and buffering never starts a turn nobody asked for.

## 5. Telling the target

- The ask's header now ends `| reply with your final message | return files with hqc_attach or
  in files of hqc_send]` (`peer::RETURN_FILES`); the worker builds it for every input, so even
  an agent whose tool schema is stale (section 6) reads it.
- `hqc_send`: "While you answer an hqc_ask, a send to the asker goes back with your reply (its
  text after your final message, its files with the reply's files; `delivery: with_reply`)."
- `hqc_ask`: files the other agent "attaches with hqc_attach or sends to you with hqc_send
  while answering" come back in `files`, and such a send's text follows the final message.
- `hqc_attach`: "so do the files of an hqc_send to the asker from that turn".

## 6. [verify] A stale tool schema after ToolSearch

A said `files` was not in its description, yet its call with `files` went through.

Test `crates/hqc-hub/tests/live_tool_schema.rs`, 2026-10-31, home-mac, Claude Code 2.1.308,
`claude-haiku-5-5`, `--max-turns 2`, `ENABLE_TOOL_SEARCH=true`, one run (two `claude -p`
processes on one session id, folder `tmp/spike-t42-schema-<uuid>`, which the test keeps for
reading and which was removed by hand after: it holds a transcript copy with
`api-request-blob` records). The MCP
server `hqc` was a stand-in (`probe.py`) listing `hqc_proto::mcp::TOOLS`: the first process got
`hqc_send`/`hqc_ask` as before T41 (no `files`), the second, a `--resume`, as they are now.
Both processes called `tools/list` on their own server (the server's log).

- First process: `ToolSearch {"query": "select:mcp__hqc__hqc_send"}`; the result in the
  transcript is only `[{"type": "tool_reference", "tool_name": "mcp__hqc__hqc_send"}]`, and
  the CLI adds an attachment `{"type": "deferred_tools_record", "entries": [{"name":
  "mcp__hqc__hqc_send", "description": …, "input_schema": {…text, session…}, "defer_loading":
  true}]}` with the whole definition as loaded. Answer: `session, text`.
- Second process (`--resume`, the server now lists `files`): asked without tools for the
  parameters and the last sentence of the description, the model answered `session, text` and
  the old last sentence. Its request had the same shape hash as the first process's second
  request (the transcript's `api-request-shape` records), i.e. the old definition was sent.

So on 2.1.308 a loaded tool's schema is frozen in the transcript and a resume replays it; the
server's current `tools/list` does not replace it. That explains the 2026-10-31 run: A's
session had loaded `hqc_ask`/`hqc_send` before the T41 update. The call still worked because
the model may pass an argument outside the schema it sees and the hqc server checks its own.

What hqc could do, and why not now:
- Rewrite the `deferred_tools_record` in the transcript on resume: hqc does not edit Claude
  Code's files (format not ours, changes between versions).
- Start Claude Code with `ENABLE_TOOL_SEARCH=false`: all hqc tools would be in every request
  from the server's current list (not checked whether a resume then drops the recorded
  schema), at the cost of their full descriptions in every session's context. A trade-off for
  the user, not part of this task.
- The header of an ask says how to return files (section 5): the part that mattered for the
  run is now fresh in every input.

## 7. Live run

`crates/hqc-hub/tests/live_reply_send.rs`, 2026-10-31, home-mac, Claude Code 2.1.308,
`claude-haiku-5-5`, `--max-turns 2`, `ENABLE_TOOL_SEARCH=false` (as `live_files`), one run,
folder `tmp/spike-t42-live-<uuid>` (removed after). A called `mcp__hqc__hqc_ask
{"session": "live/helper", "text": "Call the hqc_send tool once with session 'live/asker', text
'fixed' and files ['report.txt'], then reply with exactly one word: done"}`. B called
`mcp__hqc__hqc_send {"files": ["report.txt"], "session": "live/asker", "text": "fixed"}`; B's
daemon uploaded the 53 bytes, the hub logged "peer message goes with the reply" and B's tool
result was `{"delivery": "with_reply", "to": "live/asker"}`; B answered `done` (`in_reply_to`
the question). The hub answered the ask with `files=1 joined=1`; A's `tool.result` was
`{"files": [{"kind": "text", "name": "report.txt", "path":
".../ws-a/.hqc/inbox/<request_id>/report.txt", "size": 53}], "from": "live/helper", "reply":
"done\n\n--- sent with hqc_send while answering ---\nfixed"}`, the file there had B's content,
and A's feed had B's `peer.message` without `delivery`.

## 8. Open questions

1. `ENABLE_TOOL_SEARCH=false` for hqc sessions (section 6), or a note in the client that a
   session started before a worker update does not know new tool arguments.
2. A send joined to an ask that then fails goes to A's buffer even when A wakes on sends
   (section 4).
3. B may also `hqc_ask` A back from the answering turn; that is still a separate ask (and a
   likely deadlock until a timeout, as before).
