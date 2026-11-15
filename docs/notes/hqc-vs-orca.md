# hqc and Orca: what it has that we do not

Orca (https://orca.dev, https://github.com/stablyai/orca, MIT — `README.md:323`) by Stably AI is
an Electron desktop app for macOS, Windows and Linux that can also serve a web client and pairs
with iOS and Android apps. Every task runs in its own workspace (a worktree or a folder) with its
own agents, tabs, terminal and browser. An agent runs either as a program in a terminal or as a
"Chat UI"; there Claude works through `@anthropic-ai/claude-agent-sdk` 0.3.319
(`package.json:235`). This note lists every feature of Orca a user sees and, for each one,
whether hqc has it.

The two products are built differently. In Orca every machine runs its own instance — the window
or `orca serve` without one — and clients connect to each machine separately: by an access link
over Tailscale or the local network, or through Orca Relay, Orca's cloud intermediary. In hqc there
is one hub, and the workers connect to it themselves. So Orca's features for reaching individual
machines (SSH targets, pairing the phone with each host) are not needed in hqc. The hqc SPEC says
"it is not an IDE: no file editor, diffs only as artifacts" (`docs/SPEC.md:39`) — Orca's git panel,
terminal, browser and editor fall under that.

## What was compared

- Orca — commit `2cd3f168` (2026-11-15 07:45:48 −0800). Orca paths are from the root of its
  repository.
- What was read of Orca: the user docs `docs/site/content/docs/` (of 60 files, 37 opened, 36 of
  them in full; 23 not opened — listed under "Not checked"), `README.md`, release notes v1.4.259
  (the first 340 of 636 lines), mobile-android-v0.0.64 and v0.0.65, the home page https://orca.dev.
  Code — selectively: the chat components `src/renderer/src/components/native-chat/`, the settings
  search entries (295), the 100 key bindings in `src/shared/keybindings/definitions-core-{1,2,3,4}.ts`,
  the strings and routes of the phone app `mobile/`, speech recognition `src/main/speech/`, the web
  client `src/renderer/src/web/`, the cloud parts `cloud/`. Feature behaviour is described from the
  docs and UI strings; Orca was not run.
- hqc — `origin/master` `5e11381` (2026-11-15 05:35 UTC). hqc paths are from the root of the
  repository. Model providers arrived in master after it (`1bc972a7`, SPEC 0.144: an
  Anthropic-compatible endpoint per session, `[providers.<name>]` in `worker.toml`,
  `hqc_spawn(provider)`) — they are not in the tables and bear on the rows "34 agents" and "Several
  accounts".
- Each "no" names the patterns searched for in `docs/SPEC.md`, `client/src` and `crates/*/src`;
  "only in hqc" was also checked by searching Orca. "Yes" includes cases where hqc does the same
  differently — how is said in the cell.
- Marks: [multi] — makes sense only with several agent vendors; [desktop] — only for a desktop app;
  [mobile] — only for a native phone app.

## In short

| In hqc | Orca features |
| --- | ---: |
| no | 68 |
| partly | 51 |
| yes | 32 |
| not needed with a hub | 4 |
| not checked | 2 |
| total | 157 |

Of the 68 "no", 9 are marked [desktop], 5 [mobile], [multi] — 0.

What hqc lacks entirely and what matters most with many Claude Code sessions on several machines,
watched from a browser and a phone:

1. **Voice input** — on the desktop and on the phone (below, "How Orca does voice input").
2. **Continuing after a limit reset.** Orca itself writes "Continue where you left off" to the agent
   when the limit resets, offers "Continue Now" and continuing under another account. In hqc a
   session that hit its limit stands still, though hqc knows the reset time (`worker.limits`,
   SPEC:1634-1636).
3. **An event feed across all sessions**: who finished, who waits, a preview of the last answer; on
   the phone, the Feed tab. In hqc "Requests" lists only open requests, and a session row has no
   preview.
4. **Context fill** (an "N% used" ring) and a **prompt cache timer**.
5. **The agent's task list** (TodoWrite), pinned above the feed.
6. **Side chat `/btw`** — ask a working session without disturbing its turn.
7. **A turn summary**: changed files with line counts, PRs the agent opened, CI state lines.
8. **A "New" marker** above what arrived while you were away and a **"Jump to latest"** button.
9. **↑ in an empty box** brings back the previous prompt; a long paste goes as an attachment.
10. **Quick Commands** — saved prompts and commands with one press, shared with the phone.

Of what is done partly, these most deserve finishing: the input queue (edit and delete waiting
messages, choose "join the turn or wait"); a plan-mode switch; Stop for background tasks and a
subagent's own feed (separate PRs are in preparation); a new session on every run of a schedule
(described in SPEC:4665-4666, absent from `crates/hqc-hub/src/schedules.rs`); notification
categories instead of a single switch.

Only in hqc, Orca does not have it:

- One hub keeps the history, search and input queue of all machines, even when a machine is off
  (SPEC:43-57, 4799-4800). Orca's search skips unreachable hosts
  (`sessionSearch.panel.hostSkipReason.unreachable` in `src/renderer/src/i18n/locales/en.json`).
- A live mirror and takeover of VS Code sessions and `claude` in a terminal (SPEC §9.2–9.3). In Orca
  `mirror … vs code session|take over … session` — 0 files; there is only a history of past sessions
  with Resume.
- Moving a session to another project or folder: `hqc_move` and drag and drop. In Orca `Move to
  project|move session to` — 0.
- Folder rules and a permission ceiling along a chain of sessions (SPEC:115-134, §10). In Orca
  `folder rules|deny rules|path rules` — 7 files, all about Windows paths and permissions; Orca goes
  the other way — a coordinator can approve its workers' requests itself
  (`docs/site/content/docs/cli/orchestration.mdx:89-99`).
- The token-rate bar on the project bar (SPEC:1764-1772). In Orca `tokens per
  second|tok/s|output_per_s` — only an OpenCode screen capture among bug-report material.
- Web search for text selected in the chat (SPEC:1963-1965). In Orca such an item exists only in
  the menu of its built-in browser.
- Web Push to a plain browser without a native app. Whether Orca's web client receives push — not
  checked.

## How Orca does voice input

- **Desktop.** The shortcut `Mod+E` (`src/shared/keybindings/definitions-core-1.ts:164-169`), Toggle
  or Hold mode (`docs/site/content/docs/settings.mdx:115-129`). The text lands in whatever has
  focus: the agent's box, a terminal or an editor. The machine itself transcribes: `transcribe-cpp`
  0.4.1 (`package.json:400-406`) in a separate process (`src/main/speech/stt-worker-process.ts`),
  with the Silero detector cutting silence; its model downloads in the background
  (`src/main/speech/speech-vad-model.ts:4-13`).
- **Models.** The catalogue `src/main/speech/model-catalog.ts:19-241` has 17 models: 13 local and 4
  cloud (GPT Transcribe, OpenAI-compatible, GPT-4o mini Transcribe, GPT-4o Transcribe; an OpenAI key
  is needed). By the language lists (`src/shared/speech/models/dictation-language.ts`), Russian is
  supported by 4 local models (Parakeet TDT v3 — the recommended one, Whisper Tiny, Whisper Large v3
  Turbo, Nemotron 3.5) and by all 4 cloud ones. Orca's own measurement for Russian, word error rate:
  Parakeet TDT v3 3.4 %, Whisper Large v3 Turbo 3.4 %, Nemotron 3.5 14.6 %, Whisper Tiny 38.2 %
  (`src/main/speech/speech-model-error-rates.ts:8-62`); that is 5 FLEURS recordings on an M4 Max,
  and the authors call the measurement rough in a comment.
- **Settings** (`src/renderer/src/components/settings/voice/voice-pane-search.ts:16-269`): a
  vocabulary of terms, personal and per project; filler-word removal; "AI cleanup" — cleaning the
  transcript with one's own agent, off by default and asking for consent; stop on silence, keep the
  model in memory, GPU, microphone choice, language.
- **Phone.** The app records audio (`@orca/expo-two-way-audio`, `mobile/package.json:28`) and sends
  it in chunks to its computer (`speech.dictation.start`, `speech.dictation.chunk`); the computer
  transcribes (`mobile/src/dictation/mobile-dictation-operations.ts:52-66`). The model can be
  downloaded to the computer from the phone. Without the window (`orca serve` on a server) there is
  no dictation (`docs/site/content/docs/remote-servers.mdx:208`).
- **hqc.** The patterns `voice|dictat|microphone|getUserMedia|speech` do not occur in SPEC or
  `client/src`; the only mention is the worker turning off Claude Code's own voice
  (`crates/hqc-worker/src/title.rs:50`).

## Input box

| Feature | What the user gets | Where in Orca | hqc |
| --- | --- | --- | --- |
| Voice input | dictation by a shortcut (Mod+E) into the agent's box, a terminal or an editor; a model on one's own machine or the cloud | `docs/site/content/docs/settings.mdx:115-129`; `src/main/speech/model-catalog.ts:19-241`; `src/shared/keybindings/definitions-core-1.ts:164-169` | **no**: `voice\|dictat\|microphone\|getUserMedia\|speech` — 0 in SPEC and client; the only hit is the worker turning off the CLI's own microphone (`crates/hqc-worker/src/title.rs:50`) |
| Queue or Steer during a turn | a "Queue follow-ups" setting; Cmd/Ctrl+Enter does the opposite for one message, and with an empty box pushes the first waiting card into the turn | `src/renderer/src/components/settings/chat/chat-behavior-search.ts:57-69`; `src/renderer/src/components/native-chat/composer/controls/NativeChatComposerActions.tsx:127-139` | **partly**: an input joins the turn or waits by itself (SPEC §17.2, 4748-4785), a "joined the turn" mark (`client/src/ui/cards.tsx:595`); no mode choice; "interrupt and send" is only in SPEC:4779 |
| Waiting messages as cards | edit, delete, drag to reorder, "Send this message now", "Clear all" | `src/renderer/src/components/native-chat/queue/NativeChatQueuedMessageCard.tsx:108, 304`; `src/renderer/src/components/native-chat/queue/NativeChatQueuedMessageRows.tsx:77`; `src/renderer/src/components/native-chat/queue/NativeChatQueuedMessageList.tsx:233`; `src/renderer/src/components/native-chat/queue/editing/NativeChatQueuedMessageComposer.tsx:71` | **partly**: "· awaiting delivery" on a waiting one (`client/src/ui/cards.tsx:598`), "N queued" on the row (`client/src/ui/sessions.ts:118`); Retry and Discard only on a message the client failed to send (`client/src/ui/cards.tsx:654-663`); a message waiting on the hub cannot be edited or deleted |
| Offline queue on the phone [mobile] | a message without a connection waits: "Sends when the desktop is back" | `mobile/src/i18n/locales/en.json:3064-3070` | **partly**: input for an offline worker waits on the hub (SPEC:4799-4800); the client keeps nothing while offline |
| A draft survives a reconnect | text and attachments stay through a drop and a reconnect | `docs/site/content/docs/agents/native-chat.mdx:47`; `src/renderer/src/components/native-chat/drafts/native-chat-composer-draft-indexeddb.ts` | **yes**: the draft in the browser, "Draft" on the row (SPEC:1720-1722); attachments in the draft — not checked |
| Attachments | paste, drop onto the chat pane, images as thumbnails | `src/renderer/src/components/native-chat/attachments/intake/NativeChatPaneFileDropSurface.tsx:189-193` | **yes**: paste and drag and drop (`client/src/ui/Chat.tsx:1197`), HEIC→JPEG, up to 32 files (SPEC:1606) |
| A long paste as an attachment | large text becomes a "Pasted text" chip; a message limit of 256 KB | `src/renderer/src/components/native-chat/media/text/NativeChatTextAttachmentPreview.tsx:49`; `src/renderer/src/components/native-chat/paste/use-native-chat-pasted-text-file.ts:33` | **no**: only in the backlog (`docs/BACKLOG.md:35-46`) |
| A file with `@` | a list of the workspace's files on `@` | `src/renderer/src/components/native-chat/composer/pickers/NativeChatAutocompleteMenus.tsx:382-385`; `docs/site/content/docs/mobile.mdx:67` | **no**: `mention\|@file\|file picker\|autocomplete`, relevant hits — 0 (only `autocomplete="off"` in forms) |
| Slash commands and skills | a `/` menu with descriptions, Commands and Skills apart, a skill's source Project / Personal / Built-in / Plugin | `src/renderer/src/components/native-chat/composer/pickers/NativeChatAutocompleteMenus.tsx:149-150, 350-353`; `src/renderer/src/components/native-chat/composer/pickers/native-chat-slash-command-descriptions.ts:11-66` | **partly**: `/` completes the list of CLI commands, names only (SPEC:2066-2071; `client/src/proto.ts:1098-1100`) |
| Previous prompt with an arrow | ↑ in an empty box brings back one's past messages from the feed | `src/renderer/src/components/native-chat/drafts/native-chat-sent-prompt-history.ts:1`; `src/renderer/src/components/native-chat/drafts/use-native-chat-composer-recall.ts` | **no**: `ArrowUp` only in lists (`client/src/ui/SessionList.tsx:333`, `client/src/ui/ProjectRail.tsx:178`) |
| Selection into the input box | "Add to chat" on a selection in the feed | `src/renderer/src/components/native-chat/selection-quotes/AddToChatMenu.tsx:46` | **yes**: "Quote" inserts the selection as `> ` lines (SPEC:1963-1965) |
| Side chat `/side`, `/btw` | a question in a copy beside the chat that does not reach the agent | `src/renderer/src/components/native-chat/composer/pickers/use-native-chat-side-chat-command.ts:26, 48-50` | **no**: `side chat\|/btw\|side question` — 0 |
| A `/goal` | a goal banner: "Pursuing goal", pause, resume, clear | `src/renderer/src/components/native-chat/composer/controls/NativeChatComposerGoalChip.tsx:11-12`; `src/renderer/src/components/native-chat/session/goals/NativeChatThreadGoalBanner.tsx:86-103` | **no**: `/goal\|goal mode\|thread goal` — 0. In Orca `/goal` is described among Codex commands (`src/renderer/src/components/native-chat/composer/pickers/native-chat-codex-slash-command-descriptions.ts:131` in Orca); whether it works with Claude — not checked |
| Plan mode | a "Plan" chip and "Exit plan mode", a card with the plan file | `src/renderer/src/components/native-chat/composer/controls/NativeChatComposerPlanChip.tsx:50-51`; `src/renderer/src/components/native-chat/prompts/NativeChatApprovalCard.tsx:179` | **partly**: no switch; `ExitPlanMode` arrives as a permission request (SPEC:3205); the worker knows the `plan` mode (`crates/hqc-worker/src/capabilities.rs:179-187`) |
| Model, effort, Fast | a model picker with search and descriptions (the list comes from the installed CLI), reasoning effort, Fast mode | `src/renderer/src/components/native-chat/session/options/controls/NativeChatModelCombobox.tsx:49`; `src/renderer/src/components/native-chat/session/options/controls/native-chat-session-option-labels-text.ts:11-27`; `docs/site/content/docs/agents/native-chat.mdx:49` | **partly**: the model can be changed (SPEC:1896-1897); `effort\|thinking budget\|reasoning_effort\|ultrathink` — 0, `fast mode\|fast_mode` — 0 |
| A git branch in the input box | switch the workspace's branch while the agent is not working | `src/renderer/src/components/native-chat/composer/controls/NativeChatComposerBranchPicker.tsx:53-69` | **no**: a branch exists only on a worktree or clone record (SPEC:245-261); no switching from the chat |
| Stop a turn | a "Stop the agent" button | `src/renderer/src/components/native-chat/composer/controls/NativeChatComposerActions.tsx:164-165` | **yes**: "Stop the agent" (`client/src/ui/Chat.tsx:537`) |

## Feed

| Feature | What the user gets | Where in Orca | hqc |
| --- | --- | --- | --- |
| Find in the open chat | Mod+F, a find bar over the feed | `src/shared/keybindings/definitions-core-4.ts:35`; `src/renderer/src/components/native-chat/viewport/find/NativeChatFindBar.tsx` | **partly**: no find bar in the chat (`find in chat\|cmd\+f\|Mod\+F` — 0); the search page filters by session (`session=`, SPEC:1663-1664) |
| A "Your messages" rail | a column of one's own messages beside the feed, jump to any | `src/renderer/src/components/native-chat/viewport/rail/NativeChatMessageRail.tsx:115` | **no**: `minimap\|outline\|jump to` — 0 |
| A new-messages marker | on returning to a chat, a "New" line above what arrived while you were away | `src/renderer/src/components/native-chat/viewport/new-rows/NativeChatNewRowsMarker.tsx:77`; `src/renderer/src/components/settings/chat/chat-behavior-search.ts:83` | **no**: `new messages\|unread divider\|while you were away` — 0; there is an unread count on the row (`client/src/ui/SessionList.tsx:852`) |
| "Jump to latest" | a down button when the feed is scrolled up | `src/renderer/src/components/native-chat/viewport/scroll/NativeChatJumpToLatest.tsx:19` | **no**: `to-bottom\|toBottom\|scrollToBottom\|jump-latest` — 0; the feed holds the bottom by itself (SPEC:1970-1971) |
| Older history in parts | earlier messages load on scrolling up | `src/renderer/src/components/native-chat/viewport/history/NativeChatOlderHistoryRow.tsx:17` | **yes**: older blocks by 60 (SPEC:1987-1989) |
| Time and copying a message | a time on a message, "Copy message", copy and line wrap on a code block | `src/renderer/src/components/native-chat/messages/rows/NativeChatMessageTimestamp.tsx`; `src/renderer/src/components/native-chat/messages/actions/NativeChatCopyButton.tsx:47`; `src/renderer/src/components/native-chat/markdown/code/NativeChatCodeBlock.tsx:37, 91` | **yes**: the time in the first line, copying (SPEC:1914-1919; `client/src/ui/cards.tsx:631`), copying a code block (`client/src/ui/Markdown.tsx:344-363`); a line-wrap toggle — not checked |
| Reasoning | a "Thought for N s" line that expands | `src/renderer/src/components/native-chat/messages/reasoning/NativeChatReasoningRow.tsx:22-28` | **yes**: a Thinking line (`client/src/ui/feed.ts:189`; SPEC:1922-1923); no live reasoning while it is written (a separate PR is in preparation) |
| Groups of calls | "Read 3 files · Ran 2 commands", expandable | `src/renderer/src/components/native-chat/tools/runs/native-chat-tool-run-label.ts:17-143` | **yes**: "N tool calls · time" and the list of tools (SPEC:1919-1922) |
| Diff of an edit | a diff card, "Copy diff" | `src/renderer/src/components/native-chat/tools/diffs/NativeChatDiffCard.tsx:188-202` | **yes**: the card and the expanded Edit, MultiEdit, Write row as a diff (SPEC:3871-3876) |
| A turn summary: changed files | under the answer, the turn's files with line counts | `src/renderer/src/components/settings/chat/chat-turn-activity-search.ts:17`; `src/renderer/src/components/native-chat/tools/diffs/NativeChatTurnDiffRollup.tsx:86-110` | **no**: no combined diff of a turn (against "diffs only as artifacts", SPEC:39) |
| PRs the agent opened and CI lines | an "Opened N pull requests" card, a "CI failing on / CI green on" line | `src/renderer/src/components/settings/chat/chat-turn-activity-search.ts:29-47`; `src/renderer/src/components/native-chat/tools/pull-requests/NativeChatOpenedPullRequestCard.tsx:232`; `src/renderer/src/components/native-chat/messages/notices/NativeChatPullRequestEventLine.tsx:68-69` | **no**: `pull request\|git push\|commit message` — only deny rules (SPEC:4394) |
| The agent's task list (TodoWrite) | "3 of 7 tasks completed", a pinned list | `src/renderer/src/components/native-chat/tools/tasks/NativeChatTaskList.tsx:86`; `src/renderer/src/components/native-chat/tools/tasks/NativeChatDockedTaskList.tsx` | **no**: `TodoWrite\|todo\|task list` in client — 0 |
| Background tasks with "Stop" | a line of background commands and agents, a stop button on each and "Stop background tasks" | `src/renderer/src/components/native-chat/tools/tasks/NativeChatBackgroundTasksStatus.tsx:136`; `src/shared/background-task-copy.ts:6-14` | **partly**: background commands and subagents as a list above the input (SPEC:2019-2030), "Run in background" (SPEC:4728-4730); no stopping (a separate PR is in preparation) |
| A subagent's own feed | the task, steps and answer of a subagent in a separate view, "Back to main agent"; each with its model and current tool | `src/renderer/src/components/native-chat/subagents/NativeChatSubagentStatusBar.tsx:40`; `src/renderer/src/components/native-chat/subagents/NativeChatSubagentPrompt.tsx:18`; GitHub v1.4.259, PR #41766, #42066 | **partly**: start and end cards with state (SPEC:1923-1961); no subagent feed (a separate PR is in preparation) |
| Inline visuals | charts, diagrams, mock-ups inside an answer; "Open in new tab" | `src/renderer/src/components/settings/chat/chat-inline-visuals-search.ts:7`; `src/renderer/src/components/native-chat/media/visuals/NativeChatInlineVisual.tsx:31, 117` | **partly**: HTML sent with `hqc_attach` in a sandboxed iframe (SPEC:4212; `client/src/ui/attachments.tsx:346`) |
| Mermaid | diagrams in Markdown, the phone included | `docs/site/content/docs/editing/viewers.mdx:17-19`; `docs/site/content/docs/mobile.mdx:21` | **no**: `mermaid\|diagram` — 0 |
| Link previews | a card of a web page or a GitHub issue or PR on hover | `src/renderer/src/components/settings/chat/chat-behavior-search.ts:95`; `src/renderer/src/components/native-chat/links/previews/NativeChatGitHubLinkPeek.tsx:57-74` | **no**: `link preview\|hover card\|og:\|unfurl`, relevant hits — 0 |
| File links open the file | a path in an answer opens the file in an editor tab | `src/renderer/src/components/native-chat/links/actions/native-chat-file-link-toasts.ts:7-41` | **no**: not an IDE (SPEC:39) |
| Messages from other agents | "From <agent>", "Go to agent" | `src/renderer/src/components/native-chat/messages/agent-senders/NativeChatAgentMessageSenders.tsx:43, 112` | **yes**: purple "↙ Message from X" blocks with a link to the session (SPEC:1991-2007) |
| A copy from an answer | "Fork from this response", `/fork` | `src/renderer/src/components/native-chat/messages/actions/NativeChatForkAction.tsx:14, 46`; `src/renderer/src/components/native-chat/composer/pickers/use-native-chat-resume-command.ts:57` | **yes**: "Fork from here" (`client/src/ui/cards.tsx:582-583`; SPEC 0.80) |
| Rewind to here | the conversation in the same session rewinds to a message, which returns to the box; files on disk are not touched | `src/renderer/src/components/native-chat/messages/actions/use-native-chat-rewind.ts:62-67` | **partly**: only a copy before the message (Fork); in the same session — `rewind\|revert to\|discard this message` in client 0 |
| Chat looks | font, text and code size, width, contrast, "Match terminal interface" | `src/renderer/src/components/settings/appearance/chat/chat-appearance-search.ts:23-93` | **partly**: Compact and Spacious looks (SPEC:1705-1715); no font size, width or contrast |

## Sessions

| Feature | What the user gets | Where in Orca | hqc |
| --- | --- | --- | --- |
| Chat or the bare CLI, by choice | an agent opens as a chat or a terminal, Cmd+Shift+J switches; "Continue with Terminal Agent", "Continue in Chat UI" | `docs/site/content/docs/agents/native-chat.mdx:12-43`; `src/shared/keybindings/definitions-core-4.ts:43`; `src/renderer/src/components/native-chat/pane/actions/NativeChatResumeInCliButton.tsx:80` | **partly**: chat only, no terminal (SPEC:39); a session from VS Code or `claude` in a terminal hqc shows as a mirror and takes over (SPEC §9.2–9.3, 1909-1914) |
| "Hand Off to Another Agent" [multi] | hand the conversation to another agent in the same workspace | `src/renderer/src/components/native-chat/pane/actions/NativeChatPaneHeader.tsx:46`; `src/renderer/src/components/native-chat/pane/actions/use-native-chat-hand-off.tsx` | **partly**: the agent itself starts a session with a prompt via `hqc_spawn` (SPEC:2270-2293); not from the chat menu |
| Chat titles | the agent names the chat by its first message; another agent can be chosen for titles | `src/renderer/src/components/settings/chat/chat-naming-search.ts:7` | **yes**: Claude Code writes the title (`ai-title`, SPEC:342-356), Rename, `hqc_rename` (SPEC:2439) |
| Restarting the agent | a Restart chip after the agent exits | `docs/site/content/docs/model/agents-sessions.mdx:67-69` | **yes**: Restart session (SPEC:1817, 4794-4798) |
| Stopping idle agents | "Stop idle agents after", "Agent sleep": the process is stopped, the next message brings it back with the same history | `src/renderer/src/components/settings/chat/chat-behavior-search.ts:44`; `docs/site/content/docs/agents/hibernation.mdx:8-40` | **partly**: the worker's `idle_timeout` (SPEC:4787-4793) — in `worker.toml`, off by default, not in the UI |
| Continue after a restart | chats that were working continue by themselves after Orca exits or updates; "Continue" after a drop — the agent first checks its last step | `src/renderer/src/components/settings/chat/chat-behavior-search.ts:14`; `src/renderer/src/components/native-chat/usage/NativeChatInterruptedContinue.tsx:247` | **partly**: input is not lost, an interrupted turn is marked `turn_lost` and does not continue by itself (SPEC:4802-4806); Retry on a session in error (SPEC:1914) |
| Agents live without the app open | a daemon keeps the agents' processes after the window closes | `docs/site/content/docs/model/session-restore.mdx:14-42` | **yes**: agents run on workers, no browser needed (SPEC:43-57) |
| Session history of every CLI on the machine | a list of past sessions with filters, Resume, "Copy resume command" | `docs/site/content/docs/agents/session-history.mdx:8-52` | **yes**: for Claude Code — import (SPEC §9.1, 3524-3620), "Import from Claude Code" in a project (SPEC:1753); hqc does not run other agents |
| Full-text search across sessions | search over the text of all past sessions on all machines, "Found on <machine>"; agents search too | `src/renderer/src/i18n/locales/en.json` (`featureTips.sessionSearch.*`, `sessionSearch.panel.*`) | **yes**: the search page (SPEC §15, 1657-1680), `hqc_search_history` (SPEC:2322-2328); one index on the hub |
| Agent Dashboard | a Needs You / Working / Done / Idle board across all worktrees with search and filters, in the window or a separate one | `docs/site/content/docs/model/agents-sessions.mdx:23-58` | **partly**: Needs you / Running / Idle groups in a project (SPEC:1833-1839), "Sessions" across all projects on the phone (SPEC:1867-1870); no Done column and no preview of the last message |
| An agent event feed (Activity) | a chronological feed: who finished, who waits, a preview of the last answer, across all worktrees | `docs/site/content/docs/activity.mdx:6-25` | **no**: no preview on the row (SPEC:1805-1808); "Requests" lists only open requests (SPEC:1608-1618) |
| Workspace Board | a board of workspaces by state todo / in-progress / in-review / completed | `src/shared/keybindings/definitions-core-1.ts:102-120`; `docs/site/content/docs/cli/worktree-checkpoints.mdx:13-24` | **no**: SPEC:38 — hqc "does not duplicate what Hermes offers (… kanban …)" |
| A status from the agent | the agent writes a status comment for the workspace, the human sees it on the card | `docs/site/content/docs/cli/worktree-checkpoints.mdx:5-51` | **no**: the closest is the agent renaming its session (`hqc_rename`, SPEC:2439) |
| State badges | working / waiting / done / blocked / idle on the tab and the row | `docs/site/content/docs/model/agents-sessions.mdx:10-21` | **yes**: the row's dot: waiting, error, working, idle (SPEC:1803-1806) |
| Cmd-J: jump to a session | recent chats and terminals, Cmd-1..6, search over worktrees, tabs, PRs | `docs/site/content/docs/model/quick-open.mdx:20-37` | **partly**: ⌘K finds only projects (SPEC:1789-1797) |
| Pinning, multi-select, order | pin a worktree, Cmd/Shift-click for bulk actions, drag projects | `docs/site/content/docs/model/worktrees.mdx:113` | **partly**: favorites (SPEC:1620-1630), Reorder (SPEC:1840-1843), dragging a session onto a project (SPEC:1819-1822); no multi-select (`multi-?select\|shiftKey` in client — only shortcuts) |
| List filters | hide sleeping ones, ones made by an automation or the CLI, other clients', detached HEAD | `docs/site/content/docs/model/worktrees.mdx:97-107` | **no**: the list has orders and the archive (SPEC:1833-1853), no filters |
| Hide a project without stopping it | the project leaves the list, its agents and automations keep running | `src/renderer/src/components/settings/repository/repository-project-presence-search.ts:30` | **partly**: archiving a project takes it off the bar (SPEC:1784-1789); what happens to its running sessions — not checked |
| A session ID for agents to address each other | "Copy Orca Session ID" | `src/renderer/src/components/native-chat/pane/actions/NativeChatCopyOrcaSessionIdMenuItem.tsx:72` | **yes**: the address `<project>/<slug>` (SPEC:1611-1612), in a link's tooltip (SPEC:1993-1994); no "copy address" button (`copy (the )?(link\|address)` — 0) |
| A prompt cache timer | a countdown to the expiry of Claude's prompt cache | `src/renderer/src/components/settings/agents/behavior/agent-cache-timer-search.ts:7` | **no**: `prompt cache\|cache (timer\|expir)` — 0; only `cache` in the spending line (`client/src/ui/cards.tsx:675`) |
| Context fill | a ring "Context X of Y tokens, N% used" | `src/renderer/src/components/native-chat/usage/NativeChatContextUsageRing.tsx:103-212` | **no**: `context window\|context usage\|tokens left\|context ring` — 0 |

## Permissions

| Feature | What the user gets | Where in Orca | hqc |
| --- | --- | --- | --- |
| Permission modes | Ask for approval / Accept edits / Approve for me / Full access; terminal agents start with full-autonomy flags by default | `src/renderer/src/components/native-chat/session/options/controls/native-chat-permission-mode-labels.ts:31-83`; `docs/site/content/docs/model/agents-sessions.mdx:61-65`; `src/renderer/src/components/settings/agents/agents-search.ts:225` | **partly**: Ask / Auto / Allow all (SPEC:2038-2055); no Accept edits |
| A permission card | the reason, "Needs access to", the plan file, "Answer in terminal" | `src/renderer/src/components/native-chat/prompts/NativeChatApprovalCard.tsx:154-231` | **yes**: the options from the request and a text answer instead of a permission (SPEC:1590-1595, 3868-3876) |
| The agent's questions (AskUserQuestion) | a step-by-step card: Next, Skip, one's own answer, Submit | `src/renderer/src/components/native-chat/prompts/NativeChatQuestionCard.tsx:51-89`; `docs/site/content/docs/agents/native-chat.mdx:51-53` | **yes**: a question card (SPEC §10.1); Answer waits for an answer to every question, no Skip (#73) |
| A shared "waiting for you" list | a Needs You column, questions and permissions of all agents | `docs/site/content/docs/model/agents-sessions.mdx:31` | **yes**: "Requests" with all open requests and the chain of waiting sessions (SPEC:1608-1618) |
| One device holds the input | "Input is held by another device", "Take back" | `src/renderer/src/components/native-chat/composer/controls/native-chat-composer-placeholder.ts:22`; `src/renderer/src/components/native-chat/host-access/use-native-chat-mobile-hold-notice.ts:49` | **not needed with a hub**: hqc has one input queue on the hub, several writers are fine (SPEC:4781) |

## Orchestration

| Feature | What the user gets | Where in Orca | hqc |
| --- | --- | --- | --- |
| A coordinator and workers | Run, Tasks with dependencies, Dispatch; `worker-start` in the current worktree, a new child worktree or on another machine (`--on`), any agent, model and effort per worker | `docs/site/content/docs/cli/orchestration.mdx:8-58, 103-119`; `skills/orchestration/SKILL.md` | **partly**: `hqc_spawn` with `isolation: "worktree"` on the project's worker (SPEC:2270-2314); no tasks with dependencies, no attempt tracking (`dependen\|task graph\|dag` — 0); the only runtime is `claude_code` (SPEC:2283) |
| The coordinator wakes up on mail | the coordinator ends its turn; Orca wakes it when a worker reports (`worker_done`), asks or gets stuck | `docs/site/content/docs/cli/orchestration.mdx:53-72` | **yes**: `hqc_send` wakes the target (`wake_on_send`, SPEC:307, 2235-2236), the answer to `hqc_ask` returns to the caller (SPEC:2240-2244); but `hqc_ask` holds the call until the answer or a timeout, while Orca's `ask` returns at once |
| A worker's questions and decision gates | `ask` with options; a gate blocks a task until decided | `docs/site/content/docs/cli/orchestration.mdx:146-174` | **partly**: `hqc_ask` (SPEC:2240-2244); no gates |
| Group addresses | `@all`, `@idle`, `@claude`, `@worktree:<id>` | `docs/site/content/docs/cli/orchestration.mdx:129-142` | **no**: `broadcast\|@all\|fan.?out`, relevant hits — 0 |
| Workers' permission requests at the coordinator | the workers' permission cards in the coordinator's chat; optionally the coordinator approves itself, with a reason | `docs/site/content/docs/cli/orchestration.mdx:89-99`; `src/renderer/src/components/native-chat/worker-approvals/NativeChatWorkerApprovals.tsx:96-164`; `src/renderer/src/components/native-chat/worker-approvals/worker-approval-card-model.ts:79-104` | **partly**: all open requests and the chain of waiting sessions in "Requests", push leads to the session where the work started (SPEC:1608-1618, 4656-4662); an agent cannot answer a permission request (SPEC:4131) |
| A workers panel at the coordinator | "N need you", a worker's task and last message, "Mention in message" | `src/renderer/src/components/orchestration-crew/OrchestrationCrewPanel.tsx:59-104`; `src/renderer/src/components/orchestration-crew/CrewRowMenu.tsx:52` | **partly**: "started by <address>" in the worker's header (SPEC:2056-2059), messages between sessions in the feeds (SPEC:1991-2007); no summary of workers at the coordinator |
| Releasing and cleaning up workers | a finished worker is released, its worktree is removed by a rule | `src/renderer/src/components/settings/orchestration/OrchestrationWorkerAutoReleaseSetting.tsx`; `src/renderer/src/components/settings/workspace-cleanup/workspace-cleanup-settings-search.ts:99` | **partly**: a worktree from `hqc_spawn` is marked for removal when its last session is archived (SPEC:4833-4834) |
| A limit on worker nesting | a setting for the depth of nested workers | `src/renderer/src/components/settings/orchestration/nested-worker-depth-copy.ts` | **yes**: limits on chains, spawns and messages (SPEC:118-122) |

## Git and PRs

| Feature | What the user gets | Where in Orca | hqc |
| --- | --- | --- | --- |
| A worktree per task | "+" on a project: the worktree is created in the background, a choice of machine (Run on) and agent | `docs/site/content/docs/model/worktrees.mdx:25-77` | **partly**: a worktree by hand in the project settings (SPEC §13.1, 4290-4311) and `hqc_spawn isolation: worktree`; no option in the new-session window |
| Start from a branch, a commit, a remote branch; parent and children | a start-from picker, a parent workspace under Advanced | `docs/site/content/docs/model/worktrees.mdx:29-36, 83-91` | **partly**: a new worktree branch (SPEC:245-261), `hqc_spawn` — from the source's HEAD (SPEC:2300-2301) |
| `orca.yaml`: setup, shared folders, `.env` copies, tabs | a setup script, `sharedDirectories`, `.worktreeinclude`, default tabs | `docs/site/content/docs/model/orca-yaml.mdx:10-46`; `docs/site/content/docs/model/worktrees.mdx:38-65` | **no**: `setup (script\|hook\|command)\|post.?create`, relevant hits — 0 |
| A workspace from an issue or a PR | from GitHub, GitLab, Linear, Jira, linked to the task | `docs/site/content/docs/model/worktrees.mdx:75`; `docs/site/content/docs/review/github.mdx:41`; `docs/site/content/docs/review/linear.mdx:19` | **no**: `pull request` — only deny rules (SPEC:4394) |
| Tasks: GitHub, Linear, Jira, GitLab issues | a list, editing fields, GitHub Projects | `docs/site/content/docs/review/github.mdx:39-55`; `docs/site/content/docs/review/linear.mdx:15-27`; `docs/site/content/docs/settings.mdx:99-106` | **no**: `issue\|linear\|jira`, relevant hits — 0 (only "issued", `linear-gradient`) |
| Diff of a workspace | against the base, per commit, images, three sides of a conflict | `docs/site/content/docs/review/diff-viewer.mdx:9-18` | **partly**: a diff of each edit (SPEC:3871-3876), diff attachments (SPEC §12); no combined one (SPEC:39) |
| Notes on diff lines | comments on lines, "Send to agent" in a batch | `docs/site/content/docs/review/annotate-ai-diff.mdx:14-37`; `src/shared/keybindings/definitions-core-3.ts:132-141` | **no**: `review (note\|comment)\|line comment\|annotat` — 0 |
| Lines written by AI | a mark in the diff gutter | `docs/site/content/docs/review/attribution.mdx:5-19` | **no**: in Orca this is only in the docs, not found in code or UI strings (`aiAuthored\|lineAttribution\|ai-attribution` — 0) |
| Commit, push, "Fix with AI" | stage, commit, an AI message, push, force-with-lease, amend, "Resolve with AI" for conflicts | `docs/site/content/docs/review/commit-push.mdx:7-57` | **no**: `git push\|commit message` — only deny rules (SPEC:4394) |
| Pull requests | creation with AI text, stacks, auto-merge, merge queue, review, replies, reactions, "Fix broken checks" | `docs/site/content/docs/review/github.mdx:13-37`; `docs/site/content/docs/review/commit-push.mdx:23-29` | **no**: `pull.?request\|merge request\|\bPR\b` — only SPEC version entries and deny rules (SPEC:4394, 5093) |
| Code hosts | GitHub, GitLab, Bitbucket, Azure DevOps, Gitea | `docs/site/content/docs/review/github.mdx:11`; `src/renderer/src/components/settings/integrations/integrations-search.ts:23-151` | **no**: `github\|gitlab\|bitbucket\|gitea` — only `GITHUB_TOKEN` in a project's `env` (SPEC:4443, 4468) and a clone URL |
| A branch named after the work | auto-renaming the branch once the agent has started | `src/renderer/src/components/settings/source-control/auto-rename-branch-search.ts:8` | **no**: `rename (the )?branch\|branch name` — only the branch-name check (`crates/hqc-proto/src/frame.rs:713`) |
| A fresh main, a fork synced with upstream | fast-forwarding the base on creation, syncing a fork | `src/renderer/src/components/settings/source-control/git-search.ts:34`; `src/renderer/src/components/settings/repository/repository-search.ts:132` | **no**: `fast.?forward\|upstream\|fork sync`, relevant hits — 0 |
| Cleaning up workspaces by rules | merged, unchanged, made by automations and workers; a warning about `.env` | `src/renderer/src/components/settings/workspace-cleanup/workspace-cleanup-settings-search.ts:20-134` | **partly**: deleting what hqc created (SPEC §13.2), ephemeral worktrees (SPEC:4833-4834) |
| A folder with several repositories | one project over several repos, a chat in the parent folder | `docs/site/content/docs/model/worktrees.mdx:129-135` | **yes**: a workspace is any folder (`dir`, SPEC:245) |

## Terminal, browser, editor, devices

| Feature | What the user gets | Where in Orca | hqc |
| --- | --- | --- | --- |
| Terminal | xterm.js with WebGL, splits, search, themes, import from Ghostty, Kitty, Warp | `docs/site/content/docs/terminal.mdx`; `src/renderer/src/components/settings/terminal/appearance/terminal-typography-search.ts:113` | **no**: `pty\|xterm\|terminal panel` — only about "claude in a terminal" (SPEC:364) |
| A floating terminal [desktop] | a terminal above everything on Mod+Alt+A | `docs/site/content/docs/terminal.mdx:138-145` | **no**: `terminal\|xterm\|pty` — only about `claude` in a terminal and the mirror |
| Quick Commands | saved commands and prompts to agents, shared with the phone | `docs/site/content/docs/terminal.mdx:147-153`; `docs/site/content/docs/mobile.mdx:71-80` | **no**: `template\|snippet\|saved prompt\|prompt preset` — only a project's session template in the backlog (SPEC:476-480, §13.3) |
| Tabs and splits [desktop] | an agent, a browser, a diff side by side; each workspace has its own layout | `docs/site/content/docs/model/tabs-panes-splits.mdx:15-53` | **partly**: "Open in new tab" on a session and a project (SPEC:1782, 1816) |
| A browser in the workspace [desktop] | Chromium, profiles, cookies, Chrome extensions, viewport size | `docs/site/content/docs/browser/overview.mdx:7-62`; `src/renderer/src/components/settings/browser/browser-search.ts:181` | **no**: `browser pane\|chromium\|playwright\|devtools` — only MCP tool names in the feed (SPEC:1921) |
| Design Mode [desktop] | a click on a page element goes to the chat with its HTML, styles and a screenshot | `docs/site/content/docs/browser/design-mode.mdx:7-27` | **no**: same, no browser |
| The agent drives the browser | `orca snapshot`, `click`, `fill`, `screenshot` | `docs/site/content/docs/cli/overview.mdx:88-105` | **no**: the agent has only its own MCP servers |
| An editor and Cmd+P | Monaco, a Markdown editor, file search | `docs/site/content/docs/editing/monaco.mdx`; `docs/site/content/docs/editing/markdown.mdx`; `docs/site/content/docs/model/quick-open.mdx:10-12` | **no**: "not an IDE" (SPEC:39) |
| Viewers | PDF, CSV, notebooks, Mermaid, HTML, images | `docs/site/content/docs/editing/viewers.mdx:9-35` | **partly**: images and Markdown in the viewer, HTML in an iframe (SPEC:4212, 4246-4252); no PDF, CSV or ipynb |
| A file tree, files to the agent | an explorer with git state, drag a file into the prompt | `docs/site/content/docs/editing/file-explorer.mdx:5-26` | **partly**: files can be dragged into the input box; no tree |
| An iOS and Android simulator [desktop] | the agent taps and types in a simulator | `docs/site/content/docs/cli/overview.mdx:107-121`; `docs/site/content/docs/cli/skills.mdx:28-29` | **no**: `simulator\|emulator\|computer.?use` — 0 |
| Computer use [desktop] | the agent sees and drives the machine's apps | `README.md:211`; `docs/site/content/docs/cli/skills.mdx:26` | **no**: `simulator\|emulator\|computer.?use` — 0 |
| A workspace's ports | live ports in the status bar, localhost addresses per workspace | `src/renderer/src/components/settings/appearance/appearance-status-bar-search.ts:243`; `src/renderer/src/components/settings/browser/browser-search.ts:162` | **no**: `port(s)? (list\|forward)\|localhost url` — 0 |
| Resource Manager | CPU, memory, terminals, disk of workspaces; End a terminal | `docs/site/content/docs/settings.mdx:29`; `docs/site/content/docs/terminal.mdx:13-21` | **no**: `cpu\|memory usage\|disk usage` — 0 |
| A public link to HTML and Markdown | Share as artifact through an Orca account | `docs/site/content/docs/settings.mdx:91-97`; `docs/site/content/docs/cli/overview.mdx:84-86` | **no**: `hqc_publish\|public link` — 0; there is a signed link for 10 minutes (SPEC:1602-1606); publishing by link is being prepared as a separate PR |

## Machines and access

| Feature | What the user gets | Where in Orca | hqc |
| --- | --- | --- | --- |
| One client for many machines | agents work while the laptop sleeps | `docs/site/content/docs/ways-to-run.mdx:43-53`; `docs/site/content/docs/remote-servers.mdx:99-111` | **yes**: a hub and workers (SPEC:43-57) |
| Remote Orca Server | an access link over Tailscale or LAN; `orca serve` without a window; a separate Linux server package | `docs/site/content/docs/remote-servers.mdx:47-208` | **yes**: a worker is one binary that connects to the hub by itself (SPEC:48-49, 4865-4866) |
| Orca Relay | phone and computer without port forwarding, through Orca's cloud, an account sign-in needed | `docs/site/content/docs/mobile.mdx:39-44, 122-133`; `cloud/apps/relay` | **yes**: the hub behind Cloudflare Tunnel and Access (SPEC:55-56) |
| End-to-end encryption | "Everything is encrypted end-to-end" for the phone–computer pair and the web client | `mobile/src/i18n/locales/en.json:619`; `src/renderer/src/web/web-e2ee.ts` | **no**: `end-to-end\|e2ee` — 0; the hub sees the whole history |
| SSH targets | agents on a remote machine, the UI on the laptop | `docs/site/content/docs/ways-to-run.mdx:29-41` | **not needed with a hub**: the worker is installed on the machine itself |
| A cloud VM per workspace | from a recipe in `orca.yaml`: Vercel Sandbox, Fly, Modal, Docker | `docs/site/content/docs/ways-to-run.mdx:72-111` | **no**: `isolation` — only `worktree` (SPEC:2295) |
| Pairing, revoking devices | a code or QR, a token per client, revocation in a list | `docs/site/content/docs/mobile.mdx:37-59`; `docs/site/content/docs/remote-servers.mdx:113-121` | **partly**: a worker connects with a code and can be revoked (SPEC:1637-1645); browsers sign in through Access or OIDC, no list of devices |
| Web client | a browser connects to Orca with a code | `src/renderer/src/components/settings/runtime-environments/catalog/runtime-environments-search.ts:105`; `src/renderer/src/web/WebConnect.tsx:135-166` | **yes**: hqc is a web client (SPEC:53) |
| Machine name | the name under which the computer is seen by others | `src/renderer/src/components/settings/machine-identity/machine-name-search.ts:19` | **yes**: the worker's name (SPEC:1641) |
| Keep the machine awake [desktop] | Caffeinate: always or while an agent works | `docs/site/content/docs/settings.mdx:65` | **no**: `caffeinate\|keep (the )?(machine\|computer) awake\|prevent sleep` — 0 |
| A memory limit for terminals on a server | a cgroup `MemoryMax` per terminal | `docs/site/content/docs/remote-servers.mdx:210-225` | **no**: `memorymax\|cgroup` — 0 |
| A WSL distribution as a host [desktop] | Orca runs its server inside the distribution | `src/renderer/src/components/settings/wsl/hosts/wsl-hosts-experimental-search.ts:8` | **not checked**: whether a worker runs inside WSL as on Linux was not tried |

## Phone

| Feature | What the user gets | Where in Orca | hqc |
| --- | --- | --- | --- |
| Native apps [mobile] | iOS (App Store) and Android (Google Play, since mobile-android-v0.0.65) | `docs/site/content/docs/mobile.mdx:8-14`; release mobile-android-v0.0.65 | **partly**: a PWA, an icon on the Home Screen (SPEC:1863-1887) |
| An Agents tab | all agents of all computers, grouped by state, workspace, project, agent, computer; Mark as unread, Clear finished | `mobile/src/i18n/locales/en.json` (`agents.*`, around 4978-4994); release mobile-android-v0.0.65 | **partly**: "Sessions": live sessions of all projects by state with unread marks (SPEC:1867-1872); no mark-as-unread, no clearing finished ones |
| Feed and spending on the home screen | Feed and Usage tabs; Agents spawned, Agent time, PRs created counters | `mobile/src/i18n/locales/en.json:651, 674, 676` | **no**: the first screen shows projects or sessions (SPEC:1863-1867) |
| Chat on the phone | `@` files, `/` commands, a model picker, photos, dictation | `docs/site/content/docs/mobile.mdx:61-69` | **partly**: chat, attachments, `/` (SPEC:1863-1887); no `@`, no dictation |
| A terminal on the phone [mobile] | a live terminal, an input mode without Return, Quick Commands, font settings | `docs/site/content/docs/mobile.mdx:24, 71-80, 113-118` | **no**: `terminal\|xterm\|pty` — no terminal |
| Files on the phone [mobile] | a tree and a viewer of the workspace's files | `mobile/app/h/[hostId]/files` | **no**: no file tree on the desktop either (SPEC:39) |
| Source control and PRs on the phone [mobile] | stage, commit, push; PRs: merge, reviewers, threads, "Fix with AI"; diff review | `docs/site/content/docs/mobile.mdx:105-111`; `mobile/app/h/[hostId]/{source-control,pr,review}`; `mobile/src/i18n/locales/en.json` (`pullRequest.*`) | **no**: `pull.?request\|\bPR\b`, `git push\|commit message` — only deny rules (SPEC:4394) |
| A browser on the phone [mobile] | the workspace's page as Web or Mobile | `docs/site/content/docs/mobile.mdx:105-111` | **no**: no browser on the desktop either |
| Accounts and limits | agent accounts, a Codex reset, limits | `docs/site/content/docs/mobile.mdx:86-90`; `mobile/app/h/[hostId]/accounts.tsx` | **partly**: the session worker's limits in the More menu (SPEC:1634-1636) |
| A new workspace from the phone | from an issue, a branch, an agent | `docs/site/content/docs/mobile.mdx:92-94`; `mobile/app/h/[hostId]/new-workspace.tsx` | **partly**: a new session with project and folder chips (SPEC:1889-1907) |
| Automations on the phone | list, run, edit | `mobile/app/h/[hostId]/automations.tsx` | **partly**: Schedules in the More menu: list, pause, delete; only an agent creates them (`client/src/ui/Schedules.tsx:1-2`; SPEC:2338) |
| Dictation on the phone [mobile] | the phone records audio and sends it in chunks to its computer, which transcribes it | `mobile/src/dictation/mobile-dictation-operations.ts:52-66`; `mobile/src/hooks/mobile-dictation-audio-chunk.ts:7` | **no**: `dictat\|microphone\|getUserMedia\|speech` in SPEC and client — 0 |
| Push with filters | only when away from the computer (3 minutes without keyboard input, or locked), only when an agent waits, sound; paused after 7 days without signing in | `mobile/src/i18n/locales/en.json:878, 884` (`notifications.*`) | **partly**: Web Push for requests and answers, no push while the session is open (SPEC:4637-4642, 4660-4661); on an iPhone only from the PWA (SPEC:4642-4643); no filters |
| Several hosts, editing a host's address [mobile] | the phone keeps a pairing with each computer | `docs/site/content/docs/mobile.mdx:37-59` | **not needed with a hub**: one hub for all workers (SPEC:43-57) |

## Notifications

| Feature | What the user gets | Where in Orca | hqc |
| --- | --- | --- | --- |
| Notification categories | agent finished, waits for input, terminal bell; a choice of machines; a test | `src/renderer/src/components/settings/notifications/notifications-search.ts:7-217`; `docs/site/content/docs/notifications.mdx:7-30` | **partly**: one switch per device (`client/src/ui/Settings.tsx:25-31`); events — requests, the final answer, a message from a session, undelivered input (SPEC:4637-4641) |
| A bell and "Mark unread" | a list of unread items in the header, return to unread | `docs/site/content/docs/notifications.mdx:32-41`; `docs/site/content/docs/activity.mdx:6-25` | **partly**: unread marks on rows and projects, a dot on the browser tab (SPEC:1715-1717); cannot return to unread (`mark (as )?(un)?read` — 0) |
| A Dock badge [desktop] | the number of waiting items on the icon | `docs/site/content/docs/notifications.mdx:43` | **no**: `setAppBadge\|badge` — only badges in lists; a macOS wrapper with a badge is being prepared as a separate PR |
| Sounds | a custom MP3, WAV, OGG, M4A, AAC, FLAC sound, volume | `docs/site/content/docs/notifications.mdx:45-52`; `src/renderer/src/components/settings/notifications/notifications-search.ts` | **no**: `sound\|audio` in SPEC and client — 0 |
| No notifications while you look | "Suppress while focused" | `src/renderer/src/components/settings/notifications/notifications-search.ts` | **yes**: no push while the session is open, or the session waiting on it (SPEC:4641-4642, 4660-4661) |
| PRs and updates | a PR check failed, an update is out | `docs/site/content/docs/settings.mdx:110-113` | **no**: `check(s)? fail\|ci (failing\|green)\|update available`, relevant hits — 0 |

## Models, accounts, spending

| Feature | What the user gets | Where in Orca | hqc |
| --- | --- | --- | --- |
| 34 agents [multi] | Claude Code, Codex, Gemini, OpenCode, Cursor CLI, Aider and others | `README.md:216-256` | **partly**: only Claude Code (SPEC:2283; `crates/hqc-worker/src/adapter.rs`) |
| Several accounts | several Claude and Codex accounts, switching on the fly | `docs/site/content/docs/settings.mdx:68-72`; `src/renderer/src/components/settings/accounts/accounts-claude-search.ts:7-26`; `docs/site/content/docs/agents/claude-code.mdx:21-23` | **partly**: the account is the one the worker is signed in with (`crates/hqc-worker/src/adapter.rs:73-74`); different machines — different accounts |
| Provider limits | 5 hours, a day, a week for all providers; a warning at 80%; a format in the status bar | `docs/site/content/docs/agents/usage-tracking.mdx:5-40`; `src/renderer/src/components/settings/appearance/appearance-status-bar-usage-format-search.ts:8` | **partly**: Claude's five hours and seven days for the session's worker (`client/src/ui/limits.tsx:1-46`; SPEC:1634-1636) |
| Spending statistics | Stats: cost estimate, cache, top projects, 7 days, "Share on X" | `docs/site/content/docs/agents/usage-tracking.mdx:41-55`; `ClaudeUsagePane` strings in `src/renderer/src/i18n/locales/en.json` | **partly**: a turn's spending and the session total (SPEC:513; `client/src/ui/cards.tsx:672-676`); no summaries by day or project |
| Continuing after a limit reset | "Continue where you left off" by itself when the limit resets; "Continue Now" | `src/renderer/src/components/settings/chat/chat-behavior-search.ts:29`; `src/renderer/src/components/native-chat/usage/NativeChatUsageLimitAutoContinue.tsx:37-57`; `src/renderer/src/components/native-chat/messages/notices/NativeChatNoticeRow.tsx:361-364` | **no**: `auto.?continue\|continue where you left` — 0 |
| Continue under another account | hit the limit — continue the conversation under another account | release v1.4.259, line 77; `src/renderer/src/components/native-chat/usage/NativeChatUsageLimitActions.tsx:254` | **no**: `another account\|switch account` — 0; the account is set by signing in on the worker |
| Polling limits | an OAuth usage request or the CLI's status line | `docs/site/content/docs/agents/usage-tracking.mdx:24-40` | **yes**: from Claude Code's `rate_limit_event` (`crates/hqc-proto/src/frame.rs:1102-1103`; SPEC:1235-1236) |

## Automation, CLI, skills, plugins

| Feature | What the user gets | Where in Orca | hqc |
| --- | --- | --- | --- |
| Automations | a schedule by preset, cron or RRULE; a new session in a repo or in a workspace; a precheck; missed runs; Run now, Rerun, history | `docs/site/content/docs/cli/automations.mdx:5-155` | **partly**: `hqc_schedule`: a message to an existing session by `at` or `cron`, `catch_up` (SPEC:2329-2338, 4664-4699); creating a session per run is described (SPEC:4665-4666), but `crates/hqc-hub/src/schedules.rs` has no `workspace`; no precheck, no run history |
| The `orca` CLI | workspaces, terminals, browser, orchestration from any shell and by an agent | `docs/site/content/docs/cli/overview.mdx:15-127` | **partly**: hqc's MCP tools for the agent (SPEC §7, 2213-2456); no CLI for a human |
| Skills | install, update, share by link | `docs/site/content/docs/cli/skills.mdx:16-140` | **no**: a skill is seen only by name in the call line (SPEC:1922) |
| An MCP server registry | search, install and configure MCP servers | `docs/site/content/docs/cli/skills.mdx:141-181`; `src/renderer/src/components/settings/repository/mcp/mcp-config-search.ts:14` | **no**: `mcp server\|mcp registry\|install (a )?skill\|plugin` — a machine's MCP servers are set only in the worker's local config (SPEC:87) |
| Plugins | Orca plugins | `docs/site/content/docs/settings.mdx:170-175`; `docs/reference/plugin-*.md` | **no**: the same search — plugins are seen only as the prefix of an MCP server name in the feed (`client/src/ui/feed.ts:431`) |
| Slack, Discord, Linear connectors | — | `src/main/connector-cloud/connector-hello.ts:13`; `cloud/packages/connector-contract/src/connector-wire-scalars.ts:16` | **not checked**: not found in Orca's docs or UI strings; hqc has Telegram through Hermes (SPEC:38, 4643-4644) |

## Looks, keyboard, settings

| Feature | What the user gets | Where in Orca | hqc |
| --- | --- | --- | --- |
| Themes, accent, density | a theme, an accent colour, UI density | `docs/site/content/docs/settings.mdx:24-32`; `src/renderer/src/components/settings/appearance/appearance-search.ts:18-126` | **partly**: six palettes (three dark and three light), Compact and Spacious, a project colour (SPEC:159-163, 1705-1741); no custom accent |
| UI language | 12 languages, Russian among them | `docs/site/content/docs/settings.mdx:24-32` | **no**: the UI is in English, dates via `toLocaleString('en-US')` (`client/src/ui/labels.ts:126-145`) |
| App icon [desktop] | a choice of icon | `src/renderer/src/components/settings/appearance/appearance-app-icon-search.ts:8` | **no**: `app icon\|pet\|mascot`, relevant hits — 0 |
| Key bindings | 100 actions, rebindable, `~/.orca/keybindings.json` | `docs/site/content/docs/settings.mdx:143-154`; `src/shared/keybindings/definitions-core-{1,2,3,4}.ts` | **partly**: ⌘K — find a project (SPEC:1790), ⌘\ — the session column (SPEC:1857); cannot be rebound |
| Search in settings | Cmd-, and a search | `docs/site/content/docs/settings.mdx:5`; `src/shared/keybindings/definitions-core-3.ts:214` | **no**: `settings search` — 0 |
| A pet | an animated pet | `src/renderer/src/components/settings/advanced/experimental-search.ts:13` | **no**: `app icon\|pet\|mascot`, relevant hits — 0 |
| Confirmations | "ask before…" settings | `src/renderer/src/components/settings/general/general-confirmations-search.ts:7-94` | **yes**: deletion dialogs and archiving with Undo (SPEC:1818, 1843-1847) |
| Telemetry | sending statistics, can be turned off | `src/renderer/src/components/settings/privacy/privacy-search.ts:14-67` | **not needed with a hub**: hqc has no telemetry (`telemetry\|analytics` — 0) |
| Tray and menu bar [desktop] | a tray icon, the window in the background | `src/renderer/src/components/settings/appearance/appearance-system-presence-search.ts:10-52` | **no**: the worker's tray is described as optional (SPEC:4866), there is none in the code (`crates/hqc-worker/src/connection.rs:74`: "later the tray") |

## Not checked

- 23 files of Orca's docs were not opened: `agents/antigravity-accounts.mdx`,
  `agents/antigravity.mdx`, `agents/codex-hot-swap.mdx`, `agents/codex.mdx`, `agents/cursor-cli.mdx`,
  `agents/glm-agent.mdx`, `agents/supported.mdx`, `android-apk.mdx`, `browser/profiles.mdx`,
  `cli/computer-use.mdx`, `cli/reference.mdx`, `github-errors.mdx`, `index.mdx`, `install.mdx`,
  `recipes/design-mode-fix.mdx`, `recipes/jump-worktrees.mdx`, `recipes/parallel-agents.mdx`,
  `recipes/remote-worktrees.mdx`, `recipes/review-ai-diff.mdx`, `review/jira.mdx`, `ssh.mdx`,
  `telemetry.mdx`, `troubleshooting.mdx`. Release notes v1.4.259 were read for 340 of 636 lines.
- Orca was not run: everything about it comes from its docs, UI strings and code.
- The mark for lines written by AI exists only in Orca's docs, not found in code.
- Whether Orca's `/goal` works with Claude and not only with Codex.
- Slack, Discord, Linear connectors — only protocol and cloud code found, no docs or UI.
- Whether Orca's web client (not the app) receives push.
- hqc: a worker inside WSL; what archiving a project does to its running sessions; whether
  attachments in a draft survive a reconnect; a line-wrap toggle on a code block.
