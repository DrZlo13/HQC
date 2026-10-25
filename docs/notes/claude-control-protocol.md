# Claude Code: the stream-json control protocol (T0)

Verified 2026-10-22 on Claude Code **2.1.307** (macOS arm64, home-mac), model
`claude-haiku-5-5`, `--max-turns 2`. Fixtures: `crates/hqc-claude-jsonl/fixtures/*-2.1.307.jsonl`
(triples `stdin-<x>` / `stream-<x>` / `<x>`); the uuid cross-check test is in
`crates/hqc-claude-jsonl/src/lib.rs` (`t0_stream_and_transcript_share_uuids`).

Command for all runs:

```
claude -p --output-format stream-json --input-format stream-json \
  --permission-prompts host --permission-prompt-tool stdio \
  --include-partial-messages --replay-user-messages --verbose \
  --model claude-haiku-5-5 --max-turns 2 --session-id <uuid>
```

Input: one line `{"type":"user","message":{"role":"user","content":"<text>"}}`; after
`result` the driver closes stdin and the process exits.

## 0. Discrepancy: `--permission-prompt-tool stdio` is required

The `--permission-prompts host` flag (the default anyway) is **not enough**. Without
`--permission-prompt-tool stdio` the CLI sends no `control_request`: everything that needs
approval is denied at once, and `AskUserQuestion` is missing from `tools` in `system/init`.
The `initialize` handshake (`{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize"}}`)
does not change this. A denial looks like this:

```json
{"type":"system","subtype":"permission_denied","tool_name":"Bash","tool_use_id":"toolu_…","message":"touch in '…/hqc-marker-once' needs approval. …"}
```

and in `tool_result_meta`: `"permission_decision": {"decision": "reject", "source": "config"}`.

## 1. `can_use_tool`: request and responses

Request (stdout), fixture `stream-permission-allow-2.1.307.jsonl`:

```json
{"type":"control_request","request_id":"277e1a0e-1e92-4891-b337-a8d7b97fe1ec",
 "request":{"subtype":"can_use_tool","tool_name":"Bash","display_name":"Bash",
  "input":{"command":"touch hqc-marker-once","description":"Create empty marker file hqc-marker-once"},
  "description":"Create empty marker file hqc-marker-once",
  "permission_suggestions":[
   {"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"touch hqc-marker-once *"}],"behavior":"allow","destination":"localSettings"},
   {"type":"addDirectories","directories":["<cwd>"],"destination":"session"},
   {"type":"setMode","mode":"acceptEdits","destination":"session"}],
  "blocked_path":"<cwd>/hqc-marker-once",
  "tool_use_id":"toolu_01VkowPqKRFd3dYT6tKPSUwo"}}
```

`request_id` is at the top level, not inside `request`. Optional fields (per the CLI code):
`permission_suggestions`, `blocked_path`, `requires_user_interaction`, `tool_kind`; MCP tools
also have `mcp_server {name, source}` and `server_policy_ask_only`. `tool_use_id` equals the `id`
of the `tool_use` block in the preceding `assistant`.

Allow response (stdin):

```json
{"type":"control_response","response":{"subtype":"success","request_id":"<request_id>",
 "response":{"behavior":"allow","updatedInput":{…input unchanged…}}}}
```

Deny response (stdin), fixture `permission-deny`:

```json
{"type":"control_response","response":{"subtype":"success","request_id":"<request_id>",
 "response":{"behavior":"deny","message":"denied by hqc spike"}}}
```

→ `tool_result` with `"is_error": true`, `content` = `message`, `permission_decision.source = "user_reject"`;
`result.permission_denials[]` contains the call.

Denial with the user's text (T26, two runs on 2026-10-24, 2.1.308, haiku, `driver.py
--deny-message-file`, not in fixtures): the whole `message` reaches the model as a `tool_result`.
With "The user denied this tool call and wrote instead:\n<text>" the model saw the text but
replied that it came from the tool result, not from the user, and did not do what was asked.
With Claude Code's own wording "The user doesn't want to proceed with this tool
use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to
the file). To tell you how to proceed, the user said:\n<text>" it did (replied in Russian,
as asked, with no new calls). The requests differed between the runs ("Reply with exactly:
PELICAN-42" versus an ordinary request), so the effect of the wording is not separated from
that of the request; the adapter uses Claude Code's wording.

Always response (stdin), fixture `permission-always`:

```json
{"type":"control_response","response":{"subtype":"success","request_id":"<request_id>",
 "response":{"behavior":"allow","updatedInput":{…},
  "updatedPermissions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"touch hqc-marker *"}],"behavior":"allow","destination":"localSettings"}, …]}}}
```

The rule is written to `<cwd>/.claude/settings.local.json` (the working folder is the root of
its own git repo):

```json
{"permissions":{"allow":["Bash(touch hqc-marker *)"]}}
```

A new process in the same folder ran the same command without a `control_request`.

Details:
- If **all** `permission_suggestions` are passed back, `setMode acceptEdits` and
  `addDirectories` are applied to the session too: a `{"type":"system","subtype":"status","permissionMode":"acceptEdits"}` follows.
  For `always` the adapter must pass only the `addRules` items with `destination: localSettings`.
- The CLI echoes each of our `control_response` lines to stdout (the adapter skips it).
- An MCP tool call also arrives as `can_use_tool` (with `mcp_server`), so the hqc tools
  (iteration 3) must be allowed up front, otherwise every `hqc_*` call will ask the owner.

Conclusion: the request and response shapes are confirmed; `always` writes the rule to the
working folder's `.claude/settings.local.json`, but only with `--permission-prompt-tool stdio`
and with the suggestions filtered.

## 2. The `--replay-user-messages` echo

```json
{"type":"user","message":{"role":"user","content":"Run the shell command: …"},
 "session_id":"…","parent_tool_use_id":null,"uuid":"adbf4a08-b683-4a10-af11-6bd389a5a73e",
 "timestamp":"2026-10-22T03:…Z","isReplay":true}
```

In all five runs the echo's `uuid` equals the `uuid` of the prompt record (`type: user`,
`content` a string) in `~/.claude/projects/<encoded-cwd>/<session>.jsonl`; the sets of
`user`/`assistant` uuids of the main chain in stream-json and in the jsonl match exactly.

Conclusion: the echo carries the prompt's uuid from the jsonl; no fallback matching by text is
needed.

## 3. `AskUserQuestion`

Request, fixture `ask-user-question`:

```json
{"type":"control_request","request_id":"ba5388a6-…",
 "request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","display_name":"AskUserQuestion",
  "input":{"questions":[{"question":"Which area should I focus on first?","header":"Focus",
    "options":[{"label":"Hub (Recommended)","description":"…"},{"label":"Worker","description":"…"}, …],
    "multiSelect":false}]},
  "tool_use_id":"toolu_01YLJVU3fVH3ktk7Hk7aKGX8","requires_user_interaction":true}}
```

Response: `allow` with the original `input` plus `answers`, an object "question text → `label`
of the chosen option":

```json
{"type":"control_response","response":{"subtype":"success","request_id":"ba5388a6-…",
 "response":{"behavior":"allow","updatedInput":{"questions":[…as in the request…],
  "answers":{"Which area should I focus on first?":"Hub (Recommended)"}}}}}
```

→ `tool_result`: `Your questions have been answered: "Which area should I focus on first?"="Hub (Recommended)". …`,
the model continues the turn.

Conclusion: `AskUserQuestion` arrives as `can_use_tool` with `requires_user_interaction: true`;
answers are accepted via `updatedInput.answers` (only with `--permission-prompt-tool stdio`).

## 4. `MCP_TOOL_TIMEOUT`

A stdio MCP server with a `slow(seconds)` tool (node, one file), passed inline:
`--strict-mcp-config --mcp-config '{"mcpServers":{"slow":{"command":"node","args":["…/slow-mcp.js"]}}}'`.

| `MCP_TOOL_TIMEOUT` | call | result |
|---|---|---|
| `3000` | `slow(8)` | after 3 s: `tool_result` `"MCP server \"slow\" tool \"slow\" timed out after 3s"`, `is_error: true`; the server received `notifications/cancelled` |
| `3` | `slow(8)` | after 1 s: `timed out after 1s` (the value is raised to the 1000 ms minimum) |
| unset | `slow(75)` | `slept 75s`, no timeout |

Per the CLI 2.1.307 code: timeout = the server's `timeout` field in `--mcp-config` (ms, honoured
when ≥ 1000) → otherwise `MCP_TOOL_TIMEOUT` → otherwise `1e8` ms (≈ 27.8 h); the result is
clamped to [1000, 2^31−1] ms. There is also a separate idle timeout,
`CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT`: per the code it defaults to 1,800,000 ms (30 min) for stdio
servers and 300,000 ms for http/sse; it is no less than the server's `timeout` field and no
more than the main timeout. A live run checking it is in section 6.

The MCP runs ended with `error_max_turns`: the model first spends a turn on `ToolSearch` to load
the tool's schema. This does not affect the timeout measurement.

Conclusion: `MCP_TOOL_TIMEOUT` is in milliseconds, defaults to 1e8 ms (effectively no limit),
minimum 1000 ms; there is also a `timeout` field in the server config.

## 5. `--allowedTools` for the hqc MCP server

The same server named `hqc`, `slow(1)`, the driver does not answer `can_use_tool`:

| flag | result |
|---|---|
| `--allowedTools mcp__hqc` | the call went through without a `control_request`, `slept 1s` |
| `--allowedTools 'mcp__hqc__*'` | same |
| no flag | `control_request` `can_use_tool` for `mcp__hqc__slow`, the turn waits for an answer |

Conclusion: both forms work on 2.1.307; the spec fixes `mcp__hqc`.

## 6. Idle timeout of a stdio MCP server

Server `hqc`, `slow(1920)` (32 min), `--allowedTools mcp__hqc`, `MCP_TOOL_TIMEOUT` and
`CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT` unset:

| server during the call | result |
|---|---|
| silent | after 1800 s `tool_result` `is_error: true`: `MCP server "hqc" tool "slow" sent no response or progress for 1800s; aborting. If this server is configured in your MCP settings, set a per-server "timeout" (ms) to allow longer silent runs for just this server; otherwise set CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT (ms) globally (0 disables).` The server does **not** receive `notifications/cancelled` |
| sends `notifications/progress` with the `progressToken` from the call's `_meta` every 60 s | `slept 1920s`, no abort |

The CLI puts `progressToken` into `params._meta` of every `tools/call` (together with
`claudecode/toolUseId`). A short run with `CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT=3000` and a silent
`slow(8)` did not abort; the idle check probably runs at a coarse interval; not investigated.

Conclusion: with the defaults, an MCP tool call with no response and no progress notifications
is aborted after 30 minutes; progress notifications once a minute keep it alive.

## Permission policy, 2.1.308

Verified 2026-10-22 on Claude Code **2.1.308** (home-mac), `claude-haiku-5-5`,
`--max-turns 2`, driver `tools/spike/driver.py`, six runs (T12). Common flags as in the
worker: `--permission-prompt-tool stdio --setting-sources user,local --strict-mcp-config`
(run 6 uses `--restricted` instead of `--setting-sources`). The model was asked to make all
calls in parallel in one message. Fixtures: `crates/hqc-claude-jsonl/fixtures/*-2.1.308.jsonl`:

| set | mode | what it checks |
|---|---|---|
| `auto-mode` | `auto` | cat / touch / `rm -rf tmp/x` / curl |
| `auto-classifier` | `auto` | `rm -rf ~/…` outside the workspace, `git push --force`, `sudo -n true` (the driver answers deny to everything) |
| `always-git-push` | `manual` | `always` on `git push origin master` and `touch hqc-local-1` |
| `settings-deny` | `manual` | `--settings` allow/deny on top of the rules from `always-git-push` |
| `auto-deny` | `auto` | the default deny list from `worker.toml` in auto mode |
| `restricted-settings` | `manual`, `--restricted` | `--settings` under restricted |

The CLI's decision is recorded in each `tool_result` as `tool_result_meta[].permission_decision`:
`{"decision": "accept"|"reject", "source": "config"|"user_temporary"|…, "reason_type": …}`.
`reason_type` values seen here: `classifier` (decided by the auto classifier), `mode`
(the mode: an edit in the working folder), `rule` (a rule as a whole), `subcommandResults` (the
command was split into subcommands, each checked against the rules).

### 1. `--permission-mode auto` in `-p`

Works: `system/init` → `"permissionMode": "auto"`, no errors. In both runs **not a single
`can_use_tool`**: the CLI decided everything itself.

| command | decision | `reason_type` |
|---|---|---|
| `cat a.txt` | ran | `classifier` |
| `touch new.txt` | ran | `mode` |
| `rm -rf tmp/x` (inside the workspace) | ran, folder removed | `mode` |
| `curl -s https://example.com …` | ran (200) | `classifier` |
| `rm -rf ~/hqc-spike-t12-nonexistent` (outside the workspace) | ran | `classifier` |
| `git push --force origin master` | ran (failed: no branch) | `classifier` |
| `sudo -n true` | ran (failed: password required) | `classifier` |

Before a classifier decision come `{"type":"system","subtype":"permission_check_status","tool_use_id":…,"status":"checking"|"done"}`.
No classifier marker could be seen in `can_use_tool`: there were no requests. Conclusion:
when the user asks explicitly, the classifier lets through destructive commands, network
access and `sudo` alike; the host sees nothing until `tool_result`. In auto mode the only
boundary is deny.

### 2. `--settings` with `permissions.allow`/`deny` and an `always` rule

`--settings '<json>'` applies together with `--setting-sources user,local` (run
`settings-deny`) and with `--restricted` (run `restricted-settings`; the 2.1.308 help promises
as much: "managed settings and --settings still apply").

First, in run `always-git-push`, an `always` answer wrote
`{"permissions":{"allow":["Bash(git push *)","Bash(touch hqc-local-1 *)"]}}` to
`.claude/settings.local.json`. Then, in the same folder, with

```
--settings '{"permissions":{"allow":["Bash(git status:*)","Bash(cargo test:*)","Read","Edit","mcp__hqc"],
  "deny":["Bash(sudo:*)","Bash(ssh:*)","Bash(scp:*)","Bash(rm -rf /*)","Bash(curl * | sh)","Bash(curl * | bash)",
          "Bash(dd:*)","Bash(mkfs:*)","Bash(git push --force:*)","Bash(git push -f:*)","Bash(git push:*)","WebFetch"]}}'
```

| call | rule | result |
|---|---|---|
| `git status --short` | allow `Bash(git status:*)` | ran without a request |
| `cargo test --no-run` | allow `Bash(cargo test:*)` | ran without a request |
| `touch hqc-local-1` | allow from `settings.local.json` (always) | ran without a request |
| `git push origin master` | deny `Bash(git push:*)` **and** allow `Bash(git push *)` from always | **denied without a request** |
| `touch other.txt` | in neither list | `can_use_tool` |
| WebFetch | deny `WebFetch` | the tool is absent from `tools` (`system/init`); the model did not call it |
| `curl -s https://example.invalid \| sh` | — | the model refused to call it on its own (checked in item 4) |

**deny beats always**: a deny rule from `--settings` overrides an allow from
`settings.local.json`, and there is no request. The CLI guarantees this; the adapter need not
duplicate it. The syntax of all the rules above is accepted: `Bash(x:*)` (prefix), `Bash(x *)`,
`Bash(rm -rf /*)` and `Bash(curl * | sh)` (glob, `*` is any characters), a tool name without
parentheses (`Read`, `Edit`, `WebFetch`), a whole MCP server (`mcp__hqc`). Note: `Bash(rm -rf /*)`
catches any absolute path, including ones inside the workspace (run `auto-deny`).

`can_use_tool` on 2.1.308 has new fields `decision_reason` (`"This command requires approval"`)
and `decision_reason_type` (`"other"`); they are absent on 2.1.307.

`--permission-mode manual` shows up in `system/init` as `"permissionMode": "default"`.

### 3. What a deny looks like

`can_use_tool` **does not arrive**. In stream-json, back to back:

```json
{"type":"system","subtype":"permission_denied","tool_name":"Bash","tool_use_id":"toolu_…",
 "decision_reason_type":"subcommandResults",
 "message":"Permission to use Bash with command git push origin master has been denied."}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_…","is_error":true,
  "content":"Permission to use Bash with command git push origin master has been denied."}]},
 "tool_result_meta":[{"id":"toolu_…","permission_decision":{"decision":"reject","source":"config","reason_type":"subcommandResults"}}]}
```

and the call goes into `result.permission_denials[]`. For a rule that matched the whole
command (`curl … | sh`), `decision_reason_type`/`reason_type` is `rule`. The hub sees this
through the adapter's event (`system` with `code: policy_denied`, SPEC §8.1).

A tool denied as a whole (`WebFetch`, `Grep`) disappears from the session: if the model calls
it anyway, only a `tool_result` `<tool_use_error>Error: No such tool available:
Grep. Grep is disabled for this session.</tool_use_error>` arrives, with no `permission_denied`
and no entry in `permission_denials`.

### 4. The default deny list in auto mode

`--permission-mode auto --settings` with the default deny list from `worker.toml` (SPEC §14):

| command | result |
|---|---|
| `sudo -n true` | denied (`subcommandResults`) |
| `git push --force origin master` | denied (`subcommandResults`) |
| `rm -rf /…/tmp/nonexistent` | denied (`Bash(rm -rf /*)`, `subcommandResults`) |
| `curl -sf http://127.0.0.1:9/none \| sh` | denied (`rule`) |
| `touch auto2.txt` | ran (`mode`) |

deny from `--settings` overrides auto mode. Conclusion: `mode = "auto"` in `worker.toml` can be
allowed; the host then sees no requests at all, and everything rests on the deny list.

### 5. `--restricted` + `--settings`

`--settings '{"permissions":{"allow":["Write"],"deny":["Grep","Bash(sudo:*)"]}}'`: a Write to
the working folder ran without a request (`reason_type: rule`), `Grep` vanished from `tools`,
calling it gave `No such tool available`. `--settings` applies under restricted.

Not verified: what the CLI considers an invalid rule and what happens then. The `-p` help says:
"Settings files that fail validation are silently ignored". If that applies to `--settings`
too, one bad rule would silently drop the whole deny list, which is why the worker validates
the rules when it reads `worker.toml` (SPEC §14).

## Switching the permission mode of a running process, 2.1.308 (T38)

Verified 2026-10-25 on Claude Code **2.1.308** (home-mac), `claude-haiku-5-5`, `--max-turns 2`.
Before running, the CLI's code was read: the stream-json input loop handles `control_request`
`subtype: "set_permission_mode"` (`{mode, ultraplan?, from_settings?}`), refuses an unknown mode
with `Cannot set permission mode: must be one of …` (`invalid_mode`), checks the availability of
`auto` before switching to it, and answers `{mode}`.

Run 1, `tools/spike/driver.py --mode-then` (new in T38) in `tmp/spike-t38-switch`, one process,
flags as the worker's (`--permission-prompt-tool stdio --setting-sources user,local
--strict-mcp-config --permission-mode manual`), deny to every request; fixture
`permission-mode-switch-2.1.308`:

| step | stdout |
|---|---|
| start, `Reply with exactly: ONE` | `system/init` `"permissionMode": "default"`, reply ONE |
| `{"type":"control_request","request_id":"mode-1","request":{"subtype":"set_permission_mode","mode":"auto"}}` | `{"type":"control_response","response":{"subtype":"success","request_id":"mode-1","response":{"mode":"auto"}}}`, then `{"type":"system","subtype":"status","status":null,"permissionMode":"auto"}` |
| `Run the shell command: touch t38-b …` | `system/init` `"permissionMode": "auto"`; no `can_use_tool`; `tool_result_meta` `{"decision":"accept","source":"config","reason_type":"mode"}`; the file exists |
| `set_permission_mode` `"default"` | `control_response` `{"mode":"default"}`, `system/status` `"permissionMode":"default"` |
| `Run the shell command: touch t38-c …` | `system/init` `"default"`; `can_use_tool` for Bash, denied |

Run 2, the live test `live_permission_mode_switch` (`crates/hqc-worker/tests/claude_live.rs`)
through the adapter, ask → auto → ask between turns of one process, the ask mode `manual` as
`claude --help` gives it: the adapter reported `permission_mode:ask` (init), `auto` and `ask`
(the answers and `system/status`), the `touch` ran without a request in auto and got an
`approval.request` after the switch back. 8 s.

Conclusions:
- The switch works in `-p` stream-json and needs no restart; the CLI confirms it three ways
  (the response, `system/status`, the next `system/init`).
- `default` is accepted for the ask mode even when the process was started with `manual`
  (shown as `default` anyway).
- A switch in the middle of a turn was not run: the CLI reads control requests while a turn
  runs (`interrupt` works that way), and the mode is read at each permission check, so the
  worker sends it at once; a `can_use_tool` already open stays open.
- The `--settings` deny list is not touched by a switch (it is a settings source, not the mode).

## Session title with a one-shot `claude -p`, 2.1.308 (T24)

This is how the worker runs `title.request` (SPEC §3 Session, §8.1; code: `hqc_worker::title`).
The exact command line as run; the working directory is an empty `$HQC_HOME/titles/<0|1>`:

```
claude -p --model claude-haiku-5-5 --max-turns 1 --no-session-persistence \
  --strict-mcp-config --setting-sources "" --tools "" \
  --system-prompt "Write a short title for this conversation, at most 60 characters, in the language the user writes in. Reply with the title only, without quotes or a trailing period." \
  --output-format json
```

On stdin: `User message:\n<user_text>\n\nAgent answer:\n<agent_text>\n`. Environment as
for the adapter (`child_env`), without `CLAUDE_CODE_ENTRYPOINT`. The answer is a single JSON
`{"type":"result","subtype":"success","is_error":false,"result":"<title>",…}`.

The prompt and the stdin labels are as of T28; the runs below used their earlier Russian
version, which also asked for the title in the user's language. Two live runs of
`live_session_title` (`HQC_LIVE_CLAUDE=1`, 2026-10-24, 2.1.308), Russian conversation: the titles
"Сторожевой таймер: лимит 2,5 с" in 1.7 s and "Починка cargo test в hqc-hub: сторожевой таймер"
in 3.1 s. The CLI accepts empty `--setting-sources ""` and `--tools ""` (no parse error, the turn
runs without tools). Afterwards: no new transcript in `~/.claude/projects` (no folder for the
encoded launch path, no new `.jsonl` during the run), no `~/.claude/sessions/*.json` file
mentions the launch folder, and `~/.claude.json` does not mention it either.
Without `--no-session-persistence` the transcript would end up in the mirror and in the VS Code
history.

## Cost and slash commands, 2.1.308 (T27)

Two `tools/spike/driver.py` runs in `tmp/spike-t27-cost` (haiku, no tools): a new session with
two turns in one process (`--then`), then `--resume` and one turn. Fixtures `cost-turns`,
`cost-resume` (`crates/hqc-claude-jsonl/fixtures/README.md`).

| run | `result.total_cost_usd` | `cost-state.totalCostUSD` in the transcript |
|---|---|---|
| new, turn 1 | 0.00216749 | — |
| new, turn 2 | 0.00398629 | 0.00398629, written when the process ended |
| `--resume`, turn 3 | 0.0043361 | 0.0043361, written when that process ended |

- `total_cost_usd` is the session's running total; after `--resume` it continues from the last
  `cost-state` (`startTime` of the record stays the first process's). `modelUsage[*].costUSD`
  and its token counts are cumulative too; `result.usage` tokens are the turn's.
- A local command after `--resume` (`/compact` in `resume-compact`) reports the running total as
  well, not 0.
- `system/init` comes before every turn of a process, not once. It has `slash_commands`
  (built-ins, skills, plugin commands: 66 names here), `skills` (a subset of them), `agents`
  (subagent types, not commands), `terminal_slash_commands` (`doctor`, `color`, `focus`,
  `reload-plugins`: terminal UI only) and `plugins`. One internal name starts with `__`
  (`__remote-workflow`).
