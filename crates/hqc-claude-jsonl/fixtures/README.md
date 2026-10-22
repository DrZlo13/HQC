Fixtures captured from Claude Code 2.1.307 on macOS, one headless turn with a Bash tool call.

- `tool-call-2.1.307.jsonl` — the `~/.claude/projects/<cwd>/<uuid>.jsonl` transcript with
  `api-request*` records removed and the file-history snapshot replaced by `{"stripped":true}`.
- `stream-tool-call-2.1.307.jsonl` — stdout of the same run with
  `--output-format stream-json --verbose`; the `system/init` record is trimmed to a few fields.

Message `uuid`s in both files match (SPEC §8.1, §9.3). Add new fixtures per CLI version;
never commit `api-request-blob` records (they contain the full system prompt).
