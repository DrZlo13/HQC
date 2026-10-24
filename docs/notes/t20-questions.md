# T20: questions and findings

Takeover without forks (SPEC §9.3, version 0.22). Claude Code **2.1.308**, home-mac,
`claude-haiku-5-5`, `--max-turns 2`. Live runs: two of the two allowed, both through
`tools/spike/driver.py`:

| run | what | result |
|---|---|---|
| a | `tmp/spike-t20-a/ws`: `claude -p "Reply with exactly: ONE"` (`--arg-prompt`), then `--resume` with the adapter's flags and turn `HQC-TURN`, then `--arg-prompt --resume <uuid> -- --resume-session-at <uuid of answer ONE>` and turn `VSCODE-TURN` | a fork in one file without VS Code; fixture `fork-2.1.308` |
| b | `HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_holder`: `driver.py` in the background, `claude -p` waits in the `slow` tool from `tools/spike/slow-mcp.js` (40 s) | the registry saw the holder (`sdk-cli`, `busy`), the session was imported `locked_vscode` with `held_by`, `POST /takeover` → `409 vscode_open`; after the process exited the session was **not** released — see below; fixed, not rerun live |

`HQC_LIVE_CLAUDE=1` and `HQC_TEST_DATABASE_URL` were passed through `cargo --config 'env.…'`.

## The registry of live sessions (2.1.308, home-mac)

Records `~/.claude/sessions/<pid>.json` on this machine (all `sdk-cli`, VS Code is not open
here): `pid` (number), `sessionId`, `cwd`, `startedAt`, `procStart` (a string with the process
start time), `version`, `peerProtocol`, `peerFeatures[]`, `kind: interactive`, `entrypoint`,
`pidDomain`, `messagingSocketPath`, `name`, `nameSource`, `nameSince`, `status`
(`idle`/`busy`), `updatedAt` and `statusUpdatedAt` (ms). Next to them, `<pid>.<hash>.key` — not
read. In the user's VS Code example `pid` is a string (`"69786"`), `entrypoint:
claude-vscode`; the parser accepts both forms.

- `claude -p` in stream-json mode (like the SDK and our adapter) registers (`sdk-cli`, `busy`
  during a turn) — run b. Whether `claude -p "<prompt>"` in text mode registers was not
  checked (run b used stream-json).
- hqc adapter processes are `sdk-cli` too; the worker excludes them by pid and process group
  (the adapter starts the CLI as a group leader), and also any `sdk-cli` record of a
  transcript that one of its own processes is currently running (in case the CLI is started
  by a wrapper with a different pid). The cost: `claude -p --resume` from a terminal during an
  hqc turn is not counted as a holder — the old foreign-write detection catches it (§9.3).
  **T25 (SPEC 0.26):** a worker with the registry no longer has `uuid`-based detection — it
  twice released a session falsely (`/compact` and the first input after `--resume`,
  production log 2026-10-24). Such a `claude -p --resume` while hqc is working no longer
  releases the session; its records are emitted into the feed; see
  `docs/notes/t25-questions.md`.

## The fork (run a)

`--resume-session-at <uuid>` (accepted by 2.1.308; whether it is in `--help` we did not check)
continues the session from the given record in **the same** file: the prompt `VSCODE-TURN` got
`parentUuid` = the uuid of answer `ONE`, which already had a child — `attachment
prompt_snapshot` before the prompt `HQC-TURN`. This reproduces a fork without VS Code; a
synthetic fixture was not needed.

Format findings:
- on 2.1.308 an `attachment` `prompt_snapshot` precedes the prompt of a `--resume` turn, so the
  children of a fork point are not necessarily prompts themselves;
- **a record has two children even without a fork**: parallel tool calls produce `tool_use`
  records as a second child (the next `tool_use` and `tool_result`; fixtures `auto-mode`,
  `auto-classifier`). So the rule "the record got a second child" from the task statement was
  refined: a fork is a prompt above which the record has another child with prompts in its
  subtree.

## What went wrong in run b and how it was fixed

After `claude -p` exited, the session stayed `locked_vscode`. Two causes, both reproduced by
fixture tests and fixed:

1. The "no holder" report went as a direct frame, while `mirror.event` went through the
   outbox; the direct frame overtook the previous pass's events, and their `held_by` (the
   holder still alive) put the lock back. Now holder reports go through the outbox in the same
   order as `mirror.event`.
2. The process's last records came with `recent` and, by the T15 rule, locked the session
   again, already without a holder. Now, with registry data (`held_by` in the frame), `recent`
   does not lock: only a holder locks. The hub test `holder_blocks_takeover_until_closed`
   checks exactly this sequence.

The run limit did not allow repeating the live test (`live_holder`). **A rerun is needed**:
`HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_holder -- --nocapture` (one `claude -p`,
about 50 s).

## Decisions worth confirming

- **The holder report is a `session.state` with a nil `session_id`.** The worker does not know
  the `session_id` of sessions created by the mirror, and the task only allowed adding fields
  to `mirror.event` and `session.state`. A new frame (`mirror.holder`) would be cleaner; left
  as is, the hub recognizes the report by the nil `session_id` and reads only `held_by` from
  it.
- **`held_by` has three states**: the field is absent — no registry data, `null` — nobody, an
  object — the holder. By the presence of the field the hub decides whether `recent` and the
  30 s rule apply; for `POST /takeover` — by the worker's
  `capabilities.claude_code.sessions_registry`.
- **`held_by` in `session.summary`** was added too (it was not in the task statement): without
  it the client does not learn that a holder appeared or left until it re-requests the list.
- **The holder is gone, but the session's hqc process is still alive** (a turn after the
  release): `locked_by` stays `vscode`; when the process stops, the session stays "In VS Code"
  with the "Take over" button, and the takeover goes through immediately (there is no
  holder).
- **On `hello` the hub forgets the holders**, leaving `locked_by` alone; the worker reports the
  current ones after `hello.ack`. In between (before the first pass, ≤ 2 s) a takeover of an
  open session may go through.
- **`reason` of `session_released`**: `vscode_open` (a holder) and `foreign_write` (a foreign
  write); the task statement had only `vscode_open`.
- **A takeover with a fresh write and no holder goes through immediately** (for a worker with
  the registry): such a write was made by a process that has already exited.
- **The "open" banner** says "VS Code" for `entrypoint: claude-vscode` and "Claude Code outside
  hqc" for the rest (terminal, SDK).

## Open questions

1. **pid reuse.** A crashed process's file stays; if another process takes its pid, `kill -0`
   says "alive", and the session stays "open" until the machine reboots or a new process
   comes with that file. The record's `procStart` could be checked against the process start
   time (`proc_pidinfo` on macOS, `/proc/<pid>/stat` on Linux). Not done.
2. **Live VS Code** was not checked: it is not open on home-mac. That the plugin holds the
   record while the session tab is open and removes it when the tab closes (not only when the
   window closes) — **[verify]** on the user's Mac per RUNBOOK §4.
3. **Forks outside the mirror.** An hqc session in a workspace without `mirror_enabled` will
   not show a fork: only the mirror builds the tree. The holder works regardless (the registry
   is read for all transcripts). Related to T15 question 2 (mirror on all workspaces by
   default).
   Resolved in T21: the mirror is on for all workspaces by default (SPEC 0.23 §9.2).
4. **A fork from `/rewind` or editing a message in the CLI** is also a second line of turns;
   the mirror will call it "VS Code continued from an earlier point", although it is a
   deliberate rollback. We cannot tell them apart.
5. **The text for `branch_from: hqc`** ("hqc continued from an earlier point…") has not
   occurred live, it was checked only by a unit test of the text. Which branch `--resume`
   takes in a file with a fork (the latest by time or by line order) was not checked.
