Scripts used for task T0 (docs/ITERATION-1.md) to probe the Claude Code stream-json control
protocol. Findings are in `docs/notes/claude-control-protocol.md`, captured runs in
`crates/hqc-claude-jsonl/fixtures/`.

Every run spends the subscription: keep to `claude-haiku-5-5` and `--max-turns` no more than 2
(the driver hardcodes both), and run inside an empty folder under `tmp/` (git-ignored).

## driver.py

Starts `claude -p` in stream-json mode with `--permission-prompts host
--include-partial-messages --replay-user-messages --verbose --model claude-haiku-5-5
--max-turns 2 --session-id <new uuid>`, sends one prompt, answers every `can_use_tool` by
policy and closes stdin after `result`.

```
tools/spike/driver.py [--resume UUID] [--arg-prompt] OUT_DIR CWD POLICY PROMPT [-- extra claude args]
```

- `--resume UUID` (or `SPIKE_RESUME`): continue that session with `--resume` instead of a new
  `--session-id` (T15: a takeover with the adapter's flags);
- `--arg-prompt` (or `SPIKE_ARG_PROMPT=1`): plain `claude -p PROMPT --model claude-haiku-5-5
  --max-turns 2`, the prompt as an argument and text output in `stdout.txt`, the way a person
  runs it in a terminal (T15: the "VS Code" side); `POLICY` is ignored.

- `POLICY`: `once` (allow), `deny`, `always` (allow + every `permission_suggestions` entry
  as `updatedPermissions`), `ask_answer` (answer `AskUserQuestion` with the first option of
  each question), `none` (never answer).
- Pass `--permission-prompt-tool stdio` after `--`: without it the CLI denies instead of
  asking (SPEC §8.1).
- `SPIKE_TIMEOUT` (seconds, default 180) kills the run; `SPIKE_INIT=1` sends an `initialize`
  control request first.
- `OUT_DIR` gets `run.json` (command, cwd, session id), `stdin.jsonl`, `stdout.jsonl`,
  `stderr.txt` and `timing.txt`. The transcript is
  `~/.claude/projects/<encoded cwd>/<session id>.jsonl`.

Example:

```
mkdir -p tmp/spike-allow && cd tmp/spike-allow
../../tools/spike/driver.py ../out/allow . once \
  "Run the shell command: touch hqc-marker. Then reply with exactly: DONE" \
  -- --permission-prompt-tool stdio
```

## slow-mcp.js

Stdio MCP server (node, no dependencies) with one tool `slow(seconds)` that sleeps and returns.

- `SLOW_MCP_LOG` — file to log every request and reply with timestamps;
- `SLOW_MCP_PROGRESS_MS` — send `notifications/progress` at this interval while sleeping.

```
CFG='{"mcpServers":{"hqc":{"command":"node","args":["/abs/path/tools/spike/slow-mcp.js"],"env":{"SLOW_MCP_LOG":"/abs/path/mcp.log"}}}}'
MCP_TOOL_TIMEOUT=3000 tools/spike/driver.py OUT . none \
  "Call the tool mcp__hqc__slow with seconds=8 exactly once. Then reply with exactly: DONE" \
  -- --permission-prompt-tool stdio --strict-mcp-config --mcp-config "$CFG" --allowedTools mcp__hqc
```

## sanitize.py

Turns driver runs into fixtures following `crates/hqc-claude-jsonl/fixtures/README.md`. All
paths come from arguments; nothing is tied to a machine.

```
tools/spike/sanitize.py --runs RUNS_DIR --out crates/hqc-claude-jsonl/fixtures \
  --cli-version 2.1.307 [--projects ~/.claude/projects] [--replace OLD=NEW ...] \
  NAME=RUN [NAME=RUN ...]
```

For each `NAME=RUN` (`RUN` is a driver `OUT_DIR` under `RUNS_DIR`) it finds the transcript by
the session id in `run.json` and writes `<NAME>-<ver>.jsonl`, `stream-<NAME>-<ver>.jsonl`
and `stdin-<NAME>-<ver>.jsonl`. On the way it:

- drops `api-request*` records;
- replaces `attachment` and `rendered` of attachment records and the file-history snapshot
  with `{"stripped":true}`;
- drops every key containing `hook` (`hookInfos` and the like) and `system` hook events;
- trims `system/init` to a few fields;
- rewrites the run's cwd (and its `/private` and folder-name forms) to
  `/tmp/hqc-fixture/<NAME>`, then applies `--replace`, then `/Users/<name>` and
  `/home/<name>` to `/Users/user`, `/home/user`, and emails to `user@example.com`.

Finally it re-reads the written files and checks for each of those signs; if anything is left
it prints `LEFT file:line: reason` lines and exits with code 1. Review the diff before
committing anyway: the check knows only these patterns.
