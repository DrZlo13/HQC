# Claude Code: what the CLI gives the client and what it takes

A reference of the Claude Code API, reconstructed from code. What the CLI sends to stdout in the
`-p --input-format stream-json --output-format stream-json` mode, what it takes on stdin, which
control requests go both ways, what it writes to the JSONL transcript and how the VS Code extension
parses all of it — with the edge cases found in the code. Versions: CLI 2.1.312 (on the workers of
the laptop, agent-node-227 and agent-node-129; on agent-node-226 — 2.1.304, on agent-node-3 and
vvzvladmbpm1 — 2.1.318 by `claude --version`), Agent SDK types 0.3.312 (for the same CLI) and 0.3.319 (for comparison),
VS Code extension 2.1.319. The Claude app (5.1231.2) carries the same Agent SDK and runs the same
CLI with the same stream-json flags, so its protocol is the same one.

The document is built from code; what the code does not settle was checked with live runs on the
agent-node-227 test bench (marked "live run" with the capture name, see "Sources"). What was checked
with live runs earlier is marked with a reference to a SPEC (`hqc:docs/SPEC.md`, `docs/SPEC.md` of
this repository, `cc_remote/docs/SPEC.md`).

## Sources

The files are in `vendor-src/` of this repository (`/Users/vvzvlad/Data/Projects/hqc_dev/vendor-src/`,
not in git).

| Label | File | What it is |
| --- | --- | --- |
| `cli:N` | `cli-2.1.312.pretty.js`, line N | JS from the binary `~/.local/share/claude/versions/2.1.312`: each module from the `// @bun` mark to the first zero byte, formatted with prettier 3.3.3; modules are separated by the line `/* ==== module at byte N of the binary ==== */`, names are minified. Some occurrences exist only in the raw binary (bytecode) — check them with `LC_ALL=C /usr/bin/grep -a -c` on it |
| `ext:N` | `ext-2.1.319-extension.js` | the VS Code extension host 2.1.319 — runs `claude` and talks to it in stream-json; formatted from `extension.js.orig` (the original, without the local `patch-claude-vscode` patch) |
| `web:N` | `ext-2.1.319-webview.js` | the extension's page itself (webview), formatted from `webview/index.js.orig` |
| `d312:N`, `d319:N` | `sdk-0.3.312/sdk.d.ts`, `sdk-0.3.319/sdk.d.ts` | the official `@anthropic-ai/claude-agent-sdk` types from npm (`claudeCodeVersion` 2.1.312 and 2.1.319); next to them, `sdk-tools.d.ts` — the tool schemas |
| `hqc:…` | `/Users/vvzvlad/Data/Projects/hqc/…` at `9f08283` | the SPEC, RUNBOOK and code of hqc: live checks and the comparison with the worker |

- The main source for the shape of messages is the zod schema of the protocol inside the CLI itself
  (`cli:105050`–`cli:114280`). It is wider than `sdk.d.ts`: the `@internal` fields are cut out of the
  d.ts (empty lines in their place, for example `d312:4839`–`d312:4842`), while the CLI schema has
  them. The field lists in part I are taken from the schema with the script
  `vendor-src/scripts/fields.py` (output — `scripts/fields.txt`, `scripts/ctlfields.json`);
  `type`, `subtype`, `uuid`, `session_id` are left out of the lists — every message has them. Marks:
  `[i]` — `@internal`, `[?]` — optional, `[n]` — can be `null`. The schema describes the shape but
  does not guarantee that the message goes to stdout: what gets through is decided by filters (2.0),
  and this is noted at each type. All 101 subtypes of requests to the CLI are in
  `scripts/dispatcher.txt`.
- The extension host has two copies of the SDK's history reader: `ZTe` (`ext:57468`, when the
  storage `ip()` is set) and `ADn` (`ext:87619`, by default via `XKe`, `ext:112675`). A normalized
  diff (names → `I`): 36 differing lines out of 571, only formatting and one extra function in the
  second copy. Part II cites lines of the first copy.
- Counts over transcripts come from `scripts/scan.py` and `scripts/scan2.py` (they print only keys,
  types and counts) over all `~/.claude/projects/**/*.jsonl` except `journal.jsonl`: 14 174 files
  (1 633 sessions and 12 541 subagent files), 4 380 776 lines, ~13.7 GB, not one line dropped, 0
  broken JSON. Results — `scripts/scan-all.json`, `scripts/scan2.json`, queries over them —
  `scripts/q.py`, `scripts/q2.py`, table 7.3 — `scripts/table.py`. The sample is skewed: this is the
  laptop, and almost all records were written by VS Code of various versions (2.1.260–2.1.319).
  Records with `version: "2.1.312"` — 68 168, in 67 session files and 93 subagent files ("files with
  2.1.312" below). Transcripts of the hqc workers on the nodes are not included here — they are in 7.7.
- Live checks — 35 runs on agent-node-227 on 2026-11-12: CLI 2.1.312, `--model haiku`,
  `-p --input-format stream-json --output-format stream-json --verbose`,
  `CLAUDE_CODE_ENTRYPOINT=claude-vscode`; they differed from the extension in `--strict-mcp-config
  --mcp-config '{"mcpServers":{}}'`, `--permission-mode bypassPermissions` and `--session-id`. Each
  case was captured in one run, except the position of the first prompt's echo (24 runs). Another 3
  `w*` runs with the same flags and 22 `x*` runs with the hqc worker's flags
  (`--permission-prompt-tool stdio`, `--setting-sources user,local`, mode `auto` or `default`, sonnet
  or haiku); permission requests in them are answered by the runner itself (`drive.py`,
  `auto_respond`); question previews — three runs per variant. Captures —
  `/home/claude/cc-api-probe/captures/<name>.jsonl` on the test bench (next to them `.stderr`,
  `.times` — ms from start for each line, `.stdin.jsonl` — what was sent and when, `.meta.json` — the
  command and exit code); scenarios — `scen/<name>.json`, driver — `drive.py`. In the text: "live run
  `<name>`, line N".
- The VS Code extension 2.1.319 runs its own bundled CLI 2.1.319
  (`resources/native-binary/claude` of the extension), the hqc workers run 2.1.312. So the
  extension's parsing is described for the 2.1.319 stream, and the protocol and the on-disk record
  from the 2.1.312 code.
- "Ignore the unknown": "Consumers should ignore types and subtypes they do not recognize: the
  set grows over time" (`d312:5591`, `cli:109875`).

## In short

Cases that caught us, from the protocol code (details and lines in sections 5.3–5.8):

1. **A subagent's Bash goes to the background after ~180 s only in the VS Code extension.** The Bash time limit of a background subagent comes from the server flag `tengu_maple_ferry` (default 0 — no limit), but only when `WP()` is false, and `WP()` is always false with `CLAUDE_CODE_ENTRYPOINT=claude-vscode` (`cli:36939`–`cli:36945`, `cli:518285`–`cli:518299`, call `cli:587148`, use `cli:377239`–`cli:377248`, `cli:587511`). The subagent sees "Command was moved to the background (ID: …) after running for Ns" (`cli:518320`). In the code the flag defaults to 0 and the value comes from the server: in the flag cache (`cachedGrowthBookFeatures.tengu_maple_ferry` in `~/.claude.json`) it is `180000` ms both on agent-node-227 and on the laptop, hence ~180 s.
2. **`task_notification completed` with the text "This agent has not reported yet…" without `handback`, then `task_started` of the same `task_id`.** The subagent finished its turn but holds its own background work (`vyt`, `cli:582087`–`cli:582097`): this is an interim report. The text is `Ynr` (`cli:195067`, `cli:727680`), and the `handback` field is dropped precisely for the interim `withheld` (`cli:728549`). The terminal claim `terminalEmitClaims` is released after this, and on the next start of the same task the CLI issues a new `run_id` and `task_started` again (`cli:332849`–`cli:332914`). So one task can send several `task_notification`s, each with its own `run_id`.
3. **A turn closed on a background subagent's notification without a model call.** When the subagent's report has already been delivered to the main agent by a `SubagentHandback` call (handback `send`), the completion notification is queued with `shouldQuery:false, handbackPointer:true` (`cli:582337`, condition `cMt` — `cli:422524`–`cli:422531`, flag `tengu_hushed_kestrel`, default true). Such a turn takes the branch without a query: `result` with `num_turns:0`, `duration_api_ms:0`, `stop_reason:null`, and `total_cost_usd` is the session's running total, not the turn's cost (`cli:1497509`–`cli:1497553`). The extension recognizes it as `num_turns === 0 && origin.kind === "task-notification"` (`ext:114647`) and does not count it as a finished turn.
4. **`--thinking-display summarized`.** Without the flag, the display mode in stream-json is not set at all (`KXo`, `cli:144218`–`cli:144223`) and the API decides; with the flag, thinking arrives in pieces as `stream_event` with `thinking_delta` (only with `--include-partial-messages`), and `stream_event` and `assistant` carry `thinking_display` (`cli:108096`, `cli:723345`–`cli:723349`). The extension passes the flag only if `showThinkingSummaries` is on in the Claude settings (`ext:123928`–`ext:123931`, `ext:129514`–`ext:129525`).
5. **A message sent mid-turn lands after a tool result; a message sent during the final answer goes into the next turn.** The queue is folded into the running turn only after a batch of tool calls has run (after the `PostToolBatch` hook) and before the next model request (`cli:762448`–`cli:762527`; the documentation: "folded into the running turn between tool rounds", `d312:5987`–`d312:5989`). The `isReplay:true` echo of a folded-in message is born at that moment from the `queued_command` attachment (`cli:1489207`–`cli:1489210`, `vAr` — `cli:723351`). If the model is already writing the last answer without tool calls, there is no next request in this turn: the message stays in the queue and opens its own turn. `command_lifecycle` reports the fate of every message with a `uuid` (`queued` → `started` → one of `completed/cancelled/discarded/refused`, `cli:109493`–`cli:109519`).
6. **`/compact`, auto-compaction and `--resume`.** Compaction: `status:"compacting"` → `compact_boundary` → the summary as `user` (for `/compact` — `isReplay:false`, `isSynthetic:true`, `cli:1489282`–`cli:1489293`). After `--resume` the CLI itself sends notifications about tasks that the previous process did not finish: background commands get `status:"stopped"` with the text "Background shell command didn't finish before the previous session ended" (`cli:1353889`–`cli:1353893`, `cli:1354471`–`cli:1354486`).

More edge cases, captured by live runs:

- A turn that the CLI started by itself on a background task's notification sends no `user` at all —
  even with `--replay-user-messages`; the notification text is only in the transcript (5.3).
- In the "heartbeat" of a long call (`tool_progress`, `heartbeat: true`) in the main thread,
  `parent_tool_use_id` is the id of the call itself, and `tool_use_id` is `<id>-heartbeat-N`: telling
  "this is a subagent" by `parent_tool_use_id` goes wrong here (2.1).
- The summary of auto-compaction and the one of `/compact` arrive differently: for auto-compaction
  `content` is an array and there is no `isReplay` key, and the prompt echo comes before the boundary;
  for `/compact` it is a string and `isReplay:false`. Compaction can fail (`too_few_groups`), and the
  turn goes on overfull (5.7).
- `[Request interrupted by user for tool use]` arrives without `isSynthetic` (5.4).
- `priority:"now"` moves a running Bash, MCP call or subagent to the background and folds the message
  into the same turn only with a `uuid`, `origin.kind:"human"` and a running call; an MCP call then
  becomes an `mcp_task` task for the first time, with its own `task_started` and notification.
  Otherwise the turn is cut off, and during text — with the unfinished text in `result` (3.2).
- `end_session` mid-turn — exit code 1, when idle — 0 (5.9).
- With `claude-vscode`, `Agent` goes to the background even without `run_in_background`; Bash refuses
  `sleep N` with N ≥ 25 in the foreground (5.6).

The transcript and the VS Code extension (details in sections 7–9):

1. The 2.1.312 code has 41 kinds of top-level records (two policy tables, `cli:409328` and
   `cli:411518`, have the same set). Real files contain 27 kinds: 26 of these 41 and
   `session-owner`, which 2.1.312 does not have (2.1.319 writes it). Files with 2.1.312 contain 23 of
   41 and `session-owner`. The conversation is carried by `user`, `assistant`, `system`, `attachment`
   (they have `uuid`/`parentUuid`; so does `progress`, but the files do not contain it); the other 36
   are service records of the session with no place in the chain.
2. The CLI process writes a conversation line once per `uuid`: before writing it checks the `uuid`
   against those already written in this session (`cli:414107`). The exception is subagents: their
   lines go to their own file without this check. The files still have repeated `uuid`s (7.5); their
   cause is not checked.
3. One API message is written as several `assistant` lines — a line per content block, with one
   `message.id` and a continuous chain (in files with 2.1.312, 16 215 of 23 093 messages are split).
   A `user` line with a `tool_result` points its `parentUuid` at the `assistant` line that had the
   `tool_use` (`sourceToolAssistantUUID`, `cli:413847`), not at the previous line of the file.
4. Compaction writes `system/compact_boundary` with `parentUuid: null` and `logicalParentUuid`
   pointing at the last line before compaction, then attachments and a `user` line with
   `isCompactSummary: true` and `isVisibleInTranscriptOnly: true` — its `message.content` (a string)
   holds the summary. The tail of the conversation kept by compaction stays above the boundary in the
   file and is stitched to the summary only on reading (`compactMetadata.preservedSegment`).
5. The CLI rewrites metadata (`last-prompt`, `ai-title`, `agent-setting`, `mode`, `atis-latch` and
   others) anew at the end of the file every 32 KiB written and on exit (`cli:412948`,
   `cli:413123`) — so the files hold hundreds of thousands of copies of them.
6. The CLI can rewrite a file whole, dropping everything before the last compaction boundary
   (temporary file + rename, `cli:413480`), but in `-p` mode only with
   `CLAUDE_CODE_TRANSCRIPT_LOCAL_GC=1` (`cli:793877`); it is off by default.
7. The extension reads the file once — on opening (`loadFromServer`, `web:168861`) through the SDK
   reader in the host; after that it lives only on the stream of its own process. It does not see
   appends to the file by others (another process, `claude -p --resume`) until the session is opened
   in it again.
8. History and the live stream look different in the extension: history has no `system` lines
   (including the compaction boundary: the summary arrives as a plain `user` line), no `isMeta` lines,
   and messages taken mid-turn turn from `attachment/queued_command` into `user` with
   `isQueuedCommand`; in the live stream the summary hides in a collapsed "Compacted chat" line.
9. The extension's feed holds no more than ~600 lines: old ones are pushed out (`web:171897`,
   `Sq = 600`, `web:167672`). Consecutive successful `Read`s are glued into one line "Successfully
   read N files" (`web:163312`).

---

# Part I. The stdin/stdout protocol

## 1. Launch

### 1.1 The flags the VS Code extension runs the CLI with

The arguments are assembled by `ProcessTransport.initialize` of the bundled SDK (`ext:109352`–`ext:109473`) from the `spawnClaude` options (`ext:138007`–`ext:138073`). `--output-format stream-json --verbose --input-format stream-json` always come first (`ext:109396`).

| Flag | Where the extension gets it | What it changes in the stream |
| --- | --- | --- |
| `--output-format stream-json --input-format stream-json --verbose` | always, `ext:109396` | line-by-line JSON both ways; without `--verbose` the CLI exits with "Error: When using --print, --output-format=stream-json requires --verbose" (`cli:1503886`–`cli:1503887`) |
| `--permission-prompt-tool=stdio` | `canUseTool` is set, `ext:109429`–`ext:109433` | permission questions arrive as `control_request can_use_tool` (section 4.1); without the flag the CLI refuses by itself (`permission_denied`), confirmed live in `hqc:docs/SPEC.md:2242` |
| `--include-partial-messages` | `includePartialMessages: !remoteName` — only outside a Remote window, `ext:138050`, `ext:109454` | turns on `stream_event` (`cli:1489250`) and `status:"requesting"` (`cli:1489167`) |
| `--replay-user-messages` | `extraArgs`, `ext:138069` | echo of `user` with `isReplay:true`, echo of incoming `control_response` and `assistant` (`cli:1512337`–`cli:1512339`, `cli:1512409`–`cli:1512412`), echo of folded-in messages (`cli:1489207`) |
| `--enable-auth-status` | `extraArgs`, `ext:138067` | `auth_status` messages (`cli:1505025`, `cli:1513252`) |
| `--debug --debug-to-stderr` | `extraArgs`, `ext:138065`–`ext:138066` | stderr only |
| `--no-chrome` | `extraArgs`, `ext:138068` | without Claude in Chrome |
| `--safe-mode` | if `CLAUDE_CODE_SAFE_MODE` is in the user's variables, `ext:112977`–`ext:112979` | live runs (`p13-safe-mode` against `p13-mcp-default`): `init` has no `mcp_servers` (the server from `--mcp-config` is dropped) and none of its tools, no `memory_paths`, `slash_commands` 80 → 57 (without user skills and plugin commands), `skills` 42 → 22, `agents` 7 → 5; before the first turn — an extra frame `system/ui_invalidate {event:"ui.render"}` |
| `--setting-sources user,project,local` | `ext:138062`, `ext:109442` | which settings files are read |
| `--resume <id>` | when an old session is opened, `ext:138019`, `ext:109433` | see 5.8 |
| `--model`, `--permission-mode`, `--allow-dangerously-skip-permissions`, `--add-dir` | the extension's options (`ext:138034`), `ext:109416`, `ext:109444`–`ext:109445`, `ext:109457` | the model and the permission mode in `system/init` |
| `--thinking=disabled` or `--max-thinking-tokens 31999` | `BQ()`: "off" → disabled, otherwise `budgetTokens: 31999` (`ext:123927`–`ext:123931`, conversion into `thinkingConfig` — `ext:112382`–`ext:112391`, the flag — `ext:109401`, `ext:109404`) | the thinking budget |
| `--thinking-display summarized` | only with `showThinkingSummaries: true` in the Claude settings (`ext:129521`–`ext:129525`; `thinkingSummariesDefaultOn()` — `false`, `ext:129514`) | the thinking text in pieces (5.11) |
| `--mcp-config <json>` | if the extension passed MCP servers, `ext:109441` | — |

What the extension does **not** pass (a comparison for the hqc worker): `--session-mirror` (`ext:109455`; without it there is no `transcript_mirror`, `cli:1504868`–`cli:1504871`), `--include-hook-events` (`ext:109453`; without it, of the hook events only `SessionStart`/`Setup` go out and, by an environment variable, `UserPromptSubmit`, `cli:436627`–`cli:436633`), `--permission-prompts`, `--strict-mcp-config`, `--session-id`.

### 1.2 Environment variables

The environment is assembled by `Go()` (`ext:139558`–`ext:139580`), `spawnClaude` (`ext:138080`–`ext:138088`) and the SDK (`ext:112343`–`ext:112360`).

| Variable | Value in the extension | What it changes in the CLI |
| --- | --- | --- |
| `CLAUDE_CODE_ENTRYPOINT` | `claude-vscode` (`ext:139574`; the SDK sets `sdk-ts` only if it is empty — `ext:112343`, `ext:109476`) | `$m()` is true (`cli:36939`–`cli:36942`) → `WP()` is false (`cli:36943`–`cli:36946`) → background subagents get the Bash limit from `tengu_maple_ferry` (case 1); it is written to the transcript as `entrypoint`, and by it the extension shows the session in its history (confirmed live on 2.1.309, `hqc:docs/RUNBOOK.md:858`–`hqc:docs/RUNBOOK.md:861`) |
| `CLAUDE_CODE_SDK_READS_SESSION_STATE` | `1` (`ext:112346`–`ext:112347`) | the CLI sends `session_state_changed` with `sdk_host_only:true` (`cli:390593`–`cli:390596`); the extension's SDK swallows such frames (`ext:110294`) |
| `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS` | not set | without `sdk_host_only` — the frames are visible to the client (`cli:390593`) |
| `CLAUDE_CODE_ENABLE_TASKS` | `0` (`ext:139563`) | `gM()` is false (`cli:374072`–`cli:374075`): the model has the old `TodoWrite` (`cli:743559`) instead of the four task list tools (`cli:746996`) |
| `CLAUDE_CODE_EMIT_STARTUP_TIMING` | `1` (`ext:139564`) | the `startup_timing[i]` field in `system/init` (`cli:107940`) |
| `CLAUDE_CODE_STARTUP_FAILURE_RESULTS` | `1` (`ext:139565`) | on a known startup error the CLI writes a zero `result error_during_execution` with `startup_failure_reason` (`d312:5981`) |
| `MCP_CONNECTION_NONBLOCKING` | `true` (`ext:139562`) | by the code, without the variable the mode is already non-blocking (`cli:801525`). Live (`p13-*`, an MCP server that answers after 8 and 40 s): the first turn in `-p` waits for the MCP connection with any value — up to ~27 s, then the server is in `init.mcp_servers` with `status:"failed"`, and there is no retry in the second turn; the variable changes only the moment stdin is read — `command_lifecycle queued` after 0.7 s against 5.8 s with `false`. Where ~27 s comes from was not found in the code (`MCP_CONNECT_TIMEOUT_MS` defaults to 5000, `MCP_TIMEOUT` to 30000, `cli:221653`–`cli:221660`) |
| `CLAUDE_CODE_EMIT_USER_PROMPT_SUBMIT_HOOK_EVENTS` | `1` if the gate `tengu_optimized_puzzle` is not `false` (`ext:138081`–`ext:138083`) | `hook_started`/`hook_response` for `UserPromptSubmit` (`cli:436708`, `cli:436631`) |
| `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING` | `true` (`enableFileCheckpointing`, `ext:138048`, `ext:112345`) | file snapshots for `rewind_files` |
| `CLAUDE_CODE_QUESTION_PREVIEW_FORMAT` | `markdown` (`toolConfig.askUserQuestion`, `ext:138063`, `ext:112354`) | the format of previews on `AskUserQuestion` options; in `-p` with `claude-vscode` it does not affect previews live: when asked for previews — on all options in 3 of 3 runs both with it and without it; when not asked — 0 of 3 in both cases (`x5-*`, `x5n-*`, sonnet) |
| `CLAUDE_CODE_RESUME_INTERRUPTED_TURN`, `…_MAX_AGE_MS=3600000` | only on a restart after a crash (`IJe`, `ext:132857`–`ext:132864`) | the CLI itself restarts the interrupted turn; its `result` has `resume_reason` (`d312:5993`) |
| `DISABLE_TELEMETRY` | `1` if telemetry is off in VS Code (`ext:137228`–`ext:137230`) | no metrics and no flags from the server — then the server gates take their default values |
| `CLAUDE_AGENT_SDK_VERSION` | `0.3.319` (`ext:112344`) | — |
| removed | `CLAUDECODE`, `CLAUDE_CODE_CHILD_SESSION`, `TRACEPARENT`, `TRACESTATE`, `NODE_OPTIONS` (`ext:139575`–`ext:139578`, `ext:109477`) | otherwise the CLI would consider itself a child (`$m()` requires `!childSession && !claudecode`) |

### 1.3 The `initialize` request the extension sends

As the first frame on stdin the SDK sends `control_request initialize` (`buildInitializeRequest`, `ext:110591`–`ext:110642`). The fields the extension actually fills (`ext:138047`–`ext:138063`) and what they do in the CLI:

- `hooks` (`ext:138055`–`ext:138060`) — its own `PreToolUse` (`Edit|Write|MultiEdit`, `Edit|Write|Read`) and `PostToolUse`; each callback is assigned `hook_N`, and the CLI later calls it with a `hook_callback` request (section 4.2). Shape check — `cli:1508863`–`cli:1508886`, error — "initialize: hooks must map hook events to arrays of matchers…".
- `perTaskStopAffordance: true` — on `interrupt` the CLI does **not** stop background subagents and workflows (`cli:1504650`–`cli:1504653`, `cli:1513148`, description `cli:110014`). Without this field an interrupt kills them (`Kle`, `cli:742336`–`cli:742345`).
- `forwardNestedSubagentToolCalls: true` — `tool_use`/`tool_result` of second-level subagents go into the stream with `parent_tool_use_id` (`cli:110019`, `cli:1513150`).
- `systemPrompt: {preset: "claude_code", append}`, `supportedDialogKinds` (the `request_user_dialog` kinds the extension can draw; without the declaration the CLI does not send the dialog, `cli:110004`), `agentProgressSummaries`/`promptSuggestions` — `undefined` in the extension (`ext:138051`–`ext:138052`).
- The extension does not set `rapidFollowupPreempt`: without it a message that arrives before the turn's first output waits its turn rather than interrupting the turn (`cli:110024`).

The response to `initialize` is `success` with `commands`, `agents`, `output_style`, `models`, `account`, `pid` and so on (schema `cli:110094`), plus `pending_permission_requests` and `pending_user_dialog_requests` — the open questions of this process (`d312:418`, `d312:422`). A shape error is `error` with the text `initialize: …` (`cli:1508851`, `cli:1508856`, `cli:1508890`). A repeated `initialize` is not rejected: "First-attached-client-wins… later initializes do not change it" (`cli:110014`, `cli:110019`).

### 1.4 For comparison: the hqc worker

The hqc worker runs the CLI itself (flags — `hqc:docs/SPEC.md:2211`–`hqc:docs/SPEC.md:2217`) and sends only three control requests — `interrupt`, `set_permission_mode`, `set_model` (`hqc:crates/hqc-worker/src/adapter/claude_code/engine.rs:390`–`hqc:crates/hqc-worker/src/adapter/claude_code/engine.rs:417`); it does not send `initialize`. Consequences by the CLI code: `perTaskStopAffordance` is not declared, so an `interrupt` from hqc kills background subagents (`cli:1504653`); there is no `session_state_changed` unless one of the two variables above is in the environment; the worker passes `CLAUDE_CODE_ENTRYPOINT=claude-vscode` by the `show_in_vscode` setting (`hqc:docs/SPEC.md:4784`), and then case 1 applies.

## 2. CLI → client: stdout messages

### 2.0 How a message gets into stdout

- **One line — one JSON.** Everything that goes to stdout is a member of `StdoutMessage`: `SDKMessage`, `SDKActiveGoalMessage`, `control_response`, `control_request`, `control_cancel_request`, `keep_alive` (`d312:9980`).
- **Two paths.** (1) Events of the query loop go through the engine's generator: it yields the raw event and, where needed, its "twin" in SDK shape (`yield-twin`, `cli:768700`–`cli:768850`), and then the `RC` filter in `print.ts` decides what to let through (`cli:1489154`–`cli:1489276`). (2) The rest is put into the output queue directly via `ea()` (tasks, hooks, session state, `command_lifecycle`, `rate_limit_event` and so on) and does not pass the `RC` filter.
- **What `RC` lets through from the query loop:** `assistant` (without an already set `parent_tool_use_id`, `cli:1489157`–`cli:1489158`), `user` and `progress` (via the converter `Xge`, `cli:1489160`–`cli:1489162`, `cli:723739`), of `system` — only `status` (`requesting` — only with `--include-partial-messages`), `model_refusal_fallback` (without the internal `provisional`), `init`, `notification`, `api_retry`, `model_refusal_no_fallback`, `memory_recall`, `thinking_tokens`, `compact_boundary`, `model_fallback`, `model_consent_fallback`, `informational` (`cli:1489166`–`cli:1489200`); `tool_use_summary`, `queued_command` attachments (as an echo), `result`, `stream_event` (only with partial). **All other `system` subtypes of the loop are dropped** (`default: return`, `cli:1489201`–`cli:1489202`), and so are the raw types `tombstone`, `stream_request_start`, `tool_progress`, `command_lifecycle`, `auth_status`, `prompt_suggestion`, `rate_limit_event`, `sdk_status`, `compact_progress`, `post_turn_summary`, `active_goal`, `conversation_reset` and others (`cli:1489252`–`cli:1489275`); their SDK forms, if any, arrive by path (2).
- **Order relative to `result`.** `result` is one per turn, after all `assistant`/`user`/`stream_event` of the turn; informational `system` messages (task notifications, session state, suggestions) can come after it (`d312:6013`, the text above `SDKResultMessage`). What `print.ts` counts as "the last message of the turn" is decided by `cV` (`cli:1503369`–`cli:1503425`): `control_*`, `session_state_changed`, `permission_denied`, all `task_*`, `background_tasks_changed`, hooks, `stream_event`, `keep_alive`, `prompt_suggestion`, `conversation_reset`, `transcript_mirror`, `command_lifecycle`, `active_goal`, `autocompact_state` and ~20 more subtypes do not count.
- **Ignore the unknown.** "Consumers should ignore types and subtypes they do not recognize: the set grows over time" (`d312:5591`, `cli:109875`).

### 2.1 Members of the `SDKMessage` union (0.3.312: 39 members, `d312:5594`)

Entry form: "When", "Fields", "Edge cases", "Extension". No "Edge cases" item — I did not look for separate branches for the type beyond what "When" says. No "Extension" item — the extension host forwards the type to the webview unprocessed (`io_message`, `ext:125715`). Exceptions in the host: `hook_*` except `SessionStart`/`Setup` are dropped (`ext:123961`), `bridge_state` and `session_title_changed` are handled and not forwarded (`ext:125642`–`ext:125652`, `ext:125708`–`ext:125712`); at the SDK level `keep_alive`, `transcript_mirror`, `session_state_changed` with `sdk_host_only` and all `control_*` are swallowed (`ext:110214`–`ext:110294`).

### `assistant`
- **When:** every model response. The converter `Xge` cuts the message into separate `assistant`s — one per content block (`for (let y of km([e]))`, `cli:723743`; the split is `lTs`, `cli:588845`–`cli:588852`; `Xge` imports `km` from the module `cli:394416`–`598147`, where it is defined at `cli:588808`, `cli:721664`). A subagent's response comes with the `parent_tool_use_id` of the `Agent` call.
- **Fields** (`cli:107133`): `message`, `parent_tool_use_id[n]`, `error[?]`, `request_id[?]`, `supersedes[?]`, `aborted[?]`, `user_message_uuid(s)[?]`, `resume_reason[?]`, `agent_id[?]`, `subagent_type[?]`, `task_description[?]`, `timestamp[?]`, `context_usage[?]`, `usage_report[?]`, internal `thinking_display`, `thinking_duration_ms`, `tool_use_meta`, `is_api_error_message`, `api_error*`, `local_command_*`, `attribution_*` and others (47 in all).
- **Edge cases:** an API error arrives as `assistant` with `error` and the error text, while `result` is then `success` with `is_error:true` (`cli:768983`–`cli:768990`). The output of a local slash command also arrives as an `assistant` "twin" (`Mtn`, `cli:768813`–`cli:768821`). From subagents, tool calls and results go into the stream, and their text and thinking only with `initialize.forwardSubagentText` (`zHt`, `cli:725265`–`cli:725272`; for second-level subagents — `Tje`, `cli:724979`–`cli:724986`; the condition `s` in `zHt` was not traced).
- **Extension:** forwards it to the webview as is (`io_message`, `ext:125715`); counts `Edit/Write` edits as accepted by the pair `tool_use` → `tool_result` (`IVe`, `ext:123951`).

### `user`
- **When:** tool results (`tool_result` + `tool_use_result`), synthetic messages of the loop (`isSynthetic`), subagent messages with `parent_tool_use_id`.
- **Fields** (`cli:106665`, `cli:106813`): `message`, `parent_tool_use_id[n]`, `isSynthetic[?]`, `tool_use_result[?]`, `priority[?]`, `origin[?]`, `shouldQuery[?]`, `timestamp[?]`, `client_composed[?]`, `pasted_content[?]`, `inline_pastes[?]`, `agent_id[?]`, `subagent_type[?]`, `task_description[?]` and 20 internal ones.
- **Edge cases:** a call that gave way to a user message (currently `WebFetch`/`WebSearch`) carries `tool_use_result: {detachedToolCall: true}` — the call is still running (`d312:6569`–`d312:6572`).
- **Extension:** forwards as is (`ext:125715`).

### `user` with `isReplay: true` (`SDKUserMessageReplay`)
- **When:** only with `--replay-user-messages`. Three sources: (a) a repeated frame with the same `uuid` — the CLI does not execute it but answers with an echo (`cli:1512487`–`cli:1512498`); (b) a message from the queue folded into the running turn — the echo is born from the `queued_command` attachment at the moment of folding (`vAr`, `cli:723351`–`cli:723364`, `cli:1489207`–`cli:1489210`); (c) the echo of a local command and of the compaction summary (`CC`, `cli:1489282`–`cli:1489293`): the summary has `isReplay:false`.
- **Fields** (`cli:106845`): everything from `user` + `uuid`, `session_id`, `isReplay: true`, `file_attachments[?]`; the echo from the queue also has `isSynthetic` (if the command is a service one) and `origin`.
- **Edge cases:** the echo of a turn's first prompt comes not on `command_lifecycle started` but right before the first `stream_event message_start` (without partial — before the first `assistant`), 1.1–3.6 s after `started`; for a local command — after its `assistant`, before `result` (live, 24 runs, `p7-first-echo` and others). The echo's `uuid` is the one sent; for a frame without a `uuid` — the one the CLI gave the record in the transcript, and such a turn has no `command_lifecycle` and no `user_message_uuid(s)` in `result`. Without `--replay-user-messages` there is no echo. In the code: the echo candidates are `Fi = mA(Er)` in the turn generator (`cli:1497448`; `mA` takes `user` without `toolUseResult`, non-service ones, and `compact_boundary`, `cli:1495783`–`cli:1495789`), the shape is `gA` (`cli:1495817`–`cli:1495831`, `uuid` — the one written to the transcript, `isSynthetic: uXe(e)`); the release is lazy, once, before the first `assistant`, `user`, `compact_boundary` or non-`ping` `stream_event` (`cli:1497664`–`cli:1497667`, `cli:1498005`–`cli:1498011`), and in a turn without a model (a local command, `shouldQuery:false`) — right after `system/init` (`cli:1497514`). Before `status requesting` the echo goes out only on a remote transport. The turn's `user_message_uuids` are also carried by `system/init`, `system/status requesting`, `stream_event`, `assistant`, `result`; `system/thinking_tokens` carries the `user_message_uuid` of the turn's first message, in the second iteration as well.
- **Extension:** forwards as is.

### `result/success`
- **When:** the end of every turn, including a turn without a model (`cli:1497509`–`cli:1497553`) and a local command.
- **Fields** (`cli:107485`, 89 fields): `duration_ms`, `duration_api_ms`, `is_error`, `num_turns`, `result`, `stop_reason[n]`, `total_cost_usd`, `usage`, `modelUsage`, `permission_denials`, `queued_turn_count[?]`, `structured_output[?]`, `deferred_tool_use[?]`, `terminal_reason[?]`, `result_index[?]`, `user_message_uuid(s)[?]`, `resume_reason[?]`, `origin[?]`, `subagent_stats[?]`, `safety_stops[?]`, `fast_mode_state[?]`, `api_error_status[?n]`, the timings `ttft_ms`, `first_*_ms` and ~50 internal metrics.
- **Edge cases:** `total_cost_usd` and `modelUsage` are cumulative since the process started: read the latest, do not add them up (`d312:5958`–`d312:5962`); `usage` covers only the main loop of this turn. `success` can come with `is_error:true` when the turn ended with an API error (`cli:768983`–`cli:768990`). On a deferred tool call `RC` rewrites it into `is_error:false, stop_reason:"tool_deferred", result:""` (`cli:1489233`–`cli:1489244`). `queued_turn_count > 0` — after this result another turn will come without new input (`d312:5975`). `terminal_reason` — 19 values (`d312:10128`), absent for a local command (`HA`, `cli:107397`–`cli:107399`).
- **Extension:** `lv()` singles out a turn without a model (`num_turns === 0 && origin.kind === "task-notification"`, `ext:114647`) and does not count it as a finished turn (`ext:125674`–`ext:125677`); the SDK marks `modelLessTurnEnded` (`ext:110277`).

### `result/error_during_execution`, `result/error_max_turns`, `result/error_max_budget_usd`, `result/error_max_structured_output_retries`
- **When:** `error_max_budget_usd` — `--max-budget-usd` is exhausted (`cli:768946`–`cli:768951`); `error_max_turns` — `--max-turns` (`cli:768954`–`cli:768963`); `error_during_execution` — an exception in the turn, a setup failure (`terminal_reason:"turn_setup_failed"`, `cli:768540`–`cli:768552`), a turn interrupted before the prompt check (`cli:1497541`–`cli:1497545`), a zero result on a known startup error (`d312:5981`); `error_max_structured_output_retries` — `--json-schema` (`cli:1497873`, `cli:1498081`).
- **Fields** (`cli:107795`): as in `success`, but instead of `result` — `errors: string[]`, plus `startup_failure_reason[?]`.
- **Edge cases:** an interrupted turn (`terminal_reason` `aborted_streaming` or `aborted_tools`) as a rule closes with `error_during_execution` with a description in `errors`; if the turn's last message is deemed finished (`mIs`) — with `success` (`cli:768891`–`cli:768900`). The `interrupt` receipt arrives before this result (`d312:4848`, the "Ordering" paragraph).
- **Extension:** the SDK remembers the error text (`ext:110261`–`ext:110276`).

### `system/init` (`SDKSystemMessage`)
- **When:** at the start of **every** turn, including a turn without a model: `yield HA(...)` in the turn handler (`cli:1497286`, `cli:1497497`), assembled by `npt` (`cli:747367`).
- **Fields** (`cli:107940`): `cwd`, `tools`, `mcp_servers[?]`, `model`, `permissionMode`, `slash_commands`, `output_style`, `skills`, `agents[?]`, `apiKeySource`, `betas[?]`, `claude_code_version`, `capabilities[?]`, `fast_mode_state[?]`, `effort[?n]`, `plugin_errors[?]` and the internal `startup_timing`, `memory_paths`, `scratchpad_path`, `worker_epoch` and so on.
- **Edge cases:** `capabilities` in 2.1.312 — `interrupt_receipt_v1`, `interrupt_cancel_queued_v1`, `interrupt_send_now_v1`, `interrupt_requeue_taken_v1`, `msg_lifecycle_v1`, `request_marker_lists_v1`, `ask_user_question_unanswered_v1` plus conditional ones (`cli:747283`–`cli:747310`, the list `Fks` — `cli:747310`). The `manual` mode is shown as `"default"` (confirmed live, `hqc:docs/SPEC.md:2301`–`hqc:docs/SPEC.md:2303`).
- **Extension:** fixes the session's `session_id` from `init` (`ext:125626`–`ext:125629`).

### `stream_event` (`SDKPartialAssistantMessage`)
- **When:** only with `--include-partial-messages` (`cli:1489249`–`cli:1489250`): raw Messages API events as they are generated.
- **Fields** (`cli:108096`): `event`, `parent_tool_use_id[n]`, `ttft_ms[?]`, `user_message_uuid(s)[?]`, `resume_reason[?]`, internal `thinking_display`, `api_message_id`, `abandoned_blocks`, `text_runs_joined`.
- **Edge cases:** thinking pieces (`thinking_delta`) come only with the `summarized` display (5.11).
- **Extension:** the extension turns on partial only outside a Remote window (`ext:138050`).

### `system/compact_boundary`
- **When:** the boundary of context compaction, both on `/compact` and on auto-compaction (`cli:768744`, `cli:1489317`–`cli:1489324`).
- **Fields** (`cli:108153`): `compact_metadata[i?]` (the trigger, tokens before), `logical_parent_uuid[i?n]`, `historical[i?]`.
- **Edge cases:** see 5.7.
- **Extension:** forwards as is.

### `system/status`
- **When:** `status:"compacting"` at the start of compaction, `null` at the end (`sdk_status`, `cli:768731`–`cli:768738`); `status:"requesting"` before every API request — only with partial (`cli:768740`, `cli:1489167`); `status:null, permissionMode` — on a permission mode change (`Rd`, `cli:1504860`–`cli:1504866`).
- **Fields** (`cli:108222`): `status` (`compacting` | `requesting` | `null`, `cli:106504`), `permissionMode[?]`, `compact_result[?]` (`success`/`failed`), `compact_error[?]`, `compact_output_chars[i?]`.
- **Edge cases:** `compact_output_chars` exists only for auto-compaction; a typed `/compact` does not have it (`cli:108230`–`cli:108234`).
- **Extension:** itself mixes into the webview a `status` with `userPromptSubmitHooksRunning`, which the CLI does not send (`ext:125659`–`ext:125664`).

### `system/api_retry`
- **When:** an API request failed with an error that the CLI will retry (the twin of `api_error`, `cli:768796`–`cli:768805`). The second source is the "heartbeat" of waiting for a rate limit for requests not on the main thread: `k$` builds `api_retry` with `error_status:429, error:"rate_limit"` (`cli:1502937`–`cli:1502955`, call `cli:1502915`, gate `tengu_rustling_sunset`, off on the gateway).
- **Fields** (`cli:108426`): `attempt`, `max_retries`, `retry_delay_ms`, `error_status[n]` (`null` — no HTTP response), `error`, `no_response[?]` (`waited_ms`, `retry_wait_ms`).
- **Edge cases:** retries inside a subagent do not land here — they arrive as `tool_progress` with `subagent_retry` (`cli:723781`).
- **Extension:** forwards as is.

### `system/control_request_progress`
- **When:** the progress of a long control request from the client; currently only `side_question` (`cli:1508441`, `d312:5251`).
- **Fields** (`cli:108446`): `request_id`, `status` (`started` | `api_retry`), retry counters.

### `system/model_refusal_fallback`
- **When:** the model refused, and the CLI switched to a fallback model (`cli:768750`–`cli:768768`).
- **Fields** (`cli:108462`): `trigger:"refusal"`, `direction`, `scope[?]`, `original_model`, `fallback_model`, `request_id[n]`, `api_refusal_category[?n]`, `api_refusal_explanation[?n]`, `retracted_message_uuids[?]` (which already shown messages are retracted), `refused_user_message_uuid[?n]`, `content`.
- **Edge cases:** `RC` drops the internal `provisional` (`cli:1489169`–`cli:1489171`).

### `system/model_refusal_no_fallback`
- **When:** a refusal without a fallback model (`cli:768769`–`cli:768779`). **Fields** (`cli:108511`): `original_model`, `request_id[n]`, `api_refusal_*`, `refused_user_message_uuid[?n]`, `content`.

### `system/local_command_output`
- **When:** the schema exists (`cli:108697`), but the sender exists neither in the extracted code nor as a string literal in the binary (`"local_command_output"`: 2 occurrences in `cli-2.1.312.pretty.js` and 2 in the raw `~/.local/share/claude/versions/2.1.312`, both are the schema and the parsing at the receiver `cli:1340689`). In 2.1.312 a local command's output arrives as `assistant` (`cli:768813`–`cli:768821`). Live (`p4-local-commands`): on `/cost`, `/context`, `/model`, `/usage` — 0 `local_command_output` frames; each answers with a triple: `assistant` (`stop_reason: end_turn`, the output text) → `user` echo with `<command-name>…` → `result success` with `num_turns:0`, `duration_api_ms:0`. `/cost` in 2.1.312 is a synonym of `/usage`. The output of `/compact` is not `assistant` but a `user` echo with `<local-command-stdout>Compacted </local-command-stdout>` (`p10-autocompact`, line 70).

### `system/hook_started`, `system/hook_progress`, `system/hook_response`
- **When:** the start, the output and the end of a hook (`cli:436634`–`cli:436710`). Without `--include-hook-events` — only for `SessionStart` and `Setup` (`T6s`, `cli:333615`); `UserPromptSubmit` — with `CLAUDE_CODE_EMIT_USER_PROMPT_SUBMIT_HOOK_EVENTS` (`cli:436631`–`cli:436633`, `cli:436708`).
- **Fields:** `hook_id`, `hook_name`, `hook_event` (`cli:108702`); `progress` also has `stdout`, `stderr`, `output` (`cli:108713`); `response` has `output`, `stdout`, `stderr`, `exit_code[?]`, `outcome` (`cli:108727`).
- **Extension:** lets only `SessionStart`/`Setup` events into the webview (`DVe`, `ext:123961`; `b3`, `ext:50679`); counts unfinished `UserPromptSubmit` hooks itself (`LVe`, `ext:123968`).

### `system/plugin_install`
- **When:** only with `CLAUDE_CODE_SYNC_PLUGIN_INSTALL` and stream-json (`cli:1506589`–`cli:1506600`). **Fields** (`cli:108743`): `status`, `name[?]`, `error[?]`.

### `tool_progress`
- **When:** (a) the "heartbeat" of a long tool call on the main thread — every 30 s (`cVr = 30000`, `cli:550261`; the timer is `kNr`, `cli:550262`; for subagents it is not set up — `s.agentId ? () => {} : kNr(...)`, `cli:552244`–`cli:552246`), `heartbeat: true` (`cli:723768`–`cli:723778`); (b) an API retry inside a subagent — `subagent_retry` (`cli:723779`–`cli:723790`).
- **Fields** (`cli:108756`): `tool_use_id`, `tool_name`, `parent_tool_use_id[n]`, `elapsed_time_seconds`, `task_id[?]`, `heartbeat[?]`, `subagent_type[?]`, `subagent_retry[?n]`.
- **Edge cases:** in the "heartbeat", `tool_use_id` on the wire is `<call id>-heartbeat-N` (`cli:550277`), and `parent_tool_use_id` is the id of the call itself, although this is the main thread, not a subagent: telling "this is a subagent" by `parent_tool_use_id` goes wrong here (live run `p3p5-bash-heartbeat`, lines 33–34: `toolu_…-heartbeat-0` at 30 s, `-heartbeat-1` at 60 s). The engine yields the main thread's Bash progress (`bash_progress`) only as a `tool_progress` "twin", which `RC` drops (`cli:768822`–`cli:768831`, `cli:1489254`): live, a Bash that printed a line every 5 s for 70 s gave only two "heartbeats", and the output came as one `tool_result` at the end. A subagent's Bash has no "heartbeats" (32 s, `t2-subagent-progress`, 0 frames).

### `auth_status`
- **When:** only with `--enable-auth-status` (the extension passes it): the progress of signing in to the account (`cli:1505025`, `cli:1513252`). **Fields** (`cli:108778`): `isAuthenticating`, `output`, `error[?]`.

### `system/task_notification`
- **When:** a task (a background command, a subagent, a monitor, a workflow) reached a final state. The sender `ca()` emits the notification once per "terminal claim" of the task (`s8r`, `cli:138697`–`cli:138700`; `ca`, `cli:138872`–`cli:138892`); the transition to a final state in the registry calls it itself (`cli:332785`–`cli:332814`).
- **Fields** (`cli:108807`): `task_id`, `run_id[?]`, `tool_use_id[?]`, `status` (`completed`/`failed`/`stopped`), `reason[?]`, `output_file`, `summary`, `usage[?]`, `resource_links[?]`, the internal `handback` (`send`/`flagged`/`withheld`, `cli:104998`) and `handback_report`, `skip_transcript[?]`, `ambient[?]`.
- **Edge cases:** one task can send several notifications — after an interim report the claim is released, and the next start gets a new `run_id` (5.6). `handback_report` should be shown instead of `summary`; the interim `withheld` has no `handback` field (`cli:108828`, `cli:728549`). A subagent stopped by an interrupt without `perTaskStopAffordance` gets `stopped` with `summary` = the description (`cli:742344`). The report via `SubagentHandback` is on in the `auto` permission mode unless `CLAUDE_CODE_SENDMESSAGE_HANDBACK` turns it off (by default the flag `tengu_lively_waffle` = `true`, `cli:194998`–`cli:195005`); live run `x1-handback` (`auto`): `handback: send`, `summary` — "This agent's report was delivered to you as a message from "…" (its SubagentHandback call). Read it there; it is not repeated here.", `handback_report: {text}` — the report itself; right after, the CLI opens a turn by itself, and the agent retells the report.
- **Extension:** forwards as is.

### `system/task_started`
- **When:** a task is registered (`vo`, `cli:332849`–`cli:332914`) — and **again** for the same `task_id` if the task came alive again after the terminal claim (`cli:332850`–`cli:332851`, `cli:332885`–`cli:332895`).
- **Fields** (`cli:108844`): `task_id`, `run_id[?]`, `tool_use_id[?]`, `description`, `subagent_type[?]`, `is_backgrounded[?]`, `awaited[i?]`, `spawn_depth[?]`, `parent_task_id[?]`, `task_type[?]`, `workflow_name[?]`, `prompt[?]`, `skip_transcript[?]`, `ambient[?]`. The wire also carries `owned_by_subagent` (`true` for a Bash started by a subagent, `cli:332830`–`cli:332833`, `cli:332899`) — it is in neither the schema nor the d.ts.
- **Edge cases:** do not show tasks with `skip_transcript`/`ambient` in the feed (`d312:6410`–`d312:6418`).

### `system/task_updated`
- **When:** one of the task's fields changed (`cli:332809`–`cli:332812`). **Fields** (`cli:108881`): `task_id`, `run_id[?]`, `patch` — only the changed `status`, `description`, `end_time`, `total_paused_ms`, `error`, `is_backgrounded` (`Ao`, `cli:332815`–`cli:332828`).
- **Edge cases:** moving a call to the background arrives as `patch.is_backgrounded:true` (`d312:6390`).

### `system/task_progress`
- **When:** a subagent's progress (`aye`, `cli:581928`–`cli:581945`). **Fields** (`cli:109274`): `task_id`, `run_id[?]`, `tool_use_id[?]`, `description`, `subagent_type[?]`, `usage`, `last_tool_name[?]`, `summary[?]`.
- **Edge cases:** "Summaries are not a heartbeat" — a pause in the summaries does not mean the subagent has stalled (`d319:2162`).

### `system/background_tasks_changed`
- **When:** the list of background tasks changed (`cli:770640`–`cli:770646`, `cli:770853`–`cli:770854`). **Fields** (`cli:108901`): `tasks[?]` — a snapshot of the list.
- **Extension:** the number of non-ambient tasks (`AVe`, `ext:123912`) decides whether the session counts as "at work" (`FQ`, `ext:123862`–`ext:123881`).

### `system/thinking_tokens`
- **When:** an estimate of the number of thinking tokens as the stream goes (`cli:768702`–`cli:768713`). **Fields** (`cli:109294`): `estimated_tokens`, `estimated_tokens_delta`, `user_message_uuid[?]`.

### `system/session_state_changed`
- **When:** a change of state `idle` / `running` / `requires_action`, but only if the environment has `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS` (a frame for the client) or `CLAUDE_CODE_SDK_READS_SESSION_STATE` (the same frame with `sdk_host_only:true`) (`cli:390593`–`cli:390595`).
- **Fields** (`cli:108945`): `state`; the wire also carries `sdk_host_only` (it is not in the schema).
- **Extension:** the SDK uses it for the end of a run and swallows the frame with `sdk_host_only` (`ext:110287`–`ext:110294`).

### `system/worker_shutting_down`
- **When:** the Remote Control v2 bridge worker shuts down (constructor `$5o`, `cli:320733`–`cli:320735`): both calls write to the bridge transport (`cli:1064984`, `cli:1066437`), not to stdout. **Fields** (`cli:108969`): `reason`. It does not come in local `-p`.

### `system/commands_changed`
- **When:** the list of slash commands changed (`cli:1507278`–`cli:1507283`). **Fields:** `commands`.
- **Extension:** updates the command cache (`ext:125682`–`ext:125690`).

### `system/notification`
- **When:** a UI notification from the loop (`cli:768716`–`cli:768726`) and from other places (`cli:1486975`, `cli:1487048`). **Fields** (`cli:109259`): `key`, `text`, `priority`, `color[?]`, `timeout_ms[?]`.

### `system/files_persisted`
- **When:** the sender was not found: the extracted code has only the schema (`cli:108788`) and the subtype lists (`cli:1250325`, `cli:1439090`, `cli:1503394`); the string `"files_persisted"` occurs in the raw binary 4 times, as in the extracted code; in 33 live runs on the test bench — 0 frames. **Fields:** `files`, `failed`, `processed_at`.

### `tool_use_summary`
- **When:** a short summary of a group of tool calls (`mzs`, `cli:593896`; the twin — `cli:768791`). **Fields** (`cli:109311`): `summary`, `preceding_tool_use_ids`.

### `system/memory_recall`
- **When:** the `relevant_memories` attachment in the loop (`cli:766795`–`cli:766802`, the twin `cli:768807`–`cli:768812`). **Fields** (`cli:109325`): `mode`, `memories[?]`. The condition is `ef() && mie()`, where `mie()` is the flag `tengu_moth_copse` or a non-empty `CLAUDE_MEMORY_STORES` (`B2s`, `cli:527312`–`cli:527345`; `cli:126961`–`cli:126966`). Live: with only the project's memory files there is no frame (the flag is `false` on the test bench, `p6-memory-default`); with `CLAUDE_MEMORY_STORES` — `{mode:"select", memories:[{path, scope:"personal"}]}` in the second iteration of the turn, between `tool_result` and the next `status requesting`, and the key `team` appears in `init.memory_paths` (`p6-memory-stores`, line 65).

### `rate_limit_event`
- **When:** the listener `NR` (`cli:1486662`–`cli:1486710`), subscribed in every `-p` session (`cli:1505033`), not the query loop (`RC` drops the raw `rate_limit_event`, `cli:1489258`); every trigger gives a frame (`ZHr`, `cli:462886`–`cli:462890`). It fires: (1) after the headers of every API response, if the limits object changed by deep comparison (`cli:324136`–`cli:324162`, `cli:324223`) or for any window (`five_hour`, `seven_day`, `seven_day_overage_included`) the key `rounded percent @ resets_at : status : surpassedThreshold` changed (`cli:323738`–`cli:323740`, `cli:324224`–`cli:324238`) — that is, usage growing by 1 percentage point gives a frame with no status change; also on a 429 response, a reset "not a subscriber → allowed", forgetting and restoring windows (`cli:324110`–`cli:324195`, `cli:324336`); (2) a repeated refusal — under the flag `tengu_gravel_chorus` (default `true`), not on the gateway, with the same `resetsAt` no more than once per 30 s (`cli:1352696`–`cli:1352712`); (3) a change of the credits offer and of the request to the administrator (`cli:1306109`, `cli:1486379`, `cli:1486694`–`cli:1486712`). Live on the test bench — 28 frames over 33 runs, all `allowed`.
- **Fields:** `rate_limit_info` (`cli:106863`: `status`, `resetsAt`, `rateLimitType`, `utilization`, `overage*`, `unifiedWindows[i]` and others).
- **Extension:** shows the limit windows from `unifiedWindows` on the panel (`ext:125724`–`ext:125729`).

### `system/elicitation_complete`
- **When:** an MCP server confirmed the end of an input request (`cli:1494366`). **Fields:** `mcp_server_name`, `elicitation_id`.

### `system/permission_denied`
- **When:** a tool call was denied by a rule, by a mode or by the host's answer (`emitPermissionDenied`, `cli:1468181`–`cli:1468195`; `cli:768587`). **Fields** (`cli:109361`): `tool_name`, `tool_use_id`, `agent_id[?]`, `decision_reason_type[?]`, `decision_reason[?]`, `message`.

### `prompt_suggestion`
- **When:** after a turn, if suggestions are on (`initialize.promptSuggestions`) (`cli:1486897`). **Fields:** `suggestion`. The extension does not ask for suggestions (`ext:138052`).

### `system/mirror_error`
- **When:** an error of the transcript mirror (`--session-mirror`) (`cli:1087039`–`cli:1087041`). **Fields:** `error`, `key[?]`.

### `system/informational`
- **When:** service lines for the feed (17 places, for example `cli:1485426`); `RC` converts the internal form with `timestamp` into the SDK form (`cli:1489187`–`cli:1489199`). **Fields** (`cli:108274`): `content`, `level`, `tool_use_id[?]`, `prevent_continuation[?]`, `tag[?]`.

### `conversation_reset`
- **When:** `/clear` in non-interactive mode (`cli:992320`–`cli:992333`). **Fields** (`cli:109433`): `new_conversation_id`, `trigger[?]`, `user_message_uuid[?]`. After it `total_cost_usd` is counted anew (`d312:5958`).

### 2.2 What the CLI sends beyond the `SDKMessage` union

The stdout schema inside the CLI (`Oo`, `cli:109811`–`cli:109877`, 61 members) contains 22 types that are not in the d.ts union; 9 more types go to stdout bypassing `Oo`. The entry format is the same, shorter.

### `command_lifecycle`
- **When:** the fate of every incoming message with a `uuid` (a prompt, a slash command). Forwarding is set up once per transport, except the remote one (`aV`/`dV`, `cli:1503342`–`cli:1503358`); a repeated `queued` with `ran_ahead:true` — for instant commands during a turn (`cli:1512829`–`cli:1512837`).
- **Fields** (`cli:109493`): `command_uuid`, `state` (`queued`, `started`, `completed`, `cancelled`, `discarded`, `refused`), `wakeup_source[?]` (`loop_wakeup`/`schedule_wakeup`), `scheduled_for[?]`, `ran_ahead[i?]`.
- **Edge cases** (the schema description, `cli:109518`): a message without a `uuid` gets no events; `cancelled` comes from `cancel_async_message`, from `interrupt` with `cancel_queued`, and also when the turn the message was folded into is interrupted or fails — do not resend blindly, check against the responses to your own cancel requests and against `cancelled` in the `interrupt` receipt; `discarded` — the session was closed by `end_session` while the message waited; `refused` — a refusal by the intake policy, without `queued`; messages folded into turns that ended with `max_turns`/`hook_stopped`/`tool_deferred`/`background_requested` get `completed`, although their content may not have run. Declared by the capability `msg_lifecycle_v1` (`cli:747284`). For every incoming `control_response` with a `uuid` the CLI itself answers `completed` (`cli:1467782`–`cli:1467804`).
- **Extension:** keeps a set of pending `uuid`s (`xVe`, `ext:123896`–`ext:123911`) and by it decides whether the turn has ended (`FQ`, `ext:123862`).

### `system/session_title_changed`
- **When:** the session got a title or changed it — an auto-title or `rename_session` (subscription `Vhs`, `cli:1476948`–`cli:1476953`). **Fields:** `title`.
- **Extension:** applies the title and does not forward the frame to the webview (`ext:125708`–`ext:125712`).

### `system/turn_preempted`
- **When:** only if the client declared `rapidFollowupPreempt` and the flag is on: a turn that has shown nothing yet is stopped for the sake of a new message (`cli:1482713`). **Fields** (`cli:109606`): `reason` (`rapid_followup`), `preempted_by_uuid`, `preempted_message_uuids`.

### `system/inherited_asks_settled`
- **When:** a new process of the same session has sorted out the questions inherited from the previous worker (`cli:1492393`–`cli:1492397`). **Fields:** `kept_request_ids`.

### `system/instruction_size_warning`
- **When:** the instruction files (`CLAUDE.md` and rules) exceeded the limit (`cli:1495543`). **Fields** (`cli:108370`): `total_chars`, `total_limit_chars`, `file_count`, `largest_chars[?]`; the wire also carries `limit_chars`, `files_over_limit` (`cli:1495538`–`cli:1495541`).

### `system/permission_check_status`
- **When:** a long permission check of a call (the classifier) — its start and end (`cli:452704`–`cli:452712`). **Fields** (`cli:109388`): `tool_use_id`, `agent_id[?]`, `status`.

### `system/per_turn_effort_changed`
- **When:** the one-off effort of a turn turned off (`OZ`, `cli:494933`–`cli:494935`). **Fields:** `per_turn_effort_active`.

### `system/peer_message_hold`
- **When:** a message from another session is held or released by the intake policy (`cli:1500615`–`cli:1500625`). **Fields** (`cli:108595`): `state`, `message_uuid[?]`, `lane`, `from`, `from_name[?]`, `cause[?]`, `outcome[?]`.

### `system/feedback_draft_queued`
- **When:** the CLI prepared a feedback draft (`cli:744568`–`cli:744575`). **Fields:** `draft_id`, `draft_type`, `title`, `details_preview`.

### `system/code_change_published`, `system/vcs_state_changed`
- **When:** a PR or a branch was published (`oc`, `cli:358828`–`cli:358840`); the git state in the working folder changed (`cli:358862`). **Fields:** `provider`, `url`, `repo`, `identifier`, `action[?]`, `branch[?]` (`cli:109521`); `kind`, `cwd`, `branch[?]` (`cli:109567`).

### `system/dev_intent`
- **When:** the intent detector fired in a turn (`cli:990925`–`cli:990927`, `cli:991001`). **Fields:** `kind`, `trigger[?]`.

### `system/file_attachments_missing`
- **When:** the attachments of a message that came through the bridge were not found (`cli:1352361`–`cli:1352370`). **Fields** (`cli:109636`): `message_uuid`, `sent_count`, `missing`.

### `system/ui_invalidate`, `system/ui_log`, `system/ui_toast`, `system/ui_status`, `system/ui_panes`, `system/ui_scroll`, `system/ui_focus`
- **When:** plugin surfaces (`$.ui.*`) in a headless session: `cli:1506313`–`cli:1506317`, `cli:1507265`, `cli:1507409`–`cli:1507426`, `cli:1507503`–`cli:1507523`. All fields are internal (`cli:109053`–`cli:109242`). They also come in local `-p` with stream-json if a plugin's hooks module calls `$.ui.*`: the gate is `El()` = `outputFormat === "stream-json" && Vle(channel)` (`cli:1507233`–`cli:1507241`), plugin hooks modules are turned on by the flag `tengu_plugin_hooks_modules` (default `true`, `cli:209956`), and for `claude-vscode` — as long as `tengu_melodic_ember` is off (`svr()`, `cli:1466673`–`cli:1466675`). Live, `--safe-mode` gave `system/ui_invalidate {event:"ui.render"}` before the first turn (1.1). The requests to the host in 4.8 go by the same gate (`cli:1506366`–`cli:1506427`).

### `system/turn_handoff_available`, `system/cloud_session_delta`
- **When:** cloud workers and the cloud sessions client (`cli:1471282`–`cli:1471290`, `cli:1517185` — outside the `print.ts` module). They do not come in local `-p`: `turn_handoff_available` is written only by the remote transport `lU` (`cli:1473597`–`cli:1473598`), and it is created only with `--sdk-url` (`cli:1514055`–`cli:1514058`); `cloud_session_delta` — only with `--cloud` and `-p` (`writeDelta`, `cli:1517173`–`cli:1517193`; `cli:795266`–`cli:795275`).

### `system/post_turn_summary`, `system/task_summary`, `system/session_metadata`
- **When:** a summary after a turn (`cli:756361`–`cli:756370`); the outcome of the session's task and the artifacts — when the session metadata is written (`notifyMetadataChanged`, `cli:390640`–`cli:390650`). **Fields:** `summarizes_uuid`, `status_category`, `status_detail`, `needs_action` (`cli:108240`); `detail` (on the wire; the schema has no fields, `cli:108254`); `metadata` (`cli:108259`).
- **Extension:** the SDK passes them to the consumer in a separate branch (`ext:110246`–`ext:110252`).

### `system/model_fallback`, `system/model_consent_fallback`
- **When:** the main model is unavailable, and the CLI moved to the fallback one (`cli:768781`–`cli:768790`, in the loop — `cli:761495`); the same after the user's consent (`Lbo`, `cli:768790`). `RC` lets both through (`cli:1489183`–`cli:1489186`). **Fields** (`cli:108527`, `cli:108548`): `trigger`, `original_model`, `fallback_model`, `content`; the second one also has `choice`, `original_model_name[?]`, `persisted_as_default`.

### `transcript_mirror`
- **When:** only with `--session-mirror`: the lines appended to the transcript (`cli:1504868`–`cli:1504871`). **Fields:** `filePath`, `entries`. The SDK hands them to its batcher and does not show them to the consumer (`ext:110240`–`ext:110242`). Live run `x4-session-mirror`: a frame for every batch of appended records (`entries` are the transcript records themselves); after `/clear` the very next frame carries the path of the new file — earlier than the new `system/init`.

### `active_goal`, `autocompact_state`
- **When:** only with `CLAUDE_CODE_REMOTE` (`cli:1504978`–`cli:1504990`): the session goal and the auto-compaction state (`Lb`, `cli:1482999`–`cli:1483013`). **Fields:** `value` (`cli:109688`; `cli:109704`: `enabled`, `effective_window`, `threshold`, `enforced`, `source`). Local `-p` has none of them, so the auto-compaction threshold cannot be learned from there.

### `system/bridge_state`
- **When:** the state of the Remote Control bridge (`kk`, `cli:1505803`–`cli:1505812`). **Fields:** `state`, `detail`, `failure_kind` (on `failed`), `bridge_epoch`. Not in the schema.
- **Extension:** handles it itself and does not forward it to the webview (`ext:125642`–`ext:125652`).

### 2.3 What is in the schema but does not reach `-p` stdout

- Internal `system` messages of the transcript, born in the loop (`cli:593241`–`cli:593383`): `permission_retry`, `stop_hook_summary`, `memory_saved`, `agents_killed`, `away_summary`, `scheduled_task_fire`, `turn_duration`, `api_error` (`api_retry` comes instead of it), and also `thinking` and `file_snapshot` (`cli:460609`). `RC` drops them (`cli:1489201`–`cli:1489202`); they have no direct senders to stdout (none of the places that create them has `ea()`/`enqueue()` nearby).
- Raw engine types: `tombstone`, `api_metrics`, `os_notification`, `apply_flag_settings`, `set_expanded_view`, `set_in_progress_tool_use_ids`, `hint_clears`, `open_message_selector`, `compact_progress`, `stream_mode`, `response_length`, `refusal_continuation` (`cli:1489252`–`cli:1489275`).
- `system` / `session_writes_lost` — appeared in the 0.3.319 types (`d319:6274`); 2.1.312 does not have it (`"session_writes_lost"`: 0 occurrences both in the extracted code and in the raw binary).

### 2.4 Control envelopes in stdout

- `control_request` — a request from the CLI to the client, section 4 (`cli:114137`).
- `control_response` — the CLI's response to a client request: `{subtype:"success", request_id, response?}` or `{subtype:"error", request_id, error, error_code[i?], model[i?]}` (`cli:114162`–`cli:114196`); the response to `initialize` also has `pending_permission_requests`, `pending_user_dialog_requests`. With `--replay-user-messages` the CLI also **echoes** the client's incoming `control_response`s (`cli:1512337`–`cli:1512339`).
- `control_cancel_request` — the CLI withdraws its own request: `{request_id}` (`cli:114202`). It is sent when the wait is cut off (the turn is stopped, the question is cancelled) — `enqueueCancelRequest` in `sendRequest` (`cli:1468148`).
- `keep_alive` — an empty frame: every 30 s during five long control requests — `register_repo_root`, `fork_conversation`, `stage_file`, `add_directory`, `mcp_call` (`fc`, `cli:1508426`–`cli:1508435`; the calls — `cli:1508473`, `cli:1509854`, `cli:1510448`, `cli:1510477`, `cli:1510731`); before the response to `initialize` — `{"type":"keep_alive","startup_mark":…}` with `CLAUDE_CODE_SDK_READS_STARTUP_MARKS` (`cli:68222`–`cli:68228`, `cli:806996`–`cli:807003`); between turns, while background tasks run, — the timer `py`: period `JN = 30000` ms, flag `tengu_ccr_keepalive_after_stop_enabled` (`cli:1491110`–`cli:1491145`), armed on `interrupt` and `remote-cancel`. But the flag is read through `sv()`, which without `CLAUDE_CODE_REMOTE` returns `"off"` (`cli:509427`–`cli:509431`), and `arm()` returns at once (`cli:1491128`): in local `-p` there is no `keep_alive` between turns — live, 0 frames over ~95 s after `interrupt` while a background command was alive (`t5-p12-bg`; the flag is `true` on the test bench).

---

## 3. Client → CLI

### 3.1 What the CLI takes on stdin

Line parsing is `processLine` (`cli:1467685`–`cli:1467956`).

| Type | What it does |
| --- | --- |
| `user` | a user message (3.2) |
| `control_request` | a request to the CLI (3.3); without the `request` field **the process exits** ("Error: Missing request on control_request", `cli:1467892`) |
| `control_response` | a response to a CLI request (section 4); a response without a pending request goes to `settleUnknownResponse` (`cli:1467793`) |
| `control_cancel_request` | cancel a request the CLI is still handling: interrupts it (`remote-cancel`, `cli:1512340`–`cli:1512343`) |
| `keep_alive` | ignored (`cli:1467690`) |
| `update_environment_variables` | changes `process.env` only for allowed keys and only on transports that accept it (`cli:1467691`–`cli:1467767`); otherwise a refusal in the log |
| `assistant`, `system` | **history**: appended to the conversation without a turn (`Eft`, `cli:1512401`–`cli:1512413`); `assistant` is echoed with `--replay-user-messages` |
| `bash_command` | run a shell command as `!` in the terminal: `command`, `cwd[?]` (`cli:106846`, `cli:1512430`) |
| `session_notice` | a service notice from another session (`cli:1512359`–`cli:1512374`, schema `cli:114220`) |
| `queued_notification` | only on the remote transport; dropped on the local one (`cli:1512375`–`cli:1512383`) |
| `workflow_launch` | start a workflow (`cli:1512345`–`cli:1512357`) |
| anything else | "Ignoring unknown message type" in the log, the frame is dropped (`cli:1467869`) |

Invalid JSON on local stdio is **fatal**: `exitOnFatalInput` (`cli:1467939`); a `user` whose `message.role` is not `user` is fatal too on the local transport (`cli:1467900`–`cli:1467902`), and on the remote one it is either repaired or dropped.

### 3.2 A user message

```json
{"type":"user","uuid":"<uuid>","parent_tool_use_id":null,
 "message":{"role":"user","content":[{"type":"text","text":"…"},
   {"type":"image","source":{"type":"base64","media_type":"image/png","data":"…"}}]},
 "priority":"next"}
```

- `message` — the Messages API `MessageParam`: a string or `text`, `image`, `document`, `tool_result` blocks (`d312:6564`–`d312:6567`). An image: `source` is `base64` (`image/jpeg|png|gif|webp`), `url` or `file` (Files API) (`cli:105052`–`cli:105060`); a document — `application/pdf`, `text/plain`, `url`, `file` (`cli:105061`–`cli:105065`).
- `uuid` — the key to everything: the echo, `command_lifecycle`, `result.user_message_uuid(s)` and cancellation (`cancel_async_message`) go by it. Without a `uuid` the message is executed but not tracked and not listed in receipts (`cli:109496`, `d312:4848`). A repeat with the same `uuid` is not executed but confirmed with an echo (`cli:1512484`–`cli:1512498`).
- `parent_tool_use_id` — `null` for the main thread.
- `priority` — `now` | `next` | `later` (`cli:106676`). `next` is the default. `later` — after everything else. `now` during a turn: the CLI turns it into `next` marked `sentNow` and tries to deliver it at once — it moves the blocking commands to the background or interrupts the turn for the sake of the new message (`takeBeforeEnqueue`, `cli:1491720`–`cli:1491737`; `interruptForSubmit` — `cli:1491709`). The rule (by the code and live, `p9-*`): fast delivery takes over the frame only if there is a `uuid`, a turn is running, `origin.kind === "human"` (`Id`, `cli:149145`) and the conversation has an unclosed `tool_use`; it is turned on by the flags `tengu_velvet_panda` and `tengu_bubbly_meteor` (`wIo`, `cli:549359`; both `true` on the test bench). Then the movable call (Bash) goes to the background — `task_updated {is_backgrounded:true}` 3 ms after `queued`, `tool_result` "Command was moved to the background … so that a message that arrived while it was ru…" with `tool_use_result.backgroundedToDeliverMessage: true`, the echo of the new message, and the turn goes on with both `uuid`s in `user_message_uuids`; a foreground MCP call and a foreground subagent are moved the same way. Live run `p9-now-mcp-human`: a 30 s MCP call becomes a task for the first time at the moment it is moved — `task_started {task_type:"mcp_task"}` and `background_tasks_changed`, `tool_result` "MCP tool "slowtool/wait" was moved to the background as task … so that a message t…"; at its end — `task_notification completed` and a turn the CLI starts by itself. `p9-now-agent-human` (without `claude-vscode`, `run_in_background:false`): `task_updated {is_backgrounded:true}` on the running subagent, `tool_result` "Async agent launched successfully…". In both cases the message is folded into the same turn, and `user_message_uuids` holds both. Interruption or cancellation (`wi()`, `cli:1353055`–`cli:1353069`) remains for a call without a movable holder that runs longer than two checks of 200 ms (`qn = 2`, `yi = 200`); Bash, MCP and a subagent did not produce such a case. In all other cases the general path `PP` (`cli:1491608`–`cli:1491613`) applies — interrupting the turn: without `origin`, during Bash the command runs to its end, the turn closes with `result success`, `terminal_reason:"aborted_tools"`, `result:""`, and the first message gets `cancelled`; during the model's text — with `origin` and without — `result success`, `is_error:false`, `terminal_reason:"aborted_streaming"`, `stop_reason:null`, with the unfinished text in `result`. The new message goes as its own turn. The extension does not set `priority`, but it does set `origin {kind:"human"}` (`web:259692`).
- `shouldQuery: false` — append without a turn; the text will merge with the next message, the one that opens a turn (`cli:106703`–`cli:106707`).
- `client_composed: true` — the text was not typed by a person: no `@path` expansion, no slash commands and no turn-start attachments (`d312:6614`–`d312:6617`).
- `pasted_content`, `inline_pastes` — pasted text (`d312:6635`–`d312:6642`).
- `origin` — the source; a host that passes keyboard input should set `{kind:"human"}`, otherwise strict "this is a person" checks will not pass (`d312:5596`–`d312:5599`).
- Internal: `requeue_if_interrupted_within_ms` — put the message back into the queue if an `interrupt` crossed a turn that had already taken it (up to 10000 ms, `cli:106677`–`cli:106683`); `file_attachments` (`cli:106813`).

### 3.3 Client requests to the CLI (`control_request`)

Envelope: `{"type":"control_request","request_id":"<your id>","request":{"subtype":"…",…}}`. The response is one `control_response` with the same `request_id` (2.4). The dispatcher is one `for await` loop in `print.ts` (`cli:1508764`–`cli:1512336`) plus a table of simple handlers `Qe` (`cli:1477045`–`cli:1477066`). In all it knows **101** subtypes (the list — `vendor-src/scripts/dispatcher.txt`).

General response rules:
- An unknown subtype — `error` with the text `Unsupported control request subtype: <subtype>` (`Z7`, `cli:1339784`; the `else` branch, `cli:1512323`). This is also the answer to `set_color`, although it is in the d.ts union (`d312:5318`), and to `send_task_message` (its handler always throws this error, `cli:1477042`–`cli:1477044`).
- A process started with `--await-claim` and not yet claimed answers everything except `claim_session` and the allowed requests with the error `not_claimed: …` (`cli:1508801`–`cli:1508803`).
- After handling a request with a `uuid` the CLI sends `command_lifecycle completed` (`cli:1512325`).
- A slow handler is logged as `cli_control_request_handled` (`cli:1512329`–`cli:1512334`). There is no common deadline for a response; an internal deadline was found only for `reload_plugins` and `reload_skills`, and not always (`cli:1510614`–`cli:1510615`, `cli:1510699`–`cli:1510702`).

#### `initialize`
See 1.3. The branch is `cli:1508842`–`cli:1509044`. Shape errors are returned as `error` rather than crashing the process.

#### `interrupt`
- **Request** (`cli:110245`): `cancel_queued[?]`; internal `send_now`, `message_uuid`, `for_user_message_uuids`, `worker_epoch`, `reason`.
- **What it does** (`vr`, `cli:1508637`–`cli:1508707`): interrupts the running turn (`abort`); stops a deferred permission question (`np`, `cli:1508547`–`cli:1508552`); spares background subagents and workflows only with `perTaskStopAffordance` (`La`, `cli:1504650`–`cli:1504655`), otherwise stops them (`Kle`); with `cancel_queued:true` it also takes all messages with a `uuid` off the queue — each gets `command_lifecycle cancelled` (`d312:4836`).
- **Response** (`cli:110282`–`cli:110305`): `still_queued` — the `uuid`s of messages that will survive the interrupt and **will be executed**; `cancelled[?]` — the ones taken off (only with `cancel_queued`); internal `put_back` (returned to the queue from the taken turn), `send_now` (`stopped`/`delivering`/`nothing_waiting`), `turn` (`stopped`/`idle`/`spared`).
- **Edge cases** (`d312:4845`–`d312:4848`): the receipt is written before the `result` of the interrupted turn; the `still_queued` list is incomplete — messages without a `uuid` and task notifications are not visible in it, and an empty list does not mean "nothing will happen"; an interrupt in the window before the first turn "latches": the first turn that carries the doomed work starts already interrupted. The `turn` field comes only if the request had `for_user_message_uuids`; `idle` — there was no turn (`cli:110297`).

#### `end_session`
- **What it does** (`cli:1508826`–`cli:1508841`): sets `endSessionRequested`, interrupts the turn with the reason `shutdown`, flushes the queue (the waiting messages get `discarded`), answers `success` and leaves the loop — the process exits. `end_session` with `reason` "archived" from a previous life cycle is ignored (`cli:1508827`–`cli:1508829`).

#### `set_permission_mode`
- **Request** (`cli:110416`): `mode`; internal `ultraplan`, `from_settings`.
- **Response** (`cli:1509045`–`cli:1509072`): `{mode}` — the mode that actually took effect; an unknown mode — `error` with the code `invalid_mode`; a move to `auto` can be rejected by a check (`cli:1509057`–`cli:1509062`). After the change the CLI sends `system/status` with `permissionMode` (`cli:1504860`–`cli:1504866`).

#### `set_model`
- **Request** (`cli:110435`): `model[?n]` (`null` — the default model); internal `system_prompt`, `only_if_model_is_current`.
- **Response** (`cli:1509084`–`cli:1509106`): `{model}` — the model of the next turn; an error — `error` with `error_code` (`invalid_request`, `catalog_unknown`, `not_found`, … — `cli:114180`) and the `model` field — which model remained; an internal failure — `set_model failed`, code `internal_error`. It takes effect from the next turn.

#### `set_max_thinking_tokens`
- **Request** (`cli:110464`): `max_thinking_tokens[?n]`, `thinking_display[?n]` (`summarized`/`omitted`/`highlights`). `null` for the budget — return to the startup budget; `null` for the display — remove the override.
- **Response:** `success` or `error` "set_max_thinking_tokens: part of the request refused" (`NOn`, `cli:1357090`–`cli:1357094`); `highlights` is not accepted everywhere, the reason is in the error text, and the budget is applied anyway (`cli:1357104`, `cli:110470`).

#### `stop_task`, `background_tasks`, `get_task_output`
- `stop_task {task_id}` — stop one task, response `{}`; an error — "stop_task: task_id must be a string" or the reason from the stop attempt (`Nt`, `cli:1476997`–`cli:1477013`). A stopped subagent arrives as `task_notification stopped`.
- `background_tasks {tool_use_id?}` — move running Bash calls and subagents to the background: with `tool_use_id` — one, response `{backgrounded}`; without it — all, response `{}` (`Bt`, `cli:1477015`–`cli:1477022`).
- `get_task_output {task_id}` — the tail of the output of a background command or Monitor (the last 8 KiB): `{output, total_bytes, truncated}`; for a subagent — the error "no shell or Monitor task with that task_id" (`qt`, `cli:1477023`–`cli:1477040`, description `cli:112294`).

#### `cancel_async_message`
- **Request:** `message_uuid`. **Response:** `{cancelled: boolean}` (`cli:1509626`–`cli:1509630`). `false` if the message has already been taken off the queue, is already being folded into a turn (`isFoldInFlight`) or does not exist. A cancelled message gets `command_lifecycle cancelled`.

#### `rename_session`
- **Request:** `title`, `source[?]` (`remote`/`host`), `session_id[?]` (must match the current one). An empty title — the error "title must be non-empty" (`jt`, `cli:1476961`–`cli:1476975`). `session_title_changed` follows.

#### `get_context_usage`
- **Request:** `detail[?]` (`summary`/`full`). **Response:** the breakdown of the window by category (`Ct`, `cli:1476574`–`cli:1476579`; the response schema — `cli:107082`).

#### `rewind_files`
- **Request:** `user_message_id`, `dry_run[?]`. **Response:** `{canRewind, filesChanged, …}` or `error` "Failed to rewind…" (`cli:1509612`–`cli:1509625`). Requires `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING`.

#### `mcp_message`, `mcp_set_servers`, `mcp_status`
- `mcp_message {server_name, message}` — both ways: from the CLI to the client for servers that live in the SDK, and back; response `{mcp_response}` (`cli:111996`–`cli:112006`, branch `cli:1509599`).
- `mcp_set_servers` — replace the set of dynamic MCP servers (`cli:1510553`); `mcp_status` — the state of the connections (`cli:1508807`).

#### The dispatcher's other subtypes

"Line" is the subtype's first branch in the dispatcher; "Qe" is the table `cli:1477045`–`cli:1477066`. The descriptions are condensed from the CLI schema descriptions (`cli:109920`–`cli:113786`); for subtypes without a schema — from the name and the branch.

| Subtype | What it does | Line |
| --- | --- | --- |
| `side_question` | a "side" question without a turn; its progress is reported by `control_request_progress` | `cli:1511702` |
| `fork_conversation` | fork the conversation (no schema) | `cli:1509851` |
| `rewind_conversation` | roll the conversation back to `target_message_uuid` (no schema) | `cli:1509642` |
| `export_conversation` | the conversation text, as `/export` writes it | `cli:1509423` |
| `generate_session_title` | generate a title (no schema) | Qe |
| `add_directory`, `register_repo_root`, `set_cwd` | add a working folder; a repository root, with `CLAUDE.md` re-read; move to another folder (like `/cd`) | `cli:1510474`, `cli:1510471`, `cli:1509107` |
| `claim_session` | claim a "spare" `--await-claim` process | `cli:1509186` |
| `poll_event`, `turn_handoff`, `prefetch_attachments`, `stage_file` | cloud workers: events, turn handoff, attachment prefetch, file staging | `cli:1509631`, `cli:1512320`, Qe, `cli:1510426` |
| `seed_read_state`, `read_file`, `list_directory`, `get_workspace_diff`, `get_plan` | mark a file as read for `Edit`; read a file with the `Read` permissions; list a folder; git diff; the plan of plan mode | `cli:1510495`, `cli:1509868`, `cli:1509909`, `cli:1510401`, `cli:1510411` |
| `start_shell_task` | a background command of a cloud session | `cli:1509936` |
| `set_proactivity_level`, `set_prompt_suggestions_paused` | the agent's initiative level; pause suggestions | `cli:1509073`, `cli:1510938` |
| `apply_flag_settings`, `get_settings`, `update_settings` | a settings layer from flags; the resulting settings; writing settings | `cli:1508771`, `cli:1511526`, `cli:1511569` |
| `get_hooks_listing`, `list_permission_rules`, `set_mcp_permission_mode_override` | the list of hooks (`/hooks`); permission rules (`/permissions`); an MCP server's permission mode, tightening only | `cli:1511559`, `cli:1511595`, Qe |
| `reload_plugins`, `reload_skills`, `reload_output_styles` | re-read from disk | `cli:1510601`, `cli:1510697`, Qe |
| `get_session_cost`, `get_usage`, `list_models`, `get_binary_version`, `get_status` | cost; `/usage`; the model catalog; the version; the `/status` lines | Qe, Qe, Qe, Qe, `cli:1509397` |
| `file_suggestions` | autocomplete of `@` paths | `cli:1510484` |
| `get_memory_dialog`, `get_skills_dialog`, `get_sandbox_dialog`, `get_chrome_dialog` | data for the `/memory`, `/skills`, `/sandbox`, `/chrome` dialogs | `cli:1509486`, `cli:1509506`, `cli:1509588`, `cli:1509517` |
| `get_chrome_browsers`, `select_chrome_browser`, `set_chrome_browser_hints` | Claude in Chrome | `cli:1509531`, `cli:1510917` |
| `mcp_call` | call an MCP tool without a turn and without a permission check | `cli:1510708` |
| `mcp_read_resource`, `mcp_reconnect`, `mcp_toggle`, `mcp_tasks_changed` | a `ui://` resource; reconnect; on/off; a notification about MCP tasks | Qe |
| `mcp_authenticate`, `mcp_clear_auth`, `mcp_oauth_callback_url`, `channel_enable` | sign in to an MCP server, reset the sign-in, the OAuth address, enable a channel (no schemas) | `cli:1510948`, `cli:1511274`, `cli:1511083`, `cli:1510945` |
| `claude_authenticate`, `claude_oauth_callback`, `claude_oauth_wait_for_completion` | sign in to the Claude account (no schemas) | `cli:1511123`, `cli:1511240`, `cli:1511241` |
| `list_cloud_sessions`, `get_cloud_session`, `get_cloud_session_transcript` | the account's cloud sessions | `cli:1509447`–`cli:1509449` |
| `remote_control`, `remote_tools_announce`, `register_device_hooks`, `upload_device_hook_template` | Remote Control and the machines that serve a cloud session's tools | `cli:1511823`, `cli:1512286`, `cli:1512254`, — |
| `message_rated`, `submit_feedback`, `submit_survey_feedback`, `ultrareview_launch` | rating a message; `/feedback`; a survey; launching ultrareview | `cli:1511809`, `cli:1511609`, `cli:1511683`, `cli:1511774` |
| `ui_attach`, `ui_detach`, `ui_render`, `ui_press`, `ui_client_module`, `ui_client_press`, `ui_message`, `ui_client_fault`, `ui_input`, `ui_select`, `ui_panes`, `ui_pane_show`, `ui_pane_focus`, `ui_close`, `ui_scroll`, `ui_focus`, `ui_prompt_edit`, `ui_prompt_autocomplete` | remote plugin surfaces (all `@internal`) | `cli:1509971`–`cli:1510311` |

---

## 4. CLI requests to the client

All of them go through `sendRequest` (`cli:1468130`–`cli:1468180`): the CLI writes a `control_request` and waits for a `control_response` with the same `request_id`. **`sendRequest` has no timeout of its own**; the wait ends with a response, a cancellation (the turn's `AbortSignal` → the CLI sends `control_cancel_request`, `cli:1468141`–`cli:1468157`) or stdin closing ("Stream closed", `cli:1468139`). While it waits for a person's answer, the session is in `requires_action` (`beginUserDecision`, `cli:1468143`). The d.ts union has 4 of them (`can_use_tool`, `hook_callback`, `elicitation`, `request_user_dialog`); the CLI schema has 17 plus the two-way `mcp_message` (`upgrade_relay_marker`, listed in 4.7, is not a request but a kind of string in the `relay_marker` field of the client's `turn_handoff` request, `cli:111965`–`cli:111968`).

### 4.1 `can_use_tool`
- **When:** a tool call needs a decision, and the CLI was started with `--permission-prompt-tool stdio` (otherwise a refusal without a question). With `--permission-prompts none` — a local refusal, the host is not asked (`cli:1503611`–`cli:1503617`).
- **Request** (`cli:110311`): `tool_name`, `input`, `tool_use_id`, `permission_suggestions[?]`, `blocked_path[?]`, `decision_reason[?]`, `decision_reason_type[?]`, `title[?]`, `display_name[?]`, `description[?]`, `agent_id[?]` (a subagent's call), `mcp_server[?]`, `suppress_always_allow_rule[?]`, `default_to_no[?]`, `matched_ask_rule[?]`, `requires_user_interaction[?]`, `classifier_approvable[?]` and internal ones. Live, `requires_user_interaction: true` came on each of the 12 `AskUserQuestion` requests; on a Bash with a `permissions.ask` rule — `decision_reason_type: rule` and `decision_reason_rule_source`, without `matched_ask_rule`; `suppress_always_allow_rule` never appeared (`x2-perm-*`, `x2b-perm-*`, `x5-*`).
- **Allow:** `{"behavior":"allow","updatedInput":{…},"updatedPermissions":[…]}` — `updatedInput` is optional (empty → the original input), `toolUseID`, `decisionClassification` (`user_temporary`/`user_permanent`/`user_reject`) — at will (`cli:1458610`–`cli:1458616`). `updatedPermissions` with `setMode` changes the mode (`cli:1458682`). For `AskUserQuestion` the answers go into `updatedInput.answers` (confirmed live, `hqc:docs/SPEC.md:2376`, `hqc:docs/SPEC.md:5094`).
- **Deny:** `{"behavior":"deny","message":"…"}` — the model gets a refusal with this text and continues the turn.
- **Deny and interrupt:** `{"behavior":"deny","message":"…","interrupt":true}` — the CLI interrupts the turn (`JM(...).abort()`, `cli:1458710`–`cli:1458711`).
- **An error instead of a response:** `control_response` `subtype:"error"` or an exception → a refusal with the text `Tool permission request failed: <error>` (`VPo`, `cli:434061`–`cli:434067`; `cli:1459241`); a response of the wrong shape → the refusal "The canUseTool callback returned an invalid permission result. Expected {behavior: 'allow', …} or {behavior: 'deny', message: string}" (`cli:1459243`–`cli:1459247`, `cli:1458632`); the turn was interrupted during the wait → "Tool permission request aborted" (`cli:1459248`–`cli:1459251`).
- **No response:** the CLI waits as long as it takes. Open questions are listed in the response to `initialize` (`pending_permission_requests`, `d312:418`). The CLI drops a response to an already withdrawn question (`settleUnknownResponse`, `cli:1467793`).
- **Extension:** the SDK calls its own `canUseTool`; if the callback returned `null`, it leaves the request unanswered (`ext:110441`); on `control_cancel_request` it aborts the callback (`ext:110378`–`ext:110381`).

### 4.2 `hook_callback`
- **Request** (`cli:111481`): `callback_id` (from `initialize.hooks`), `input` (the hook's input), `tool_use_id[?]`; internal `issued_at`, `deadline_ms`.
- **Response:** the JSON of the hook output (like the stdout of a command hook: `decision`, `hookSpecificOutput` and so on); `{}` — change nothing. An error or an exception → "Error in hook callback" in the log, and the CLI treats the response as an empty `{}` (`cli:1468265`–`cli:1468268`). The timeout is the matcher's `timeout` from `initialize` (`createHookCallback`, `cli:1468251`–`cli:1468255`).

### 4.3 `mcp_message`
- **Request:** `server_name`, `message` (JSON-RPC). **Response:** `{mcp_response}`; for an unknown server the SDK answers with an error (`ext:110446`–`ext:110451`).

### 4.4 `elicitation`
- **When:** an MCP server asks the user for input (`handleElicitation`, `cli:1468460`–`cli:1468501`). It is not sent if stdin is closed, the turn is interrupted or `--permission-prompts none` is set — then `cancel` at once.
- **Request** (`cli:112560`): `mcp_server_name`, `message`, `mode[?]`, `url[?]`, `elicitation_id[?]`, `requested_schema[?]`, `property_order[?]`, `title[?]`, `display_name[?]`, `description[?]`.
- **Response** (`UQo`, `cli:112591`): `{action: "accept"|"decline"|"cancel", content?}`. An error or an exception → `cancel` (`cli:1468497`–`cli:1468498`). The session is in `requires_action` during the question.

### 4.5 `request_user_dialog`
- **When:** a tool asks the host to draw a blocking dialog (`requestUserDialog`, `cli:1468503`–`cli:1468545`); only for the kinds declared in `initialize.supportedDialogKinds`.
- **Request** (`cli:112596`): `dialog_kind`, `payload`, `tool_use_id[?]`.
- **Response** (`wkn`, `cli:112610`–`cli:112619`): `{behavior: "completed"|"cancelled", result?}`. The CLI **ignores** an `error` response — the dialog stays hanging (`cli:1467805`, `cli:1467561`). A kind the host did not declare must not be answered, and `cancelled` is a real refusal by the user (`d312:5276`).
- **No response:** a dialog can have a deadline (`parkTimeoutMs`); when it passes, the CLI cancels the dialog itself and sends `control_cancel_request` (`cli:1468518`–`cli:1468536`); without a deadline it hangs until a response or a withdrawal.

### 4.6 `oauth_token_refresh`, `host_auth_token_refresh`, `remote_control_work_secret`
- The host holds the credentials: a fresh OAuth token after a 401, the provider key, the bridge work secret (`cli:112709`, `cli:112734`, `cli:112696`). Without the matching callback the SDK answers with the error "… callback is not provided" (`ext:110491`–`ext:110510`). They also come in local `-p` if the host has set the variables: `oauth_token_refresh` — with `CLAUDE_CODE_SDK_HAS_OAUTH_REFRESH` and the entry point `claude-desktop`/`local-agent`/`claude-vscode` (`cli:1503603`, `cli:145483`, `cli:145567`–`cli:145569`); `host_auth_token_refresh` — with `CLAUDE_CODE_SDK_HAS_HOST_AUTH_REFRESH` and `claude-desktop`/`claude-desktop-3p`/`local-agent` (`cli:1503605`–`cli:1503607`); `remote_control_work_secret` — only from the bridge that the client turned on with a `remote_control` request (`cli:1468705`–`cli:1468724`).

### 4.7 `remote_tool_call`, `remote_plumbing_call`, `remote_tools_probe`, `remote_tools_reannounce`, `upgrade_relay_marker`
- A cloud session addresses the machine that serves its tools (`cli:111845`–`cli:111965`). The extension's SDK deliberately leaves the first four unanswered (`uVn`, `ext:109971`; `ext:110383`–`ext:110388`). They do not come in local `-p`: they are sent by the worker of the declared remote tools (`cli:968368`–`cli:969775`), and admission requires `--sdk-url` and `CLAUDE_CODE_REMOTE_SESSION_ID` (`cli:1503124`–`cli:1503131`). `upgrade_relay_marker` is not a request (section 4, introduction).

### 4.8 `ui_copy`, `ui_prompt_read`, `ui_prompt_fill`, `ui_prompt_paint`, `ui_prompt_suggest`, `ui_read_selection`
- A plugin in a headless session asks the host to copy text, read or fill the composer, show a suggestion, read the selection (`cli:113581`–`cli:113740`). These requests, unlike the others, do have a deadline: `AbortSignal.timeout(n)` (`sendUiHostRequest`, `cli:1468272`–`cli:1468286`). Without `uiHost` the SDK answers "Unsupported control request subtype" (`ext:110514`–`ext:110538`).

---

## 5. Life cycle

### 5.1 A turn: opening

1. The client writes a `user` with a `uuid`. The CLI puts it into the command queue: `command_lifecycle queued` (`cli:109518`).
2. The queue hands a batch to the turn: `command_lifecycle started`. Several messages that arrived in a row can merge into one turn — then `result.user_message_uuid` is the last member of the batch, and `user_message_uuids` are all of them (`d312:5985`–`d312:5989`).
3. `system/init` — at the start of **every** turn (`cli:1497286`, `cli:1497497`).
4. With `--include-partial-messages`: `system/status requesting` before every API request (`cli:768740`), then `stream_event`. Without partial — only finished `assistant`s.
5. `assistant` (one per content block), `user` with `tool_result`, repeated — while the model calls tools.

### 5.2 A turn: closing

- Exactly one `result` per turn — the signal that the turn has ended (`d312:6010`–`d312:6013`); `result_index` numbers the process's results from 0, and a skipped number is a lost result (`d312:6002`).
- After `result`, `task_*`, `session_state_changed`, `prompt_suggestion`, `rate_limit_event` and so on can still come.
- The `result` subtypes and `terminal_reason` — 2.1. An interrupted turn — as a rule `error_during_execution` with `aborted_streaming`/`aborted_tools` (2.1); a turn where a stop hook refused — `stop_hook_prevented`/`hook_stopped`; the model asked to go to the background — `background_requested` (`d312:10128`).
- `queued_turn_count > 0` — after this `result` the CLI itself will start the next turn from the queue (`d312:5975`).
- The end of the session's "work" for the client is not `result`: in the extension a turn counts as finished when `result` is not an error, there are no unanswered sends and no non-ambient background tasks (`FQ`, `ext:123862`–`ext:123881`).

### 5.3 Turns the CLI starts by itself

- **A task notification.** The end of a background command or of a subagent queues a service command `mode: "task-notification"` with `priority: "next"` (`oUt`, `cli:582323`–`cli:582339`; `Qbt = "next"`, `cli:368747`). It opens a turn like an ordinary message; the turn's `result` has `origin.kind: "task-notification"` (the extension relies on this, `ext:114647`). The model gets the text `<task-notification>…`; for a subagent whose own background work is still running, this is added: "This agent stopped with background work of its own still running. It may resume on its own when that work completes or reports, and the same task-id notifies again if it does; the result below may be interim." (`cli:582289`). The client does not see the notification itself in such a turn: there is no `user` frame in stdout even with `--replay-user-messages`, no `command_lifecycle`, and `status requesting` comes without `user_message_uuids`; what arrives is `init`, `status`, the model's answer and `result` with `origin {kind:"task-notification", producer:"session-task", runId}` (live run `t5-p12-bg`, line 114, likewise in `p9-now-bash-human` and `t2-subagent-progress`); the text `<task-notification>…` is only in the transcript — as a `user` line with this `origin`. A notification folded into another turn mid-way arrives as a `user` echo with `isReplay:true`, `origin.kind:"task-notification"` and **without** `isSynthetic`; its `uuid` is the `commandUuid` from `queue-operation remove absorbed_mid_turn` (`t5-midturn-notification`, line 62).
- **A turn without a model.** If the subagent's report has already been delivered to the main agent by a `SubagentHandback` call (handback `send`, the recipient is the main thread, flag `tengu_hushed_kestrel`), the notification goes with `shouldQuery:false` (`cli:582337`, `cMt` — `cli:422524`–`cli:422531`). Such a turn does not call the API and closes with `result success` with `num_turns:0`, `duration_api_ms:0`, `stop_reason:null`, `result:""` or the text of a local command, `total_cost_usd` — the running total, `usage` — zero (`hr = Pm`, `cli:1497127`; the zeros — `cli:211266`–`cli:211279`, `Pm` resolves exactly here) (`cli:1497509`–`cli:1497553`). The extension recognizes such a turn by `num_turns === 0 && origin.kind === "task-notification"` (`ext:114647`). This is **case 3**: the cost line "0 → 0 tokens" is a turn without a model, not a cost.
- **A fired timer** (`ScheduleWakeup`, `CronCreate`): `command_lifecycle started` with `wakeup_source` and `scheduled_for` (`cli:109498`–`cli:109505`). The `ScheduleWakeup` timer lives only in the process: in a live process it fires (`x6-wake-control`, `wakeup_source: loop_wakeup`), after SIGTERM and `--resume` — neither on time nor later (`x6-wake-kill`, `x6-wake-resume`); it is not on disk. The same happens in hqc after unloading on idle (trial session `agent-trader/session-7`, 2026-11-11).
- **Restarting an interrupted turn** after a process crash — only with `CLAUDE_CODE_RESUME_INTERRUPTED_TURN`; the `result` has `resume_reason` (`d312:5993`).
- Queue messages that the CLI added itself (timers, auto-continuations) can appear in `still_queued` of the `interrupt` receipt — unknown `uuid`s should be skipped (`d312:4848`).

### 5.4 Interruption

- `interrupt` (3.3) interrupts the turn; its `result` is as a rule `error_during_execution` (2.1), and the receipt arrives before the result.
- Messages in the queue are **not** lost: they are in `still_queued` and will run in the next turns unless removed with `cancel_async_message` or `cancel_queued:true`.
- Background tasks: without `perTaskStopAffordance` an interrupt kills background subagents and workflows, and each sends `task_notification stopped` (`Kle`, `cli:742336`–`cli:742345`); with it the interrupt spares them and stops only observer agents (`isObserver`: `qp`, `cli:330873`–`cli:330875`; `JQt`, `cli:742347`–`cli:742350`). An interrupt arms the `keep_alive` timer (`ai.arm()`, `cli:1504654`), but in local `-p` there are no frames: without `CLAUDE_CODE_REMOTE` the timer is off (2.4).
- A permission question that was pending at the moment of the interrupt: the wait is cut off by the turn's signal, and the CLI sends `control_cancel_request` (`cli:1468145`–`cli:1468148`); a deferred question is removed by `np` → `rt.fire("interrupt")` (`cli:1508547`–`cli:1508552`).
- `deny` with `interrupt:true` in response to `can_use_tool` interrupts the turn in the same way (`cli:1458710`–`cli:1458711`).
- The line `[Request interrupted by user for tool use]` arrives in stdout as a `user` frame **without** `isSynthetic` (live run `t5-p12-bg`, line 77): a client that hides service lines by `isSynthetic` will show it.
- A turn that took a message at the moment of an interrupt can put it back into the queue if the message had `requeue_if_interrupted_within_ms` (`cli:106677`–`cli:106683`; in the receipt — `put_back`).

### 5.5 The input queue and messages mid-turn (case 5)

- A message that arrives during a turn waits in the queue (`command_lifecycle queued`).
- **Folding in** happens in the query loop after a batch of tool calls has run (`aborted_tools` and the `PostToolBatch` hook — `cli:762387`–`cli:762397`) and before the next model request: commands from the queue turn into `queued_command` attachments (`AGe`, `cli:525584`), are taken off the queue with the reason `absorbed_mid_turn` and are marked `started` (`cli:762448`–`cli:762527`). In the conversation the message lands after the `tool_result` — hence "inserted after the tool result". On the last iteration allowed by `--max-turns` there is no folding in (`cli:762457`–`cli:762459`).
- The **echo** of a folded-in message (`user`, `isReplay:true`, its `uuid`, `isSynthetic` for service ones, `origin`) is born from this attachment at the moment of folding (`cli:1489207`–`cli:1489210`, `vAr` — `cli:723351`–`cli:723364`) — that is, later than the message was sent, and after the `tool_result`.
- **During the final answer** (the model writes text without tool calls) there is no next request in this turn, the attachment pass does not happen, and the message stays in the queue and opens **its own turn** after `result`. This follows from where folding happens: the folding block is in the branch the loop reaches only after tool calls; the code has no separate rule "do not fold in during an answer". Confirmed live (2.1.312, haiku, `--replay-user-messages`): a message sent during the final answer does not enter the current turn — the first turn's `result` comes, then the CLI itself starts a second turn with it: the echo, the answer, its own `result` (`docs/SPEC.md` §2.4, measurements of 2026-11-05).
- `result.user_message_uuids` lists folded-in messages too (`d312:5987`–`d312:5989`).
- At the start of a turn the CLI also folds in suitable commands from the queue (`absorbed_mid_turn`, `cli:759596`–`cli:759606`).
- `priority:"now"` tries to deliver the message at once, without waiting for a tool call (3.2).
- `command_lifecycle` for a message: `queued` → `started` → `completed` (the turn it entered ended cleanly) or `cancelled` (the turn was interrupted or failed — even if the message had already been folded in); the exact rule is the schema description (`cli:109518`).

### 5.6 Background tasks and subagents (cases 1–3)

- **Task events:** `task_started` (registration) → `task_updated` (patches) → `task_progress` (for a subagent) → `task_notification` (the end). Plus the snapshot `background_tasks_changed`.
- **`run_id`:** each start has its own; the format is the time in base36 and 4 random bytes (`qL`, `cli:138693`–`cli:138696`), so the ids of one task sort by time; a frame with an old `run_id` is a late one (`d312:6378`).
- **One claim per notification.** `ca()` emits `task_notification` only if the claim is free (`s8r`, `cli:138697`–`cli:138700`). When the task comes alive again (the subagent was woken by its background command, a user message, a continuation), `vo()` releases the claim, issues a new `run_id` and **sends `task_started` again** with the same `task_id` (`cli:332850`–`cli:332851`, `cli:332890`–`cli:332895`).
- **A subagent's Bash in the background (case 1).** The Bash tool passes the limit `subagentCapMs: _Lr(...)` (`cli:587148`). `_Lr` returns the value of the flag `tengu_maple_ferry` (default 0) only for an asynchronous subagent, not for the main thread and not for a nested call, and only if `WP()` and `KNe()` are false (`cli:518285`–`cli:518299`). `WP()` = `Se() && !(Ap() && !claudecode) && !$m()` (`cli:36943`–`cli:36946`), and `Se()` here is an import from the module `cli:4393`–`11551` (`cli:36567`): `!launchOptions.isInteractive()` (`cli:9442`–`cli:9444`), true in `-p`. `Ap()` is an entry point from the set `claude-desktop`, `claude-desktop-3p`, `local-agent` (`cli:36654`, `cli:36685`–`cli:36688`, `cli:36906`–`cli:36908`), `$m()` is `claude-vscode` (both — without `childSession`/`claudecode`). So in `-p` `WP()` is false, and the limit applies with `claude-vscode`, `claude-desktop`, `claude-desktop-3p` and `local-agent`; with `sdk-cli`, `cli` and others there is no limit. The hqc worker sets `claude-vscode` (`hqc:crates/hqc-worker/src/adapter/claude_code.rs:105`–`106`), so it has the same limit as the extension. `p7n` takes `min(requested timeout, max(limit, 2000 ms))` (`cli:377239`–`cli:377250`); when it runs out, the command is not killed but goes to the background with the text "Command was moved to the background (ID: …) after running for Ns" (`cli:518320`). How many seconds that is, the server decides: the cached flag value on agent-node-227 and on the laptop is `180000` ms (`cachedGrowthBookFeatures` in `~/.claude.json`, cache of 2026-11-11), which gives the observed ~180 s. Live, in passing: `Agent` without `run_in_background` with `claude-vscode` also went to the background (`tool_use_result {isAsync:true, status:"async_launched"}`, `task_started is_backgrounded:true`, `t2-subagent-progress` line 47; without this entry point it was not checked). A foreground Bash refuses a command if its first subcommand is `sleep N` with N ≥ 25: `tool_result is_error:true` "Blocked: sleep 45 followed by: … To wait for a condition, use Monitor…" (`mLr = 25`, `cli:518186`; the check — `cli:587021`–`cli:587034`; it does not apply to `run_in_background`).
- **An interim report (case 2).** When a subagent has live background work of its own — a nested agent, a workflow or (with the flag `tengu_concurrent_shore`, default true) a background Bash (`vyt`, `cli:582087`–`cli:582097`; `LK`, `cli:464962`–`cli:464964`), its end of turn is interim: `handback = "withheld"`, the report text gets "This agent has not reported yet: it is waiting on its own background work and will deliver its report through SubagentHandback when that finishes." (`Ynr`, `cli:195067`; the tool name `Yh = "SubagentHandback"`, `cli:194867`; the choice — `cli:727680`), and in `task_notification` the `handback` field is dropped for this case (`cli:728549`). The notification status is `completed`.
- **The final report.** When the subagent's background work is over, the subagent wakes up (in the 0.3.319 types this is the field `backgroundWakesOwner` of the Bash result: "the subagent that owns this backgrounded command parks for its end and is woken by it", `/Users/vvzvlad/Data/Projects/hqc_dev/vendor-src/sdk-0.3.319/sdk-tools.d.ts:3318`–`3321`; a new `run_id`, `task_started`), finishes its answer and delivers it to the main agent with a `SubagentHandback` call: `handback: "send"` (or `"flagged"` with a warning), `handback_report` — the report itself, and the text for the model is "This agent's report was delivered to you as a message from "…" (its SubagentHandback call). Read it there; it is not repeated here." (`woi`, `cli:195059`–`cli:195065`; `cli:727677`–`cli:727679`). The notification about such a completion goes with `shouldQuery:false` — **a turn without a model (case 3)**.
- **If a command still runs in the subagent after the report**, the subagent has again "stopped with background work" and will wake up when that ends: the same cycle `task_started` → `task_notification` with a new `run_id` (`cli:582289`).
- **Stopping:** `stop_task`, an interrupt without `perTaskStopAffordance`, stdin closing with the deadline exceeded (5.9) — `task_notification stopped`.

### 5.7 Context compaction (case 6)

- **`/compact`** (as `user` text) — a local command, a turn without a request to the model of the main loop: `status compacting` (a typed `/compact` has compaction statuses too, `cli:108230`–`cli:108234`) → `compact_boundary` → the summary as a `user` with a string `content`, `isReplay:false` and `isSynthetic:true` (`AC`/`CC`, `cli:1489308`–`cli:1489327`, `cli:1489282`–`cli:1489293`; `isSynthetic` = `isMeta || isVisibleInTranscriptOnly || isCompactSummary`, `uXe`, `cli:723153`) — the extension catches it by these three marks (section 8, "compaction summary") → `status` with `compact_result` → `result`. There is no `compact_output_chars` here (`cli:108230`–`cli:108234`).
- **Auto-compaction** in a turn: `status compacting` (with `compact_output_chars` as the summary is written) → `compact_boundary` (`cli:768744`) → `status null` with `compact_result: success|failed`, `compact_error` (`cli:768731`–`cli:768738`). Live (`p10-autocompact`, `CLAUDE_CODE_AUTO_COMPACT_WINDOW=100000`, threshold 67000 — matches the formula in `docs/SPEC.md` §2.4): the echo of the prompt that opened the turn comes **before** the boundary, and the boundary's `logical_parent_uuid` is this prompt's `uuid`; then `compact_boundary {trigger:"auto", pre_tokens, post_tokens}` and the summary — a `user` with `isSynthetic:true`, **without the `isReplay` key**, and `content` is an **array** of one `text` (for `/compact` — a string and `isReplay:false`); then the turn goes on (`assistant`, `result success`, `num_turns:2`). During compaction `compact_output_chars` came only as `0`. Compaction can fail: the first attempt gave `compact_result:"failed"`, `compact_error:"too_few_groups"`, and the turn went on without compaction with the window 165 % full; the next turn compacted. In the transcript both summaries are a `user` with `isCompactSummary` and `isVisibleInTranscriptOnly` and a string `content`.
- `compact_boundary.compact_metadata` (internal) carries the trigger and the token count; `logical_parent_uuid` links it with the chain before compaction.
- The auto-compaction threshold is not reported to the client in local `-p`: `autocompact_state` exists only with `CLAUDE_CODE_REMOTE` (`cli:1504978`–`cli:1504990`).

### 5.8 `--resume` (case 6)

- `system/init` comes again on every turn; `total_cost_usd` and `modelUsage` continue from the total saved in the transcript (`d312:5958`–`d312:5962`).
- **Orphans of the previous process** (`KYt`, `cli:1353895`): for background commands that have no completion record in the transcript, the CLI itself queues notifications `status:"stopped"` with the text "Background shell command didn't finish before the previous session ended" and the body "No completion record was found for it in the previous session. It may have been stopped (via the UI, Monitor timeout, or agent teardown — these leave no transcript marker), or it may have been running when the previous Claude Code process exited…" (`cli:1354470`–`cli:1354486`, `shouldQuery:false`). If there are many of them, one aggregate notification goes, and each command gets `task_notification stopped` with `summary` "Orphaned by a previous Claude Code process exit and reported in an aggregate summary." (`cli:1354466`, the text — `cli:1353889`). For background subagents — `stopped` or `failed` (`cli:1354304`).
- So after `--resume` the client will get `task_notification` about tasks that did not exist in this process (no `task_started` precedes them).

### 5.9 Process exit

- **stdin closed:** the CLI finishes the queue, then waits for background tasks no longer than `CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS` (default 600000 ms — `BBt`, `cli:333401`; `vFs`, `cli:403484`–`cli:403486`) and stops them with the stderr message "Background tasks still running … after the last turn (…); stopping them. Set CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS=0 to wait indefinitely." (`cli:1483505`–`cli:1483540`).
- **`end_session`** — 3.3.
- **Fatal input** — invalid JSON, a `user` with a wrong role, a `control_request` without `request` (3.1).
- While stdin is closed, the CLI sends no requests to the client: `sendRequest` throws "Stream closed" (`cli:1468139`), and a permission question becomes a refusal (`ze`, `cli:333613`).
- **Exit codes** (live, `p14-*`): stdin closing — 0 before a turn, after it and mid-turn (the CLI finishes the turn to `result`); `end_session` when idle — 0, mid-turn — **1** (`control_response success`, then `result error_during_execution` with `aborted_tools`, the message gets `cancelled`); invalid JSON — 1 ("Error parsing streaming input line … SyntaxError" in stderr), `control_request` without `request` — 1 ("Error: Missing request on control_request"), `user` with the role `assistant` — 1 ("Error: Expected message role 'user', got 'assistant'"); in all three fatal cases stdout is empty. By the code, also: 1 — the last `result` with `is_error: true` (`cli:1504381`), flag and startup errors with a line in stderr (`--resume-session-at` without `--resume`, `stream-json` without `--verbose`, the session is busy or not found, `--permission-prompt-tool` not found — `cli:1503460`–`cli:1503889`, `cli:1513031`–`cli:1513908`), a stdin line longer than `maxLineChars` (`cli:1467401`), a failed write to stdout (`cli:1513495`–`cli:1513523`), a loop of unhandled exceptions (`cli:408799`–`cli:408826`); 0 — SIGINT (`cli:1504872`–`cli:1504881`) and idling longer than `CLAUDE_CODE_EXIT_AFTER_STOP_DELAY` (`cli:1498489`–`cli:1498495`); 143 — SIGTERM (`cli:1504882`–`cli:1504887`); 129 — SIGHUP (`cli:408750`–`cli:408763`); 70 — an unhandled exception with `CLAUDE_CODE_SUPERVISED` (`cli:408787`–`cli:408797`, `iRt = 70` — `cli:68726`). The code is set once: a later exit does not change it (`cli:408880`–`cli:408881`). 75 and 78 occur only in Remote Control and the daemon (`cli:68724`–`cli:68725`).

### 5.10 API retries, limits, the fallback model

- A request retry: `system/api_retry` with `attempt`, `max_retries`, `retry_delay_ms`, `error_status` (`null` — no HTTP response), `no_response` (`cli:768796`–`cli:768805`). Retries exhausted — `assistant` with `error` and `result success` with `is_error:true`, `api_error_status`, `terminal_reason` `api_error`/`model_error`/`prompt_too_long` (`cli:768983`–`cli:768990`).
- A retry inside a subagent — `tool_progress` with `subagent_retry` (`cli:723779`–`cli:723790`).
- Background requests waiting for the subscription limit — the "heartbeat" `api_retry` with `error_status:429, error:"rate_limit"` (`cli:1502915`, `cli:1502937`–`cli:1502955`).
- The limits status — `rate_limit_event` on every change (`cli:1486662`–`cli:1486685`).
- A model change: `model_fallback` (unavailable), `model_consent_fallback` (with consent), `model_refusal_fallback` (the model refused, with `retracted_message_uuids` — which shown messages are retracted), `model_refusal_no_fallback`.

### 5.11 Thinking display (case 4)

- The flag `--thinking-display <summarized|omitted|highlights>` is hidden from the help (`cli:807312`, the values — `Ust`, `cli:104883`).
- Without the flag: in interactive mode — `summarized` with the `showThinkingSummaries` setting; in non-interactive mode with `text`/`json` (without `--verbose`) — `omitted`; in stream-json — **not set**, the API decides (`KXo`, `cli:144218`–`cli:144223`). For opus 5.5 on 2.1.312 this means an empty thinking text (confirmed live, `hqc:docs/SPEC.md:2248`–`hqc:docs/SPEC.md:2250`).
- With `summarized`: the `thinking` blocks in `assistant` carry the summary text; with `--include-partial-messages` it comes in pieces as `stream_event` (`content_block_delta` / `thinking_delta`) before the finished `assistant`. `assistant` and `stream_event` carry the internal `thinking_display`, and `assistant` also `thinking_duration_ms` (`cli:723345`–`cli:723349`).
- To change the display on the fly — `set_max_thinking_tokens {thinking_display}` (3.3).
- Regardless of the display, a volume estimate comes — `system/thinking_tokens` (`cli:768702`–`cli:768713`).

---

## 6. What changed in the 0.3.319 types against 0.3.312

A `diff` of the two `sdk.d.ts` (247 changed lines) and of `sdk-tools.d.ts` (38 lines).

| What | Where in 0.3.319 | Meaning |
| --- | --- | --- |
| a new union member `SDKSessionWritesLostMessage` (`system/session_writes_lost`) | `d319:6274`, union `d319:5666` | the CLI could not write the transcript or `~/.claude.json` (disk full, quota); sent once, on exit with code 0. 2.1.312 does not have it |
| a new request `set_has_subscribers {has_subscribers: boolean|null}` | `d319:5382` | the host reports whether anyone is watching the session |
| `pending_elicitation_requests` in `control_response` responses | `d319:429`, `d319:456` | open `elicitation`s on connecting, as with permission questions |
| `hook_on_failure_supported` in the `initialize` response; `onFailure` on a hook matcher | `d319:4838`; `d319:1025`, `d319:5514`, `d319:7523`, `d319:7625` | what to do if a hook callback failed |
| `check_permissions` on `mcp_call` | `d319:4960` | an MCP call with the usual permission check |
| `lossy` in the `read_file` response | `d319:5185` | the read was lossy (utf-8 over binary) |
| `credential_expires_at` in the responses to token refreshes | `d319:6081`, `d319:6206` | the credentials' expiry |
| `task_progress`: "Summaries are not a heartbeat" | `d319:2162` | a pause in the summaries does not mean a stop |
| an extended description of `still_queued` | `d319:4896` | — |
| `can_use_tool`: `suppressAlwaysAllowRule` for plugins, names of other servers in `matched_ask_rule` | `d319:288`, `d319:308`, `d319:5112` | — |
| tools (`sdk-tools.d.ts`): `file_unchanged` removed; added `backgroundWakesOwner` (a background command will wake the subagent that owns it), `outsidersMayHaveWritten`, `page_token`/`next_page_token`, 2 more occurrences of `timedOut`; `status` became optional | `sdk-tools.d.ts` 0.3.319: 3321, 457, 463 | `backgroundWakesOwner` is the very mechanism of case 2, now visible in the types |

---

---

# Part II. The JSONL transcript

## 7. The transcript

### 7.1 Where the files are

| What | Path | Code |
|---|---|---|
| Project directory | `~/.claude/projects/<slug>/`, `slug` is the cwd with every character except `[a-zA-Z0-9]` replaced by `-`; longer than 200 characters — cut, with `-<hash>` added | `ext:26826`, `ext:27395` (`sp = 200`), `cli:231947` |
| Session transcript | `<slug>/<sessionId>.jsonl` | `cli:411636` (`cm`), `cli:231950` (`up`) |
| Subagent | `<slug>/<sessionId>/subagents/agent-<agentId>.jsonl`; for workflow subagents — `subagents/workflows/<runId>/agent-<id>.jsonl` (the subdirectory comes from `agentTranscriptSubdirs`) | `cli:231963`, `cli:231968` (`pm`), `cli:721103` |
| Subagent metadata | next to it, `agent-<agentId>.meta.json` | `cli:411650` |
| Large tool outputs | `<slug>/<sessionId>/tool-results/` (the transcript holds `<persisted-output>` with the path instead of the output) | `cli:64897`, `cli:64910` |
| Remote agents | `<slug>/<sessionId>/remote-agents/remote-agent-<id>.meta.json` | `cli:411815`–`411820` |
| Workflows | `<slug>/<sessionId>/workflows/` (`scripts/`, `journal.jsonl` — one `{"type":"result",…}` line per agent) | `cli:247843`, `cli:802789`, `cli:950958` |
| Other files in the session directory | `custom-title.json`, `precompact.json`, `sent-prefix.json`, `ccr-tip.json` | `cli:836829`, `cli:333575` |

In the files: 12 533 subagent `.meta.json`, keys `agentType` (12 533), `spawnDepth` (12 502),
`description` (11 376), `toolUseId` (10 921), `model` (4 995), `requestShape` /
`requestNonInteractive` (4 608), `finished` (1 164), `isBuiltIn` (786), `parentAgentId` (783),
`workflowPhase` (455), `pickedEffortModel` (351), `stoppedByUser` (77), `effort` (14), `isFork`
(8), `launchedWithDefaults` (2). Subagent files directly in `subagents/` — 10 921, in
`subagents/workflows/<runId>/` — 1 688. In `tool-results/` — 7 840 `.txt`, 904 `.jpg`, 287 `.png`,
57 `.pdf`, 53 `.json`. 6 `custom-title.json` were found in session directories. The
`<sessionId>.jsonl.latest` files (1 637 of them) are written not by Claude Code but by logovo
(`/Users/vvzvlad/Data/Projects/logovo/scan.go`; the 2.1.312 binary has no `jsonl.latest` string:
`grep -a -c` = 0).

### 7.2 How a conversation line is written

- All `user`/`assistant`/`system`/`attachment` lines of the main session go through
  `insertMessageChain` (`cli:413793`). For each line an object is built (`cli:413853`–`413866`):
  `parentUuid` (`null` at a compaction boundary), `logicalParentUuid` (only at a compaction
  boundary), `isSidechain`, `teamName`, `agentName`, `promptId` (only on `user`), `agentId`, then all
  fields of the message itself (`MAi`, `cli:415947`), then `sessionKind` (`bg`/`daemon`/`daemon-worker`
  from `CLAUDE_CODE_SESSION_KIND`), `userType: "external"`, `entrypoint` (= `CLAUDE_CODE_ENTRYPOINT`),
  `cwd`, `sessionId`, `version` (`L$r`, `cli:411965`; `g0e`, `cli:411980`), `gitBranch`, `slug`.
- `parentUuid` is the `uuid` of the previous line of the same batch (or of the parent hint passed
  to the writer), but: a `user` line with `sourceToolAssistantUUID` gets that `assistant` line as its
  parent if it is already written (`cli:413847`–`413851`); `progress` lines never become parents
  (`kQ`, `cli:411586`, `cli:413876`).
- Before writing, `ZC` (`cli:414390`) drops messages whose `uuid` is already in the session and
  moves the parent hint to the last one already written (`cli:414407`–`414417`); `appendEntry`
  checks the `uuid` against the session's set once more (`cli:414093`, `cli:414107`). So a repeat of
  the same message (an echo, a resend with the same `uuid`) gives no second line.
- `toolUseResult` on a `user` line may be trimmed before writing: an `originalFile` longer than a
  threshold is replaced with `null` (`QPt`, `cli:593141`, `cli:413868`).
- Subagent: `BGe` (`cli:414461`) writes a batch with `isSidechain: true` and `agentId`, the file is
  `subagents/agent-<agentId>.jsonl` (`cli:414094`–`414095`); there is no repeated-`uuid` check for
  them (`cli:414108`). The first line of a subagent file is a `user` with `parentUuid: null` (12 535
  such lines in 12 541 files).
- Service records (`always`) are written to the file as is; `content-replacement`, `api-request*`,
  `fork-context-ref`, `observer-ref` with an `agentId` go to the subagent's file (`route-by-agent`,
  `cli:414069`–`414087`).
- While the session file does not exist yet, records pile up in `pendingEntries`
  (`cli:414046`–`414048`); the file is created on the first `user`/`assistant`/`system` line
  (`cli:413802`–`413807`).

### 7.3 Record kinds

The table is built by `scripts/table.py` from the CLI's two policy tables and both scanner passes.
"File policy" is what rewriting the file after compaction does (`transcript` — chain lines,
`boundary-cleared` — dropped before the boundary, `last-wins` — the last copy is kept,
`accumulate` — all are kept). "Write policy" is `appendEntry`'s (`dedup-transcript` — written only
for a new `uuid`, `always` — always, `route-by-agent` — to the subagent's file when there is an
`agentId`). Last column: for `user`/`assistant`/`system`/`attachment`/`api-request`/`api-request-shape`
— records with `version: "2.1.312"`; for records without a `version` field — all records of that
kind in files that hold at least one 2.1.312 record (other versions also appended to such files, so
the column is an upper bound).

| Record kind | File policy (409328) | Write policy (411518) | Where created, cli:line | Total, sessions | Total, subagents | In files with 2.1.312 |
|---|---|---|---|---:|---:|---:|
| `user` | transcript | dedup-transcript | 413793 insertMessageChain | 367907 | 770134 | 17376 |
| `assistant` | transcript | dedup-transcript | 413793 insertMessageChain | 648952 | 1270083 | 29081 |
| `system` | transcript | dedup-transcript | 413793 insertMessageChain | 43122 | 1319 | 800 |
| `attachment` | transcript | dedup-transcript | 413793 insertMessageChain | 203078 | 121902 | 6633 |
| `progress` | boundary-cleared | dedup-transcript | (44 creation sites, 0 in files) | 0 | 0 | 0 |
| `file-history-snapshot` | boundary-cleared | always | 413892 | 20427 | 0 | 887 |
| `file-history-delta` | boundary-cleared | always | 414887 | 8476 | 0 | 468 |
| `last-prompt` | boundary-cleared | always | 413165, 414420 | 115161 | 0 | 10253 |
| `continued-in` | boundary-cleared | always | 333504 | 0 | 0 | 0 |
| `content-replacement` | accumulate | route-by-agent | 413921, 967194 | 0 | 0 | 0 |
| `api-request-shape` | boundary-cleared | route-by-agent | 413988 | 2400 | 2169 | 442 |
| `api-request-blob` | boundary-cleared | route-by-agent | 414007 | 43567 | 88771 | 14599 |
| `api-request` | boundary-cleared | route-by-agent | 405848 | 75328 | 110344 | 15166 |
| `fork-context-ref` | accumulate | route-by-agent | 414471 | 0 | 0 | 0 |
| `frame-link` | accumulate | always | 413233 | 4453 | 0 | 2123 |
| `summary` | last-wins | always | ext 58548 (fork) | 0 | 0 | 0 |
| `custom-title` | last-wins | always | 413171, 416933; ext 58372 | 6198 | 0 | 531 |
| `ended-by-model` | last-wins | always | 416922 | 0 | 0 | 0 |
| `ai-title` | last-wins | always | 413172, 416947 | 96670 | 0 | 10119 |
| `tag` | last-wins | always | 413173 | 0 | 0 | 0 |
| `relocated` | last-wins | always | 413175, 414979 | 431 | 0 | 0 |
| `agent-name` | last-wins | always | 413177, 417530 | 591 | 0 | 530 |
| `agent-color` | last-wins | always | 413179, 417541 | 0 | 0 | 0 |
| `agent-setting` | last-wins | always | 413181 | 114865 | 0 | 10181 |
| `pr-link` | last-wins | always | 413225, 416959 | 4157 | 0 | 1492 |
| `artifact-comment-monitor` | accumulate | always | 710549 | 136 | 0 | 16 |
| `artifact-autoreact-ledger` | last-wins | always | 409476, 711645 | 554 | 0 | 100 |
| `bridge-session` | last-wins | always | 413242 | 0 | 0 | 0 |
| `history-suppression` | last-wins | always | 58308 (wlt), 413239 | 0 | 0 | 0 |
| `attribution-snapshot` | last-wins | always | 418121 (buffer) | 0 | 0 | 0 |
| `mode` | last-wins | always | 413182 | 47633 | 0 | 5567 |
| `permission-mode` | last-wins | always | 413184 | 13 | 0 | 0 |
| `isolation-latch` | last-wins | always | 413186 | 0 | 0 | 0 |
| `dev-mods` | last-wins | always | 413188 | 0 | 0 | 0 |
| `memory-mode` | accumulate | always | 413196 | 0 | 0 | 0 |
| `atis-latch` | last-wins | always | 413207, 413828 | 91471 | 0 | 10179 |
| `tools-used` | last-wins | always | 413218, 413835 | 2298 | 0 | 1005 |
| `worktree-state` | last-wins | always | 413222, 417726 | 450 | 0 | 0 |
| `cost-state` | last-wins | always | 437971 | 1341 | 0 | 198 |
| `queue-operation` | last-wins | always | 421420 | 108788 | 0 | 9500 |
| `observer-ref` | last-wins | route-by-agent | 414484 | 0 | 0 | 0 |
| `session-owner` | — | — | not in 2.1.312 (written by 2.1.319) | 7587 | 0 | 2560 |

What the records mean and when they are written (conversation lines excluded — they are in 1.4):

- **Session metadata** — `last-prompt` (`lastPrompt` up to 200 characters, `leafUuid`; an explicit
  one has `explicit: true`, and on a rewind `rewound: true`, `cli:414420`), `custom-title`, `ai-title`,
  `tag`, `relocated` (`relocatedCwd` after a folder change, `cli:414979`), `agent-name`,
  `agent-color`, `agent-setting`, `mode`, `permission-mode`, `isolation-latch`, `dev-mods`,
  `memory-mode` (`afterUuid`), `atis-latch`, `tools-used`, `worktree-state`, `pr-link`, `frame-link`
  (`artifactCount`), `history-suppression`, `bridge-session`. All of them are collected by
  `planReAppendSessionMetadata` (`cli:413123`–`413292`) and appended to the tail if the record
  differs from the last one of the same kind in the last 32 KiB of the file
  (`cli:413259`–`413290`). The occasion: every `hw/2` = 32 768 bytes written (`cli:412948`,
  `hw = 65536`, `cli:57178`), exit, file creation, after a file rewrite and after a move
  (`cli:413613`, `cli:413723`, `cli:413757`, `cli:413765`). Hence 115 161 `last-prompt` and 96 670
  `ai-title` over 1 633 sessions. `atis-latch` and `tools-used` are also written before a batch of
  conversation lines if they changed (`cli:413818`–`413837`).
- **`queue-operation`** — the input queue log (`SNo`, `cli:421415`): `operation`
  (`enqueue`/`dequeue`/`remove`), `content` (text), `reason` (`absorbed_mid_turn` — taken mid-turn,
  `delivered_to_agent` — handed to the agent; `cli:762510`, `cli:1491873`), `commandUuid`,
  `deliveryId`. In files with 2.1.312: `enqueue` 4 764, `dequeue` 2 610, `remove` 2 126 (of them
  `absorbed_mid_turn` 1 597, `delivered_to_agent` 184).
- **`file-history-snapshot`** (`messageId`, `snapshot`, `isSnapshotUpdate`, `cli:413892`) and
  **`file-history-delta`** (`messageId`, `snapshotMessageId`, `trackingPath`, `backup`,
  `cli:414886`) — file snapshots for rewinding edits; tied to the `uuid` of a `user` line through
  `messageId`.
- **`api-request`**, **`api-request-shape`**, **`api-request-blob`** — records of API requests for
  replay (`writeRequestReplayRecords`, `cli:413976`; `api-request` — `cli:405842`): the request
  shape (system + tools) by hash, message chunks by hash, the request itself with references to them
  (`params`, `base`, `tail`, `keep`, `cacheControl`, `requestDigest`, `querySource`). With an
  `agentId` — into the subagent's file. Keys of `api-request` in files with 2.1.312: 17 143 for each
  of those listed.
- **`cost-state`** — the session's spending total (`totalCostUSD`, durations, `modelUsage` per
  model, `cli:437966`); checked against a schema when read (`cli:409287`, `cli:409526`).
- **`content-replacement`** — replacements of tool content (`replacements`, `agentId`,
  `cli:413921`); **`fork-context-ref`** (`cli:414471`) and **`observer-ref`** (`cli:414483`) —
  references to another context; **`continued-in`** (`continuedInSessionId`, `cli:333503`) — the
  session continued in another one; **`ended-by-model`** (`cli:416920`); **`artifact-*`**,
  **`attribution-snapshot`** — service records for artifacts and attribution. The laptop's files hold
  none of `content-replacement`, `fork-context-ref`, `observer-ref`, `continued-in`, `ended-by-model`,
  `attribution-snapshot`, `bridge-session`, `history-suppression`, `tag`, `agent-color`,
  `isolation-latch`, `dev-mods`, `memory-mode` (0 by `scan.py`).
- **`summary`** — the old kind (`summary`, `leafUuid`): CLI 2.1.312 only reads it (key
  `leafUuid`, `cli:409497`); the extension writes it when it forks a session (`ext:58546`–`58550`).
  In the files: 0.
- **`progress`** — present in the tables, 44 creation sites, but 2.1.312 does not write it to disk:
  0 lines in files of all versions, and also 0 in 28 files of live runs on the test bench (1 104
  records, among them a 70 s Bash with output, a subagent with Bash, Stop hooks).
- **`session-owner`** (`accountUuid`, `organizationUuid`) and `repl-mode` — written by 2.1.319 (9
  occurrences of `session-owner` in its binary); 2.1.312 has neither (`grep -a -c` = 0). In the
  files: 7 587 `session-owner`.

### 7.4 Conversation lines: fields

Fields common to all four kinds (from `insertMessageChain`, see 7.2): `type`, `uuid`, `parentUuid`,
`isSidechain`, `timestamp`, `sessionId`, `version`, `cwd`, `gitBranch`, `userType`, `entrypoint`,
`slug` (not on all), `agentId` (on subagents), `teamName`/`agentName` (teams), `sessionKind`.

**`user`.** The constructor is `Ie` (`cli:588636`–`588732`); the full list of message fields:
`message {role, content}`, `isMeta`, `promptShellHandOff`, `mcpSkill`, `invokedSkill`,
`ephemeral`, `turnCompanion`, `usageLimitNote`, `replacesSpan`, `isVisibleInTranscriptOnly`,
`isVirtual`, `isCompactSummary`, `repeatsCompactSummary`, `modelOnlyText`, `summarizeMetadata`,
`toolUseResult`, `classifierMetaLines`, `classifierBoundary`, `serverClassifierContext`,
`hostClassifierContext`, `toolDenialKind`, `toolDenialEndsTurn`, `toolDenialMonitorBlocked`,
`toolDenialUnanswered`, `userFeedback`, `toolResultRemedy`, `permissionDecision`,
`sandboxViolations`, `sandboxed`, `mcpMeta`, `toolEndsTurn`, `imagePasteIds`,
`sourceToolAssistantUUID`, `permissionMode`, `origin`, `promptSource`, `promptId`,
`interruptedMessageId`, `refusedMessageId`, `interruptedByShutdown`. The record also gets queue and
turn fields: `mergedSendUuids`, `turnAttributionKey`, `clientPlatform` (`kA`,
`cli:1495861`), `queueSkipAttachments`, `queueTranscriptOnly` (`cli:594335`, `cli:594341`),
`turnOrigin`, `turnPosition`, `sourceToolUseID`, `summarizedTurnPromptUuids`,
`joinedNotice*`, `wakeupSource`.
Seen on 17 376 `user` lines of 2.1.312 (`scan2.py`, keys of records with `version` 2.1.312):
`permissionDecision` 16 123, `sourceToolAssistantUUID` 16 143, `sandboxed` 10 905, `toolUseResult`
8 157, `serverClassifierContext` 5 731, `mcpMeta` 1 044, `permissionMode` 631, `promptSource` 631,
`turnOrigin` 624, `turnPosition` 593, `isMeta` 525, `origin` 468, `queueSkipAttachments` 332,
`turnAttributionKey` 308, `turnCompanion` 201, `toolEndsTurn` 62, `queueSkipSlashCommands` 53,
`joinedNotice{Lengths,Own,RunIds}` 53, `isCompactSummary` 38, `isVisibleInTranscriptOnly` 38,
`queueTranscriptOnly` 37, `toolDenialKind` 31, `sourceToolUseID` 29, `imagePasteIds` 19,
`summarizedTurnPromptUuids` 14, `classifierBoundary` 5, `mergedSendUuids` 1,
`classifierMetaLines` 1. Across all versions also `interruptedByShutdown` (59 in sessions),
`refusedMessageId` (54), `wakeupSource` (49), `toolDenialUnanswered` (1).
`promptSource` in 2.1.312: `system` 318, `sdk` 313 (across all versions also `typed` 6). `origin.kind`
across all versions: `human` 19 088, `task-notification` 14 828, `peer` 1 557. `toolDenialKind`, per
the CLI's own text (`cli:788572`): `user-rejected`, `permission-rule`, `automode-*`, while
`interrupted`/`cancelled` mean a stop, not a denial.
`toolUseResult` is the tool's structured result (not what the model sees): across all versions in
sessions `dict` 261 717, `list` 42 013, string 17 515 (of them starting with `Error:` 8 912,
`User rejected` 388, `InputValidationError` 39).
`isMeta: true` marks a line for the model, not a human's message: across all versions in sessions,
`[Image #N]` captions for images 1 419, messages from other sessions ("Another Claude session…")
1 505, `<local-command-caveat>` 355, `<system-reminder>` 85, other 3 997 (`scan2.py`,
`meta_user_class`). The CLI finds the human's last message by skipping `isMeta`, `isCompactSummary`,
built-in commands and texts like `<tag …>`/`[Request interrupted by user…]` (`K4n`,
`cli:415897`; regex `yAt`, `cli:411496`). `isVisibleInTranscriptOnly: true` is on 1 519 session
lines, exactly as many as there are summaries (`isCompactSummary`); the CLI sets both flags together
(`cli:483086`–`483087`). `isSidechain: true` is on every line of subagent files.

**`assistant`.** A line is one content block of an API message; the `message.id` is shared, the
block number is `apiBlockIndex`. Fields in the 29 081 lines of 2.1.312: `requestId` 29 046,
`effort`/`requestedModel`/`requestRef`/`apiBlockIndex` 29 065, `perTurnEffort` 29 071,
`wireToolInputs` 16 145, `attributionAgent` 13 919 (on subagents), `serverClassifierRequest` 12 498,
`thinkingDurationMs` 11 201, `thinkingDisplay` 11 200, `attributionSkill` 2 953,
`attributionMcpServer`/`attributionMcpTool` 2 825, `wireIngestContext` 1 487, `isApiErrorMessage` 16,
`error` 6, `apiErrorStatus` 6, `apiErrorIsTransient` 5, `apiErrorCode` 1, `resendCannotCure` 1. On
agent-node-227 also `isAbortedMidStream: true` (2 lines) — the answer was cut off midway
(`cli:506817`, `cli:723092`; in the stream frame it is `aborted`, `cli:723317`).
A synthetic answer (not from the model) is built by `AYn` (`cli:588415`): `message.model:
"<synthetic>"` (`ki`, `cli:6506`), a new random `message.id`, `stop_reason: "stop_sequence"`, zero
`usage`, plus `apiError`, `apiErrorParams`, `apiErrorIsTransient`, `quotaLimits`, `error`,
`errorDetails`, `truncatedAfterOutput`, `isApiErrorMessage`, `isVirtual`.

**`system`.** Subtypes in the files (all versions, sessions): `stop_hook_summary` 36 105, `api_error`
5 271, `compact_boundary` 1 519, `informational` 114, `model_refusal_fallback` 44,
`local_command` 33, `model_refusal_no_fallback` 28, `model_consent_fallback` 4, `turn_duration` 4.
In 2.1.312: `stop_hook_summary` 693, `api_error` 60, `compact_boundary` 38 (31 + 7 on subagents),
`informational` 9. `api_error` is the request-retry line (`w2`, `cli:593380`: `error`, `retryInMs`,
`retryAttempt`, `maxRetries`, `source`, `level: "error"`). `compact_boundary` is `c2`
(`cli:593367`), see 1.5.

**`attachment`.** An `attachment` field with its own `type`; in 2.1.312 the line also has
`rendered`, `renderedRole` (3 685 of 6 633), `renderedInHumanTurn` (313), `renderedBesideToolResult`
(170). There are 58 kinds of `attachment.type` in the files; frequent ones: `output_style` 74 080,
`deferred_tools_record` 31 810, `queued_command` 14 234, `todo_reminder` 8 760,
`hook_additional_context` 7 289. For parsing, `queued_command` matters — a message taken from the
queue mid-turn (see 7.6): `prompt`, `commandMode` (`prompt`/`task-notification`), `origin`, `isMeta`,
`source_uuid` (the `uuid` of the sent message), `delivery_id`.

### 7.5 The chain: branches, compaction, rewind and fork, resume, subagents

- **A normal turn.** `user` → `attachment`… → `assistant` (block 0) → `assistant` (block 1) … A
  `tool_result` line hangs on the line with the `tool_use`, so one `assistant` line has two children:
  the next block of the same message and the `tool_result`. This is not a branch. Parents with two
  or more `user`/`assistant` children in session files (`scan2.py`, `branch`): an `assistant` +
  `tool_result` pair — 10 952 (741 of them in files with 2.1.312); real branches — two `assistant`
  (1 903), several `tool_result` (891), two `user` (157), `assistant` + `user` (42). In 2.1.312, on
  all 8 048 lines with `sourceToolAssistantUUID` it matches `parentUuid`; in records of other
  versions it did not match on 34 437.
- **Compaction** (`c2`, `cli:593367`; the summary line — `cli:483086`, `cli:492469`;
  the chain — `cli:413840`–`413855`). The boundary: `type: "system"`, `subtype: "compact_boundary"`,
  `content: "Conversation compacted"`, `parentUuid: null`, `logicalParentUuid` — the last line
  before compaction, `compactMetadata` (in 2.1.312: `trigger` 31 of 31, `preTokens`, `postTokens`,
  `cumulativeDroppedTokens`, `durationMs` — 31; `preCompactDiscoveredTools`, `preservedSegment`,
  `preservedMessages` — 30; `runningTurnSends` 17; `precomputed` 8). On all 38 boundaries of 2.1.312
  `parentUuid` = `null` and `logicalParentUuid` is set; in 23 of 31 it is found in the same file. The
  other 8 are a bug of the CLI itself: `logicalParentUuid` is taken from memory (`r.at(-1)?.uuid`,
  `XNr`, `cli:483771`–`483772`), while only messages that pass the `EE` filter are written to disk
  (`cli:420187`–`420201`, `cli:420269`–`420273`): it drops `progress`, `hook_success` attachments
  without output, `system` with `persist: false` and others. If the last message in memory is one of
  those, the boundary points to a line that is not on disk (a precomputed compaction takes
  `r.findLast(EE)?.uuid`, `cli:494420`, and has no such bug). In all 8 the last written line before
  the boundary is `user(tool_result)`; in the 23 found, `logicalParentUuid` matches the last written
  line; for subagents of 2.1.312, 8 of 9 have no parent; across all versions, 463 of 1 488 in
  sessions and 1 031 of 1 312 in subagents. Further in 2.1.312: the boundary's child is
  `attachment/instructions` (30 of 31), a chain of attachments, then a `user` line with
  `isCompactSummary: true` and `isVisibleInTranscriptOnly: true`, no `isMeta`, no `origin`, and
  `content` the string "This session is being continued from a previous conversation…"
  (`cli:195277`); its parent is `attachment/date` (30 of 31). In records of other versions the summary
  is a direct child of the boundary in 824 + 759 cases (sessions + subagents), and comes after
  `attachment/date` in 664 + 553. `preservedSegment` (`headUuid`, `anchorUuid`, `tailUuid`)
  and `preservedMessages` (`uuids`, `anchorUuid`) are objects on 38 of the 39 found boundaries: the
  preserved tail stays in the file above the boundary; on reading, the SDK reader re-points its
  head's `parentUuid` to `anchorUuid`, and the anchor's children to the tail (`ext:57041`–`57063`).
- **Rewriting the file after compaction** (`performCompactTranscript`, `cli:413480`–`413625`):
  file ≥ 5 MiB (`PGs`, `cli:411561`), gain ≥ 10 % (`rdo`, `cli:411564`); lines before the last
  boundary are dropped, except the preserved tail, its snapshots and the "last-wins" metadata
  (`c$e`, `cli:409485`); written to a temporary `<file>.compact.tmp.<hex>` and renamed over it
  (`cli:413484`, `cli:413606`), what was appended meanwhile is carried over. It is on only with
  `store.localGcEnabled`: in `-p` without `sdkUrl` — with `CLAUDE_CODE_TRANSCRIPT_LOCAL_GC=1`
  (`cli:793877`), with `sdkUrl` — by the same variable or the `tengu_transcript_local_gc` flag
  (`cli:1470799`, `cli:1513768`). By default the file is only appended to.
- **Rewind and fork.** A rewind writes an explicit `last-prompt` with `leafUuid` and `rewound: true`
  (`O1t`, `cli:414420`); the SDK reader removes the cut branch by `rewound` and `cutUuid`
  (`JTe.readCheckpoint`, `ext:57537`–`57555`). In the files `rewound` is 0. The extension's fork
  (`forkSession`, `ext:58393`) writes a new file: a copy of the chain with new `uuid`s and rewritten
  `parentUuid`, `logicalParentUuid`, `source_uuid`, `compactMetadata`, then `summary`
  (`leafUuid`) and `custom-title` "… (fork)" (`ext:58520`–`58552`).
- **Resume.** The same `sessionId` means the same file (`cm`, `cli:411636`); the caller passes the
  parent of the new batch, the CLI remembers the last leaf (`currentSessionLeafUuid`, `cli:413883`)
  and writes it into `last-prompt.leafUuid`. If two processes appended to the file, one parent gets
  two children (cc_remote `docs/SPEC.md` §2.4, method B). The same `uuid` twice in one file was seen
  on: `assistant` 2 040, `user` 1 271, `attachment` 621, `system` 103 lines (`scan.py`, `dup_uuid`). All 4 035 are in 39 VS Code session
  files (`entrypoint: claude-vscode`) of versions 2.1.209–2.1.280; in versions newer than 2.1.280 —
  0 in 690 879 records, 0 for subagents and on the nodes. The copy is always later than the original
  (by 11–10 000 lines), with the same `timestamp` and `sessionId`, but with the
  `cwd`/`version`/`slug`/`promptId` of the neighboring lines, and is not attached to the current
  chain; there is always a `last-prompt` between them. It looks as if the process that ran the
  session later rewrote old pieces of the conversation again (a hypothesis: the code of those
  versions is not available). In 2.1.312 this should not happen: the `uuid` set for the check is
  built over the whole file and filled in on resume (`cli:412184`–`412199`, `t5n` — `cli:419655`,
  the check — `cli:414093`–`414107`). The scanners are `dupscan.py`, `dupscan2.py` in
  `vendor-src/scripts/`.
- **Which leaf the CLI takes on `--resume`.** Not the way the SDK reader in the extension does it
  (that one takes the last line of the file without children, 8.2 item 5). `--resume` in `-p` →
  `CHe` → `oDe` → `roe(path)` without `keepAllLeaves` (`cli:1513834`, `cli:419598`,
  `cli:419464`–`419482`). The mark is the `leafUuid` of the last `last-prompt` (`explicit` is reset
  by any conversation line after it; `leafUuid: null` with `explicit` means the session was cleared;
  a compaction boundary resets the mark; `cli:419054`–`419072`). The choice
  (`zr`, `cli:419188`–`419285`): a cleared session gives an empty conversation; if the file has no
  mark at all and the latest line by time is not the last one in the file, that line is taken;
  otherwise the candidate is the mark (if it is not explicit and the last line of the file is its
  direct descendant, then the last line); with several leaves — the mark if it is among them,
  otherwise the last line of the file. From the candidate up to the nearest `user`/`assistant`; the
  chain follows `parentUuid`, with a broken parent — matching by time
  (`tengu_chain_timestamp_fallback`, `cli:416065`–`416087`). If two processes wrote to one file, the
  CLI continues from the `last-prompt.leafUuid` of the last writer, and the extension from the last
  line of the file without children: the branches can diverge.
- **Subagents.** A file of its own, `isSidechain: true`, `agentId`, the first line is a `user` with
  `parentUuid: null`. The main file holds only the subagent's `Agent`/`Task` call, its
  `tool_result` and the notifications.

### 7.6 Edge cases found in practice

**Messages the CLI took from the queue together.** When taking from the queue
(`cli:1484811`–`1484840`) the following `prompt`s are pulled up to the first one, and with several
messages `pp` (`cli:1481797`) joins them: `value` is the texts joined with `\n` (`WU`,
`cli:1481790`), `uuid` is **the `uuid` of the last message that has one**
(`e.findLast((R) => R.uuid)`), `origin` is the first one's. `Y_` (`cli:1489525`) puts the `uuid`s of
the others into `mergedSendUuids`; the `user` line is written with that `uuid` (`MKn`,
`cli:1497334`) and the `mergedSendUuids` field (`kA`, `cli:1495861`). With
`--replay-user-messages` the others get an `isReplay` echo with their own `uuid`s (`replayMerged`,
`cli:1508008`–`1508024`). The files hold 6 lines with `mergedSendUuids` (1 of 2.1.312); in all 6 the
line's own `uuid` is not in the list. Live run (`t1-*`, messages `$U2` and `$U3` sent 0.5 s apart):
- joining happens only when the messages wait in the queue together — for example, they arrived
  during the final answer; then the transcript has one `user` record with `uuid = $U3`,
  `mergedSendUuids = ['$U2']` and the text of both joined with `\n`; with `--replay-user-messages`
  two echoes arrive — `$U2` with its own text (right when it is taken from the queue, after the
  previous turn's `result`) and `$U3` with the **joined** text of both;
  `result.user_message_uuid = $U3`, `user_message_uuids = [$U2, $U3]`; without replay there is no
  echo, and both `uuid`s are in `command_lifecycle`, `status.user_message_uuids` and `result`;
- mid-command there is no joining: each message is its own `attachment/queued_command` with
  `source_uuid` and `queue-operation remove absorbed_mid_turn`, each has its own echo, at the moment
  it is merged in; a message taken mid-turn is written the same way, and `command_lifecycle started`
  goes to the stream (`cli:762476`–`762516`);
- two frames at process start (0.3 s apart, and in one stdin write) were not joined: the CLI took
  the first into a turn 4–5 ms after `queued` and merged the second in mid-turn.

In every captured case the stream named the record's `uuid`. The record "with a uuid the stream did
not name" from DrZlo13/hqc#42 (commit `feb3cf3`) is not explained by this.

**`<task-notification>`.** A background task notification is an ordinary `user` line: `content` is
the string `<task-notification><task-id>…<status>…</status><summary>…`, no `isMeta`,
`origin.kind: "task-notification"` (14 828 across all versions), `promptSource` `system`/`sdk`. In
2.1.312: 266 such lines (`completed` 210, no status 32, `stopped` 16, `failed` 8). If the
notification arrived mid-turn, it is an `attachment/queued_command` with
`commandMode: "task-notification"` (in 2.1.312: 134 in sessions, 179 in subagents). After
`--resume` the CLI scans the restored messages for background tasks without an outcome (`KYt`,
`cli:1353895`) and adds a notification with the text "… didn't finish before the previous session
ended" (`cli:1353892`); the files hold 35 such lines, all with `<status>stopped</status>` (11 of
2.1.312).

**Compaction.** Records: the boundary, attachments, the summary line (7.5). The summary is the
`message.content` of the `user` line with `isCompactSummary: true`. The counts agree: boundaries in
sessions 1 519, `isCompactSummary` lines 1 519, `isVisibleInTranscriptOnly` 1 519 (`scan.py`).

**"[Request interrupted by user]" and a tool denial.** The line is built by `Vv` (`cli:588739`):
`user`, `content` is a list with one text, `[Request interrupted by user]` (`gk`) or
`[Request interrupted by user for tool use]` (`Zu`, `cli:95475`), no `isMeta`. A denied call is a
`tool_result` with `is_error: true` and the text "The user doesn't want to proceed with this tool
use…" (`Ty`, `cli:95518`), `toolUseResult: "User rejected tool use"` (`kAt`, `cli:95520`),
`toolDenialKind`; next comes a `[… for tool use]` line: 354 of 358 such lines in sessions hang on a
`tool_result` (`scan2.py`, `interrupt`). A plain `[Request interrupted by user]` hangs on `assistant`
(214), on `tool_result` (171), on `user` (153), on attachments. The full list of "interruption
texts" the CLI treats as its own is `BPe` (`cli:95494`, function `nm`, `cli:95505`): also "session
ended before this agent finished", "Tool call did not complete…", "Tool call interrupted: the
session ended…", "Tool call result not in this copy…", "The user doesn't want to take this action
right now…", two "Tool call skipped…". Messages interrupted by the process exiting have
`interruptedByShutdown: true` (59 lines across all versions).

**`<synthetic>` answers.** Not from the model but from the CLI (`AYn`, `cli:588415`). 811 in
sessions across all versions; by text: "API Error…" with `isApiErrorMessage` and `error`
(`rate_limit`/`server_error`/`invalid_request`/`authentication_failed`/`unknown`…), "You've hit
your…" (`rate_limit`), "Credit balance…" (`billing_error`), "No response requested." (`d2`,
`cli:6505`; 214, without `isApiErrorMessage`). In 2.1.312: 16 lines (`scripts/scan2.json`): "No
response requested." 10, "API Error" with `rate_limit` 3 and `verification_required` 1 in sessions,
"API Error" with `rate_limit` 2 in subagents.

**Local commands.** `/cost` and the like are written as three `user` lines: `isMeta: true` with
`<local-command-caveat>…` (`Xye`, `cli:588748`), then without `isMeta` —
`<command-name>/cost</command-name> <command-message>…<command-args>…` and
`<local-command-stdout>…</local-command-stdout>` (356 / 356 / 333 lines across all versions, of them
1 / 1 / 1 in 2.1.312). Occasionally the output is a
`system/local_command` line (33 on the laptop, all of other versions; on agent-node-227, 4 lines of
2.1.312 with `<local-command-stdout>`, 7.7). A skill call (`<command-message>` with `isMeta`, a list)
— 328 in subagents.

### 7.7 Transcripts of hqc workers

The same scanners (`scan.py`, `scan2.py`, `dupscan.py`, `cbscan.py`, `vescan.py`) were run on
2026-11-12 over `~/.claude/projects/` of the `claude` user on five nodes; the results are in
`vendor-src/scripts/nodes/<address>/`, the comparison is `scripts/table312.py`. hqc worker records
on 2.1.312 exist only on agent-node-227 (46 `claude-vscode` session files, 3 331 records, and 12
subagent files); on 226, agent-node-3, 129 and vvzvladmbpm1 there are a few files of old
interactive sessions each. They have no other directory: `CLAUDE_CONFIG_DIR`
(`hqc:crates/hqc-worker/src/adapter/claude_code/paths.rs:54`–`56`) is set neither in the environment
of the `hqc-worker` service (`systemctl show`, only the variable name was checked), nor in the
vvzvladmbpm1 plist, nor in `~/.hqc/worker.toml`, and the `claude` user has a single settings
directory, `~/.claude`. So these workers simply ran almost no hqc sessions.

| 2.1.312, lines | Laptop (2026-11-11) | Nodes (2026-11-12) |
| --- | ---: | ---: |
| `assistant` / `user` | 29 081 / 17 376 | 1 097 / 1 017 |
| `<synthetic>` (of them "No response requested.") | 16 (10) | 2 (1) |
| `[Request interrupted by user …]` | 0 | 2 |
| tool denial (`toolUseResult: "User rejected tool use"`) | 0 | 2 |
| `interruptedByShutdown` | 0 | 2 |
| `system/local_command` | 0 | 4 |
| `assistant` with `isAbortedMidStream` | 0 | 2 |
| `mergedSendUuids` (own `uuid` not in the list) | 1 | 2 |
| compaction boundaries (with `logicalParentUuid` in their own file) | 38 (24) | 2 (2) |
| `message.id` in files with 2.1.312 (split over several lines) | 31 215 (22 308) | 952 (139) |
| "`assistant` + `tool_result`" branchings | 864 | 0 |
| `uuid` repeats, all versions | 4 035 | 0 |

How the worker differs: API messages are split into lines less often (fewer answers with thinking —
`thinkingDisplay` on 117 of 1 034 `assistant` lines against 5 846 of 15 160), so there are no
"`assistant` + `tool_result`" branchings; all 381 `queue-operation enqueue` have `content` (on the
laptop 1 243 lack it), `remove` comes only with a reason; session files have no `agent-setting`,
`session-owner`, `frame-link`, `pr-link`, `custom-title`, `agent-name`, `file-history-delta`. The
same: `sourceToolAssistantUUID` = `parentUuid` on all 569 lines, `<task-notification>` is a `user`
line with `origin.kind: task-notification` and `promptSource: system`, no broken lines. The laptop
was captured a day earlier than the nodes (9 files were added on it in that time).

---

# Part III. How the VS Code extension parses messages

## 8. How the extension understands messages

### 8.0 A message's path and the line model

- The extension host reads the CLI stream (`ext:125619`–`125731`) and sends each message to the
  webview as `io_message` (`ext:125715`), except: `system/bridge_state` (`ext:125642`–`125653`);
  `system/hook_started|hook_progress|hook_response` of all events except `SessionStart` and `Setup`
  (`DVe`, `ext:123961`; `b3`, `ext:50679`); `system/session_title_changed` — the host sets the title
  itself (`ext:125708`–`125712`). It adds `system/status` with
  `userPromptSubmitHooksRunning` itself — the number of open `UserPromptSubmit` hooks
  (`ext:125654`–`125665`, `LVe`, `ext:123968`).
- The webview sorts `io_message` by channel (`web:154285`–`154295`); `auth_status` with
  `isAuthenticating` also opens the AWS sign-in window along the way. Each message is in its own `try`
  (`web:170880`–`170890`). A `user`/`assistant` that fails to parse and has no feed line yet
  (`hasRowForFrame`) gives the line "Couldn't display a message here." (several in a row —
  "Couldn't display N messages here.", `web:167702`, `web:170930`–`170937`), except a `user` with
  `parent_tool_use_id` and synthetic messages with an `origin` other than `peer`/`channel` (`mGe`,
  `web:167721`); a failed `result` still closes the turn (`web:170915`). The console gets "Skipped a
  message (…) that failed to process" (the first 5, `web:167766`).
- A feed line is `Ap` (`web:160591`): `type` ∈ `user`/`assistant`/`meta`/`compact`/
  `refusal_fallback`, blocks `Js` (`web:160702`, a `tool_use` block has its own `toolResult`),
  `uuid`, `betaMessageId` (= `message.id`), `blockIndex`, `parentToolUseId`,
  `sdkParentToolUseId`, `hiddenFromChat`, `isSynthetic`, `isSynthesizedByLoop`, `origin`,
  `interruptedByShutdown`, `notAPrompt`, `foldedIntoTurn`, `failure`, `compactMetadata`,
  `compactSummary`. A message is turned into a line by `Ky` (`web:160526`–`160576`), inserted into
  the feed by `j8` (`web:172020`–`172075`), drawn by `qA` (`web:261240`–`261318`).
- An empty line (`isEmpty`, `web:160683`–`160694`) is not drawn: no blocks, or all blocks are
  `tool_result`, or all are an unfinished `tool_use`, or all the text is empty / "(no content)".

What follows goes by message kind. "Live" is the handling in `processIncomingMessage`
(`web:171028`) and `processMessage` (`web:171277`); "history" is what happens when the extension
opens a session (8.2).

### `user` (live, without `isReplay`)

- `Ky`: `parentToolUseId` = `parent_tool_use_id` (only on `user`), `isSynthetic` =
  `isSynthetic` or (`is_meta` and an `origin` is present), `notAPrompt` = `is_meta` or
  `isCompletedLocalCommand`, `interruptedByShutdown`, `origin`; a string `content` → one
  text block; `type: "fallback"` blocks are dropped (`web:160527`–`160565`).
- `j8`: each `tool_result` finds its `tool_use` (searching `assistant` lines from the end of the
  feed, `lme`, `web:171759`) and puts the result into it (`setToolResult`) with
  `interruptedByShutdown` and `toolDenialUnanswered`; `tool_use_result` is passed only for
  `AskUserQuestion` and only if the message has one `tool_result` (`web:172021`–`172039`). Then the
  `user` line is added to the feed as is; if it holds only `tool_result`s, it is empty and not drawn.
- A main-session `tool_result` during a turn sets "turn with tools"
  (`turnHadToolRound`, `web:171176`–`171184`) — it decides where the echo lands (see
  `isReplay`). A `tool_result` of an `Artifact` call with a URL opens it in the browser
  (`web:171198`–`171217`).
- Drawing (`web:261243`–`261245`): a line with `parentToolUseId` (a prompt to a subagent) is not
  drawn; `isSynthetic` is not drawn, except a message from another session (`PD`, `web:162269`).
  `image`/`document` blocks are drawn as attachments (`web:261026`).
- A text block is parsed by `pc` (`web:161386`–`161445`) in this order: exactly
  `[Request interrupted by user]` → "Interrupted", exactly `[Request interrupted by user for tool
  use]` → "Tool interrupted" (`Xk`, `web:161382`); `<task-notification>…</task-notification>` →
  a notification card (`AD`, `web:161462`: `task-id`, `tool-use-id`, `status`, `summary`,
  `result`, `usage` from `subagent_tokens`/`tool_uses`/`duration_ms`; extra text after the tag or a
  `<` inside `summary` — not a card); a message from another session; wrapped notifications
  (`<teammate-message>`, `<bash-stdout>`, `<bash-stderr>`, `<tick>`, `mcp-resource-update`,
  `mcp-polling-update`, `<local-command-caveat>`, `Qjt`, `web:161338`–`161351`); `<ide_selection>`,
  `<ide_opened_file>`, `<terminal …>`, `<browser …>`; `<local-command-stdout>X</…>` → a command
  result, `<local-command-stderr>` → a result with an error (`Aqt`, `web:161888`–`161934`);
  `<post-tool-use-hook><ide_diagnostics>` → diagnostics; otherwise text, and
  `<command-name>…<command-args>…` becomes the line `/command arguments` (`Eqt`,
  `web:161845`; `wUe`, `web:160916`).

### `user` with `isReplay: true`

The echo of one's own message (`--replay-user-messages`), `web:171151`–`171174`:
- If a line with this `uuid` already exists (the extension sent it itself): the text is replaced by
  the incoming one (`aUe`, `web:160577`), `createdAt` follows the echo's `timestamp`; no new line.
- If not (the message did not come from this tab): the line is inserted at
  `replayInsertIndex` (the end of the last finished turn) — or at the end of the feed if the message
  is not a human's (`vGe`), the turn was started by this tab, or the turn had tools; a non-human
  message during a turn after the answer is marked `foldedIntoTurn`.
- The echo releases the "held" messages up to and including it (`heldPickupFor`,
  `web:169228`–`169231`) and, if there was no answer yet, counts as the start of the turn.
- The echo of one's own message that was waiting for a session title (`unnamedSends`) gives the
  session its title if the text is not a service text (`KWe`, `web:160338`; `web:171153`–`171156`).

### `user` — the compaction summary (`isCompactSummary`, live stream)

Right after `system/compact_boundary` the extension keeps the compaction line open
(`pendingCompactMessage`); the next `user` with `isSynthetic` and a string `content` does not go
into the feed but becomes the text of the collapsed "Compacted chat" line (`web:171144`–`171150`).
Any other message except `prompt_suggestion` and `command_lifecycle` ends this wait.
In the main session `isCompactSummary` also marks the prompt cache "compacted" (`web:171286`–`171289`).

### `user` / `assistant` with `parent_tool_use_id` (subagents)

Subagent lines go into the same flat feed. On `assistant`, `parent_tool_use_id` goes only into
`sdkParentToolUseId`; `hiddenFromChat` is set only in the live stream, if
`$Ve(agent map, feed, parent_tool_use_id)` is true (`web:171186`–`171191`, `web:163030`–`163047`);
hidden lines are not drawn (`web:261269`). The rules of `$Ve`:
1. Find the agent whose launch or "wake" call is the frame's `parent_tool_use_id`
   (`[toolUseId, ...wakeToolUseIds]`, `web:162751`–`162753`; `wakeToolUseIds` grow when the same
   task arrives in a `task_started` with a different `tool_use_id`, `web:162705`–`162735`).
2. The agent is in the background (`isBackgrounded === true`): the frame is hidden if the last
   launch/wake call of the agent itself or of its background ancestor sits in a visible feed line as
   a `tool_use` `Agent`/`Task`/`Skill`/`skill__*`/`SendMessage` (`web:163049`–`163068`); if there
   is no such line (evicted, hidden), the frame is shown.
3. The agent is known but `isBackgrounded` is undefined — shown.
4. The agent is not found or not in the background — hidden if the launch call itself was made
   inside a subagent (`aj`/`nfe`, `web:162999`–`163011`).

`isBackgrounded` for agents from history comes from the call's `run_in_background` (a result is
present — `false`, `web:163138`–`163150`); in the live stream, from `is_backgrounded` in
`task_started`/`task_updated`. History has no subagent lines at all (`isSidechain` is dropped,
`ext:57390`). A subagent's stream deltas are assembled by a separate assembler per
`parent_tool_use_id` (`web:163703`–`163715`). `Agent`/`Task` calls in an answer add to the agent map
(`web:171192`–`171197`). Only lines without `parent_tool_use_id` affect spending, the model,
`TodoWrite`, the prompt cache and "there was an answer" (`web:171283`–`171331`).

### `assistant`

- `Ky`: `betaMessageId` = `message.id`, `model` (not `<synthetic>`), `isSynthesizedByLoop` =
  `message.model === "<synthetic>"`, `failure` from `error`, `api_error`, `api_error_params.remedy`,
  `commandPlugin` from `local_command_run.plugin`, the `narration_hint` hint `hidden`/`faint` on
  all blocks (`web:160539`–`160564`).
- `narration_hint` is read only on a `thinking` block whose signature decodes to `narration` and
  whose text is not empty (`U5`, `web:227905`–`227927`): such a block is drawn not as a collapsible
  "Thinking" but as a line of text with the tail " · summarized" (no hint), " · summary" (`faint`)
  or no tail (`hidden`) (`web:227929`–`227940`, `web:230724`–`230725`); other blocks do not use the
  hint. In a live frame the CLI itself sets it, only with `entrypoint: claude-vscode`
  (`qo`, `cli:723336`–`723342`) — that is, for the hqc worker too; in history the host adds it
  itself: `hidden` for models with `quizzical_shore` or `opus_5_5_prompt_bundle`, otherwise `faint`
  (`ext:123048`–`123104`, `ext:137571`).
- Insertion (`j8`, `web:172041`–`172071`), if the extension saw stream events: look for a line with
  the same `uuid`, otherwise a streamed line without a `uuid` with the same `message.id` and the same
  type of the first block (empty text is not matched); the found one is replaced and block timings
  are carried over. Otherwise a new line at the end.
- A final main-session answer with a `message.id` different from the current streamed attempt's
  removes that attempt's unfinished streamed lines (`retireAbandonedStreamedRows`,
  `web:171092`–`171099`, `web:169512`).
- `supersedes` (a list of `uuid`s) on `assistant` removes lines with those `uuid`s from the feed
  (`Eue`, `web:153828`; `evictRetractedMessages`, `web:169482`).
- `error` + `api_error`: on `authentication_failed` (not a provider, not 3p) and on the gateway
  errors `gateway_signin_required`/`gateway_session_expired` the extension opens sign-in (`ffe`,
  `web:163669`; `web:171300`–`171311`).
- A main-session `TodoWrite` → the task list (`web:171312`–`171320`); `usage` → the token counter
  (`web:171321`–`171329`, `updateUsage`, `web:171732`).
- Drawing: if all blocks are tools hidden by their own renderer, the line is not drawn
  (`web:261270`). Call status: no result and the session not busy → "failure", `is_error` →
  "failure" (`rVe`, `web:161966`).

### `assistant` with `message.model: "<synthetic>"`

Drawn as an ordinary answer (`qA` has no exception for it), but: it does not count as the turn's
"answer" (`turnHadReplyFrame`, `web:171291`–`171295`), does not update the model and the prompt cache
(`web:171322`, `web:171330`), does not reset the streamed attempt (`web:171094`), is not copied
by the copy button (`CLe`, `web:234642`). The error (`error`) is shown through `failure`.

### `stream_event`

The assembler `mfe`/`T$e` (`web:163693`–`163783`), one per `parent_tool_use_id` (`"root"` for the
main session):
- A main-session `message_start` is a new attempt: streamed lines of the previous attempt that no
  final `assistant` replaced (a request retry, a cut) are removed, and the `message.id` is remembered
  (`web:163707`–`163711`, `web:168375`–`168384`).
- `content_block_start` — a new `assistant` line without a `uuid`, with `betaMessageId` and
  `blockIndex`, the block marked unfinished; `type: "fallback"` blocks do not go into the feed
  (`web:163735`–`163739`, the line is created at `web:168366`–`168370`).
- `content_block_delta` (`yKt`, `web:163798`–`163844`): `text_delta` — appended to the text;
  `citations_delta` — to the citations; `input_json_delta` — accumulates the input JSON (`tool_use`,
  `server_tool_use`); `thinking_delta` — to `thinking` (skipped for `redacted_thinking`),
  `estimated_tokens` in the delta is an estimate of thinking tokens; `signature_delta` — the
  signature; `compaction_delta` — skipped; a delta of a type other than the block's is an exception:
  the frame is skipped and logged to the console (there is no placeholder line for `stream_event`).
- `content_block_stop` — the block is done, the `tool_use` input is parsed from JSON (if it fails to
  parse, it stays a string, `xKt`, `web:163846`).
- `message_delta` — `stop_reason`, `usage`; `message_stop` — the assembler is reset.
- Any `stream_event` except `ping` marks "there was an answer" (`web:171291`–`171295`).
- Consecutive successful `Read`s are merged into "ReadCoalesced" right as lines are added
  (`lfe`, `web:163298`; `web:168368`).

### `system/thinking_tokens`

`estimated_tokens` → the thinking token estimate; after the first such message the estimate from
deltas is no longer used (`PKe`, `web:166620`; `web:171279`–`171282`, `web:168371`–`168374`).

### `system/init`

The session is busy, turn flags are reset (`web:171239`–`171247`); permission mode, model,
`fast_mode_state`, `analytics_disabled`. If `session_id` differs from the current one, **the feed
is cleared entirely**: title, messages, tasks, agent map, spending, prompt cache
(`web:171347`–`171386`). The first `init`/`status` with `launchTiming` or `connectSnapshot` marks
the channel ready (`web:170881`–`170886`).

### `system/status`

Status text (`status`) unless it is the hook counter; `launchTiming` — the process is up;
`startupHooksRunning`, `userPromptSubmitHooksRunning`; `sessionHeldElsewhere` — another process
holds the session; `interruptedTurnLeft` + `newestMessageAt` — inserts "No reply was recorded for
this message…"; `resume_held_for_plan`; `resumeDropped` — the session is not resumed;
`permissionMode` (`web:171416`–`171470`).

### `system/compact_boundary`

A new `compact` line ("Compacted chat · auto|manual · N k tokens freed", `HTt`,
`web:235028`–`235055`) at the end of the feed and a wait for the summary (`web:171141`–`171143`);
the token counter is 0, the prompt cache is "compacted" (`web:171473`–`171475`). Without a summary
the line reads "Conversation was compacted to free up context.".

### `system/informational`

With `tag: "brass_thimble"` — a `meta` line with `content` (`web:171471`). Otherwise a `meta` line
only when `level` is not `info`, the text is not empty, and the feed already has a conversation
(`sme`, `web:172224`; `web:171481`–`171486`).

### `system/task_started`, `task_progress`, `task_updated`, `task_notification`, `background_tasks_changed`

They give no feed lines, only state (`web:171551`–`171724`):
- `task_started`: non-background `local_bash`/`local_agent` with a `tool_use_id` — a "foreground
  call"; for a `task_type` other than `local_agent` the background task's start time is remembered;
  for `local_agent` — an entry in `subagentTasks` (description, prompt, background) and in the
  agent map.
- `task_progress`: `usage` (tokens, calls, duration), `summary`, the last 3 tools.
- `task_updated`: `patch.status` `completed`/`failed`/`killed` removes the subagent from the
  "running" ones; a change of `is_backgrounded` is an update.
- `task_notification`: the outcome into the agent map (`status`, `summary`, `usage`, `output_file`);
  for tasks not in the map — the result (`completed` → "finished"), no more than 1000 entries.
- `background_tasks_changed`: background subagents that are not in `tasks` are removed; the list of
  background tasks (`local_agent`/`local_workflow`) and the descriptions of the others.

### `system/model_refusal_fallback`, `model_consent_fallback`, `model_fallback`

`model_refusal_fallback` (with the gate on) — a `refusal_fallback` line and removal of the lines in
`retracted_message_uuids` (`HPe`, `web:153767`; `web:171121`–`171140`). `model_consent_fallback` —
a switch to the default model and a notification (`web:171100`–`171120`). `model_fallback` —
remembers the turn's fallback model (`web:171069`–`171072`).

### `system/notification`, `system/per_turn_effort_changed`

`notification` with the key `fast-mode-overage-rejected` — fast mode is turned off and a
notification shown (`web:170981`–`171014`); others are not handled. `per_turn_effort_changed` clears
"ultrathink available" (`web:171067`).

### `system/api_retry`, `system/hook_*`, `tool_progress`, `tool_use_summary`, `auth_status`

`api_retry` never appears in the extension's parsing (`grep -c api_retry` over the webview = 0) —
a request retry is not visible. `hook_*` do not reach the webview (8.0). `tool_progress`,
`tool_use_summary`, `auth_status` give no lines (`Ky`, `web:160567`–`160575`); the extension has
no other `tool_progress` handlers (2 occurrences: `Ky` and the kind table `web:167739`).

### `command_lifecycle`

Gives no lines (`web:171073`–`171085`). `ran_ahead: true` — the held message gets the mark "Ran
at once, while Claude was working."; `started` — held messages up to and including this
`command_uuid` go into the feed; `completed`/`cancelled`/`discarded`/`refused` — this one message
goes into the feed (`heldPickupFor`, `web:169219`–`169227`). Any `command_lifecycle` takes
`command_uuid` off the wait for confirmation (`web:170964`–`170966`).

### `result`

End of turn (`endTurn`), `replayInsertIndex` is the end of the feed, turn flags are reset
(`web:171543`–`171548`, `web:171248`); `total_cost_usd` and `modelUsage[model]` →
`contextWindow`, `maxOutputTokens` (`web:171487`–`171501`); `fast_mode_state`; `user_message_uuid(s)`
— confirmation of the sent messages (`web:170967`–`170971`). If the stream ended without a `result`
while the session was busy, or with unconfirmed sends — the error "The connection to Claude Code
ended before this message completed — it may not have been processed, so please send it again."
(`web:170957`–`170963`).

### `rate_limit_event`

`rate_limit_info` → the limit bar: `status: "allowed"` — nothing; `rejected` — "You've hit your
5-hour limit · resets …" / "Usage is paused…" / an offer of credits; otherwise, with `utilization`
≥ 0.7 — "You've used N% of your …" (`_en`, `web:172185`–`172222`; `web:171509`–`171542`).

### `prompt_suggestion`

Only a suggestion for the next message (`web:171040`–`171043`).

### 8.2 History: what the extension gets on opening

The call chain: `loadFromServer` (`web:168861`) → the host's `getSession` (`ext:128502`) →
`readSessionForHost` (`ext:58318`) → the SDK reader (`ADn`, `ext:87619`; the same logic as
`getSessionMessages`, `d312:926`) → `readTrailingHookNotices` (`ext:58337`) → the response
`get_session_response {messages, sessionDiffs, liveInOtherSurface}`.

The SDK reader (line references are to the first copy):
1. Reads the file; if it is larger than 5 MiB (`qxe`, `ext:27397`) and
   `CLAUDE_CODE_DISABLE_PRECOMPACT_SKIP` is not set — only the part after the last compaction boundary
   (`m3t`, `ext:56908`–`56923`; the preserved tail is accounted for by `Vxe`, `ext:27174`).
2. Takes only `user`/`assistant`/`progress`/`system`/`attachment` lines with a string `uuid`;
   drops all other kinds (7.3); along the way counts `queue-operation remove
   absorbed_mid_turn` by `deliveryId`/`commandUuid` (`b3t`, `ext:56998`; `_3t`, `ext:56944`).
3. A repeated `uuid` — the last line wins (a `Map` by `uuid`, `ext:57036`–`57037`).
4. Attaches the preserved compaction tails (`ext:57039`–`57064`).
5. Chooses the leaf: the last line in file order without children, not `isSidechain`, not of a team,
   not `progress`, not `attachment/fork_briefing`, with a `user`/`assistant` up the chain from it;
   otherwise the latest of the found `user`/`assistant`, preferring non-`isMeta`
   (`S3t`, `ext:57065`–`57116`). Branches that do not lead to this leaf do not go into history.
6. Walks from the leaf to the root (`ext:57117`–`57126`) and inserts the lines with the same
   `message.id` left on neighboring branches, and the `tool_result`s for them (parallel calls, `q6`,
   `ext:57143`–`57264`).
7. Adds the `attachment/queued_command`s that hang after the leaf (`A3t`, `ext:57421`).
8. `kA` (`ext:57310`): after an `isMeta` with `<local-command-caveat>`, the `<command-name>` and
   `<local-command-stdout|stderr>` lines get `isCompletedLocalCommand` (`v3t`, `ext:57290`;
   tags — `GTe`, `ext:56862`); `attachment/queued_command` becomes a `user` with
   `uuid` = `source_uuid` (or the attachment's `uuid`), `isQueuedCommand: true`, `origin` (`C3t`,
   `ext:57344`; `BTe`, `ext:56770`).
9. Filter (`x3t`, `ext:57386`): only `user`/`assistant` (`system` only with
   `includeSystemMessages`, which the host does not pass, `ext:128513`); `isMeta` only with an
   `origin` of kind `channel`/`observer`/`observer-activity`/`slack-ping`/`voice-call`/`peer` (`CA`,
   `ext:56834`); `isSidechain` and `teamName` are dropped.
10. Shape (`V6`, `ext:57395`): `type`, `uuid`, `session_id`, `message`, `parent_tool_use_id:
    null`, `interruptedByShutdown`, `toolDenialUnanswered`, `isCompactSummary`, `is_meta` (if
    `isMeta`, `isCompactSummary` or `isVisibleInTranscriptOnly`), `isQueuedCommand`,
    `isCompletedLocalCommand`, `timestamp`, `origin`. `toolUseResult` and the line's other fields
    do not reach the webview.

The host appends the `system/informational` messages with `preventContinuation: true`, not `info`,
that hang after the last message and are not followed by a `user` (`U3t`, `ext:58609`), and in a VS
Code window stamps answers with the `narration_hint` by model (`stampReplayedFrames`, `ext:128565`;
`dVe`, `ext:123092`; `ext:137571`).

The webview (`web:168873`–`168970`) runs each message through: `system/informational` → a `meta`
line; the rest — `processMessage` (counters, tasks), removal of retracted lines (`hGe`,
`web:171983`) and `j8` without the streaming logic; `foldedIntoTurn` is set on an `isQueuedCommand`
that came right after a `tool_result` of the same answer (`rGe`, `web:167927`–`167948`). Then `Read`
merging (`C8`, `web:163312`), trimming to the line limit (`ome`/`wGe`), resetting tasks and
background tasks, the agent map from history, restoring answers to questions (`jpe`, `web:162128`),
a note about the prompt cache, starting the process (`launchClaude`) and, if the last
`AskUserQuestion` has no answer, showing the question again (`maybeReplayUnansweredQuestion`,
`web:168984`; `Upe`, `web:161989`).

Differences between history and the live stream that follow from this:
- **Compaction is not marked in history**: there is no boundary (it is `system`), the summary
  arrives as a `user` line with `is_meta`/`isCompactSummary` and no `origin`, so `isSynthetic` is
  not set and `qA` draws it as an ordinary user bubble (`oBn`/`YFt`, `web:260968`–`261040`) with the
  full text "This session is being continued…": the extension has no special handling for this text,
  and there is no "Compacted chat" line — it is born only from a live `compact_boundary` (`rUe`,
  `web:160520`–`160522`). `notAPrompt: true` removes it from navigation over the user's messages
  (`Qfe`, `web:166883`–`166895`) and from the search for a "prompt without an answer" (`Sje`,
  `web:165122`). Traced in the code; nobody looked at the extension with their own eyes.
- `isMeta` lines (`[Image #N]`, `<local-command-caveat>`, reminders) are not visible in history.
- In history `<task-notification>` is an ordinary `user` line, drawn as a notification card.
- Spending and the token counter after opening come only from the `usage` of the history's answers;
  the file has no `result`.

### 8.3 What is drawn and what is not (summary of `qA`, `web:261240`–`261318`)

| Line | Drawn? |
|---|---|
| any with `isEmpty` | no |
| `user` with `parentToolUseId` | no (except read-only mode with text) |
| `user` with `isSynthetic` | no, except a message from another session |
| `user`, other | yes, as blocks per `pc` |
| `assistant` with `hiddenFromChat` | no |
| `assistant`, all blocks hidden tools | no |
| `assistant`, other, including `<synthetic>` | yes |
| `meta` | as text |
| `compact` | as a collapsed "Compacted chat" line |
| `refusal_fallback` | as a refusal banner |

## 9. Extension logic not tied to one message

- **Order.** The feed is an array in arrival order. `replayInsertIndex` is the place for echoes of
  others' messages (the end of the last finished turn): after `result` it equals the feed length
  (`web:171543`–`171544`); it shifts when `meta` lines are inserted and on eviction
  (`web:169270`–`169273`, `shiftIndicesPastEvicted`, `web:169503`).
- **One's own message.** `send` (`web:169066`) puts a line with a new `uuid` right away
  (`crypto.randomUUID()`, `web:169114`); if a turn is running, the extension's message waits in
  `heldPrompts` (`turnRunningOrQueued`, `web:168020`) and goes into the feed on `command_lifecycle
  started`/the end of the turn, or on its own echo (`web:169128`–`169134`, `web:169215`–`169254`). A
  repeated add is skipped if a line with this `uuid` already exists (`appendPickedUp`, `web:169241`).
- **Duplicates.** By `uuid`: an echo updates its line (`web:171152`–`171163`), a final
  `assistant` replaces the line with the same `uuid` or the streamed one with the same `message.id`
  (`web:172050`–`172069`); the "a line for the frame already exists" mark is `hasRowForFrame`
  (`web:170946`). Removal by a `uuid` list: `supersedes`, `retracted_message_uuids`
  (`web:169482`–`169503`). `j8` has no separate dedup of `user` by `uuid`: two `user` frames with
  one `uuid` without `isReplay` give two lines (per the code).
- **Line limit.** `wGe` (`web:171897`–`171950`), constants `Sq = 600`, `MJt = 300`, `xq = 100`,
  `PJt = 10` (`web:167672`–`167675`), `wq(l) = max(300, 600 − l)`:
  1. Up to 600 lines — nothing. Lines fall into three groups: "conversation" (`compact`,
     `refusal_fallback`, `meta`, main-session `assistant` with text, human messages, plus results of
     `Agent`/`Task`/`Skill`/`SendMessage` from conversation lines; `web:171767`–`171792`,
     `web:171882`–`171896`), hidden (`hiddenFromChat`) and other.
  2. Budgets: conversation above 600 — 500 stay; other and hidden — each group down to
     `wq(kept conversation) − 100` if it exceeds `wq(…)` (`web:171901`–`171908`).
  3. Protection levels 1–11 (`web:171793`–`171803`): hidden — 1; ordinary — 2; with text and
     `meta` — 3; a finished `Agent`/`Task` call — 4; a non-human prompt with `origin` — 5; a group
     member — 6 (running — 10); a group's turn — 7 (running — 10); the last 100 lines of the live
     feed — 8, the last 10 — 9; unfinished lines, running calls and streamed lines without `uuid` —
     9; lines of the last `TodoWrite` — 11. A `tool_result` inherits its call's level
     (`web:171915`–`171918`).
  4. Order: if conversation exceeds 500 — first its lines from oldest to newest (except
     `TodoWrite`), then everything that is not conversation up to the first remaining conversation
     line with a level below 9; then by level 1 → 11, oldest to newest, until the group's limit is
     exhausted; a call goes together with its `tool_result` (`web:171920`–`171943`).
  5. The live feed is trimmed with the last 100 protected (`rme`, `web:171219`–`171225`), history
     without it (`ome(…, protectRecent: !1)`, `web:171951`–`171953`).
- **History versus the live stream.** The file is read once per opening (`loadingPromise`
  is cached, `web:168862`); later only the file diffs (`reloadSessionDiff`,
  `web:168856`). Lines that a process other than the extension's appended to the file, the extension
  will not see until it is opened again (matches the cc_remote experience, `docs/SPEC.md` §2.4,
  method B). The `system` tail in the host's response does not count toward the carried-over lines
  (`web:168885`).
- **Session switch and reload.** `system/init` with another `session_id` clears the feed
  (`web:171347`–`171386`). `loadFromMessages` (`web:169014`) builds the feed from a ready list
  (teleport). After a window reload or an extension restart mid-turn, the extension resumes the
  turn and writes "Continuing the step that was interrupted when the window reloaded." or "No reply
  was recorded for this message before the session was reloaded." (`ame`,
  `web:172230`–`172241`; `web:168954`–`168968`, `web:170973`–`170980`).
- **Parse errors.** One broken record does not bring down the feed: a "Couldn't display…" line, a
  skip counter (`pendingRebuildSkips`, `web:168913`), telemetry.


---

## 10. What remains open

The other items of the earlier list were closed by the code or by live runs and moved into the
sections above.

1. The record "with a uuid the stream did not name" from DrZlo13/hqc#42 (7.6): neither joining
   during the final answer, nor messages mid-command, nor two frames at start produced it — in all
   cases the stream named the record's `uuid`.
2. Why old VS Code versions (2.1.209–2.1.280) wrote one `uuid` twice (7.5): the code of those
   versions is not available, the mechanism is a hypothesis.
3. Which exact message was the last in memory before the 8 compaction boundaries without a parent on
   disk (7.5): the mechanism is found, but the message itself did not reach the disk.
4. `system/files_persisted`: the sender was not found in the code; the frame did not appear in 33
   runs.
5. Where the ~27 s wait for an MCP server before the first turn comes from (1.2): none of the
   timeouts found (5 and 30 s) gives this number.
6. `priority:"now"`: the interrupt-and-cancel branch in `wi()` (3.2) could not be triggered live — a
   foreground Bash, MCP call and subagent are moved to the background; which call would be impossible
   to move was not found.
7. `Agent` without a `run_in_background` field went to the background with `claude-vscode` (5.6),
   and without `claude-vscode`, with `run_in_background:false`, ran in the foreground
   (`p9-now-agent-human`); other combinations were not captured.
8. How the extension draws a summary from history, answers with `narration_hint` and the hiding of
   subagent lines (8, 9) is traced only in the code: nobody looked at the extension with their own
   eyes — GUI automation on the laptop is forbidden.
9. The condition `s` in `zHt` (2.1, `assistant`): when subagents' text and thinking go into the
   stream with `initialize.forwardSubagentText` is not traced.
10. Each live case was captured with one run (except the place of the first prompt's echo — 24 runs)
    and one model (haiku); the `keep_alive` period was not measured live — without
    `CLAUDE_CODE_REMOTE` there are no frames, and that mode changes the transport.
