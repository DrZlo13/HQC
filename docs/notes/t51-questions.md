# T51: decisions and differences from the mockups

Messages between sessions in the feed, by `docs/design/redesign-2026-11-01` (`Session`,
`AskingSession`, `MSession2`), SPEC 0.52. No live `claude` run: the client tests use fixtures
built from the events of the T41 and T42 live runs (`client/tests/fixtures/t51-exchange.json`,
`t51-reply-send.json`). No screenshots were compared (no headless browser here), and the check by
hand on a phone and a desktop is still to do.

## 1. Where each block comes from

| Block | Session | Source |
|---|---|---|
| ↙ Question from X / Message from X | answering | its `peer.message` (`mode: ask` / `send`), text and `artifact_ids` |
| ↗ Reply to X · delivered | answering | the final `agent.message` whose `in_reply_to` is a question in this feed, the `attachment`s between the two, the `usage` after it, its `with_reply` sends |
| ↗ Sent to X · delivered | asking | the agent's `tool.call` of `hqc_send` / `hqc_ask` (`text`, `session`, `files`) and its result |
| ↙ Reply from X | asking | the `tool.result` of `hqc_ask` (`from`, `reply`, `files`), at the result's place in the feed |

The blocks replace the old peer card, the plain final message, the standalone attachments of
the answering turn and the rows of these calls in the tool groups. A reply whose question is
not loaded (older than the loaded history) stays a plain agent message.

## 2. The delivery time in the asking session (the hub's minimum)

The asking feed has the call and the result; the time is on the `peer.message` in the other
session. The hub links them by one number: the results of `hqc_send` and `hqc_ask` now carry
`message_seq`, the `seq` of the message in the session it went to. The client resolves the
address of the result (`to` / `from`, else the call's `session`) to a session of the user from
the lists it already has (project slug + session slug), and reads that one event with the
existing API (`GET /api/sessions/{id}/events?after_seq=<seq-1>&limit=1`): its `delivered_at` is
the time, its `artifact_ids` are the files (Open / Download), before that the names in `files`.
The read is cached once delivered and repeated at most every 15 s before that.

Rejected: matching the call to the message by text and time (fragile, and a full scan of the
other session); the tool-use id in the MCP call's `_meta` (not verified on a real build, and
CLAUDE.md wants that first); a new event or route. The cost of `message_seq` is a few bytes in
the agent's tool result.

An ask shows "waiting for a reply" until its result, because `message_seq` comes with the
result; the question is delivered by then. A send before its message is delivered shows
"queued", "waits for its next turn" (buffered) or "goes with the reply".

Buffered sends never had a delivery time: they are stored without `delivery`. They now get
`delivered_at` (still without `delivery`) when the input that carried them is acknowledged
(`peers::delivered`). Without this "delivered" would never show for a send to a session that
does not wake on sends, the default.

## 3. `reply_delivered_at`

- On the final message `peers::on_final` stores the reply's id on the ask
  (`payload.reply_event_id`); the result gets `message_seq` there too.
- `peers::result_acked` replaces `db::result_delivered` for a worker's `ack` of an `mcp.result`
  and for a repeat of the ask answered from the stored result: the first time is kept on the ask
  (`payload.reply_delivered_at`). A worker's `error` for the frame, the results dropped after a
  worker restart and an ask closed with an error keep calling `db::result_delivered` and are no
  delivery (test `a_rejected_result_is_not_a_delivery`).
- `db::events_for_clients` adds the field in `GET .../events` and the `/api/ws` backfill, by one
  query per page that has final messages with `in_reply_to`. The field is in
  `hqc_proto::Event` as optional, never stored.
- Live: the ack comes after the reply was sent to clients, so the hub sends the reply event
  again (a new `Notice::Resent`, same `seq`) to subscriptions that already had it; the client's
  timeline takes `reply_delivered_at` from a copy of an event it has, as it takes `delivery`.
  Not a new event, not a new frame type.
- No migration: the ask's `payload` (hub-internal jsonb) holds both fields.

## 4. Files of a reply in the asking session

The asking worker gave the agent `{name, kind, path, size}` without the artifact id (T41), so
the client could not open the file. `ReplyFile` now has `artifact_id` too; the block opens and
downloads by it (the hub's copy, the same bytes as the local copy) and shows the local `path`
under the file. The agent sees the id as well and may pass it on in `files`.

## 5. A send folded into a reply (T42)

- Asking side: the reply text is split at `--- sent with hqc_send while answering ---`; each
  part follows a separator line with those words. The same send is also a `peer.message`
  without `delivery` in the asker; the client hides a `mode: send` message without `delivery`
  whose sender address and text equal a part of an ask's reply from that address.
- Answering side: an `hqc_send` call between the question and the final message whose result
  says `with_reply` goes into the "Reply to" block after the separator, with its files as they
  arrived in the asker (`message_seq` of its result, read from the asker's session). Before the
  final message it is a "Sent to" block with "goes with the reply".

## 6. Links

The other session's address links to `#/s/<id>` when the user has it (`App.findSession`, by id
or address); a deleted or unknown session is plain text, and nothing is read from it.

## 7. Not done from the mockups

- The key-value table, footnote and caption in the mockup's "Reply to" are the agent's
  Markdown (a table renders as a table); no special layout for them.
- The mockup's "Reply from" has no token line; neither has the block (the tokens belong to the
  other session's turn).
- On the phone the mockup shows only "Open" on a file; the block keeps "Download" too (the
  shared attachment card).
- A copy button sits in the head of an incoming message (the mockup has none; the old card had
  one and the feed's other messages have one).
- "↗ Sent to" while an ask waits shows no delivery time (section 2).

## 8. Open questions

1. A reply after the ask timed out is still drawn as "Reply to X", without a delivery time.
2. The asker hides a joined `peer.message` by comparing texts; two identical sends from the
   same session, one joined and one buffered, would both be hidden while the reply is loaded.
