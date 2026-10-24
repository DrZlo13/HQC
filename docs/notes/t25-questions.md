# T25: questions and findings

False release of a session while hqc is working (SPEC §9.2, §9.3, version 0.26). Claude Code
**2.1.308**, home-mac, `claude-haiku-5-5`, `--max-turns 2`. Live runs: three of the three
allowed, all through `tools/spike/driver.py` in `tmp/spike-t25-a/ws` with the adapter's flags
(`--permission-prompt-tool stdio --strict-mcp-config --allowedTools mcp__hqc --setting-sources
user,local --permission-mode default`):

| run | what | result |
|---|---|---|
| 1 | new session, `Reply with exactly: ONE` | — |
| 2 | `--resume`, `Reply with exactly: TWO` | the `--replay-user-messages` echo carries the `uuid` of the prompt record in jsonl (`2b86849f…` in both) |
| 3 | `--resume`, `/compact` | in jsonl: `compact_boundary`, the summary (`isCompactSummary`, present in stream-json as `isSynthetic`), an explanation (`isMeta`), the command `<command-name>/compact</command-name>…` (**not in stream-json**), the output `<local-command-stdout>Compacted </local-command-stdout>` (present, echoed) |

Fixture `resume-compact-2.1.308` (the transcript after the three runs, the streams of runs 2
and 3).

## Item 2 (`/compact`) — reproduced

Of the `/compact` records in the main chain, the only one the mirror does not know is the
`<command-name>` command: stream-json does not name its `uuid`. That is exactly the "another
writer continues a session the worker runs" from the log. Fixed in two ways: a worker with the
registry has no `uuid`-based detection at all (a T25 decision); and without the registry the
command record is now recognized as its own — the adapter keeps the input text (`/compact`),
and the mirror reduces `<command-name>`/`<command-args>` to the typed text.

## Item 1 (the first input after `--resume`) — not reproduced

On 2.1.308 the echo's `uuid` after `--resume` matches the record (run 2, test
`t25_resume_prompt_and_compact`; it was the same in the T15 and T20 fixtures). Why it was
different on drzlo-mac cannot be found out from here. Candidates, from most to least likely:

- **a different CLI version** on drzlo-mac (the log does not show it) — **[verify]**:
  `claude --version` there and the `version` field of record `e4b09ae1…` in the transcript;
- the input arrived while the CLI was busy (a turn or loading a large transcript), and the CLI
  merged it with the queue (`queue-operation`) into a record with a new `uuid`, while it emitted
  the echo when queueing; in the runs the queue was drained immediately (`enqueue`/`dequeue` in
  the same millisecond);
- the echo arrived more than 5 s after the record (reading the adapter's stdout stalls while
  the previous line's actions run).

Whatever it is, a prompt is now also recognized by the input text (`OwnRecords::claim`): a
prompt record with an unknown `uuid` is our own if its text matches a not yet matched input of
this transcript. So the fallback path (a worker without the registry) does not err either. If
the cause turns out to be several inputs merged into one record, the text will not match any
input — for a worker with the registry this is no longer a release, but without the registry a
false positive remains; a production transcript is needed.

## Decisions worth confirming

- **An unknown record with a live process and the registry is emitted** after the same 5 s
  (rather than held until the process stops): otherwise the mirror would be stuck on it for the
  whole lifetime of the process — no `ai-title`, forks or following records. The hub does not
  lock the session on a `mirror.event` with `held_by` (T20), it only updates
  `vscode_active_at` on `recent`. If a prompt is still recognized neither by `uuid` nor by text,
  the feed gets a second `user.message` with the same text.
- **The `/compact` command record (if not recognized) is shown as a `user.message`** with the
  raw `<command-name>…`. A `system` line or hiding it would be nicer; not done — after T25 it
  is recognized by text and not emitted.
- **Matching by text** is exact equality after `trim`; inputs not matched to any record (e.g.
  a workspace without the mirror) accumulate up to 32 per transcript, evicting the oldest. A
  false "own" is possible only if exactly the same text as an unmatched hqc input was typed in
  VS Code.
- **The cost of the decision** (SPEC §9.3): `claude -p --resume` from a terminal during an hqc
  turn (the registry excludes its `sdk-cli` record) and a process shorter than a pass do not
  release the session.

## Open questions

1. The cause of item 1 on drzlo-mac — see above; `claude --version` and a piece of the
   transcript around `e4b09ae1…` (the `queue-operation` records nearby) are needed.
2. Other slash commands (`/clear`, `/model`, custom commands) were not checked live; `/clear`
   probably starts a new transcript — the mirror and holders were not checked with it.

## CLI version on drzlo-mac (2026-10-24)

On the user's Mac, where item 1 reproduced, Claude Code is 2.1.307 (record e4b09ae1 is 2.1.307 too), on home-mac 2.1.308. The version fits the main hypothesis: on 2.1.307 the --replay-user-messages echo after --resume apparently carries a different uuid. Matching by text from T25 covers this even without a CLI update.
