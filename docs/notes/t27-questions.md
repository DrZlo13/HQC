# T27: questions and decisions

## 1. Cost

Two live runs on Claude Code 2.1.308 (`tools/spike/driver.py`, haiku, `--max-turns 2`, in
`tmp/spike-t27-cost`; details in `docs/notes/claude-control-protocol.md`, "Cost and slash
commands"): `result.total_cost_usd` is the session's running total, in one process and across
`--resume`, where it continues from the transcript's last `cost-state`. So "$44.401" on every
line of a resumed session was the session's total at each turn, and the header summed them.

Decisions:
- `usage.cost_usd` = the growth of `total_cost_usd` since the previous `result` of the process;
  the first turn after `--resume` is counted from `totalCostUSD` of the transcript's last
  `cost-state`, read when the process starts. A new session (or a transcript without
  `cost-state`) starts from 0.
- Never negative: a total below the baseline gives 0 and does not lower the baseline (the next
  turn is counted from the higher of the two). On the runs above this never happens; it guards
  against a CLI that stops carrying the cost over.
- The difference is rounded to 1e-10 USD (0.0043361 - 0.00398629 is not exact in binary).
- `usage.session_cost_usd` = `total_cost_usd` as is. The header shows the latest one, else the
  sum of `cost_usd` (ACP agents, events before T27).
- Events stored before T27 keep their wrong `cost_usd` (history is not rewritten, SPEC §4).
  Without `session_cost_usd` the header of such a session still sums them until its next turn;
  after one new turn it shows the right total.
- Not checked: a process killed with SIGKILL may leave no `cost-state`. The assumption is that
  the CLI then restarts from the previous one too, so the baseline still matches; if it does
  not, that turn's cost is too high once, never negative.

## 2. Slash commands

- Field: `session.state.commands?: [string]`, as the task suggested. It sits next to `title`,
  which already is "something the runtime reports about the session that the worker repeats in
  every `session.state`"; a separate frame would need its own ack and outbox handling for
  nothing.
- Hub: `sessions.commands jsonb` (migration 0013), shown as `commands?` in the session object,
  not in `session.summary` (66 names for every session in every summary would be heavy). The
  client fetches the session object when it opens an idle chat that has no list yet, which
  covers the first turn of a new session; a list that changes later (a new skill) is seen after
  the next full refresh of the session list.
- Which names: `slash_commands` of `system/init` minus `terminal_slash_commands` (they only
  work in the terminal UI, `-p` cannot run them) and minus names starting with `_`
  (`__remote-workflow`, internal). `skills` is a subset of `slash_commands`; `agents` are
  subagent types, not commands, so neither is added.
- `system/init` comes before every turn, not once per process: the adapter reports the list
  the first time and then only when it changes.
- The hub cleans the list (trim, leading `/`, no whitespace or control characters, at most
  100 characters, unique, at most 500). The worker is trusted for the content (it comes from
  the owner's own CLI), not for the size.
- Popup: shown while the text is `/` and a name with nothing after it; prefix match ignoring
  case, in the CLI's order; hidden when the only match is typed in full; Enter and Tab insert
  `/<name> ` (Enter does not send while the popup is open; Shift+Enter is still a newline), Esc
  closes it until the text changes. The `aria-label` of the list is English ("Commands"): new
  text is written in English in this task, the UI translation is a separate task.
- Nothing checks a sent `/name` against the list: the list only drives completion.

## 3. Layout

I could not reproduce horizontal scrolling. Measured in Chrome (system Chrome through
playwright-core, `tmp/t27-layout/measure.mjs`, against `npm run dev:mock`) at 1440, 1200, 1024,
960, 900, 899, 700 and 390 px, with a code block with a 350-character line, a 14-column table,
a long path, long tool lines, captions and a permission card: the page never scrolled
sideways, no box except `pre` and `.md-table` scrolled horizontally, and below 900 px the
phone layout took over. `.main` has `min-width: 0` and the chat column is `minmax(0, 1fr)`, so
nothing in T26 pins the chat column.

What was wrong and is fixed:
- The session column stayed 300px all the way down to the breakpoint (a grid track
  `minmax(250px, 300px)` grows to its maximum before `1fr` gets space), so only the chat
  shrank. Now `clamp(220px, 25vw, 300px)`: 300px from 1200px up, 225px at 900px.
- Markdown tables did not scroll: cells inherited `overflow-wrap: anywhere` of the message and
  broke letter by letter. Now `.md-table th, td { overflow-wrap: normal }` and the table
  scrolls inside `.md-table`.
- `.composer` got `min-width: 0` and `position: relative` (the popup).

Not checked: Safari / iOS (no WebKit on this machine). If the user's report was on Safari, the
first suspect is `<summary>` with `display: flex` (`.tool-row summary`), which older WebKit
ignored; the tool line would then not be clipped. Please tell which browser showed it.
`tests/layout.test.ts` checks the rules (happy-dom has no layout engine, so it cannot render at
several widths).
