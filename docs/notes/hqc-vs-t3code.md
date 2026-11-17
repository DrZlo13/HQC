# hqc and T3 Code: what it has that we do not

T3 Code (https://t3.codes, https://github.com/pingdotgg/t3code, MIT) is an open-source front end
for Claude Code, Codex, Antigravity, OpenCode, Cursor and Grok: an Electron desktop app, a web
client and native iOS and Android apps. This note lists every feature of it a user sees and, for
each one, whether hqc has it.

The two products are built differently. In T3 every machine runs its own server, and a client
connects to each machine separately. In hqc there is one hub, and the workers connect to it
themselves. So T3's features for reaching machines (QR pairing, routes, Tailscale) are not needed
in hqc. Another difference: the hqc SPEC says outright "it is not an IDE: no file editor, diffs
only as artifacts" (`docs/SPEC.md:39`). T3's git panel, terminal, file viewer and browser fall
under that limit.

## What was compared

- T3 Code — commit `14f295e` (2026-11-15 07:21 −0800). T3 paths are from the root of its
  repository; short ones such as `composer.md:15` are `docs/user/composer.md`.
- What was read of T3: the user docs `docs/user/` (of 49 files, 30 read in full, 7 by headings and
  opening — secrets, background-service, managed-policy, updating, install, cursor, telemetry; 12
  not opened — guides to other agents, 1Password, T3 Connect troubleshooting, licences), the README,
  release notes v0.1.7–v0.1.9, the pages https://t3.codes and https://t3.codes/download. Code —
  selectively: client components and routes, default key bindings, settings, MCP tools for agents,
  voice input. Feature behaviour is described from the docs, except where code is cited.
- hqc — `origin/master` `5e11381`. hqc paths are from the root of the repository. Each "no" names
  the patterns searched for in `docs/SPEC.md`, `client/src` and `crates/` (tests excluded).
- Marks: [multi] — makes sense only with several providers; [desktop] — only for a desktop app;
  [mobile] — only for a native phone app.

## In short

| In hqc | T3 features |
| --- | ---: |
| no | 56 |
| partly | 36 |
| yes | 21 |
| not needed with a hub | 1 |
| not checked | 1 |
| total | 115 |

What hqc lacks entirely and what matters most with many sessions on several machines, used from a
browser and a phone:

1. **Voice input.** On a phone it is the main way to answer a session quickly. How T3 does it is
   below, in "How T3 does voice input".
2. **Continuing after a limit reset, and "snooze until reset".** Today a session that hit its limit
   stands still until someone gets to it, though hqc already knows the reset time
   (`worker.limits`).
3. **Context window fill.** From a phone you cannot see that a long session is close to
   compaction.
4. **Side chat (`/side`, `/btw`).** Ask a working session "what are you doing" without disturbing
   its turn.
5. **Rewind to a message together with the files.** "Fork from here" leaves the files as they are.
6. **Snooze a session until a time.**
7. **A terminal on the worker's machine from the browser.** Against "not an IDE" in SPEC.
8. **Runs started by a webhook** (CI failed, a PR came in). Today there are only schedules.
9. **Live Activity and widgets** — the agent's turn on the lock screen. Cannot be done from a PWA;
   needs a native client.
10. **Reasoning effort in the model picker.**

Of what is done partly, the input queue (edit, delete, choose "join the turn or wait") and a
combined diff of a turn (against "not an IDE") most deserve finishing.

Only in hqc, T3 does not have it: the live mirror and takeover of VS Code sessions, messages and
questions between sessions, the shared "Requests" list, folder rules, moving a session to another
project or folder, the token-rate bar on the project bar, web search for selected text, Web Push
without an open client.

## How T3 does voice input

- **Browser and desktop.** The browser records the microphone (`AudioWorkletNode`,
  `apps/web/src/speech/browserPcmCapture.ts:91`) and sends the audio to the machine chosen in
  Settings → Voice; until a machine is chosen, audio goes nowhere (`voice-input.md:15-25`). That
  machine's server transcribes locally: `transcribe-cpp` 0.4.1 in a separate process
  (`apps/server/package.json:75`; `docs/internals/voice-input.md:56-68`), with the Silero detector
  cutting silence (`apps/server/src/speech/silero.ts:3`). Models come from the catalogue
  `apps/server/src/speech/catalog.json`: 59 GGUF models, 21 of them list Russian among their
  languages; among them GigaAM v3, Whisper Large v3 Turbo, Parakeet TDT v3, Qwen3-ASR. Instead of a
  local model, the recording can go to the cloud: OpenAI `gpt-transcribe`, xAI
  `grok-voice-transcribe-2.0` (`apps/server/src/speech/CloudSpeech.ts:31-32`) or OpenRouter; the
  key is stored on the machine.
- **What the user sees.** A microphone button or `mod+shift+d`, recording up to 5 minutes. The text
  lands in the draft at the cursor and sends nothing by itself. There is a vocabulary of terms
  (personal and per project), filler-word removal and an optional "cleanup" of the transcript by a
  text model, off by default (`voice-input.md:79-143`). Dictation also works in diff comments, PR
  comments and answers to questions.
- **Phone.** By default the device transcribes: iOS 26+ through `@react-native-ai/apple`
  (`apps/mobile/package.json:70`, `apps/mobile/src/native/voiceTranscription.ios.ts:2`), Android
  12+ through ML Kit GenAI Speech Recognition
  (`apps/mobile/modules/t3-voice-transcription/android/build.gradle:21`). Optionally on one's own
  machine (`composer.md:205-244`).
- **hqc.** The hub sends no headers that forbid the microphone: `Permissions-Policy|microphone` over
  `crates/hqc-hub/src` and SPEC — zero matches. The only mention of voice is the
  `voiceEnabled:false` flag with which the worker turns off Claude Code's own voice
  (`crates/hqc-worker/src/title.rs:50-52`).

## Input box

| Feature | What the user gets | Where in T3 | hqc |
|---|---|---|---|
| Voice input | dictation into the draft | `voice-input.md`; `composer.md:199-244` | **no**: `voice\|dictat\|microphone\|getUserMedia\|speech`, `MediaRecorder\|SpeechRecognition\|whisper\|audio` — 0 files in client |
| Queue or steer during a turn | choice of Queue/Steer; edit, delete and "Send now" for queued messages | `composer.md:63-91, 246-275` | **partly**: an input joins the turn or waits for its end by itself (SPEC §17.2, 4748-4786), an "Input queued" label (`client/src/ui/cards.tsx:336`); no mode choice, no edit or delete; "interrupt and send" is only in SPEC:4779, no `interrupt` found in client |
| Offline queue [mobile] | drafts and attachments without a network | `composer.md:93-98` | **partly**: the draft is kept in the browser (SPEC:1720-1722), input for an offline worker waits on the hub (SPEC:4672-4674); the client keeps nothing while offline |
| Markdown editor with formatting | lists, headings and code are formatted while typing | `composer.md:15-38` | **no**: a plain textarea (`Chat.tsx:1190-1196`); `rich.?text\|wysiwyg\|tiptap` — 0 |
| Attachments | paste, drag and drop, HEIC→JPEG | `composer.md:40-61` | **yes**: paste (`Chat.tsx:1196`), drag and drop (`Chat.tsx:423-433`), HEIC (`client/README.md:32-39`); up to 32 files per message (SPEC:1606), up to 100 MB per file (SPEC:4171) |
| A long paste as a file | a paste of 32 KiB or more becomes an attachment | `composer.md:9-13` | **no**: only in the backlog (`docs/BACKLOG.md:35-46`) |
| Files through "Share" [mobile] | sending to T3 from another app | `composer.md:56-58`; `apps/mobile/src/features/sharing` | **no**: `share_target` in `client/public/manifest.webmanifest` — 0 |
| Quote from an answer with a comment | a selected part of an answer becomes a chip with a comment | `composer.md:129-146` | **partly**: "Quote" inserts the selection as `> ` lines (SPEC:1962-1965), no comment and no jump to the source |
| Previous prompt with ↑ | | `composer.md:148-159` | **no**: `ArrowUp` only in lists (`SessionList.tsx:333`, `ProjectRail.tsx:178`) |
| Next-prompt suggestion (Labs) | | `composer.md:161-169` | **no**: `suggest(ed)? prompt\|prompt suggestion` — 0 |
| "Edit from here" | the conversation rewinds to a message, optionally with the files | `composer.md:171-186`; `apps/server/src/checkpointing/` | **partly**: "Fork from here" makes a copy of the session before the message (SPEC:1521-1550, `cards.tsx:582`); no rewind of files — `rewind\|revert\|checkpoint` in client — 0 |
| Stash a prompt (Cmd+S) | | `composer.md:188-197` | **no**: `stash` — 0; only the draft on the device |
| Slash commands and skills | a `/` menu and a skill picker on `$` | `composer.md:277-291` | **partly**: `/` completes CLI commands (SPEC:2066-2071); no separate skill picker |
| Restart the agent | reload MCP and skills | `composer.md:286-288` | **yes**: Restart session (SPEC:1510-1513, 1817) |
| `/compact` | | `composer.md:293` | **yes**: a CLI command; the "Context compacted" line opens to the summary (`cards.tsx:436`) |
| `/clear` in the same session | | `composer.md:296-300` | **no**: `/clear` — 0 (a separate PR is in preparation) |
| Context window fill | | `composer.md:302-304`; `usage.md:146-151` | **no**: `context window\|context usage\|tokens left` — 0 |
| `/goal` | the agent works until a condition is met | `composer.md:309-325` | **no**: `goal` — 0 |
| Context chips | a terminal excerpt, a diff comment, a PR with `#`, a branch with `@`, a model with `@opus` | `composer.md:327-383` | **partly**: the agent reaches other sessions by itself (`hqc_send`, `hqc_ask`, `hqc_search_history`; SPEC:2232-2329); the user cannot insert a chip for a session — `mention` — 0 |
| Attachment viewer | code, MD, HTML, CSV, audio, ZIP, PDF | `composer.md:385-430`; `apps/web/src/components/files/` | **partly**: images and Markdown in the viewer (SPEC:4246, `attachments.tsx:5`), HTML in a sandboxed iframe (`attachments.tsx:346`); no PDF, CSV, audio, video or ZIP |
| One task to several models at once | | `thread-sidebar.md:56-59` | **no**: `several models\|multiple models` in client — 0 |
| Send and open a new draft right away | | `thread-sidebar.md:49-55` | **no**: `start in background` — 0 |
| Files in an answer to the agent's question | | `question-attachments.md:1-9` | **no**: `clarify.*(artifact\|attach\|file)` — 0 |
| A text answer instead of a permission | | `permission-modes.md:51-52` | **yes** (SPEC:1594-1595) |

## Sessions

| Feature | What the user gets | Where in T3 | hqc |
|---|---|---|---|
| A session without a project | its own temporary folder, later "Turn into project" | `thread-sidebar.md:25-47` | **partly**: the worker can make a temporary folder (SPEC §8.4, 3454-3462; `crates/hqc-worker/src/cleanup.rs:5`), the client does not create a session without a workspace (SPEC:1892-1893) |
| Pinning and manual order | | `thread-sidebar.md:61-119` | **yes**: favorites above the list (SPEC:1620-1630), Reorder (SPEC:1840-1843) |
| Undo for any action on the list | | `thread-sidebar.md:65-70` | **partly**: Undo only for archiving a session or a project (SPEC:1786-1788, 1843-1847) |
| A file dropped on a session row becomes an attachment | | `thread-sidebar.md:72-75` | **no**: only another row can be dropped on a row (`ProjectRail.tsx:190`); files go only into the input box |
| Titles | an automatic title, "Regenerate title", custom instructions | `thread-sidebar.md:121-132` | **partly**: Claude Code writes the title on the worker's subscription (SPEC:349-356), Rename and `hqc_rename` exist (SPEC:2439); no regeneration, no instructions |
| Groups by state | | `thread-sidebar.md:134-149` | **yes**: Needs you / Running / Idle (SPEC:1833-1839) |
| Grouping by project | | `thread-sidebar.md:151-161` | **yes**: the project bar and the session column (SPEC:1756-1831) |
| Settle: clear finished work, by itself after 7 days or after a merge | | `thread-sidebar.md:163-211` | **partly**: manual archive (SPEC:1843-1853); no auto-archive — `auto.?archive\|settle`, relevant hits — 0 |
| Snooze until a time or until a limit reset | | `thread-sidebar.md:265-270, 325-342` | **no**: `snooze` — 0 |
| Search across sessions and message text | | `thread-sidebar.md:224-235` | **yes**: a search page with filters over all sessions (SPEC §15, 4542-4634); ⌘K finds only projects (SPEC:1789-1797), no command palette |
| Search inside the open session (Cmd+F) | | `keybindings.md:148-154` | **partly**: search shows "N more in this session"; no find bar in the chat |
| A link to a session ("Copy thread") | | `thread-sidebar.md:237-245` | **partly**: a session has the address `#/s/<id>` and "Open in new tab" (SPEC:1816); no "copy link" button |
| "Limited" and "Resume at reset" | the session continues by itself after the limit resets | `thread-sidebar.md:249-270` | **no**: limits are shown (SPEC:1634-1636), the session does not continue by itself — `resume (at\|after)\|auto.?resume` — 0 |
| Subagents | | `thread-sidebar.md:272-281` | **yes**: start and end cards with state (SPEC:1923-1961), a live list above the input (SPEC:2016-2030); no feed of a subagent and no subagent name on its permission request (a separate PR is in preparation) |
| Expand a tool call | | `activity-log.md:3-6` | **yes** (SPEC:1919-1923) |
| A session's local servers with ports and Stop | | `thread-sidebar.md:287-293` | **no** |
| Side chat (`/side`, `/btw`) | a question about the session without touching its turn | `thread-sidebar.md:300-323` | **no**: `side chat\|btw` — 0 |
| Hide tool calls, Zen mode | | `activity-log.md:21-31` | **partly**: calls are folded into groups, Compact and Spacious looks (SPEC:1705-1715) |
| A minimap of the feed by turns | | `apps/web/src/components/chat/timelineMinimapItems.ts:17-34` | **no**: `minimap\|outline\|jump to`, relevant hits — 0 |
| Fork | | `apps/web/src/components/chat/TimelineAssistantRows.tsx:182` | **yes**: "Fork from here" (SPEC:1521-1550) |
| Import of Claude Code history | | `welcome-page.md:65-114` | **yes, wider**: import, a live mirror of VS Code sessions and takeover (SPEC §9) |
| Switching model and provider mid-session [multi] | | `README.md:33`; `portable-handoffs.md` | **partly**: the Claude model can be changed (SPEC:380, `client/src/app.ts:262`); adapters — only `claude_code.rs` and `fake.rs` |
| Screen-reader announcements, state in the window title | | `activity-log.md:33-50` | **partly**: only a dot on the tab icon (SPEC:1715-1717) |

## Permissions and secrets

| Feature | Where in T3 | hqc |
|---|---|---|
| Modes Supervised / Auto-accept edits / Auto / Full access | `permission-modes.md:13-18` | **partly**: Ask / Auto / Allow all (SPEC:2038-2055), no Auto-accept edits |
| Admin policy: a ceiling on the mode, providers | `managed-policy.md` | **partly**: deny rules and `allow_all` in `worker.toml` (SPEC §14, 4429-4455) |
| The agent asks for a secret with a private card; 1Password | `secrets.md:1-8` | **no**: `secret request\|1password\|password manager` — 0; secrets live in `env` and ssh keys of the worker's environment (SPEC:4438-4455) |

## Git and pull requests

| Feature | Where in T3 | hqc |
|---|---|---|
| A worktree per session with one button | `thread-sidebar.md:3-13` | **partly**: a worktree is created by hand in the project settings (SPEC §13.1, 4290-4311), `hqc_spawn` with `isolation: worktree` makes a temporary one (SPEC:264); the new-session window has no such option |
| Diff of a turn, of the branch against its base, per commit; search in the diff | `source-control.md:6-21`; `keybindings.md:63-70` | **partly**: a diff of each edit in the feed (SPEC:3872), diff attachments (SPEC §12); no combined diff |
| Commit, push and PR with one button and a ready text | `source-control.md:178-191`; t3.codes | **no**: `pull request\|git push\|commit message` — only deny rules (SPEC:4394) |
| A PR page: review, merge, auto-merge, stacks | `source-control.md:193-245, 353-364` | **no** |
| Review guide: the agent walks you through a PR | `source-control.md:247-279` | **no**: `review guide\|code review` — 0 |
| "Viewed" marks on files | `source-control.md:281-293` | **no** |
| `watch_pull_request`: the agent wakes up when CI fails or a comment arrives | `source-control.md:328-348` | **no**: `babysit\|CI (fail\|check)` — 0 |
| Cloning and publishing a repository | `source-control.md:146-176` | **partly**: a workspace of kind clone (SPEC §13.1); no publishing |
| Auto-pull of the main branch | `project-settings.md:279-287` | **no** |
| Worktree branch names, auto-cleanup | `project-settings.md:67-79, 220-265` | **partly**: a default path (`client/src/ui/ProjectSettings.tsx:54-56`), deleting what hqc created (SPEC §13.2); no cleanup by age |
| GitHub, GitLab, Gitea/Forgejo, Bitbucket, Azure | `source-control.md:3-4` | **no** |

## Terminal, browser, files, devices

| Feature | Where in T3 | hqc |
|---|---|---|
| A built-in terminal, the phone included | `docs/user/terminal.md`; `apps/server/src/terminal/NodePtyAdapter.ts`; `apps/mobile/modules/t3-terminal` | **no**: `pty\|xterm\|terminal panel` — only about "claude in a terminal" (SPEC:364) |
| Project scripts (`t3.json`) | `project-settings.md:182-186` | **no**: `run script\|project action` — 0 |
| A browser in a pane; tabs live on the remote machine, the agent drives them | `remote-access.md:270-346` | **no** |
| Open in VS Code or JetBrains [desktop] | `apps/web/src/components/chat/OpenInPicker.tsx` | **no** |
| File tree, Cmd+P, code search, file editing | `packages/shared/src/keybindings.ts:87-88`; `apps/web/src/components/files/FileExplorerPane.tsx` | **no**: a folder tree only for picking a workspace (SPEC:1027) |
| Visual answers: the agent makes an HTML page | `html-renders.md:1-11` | **partly**: HTML sent with `hqc_attach` is shown in a sandboxed iframe when `HQC_ARTIFACT_PUBLIC_BASE` is set (SPEC §12, `attachments.tsx:346`) |
| MCP Apps — interactive apps in the feed | `html-renders.md:13-19` | **no**: `mcp app\|ui://` — 0 |
| Math and Mermaid | `appearance.md:32-42`; `apps/web/src/components/chat/MermaidDiagram.tsx` | **partly**: math yes (Temml, `client/src/ui/Markdown.tsx:7-8`), no Mermaid — `mermaid\|diagram` — 0 |
| iOS and Android simulators | `devices.md` | **no** |
| SnapShots, cookie import, T3 as the default browser [desktop] | `snap-shot.md`; `browser-import.md`; `default-browser.md` | **no** |

## Machines and access

| Feature | Where in T3 | hqc |
|---|---|---|
| One client for all machines | `README.md:34-35` | **yes**, built differently: a hub and workers (SPEC §2, 43-57) |
| Reaching a machine without port forwarding (T3 Connect through a Cloudflare Tunnel, free for up to 3 machines) | `remote-access.md:6-39, 79-89`; `faq.md:72` | **yes**, built differently: the worker connects to the hub by itself, the hub sits behind Cloudflare Tunnel and Access (SPEC:53-57) |
| Approving a new device with a code | `remote-access.md:41-64` | **partly**: a worker connects with a code (SPEC:1637-1643), a browser signs in through Access or OIDC |
| QR pairing over LAN, Tailscale, several routes | `remote-access.md:91-227` | not needed with a hub |
| Picking a machine for a new session by load | `remote-access.md:176-192` | **no**: `balanc\|least loaded`, relevant hits — 0 |
| The desktop app installs the server over SSH | `remote-access.md:239-268` | **no**: a worker is installed by RUNBOOK §2 |
| Linked machines, `delegate_task` | `remote-access.md:354-398` | **yes, wider**: `hqc_send`, `hqc_ask`, `hqc_spawn` between any workers (SPEC:2232-2314) |
| Client permissions by scope | `remote-access.md:400-455` | **no**: grants are deferred to iteration 6 (SPEC:4891-4892) |
| A background service | `background-service.md` | **yes**: launchd and systemd (RUNBOOK §1.3, 1.4, 2.4) |
| Update everything with one button | `updating.md:7-38` | **no**: `update everything\|upgrade worker` — 0 |

## Agents and automation

| Feature | Where in T3 | hqc |
|---|---|---|
| MCP tools for the agent: `t3_thread_launch/send/wait/read/search`, the queue, projects, worktrees | `apps/server/src/mcp/toolkits/` | **yes**: `hqc_attach/send/ask/spawn/list_sessions/search_history/schedule/move/rename` (SPEC:2213-2456) |
| Outside agents over MCP with OAuth (Claude Code in a terminal, ChatGPT) | `outside-agents.md` | **no**: MCP is available only to the agent inside a session, through the worker's socket (SPEC:2180-2186) |
| A command line `t3 thread start/send --wait` | `outside-agents.md:138-158` | **no** |
| Schedules | `project-settings.md:81-105` | **partly**: per session (SPEC:1689-1699, §16) and through `hqc_schedule`; no "a new session on every run" (template §13.3 is in the backlog) |
| Runs started by a webhook | `project-settings.md:107-173` | **no**: `webhook` — 0 |
| A guest link to a session | `guest-links.md` | **no**: `guest\|share link` — 0 |
| A link with a ready prompt | `prompt-links.md` | **no**: `prompt link\|?prompt=` — 0 |
| The agent changes the app theme; Triage | `appearance.md:74-80`; `triage.md` | **no** |

## Models and providers

| Feature | Where in T3 | hqc |
|---|---|---|
| 11 agents and any ACP agent [multi] | `README.md:28` | **partly**: only Claude Code; ACP, a bare model and local models are described in SPEC §8.2–8.5 as iteration 5, no code |
| Several Claude accounts | `providers-claude.md:7-49` | **partly**: the account is the worker's account (`crates/hqc-worker/src/adapter.rs:73-74`); environments have `env` (SPEC:4438-4443), whether `CLAUDE_CONFIG_DIR` can be set there — not checked |
| Auto-compaction threshold, compaction before the cache expires, a "Compact" chip | `providers-claude.md:51-82` | **no** |
| Claude's task list (TodoWrite) as a separate line | `providers-claude.md:105-110` | **no**: `TodoWrite\|todo` in client — 0 |
| The `--chrome` flag, routers such as OpenRouter | `providers-claude.md:112-158` | not checked |
| A custom model | `composer.md:100-119` | **yes**: the name is typed into the model chip (SPEC:1896-1897) |
| Reasoning effort, Fast | `keybindings.md:33-41` | **no**: `effort\|thinking budget\|reasoning\|fast_mode` — 0 |
| A fallback model when the limit runs out | `apps/web/src/components/settings/ProjectDefaultsSettings.tsx:254` | **no**: `fallback model\|out of usage` — 0 |
| Plan mode, a plan card, "Plan Ready" in the list | `packages/contracts/src/providerPolicy.ts:45`; `ClaudeAdapterV2.ts:3256`; `Sidebar.logic.ts:1295-1297` | **partly**: no switch, `ExitPlanMode` arrives as a permission request (SPEC:3205); how the plan is drawn — not checked |
| Cheap text models for titles and commits | `text-generation.md` | **no** |
| Install an agent and sign in to it from the UI | `welcome-page.md:23-52` | **no** |

T3 drives Claude through the Agent SDK (`apps/server/package.json:39`, `faq.md:36`), not through
the CLI as hqc does. T3 does not support local models (`faq.md:66-68`).

## Spending, notifications, phone, looks

| Feature | Where in T3 | hqc |
|---|---|---|
| A Usage page: tokens, cache, cost by model and machine; custom prices | `usage.md:9-85` | **partly**: a turn's spending and the session total (SPEC:513); no summaries, no custom prices |
| Subscription limits across all accounts; "Use reset" | `usage.md:111-139` | **partly**: the worker's 5-hour and weekly limits in the "More" menu (SPEC:1634-1636, `client/src/ui/limits.tsx:43`) |
| Push to a phone | `mobile-notifications.md:3, 15` (only through T3 Connect) | **yes**: Web Push (SPEC §16, 4637-4655); on an iPhone only from the app added to the Home Screen |
| Desktop notifications, sound | `NotificationSettings.tsx:26` (only while T3 is open) | **partly**: Web Push arrives without an open tab too (`client/README.md:30-31`); no sound — `sound` — 0 |
| Live Activity, widgets [mobile] | `mobile-notifications.md:5-9`; `usage.md:187-192`; `apps/mobile/src/widgets/` | **no** |
| Native iOS and Android apps [mobile] | `README.md:13-14`; `apps/mobile/` | **partly**: a PWA with a phone layout (SPEC:1863-1887) |
| A desktop app [desktop] | `apps/desktop/package.json:25` | **no**; a macOS wrapper is being prepared as a separate PR |
| Themes, light and dark | `appearance.md:3-12` | **yes** (SPEC:1732-1741) |
| Theme editor, VS Code theme import, custom CSS | `appearance.md:51-72` | **no** |
| Project icon, pinning and archiving projects | `project-settings.md:51-65, 267-277` | **yes** (SPEC:1747-1752, 1781-1789) |
| Custom key bindings (`keybindings.json`) | `keybindings.md:93-167` | **partly**: ⌘K, ⌘\, Enter and Esc in a permission card (SPEC:1790, 1857); cannot be changed — `keybinding\|rebind` — 0 |
| F6 between regions, Cmd+1..9, iPad keyboard | `keyboard-focus.md`; `keybindings.md:72-91` | **partly**: only ↑/↓ in lists |
| Settings inherited "project → machine" | `project-settings.md:1-49` | **no**: the client has only the project's look (SPEC:1747-1754) |
| Keep the machine awake while agents work [desktop] | `SettingsPanels.tsx:2236` | **no** |
| Continue interrupted sessions after a restart | `SettingsPanels.tsx:2195` | **partly**: input is not lost (SPEC:780-782); an interrupted turn is marked `turn_lost` (SPEC:522) and does not continue by itself |

## Not checked

- 12 files of T3's `docs/user/` were not opened (guides to other agents, 1Password, T3 Connect
  troubleshooting, licences); 7 were read by headings.
- How hqc draws a plan from `ExitPlanMode`; whether `CLAUDE_CONFIG_DIR` can be set through an
  environment's `env`; the `--chrome` flag and model routers.
- T3 was not run: everything about it comes from its docs and code.
