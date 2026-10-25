# T32: questions and decisions

## 1. A repeated `mcp.call` acts once (the open point of T30, notes t30 §3 and §7.3)

The worker sends a call again when its answer was lost (the connection dropped after the hub
ran it). Both tools now key what they did by the calling session and the call's
`request_id`:

- `hqc_send` and `hqc_ask` store their `peer.message` under `event_id` = uuid v5 of
  `"<calling session id>/<request_id>"`. Events are unique by `event_id`, so a repeat stores
  nothing and delivers nothing. A repeated `hqc_send` looks the event up first and answers the
  same `{to, delivery}` from it (`delivery` is `queued` when the event has a `delivery`,
  `buffered` when not), even if the target was archived since.
- A repeated `hqc_ask` finds its `PendingRequest` (`session_id` = the asking session, `kind:
  ask`, `request_id`): still open → `ack {}` again (the answer comes later as before); closed
  with an answer → that `mcp.result` under the repeat's frame `id`; closed by `mcp.cancel` →
  `unavailable` (the worker never repeats a cancelled call, so this is only defensive).

So no table of results was needed: the stored event and the request are the record. The
`mcp_call` kind of §3 PendingRequest stays unused; tools that create something else (T34's
session, T35's schedule) can key it the same way or use that kind.

The hub's registry got a second list, `mcp::CALLS`, whose handlers get the whole `McpCall`
(the `request_id`) and may answer `Answer::Later`; `REGISTRY` and its handler type are
unchanged, so T31's entry merges without edits.

## 2. Where the ask lives and who gets the answer

The ask is a row of the **asking** session (`session_id`), with `worker_id` = its worker and
`payload {target_session_id, event_id, to}`. This makes the existing paths right without
special cases: `drop_undelivered_results` and `undelivered_results` already go by the
session's worker; the state of the target (`waiting` counts only approval/clarify) is not
touched; `sessions_to_restart` does not restart anything for it.

The answer (`mcp.result` frame in `result`) goes:
- at once if the asking worker is connected;
- after its next `hello` (`peers::after_hello`) — no `session.start` is needed, the worker's
  connection routes `mcp.result` by `request_id` itself (test
  `a_deferred_answer_comes_after_a_reconnect` on the worker,
  `the_answer_waits_for_an_offline_worker_and_a_restart_closes_the_ask` on the hub);
- never after a restart (a new `boot_id`): the existing `drop_undelivered_results` marks an
  answer that came meanwhile as delivered, and `peers::worker_restarted` closes asks still
  open with `worker_restart` (stored, not sent).

## 3. The chain is set by the hub, not the MCP server (§7.1 was wrong against the code)

§7.1 said the hub gives the turn's chain to the MCP server in the first call's reply and the
server sets it on outgoing messages. The MCP process lives as long as the agent process and
sees no turns: it cannot know when the chain it holds stops being the current one. The hub
can: inputs reach the agent one turn at a time in `seq` order, so the calling session's last
`delivered` input started the current turn, and its `chain` is the chain of the call. So the
hub sets the chain itself; the MCP server keeps no state (§7.2 changed).

Rules: a `user.message` has no chain, so a call from a human's turn starts one (`depth` 1 for
an ask, 0 for a send, `hops` 1); otherwise `hops` + 1, and `depth` + 1 for an ask. `depth` > 3
or `hops` > 20 → `limit_exceeded`. Buffered sends carried in `peer_prefix` never have
`delivery`, so they cannot be mistaken for the turn's input.

Known gap: the input's `ack` bypasses the outbox and is lost with a dropped connection; if a
call of that turn reaches the hub after the reconnect but before the re-sent input's `ack`,
the hub takes the previous input's chain. That needs a reconnect in the first seconds of a
turn; the limits are a safety net against loops, not a security boundary.

## 4. The send buffer

- A buffered send is stored without `delivery` (not `pending`, which would start a turn, and
  no new enum value: the client parses `delivery` strictly) and kept in `peer_buffer`
  (migration `0015`).
- `carried_by`: when the hub sends an input it takes every buffered message not carried by
  another input still `pending` (an input that failed releases them), in `seq` order, into its
  `peer_prefix`. The input's `ack` deletes those rows. A send arriving while that input is on
  its way waits for the next input (test `sends_wait_in_the_buffer_for_the_next_input`).
- Eviction keeps the newest 50 and at most 64 KB (sum of the texts' bytes); one `system
  peer_evicted` per send that evicted, with the count.
- The worker replaces a queued input by the hub's re-sent frame (its `peer_prefix` is what the
  hub will clear on the `ack`). An input already handed to the runtime is answered as before
  and keeps its first `peer_prefix`; a buffered message that the hub added to the re-sent frame
  would then be cleared without having reached the agent. That needs a reconnect while the
  input is in flight plus a send in that window; left as is.
- `wake_on_send` is read when the message is sent. There is no API or UI to set it yet (no
  user of it in this task); tests set it in the database.

## 5. The target

- Address or id of a session of the caller's owner, not archived, not a service session;
  anything else `not_found` (another owner's session too, as T30).
- The calling session itself → `invalid_args` for both tools (a send to oneself would only
  land in one's own next input).
- `locked_by: vscode` → `unavailable` for `send` as well as `ask`: the API refuses user input
  to such a session ("no silent queue", §9.3), and a buffered message would sit there until a
  takeover the agent knows nothing about.

## 6. The reply

- Any final `agent.message` with `in_reply_to` = the question's `event_id`, stored from the
  worker or the mirror (not one the hub writes). The result is `{from, reply}`.
- A target turn lost or stopped without a final message gives no reply; the ask waits for its
  timeout (600 s by default). Closing the ask early on `turn_lost` of the target would be
  nicer; not needed for the criterion, a candidate for later.
- `timeout_s` is cut to `ask_timeout_s` (the runtime's tool timeout is `ask_timeout_s` + 60 s,
  so a longer wait would be cut by the runtime first).
- The turn watchdog skips a session with an open ask (it is waiting, not silent): with
  `HQC_TURN_WATCHDOG_S` below the ask timeout it would otherwise kill the asking turn.
- `mcp.cancel` closes the ask without a result and writes `system ask_cancelled` to the
  asking session before the `ack` (so a test that waits for the `ack` sees the event).

## 7. Progress notifications

The MCP server sends `notifications/progress` every 30 s for every open call that has a
`progressToken` (`progress` 1, 2, …, message "waiting for the hub"); fast calls end before the
first. Whether Claude Code 2.1.308 sends `_meta.progressToken` with `tools/call` was not
checked: the live run's ask was answered in about a second, before the first notification.
It does not matter yet: the ask's timeout (600 s) is far below the 30-minute idle cutoff the
notifications prevent (2.1.307, §7.1). Worth a look if `ask_timeout_s` ever grows past it.

## 8. The client

- `peer.message` shows "Question/Message from `<address>`" (`from`), else the old end of the
  session id.
- A waiting ask in the sending session: the activity line "Waiting for a reply from
  `<session>`" with the timer, from the runtime's own `tool.call` of `hqc_ask` (Claude Code
  names it `mcp__hqc__hqc_ask`) until its `tool.result`. No event of its own: the call is
  already in the feed.
- Captions for `ask_cancelled` ("Question cancelled") and `peer_evicted` ("Messages dropped").

## 9. Live run

`crates/hqc-hub/tests/live_peers.rs`, 2026-10-25, home-mac, Claude Code 2.1.308,
`claude-haiku-5-5`, `--max-turns 2`, one run: session `live/asker` called
`mcp__hqc__hqc_ask {"session": "live/helper", "text": "Reply with exactly one word:
pineapple"}`; the hub put the `peer.message` into `live/helper` (chain depth 1, hops 1); B's
turn took about a second and answered `pineapple` with `in_reply_to` = the question; A's
`tool.result` was `{"from": "live/helper", "reply": "pineapple"}`; B's transcript has the line
`[hqc peer from live/asker | mode=ask | request_id=… | reply with your final message]`. A's
turn then ended with `error_max_turns`, as expected with `--max-turns 2`.
