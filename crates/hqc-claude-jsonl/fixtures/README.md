Fixtures captured from Claude Code 2.1.307 on macOS. Each set is one headless run:

- `<name>-<ver>.jsonl` — the `~/.claude/projects/<cwd>/<uuid>.jsonl` transcript;
- `stream-<name>-<ver>.jsonl` — stdout with `--output-format stream-json --verbose`;
- `stdin-<name>-<ver>.jsonl` — the stream-json frames we wrote to stdin (T0 sets only).

Sets:

- `tool-call` — one Bash call, no permission prompt (`--permission-mode acceptEdits`).
- `permission-allow`, `permission-deny`, `permission-always` — a Bash call answered through
  `can_use_tool` with allow, deny, and allow + `updatedPermissions` (T0, SPEC §8.1).
- `ask-user-question` — `AskUserQuestion` answered through `updatedInput.answers` (T0).
- `mcp-timeout` — a stdio MCP tool cut off by `MCP_TOOL_TIMEOUT=3000` (T0, SPEC §7.1).

Claude Code 2.1.308, permission policy (T12, SPEC §8.1, §14; findings in
`docs/notes/claude-control-protocol.md`, "Permission policy, 2.1.308"):

- `auto-mode`, `auto-classifier` — `--permission-mode auto` deciding without `can_use_tool`;
- `always-git-push` — `always` writing rules to `settings.local.json`;
- `settings-deny` — `--settings` allow/deny over those rules; deny wins, `permission_denied`;
- `auto-deny` — the worker's default deny list under `--permission-mode auto`;
- `restricted-settings` — `--settings` under `--restricted`.

Claude Code 2.1.308, two writers of one session (T15, SPEC §9; `tools/spike/driver.py`
with `--arg-prompt` and `--resume`):

- `vscode-takeover` — `claude -p "<prompt>"` (prompt as argument, as a person runs it), then
  `--resume` with the adapter's flags and one more turn; the stream is the adapter's run;
- `adapter-then-outside` — an adapter session (stream), then `claude -p --resume <uuid>
  "<prompt>"` from outside; the transcript ends with the outside turn.

Claude Code 2.1.308, a fork in one transcript (T20, SPEC §9.3; `driver.py` in
`tmp/spike-t20-a`, no VS Code):

- `fork` — `claude -p "Reply with exactly: ONE"` (prompt as argument), then `--resume` with the
  adapter's flags and the turn `HQC-TURN` (the stream is this run), then `claude -p --resume
  <uuid> --resume-session-at <uuid of the ONE answer> "Reply with exactly: VSCODE-TURN"`: the
  last turn hangs off the `ONE` answer, which so has two children, the way VS Code continued
  from its own last record in the case of 2026-10-23. The stdin file is the adapter run's.

Claude Code 2.1.308, the first input after `--resume` and `/compact` (T25, SPEC §9.3;
`driver.py` in `tmp/spike-t25-a` with the adapter's flags):

- `resume-compact` — a new session (`Reply with exactly: ONE`), then `--resume` and `Reply
  with exactly: TWO` (stream and stdin: `stream-resume-prompt`, `stdin-resume-prompt`), then
  `--resume` and `/compact` (`stream-resume-compact`, `stdin-resume-compact`). The transcript
  is the file after all three runs. The command record `<command-name>/compact</command-name>`
  is in the transcript only.

Claude Code 2.1.308, cost and slash commands (T27, SPEC §4, §8.1; `driver.py --then` in
`tmp/spike-t27-cost`, the adapter's flags without `--permission-prompt-tool`):

- `cost-turns` — a new session with two turns in one process (`Reply with exactly: ONE`, then
  `TWO`): stream and stdin only. `result.total_cost_usd` grows from turn to turn (0.00216749,
  then 0.00398629): it is the session's total, not the turn's;
- `cost-resume` — `--resume` of that session and `Reply with exactly: THREE`. The transcript
  is the file after both runs: a `cost-state` record (`totalCostUSD` 0.00398629) is written when
  the first process ends, the second one after the resumed process; the resumed turn's
  `total_cost_usd` is 0.0043361 = the first `cost-state` plus that turn.

Their `system/init` keeps `slash_commands` and `terminal_slash_commands` (sanitize.py keeps
them since T27); `system/init` comes again before every turn of a process.

In the stream files of the T12 sets the `input_json_delta` fragments of a tool input whose
path had to be rewritten are joined into the first fragment (sanitizing split paths).

T0 runs used `claude -p --input-format stream-json --output-format stream-json
--permission-prompts host --permission-prompt-tool stdio --include-partial-messages
--replay-user-messages --verbose --model claude-haiku-5-5 --max-turns 2`; exact frames and
findings are in `docs/notes/claude-control-protocol.md`.

Message `uuid`s match between each transcript and its stream (SPEC §8.1, §9.3), including the
`--replay-user-messages` echo of the prompt; `src/lib.rs` tests this.

Before committing a new set, scrub it:

- drop `api-request*` records (`api-request-blob` holds the full system prompt);
- replace `attachment` and `rendered` of attachment records, and the file-history snapshot,
  with `{"stripped":true}` (they carry CLAUDE.md, skills and the user's email);
- trim `system/init` to `type, subtype, cwd, session_id, model, permissionMode,
  claude_code_version, mcp_servers, uuid`;
- rewrite paths under `/Users/<name>` and scratch dirs to `/tmp/hqc-fixture/...`, emails to
  `user@example.com`, and drop hook keys and hook events.

Add new sets per CLI version, with the version in the file name.
