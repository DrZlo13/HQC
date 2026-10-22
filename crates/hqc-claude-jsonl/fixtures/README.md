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
