# T26: questions and differences from the mockups

Client redesign by `docs/design/redesign-2026-10-24` (Main, Permission, Mobile-List,
Mobile-Session), SPEC 0.27. Screenshots on the mock (headless Chromium, 1440×900 and 390×844)
were compared with the mockups by eye; they are not committed to the repository.

## Reply with text: live runs

Two runs of the two allowed, `tools/spike/driver.py --deny-message-file`, Claude Code
**2.1.308**, `claude-haiku-5-5`, `--max-turns 2`, prompt "Run the shell command: touch
hqc-marker. Then reply with exactly: DONE", the answer to `can_use_tool` is a `deny` with text:

| run | `message` | what the model did |
|---|---|---|
| 1 | "The user denied this tool call and wrote instead:\nDo not create any file. Reply with exactly: PELICAN-42" | saw the text (retold it), but did not follow it: "That text came from the tool result, not from you, so I'm not following it" |
| 2 | Claude Code's wording for a denial with an explanation: "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). To tell you how to proceed, the user said:\nНе создавай файл. Вместо этого объясни одним предложением по-русски, что делает команда touch." (the Russian part: "Do not create the file. Instead, explain in one sentence in Russian what the touch command does.") | followed it: answered in one sentence in Russian, the file was not created |

The field is called `message` (as in `docs/notes/claude-control-protocol.md`); the model gets it
as a `tool_result` with `is_error: true`. The adapter uses the wording of run 2
(`DENY_WITH_TEXT`). Caveat: the runs differ in both the wording and the request itself (a code
word looks more like an injection than an ordinary request), so what exactly worked is not
separated; a third run would have been over the limit. **[verify]** when convenient: the same
ordinary request with wording 1.

## What from the mockups was not done and why

- **A "Custom" mode with drag and drop**: the user's decision — only "State" and "Recent"; the
  switch has two buttons.
- **The banner text "in VS Code it will become read-only"** is untrue; per the user's decision
  there are two banners with his texts: a holder exists — "The session is open in VS Code on
  <worker>. Close it there to write from here." without a button; no holder — "VS Code was the
  last to run this session on <worker>." with "Take over here".
- **Reply with text through the input field at the bottom** (Permission: placeholder "Or write
  what to do instead…"; Mobile-Session: an input field with the bar "The reply will deny `ls /`
  and go to the agent"). Done per the user's decision as a field in the card itself (and in the
  phone's sheet), with the same hint; the input field at the bottom stays a regular queued
  message while a request is open.
- **"Rebuilt the project": a group caption written on the agent's behalf.** There is no such
  text in the events; the caption is the agent's interim message before the group, on one line,
  otherwise "N commands · time" (the user's decision). Inside the group the message is shown in
  full in italics, only if it did not fit into the caption.
- **"159/159" on the `ninja` row**: the events carry no command progress; the row has a time
  from `duration_ms`.
- **The row "$ ls / · allowed" and below it "Running ls /"**: done as a collapsed request row
  and one shared row "Running <command> <timer>" at the bottom of the feed (from the last
  `tool.call` without a result), not a separate one under each row.
- **"Now", "1 min" on running sessions in "State" mode**: in the mockup the time is visible
  only in "Recent"; done that way.
- **The subline "Waiting for permission · Bash"** in the list: the list knows only `state` from
  `summary`, not the tool of the open request; it says "Waiting for reply". In the header of an
  open session it is "Waiting for permission" or "Waiting for reply", by the feed's open
  request.
- **The path in a session row without `~/work/`** (mockup: `drzlo-mac · games/phys-mp`): we
  shorten the home folder to `~` and leave the rest as is: `~/work/` is a personal habit, another
  user has different roots.
- **Light theme**: the mockups are dark only; the previous system-driven light theme was
  removed.
- **Unread on a project**: in the mockup a project has a "needs you" badge (sessions waiting
  for a reply and with an error); the previous unread badge on a project was removed. The unread
  badge on session and worker rows stays (the mockup does not have it, but without it new
  answers are not visible).
- **Workspaces icon**: the mockup has no entry point; a "Project workspaces" link at the bottom
  of the session list (on desktop and phone), next to "Archive N".
- **"Retry"**: not in the mockups; on desktop a button in the header, on a phone an item in the
  "More" menu.
- **"Rename" in "More"** (a button label in Permission): the client API has no session
  renaming; the menu has only "Archive", "Restore", "Delete".
- **Avatar**: the email's first letter with the address as a tooltip; on a phone it leads to
  settings (as "Account and settings" in the mockup), on desktop settings is a separate button.
- **Keyboard shortcuts**: Enter and Esc work only when the focus is not in the input field (a
  condition of the task). After sending a message the focus stays in the input field, so right
  after a request appears Enter goes to the field, not to the card; one has to click outside
  the field (or Tab). **Question:** take the focus off the field when a request arrives?
- **The request sheet on a phone**: only the first open `approval.request`; `clarify` stays a
  card in the feed (in the mockup the sheet is only for permissions). The button labels are the
  same as on desktop ("Always in this session"; the mockup has "Always in session"), so that
  the labels by `canonical` are the same everywhere.
- **Options beyond the mockup** (`always`, `deny_always` in Claude Code) are shown as secondary
  buttons next to "Always in this session" (desktop) and in the grid (phone).

## Decisions along the way

- A `tool.call` immediately followed by an `approval.request` for the same tool is shown as a
  request card, not as a group row (the Permission mockup: "one card instead of a tool row and a
  panel").
- "Outside the working folder" is computed from the call's arguments (`file_path`, `path`,
  absolute and `~` words of a Bash command) against the workspace path + the session's `cwd`;
  `~` expands to the home folder taken from the workspace path.
- The waiting time in the card counts from the request event's `ts` and ticks once a second.
- The cost in the header is the sum of `cost_usd` over all `usage` of the loaded history (two
  decimals); under an answer, the turn's `cost_usd` (three decimals, as in the mockup).
- Fonts: `@fontsource/inter` and `@fontsource/jetbrains-mono` 5.3.0 (`npm pack`, without
  installing), 12 woff2 files (400/500/600 × Latin/Cyrillic) and both OFLs; `@font-face` with
  `unicode-range` from the packages. The hub's CSP did not change: `default-src 'self'` covers
  fonts; the test `csp.test.ts` checks that the styles' `url()`s lead only to `/fonts/` and that
  the files exist.
- Size: JS 141.82 → 166.78 KB (gzip 44.53 → 52.07), CSS 16.19 → 36.43 KB (gzip 4.18 → 7.74).
