# hqc against the Claude app for macOS (the Code tab): what we handle wrong

A comparison of what the Claude app for macOS does with Claude Code in its Code tab — how it
starts the CLI, which control frames it sends and which it answers, how a session lives and how the
interface shows it — with what hqc does: worker, hub, client. A sequel to
`docs/notes/hqc-vs-claude-code.md` (the comparison with the VS Code extension): what is already
there is not repeated here; where the app does the same thing differently from the VS Code
extension, it gets one line with a pointer to the item. Plus a section "Only in the app" (what the
VS Code extension does not have either) and the protocol difference of CLI 2.1.312 → 2.1.325.
Items the owner declined or deferred carry a mark: "backlog" and "second list" are the first and the
second list of "From T3 Code, Orca and Claude Code" in `docs/BACKLOG.md`; "not doing" and
"undecided" are the owner's decisions in our project's notes.

"§2.3", "§3" are notes of our project, not `docs/SPEC.md` of this repository; "SPEC hqc" and
`docs/SPEC.md` are `docs/SPEC.md` of this repository.

## What was compared

- The app — Claude 5.11492.5 (`/Applications/Claude.app`), build commit `1403e2cf…`
  (`app:xV3fJhc7:81510–81513`). The app was not run — its side is by the code only.
- Live runs (2026-11-18) — on the test bench agent-node-227, model haiku. The CLI — in the probe
  harness `/home/claude/cc-api-probe/` with the worker's flags: the node's 2.1.312 and 2.1.325 of the
  same build as the app's (`downloads.claude.ai/claude-code-releases/2.1.325/linux-x64/claude`, its
  sha256 matched the manifest; the manifest's darwin-arm64 size matches the app's binary). hqc's
  behavior — on the hqc test bench (`/home/claude/hqc-stand/`). What was not run is said at the
  item.
- CLI 2.1.325 — the build the app pins by checksum (`app:xV3fJhc7:81527–81529`) and keeps in
  `~/Library/Application Support/Claude/claude-code/2.1.325/`; CLI 2.1.312 — the one
  `docs/notes/claude-code-api.md` is written against. The VS Code extension 2.1.319.
- hqc — `origin/master` `3616f328` (2026-11-18 15:36 UTC); paths `crates/…`, `client/…`,
  `docs/SPEC.md` are from the root of this repository at that commit. Master has since moved to
  `91d95d9c` (#101 and #119 merged); there some of the cited lines in `engine.rs` (after 2134),
  `crates/hqc-hub/src/events.rs` and `docs/SPEC.md` have moved by one or two. Abbreviations: `engine.rs` — `crates/hqc-worker/src/adapter/claude_code/engine.rs`,
  `claude_code.rs` — `crates/hqc-worker/src/adapter/claude_code.rs`, `supervisor.rs` —
  `crates/hqc-worker/src/supervisor.rs`.
- Open PRs — by the diff of their branches from `origin/master`: #103 "What the agent waits on, and
  what Claude Code tells the user…" (`cli-notices` `30ede38d`), #104 "Subagents and background
  tasks…" (`subagent-tasks` `28a6b6e4`), #105 "The CLI's requests and the results of tool calls…"
  (`requests-and-results` `0c08060b`); #101 "The agents' room" and #119 "An input's ack keeps its
  place after the turn before it" were open at `3616f328` and are merged now. A mark "closed by #N
  (not merged)" is set by the branch's code, not by its title.
- The app's data on the laptop — the format only: 5 files
  `~/Library/Application Support/Claude/claude-code-sessions/<account>/<org>/local_<uuid>.json` and
  `ccd-session-presets.json`; files with tokens were not opened.

## Sources

They are in `vendor-src/` of our project (not in git). The app's archives were unpacked without
`node_modules` (the Agent SDK is bundled into the main process's chunks), the JS was formatted with
prettier 3.3.3, variable names are minified.

| Label | File | What it is |
| --- | --- | --- |
| `app:<tail>:N` | `desktop-5.11492.5/app/.vite/build/index.chunk-<tail>.js`, line N | the main process from `app.asar`: `fch7zeJ_` — the bundled Agent SDK 0.3.325, `D1yJ0UIX` — the Code tab's session manager, `hrMvQ5DX` — permissions and hooks, `xV3fJhc7` — the process environment and storage, `CBoQOmxo` — starting the CLI on an SSH host, `DLJa680u`, `6TZs9uFI`, `C-96QB_l` — continuing a turn, scheduled runs, reasons for a failed start |
| `app:mainView.js:N` | `desktop-5.11492.5/app/.vite/build/mainView.js` | the bridge between the window and the main process (preload, the `LocalSessions_$_*` methods) |
| `ion:<chunk>:N` | `desktop-5.11492.5/ion/assets/v1/<chunk>-<hash>.js` | the web interface from `ion-dist.asar` (the whole claude.ai client, the Code tab is a part of it); chunks — in the table below |
| string key | `desktop-5.11492.5/ion/i18n/en-US.json` | an interface string by its key, for example "Retrying ({attempt}/{maxRetries})" (`JlATz1Qkno`); in the code it stands as `defaultMessage` next to the key. An index of all 58 592 strings with file and line — `desktop-5.11492.5/msgidx.tsv` |
| `cli325:N` | `cli-2.1.325.pretty.js` | the JS from the CLI 2.1.325 binary, extracted the same way as `cli-2.1.312.pretty.js` (modules from `// @bun` to the first zero byte, separator `/* ==== module at byte N of the binary ==== */`); prettier did not parse 4 modules of 3062 |
| `cli:N`, `ext:N`, `web:N` | `cli-2.1.312.pretty.js`, `ext-2.1.319-extension.js`, `ext-2.1.319-webview.js` | as in `docs/notes/claude-code-api.md` |
| `rec:<name>:N` | `/home/claude/cc-api-probe/captures/<name>.jsonl` on agent-node-227, line N | a capture of a live CLI run: next to it `.stderr`, `.times`, `.stdin.jsonl`, `.meta.json` (flags, binary); the scenario — `scen/<name>.json`, the debug log — `dbg/<name>.log`; the name carries the version (`312`, `325`) and the entry point (`vscode`, `desktop`) |

The interface chunks cited most:

| Chunk | What is in it |
| --- | --- |
| `c96d752d0` | the Code tab's feed: cards, the activity line, the message menu, the compaction bar |
| `c06cf64bb` | parsing CLI messages into feed lines |
| `cc43287c9` | the session screen: the rewind, model switch and effort windows |
| `c82492f2c` | the composer: sending, the queue, model, effort, Fast, dictation |
| `c9bd7af9c` | panels: the queue, the plan, background tasks, the side chat |
| `cc32965c2` | the common permission card (chat, Cowork, Code) |
| `c4a9ee384` | API error and retry captions |
| `c70db48e3` | reasons and advice of the API error card |
| `cd19fedd2` | Claude Code settings |
| `cb63ed069` | "What's new" release notes (`7403–8232`) |
| `c2d611398` | the context fill breakdown |
| `ce03dc203` | permission mode names |
| `c4db265df` | side panels |

The comparison of the 2.1.312 and 2.1.325 protocol schemas — scripts and their output in
`vendor-src/scripts/desktop-schema/` (`schema.py`, `schema2.py`, `deep.py`, `lits.py`).

## In short

| List | Items |
| --- | ---: |
| Wrong | 5 |
| Partly | 12 and 1 as a line |
| Not handled | 7 |
| Not used | 1, 8 as lines and 15 in the table |
| Only in the app | 15 |
| Protocol 2.1.312 → 2.1.325 | 14 table rows |

Most visible to the user:

1. **A session that ended its turn with a question in the text stands in Idle.** To the app, the CLI
   sends a summary after each turn by itself — "waiting for an answer / done / blocked"; live, only
   with `CLAUDE_CODE_ENTRYPOINT=claude-desktop`, while the worker starts the CLI as `claude-vscode`.
   Just switching the entry point is not possible: in sessions with a provider the subscription
   token goes to the provider, the agent's Bash gets the login shell's environment, the agent gets
   `SendUserFile` that bypasses the chat ("Can the worker start the CLI as `claude-desktop`").
2. **A `CronCreate` job dies on an unload for idleness** — checked on the hqc test bench: unloaded
   after 30 s, in 4 minutes the job did not fire once; without the unload the same CLI runs it every
   minute.
3. **A subagent cannot be continued in a copy of the session**: the CLI does not put its transcript
   into the copy's folder, `SendMessage` fails with "No transcript found for agent ID" (live on
   2.1.312 and 2.1.325).
4. **A multi-line command in the permission card shows only its first line**, the rest is glued into
   a grey subtitle — you allow without seeing the script.
5. **An API error is the CLI's raw text.** The app names the reason in words ("Service was busy",
   "Context window was full"), says what to do and gives Retry with a choice of another model; in
   hqc you can repeat only by typing again.
6. **A model refusal.** The app tells the CLI it can ask "retry on the fallback model or edit the
   prompt", and the turn goes on without sending again; "Edit and retry" rewinds the conversation to
   the refused message. hqc after "Switch model" asks to "Send the message again", the refused pair
   stays in the conversation.
7. **The queue.** In the app Enter sends "at the next step", ⌘Enter — "after the turn"; waiting
   messages are a panel above the composer: Send now, Edit in composer, Remove, drag to reorder. In
   hqc what is typed during a turn always joins the turn.
8. **A laptop's sleep and a context overflow.** A stream cut mid-answer the CLI in `-p` finishes by
   itself (live); if the sleep came before the first word of the answer or the context overflowed,
   the app writes "…Please continue" to the agent by itself, while in hqc the turn ends and the rest
   is by hand (by the code).
9. **The 2.1.325 protocol is a superset of 2.1.312**: nothing is removed, hqc's parsing works by the
   schema as before; of the new things, four are worth taking ("Protocol 2.1.312 → 2.1.325").

## "The same protocol": what it really is

`docs/notes/claude-code-api.md` said of the app 5.1231.2: it "carries the same Agent SDK and starts
the same CLI with the same stream-json flags". For 5.1231.2 there is nothing to check it with (its
sources are not at hand). For 5.11492.5:

- **Its own SDK**: Agent SDK 0.3.325 (`app:fch7zeJ_:15745`, `15858`); the VS Code extension has
  0.3.319.
- **Its own CLI**: the app downloads and pins 2.1.325 with a checksum, CLI updates are off
  (`DISABLE_AUTOUPDATER=1`, `app:xV3fJhc7:282518–282530`); for Cowork — the Linux build
  `claude-code-vm/2.1.325/claude` (ELF aarch64, `app:xV3fJhc7:211956–211959`, `213194–213235`) with
  `CLAUDE_CODE_ENTRYPOINT=local-agent` (`app:BqZKACTO:17081`).
- **Its own flags and `initialize`**: the Code tab starts as `claude-desktop`
  (`app:xV3fJhc7:283106–283108`), with `--thinking=adaptive`, `--effort`, `--setting-sources
  user,project,local` and managed settings (`app:fch7zeJ_:12166–12276`,
  `app:D1yJ0UIX:25929–26020`, `25333–25356`); in `initialize` — hooks, `supportedDialogKinds`,
  `rapidFollowupPreempt`, `codeRunningMcpTools`. The CLI's behavior depends on the entry point: in
  2.1.325 there are 29 calls of `Vd()` (desktop, desktop-3p, `local-agent`), 13 of `DP()`, 5 of
  `kct.has` (`claude-desktop`, `claude-desktop-3p`, `cli325:37640`), plus `_Se()`, `Hf()`, `vat()`
  and direct comparisons — what of this changes for the worker is in "Can the worker start the CLI
  as `claude-desktop`".
- **The format is the same**: stream-json and the `control_request`/`control_response` envelopes
  are the same, the 2.1.325 schema is a superset of 2.1.312 (not one type, field or value removed),
  the stdout filter passes the same set of types.

## Starting the CLI: the app and the worker

| What | The app (the Code tab) | The hqc worker |
| --- | --- | --- |
| Binary | its own 2.1.325, updates off (`app:xV3fJhc7:282518–282530`) | the machine's `claude`: 2.1.304–2.1.318 on our six workers (our notes, §2.3) |
| Entry point | `claude-desktop` / `claude-desktop-3p` (`app:xV3fJhc7:283106–283108`) | `claude-vscode` or none (`sdk-cli`) (`claude_code.rs:238–240`, `790–792`) |
| Stream | `--output-format stream-json --verbose --input-format stream-json`, `--include-partial-messages`, `--replay-user-messages`, `--permission-prompt-tool stdio` (`app:fch7zeJ_:12166–12212`, `app:D1yJ0UIX:25970`, `25333–25356`) | the same plus `-p`, `--permission-prompts host` (`claude_code.rs:650–664`) |
| Thinking | `--thinking=adaptive` on a condition, `--effort` (`xhigh` in the presets), `--thinking-display summarized` or `omitted` (`app:fch7zeJ_:12173–12189`, `app:D1yJ0UIX:25333–25356`) | only `--thinking-display summarized` |
| Settings | `user, project, local` always (`app:D1yJ0UIX:25969`) | `user,local`; `project` — in an environment with `project_settings` (`claude_code.rs:130–135`, `714–716`; the decision of SPEC hqc §14) |
| Permissions | `canUseTool` of its own broker; when Auto is held back by a policy, it starts in `default` (`app:D1yJ0UIX:25708–25713`) | `--permission-mode`, `--allow-dangerously-skip-permissions` with `allow_all` |
| MCP | `set_mcp_servers` on the fly (8 calls), MCP in its own process through `mcp_message` (`app:fch7zeJ_:13409–13623`) | `--mcp-config` with the hqc MCP, `--strict-mcp-config` without `allow_local_mcp` |
| Resume, copy | `--resume`; rewinding the conversation — `--resume-session-at` + `--fork-session` (`app:D1yJ0UIX:26345–26348`) | `--session-id` / `--resume` / a copy from a temporary file with `--fork-session --resume-session-at` (`claude_code.rs:737–750`) |
| `initialize` | hooks, `supportedDialogKinds`, `perTaskStopAffordance`, `rapidFollowupPreempt`, `promptSuggestions` behind a flag (`app:D1yJ0UIX:25954–26005`) | only `perTaskStopAffordance` (`engine.rs:715–721`) |
| Credentials | the app holds the token, the CLI asks it to refresh: `oauth_token_refresh`, `host_auth_token_refresh` (`app:fch7zeJ_:13542–13577`) | the CLI's own sign-in on the machine |
| Environment | see "Not used", the first item | a list of allowed names (`claude_code.rs:179–192`) |

## Can the worker start the CLI as `claude-desktop`

No, not as is: sessions with a provider break. The run is one scenario of two turns (the first ends
with a question to the owner in the answer's text, the second with a finished file edit) under
`claude-vscode` and `claude-desktop`, on 2.1.312 and 2.1.325: `rec:d1-325-vscode`,
`rec:d1-325-desktop`, `rec:d1-312-vscode`, `rec:d1-312-desktop`; the comparison — `cmp.py`,
`turns.py` in the probe harness.

What breaks:
- **A session with a provider** (`ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN`,
  `claude_code.rs:146–171`): under desktop the CLI does not take `ANTHROPIC_AUTH_TOKEN` as a sign-in
  (`nue()`, `cli325:179492–179498`; `vat()` — `179256`), and if the machine has a claude.ai sign-in,
  it builds the client with the subscription's OAuth token and the provider's `baseURL`
  (`cli325:270669–270671`, `271015–271060`) — the subscription token goes to the provider. A
  conclusion from the code, not run live.
- **The Bash environment**: the login shell snapshot appends its variables to the environment of
  the agent's Bash calls (`let ne = DP() && …`, `cli325:674782`), past the worker's environment
  filter. Live: under desktop 35 variables against 33 — the extra `GOPATH` and `GOROOT` from the
  node's `~/.profile` (`rec:d1x-325-desktop:33`, `rec:d1x-325-vscode:32`; only names were printed).
- **Extra tools**: `init.tools` under desktop has 38 against 31 — `SendUserFile`, `ListPlugins`,
  `ListSkills`, `SearchPlugins`, `SearchSkills`, `SuggestPluginInstall`, `SuggestSkills`, and the
  plugin `cc-plugin-easel` (the same on 2.1.312). On "send me the file" the model called
  `SendUserFile` by itself, the answer was "1 file delivered to user.", nothing else in the stream
  (`rec:d1f-325-desktop:104`, `108`): the file will not reach the hqc chat; it competes with
  `hqc_attach`.
- **`session_writes_lost`** is not sent under desktop (`cli325:1462659`; live — "Not handled").
- The CLI's built-in Artifacts MCP server is not connected under desktop (`skip: "desktop_host"`,
  `cli325:1202704–1202705`) — without `allow_local_mcp` it is not there anyway.

What it gives:
- **The turn summary** `system/post_turn_summary` — only under desktop, on both versions, one line
  before the turn's `result`: a turn with a question — `{"status_category":"blocked","status_detail":
  "greeting.txt content: formal or casual; awaiting choice","needs_action":"Reply A (\"Good day.\")
  or B (\"Hi there!\") for greeting.txt content"}` (`rec:d1-325-desktop:27`), finished work —
  `completed`, `needs_action: ""` (`rec:d1-325-desktop:110`; 2.1.312 — `rec:d1-312-desktop:28`,
  `92`). The price: by the code the summary is a heuristic without the model by default
  (`cli325:1043906–1043909`, `1774859–1774861`), but for our account the server turned on the model
  mode (`tengu_classifier_summary_llm_emit = true` in the flag cache of the node's `~/.claude.json`)
  — one `source=side_query` request per turn (`dbg/d1dbg-325-desktop.log`, lines 195 and 242); it
  goes into neither `modelUsage` nor `total_cost_usd`; `result` comes 1.7–2.7 s later (under vscode —
  30–61 ms after the last answer).
- **The "what the agent is doing" line** `system/task_summary` as the work goes — only under
  desktop: `{"detail":"Writing greeting.txt"}` right after `Write`, `{"detail":null}` after `result`
  (`rec:d1-325-desktop:96`, `113`; "Mid-turn progress line from the debounced classifier",
  `cli325:130476–130487`); it makes no separate model request.
- The "# Surface" text of the system prompt is about a chat in the app, not about VS Code
  (`cli325:294421`, `294423`, the choice — `636388`; only with the model's `leanRewrite`, `636329`).
- `instruction_size_warning` about a big CLAUDE.md — only under desktop and with the server flag
  `tengu_jolly_aurora` (`cli325:1432660–1432697`; the schema's description says "Sent to Claude
  Desktop's Code tab alone", `130677`). For our account the flag is off: a CLAUDE.md of 290 850
  characters, with a limit of 150–200 thousand, gave no frame under desktop or under vscode
  (`rec:p4-325-desktop-upl`, `rec:p4-325-vscode-upl`). With the worker's `--setting-sources
  user,local` the agent does not see the project's CLAUDE.md at all (`rec:p4-325-desktop-ul:53` —
  "NONE"), as SPEC hqc §8.1 says.
- Announcing the current branch's PR at start (`code_change_published action: "started"`,
  `cli325:1493201`, `1493631`) — by the code only under desktop; live it did not appear
  (`rec:prs-325-desktop`: it needs a `gh` token and a request to the GitHub REST API,
  `cli325:456763–456800`).

What the code promises and the run did not show: a slash command during a turn runs at once only
under `claude-vscode` (`cli325:1462783–1462789`) — live, `/effort low` during a turn waited for the
end of the turn under both entry points (`rec:d1z-325-vscode:27`, `41`; `rec:d1z-325-desktop:28`,
`43`).

The host's answers (`oauth_token_refresh`, `host_auth_token_refresh`) are not needed under desktop:
the CLI asks for them only with `CLAUDE_CODE_SDK_HAS_*`, which the worker does not set
(`cli325:179655–179660`, `1485772–1485779`). The turn summary cannot be had without desktop: only
the desktop, cloud session and bridge surfaces send it (`j2a`, `cli325:1043832–1043875`; "cli" is
off — `V2a()` returns `false`).

## Wrong — we show the wrong thing or in the wrong place

**The agent's cron job dies on an unload for idleness.**
- Seen: the agent made a `CronCreate` ("check the build every 10 minutes"), above the composer there
  is a "Cron:" line with a countdown; after 30 minutes without turns the worker unloads the process —
  the line goes away, the job does not fire until the next message brings the session up (then the
  CLI brings the job up from the transcript).
- hqc: `idle_deadline` looks at the turn, the queue, subagents, background tasks and the `wake_at`
  wakeup timer, but not at cron jobs (`supervisor.rs:1179–1195`); `wake_at` is set only by
  `ScheduleWakeup` (`supervisor.rs:2039–2045`, `engine.rs:100–102`); the worker does know the jobs
  themselves (`engine.rs:374–379`, `with_resumed_cron` — `engine.rs:582–587`).
- Live on the hqc test bench (`idle_timeout = 30`, session `personal/cron-idle-check-2`): the agent
  made `CronCreate '* * * * *'` at 18:39:53 UTC, the turn ended at 18:39:55, at 18:40:25 the worker
  logged "idle timeout, stopping the agent" and took the process down — in the next 4 minutes the job
  did not fire once. Control on the same CLI 2.1.312 without the unload: the job fires every minute
  (`rec:x7-cron-alive`: answers "TICK" at lines 144 and 157, after 68 and 128 s); after the process
  is taken down with TERM and `--resume` of the same session, the CLI brings the job up by itself and
  runs it without any input after 57 s (`rec:x7-cron-resume:43`), although `CronCreate` answers
  "Session-only (not written to disk, dies when Claude exits)".
- Claude Desktop: a watched session has no unload for idleness at all (`app:D1yJ0UIX:3651–3660`);
  the eviction by the process limit does not take a session with a live cron job or wakeup timer
  (`hasLiveCronJobs`, `hasLiveLoopWakeup`, `app:D1yJ0UIX:84536–84558`).
- Fix: in `idle_deadline` take the nearest cron time as `wake_at` (the same formula as the "Cron:"
  line). The owner's decision "Agents are unloaded only for idleness" (our notes, §3) does not change
  this.

**A subagent cannot be continued in a copy of the session.**
- Seen: in a copy made after a subagent's work, the agent asked to continue the same subagent gets
  "No transcript found for agent ID" (and after #104 is merged, the subagent's history on a click
  will not open in the copy either — the file is not there).
- hqc: `fork()` copies only the main transcript up to the cut (`claude_code.rs:809–848`); #104 reads
  the history from `<CLI session id>/subagents/agent-<task_id>.jsonl`.
- Live (2.1.325 and 2.1.312): the CLI with `--fork-session` does not put subagent transcripts into
  the copy's folder — neither with `--resume-session-at`, nor without it, nor from a temporary file
  as the worker does. The copy remembers the subagent's result (it is in the main transcript), but
  `SendMessage` to the same subagent fails: `{"success":false, "message":"Agent \"…\" could not be
  resumed: No transcript found for agent ID: …"}` (`rec:p3-f3-325:156` — the worker's way,
  `rec:p3-f1-325:130`, `rec:p3-f2-325:190`, `rec:p3-f1-312:192`, `rec:p3-f3-312:170`). Without a copy
  it works (`rec:p3-ctl-325:83`); a copy with the files put into `<project>/<new id>/subagents/`
  beforehand works too (`rec:p3-f4-325:83`).
- Claude Desktop: on a copy it finds the subagents the conversation refers to and copies their
  transcripts into the new session's folder (`app:D1yJ0UIX:28402–28425`, the call —
  `30066–30071`).
- Fix: in `fork()`, before the start, copy `subagents/agent-*.jsonl` (and `.meta.json`) mentioned
  before the cut into `<project>/<new id>/subagents/`.

**A background task whose work goes on outside the process keeps the session Running (2.1.325).**
- Seen: the session stands in Running and is not unloaded while the background task list has an MCP
  task the server polls, although the process need not live for it.
- hqc: everything except `ambient: true` counts as work (`engine.rs:1640–1646`).
- Claude Code 2.1.325: a new field `off_host` — "True for a task whose work goes on outside this
  process: show the task as running, but a host need not stay up for it" (`cli325:131273–131278`);
  `mcp_task` with polling sets it (`cli325:1002494`, `1002498–1002503`).
- Fix: show a task with `off_host: true` as a line above the composer, but do not count it for the
  unload.

**A multi-line command in the permission card — as its first line.**
- Seen: the agent asks to run a script (`cat > x.py <<'EOF'` and 30 lines, or `cd a && \` with line
  breaks) — the command frame has only the first line, at most 400 characters; the other lines are
  glued with spaces into a grey subtitle after "Bash ·" and the agent's description, without line
  breaks and indentation; a one-line command longer than 400 characters is cut with "…" in the frame
  and repeated whole in the subtitle. Allow can be pressed at once.
- hqc: `requestView` takes `what` from `callLine` (`client/src/ui/permission.tsx:46`), and `callLine`
  for Bash gives `oneLine(lines[0], 400)` (`client/src/ui/feed.ts:426–433`); the rest of the worker's
  description (the CLI's description and the whole command up to 4 KB, `engine.rs:2675–2689`) goes
  into `lines.filter((l) => l !== what).join(' ')` (`permission.tsx:49`) and into the subtitle.
- Claude Desktop: the card has the whole command with bash highlighting, the agent's description
  apart; for Bash with `dangerouslyDisableSandbox: true` Allow is unavailable until the command is
  scrolled to the end, and "Always allow" is off (`ion:cc32965c2:17455–17493`, `22090–22140`).
- Fix: in the frame — the whole command from the call's arguments, in the subtitle — only the CLI's
  description.

**The text of a call's error — twice.**
- Seen: a failed call has the first 300 characters of its output as a red line, and under it the
  same text once more in a block (up to 4 KB).
- hqc: the worker puts one text both into `error_text` (300 characters) and into `result_preview`
  (4 KB) (`engine.rs:1998–2000`), the client draws both (`client/src/ui/cards.tsx:1009–1010`).
- Fix: in an opened call — only `result_preview`; `error_text` — for the collapsed places
  (`cards.tsx:1072`, `client/src/ui/relay.tsx:275`).

## Partly — some cases handled

**An API error — raw text, without a reason, advice or a retry.**
- Seen: on overload, a rate limit, a full context or a network cut, the feed has a service line with
  the CLI's text ("API Error: 529 …") and "Turn ended early: … · API status 529"; the session is
  Idle, there is no retry button, what to do is not said.
- hqc: the CLI's answer — a `line-runtime` line (`client/src/ui/cards.tsx:408–412`); the turn end
  line (`engine.rs:2544–2580`); Retry — only with `state === 'error'` or an undelivered input
  (`client/src/ui/Chat.tsx:259`, `662–665`), and it repeats the agent's start, not the turn
  (`docs/SPEC.md:862–865`).
- Claude Desktop: a `kind: "error"` line with `apiError`, `apiErrorStatus`, `stopReason:
  "model_context_window_exceeded"` (`ion:c06cf64bb:8816–8858`), the `code.api_error` card
  (`ion:c96d752d0:94256–94268`, `96985–97069`); the reason — "Service was busy" (`Xf/4QSBM7N`),
  "Server was temporarily limiting requests", "Context window was full" (`9IF+UH0/HB`); the advice —
  "This chat is too long to continue. Compact it to summarize earlier messages and keep going,
  rewind to an earlier point, or start a new chat." (`j4gDC1/jbD`), "Too many requests right now.
  Try again in a moment.", "Check your internet connection, VPN, or proxy and try again." and about
  60 more (`ion:c70db48e3:812–1213`); a full context gets a compaction from the card
  (`ion:c96d752d0:97055–97062`); a "retry on another model" menu — when a model switch helps the
  error (`overloaded`, `model_not_found`, `model_requires_automode`,
  `model_requires_strict_sandbox`), on overload with a known fallback — "Try again with {model}",
  next to it "Lower effort and retry", "Switch to auto mode" (`ion:c3e34355f:1238–1270`,
  `1349–1370`, `1715–1760`; the categories — `ion:c70db48e3:2063–2071`).
- Fix (client, the data is there): by `api_error_status` and the text — the reason and the advice
  as one line (529, 429, context, network, sign-in); the last such line gets Retry (the same message
  again) and a model menu, as the refusal line has; a full context gets `/compact`.

**A model refusal: the choice is not in the turn itself, the refused message stays in the
conversation.**
- Seen: after a refusal without a fallback model — a red line, "Switch model", then "Switched to …
  Send the message again."; the person sends the text once more — the conversation keeps both the
  refused message and the refusal (whether this gets in the new model's way — not checked).
- hqc: `initialize` without `supportedDialogKinds` (`engine.rs:715–721`); the button and the caption
  — `client/src/ui/cards.tsx:574–657`, `634` (#97).
- Claude Code: without a declared dialog kind — "the classic refusal error" (`cli325:132665`); with
  it the CLI pauses the turn and asks "retry on fallback model or edit prompt" (`cli325:290094`);
  2.1.312 has it too.
- Claude Desktop: `refusal_fallback_prompt` is always declared (`app:D1yJ0UIX:80126–80140`), the
  answer goes to `request_user_dialog` (`app:fch7zeJ_:13520–13541`); the VS Code extension declares
  the same kind (`ext:124032`). The card "{fromModelName}'s safeguards flagged this message. This
  sometimes happens with safe, normal conversations." (`zuPE8IHvRX`) with "Edit and retry"
  (`/dodrm2bvQ`, `ion:c96d752d0:21730–21957`) — `rewindToRefusedPrompt`
  (`ion:cc8941b7b-dLb99Etd.js:1895–1912`, `app:mainView.js:13246`) rewinds the conversation to the
  refused message. The "Switch models when a message is flagged" setting (`ion:cd19fedd2:356–383`) —
  the VS Code extension has it too (`web:153761`).
- Fix: declare `refusal_fallback_prompt` and answer `request_user_dialog` with a card of two buttons
  (`docs/notes/claude-code-api.md` §4.5); "Edit and retry" — a copy of the session from the refused
  message (Fork from here is already there, `cards.tsx:722–733`) with its text in the composer. A
  real refusal was not caused live in hqc.

**The activity line: no turn time, tokens, stage or "Stopping…".**
- Seen: "Agent is working" without time or count; after Stop the line is the same until the turn
  ends; while the process starts or waits for the first byte — "Agent is working" as well.
- hqc: `ActivityLine` — `client/src/ui/cards.tsx:334–397`; Stop — `Chat.tsx:441`, `669`.
- Claude Desktop: the `B5` line (`ion:c96d752d0:109087–109366`): the stage — "Connecting…",
  "Sending…", "Starting session…", "Preparing…", "Waiting for Claude…", "Running tools…"
  (`ion:c96d752d0:108669–108702`); the turn time from the 2nd second (`108628–108642`); "{tokens}
  tokens" (`XyN2qD283R`, `108724–108740`); after Stop — "Stopping…" (`NACfNicI6z`, `109343–109346`);
  "Compacting session…"; "Model overloaded · Retrying (3/10)" (`ion:c4a9ee384:14336–14410`);
  "Thinking…", after 15/30/45/60 s — "Still thinking…", "Thinking more…", "Thinking some more…",
  "Almost done thinking…" (`108601–108668`); "N running tasks" opens the background tasks panel
  (`109037–109086`).
- Retries, compaction and hidden thinking are closed by #103 (not merged): `activityLabel` —
  `retrying`, `compacting`, `thinking` (`client/src/ui/labels.ts:219–228` in the branch). The turn
  time, tokens, stage and "Stopping…" are not in #103.
- Fix: the turn time and "Stopping…" — client only; the turn's tokens — a counter from the worker
  (now it sends only the `tokens.rate` speed).

**Thinking — without its duration.**
- Seen: "Thinking · <the first 60 characters>", how long the model thought is not said.
- hqc: `client/src/ui/cards.tsx:870–884`.
- Claude Desktop: "Thought for {seconds}s" (`Jj0DasG7Mx`, `ion:c96d752d0:109171–109189`); the VS
  Code extension too (`web:228082`, `web:240655`).
- Fix: from the first thinking chunk to the finished block; live thinking is not yet merged
  upstream.

**A model switch — without a warning that the cache will burn.**
- Seen: you switched the model in the middle of a long session — the next message re-reads the whole
  conversation without the cache, not a word about it.
- hqc: a confirmation — only on a provider switch (`client/src/ui/Chat.tsx:451–456`); the model menu —
  `client/src/ui/agentSettings.tsx:43`.
- Claude Desktop: "Switch model?" — "This session is cached for {current}. Switching to {next} means
  Claude re-reads the whole session on your next message, which uses more of your usage."
  (`SiwYAeEDSc`) with "Don't ask again"; the same for effort — "Change effort?"
  (`ion:cc43287c9:62936–63090`). The VS Code extension has nothing like it. It is shown only while
  the cache of the main agent's last real answer is alive, and if that answer was not on the target
  model: the TTL is 1 h if its `usage.cache_creation` has `ephemeral_1h_input_tokens`, otherwise
  5 min (`ion:c058e28e3`, functions `D`, `I`, `F`; the call — `ion:cd07e0b93:3580–3628`); once per
  answer (`ion:cd07e0b93:3803–3812`), after "Don't ask again" — never; the session's length does not
  matter.
- Fix: the same confirmation on a model switch of a running session while the agent's last answer
  is younger than 5 min; the 1 h TTL the worker has to pass on itself — hqc's `usage` has only
  `cache_write` (`client/src/proto.ts:295–305`). Which cache TTL the workers' sessions have — not
  checked.

**Notifications — no choice of what about.**
- Seen: a push comes for every request, question and every final answer of every session; turning
  off, say, "the agent finished" without touching requests is not possible.
- hqc: `crates/hqc-hub/src/push.rs:1–4`, `134–160`.
- Claude Desktop: "Permission requests", "Questions", "Task complete" — "Claude finished and is
  waiting for your input.", a sound, "Draw attention on notifications" (`ion:cd19fedd2:73–133`,
  `308–355`).
- Fix: three switches in the user's settings, the hub checks them in `push::on_event`.

**A Stop the CLI did not answer does nothing (hypothesis).**
- Seen: if the CLI hung and does not answer `interrupt`, Stop silently does not work until the hub's
  watchdog; the app takes the process down after a second and a half.
- hqc: `interrupt` is written, an `ack` goes to the hub at once, the worker does not wait for the
  CLI's answer (`supervisor.rs:1241–1256`, `engine.rs:726–736`).
- Claude Desktop: `Promise.race(interrupt, timer)` (`app:D1yJ0UIX:18854–18860`), the limit —
  `wC()` → 1500 ms (`app:D1yJ0UIX:18219–18221`, `app:xV3fJhc7:56484`), then "query.interrupt() did
  not resolve within 1500ms … falling back to stopSession" (`18937–18944`).
- Fix: wait for the answer to `interrupt` for one and a half to two seconds, otherwise kill and an
  ordinary lost turn. How often the CLI does not answer `interrupt` — not checked (we did not get a
  hung CLI on the test bench).

**An MCP call in the permission card — without all its arguments.**
- Seen: the agent calls an MCP tool with several fields (a recipient and a text) — the card has one
  "main" field or the CLI's description; the other fields are visible only if the CLI sent no
  description, and then as a JSON string of up to 300 characters in the subtitle.
- hqc: `what` — the first of `BRIEF_KEYS` (`client/src/ui/feed.ts:420`, `436`); without the CLI's
  description the worker writes `"{tool} {compact(input)}"` cut at 300 (`engine.rs:2685–2687`,
  `154`); the card shows the arguments whole only for Edit, MultiEdit, Write (`permission.tsx:296`,
  `410–417`).
- Claude Desktop: all parameters as "name — value" lines, Allow — after the lines are scrolled
  (`ion:cc32965c2:16969–16990`, `9463–9475`).
- Fix: under the frame — the call's arguments, as in an opened call line of the feed.

**"Always allow" and "Always in this session" do not say what they allow.**
- Seen: buttons without saying which rule will be written (for example, `Bash(npm test:*)`) and
  where; the tooltip repeats the caption.
- hqc: the worker keeps the rules from `permission_suggestions` to itself (`engine.rs:2258–2272`,
  `2286–2294`), `approval.request` does not carry them; `title={o.label}` (`permission.tsx:446–449`).
- Claude Desktop: for a local session — where it will be saved: "Applies to this session only",
  "Saves to local project settings", "Saves to project settings", "Saves to user settings (all
  projects)"; for an edit — "Switches this session to Accept edits" (`ion:cc32965c2:19067–19122`,
  `21950–21955`). Digits pick the card's button (`23250–23290`); hqc has only Enter and Esc
  (`permission.tsx:304–331`).
- Fix: the worker puts the rules' text and place into the `always`/`session` options; the client —
  into the button's tooltip.

**Bash output: the exit code, "no output", the saved full output.**
- Seen: a failed command — the CLI's text "Exit code 1" as the output's first line, no mark; a
  command without output — "(Bash completed with no output)"; a long output — the CLI's header
  "Output too large (45KB). Full output saved to: /…/tool-results/….txt Preview (first 2KB):" as
  text, and the file itself on the worker cannot be opened from hqc (Full result from #105 is the
  same text the model got); `git diff` in the output — grey text.
- hqc: `client/src/ui/cards.tsx:1008–1016`; the worker reads `tool_use_result` only for the wakeup
  timer and cron (`engine.rs:1976–1993`).
- Claude Desktop: "Exit code N" — a red mark, "No output", "Showing the first {shown} of {total}. The
  full output is saved." with a link to the file (`persistedOutputPath`), "Running / Finished /
  Failed / Stopped in background", "Moved to the background after its time limit of …"
  (`ion:cb6930dae:5565–5590`, `12388–12530`); diff lines in the output are coloured
  (`ion:c38a51fa0`).
- Fix: the client — "Exit code N" and "(… completed with no output)" as a mark, diff lines coloured
  as in `diff` blocks; the worker — the file from `tool_use_result.persistedOutputPath` as an
  attachment (as Full result in #105).

**The Edit/Write diff — without line numbers and counts.**
- Seen: an edit's diff — `old_string` → `new_string` without the file's line numbers, cut at 4 KB of
  arguments; the call and group lines have no "+12 −3".
- hqc: `fileChange`/`editDiff` by `args_preview` (`client/src/ui/feed.ts:583–644`).
- Claude Desktop: the diff from `tool_use_result.structuredPatch` with `oldStart`/`newStart`
  (`ion:c254f7f8c:29–100`), "+N −M" in the line and the group (`ion:c96d752d0:43750–43760`,
  `92097–92105`), "Too many changes to show as a diff…", "Created an empty file", "File unchanged"
  (`ion:cb6930dae:14674–15033`).
- Fix: the worker gives out `structuredPatch` (cut the same way), the client — line numbers and
  "+N −M".

**The agent's question: digits and paging.**
- Seen: in hqc's question card an option is picked only by mouse or touch; several questions — all
  at once.
- hqc: `ClarifyCard` without keys (`client/src/ui/cards.tsx:1146–1312`).
- Claude Desktop: digits 1…N+1 pick an option and "Other", arrows move between options
  (`ion:c9ed5627b:520–600`, `755–780`), "Show previous/next question", "Back" (`1287–1326`); after
  the answer — "Your answer", "Skipped", "Replied in chat", "Not answered"
  (`ion:cb6930dae:653–662`, `1863`). The app lets a question be skipped ("Choose an option or skip
  this question", `ion:c9ed5627b:1330`); hqc since #73 waits for an answer to each — by the owner's
  decision.
- Fix: digits and arrows in `ClarifyCard`.

As one line: as "The title and slash commands — late and without descriptions; Rename does not
reach the CLI" in `docs/notes/hqc-vs-claude-code.md`, but the app also revises the title by itself
as the work goes (`generate_session_title {persist: false}`, `app:D1yJ0UIX:60271–60330`; the name
from a bound PR — `60420–60440`) and gives it to the CLI with `rename_session` (`60402–60416`); hqc
instead tells the agent to call `hqc_rename` (#68, #76).

## Not handled — the data arrives and is lost

**The CLI could not write the transcript (`system/session_writes_lost`, 2.1.325).** Seen: the
worker's disk is full or a quota ran out — the session's history is silently not written, while
turns end as usual; after an unload the agent will not remember it, the copy and the mirror will not
find it. hqc: the subtype goes into an empty branch (`engine.rs:1162`), 0 occurrences in master and
the open PRs; besides, the worker takes the process down with SIGTERM (`claude_code.rs:1249–1262`),
and with it there is no frame. Claude Code: `error_code` `ENOSPC`/`EDQUOT`, `lost`,
`transcript_session_ids` — on exit (`cli325:132211–132230`, `1462656–1462732`); the VS Code
extension hands it to its consumer (`ext:110295`). Live (a 1 MiB tmpfs under the project folder, the
transcript broke off at 49 152 bytes): 2.1.325 under `claude-vscode`, on closing stdin and exiting
with code 0 — a frame after the last `result` with `{"error_code":"ENOSPC", "lost":["transcript"]}`
and a stderr line "Claude Code could not save all of this session's transcript… because the disk is
full (ENOSPC)" (`rec:p5-325-vscode:30`); on SIGTERM — exit code 143, no frame
(`rec:p5term-325-vscode`); under `claude-desktop` — none; 2.1.312 loses silently; both `result`s are
`success`. Fix: a line "Transcript not saved: disk full on <worker>"; for the frame to arrive, on
an unload close stdin and wait for the exit instead of sending SIGTERM.

**A retracted answer with an unparsed call (`user.retracted_message_uuids`, 2.1.325).** Seen: the
model gave a tool call the CLI could not parse; the CLI throws that answer away and asks the model
again — the hqc feed keeps both answers. hqc: the field is not read; #103 retracts only by
`supersedes` and by `retracted_message_uuids` of `model_refusal_fallback`, not of `user`. Claude
Code: "Only on the synthetic message that asks the model again after a response whose tool call
could not be parsed: the uuids of that response's messages… Remove them on receipt"
(`cli325:128597–128600`). Fix: after #103 is merged — the same retraction for this field of `user`.

**"While you were away", saved memory, stopped agents (`away_summary`, `memory_saved`,
`agents_killed`).** The app shows "Recap: {recap}" as a bar with Dismiss, "Saved {saved}" ("#
memory", "# team memory"), "All background agents stopped" (`ion:c06cf64bb:7653–7681`,
`8719–8735`, `ion:c96d752d0:29380–29430`, `100977–101041`). The stdout schema has them
(`cli325:130624–130693`), but the `-p` output filter drops them (`cli325:1432403–1432452`), and they
are made only in the interactive mode: `away_summary` — by a hook of the terminal interface
(`cli325:1967237`, `1967349–1967365`; off in `-p`, `1043979–1043986`), `agents_killed` — by its
component (`1885477`, `1885513`), `memory_saved` — through `appendSystemMessage`, which is set only
in the interactive mode (`977014–977019`, `1951778`). The app takes them from the transcript on disk
(`readTranscript`, `app:D1yJ0UIX:67925–67990`); the main process has none of these subtypes. For
hqc these are only sessions where an interactive `claude` worked — that is, the mirror, and it does
not read `system` lines (the item "Mirror (VS Code sessions): the CLI's service lines…" in
`docs/notes/hqc-vs-claude-code.md`). Fix: the mirror shows them from the transcript.

**A stop over a possible prompt injection (`stop_reason: "prompt_injection_risk"`).** The app puts
"Claude stopped because the page content may contain a prompt injection. Review the page before
continuing." (`ion:c06cf64bb:8950–8958`, `ion:c96d752d0:94299–94301`, `84661`). Of `stop_reason`
hqc looks only at `refusal` (`engine.rs:1390`) — the answer just ends. This is a value of the API
itself: CLI 2.1.325 has no such string (0 occurrences), `stop_reason` in the schema is any string
(`cli325:129874`, `126621`), the CLI passes it on as is in `assistant.message.stop_reason` and
`result.stop_reason` (`cli325:1436337`, `1436468`). Whether the API sends it to Code sessions the
code does not decide; we did not meet it live. Fix: a line with the same meaning on this
`stop_reason`.

**Dynamic workflows (`local_workflow`, `workflow_progress`).** Seen: the agent started a workflow —
the feed has a `Workflow` line with the arguments' JSON, nothing above the composer, the session in
Running; how many phases, which agents work and how much they spent is not visible, stopping it —
only Stop of the whole session. hqc: a `Workflow` call is an ordinary group line
(`client/src/ui/cards.tsx:939–1018`; only `Agent`/`Task` become a subagent card,
`client/src/ui/feed.ts:288–290`); the worker makes lines above the composer only from `local_agent`
and `local_bash` (`engine.rs:1075–1092`); `workflow_progress` and `local_workflow` — 0 occurrences
in master and #103–#105. Claude Desktop: `task_progress.workflow_progress` is merged by
`type:index` (`ion:c62da1b56:959–961`, `1633–1640`), a "Workflow" card with a table of agents by
phase — Agent, Tokens, Time, Pending / Running / Done / Error / Not run, "Up next: {phases}", "View
workflow" (`ion:c96d752d0:59676–59990`, `61563`). Whether the CLI sends `workflow_progress` under
`claude-vscode` — not checked. Fix: a `subagents.live` line for `local_workflow` with Stop (the
protocol already has the kind `workflow` → `Unknown`, `crates/hqc-proto/src/event.rs:948–951`);
phases and agents from `workflow_progress` — in the call's card.

**WebSearch and WebFetch — as raw text.** Seen: a search result is "Web search results for query: …
Links: [{"title":…,"url":…}]" as one block, a WebFetch page is text without markup. hqc: the common
`ToolCallBody`; in #105 addresses in a result become links, a list and Markdown do not. Claude
Desktop: links as a "title — domain" list, the answer as Markdown, "The page had no text to read",
"Redirects to {target}, which Claude Code didn't follow" (`ion:cb6930dae:9000–9215`). Fix:
`Links: [...]` — as a list, a WebFetch result — through the feed's Markdown renderer.

**TodoWrite — JSON instead of a list.** Seen: "Arguments" with a `todos` array and "Result: Todos
have been modified successfully…". hqc: `TodoWrite` and `todos` in `client/src` — 0 occurrences
(master and #103–#105). Claude Desktop: the `todos` kind (TodoWrite, TaskCreate/Update/Get/List/Stop)
— a list of items with their statuses (`ion:cb6930dae:12919–12960`, `15246`). Related: "the pinned
TodoWrite" — backlog. Fix: in the opened line — a list with check marks.

## Not used — what the app can do and we do not

**Environment variables the app sets for every CLI process.**
- Seen: a long model answer (a big `Write`) can fail on the request timeout; MCP servers connect
  slower; the CLI version drifts between workers.
- hqc: the environment is a list of names (`claude_code.rs:179–192`), only
  `CLAUDE_CODE_ENTRYPOINT` and `MCP_TOOL_TIMEOUT` are added (`claude_code.rs:790–796`).
- Claude Desktop (`app:xV3fJhc7:284862–284906`, `app:D1yJ0UIX:19558–19642`):
  - `API_TIMEOUT_MS=900000` (`app:xV3fJhc7:284667`, `284900`); the CLI's default comes from the
    server's settings or is 600000 (`cli325:270679`, `271167–271169`); a start on an SSH host has
    the note "Claude Code's default returns: long turns can time out" (`app:CBoQOmxo:256`).
  - `MCP_SERVER_CONNECTION_BATCH_SIZE=8` (`app:xV3fJhc7:284727–284733`); the default is 3
    (`cli325:2292181`).
  - `DISABLE_AUTOUPDATER=1` and a pinned version. The worker does not need it: in `-p` the CLI does
    not update itself in 2.1.312 or 2.1.325 — the updater lives only in the interactive mode's
    interface and in `claude update` (`cli325:783963`, `1086743`, `1087820–1087900`; `cli:847872`).
    On agent-node-227 2.1.312 has stood since 2026-10-28, 13 versions came out after it, the stderr
    and debug logs of the probes have no trace of an update. The version on a worker changes by hand
    or by an interactive `claude` on the same machine (a hypothesis for the second).
  - `CLAUDE_CODE_EAGER_FLUSH=1` (`app:D1yJ0UIX:19607`): the CLI flushes a message's record to disk
    at once (`cli325:1435839–1435848`); the gain for hqc's copy and mirror is a hypothesis.
  - Only for the CLI on an SSH host: `CLAUDE_CODE_OVERLOADED_RETRY_MAX_DELAY_MS=64000`,
    `CLAUDE_CODE_STREAM_EVENT_HOLD_MS=100` (`app:D1yJ0UIX:25357–25362`, `23950–23955`, the
    condition — `26554`).
- Fix: `API_TIMEOUT_MS` and `MCP_SERVER_CONNECTION_BATCH_SIZE` — into the process environment.

As one line (the "Not used" items in `docs/notes/hqc-vs-claude-code.md`):
- "Continuing a turn after a restart": the app does not set `CLAUDE_CODE_RESUME_INTERRUPTED_TURN`
  (on an SSH host — `"off"`, `app:CBoQOmxo:195`), but continues with its own message "The app was
  quit while you were working. Please continue from where you left off." (`app:DLJa680u:173–178`,
  `app:D1yJ0UIX:86425–86486`).
- "`--max-thinking-tokens`, `set_max_thinking_tokens`, `effort`": the app sends `--effort`
  (`xhigh` in `ccd-session-presets.json` and in all 5 `local_*.json`), `--thinking=adaptive` and
  changes the budget on the fly (`app:D1yJ0UIX:15206–15246`).
- "`priority: "now"` with `origin`": every message has `priority`, `origin` and `client_platform:
  "desktop_app"` (`app:D1yJ0UIX:38940–38952`).
- "`rewind_files`": file checkpoints are always on (`enableFileCheckpointing`,
  `app:D1yJ0UIX:25966`), the rollback with `dryRun` (`12739–12749`).
- "`get_task_output`": the tail of a background task's output (`app:D1yJ0UIX:11953–12000`).
- "`get_context_usage`" (3 calls) and "`mcp_status`, `mcp_reconnect`, `mcp_toggle`" (11, 4, 1); MCP
  sign-in by OAuth — `mcp_authenticate` (7), `mcp_submit_oauth_callback_url` (7), `mcp_clear_auth`
  (5).
- "`promptSuggestions`": only behind a flag (`app:D1yJ0UIX:26005`), `prompt_suggestion` is shown
  (`16343–16354`).
- "`CLAUDE_CODE_STARTUP_FAILURE_RESULTS=1`": the app — behind a flag (`app:C-96QB_l:3792`), the VS
  Code extension — always (`ext:139565`).

Interface features:

| What | What it would give an hqc user | Claude Desktop |
| --- | --- | --- |
| **Accept edits** and **Don't ask** modes | file edits without cards, commands — with a card and without the Auto classifier; Don't ask — everything not allowed by the rules is denied without a question. hqc has `ask`, `auto`, `allow_all` (`client/src/proto.ts:62`) | "Manual, Accept edits, Plan, Auto, Don't ask, Bypass permissions" (`ion:ce03dc203:2–9`); "Always allow" on an edit switches the session to Accept edits (`TBXLauXMwh`, `ion:cc32965c2:19669–19684`, `21950–21955`); the VS Code extension too (`web:153337`, `web:167917`) |
| **Output style** per session | "short", "detailed" and own styles without editing the prompt; hqc has 0 occurrences of `output_style` | a setting (`ion:cd19fedd2:385–412`), the session menu and an own style (`ion:cc43287c9:63145–63184`, `81899–81982`); the VS Code extension too (`web:252789`) |
| Context fill | as "`get_context_usage`" in `docs/notes/hqc-vs-claude-code.md` (backlog), but with a breakdown: "Context window {used} / {max} ({pct}%)", "MCP tools", "Memory files", "Custom agents", "Last updated {time}. Refreshes when this turn ends." | `ion:c2d611398:321–2097`, `app:mainView.js:12960` |
| Effort and Fast | as "a thinking budget … `effort`" (second list), but as a slider next to the model, levels `low, medium, high, xhigh, max` by model | `ion:cb63ed069:7496`, `ion:c325ea79a-BydY1bcv.js:12091`, "Fast mode on/off" `ion:c82492f2c:31164–31168`, `app:mainView.js:12880`, `12889` |
| Rewind | as "`rewind_files`" (not doing — the file rollback), but of the conversation too: "Rewind to this message?" with "Code and conversation / Conversation only / Code only", "Changes made through shell commands aren't reverted. Commits stay in place."; "Undo rewind" / "Redo rewind"; "Resume the session from before it was cleared". "Conversation only" is the same as Fork from here, but in place; for the owner to decide | `ion:cc43287c9:64152–64367`, `64586–64639`, `ion:c96d752d0:18769–18908`, `110347–110397`, `app:mainView.js:13236–13278` |
| The background tasks panel | as "`get_task_output`" (second list), but as a panel: Running / Finished / Stopped / Failed, live output, "Stops in {duration}", "View transcript", "Stop this task". hqc has lines above the composer with Stop (`client/src/ui/LiveSubagents.tsx:66–101`); a subagent's history — #104 (not merged) | `ion:c9bd7af9c:24632–25712`, `ion:c96d752d0:109037–109086`, `app:mainView.js:13067–13071` |
| The `/btw` side chat | as "a side question" (backlog); a floating window ⌘;, "Ask a quick question" | `ion:cb63ed069:7978`, `7427`, `ion:c9bd7af9c:23934–23986`, `app:mainView.js:13027` |
| The plan panel | plan mode — not doing; the plan in its own panel with comments on a selection ("Select any text to leave a comment for Claude"), TodoWrite in the same place (the pinned TodoWrite — backlog); "Accept / Accept and allow edits / Accept and use auto mode / Accept and bypass permissions" | `ion:c9bd7af9c:2015–2097`, `2231`, `2442–2444`, `23059–23124`, `ion:cc32965c2:5528–5575` |
| Links in one's own messages | an address or `#123` pasted by the person opens on a click; in hqc the person's text is plain text (`client/src/ui/cards.tsx:769–771`) | "`#123` and bare URLs in your messages become clickable links" (`ion:cb63ed069:8100`); parsing `owner/repo#123`, `#123`, paths and addresses into links is there (`ion:cae93ec19:188–280`, `ion:c18fe02d2:1712`), but that the person's bubble turns it on was not found in the code |
| MCP server widgets (MCP Apps) | an MCP server's tool with its own interface would show it, not text; hqc has 0 occurrences of `ui://`, `resourceUri`; for the owner to decide — this is an iframe with a host protocol | a widget in a sandbox (`ion:c96d752d0:48387–48420`), "Widget from {server}", "Couldn't load this widget, so its result is shown as text." (`48016`, `50306–50566`) |
| Attachments: up to 20 images, downscaling, video | hqc has no limit on the number, an image up to 5 MB goes to the agent as a block, a bigger one only as a path, without downscaling (`crates/hqc-worker/src/adapter/claude_code.rs:1114–1148`); `<video>` in the client — 0 | 20 images for a local session (`ion:c98ab6e79:4–43`), PNG/JPEG/GIF/WebP up to 30 MB with downscaling to 2000 px; a video in a message — a player (`ion:c06cf64bb:6000–6030`, `ion:c96d752d0:90125–90145`) |
| Own slash commands | `/btw`, `/rename`, `/export`, `/diff` and others are run by the interface itself, before the CLI; hqc has the CLI's command list (`client/src/ui/Chat.tsx:1185`, `1278`); `/btw` — backlog, `/export` — not doing | `ion:cc43287c9:72426–72600` |
| Windows and split view | "Open in new window", "Split view", "Open session in split view" | `ion:c4b3b7b1e:22–44`, `ion:c518a31cc:2151–2166` |
| `@` mentions | a file through `@` — not doing; the app also has agents from `.claude/agents`, sessions (up to 16 in a message), projects, people | `ion:c82492f2c:15404`, `16963–16985`, `19542–19598` |
| Dictation | voice input (undecided) | a Dictate button (`ion:c82492f2c:32025–32034`), "Hold to record" |

## Only in the app — the VS Code extension does not have it

For the interface items, the app's caption was searched for in `ext-2.1.319-webview.js` — 0
matches.

**The session's state from the CLI: "waiting for an answer", "ready for review", "blocked".**
- Seen: in hqc a session that ended its turn with a question in the text ("which option do we
  take?") stands in Idle together with finished ones; in the app it is marked "needs input".
- hqc: the "waiting" group is only `waiting` and `error` (`client/src/ui/sessions.ts:80–86`).
- Claude Code: the turn summary `post_turn_summary` (`status_category` blocked / completed /
  review_ready, `needs_action`, `status_detail`) the CLI sends only with the entry point
  `claude-desktop`/`-3p` — live on 2.1.312 and 2.1.325, one line before `result`: a turn with a
  question — `blocked` with `needs_action`, finished work — `completed`; under `claude-vscode` it is
  not there; the price and excerpts — in "Can the worker start the CLI as `claude-desktop`".
- Claude Desktop: keeps the summary in the session (`app:D1yJ0UIX:16355–16383`), `needs_input` —
  from `blocked` (`13199–13224`); `local_*.json` has `postTurnSummary {status_category,
  status_detail, needs_action, summarizes_uuid}`, `completed` in 4 of 5 files.
- Fix: just switching the entry point is not possible — sessions with a provider break, Bash gets
  the login shell's environment, the agent gets `SendUserFile` (the section above). If these three
  places are closed — the summary into `session.state`, `blocked`/`needs_action` into the "waiting"
  group.

**A context overflow mid-turn — compaction and continuing by itself.**
- Seen: in hqc the turn ends with "Turn ended early: …" (`engine.rs:2551–2587`), then by hand —
  `/compact` and "go on"; the app compacts by itself and writes to the agent "The context window
  filled up while you were working, so the conversation was compacted. Please continue from where
  you left off."
- Claude Desktop: `app:D1yJ0UIX:71440–71600` (the text — `71586`), the reason — by the error's text
  (`prompt_too_long`, `622–636`); only local non-archived sessions (`72315–72330`); it does not go on
  if the session is busy or unwatched (`71571–71580`); on by a flag that defaults to "yes"
  (`app:xV3fJhc7:202573–202575`).
- Fix: the worker, at the end of a turn with an overflow — `/compact`, then an input "the context
  overflowed, go on"; one attempt per turn. The overflow was not caused live: whether the CLI goes
  on by itself in this case, as after a stream cut — not checked.

**A laptop's sleep and quitting the app mid-turn.**
- Seen: the laptop lid was closed while the agent waited for the model's answer. If the answer had
  already begun, the CLI finishes the turn by itself; if the sleep came before the first word — by
  the code the turn ends with the error "API Error: Your computer went to sleep before a response was
  produced. Try again." (`cli325:662277`), and in hqc the rest is by hand. The app in this case
  writes to the agent by itself "My computer went to sleep while you were working. Please continue
  from where you left off." and shifts the wakeup timers' times by the sleep's length.
- How the CLI learns about the sleep: the stream watchdog (180 s without bytes or from 300 s of
  idleness, `cli325:271155–271194`) on firing compares `performance.now()` and `Date.now()`: "slept"
  if less than half the limit passed on the monotonic clock while the wall clock moved ahead by more
  than half (`cli325:271321–271329`) — that is, a sleep of at least ~90 s during the wait. Into the
  stream — `assistant` with `error: "server_error"`, `api_error: "computer_slept"`
  (`cli325:938895`, `939419`).
- Live: a stream cut mid-answer (SIGSTOP for 30 s with a 10 s watchdog) the CLI in `-p` repairs by
  itself — it sends itself "Your response above was cut off mid-stream. Resume directly from where
  it stops…" and ends the turn with `result success` (`rec:p7-stop-325:845`, `2653`; 2.1.312 —
  `rec:p7-stop-312:817`, `1457`); by the code `computer_slept` mid-answer takes the same branch
  (`cli325:662196–662290`, the flag `tengu_truncated_response_recovery`, on by default,
  `989435–989443`). The sleep itself cannot be caused on the test bench: SIGSTOP shifts both counters,
  faking the monotonic clock (`LD_PRELOAD`) shifts Bun's timers too. On Linux workers it apparently
  does not happen (a hypothesis: the CLI process reads only `CLOCK_MONOTONIC`, and it stands still
  during a sleep for the timers too); on the laptop and vvzvladmbpm1 — only with a real sleep, not
  checked.
- hqc: the worker knows nothing of the sleep; it does not read the answer's error kind
  (`computer_slept`, `api_error` — 0 occurrences in `crates/` and `client/src/`).
- Claude Desktop: `powerMonitor` suspend/resume (`app:xV3fJhc7:53118–53126`,
  `app:D1yJ0UIX:86034–86110`), `runSleepAutoResume` — it does not go on in a crash loop or with a full
  context (`86383–86424`, the text — `86409`), shifting `scheduledFor` (`86072–86090`).
- Claude Code 2.1.325: a new error kind `computer_slept` — "a Claude Code timer found that the
  machine slept through its wait for the response" (`cli325:129155–129197`).
- Fix: the worker, on a turn that ended with an error with `api_error: "computer_slept"`, continues
  it with an input "the computer was asleep, go on".

**The queue: "at the next step" or "after the turn", the queue panel.**
- Seen in hqc: what is typed during a turn goes into the turn ("joined the turn"); putting it off to
  the end of the turn, taking it back, editing it, reordering — not possible.
- hqc: the worker gives input during a turn to the CLI at once (`docs/notes/hqc-vs-claude-code.md`,
  "Handled right"); `queued` — only for an input waiting for its turn
  (`client/src/timeline.ts:568–581`, `client/src/ui/cards.tsx:744`).
- Claude Desktop: during a turn — "{sendKey} to send, {queueKey} to queue for later" (Enter /
  ⌘Enter, `ion:c82492f2c:21281–21307`), a "Queue for later" button (`20800–20808`); above the
  composer — "Queued messages": "Send now" with the explanation "Claude reads this message at its
  next step." / "Stops Claude's current response." / "Claude starts a new response with this
  message." (`ion:c9bd7af9c:20072–20133`), "Edit in composer", "Remove from queue", "Reorder queued
  message", "Clear all" (`19577–20654`); the main process holds the queue — `cancelQueuedMessage`,
  `reorderQueuedMessage`, `promoteQueuedMessage`, `sendSteeredNow` (`app:mainView.js:13129–13153`;
  `messageBuffer` — `app:D1yJ0UIX:39622–39660`).
- Related: "Send now", "taking a message back from the queue" — backlog.
- Fix: "after the turn" — the hub holds the message until `status idle`, as with a worker that is
  off; until it is delivered — Remove and Edit.

**"Compact now" after a long idle time.**
- Seen in hqc: you came back to a session after an hour — the next message re-reads the whole
  context without the cache, there is no warning.
- Claude Desktop: a bar above the composer with "Compact session" and "Why compact now": "After
  {age} idle, the cache has expired, so your next message re-reads all {tokens} tokens. Compact
  first so later replies run on a short summary instead." (`ion:c96d752d0:110489–110640`). The
  condition: an idle time of at least 70 min from the session's `lastActivityAt` and (a fill of
  ≥ 75 % of the window or ≥ 300 000 tokens) (`{minAgeMinutes: 70, minContextPct: 75,
  minContextTokens: 3e5}` and `Ax`, `ion:c82492f2c:4810–4827`; the condition is assembled in
  `ion:cc43287c9:75287–75470`). The bar is there only for sessions in the server experiment
  `epitaxy_stale_session_compact_absolute`, the 300 000 threshold only in its second arm; it is
  checked not by a timer but on coming back to the window. The fill is `input + cache_read +
  cache_creation` of the main agent's last answer (`ion:shared-192-DxjWqStk:21628–21640`); the button
  sends an ordinary `/compact`.
- Related: "The auto-compaction threshold, a compaction before the cache expires" — second list;
  "The prompt-cache timer" — backlog.
- Fix: hqc has no context fill — a turn's `usage` is taken from `result.usage`, the sums over all
  the turn's requests (`engine.rs:2045–2097`). The worker sends with every answer of the main agent
  `input + cache_read + cache_creation` of that answer and the model's window; the client — a bar
  on an idle time ≥ 70 min and a fill ≥ 75 %, `/compact` on a click.

**"What's using your usage limits?" — what the limit went on.**
- Seen in hqc: the limits of two windows, the turn's and the session's spending; why the limit ran
  out is not said.
- hqc: `client/src/ui/limits.tsx`, the spending line — `client/src/ui/cards.tsx:823–859`.
- Claude Desktop: a Usage panel with reasons over 24 hours — "Most usage on this machine in the last
  24 hours ran above 150k context.", "… while 4+ sessions were in parallel", "… came from
  subagent-heavy sessions", "… from sessions active for 8+ hours", "… hit a >100k-token cache miss"
  (`ion:c96d752d0:20092–20112`); advice and "Compact this session" (`15149–15291`); the data —
  `getCodeStats` (`app:mainView.js:13360`; how it is computed was not traced).
- Related: "The subscription's spending and limits" (`get_usage`) — second list.
- Fix: the hub knows the tokens and `cache_read` of every turn of every session — the same five
  reasons can be counted on the hub.

**A PR the agent opened is bound to the session (`system/code_change_published`,
`vcs_state_changed`).**
- Seen: the agent opened a PR — hqc has only the call line with the address in its output; the app
  binds the PR to the session, remembers the pushed branch and names the session after the PR.
- hqc: both subtypes go into an empty branch (`engine.rs:1162`).
- Claude Code: `code_change_published` (fields `provider`, `url`, `repo`, `identifier`, `action`,
  `branch` — `cli325:132063–132113`) — on a PR creation command (`cli325:457705–457735`); push,
  merge, rebase — `vcs_state_changed` (`cli325:457737–457758`). Live, both reach the stdout of `-p`,
  under `claude-vscode` and `claude-desktop`, and on 2.1.312: after a `git push` to a local bare
  repository — `{"subtype":"vcs_state_changed","kind":"push","branch":"probe-branch"}`
  (`rec:p2-325-vscode:134`), after `gh pr create` (a fake `gh` that prints an address) —
  `{"subtype":"code_change_published", "provider":"github",
  "url":"https://github.com/DrZlo13/hqc/pull/1","action":"created", "branch":"main"}`
  (`rec:p2-325-vscode:167`; `rec:p2-312-vscode:92`, `124`). The CLI does not react to a plain `echo`
  of a PR address; `branch` is the current local branch, not the pushed one. The VS Code extension
  does not parse them.
- Claude Desktop: `githubPr.bindFromUrl`, `recordPushedBranch` (`app:D1yJ0UIX:16441–16478`).
- Fix: a link to the PR in the chat header and in the session row.

**The feed view: Normal, Thinking, Verbose.** hqc has one feed: thinking and calls are collapsed
lines in groups (`client/src/ui/cards.tsx:870–919`). The app has a "Default transcript view" and the
session menu (`ion:cd19fedd2:520–553`); in Normal thinking is not drawn
(`ion:c96d752d0:94214–94217`), in Thinking — a one-line thinking summary above every group of calls
(`narration_summary`, `94203–94212`); whether summaries are wanted the interface tells the main
process (`setThinkingSummariesWanted`, `app:mainView.js:12893`). Fix: a switch in the chat menu; low
priority.

**An unrelated task — into its own session.** "When you ask for an unrelated task" — "Ask me /
Always do it here / Always start a new session" (`ion:cd19fedd2:220–260`), the card "Start this in
its own session?" (`ion:c96d752d0:54036–54042`, `app:mainView.js:15101–15109`); "Suggested tasks" —
proposed next tasks that can be let go into a separate session (`ion:cc43287c9:48065–50547`). In
hqc only the agent makes a separate session (`hqc_spawn`); the owner declined "suggestions for the
next prompt" — "Suggested tasks" are close to it.

**Auto-archiving inactive sessions.** "Archive inactive sessions — Automatically archive local
sessions after a period of no activity. Sessions that are running or have background work are never
archived…" (`ion:cd19fedd2:201–218`). hqc has only Archive in the menu
(`client/src/ui/SessionList.tsx:626`).

**Reading an answer aloud.** "Read aloud" at an answer (`ion:c96d752d0:83634–83660`); hqc does not
have it (`speechSynthesis` in `client/src` — 0). For a phone on the road; low priority.

**A process limit and eviction.** The app keeps at most `max(6, RAM / 3 GiB)` CLI processes
(`app:D1yJ0UIX:3661–3664`), the extra one is the one idle the longest, except running ones, with
cron, a wakeup timer, Remote Control (`84533–84599`); it also takes processes down on low memory
(`3834–3848`). The owner rejected eviction ("Agents are unloaded only for idleness", our notes, §3) —
not to fix.

**A warmed-up spare process.** The app brings a CLI up in advance (`prewarmSpare`,
`app:D1yJ0UIX:91651–91728`): the first message of a new session does not wait for the CLI and MCP
servers to start. In hqc the process starts on `session.start` (`supervisor.rs:1198`). Optional; the
price is an extra process per worker.

**Scheduled runs without a person.** The app runs a scheduled task as a separate session without
`AskUserQuestion` (`app:D1yJ0UIX:26036`), with `CLAUDE_CODE_HOST_SCHEDULED_RUN=1` and `origin {kind:
"task-notification", subkind: "scheduled-trigger"}` (`app:6TZs9uFI:40–60`). In hqc `hqc_schedule`
sends a message into an existing chat, where a person answers questions; nothing to change.

**Side panels**: Changes with comments on diff lines, Files, Terminal, Browser, Pull request,
Simulator (`ion:c4db265df:169–211`; a comment on a line goes to Claude —
`ion:c44d00c7f-BO7Ee6yo.js:2474–2492`). The owner is not doing these ("not doing") — listed for
completeness.

## Protocol 2.1.312 → 2.1.325

Schema blocks 515 → 524, top-level keys 224 → 229; no types, fields or values removed; the set of
types the stdout filter passes is the same; control subtypes 100 → 102; no CLI flags were removed
(`vendor-src/scripts/desktop-schema/`).

| What is new | What it is | What hqc does | What to do |
| --- | --- | --- | --- |
| `system/session_writes_lost` | the transcript was not written: `ENOSPC`/`EDQUOT` (`cli325:132211`); only not under desktop and on an exit with code 0 (live — `rec:p5-325-vscode:30`) | an empty branch (`engine.rs:1162`); the worker takes the process down with SIGTERM — no frame | "Not handled" |
| `user.retracted_message_uuids` | a thrown-away answer with an unparsed call (`cli325:128597`) | not read; #103 — only for `model_refusal_fallback` | "Not handled" |
| new values of `assistant.api_error` (the error kind with `is_api_error_message`): `computer_slept`, `connection_failed`, `stream_idle_timeout`, `hook_denied`, `stored_login_refused`, `malformed_tool_use_exhausted`, `network_edge_blocked`; `api_error_params.cut` (`output_limit`/`context_window`) | the answer's error kind, "for consumers that key on the cause instead of the message text" (`cli325:129155–129197`) | the kind is not read, the text is shown | `computer_slept` and `cut: context_window` — continuing after a sleep and compaction ("Only in the app"); the kind — also for the reason in the API error line ("Partly") |
| `background_tasks_changed.tasks[].off_host` | work outside the process (`cli325:131273`) | counts as work (`engine.rs:1640–1646`) | "Wrong" |
| `informational.model_restriction` `{requested_model, effective_model, at}`, `hook_no_verdict` | the model is not the one asked for; a hook without a verdict (`cli325:130549`, `130561`) | not in master; #103 shows the `informational` text (not merged) | after #103 — the model in the header from `effective_model` |
| `interrupt.spare_background_tasks` | `perTaskStopAffordance` for one request (`cli325:132981`) | the flag in `initialize` (`engine.rs:715–721`) | nothing |
| control `set_has_subscribers`, `CLAUDE_CODE_STREAM_EVENT_HOLD_MS` | the CLI glues `content_block_delta` and `thinking_tokens` chunks together, and holds them longer when nobody watches (`cli325:135467–135480`) | not used | a hypothesis: fewer worker → hub frames; the app sets 100 ms only over SSH ("several times the bytes", `app:CBoQOmxo:259–260`) |
| control `set_attached_project` | a Claude project of a cloud session (`cli325:1442212–1442222`) | — | nothing |
| `result.startup_failure_reason`: `initialize_not_first`, `config_file_invalid`, `sandbox_required_unavailable`, `managed_settings_not_delivered`; `result.invalid_config_file {path, is_global, backup_path}` | the reason for a failed start (`cli325:129403–129406`, `129935`) | does not set `STARTUP_FAILURE_RESULTS` | with it — "settings.json is broken: <path>" |
| `status.resume_held_for_plan` | with `RESUME_INTERRUPTED_TURN` (`cli325:130453`) | not used | nothing |
| `--session-began-as-copy` | `--resume` of a copy does not continue the source's last turn (`cli325:1051296–1051300`) | not needed while there is no `RESUME_INTERRUPTED_TURN` | on moving to it — set it in copies |
| `initialize.managedSettings`, `resumeOnly`; `--managed-settings-in-initialize`, `--agents-view-if-allowed`, `--lead-verify-key` | managed settings, internal | — | nothing |
| new `capabilities` in `system/init`: `background_tasks_moves_bash_v1`, `read_file_live_reply_v1`, `reply_gate_policy_v1` and 6 more (`cli325:972386–972586`) | what the CLI can do | `capabilities` are not read | nothing |
| `system/ui_notify`, `ui_notify_cancel`, `ui_placed`, `init` fields (`ultrathink_available` and others), `queued_notification.fire_reason` | plugin notifications and internals (`cli325:131465–131650`) | an empty branch | nothing |

By the schema, hqc's parsing works on 2.1.325 as on 2.1.312. The hqc worker itself was not run on
2.1.325: the 2.1.325 runs are with its flags in the probe harness, without a hub.

## Handled right

- The stream flags are the same as the app's: `--include-partial-messages`,
  `--replay-user-messages`, `--permission-prompt-tool stdio`, `--thinking-display summarized`
  (`claude_code.rs:650–664`); `initialize {perTaskStopAffordance: true}` (`engine.rs:715–721`).
- Switching the permission mode and the model on the fly, `background_tasks`, `stop_task` — the same
  requests (`engine.rs:744–795`).
- The queue is kept on our side, without `cancel_async_message`: the app also keeps the queue itself
  and does not call `cancel_async_message` (`app:D1yJ0UIX:39622–39660`, `18820–18855`).
- A stream that ended without `result` is a lost turn (`crates/hqc-hub/src/events.rs:599–606`); the
  app has "The session stopped before completing." (`app:D1yJ0UIX:16904–16932`).
- A copy from a message: a copy of the transcript up to the cut and `--fork-session
  --resume-session-at` (`claude_code.rs:737–750`, `809–848`) — the app does the same
  (`app:D1yJ0UIX:29726`, `30031–30041`).
- `ambient` tasks do not count as work, a wakeup timer keeps the process (`engine.rs:1640–1646`,
  `supervisor.rs:1183–1193`) — as `hasLiveLoopWakeup` in the app.
- API retries, compaction, hidden thinking, the limits bar, CLI and hook messages — as in the app
  (`ion:c4a9ee384:14336–14410`, `ion:c06cf64bb:8645–8700`): closed by #103 (not merged); the VS Code
  extension does not show retries.
- "Interrupted", a queued input at the bottom, Stop at a background task's line, a call without a
  result — master (#102); `/clear` — "Conversation cleared" (master), the app has a "Context
  cleared" divider (`ion:c96d752d0:94340–94346`).
- Images in results, "Full result", a request's withdrawal, "only a person's", a request's reason —
  #105 (not merged); a subagent's history and a background task's outcome — #104 (not merged).
- A turn's spending as a feed line and the session's total in the header
  (`client/src/ui/cards.tsx:823–859`): the app has no spending line in the feed, the cost is only in
  the Usage panel (`ion:c96d752d0:15283`, `15551`) — here hqc gives more.
- Quoting a selection (`client/src/ui/Chat.tsx:796–812`), Copy and Fork from here at a message
  (`client/src/ui/cards.tsx:713–733`) — as "Attach selection as context", "Copy message", "Fork from
  here" (`ion:c96d752d0:18614–18828`); messages from other sessions and the SendMessage line — as the
  app's `peer` and `peerMessage` (`ion:c06cf64bb:9069–9107`).
- What neither hqc nor the app uses: `agentProgressSummaries` (hard `false`, `app:fch7zeJ_:15881`),
  `--session-mirror`, `--include-hook-events`, `side_question`, `set_cwd`, `get_plan`,
  `export_conversation`.
- A stream cut mid-answer: the CLI in `-p` continues the answer by itself and ends the turn with
  `success` (live, `rec:p7-stop-325:845`) — nothing for the worker to do.
- The `post_turn_memory` task does not get into the background task list at all
  (`cli325:1002475–1002494`) — it does not keep the session in Running. An agent team member
  (`in_process_teammate`) comes without `ambient` and keeps Running while it lives, even when idle
  (`cli325:1758164–1758179`, `253828–253830`) — the VS Code extension counts the same way
  (`ext:123914`); whether hqc sessions have such members was not traced.
- The hooks the VS Code extension registers in `initialize` (`PreToolUse` — a diagnostics snapshot
  and autosave, `PostToolUse` — new editor errors in `additionalContext`, `ext:138055–138061`) are
  tied to the editor — nothing for hqc to take. The app's hooks (`createBaseHooks`,
  `app:hrMvQ5DX:7855–9121`; `UserPromptSubmit`, `PostToolBatch` — `app:D1yJ0UIX:22001–22047`) serve
  its folders, worktrees and Workflow; hqc has `result` instead of `Stop` as the end-of-turn signal.

## Not checked

- The app was not run: everything about its behavior is by the code. The hqc worker was not run on
  CLI 2.1.325: the 2.1.325 runs are with the worker's flags in the probe harness, without a hub.
- The subscription token leaking to a provider under `claude-desktop` — by the chain of code, not
  run live.
- Sleep: `computer_slept` cannot be caused on the Linux test bench; on the laptop and vvzvladmbpm1 —
  only with a real sleep. A context overflow mid-turn was not caused — whether the CLI goes on by
  itself is unknown.
- Not caused live: a real model refusal and the `refusal_fallback_prompt` dialog; an MCP task with
  `off_host`; `retracted_message_uuids`; `instruction_size_warning` (the server flag is off for the
  account); announcing a branch's PR at start (needs a `gh` token and a request to GitHub);
  `workflow_progress` under `claude-vscode`; `stop_reason: "prompt_injection_risk"`.
- A slash command during a turn: by the code it runs at once under `claude-vscode`, live it waited
  for the end of the turn under both entry points — why, was not traced (a hypothesis: an input from
  stream-json has no `localStdinOrigin`).
- Which cache TTL (5 min or 1 h) the workers' sessions have; whom the server puts into the "compact
  now" bar's experiment.
- How often the CLI does not answer `interrupt`; whether hqc sessions have agent team members
  (`in_process_teammate`).
- Whether the person's message bubble in the app turns on parsing of `#123` and addresses; how the
  main process shows OS notifications for Code sessions.
- Self-update was checked only on agent-node-227; the versions on the other five workers were not
  looked at.
