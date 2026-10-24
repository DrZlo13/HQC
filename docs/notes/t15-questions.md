# T15: questions and findings

Import, mirror and takeover of Claude Code sessions (SPEC §9, version 0.17; after the merge in T18 — 0.19). Claude Code **2.1.308**,
home-mac, `claude-haiku-5-5`, `--max-turns 2`. Live runs: three of the four allowed:

| run | what | result |
|---|---|---|
| a | `claude -p "<prompt>"` in `tmp/spike-t15-a/ws`, then `driver.py --resume` with the adapter's flags and one more turn | fixture `vscode-takeover-2.1.308` |
| b | a `driver.py` session with the adapter's flags in `tmp/spike-t15-b/ws`, then `claude -p --resume <uuid> "<prompt>"` | fixture `adapter-then-outside-2.1.308` |
| c | `HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_takeover` (three `claude` calls) | PASS, 44 s |

Calling `claude` directly in this session is forbidden by the tool allowlist, so runs a and b
went through `tools/spike/driver.py` (`--arg-prompt` and `--resume` were added), run c from
the test; `HQC_LIVE_CLAUDE=1` was passed through `cargo --config 'env.HQC_LIVE_CLAUDE="1"'`.

## What was confirmed about the 2.1.308 jsonl format

- `claude -p "<prompt>"` and `--resume` (including in the adapter's stream-json mode) write
  **the same** file `<config>/projects/<encoded cwd>/<uuid>.jsonl`, with the same `sessionId`
  and `cwd`; both write `entrypoint: sdk-cli`. No run went through the real VS Code plugin —
  which `entrypoint` it has (presumably `claude-vscode`) was not checked.
- The uuids of `user`/`assistant` records of the main chain match between stream-json and
  jsonl; the `--replay-user-messages` echo carries the uuid of the prompt record — as on
  2.1.307. With `--resume`, stream-json does not replay the old history: the adapter sees only
  its own process's records.
- One content block is one record with its own `uuid`; the blocks of one model response are
  linked by `message.id` and the `apiBlockIndex` field. So the rule "block `i > 0` is v5 of
  `"<uuid>/<i>"`" still has never fired, but it is implemented everywhere
  (`Event::id_from_claude_block`).
- `message.stop_reason` is present in jsonl on every response record (`tool_use`, `end_turn`;
  the `thinking` and `text` of the last response are both `end_turn`), in stream-json it is
  `null`. The mirror takes `final` of `agent.message` from it.
- `thinking` in jsonl is empty text plus a `signature`. In the adapter's stream-json there was
  no such record in either run (in jsonl, an `attachment` `thinking_drop` stood in its place in
  the second turn of run a). The mirror does not emit an empty `thinking` and does not count it
  when detecting a foreign writer — otherwise the adapter's own record, absent from
  stream-json, would look foreign.
- `attachment`s have a `uuid` and sit in the `parentUuid` chain (prompt → attachments →
  response). The mirror determines the main chain by record type and `isSidechain`/`isMeta`,
  not by `parentUuid`.
- New types without a `uuid`: `api-request-shape`, `atis-latch`, `mode`; `queue-operation`
  (`enqueue`/`dequeue`; `enqueue` has `content` with the prompt text) before every prompt.
- **`ai-title` was not written in any run**, including with the prompt as an argument (on
  2.1.307 it was in `tool-call`). So the imported sessions in the live test got "Session N".
  Remains **[verify]** on a real VS Code session.

## What diverged from §9 (as it was) and how it was resolved

1. `workspace.list_candidates {}` — without `request_id`, unlike `fs.*`. Made `{request_id}`
   as in `fs.*` (the answer is a `result` with the same `id`).
2. "On enrollment the worker sends `workspace.list_candidates`" — done on request instead:
   candidates are needed at import time, not at enrollment; the hub does not store them.
3. `mirror.event {workspace_path, runtime_session_id, cwd, event}` — the hub was missing two
   things: the title (`ai-title` lives in the file and is not a §4 event) and a "being written
   now" flag for the initial `locked_by` and the 30 s of silence. `title?` and `recent` were
   added; the worker computes `recent` from the file's modification time on its own clock —
   the hub's and the worker's clocks may differ. The hub counts the silence for takeover from
   the moment it **received** the last `recent` event (`vscode_active_at`).
4. "An unknown file uuid becomes a new session with `locked_by: vscode`" (§9.2) versus
   "`none` if the file has not been written for 30 s" (§9.1): the §9.1 rule was chosen for
   both — importing an old session does not make it "held by VS Code", a live write
   (`recent`) does.
5. "While the hqc process is not running, any such record moves the session to `vscode`" —
   refined: only a record with `recent`. Old records (import, catching up after worker
   downtime) do not touch the lock; otherwise enabling the mirror on a quiet folder would lock
   all its sessions.
6. In `mirror.event` the worker does not know the hub's `session_id` and the author id: it
   sets nil, the hub fills them in.
7. Neither §4 nor §6 could be edited in this task (parallel T16/T17), so the following are
   described in §9: the `system` code `session_released` (for the §4 table), `409
   vscode_active`, the `hint` field of `locked_vscode`, `GET /api/workers/{id}/candidates`
   ("later" in §6), `mirror_enabled` in `POST /api/workspaces`, `POST /takeover` for
   `locked_by: none`. **Move to §4/§6 when merging.**
   Resolved in T18: moved to §4, §5, §6 (SPEC 0.19, §23).

## Decisions worth confirming

- **Polling, not notify** (§9.2): 2 s, no new dependency. On a machine with thousands of
  transcripts a pass is `read_dir` + `stat` of every file; the first pass also reads the first
  lines of every file for `cwd` (cached afterwards). Not measured on a large folder.
- **A 5 s hold** while a process is live: a write to the file may come ahead of the stdout
  line. Five seconds is generous; during a long `tool_use` stdout arrives at the same moment
  as the write.
- **Release** clears the worker's input queue, and the hub moves `pending` inputs to `failed`
  (no separate `system`, visible through `delivery`). The alternative is to keep them until
  takeover; but that is a silent queue, which §9.3 forbids.
  Closed in T20 (SPEC 0.22, §9.3; `docs/notes/t20-questions.md`): release and takeover are
  decided by the transcript's holder from the registry of live Claude Code sessions — a
  session open in VS Code is released immediately (`session_released` with
  `reason: vscode_open`), takeover while the holder is alive is `409 vscode_open`; the 30 s of
  silence remain a fallback rule without the registry.
- **`locked_by` from `session.state`**: the worker's `vscode` wins; the hub's `vscode` is not
  cleared by `none` (the process stopped) or by `hqc` from a process the hub did not start via
  takeover. A process of a session the hub already considers `vscode` (race: the mirror sent a
  `recent` event before the worker let go) stays `vscode` for the hub, and the worker lets it
  go on its own by detection.
- **Push** is not sent for mirror events at all (nor for live VS Code answers): the person is
  at that machine right now. If notifying about the end of a turn in VS Code is wanted, that
  is a separate rule.
- Migration number `0006` may clash with T16/T17 — rename on merge.
  Resolved in T18: `0008_mirror.sql` (`0006` — T17, `0007` — T16), SPEC 0.19.

## Open questions

1. **hqc sessions in a folder where the mirror was enabled later.** If the mirror is enabled
   on a workspace where hqc sessions already ran (in T15 this is only possible by creating the
   workspace, `PATCH` is not done), the first pass imports their transcripts: the agent's
   answers are deduplicated by `event_id`, but the prompts are not (the hub's `user.message`
   has its own v7, the prompt record has a v5 of the uuid). The same after a worker crash
   between the adapter's write to the file and the mirror's pass: the registry of own uuids is
   in memory. Options: the worker reports to the hub the mapping "input `event_id` → record
   uuid" from the echo; or a registry on disk. Not done yet.
   Resolved in T21 (SPEC 0.23, §9.1) for enabling the mirror: `mirror.set` carries the hub's
   sessions in the workspace with the time of their last event; the first pass over such a
   transcript does not send records not newer than that. The case of a worker crash between
   the adapter's write and the mirror's pass remains (`docs/notes/t21-questions.md`).
2. **A session started in hqc and continued in VS Code** is released only if its workspace is
   mirrored. For a plain workspace (no import) the worker does not see a foreign writer at
   all. Enable the mirror on all workspaces by default?
   Resolved in T21: yes, on by default for all (migration `0011` — for existing ones), SPEC
   0.23 §9.2.
3. `PATCH /api/workspaces/{id}` with `mirror_enabled` ("mirror — later" in §6) is not done.
   Resolved in T21: `PATCH {label?, mirror_enabled?}` and the "VS Code sessions" switch.
4. A transcript whose `cwd` changed within the session is bound by the `cwd` of its first
   record.
5. Title: `ai-title` is not written in `-p` on 2.1.308; take the title from the first prompt
   (as VS Code does in its session list)?
   Resolved in T21: a title from the model (`HQC_TITLE_MODEL`) or from the first line of the
   first prompt, SPEC 0.23 §3 Session.
6. "Session N" for imports is numbered per project, like a regular one; when importing dozens
   of sessions the names are uninformative — see item 5.
   Resolved in T21: imported sessions get a title the same way, from their first records.
7. Takeover was checked from a phone only through the client on the mock hub and via REST in
   the live test; no live run through the UI on a phone was done.
