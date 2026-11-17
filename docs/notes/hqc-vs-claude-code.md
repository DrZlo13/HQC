# hqc against Claude Code and the VS Code extension: what we handle wrong

A comparison of what the Claude Code CLI sends and what the VS Code extension makes of it
(the reference `docs/notes/claude-code-api.md`, cited as "ref. N.N") with what hqc does: worker,
hub, client. Four lists: "wrong" (we show the wrong thing or in the wrong place), "partly" (some
cases handled), "not handled" (the data arrives and is lost), "not used" (what the CLI and the
extension can do and we do not). Limits of the first version ("forget about viewing diffs" and the
like) are in the lists on equal terms, marked "deferred in the first version", with the owner's words.

The comparison was made on our build — master plus our unmerged changes. Commits `bf4d73d` and
`c1f1771` are not in this repository: the file paths exist in master too, the line numbers are from
these commits; how master differs is said at the items ("Upstream") and in the last section. "§3",
"SPEC §2.7" are notes of our project, not `docs/SPEC.md` of this repository; "SPEC hqc" is
`docs/SPEC.md` of this repository.

## What was compared

- Worker — build `bf4d73d` (tree `../hqc/.claude/worktrees/worker-str`); all six workers run
  `4443c03`, which is `bf4d73d` plus the MCP server's instructions text (`crates/hqc-proto/src/mcp.rs`),
  with the same stream parsing. Paths `crates/hqc-worker/…` and `crates/hqc-claude-jsonl/…` are at
  this commit.
- Hub and client — branch `new-look` `c1f1771`, which our hub runs. Paths `crates/hqc-hub/…`,
  `crates/hqc-proto/…`, `client/…` are at this commit. After it `new-look` has three client commits
  (`ec52bf0`, `1ff2acf`, `3679f00`); of them `ec52bf0` and `3679f00` cover the edit diff in the feed
  (the "Edit diff and file" line in "not used"), the rest does not change the comparison.
- Upstream — `origin/master` `bcb10b1`. Where nothing is said about upstream, it is the same there.
- CLI 2.1.312 (`vendor-src/cli-2.1.312.pretty.js`, references `cli:N`), the extension 2.1.319
  (`vendor-src/ext-2.1.319-webview.js` — `web:N`, `ext-2.1.319-extension.js` — `ext:N`).
- Live runs — test bench agent-node-227, `/home/claude/cc-api-probe/captures/`: the reference's
  earlier captures and three new ones — `w1-clear` (`/clear` mid-session), `w2-resume-old` (`--resume`
  of the old id after `/clear`, as the worker will do after a restart), `w3-stop-bg-agent`
  (`interrupt` without `initialize`, as the worker sends it, with a background subagent running), and
  runs with the worker's flags (`--permission-prompt-tool stdio`, `--setting-sources user,local`,
  `claude-vscode`, mode `auto` or `default`): `x1-handback`, `x2-perm-*`, `x2b-perm-*`, `x3-ambient`,
  `x4-session-mirror`, `x5-preview-*` and `x5n-*`, `x6-wake-*`. Two cases were captured in our hqc on
  trial haiku sessions of the agent-trader project on agent-node-227 (a working folder with the mirror
  on): `/clear` and a wakeup timer when the process is unloaded for idleness. What was not captured
  live is said at the item.
- Frequencies — `vendor-src/scripts/count_results.py` over 52 of the laptop's 1648 main transcripts
  (3.2 %; those changed in the two days before 2026-11-11 22:47 UTC, the list is
  `vendor-src/scripts/recent.txt`; VS Code sessions and the laptop worker's sessions mixed). Run of
  2026-11-12: 12 894 tool results, of them 410 with an image (3.2 %), 1179 with text longer than
  4096 bytes (9.1 %).

## In short

| List | Items |
| --- | ---: |
| Wrong | 14 |
| Partly | 18 |
| Not handled | 9 |
| Not used | 29 |

Most visible to the user:

1. **Stop kills background subagents.** Fixed by one `initialize` frame at process start
   ("Stop kills background subagents").
2. **A worker restart cuts the turn of every running session**, although the CLI can continue an
   interrupted turn by itself after `--resume` ("Continuing a turn after a restart").
3. **`/clear` breaks the session**: the process moves to a new conversation, the worker stays on the
   old one, the mirror creates an extra session from the new file, the session cost in the header
   drops; after unloading, the agent again remembers what was before `/clear`.
4. **API request retries and hook messages are not visible**: when the API is overloaded, the session
   sits in "Agent is working" for minutes without a single word.
5. **An interrupted answer looks finished**: after Stop there is no "interrupted" mark, and the
   reason for an early end of a turn (an error, the turn limit) the worker sends, but the client does
   not draw it.
6. **Images in results are "[image]", a result longer than 4 KB is cut without "Full result"**:
   3.2 % and 9.1 % of results in the sample.
7. **A message the agent will take only on the next turn stands above the answer that did not
   see it.**
8. **In Auto mode every background subagent's report in the feed is a service phrase** "This agent's
   report was delivered to you as a message…": the CLI in `auto` mode (this is how the worker starts
   hqc sessions in Auto) delivers reports through `SubagentHandback`, and hqc takes the line's text
   from the field meant for the model.

---

## Wrong — we show the wrong thing or in the wrong place

**Stop kills background subagents and workflows.**
- Seen: you press Stop to interrupt the answer, and the background subagents are stopped too, while
  with `CLAUDE_CODE_ENTRYPOINT=claude-vscode` every `Agent` goes to the background; the feed keeps a
  line with their description, without the word "stopped".
- hqc: the button `client/src/ui/Chat.tsx:453–463` → `POST /stop` (`crates/hqc-hub/src/api.rs:1582–1592`)
  → the worker sends a bare `interrupt` (`crates/hqc-worker/src/adapter/claude_code/engine.rs:482–492`)
  and never sends `initialize` (`crates/hqc-worker/src/adapter/claude_code.rs:461–477`).
- Claude Code: without `initialize {perTaskStopAffordance: true}` an interrupt stops background
  `local_agent` and `local_workflow` (`cli:1504650–1504655`, `cli:742336–742345`); the extension sets
  the flag (`ext:138053`). Ref. 1.3, 3.3, 5.4. Live run `w3-stop-bg-agent`: line 60 — `task_started
  {task_type: local_agent, is_backgrounded: true}` on a request without `run_in_background`; after
  `interrupt` — line 105 `task_updated {status: killed}`, 106 `task_notification {status:
  stopped}`, 113 `result {terminal_reason: aborted_tools}`.
- Fix: the first stdin frame — `control_request initialize {perTaskStopAffordance: true}`.
  Stopping one subagent — `stop_task` ("Stop one subagent").

**`/clear` takes the CLI to a new session, the worker stays in the old one.**
- Seen: after `/clear` the agent works in a clean conversation, the feed runs on without a separator;
  the session cost in the header drops to zero. After an unload for idleness, a worker restart or a
  move, the agent again continues the conversation from before `/clear`.
- hqc: the transcript id is taken only from `session.start` (`claude_code.rs:422–431`, `598–610`);
  a new `session_id` from `system/init` is only remembered for subagent files (`engine.rs:719`);
  own records are registered under the old id (`claude_code.rs:670`, `974`); the spending base is
  not reset (`engine.rs:868–873`); the `conversation_reset` frame is not parsed
  (`engine.rs:690–781`).
- Claude Code: ref. 2.1 `conversation_reset` ("after it `total_cost_usd` is counted anew"),
  `cli:992300–992333`. Live run `w1-clear`: line 18 — a `conversation_reset {trigger: clear}` frame
  (its own type, not a `system` subtype); `init.session_id` before — `8ff09d1d…` (line 3), after —
  `4d4b2f4f…` (lines 19, 25); `total_cost_usd` 0.00232649 → 0 → 0.00232689; two files on disk.
  `w2-resume-old` (`--resume 8ff09d1d…`): the conversation from before `/clear` came back, the cost
  total continues the old one. The extension clears the feed on a `system/init` with a different
  `session_id` (`web:171347–171386`).
- Live in hqc (trial session `agent-trader/session-8`, 2026-11-11 23:18 UTC): after `/clear` a new
  conversation file (`b7c1de4d….jsonl`) appeared on the node next to the old one, and a second later
  the project got a session `session-9` "Session imported from Claude Code": its first user message is
  the raw `<command-name>/clear</command-name>…`, the second is a copy of the next message, and there
  is no agent answer. The mirror does not count the new file's records as its own
  (`crates/hqc-worker/src/mirror/registry.rs:71`, `89–101`). To the question "what did I ask before"
  the agent answered "NOTHING"; the turn after `/clear` is recorded with `cost_usd` 0, the session
  cost fell from $0.0088 to $0.0033.
- Fix: on `conversation_reset` move the session to the new id (`RuntimeSessionId`), own records
  there too, zero the spending base, a "Conversation cleared" line; or do not let `/clear` through
  and offer a new session.

**An interrupted answer looks finished, there is no "interrupted" mark.**
- Seen: an answer cut off by Stop or by an incoming message stands as an ordinary final agent
  message; after Stop during a tool, nothing in the feed says the turn was interrupted.
- hqc: the worker takes an `assistant` with `aborted: true` like any text and closes it with
  `final: true` (`engine.rs:927–949`, `1266`); it drops `user` text lines (`[Request interrupted by
  user for tool use]`) (`engine.rs:1237–1245`; SPEC hqc: "mapped to nothing"); the hub writes no line
  on `POST /stop`.
- Claude Code: ref. 2.1 `assistant` (`aborted`), 3.2, 5.4. Live run `p9-now-during-text`: line 59 —
  `assistant {aborted: true}`, 63 — `result {terminal_reason: aborted_streaming}`. The extension draws
  "Interrupted" and "Tool interrupted" (`web:161382`, `web:161386–161445`, ref. 8).
- Fix: on `aborted: true` or `terminal_reason: aborted_*` mark the message interrupted and add an
  "Interrupted" line.

**A message that waited for the end of the turn stands above the answer that did not see it.**
- Seen: you wrote while the agent waited for an answer to a permission request, or in the seconds
  before the final answer, or typed a slash command during a turn — the message sits in the feed
  above all the following work and the answer, yet it was answered only on the next turn; while it
  waits, it is labeled "awaiting delivery", as with a worker that is off.
- hqc: input is placed by `seq` at the moment of sending; it goes below the text only with
  `during_text` (`crates/hqc-hub/src/events.rs:297–299`, `client/src/timeline.ts:401–423`); the
  worker does not merge in input while a request is open, nor slash commands and schedule runs (SPEC
  hqc §17.2), and the CLI does not merge in during the final answer (ref. 5.5); the label is
  `client/src/ui/cards.tsx:545`. The hub knows when the agent took the input: `delivered_at` from the
  worker's `ack` (`crates/hqc-hub/src/delivery.rs:241–276`).
- Extension: keeps its own message at the bottom (`web:169128–169134`, `web:260461–260471`) and puts
  it into the feed on `command_lifecycle started` or the echo (`web:169215–169254`, ref. 9).
- Fix (client only, the data is there): place a message that was not merged in by `delivered_at`;
  while it waits during a running turn — at the bottom of the feed, labeled "queued for the next turn".

**A call without a result "runs" forever; the activity line shows an old command.**
- Seen: after a worker restart or an agent crash, the call group of the previous turn is "running"
  forever; in a new turn, until the agent calls a tool, the bottom shows "Running $ <old command>
  2:13:05".
- hqc: `groupState` is "running" while the call has no result (`client/src/ui/feed.ts:205–209`);
  `runningCall` takes the feed's last `tool.call` without a result, whatever turn it belongs to
  (`client/src/timeline.ts:298–307`).
- Extension: no result and the session not busy — "failure" (`web:161966–161975`, ref. 8).
- Fix: a call without a result that is followed by an end of turn in the feed — "not finished";
  `runningCall` searches only after the last end of turn. How many such calls the hub's database
  holds — not measured.

**The hub's watchdog declares a long quiet call a lost turn.**
- Seen: an MCP tool call longer than 15 minutes — "turn_lost · no events for 900 s", the session in
  Error, although the agent works and answers later.
- hqc: the watchdog is `crates/hqc-hub/src/watchdog.rs:42–66`, default limit 900 s
  (`crates/hqc-hub/src/config.rs:331`); any worker event resets the clock
  (`crates/hqc-hub/src/app.rs:300–306`), including `tokens.rate`, but the worker sends it only when
  the rate changes (`crates/hqc-worker/src/supervisor.rs:1724–1728`) — on a quiet call the rate
  falls to 0 and there are no events.
- Claude Code: a long call of the main thread gets a `tool_progress` "heartbeat" every 30 s
  (ref. 2.1, live run `p3p5-bash-heartbeat`); the worker does not read it (`engine.rs:780`). The
  extension has no watchdog.
- Fix: on `tool_progress` the worker sends the hub an activity frame. Rare: a foreground Bash is
  10 minutes at most by default, and the watchdog does not count `hqc_ask`.

**A background subagent's report is replaced by a service phrase meant for the model.**
- Seen: in a session in Auto every background subagent ends in the feed with the line "This agent's
  report was delivered to you as a message from "…" (its SubagentHandback call). Read it there; it is
  not repeated here.", and the report itself is absent; the agent's retelling of it comes in the next
  turn, which the CLI starts by itself.
- hqc: the line's text is `summary` (`engine.rs:1594–1600`), `handback_report` is not read; Auto in
  hqc is `--permission-mode auto` (`crates/hqc-worker/src/adapter/claude_code.rs:512–515`).
- Claude Code: the report through `SubagentHandback` is on in `auto` mode unless turned off by the
  `CLAUDE_CODE_SENDMESSAGE_HANDBACK` variable (by default the flag `tengu_lively_waffle` = `true`,
  `cli:194998–195005`); "When present, `summary` holds harness notes written for the model"
  (`cli:108830`), "render it instead of `summary`, with its `warning` … above its `text`"
  (`cli:108836`); ref. 2.1 `task_notification`, 5.6. Live run `x1-handback` (sonnet, `auto`): line
  85 — `task_notification {status: completed, handback: send}`, `summary` is this phrase,
  `handback_report: {text: "HANDBACK-DONE (…)"}`; then the CLI opened a turn by itself, and the agent
  retold the report.
- Fix: with `handback_report` — its `text` (and the `warning` above it) instead of `summary`.

**The CLI's service tasks count as the session's work.**
- Seen: the session stays in Running and is not unloaded for idleness while the CLI does its own
  background work (saving memory, watchers); their end can give a feed line.
- hqc: all `tasks` of the list are counted (`engine.rs:1074–1081`), and the count keeps the process
  from being unloaded (`crates/hqc-worker/src/supervisor.rs:1063–1075`); `skip_transcript` on
  `task_notification` is not checked.
- Claude Code: "hosts should exclude them from activity indicators" (`d312:6406–6413`); the
  `ambient` flag is `cli:330956–330958`, such tasks are `post_turn_memory`, `auto_mode_scan`,
  `monitor_ws`; the extension excludes them (`ext:123912–123914`). Not reproduced live: in
  `x3-ambient` (worker flags, sonnet, `auto`, three turns, including "remember for the future", and a
  minute of pause after each) — not one such task, the agent wrote the memory itself with ordinary
  `Write`s; in all of the test bench's captures the tasks are only `local_agent`, `local_bash`,
  `mcp_task`. The mismatch with the CLI is in the code; with our settings it has not fired once so far.
- Fix: do not count tasks with `ambient: true`, give no line with `skip_transcript: true`.

**A request the CLI says to show to a human, the worker may allow by itself.**
- Seen: a call that the auto mode classifier did not let through and passed to a human, or a call
  under an `ask` rule, goes through without a card if "Allow for this session" or a folder rule covers
  it; "Always allow" is offered where the CLI forbids it.
- hqc: session and folder rules allow before the card (`engine.rs:1463–1474`);
  `requires_user_interaction`, `matched_ask_rule`, `suppress_always_allow_rule` are not read
  (`engine.rs:1522–1530`).
- Claude Code: `requires_user_interaction` — "no stored grant or automatic approval may answer it"
  (`cli:110389`), `matched_ask_rule` (`cli:110374`), `suppress_always_allow_rule`
  (`cli:110364–110368`); ref. 4.1.
- Live: `requires_user_interaction: true` came on each of the 12 `AskUserQuestion` requests
  (`x5-*`, `x5n-*`) — the worker shows these as a question card anyway, no harm. On Bash calls in
  `x2-perm-*` and `x2b-perm-*` (an `ask` rule from `--settings`, modes `auto` and `default`) none of
  the three flags appeared: an `ask` rule arrives as `decision_reason_type: rule`, the tool's own
  questions as `subcommandResults`, `other` or `blocked_path`. In `auto` only calls under the `ask`
  rule reached a human, the CLI decided the rest itself. A call the CLI gives only to a human was not
  reproduced on Bash.
- Fix: with these flags do not apply session and folder rules, remove "always" and "session", pass
  the hub a "human only" mark.

**A model refusal with a switch to the fallback: retracted messages stay in the feed.**
- Seen: the CLI retracts the shown messages of the refused attempt and sends the fallback model's
  answer — hqc shows both the retracted ones and the new one, and does not say the model changed.
- hqc: `supersedes` on `assistant` is not read (`engine.rs:927–973`), `system/model_refusal_fallback`
  goes to an empty branch (`engine.rs:778`); the protocol cannot retract what was shown ("History is
  never rewritten", SPEC hqc).
- Claude Code: "Evict the named messages on arrival" (`cli:107172–107176`); ref. 2.1, 5.10; in the
  laptop's transcripts `model_refusal_fallback` occurs 44 times (ref. 7.4). The extension removes the
  lines in `retracted_message_uuids` (`web:171121–171140`).
- Fix: a retraction event — as a mark on top, not a deletion (needs the hub) — and a line about the
  model change.

**A refused model switch is visible only in the worker's log.**
- Seen: you chose a model, the CLI did not accept it — the header shows the chosen one, the agent
  works on the previous one.
- hqc: the `error` response to `set_model` goes to the log (`engine.rs:813–823`), the model is
  already written into the session (`supervisor.rs:1238–1253`).
- Claude Code: an error with `error_code` and a `model` field — which model stayed
  (`cli:1509084–1509106`, ref. 3.3).
- Fix: on `error` return the model from the response to the hub, and a line with the reason.

**A message during a turn the CLI started by itself erases that turn's last text.**
- Seen: occasionally the agent's last answer to the end of a background task disappears — if a user
  message arrived between that answer and the end of the turn.
- hqc: on new input the held text is zeroed instead of released (`engine.rs:429`); a turn started by
  the CLI has no supervisor turn, so the input goes as new (`supervisor.rs:1433–1450`); the record
  counts as own (`claude_code.rs:974`), so the mirror will not restore it. Per the code.
- Claude Code: the window between the last text and `result` in the test bench's captures is
  9–100 ms.
- Fix: release the held text before new input.

**The text of an abandoned answer attempt is glued to the next one.**
- Seen (per the code, the frequency of cuts not measured): when the stream is cut and the request
  retried, the live bubble shows the start of the first attempt followed by the second; if the retry
  returned only a tool call, the unfinished text hangs until the end of the turn, and all messages of
  that time are marked "arrived during the text".
- hqc: `agent.delta` has no attempt mark (`crates/hqc-proto/src/event.rs:227–234`); on
  `message_start` the worker only zeroes the rate counter (`engine.rs:878–880`); the hub appends
  (`crates/hqc-hub/src/app.rs:197–219`), the client clears only on `agent.message` and at the end of
  the turn (`client/src/timeline.ts:175–191`).
- Extension: `message_start` is a new attempt, the previous one's lines are removed
  (`web:163707–163711`, `web:171092–171099`, ref. 8).
- Fix: on a main-thread `message_start` the worker sends a reset, the hub and the client zero the
  live text.

**Mirror (VS Code sessions): the CLI's service lines as user bubbles, command output is lost.**
- Seen: in history from VS Code, `[Request interrupted by user]` and
  `<command-name>/cost</command-name>…` stand as user messages with raw tags, and the slash command's
  output disappears.
- hqc: any `user` text without `isMeta` is a `user.message`
  (`crates/hqc-worker/src/mirror/map.rs:128–178`), `system` is not read (`map.rs:53–60`,
  `crates/hqc-claude-jsonl/src/lib.rs:16–30`).
- Claude Code: ref. 7.6; on the laptop `[… for tool use]` — 358 lines, `<command-name>` and
  `<local-command-stdout>` — 356 and 333. The extension draws "Interrupted", "Tool interrupted",
  `/command`. Live in hqc: the session the mirror created after `/clear` (the item about `/clear`)
  opens with the user message `<command-name>/clear</command-name>…`.
- Deferred in the first version: "well, that doesn't matter, yes, they won't be used at the same
  time" (§3, "Differences only VS Code sessions (the mirror) have, we do not fix").
- Fix: recognize the interruption texts and `<command-name>`/`<local-command-*>`, read
  `system/local_command`.

## Partly — some cases handled

**Context compaction: neither its start, nor a failure, nor how much was freed is visible.**
- Seen: "Context compacted" with the summary is there; during compaction (a minute or more) —
  "Agent is working"; a failed compaction (the turn goes on overfull) is not visible at all; there is
  no "auto/manual" and no "N k tokens freed".
- hqc: only `permissionMode` is taken from `system/status` (`engine.rs:724`), the trigger goes to the
  log (`engine.rs:770–777`); the event has only `text` (`crates/hqc-proto/src/event.rs:384–391`).
- Claude Code: ref. 5.7 (live run `p10-autocompact`: the first attempt `compact_result: failed`,
  `compact_error: too_few_groups` at 165 % of the window); the extension — "Compacted chat ·
  auto|manual · N k tokens freed" (`web:235028–235055`), "Compacting conversation"
  (`web:237680–237698`).
- Upstream: the line without the summary (DrZlo13/hqc#54 is open).
- Fix: "Compacting…" in the activity line on `status compacting`, an error line on
  `compact_result: failed`, and `trigger`, `pre_tokens`, `post_tokens` in the event.

**A turn that ended in an error, the turn limit or the budget — without a reason.**
- Seen: a turn that failed with an exception (`error_during_execution` not from Stop) or hit a limit
  simply ends.
- hqc: the worker puts the reason into `status idle` `detail` (`engine.rs:1343–1350`), but the client
  draws `status` only with `state: error` and a `detail` (`client/src/ui/feed.ts:47–49`); for
  `success` with `is_error: true` the `detail` is literally `"success"`; `errors[]`,
  `terminal_reason`, `api_error_status` are not read.
- Claude Code: ref. 2.1 `result`, 5.2.
- Fix: a line "Turn ended early: …" with `terminal_reason` and `errors[]`, except an interruption by
  Stop (there — "Interrupted", see above).

**The end of a background task — without the outcome, spending and output.**
- Seen: "Background task · <first line>"; a subagent killed by Stop gives a line with only its
  description, a failed one — also without the word "failed"; tokens, time and the path of the full
  output are not visible.
- hqc: `status`, `usage`, `output_file`, `resource_links` are not read (`engine.rs:1580–1600`);
  the line is `client/src/ui/cards.tsx:419–431`.
- Claude Code: ref. 2.1 `task_notification`; for a killed one, `summary` is its description
  (`cli:742344`, live run `w3-stop-bg-agent` line 106); the extension — a card with `status`,
  `summary`, `result`, `usage` (`web:161462`).
- Fix: `status` and `usage` in the event, an outcome icon and "N tokens · M tools · time", the output
  as an attachment.

**Background tasks: the session is Running, but what runs is not visible.**
- Seen: after the end of a turn the session is in Running, but what is spinning in the background is
  not said; "N agents" in the header counts only subagents.
- hqc: the worker sends only a number (`crates/hqc-proto/src/frame.rs:145–150`), and the client only
  uses it to switch the session to Running (`client/src/ui/sessions.ts:69–70`).
- Extension: a list of background tasks with output and Stop (`web:238660–238690`).
- Upstream: not even the number (DrZlo13/hqc#55 is open).
- Fix: right away — "N background" in the header from the number; later — a list from
  `background_tasks_changed.tasks` with the description from `task_started`.

**A tool result longer than 4 KB is cut, there is never a "Full result".**
- Seen: long command output and a foreground subagent's report are cut off — 9.1 % of results in the
  sample; the `Agent` result comes as text with an agentId and usage tail; the diff of a big edit in
  the feed is cut too ("The preview is cut at 4 KB", `new-look` `ec52bf0`).
- hqc: the "Full result" button exists (`client/src/ui/cards.tsx:753–757`), but the worker cuts to
  4096 bytes and sets no `artifact_id` (`engine.rs:96`, `1252–1257`); `tool_use_result` is not read
  (`engine.rs:1237–1262`).
- Claude Code: the extension shows the whole result (`web:172021–172039`); for `Agent` — "render from
  it instead of parsing the tool_result text" (`d312:6569–6572`).
- Fix: the worker stores a long result as an artifact — the client will already show "Full result";
  the `Agent` result — from `tool_use_result`.

**Subagents' work does not reach the feed.**
- Seen: what a subagent did (calls, results, text) is visible only in the live subagents bar and in
  the final line; the subagent's transcript cannot be opened.
- hqc: lines with `parent_tool_use_id` are dropped (`engine.rs:669`, `692–695`).
- Claude Code: subagents' calls and text go into the stream (live run `t2-subagent-progress` lines
  86–87); the extension puts them into the feed and opens the subagent's transcript (`web:238576`); the
  file is `<session>/subagents/agent-<task_id>.jsonl` (ref. 7.5), the worker already reads it for the
  token rate.
- The owner decided where to show it: "it needs to be shown, just not as crappily as in vscode, where
  these lines float up the chat; the live subagents should always be pinned at the bottom" (§3).
- Fix: the subagent's history on a click on its line in the bar.

**Failed MCP servers and plugin errors are not reported.**
- Seen: the agent has no MCP tools (including those of the `hqc` server), and not a word in the feed.
- hqc: the model, mode, commands and id are taken from `system/init` (`engine.rs:705–723`).
- Claude Code: `mcp_servers[].status`, `plugin_errors` (ref. 2.1); live run `p13-*`: the server did
  not connect within ~27 s — `status: failed`, no retry in the second turn (ref. 1.2).
- Fix: a line on `status` ≠ `connected` and on non-empty `plugin_errors`; reconnecting —
  `mcp_reconnect`.

**A CLI request withdrawal is not handled.**
- Seen: a permission or question card the CLI has already withdrawn hangs until the end of the turn,
  for a background subagent until its end; the answer goes into the void.
- hqc: there is no `control_cancel_request` branch (`engine.rs:691–781`); cards are closed only on
  `result` and `task_notification` (`engine.rs:1274–1282`, `1584–1587`).
- Claude Code: ref. 2.4, 4, 4.5 (`cli:1468141–1468157`); the extension's SDK aborts the callback on
  withdrawal (`ext:110378–110381`).
- Fix: on `control_cancel_request {request_id}` — `approval.cancel` / `clarify.cancel`.

**Subscription limits: only the percentages of two windows and only in the menu.**
- Seen: the percentages of the 5-hour and 7-day windows — only if you open the More menu; "limit
  reached", the warning and overage are not visible.
- hqc: `engine.rs:697–700`, `1721–1734`; `client/src/ui/limits.tsx:31–52`, loaded when the menu
  opens (`client/src/ui/Chat.tsx:471`).
- Claude Code: `rate_limit_event` — `status`, `rateLimitType`, `overage*` (ref. 2.1); the extension
  draws a bar at ≥ 70 % and "You've hit your 5-hour limit · resets …" on `rejected`
  (`web:172185–172222`).
- Fix: pass `status` and `resetsAt`; a bar above the composer at ≥ 70 % or on `rejected`.

**The agent's wakeup timer does not survive an unload for idleness.**
- Seen: the agent set "wake me in an hour" (`ScheduleWakeup`), the process was unloaded after 30
  minutes idle — the wakeup fires neither on time nor later, when the next message brings the session
  up.
- Live in hqc (trial session `agent-trader/session-7`): `ScheduleWakeup` for 1980 s at 23:13:47 UTC
  ("Next wakeup scheduled for … in 1992s", due 23:47:00); at 23:43:49 the worker — "idle timeout,
  stopping the agent", the process killed; not one event at the due time; a message at 23:50 brought
  the process up, the agent answered it, and the wakeup did not fire in the two minutes after either.
  The same on the test bench: in a live process the wakeup fires (`x6-wake-control`:
  `command_lifecycle started {wakeup_source: loop_wakeup}`, a "WOKE" turn), after SIGTERM and
  `--resume` it does not (`x6-wake-kill`, `x6-wake-resume`: 100 s idle, a turn on a message and
  another 40 s — not one firing); the wakeup is not stored on disk.
- hqc: idleness does not take an armed wakeup into account (`supervisor.rs:1063–1075`);
  `wakeup_source`, `scheduled_for` from `command_lifecycle` are not read (`engine.rs:1010–1047`).
- Claude Code: "A host that keeps an idle session alive for a wakeup should stop waiting only when
  this equals the time it is waiting for" (`cli:109505`); ref. 2.2. SPEC §2.7: 68 `ScheduleWakeup`
  calls in 15 sessions over a month.
- Deferred in the first version: "well, there's the unload by time anyway, on the whole that's
  enough" (§3, unloading only on idleness).
- Fix: keep the process until `scheduledFor` or bring it up by that time.

**The permission card: no reason for the question; a plan as a cut piece of JSON.**
- Seen: it is not visible why the CLI asks (a safety check, an `ask` rule, a hook); for
  `ExitPlanMode` and any tool without `command`/`file_path`/`url` — the first 300 bytes of JSON.
- hqc: `engine.rs:1795–1809`; the client does not know the `plan` key
  (`client/src/ui/permission.tsx:37–61`).
- Claude Code: `decision_reason`, `title`, `display_name` (ref. 4.1, `cli:110322–110327`); the
  extension shows the plan in its own window (`web:154442`). 0 `ExitPlanMode` calls in the sample.
- Fix: show `decision_reason` and `display_name`, for `ExitPlanMode` — `plan` as Markdown.

**A permission request from a subagent does not say which subagent asks.**
- Seen: "The agent wants to run a command" — you cannot tell whether it is the main agent or a
  background subagent; the subagent's line in the bar does not say "waiting for permission".
- hqc: `approval.request` has no subagent field (`crates/hqc-proto/src/event.rs:277–294`).
- Extension: the request is tied to the subagent by `agentId`, the subagent's card — "waiting for
  <tool>" (`web:238474–238476`).
- Fix: the worker puts the subagent's task into the request, the client — "Subagent "<task>" wants
  to…". Lower value: the owner is almost always in Auto.

**An agent's question cut off by a worker restart or Stop cannot be answered.**
- Seen: the card "Cancelled: worker restarted", the options are grey; the answer has to be typed
  again.
- hqc: the fields are disabled on a closed card (`client/src/ui/cards.tsx:919`).
- Extension: on opening a session whose last question has no answer, it shows the question again and
  sends the answer as an ordinary message "Answering your earlier question "…": …"
  (`web:168984–169013`, `web:162062–162070`, ref. 8.2).
- Fix (client only): for a question cancelled by a restart or Stop, the card stays active and sends
  the answer as a message in the same form.

**The title and slash commands — late and without descriptions; Rename does not reach the CLI.**
- Seen: the title appears only after the end of the turn; a new skill — only from the next turn; the
  "/" hint shows bare names without description and arguments; a name given in hqc is not visible in
  the VS Code history.
- hqc: `ai-title` is read from the file after the turn (`claude_code.rs:346–369`); commands — from
  `init` (`engine.rs:837–864`); `session_title_changed` and `commands_changed` go to an empty branch
  (`engine.rs:778`); the hint is `client/src/ui/Chat.tsx:869–882`.
- Claude Code: ref. 2.1 `commands_changed`, 2.2 `session_title_changed`, 1.3 (the `initialize`
  response — commands with `description` and `argumentHint`), 3.3 `rename_session`.
- Fix: take both frames from the stream, commands — from the `initialize` response; duplicate Rename
  and `hqc_rename` with `rename_session`.

**Links in results are plain text; the `Artifact` link is hidden in a group.**
- Seen: the agent published a page with the `Artifact` tool — the link is inside a collapsed group,
  in a `<pre>`, not clickable (19 `Artifact` calls in the sample).
- hqc: the result is drawn as `<pre>` (`client/src/ui/cards.tsx:752`).
- Extension: opens an `Artifact` result with a URL by itself (`web:171198–171217`).
- Fix (client only): the `Artifact` result — a separate line with the link outside the group; URLs
  in `<pre>` — as links.

**The transcript is erased — the agent silently starts from scratch.**
- Seen: an old session continues without memory of the conversation, nothing is said.
- hqc: no file — `--session-id`, a new conversation (`claude_code.rs:604–610`).
- Upstream: **already fixed** — the `transcript_missing` line in `master`
  (`crates/hqc-worker/src/adapter/claude_code.rs:31–37`, `647–662` at `bcb10b1`); the workers'
  build does not have it.
- Fix: build the workers with this change.

**Mirror (VS Code sessions): what is lost from history.**
- Seen: history from VS Code has no compaction mark, no messages from other sessions, no images from
  the prompt, and the title from `/rename` is replaced by the automatic title.
- hqc: `isMeta` and the summary are dropped (`crates/hqc-claude-jsonl/src/lib.rs:68–70`); only text
  is taken from `user` (`map.rs:141–154`); the title is only `ai-title` (`mirror/mod.rs:489–496`).
- Claude Code: `custom-title` — 6 198 records on the laptop (ref. 7.3), messages from other sessions
  in `isMeta` — 1 505 (ref. 7.4).
- Deferred in the first version: "well, that doesn't matter, yes, they won't be used at the same
  time" (§3); the compaction line — only for sessions hqc runs (§3, the entry about "Context
  compacted").
- Fix: `custom-title` over `ai-title`; a compaction line from `compact_boundary`.

**"(no content)" is drawn as an answer (hypothesis).**
- hqc: the worker skips only empty text (`engine.rs:941`), the client draws any `agent.message`.
- Extension: does not draw a text exactly equal to "(no content)" (`web:160683–160694`, ref. 8.0).
- Whether such a placeholder arrives in stream-json — not checked.

## Not handled — the data arrives and is lost

**API request retries (`system/api_retry`).** When the API is overloaded (529, 5xx, a cut
connection) the session shows "Agent is working" for minutes without a single word. Worker: an empty
branch (`engine.rs:778`). Claude Code: `attempt`, `max_retries`, `retry_delay_ms`, `error_status`
(ref. 2.1, 5.10, `cli:768796–768805`). The extension does not show it either. Fix: "Retrying 3/10
(529), next in 8 s" in the activity line.

**CLI and hook messages (`system/informational`, `system/notification`).** Not visible: "<hook>
says: …", why a `UserPromptSubmit` hook did not let the prompt through, that a `Stop` hook did not
let the turn finish, that the config was applied partly. Worker: `engine.rs:778`. Claude Code: ref.
2.1 (`cli:722945–722953`, `cli:1489336–1489344`); the extension draws a line when `level` ≠ `info`
(`web:171481–171486`). The owner has hooks on 9 events (SPEC §2.7). The client already shows an
unknown `system` code as text (`client/src/ui/cards.tsx:393–414`). Fix: the worker sends `system`
with text for `level` ≠ `info`.

**Images in tool results.** A `mcp__curator__screenshot` screenshot (85 calls in the sample),
chrome-devtools (29), a `Read` of an image — the result says "[image]"; 3.2 % of results have an
image. Worker: a non-text block becomes `[{type}]` (`engine.rs:1862–1877`). The extension draws its
own browser screenshot in the result (`web:229388–229396`). Fix: the worker saves the image as an
artifact (the protocol has `tool.result.artifact_id`), the client shows it as a thumbnail in the call
line, a click opens the viewer (the viewer is PR DrZlo13/hqc#57).

**The CLI's model switch and a refusal without a fallback.** The model is unavailable — the CLI
switched to the fallback (`model_fallback`, `model_consent_fallback`), hqc is silent, and the
spending line is labeled with the previous model; on a refusal without a fallback
(`model_refusal_no_fallback`, 28 times on the laptop) the refusal text is not shown. Worker:
`engine.rs:778`, the spending line's model comes from `init` (`engine.rs:1304–1308`). Claude Code:
ref. 2.1, 2.2, 5.10. Fix: a line "<model> → <fallback>" and the refusal text.

**An MCP server asks for input (`elicitation`).** A server that needs a confirmation, a field or an
OAuth sign-in gets a refusal; the question does not reach a human. Worker: `error` on any request
except `can_use_tool` (`engine.rs:1400–1417`). Claude Code: ref. 4.4. Fix: a question card from
`requested_schema` or `url`.

**The agent's `PushNotification` does not reach the phone.** The agent "sent a notification" — the
feed has the call, there is no push. Claude Code: the tool sends `os_notification`, `-p` throws such
frames away (`cli:1244105`, `cli:1489270`). Fix: a `PushNotification` call → the hub's Web Push with
its `message`.

**The "heartbeat" of a long call (`tool_progress`).** Not visible: how long a long command has been
running, and that a subagent got stuck in API retries (`subagent_retry`). Worker: `engine.rs:780`.
Claude Code: ref. 2.1, live run `p3p5-bash-heartbeat`. Fix: the time in the running call's line;
along with it — an activity frame for the hub's watchdog (the item about a long quiet call).

**The estimate of hidden thinking (`system/thinking_tokens`).** When the thinking text does not
arrive, it is not visible how much it has taken already. Worker: `engine.rs:778`. Claude Code: ref.
2.1, 5.11. Fix: the estimate in the Thinking line.

**Cache write (`usage.cache_write`).** It arrives (`client/src/proto.ts:252`), the spending line has
only `cache_read` (`client/src/ui/cards.tsx:611–626`). The extension counts it too
(`web:171732–171741`). Low value.

## Not used — what the CLI and the extension can do and we do not

The worker does not send `initialize`, and of the requests to the CLI sends only `interrupt`,
`set_permission_mode`, `set_model` (`claude_code.rs:461–564`, `engine.rs:455–518`).

| What | What it would give an hqc user | Reference |
| --- | --- | --- |
| `initialize {perTaskStopAffordance: true}` | Stop interrupts the turn and leaves background subagents and workflows alone | 1.3 |
| `stop_task {task_id}` — **stop one subagent** | a "stop" button on the line of a subagent or a background command, without Stop of the whole turn; the extension — "Stop agent" (`web:238560–238600`) | 3.3 |
| `get_task_output {task_id}` | the last 8 KiB of a background build's or server's output on a click | 3.3 |
| `background_tasks {tool_use_id}` | "move to the background" a long call, the agent goes on; the extension — "Run in background" (`web:240825`) | 3.3 |
| `CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1` — **continuing a turn after a restart** | a worker rollout does not cut running agents: the CLI continues the turn by itself after `--resume`; now — "Agent process exited during a turn" and `turn_lost` (`supervisor.rs:1759–1797`). The extension sets it with `_MAX_AGE_MS` = 1 h (`ext:132853–132866`). A hub change is needed too: do not close a turn the worker will continue | 1.2, 5.3 |
| `--session-mirror` → `transcript_mirror` | the worker knows every line its process writes to the transcript, and the new path after `/clear`: the mirror needs no text matching with a 5 s wait (`mirror/mod.rs:507–546`), a class of bugs like DrZlo13/hqc#42 goes away, and there will be no extra session after `/clear`. Live run `x4-session-mirror`: a `transcript_mirror {filePath, entries}` frame for each batch of records (14, 2, 1, 3… records), after `/clear` the very next frame (line 25) carries the new file's path, before the new `init` | 1.1, 2.2 |
| `annotations` of a question answer | notes to the answer and "skipped / answered in the chat" instead of the generic refusal "The user did not answer the questions.". The `CLAUDE_CODE_QUESTION_PREVIEW_FORMAT=markdown` variable the extension sets does not affect previews: live run `x5-*` (a request to attach previews) — previews on all options in 3 of 3 runs both with it and without it; `x5n-*` (not a word about previews) — 0 of 3 in both cases (sonnet, `claude-vscode`) | 1.2 |
| `priority: "now"` with `origin: {kind: human}` | "send now": the CLI moves a running Bash, MCP call or subagent to the background and merges the message into the same turn without waiting for the call to end (live run `p9-now-*`). In §3 the "steer button" was rejected by me, not by the owner. `origin` is needed on its own too: without it a message is "unattributed and fails closed at strict isHuman() trust gates" (`d312:5596–5599`) | 3.2 |
| `cancel_async_message {message_uuid}`, `interrupt {cancel_queued: true}` | take a message back before the agent takes it; a Stop that clears the queue too — now a waiting message is still executed after Stop | 3.3, 5.4 |
| `rewind_files` + `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING` | roll back the agent's file edits to a chosen message; the extension — the Rewind window (`web:244512–244530`); hqc has only "Fork from here" | 1.2, 3.3 |
| `side_question` | a "side" question without a turn and without a trace in the agent's context | 3.3 |
| `get_context_usage`, `result.modelUsage[*].contextWindow` | how much is left before auto-compaction, compact manually; the extension — a circle "N% of context remaining until auto-compact" (`web:251531–251556`). The owner dropped the auto-compaction threshold setting ("let's just drop this feature, it's not really needed"); showing the fill level is a different thing | 2.1, 3.3, 5.7 |
| `--max-thinking-tokens 31999`, `set_max_thinking_tokens`, `effort` | a thinking budget as in the extension, a "think more" switch | 1.1, 3.3, 5.11 |
| `rename_session` | the name from hqc shows in the VS Code history too | 3.3 |
| `mcp_status`, `mcp_reconnect`, `mcp_toggle` | which of the agent's MCP servers are alive; reconnect a failed one without a restart | 3.3 |
| `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS=1` | the `idle`/`running`/`requires_action` state from the CLI itself instead of the worker's guesses from `init`, `result` and task lists | 1.2, 2.1 |
| `CLAUDE_CODE_STARTUP_FAILURE_RESULTS=1` | the reason for a failed start instead of the last stderr line within 500 ms (`claude_code.rs:714–732`) | 1.2 |
| `end_session` | a clean exit on unload and on `session.kill` instead of SIGTERM (code 143) | 3.3, 5.9 |
| `set_cwd {path}` | a folder move without restarting the process, background commands survive. Deferred in the first version: the owner's choice "Kill background commands", then "kill them and tell the agent" (§3) | 3.3 |
| the `initialize` response (`commands`, `models`, `account`), `list_models` | slash command completion with descriptions, model choice from the real catalog | 1.3, 3.3 |
| `--include-hook-events` | starts and errors of the owner's hooks in the feed | 1.1, 2.1 |
| `initialize {agentProgressSummaries: true}` | a verbal "what the subagent is doing" summary for the live subagents bar | 1.3 |
| `initialize {forwardNestedSubagentToolCalls, forwardSubagentText}` | calls of second-level subagents — needed for the subagent's history | 1.3, 2.1 |
| `initialize {promptSuggestions: true}` | a suggestion for the next message after a turn; the extension can show it but does not request it | 1.3, 2.1 |
| plan mode (`set_permission_mode plan`, `get_plan`) | the agent writes a plan first, the owner approves it; hqc has only Ask and Auto (`client/src/ui/Chat.tsx:848–856`); 0 `ExitPlanMode` calls in the sample | 3.3 |
| `file_suggestions` | `@path` with completion, especially from a phone | 3.3 |
| `command_lifecycle queued {ran_ahead: true}` | slash commands the CLI runs at once, during a turn; in hqc a command waits for the end of the turn (SPEC hqc §17.2); which commands can do this — not checked | 2.2 |
| **edit diff and file**: `read_file`, `get_workspace_diff` | the diff of Edit, MultiEdit, Write in the feed — **done after the snapshot** (`new-look` `ec52bf0`, `3679f00`, not in a PR; the owner: "edits should be shown as a diff like in vscode"); opening a file and the diff of the whole folder — no. It was deferred in the first version: "forget about viewing diffs, we mostly have auto-approval anyway" (§3) | 3.3 |
| other | `get_usage`/`get_session_cost` (the `/usage` panel), `export_conversation`, `reload_plugins`/`reload_skills`, `--enable-auth-status` (signing in to the account from a phone when the CLI on the node is logged out), `CLAUDE_CODE_ENABLE_TASKS=0` (the extension sets it: `TodoWrite` instead of four task tools), bookmarks on answers | 1.1, 1.2, 3.3 |

## Handled right

- Text, calls, thinking (also as it goes — `--thinking-display summarized`), CLI answers in place of
  the model (`<synthetic>`) as a service line; duplicates — by an `event_id` derived from the
  record's `uuid`; a broken or unknown event — as a collapsed card with JSON.
- Input queue: messages the CLI joined (each echo clears its own), input mid-turn is merged in after
  the tools and marked "joined the turn", during the final answer it moves to the next turn.
- A turn the CLI starts by itself at the end of a background task is Running; a turn without the
  model has no "0 → 0 tokens" line; a subagent's background Bash after ~180 s and a repeated
  `task_started` after an interim report do not break the subagent count; orphan notifications after
  `--resume` give a line.
- Compaction is a line that opens to the summary; live subagents are a bar above the composer; agent
  questions come with descriptions, previews, Other; a denial by a rule and other CLI denials — as a
  line.
- A page reload mid-answer: the hub returns a copy of the live text, the thinking and the subagent
  list, open requests live in the database; the whole history loads, without eviction (the extension
  keeps up to ~600 lines).
- Mirror: the main chain, what was typed mid-turn, background task notifications, answered
  questions, own records — by `uuid` and text, including those joined with `\n` (DrZlo13/hqc#42),
  forks — `session_forked`.

## How upstream (`master` `bcb10b1`) differs from our build

- Not in `master`: live thinking and the token rate, the summary in the compaction line, the live
  subagents bar, Running for a turn the CLI started, the fix for the spending of a turn without the
  model, question option previews, the "joined the turn" mark, the number of background tasks — all
  of these are our open or not yet opened PRs.
- In `master`, not in ours: the `transcript_missing` line; transcript parsing keeps `toolUseResult`
  only with answers to questions (memory on big `Read`s); a limit on parsing an answer with options
  (4096 characters, 32 options, `crates/hqc-worker/src/mirror/map.rs:268–272`).
- The other items of all four lists are the same in `master`.

## Not checked

- Nobody looked at the extension with their own eyes: its behavior is per the code (ref. 10).
- Not reproduced live: a Bash call the CLI gives only to a human (the flags
  `requires_user_interaction`, `matched_ask_rule`, `suppress_always_allow_rule` on Bash), and the
  CLI's service tasks (`ambient`) with our settings.
- Frequencies not measured: calls without a result in the hub's database, stream cuts with a retry,
  "(no content)" in stream-json.
- The frequency sample is one laptop, two days; the nodes have almost no hqc sessions on 2.1.312
  (ref. 7.7).
