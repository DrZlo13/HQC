# Inbox scenario (T16, SPEC §5.3, §12)

An input with four artifacts, as the hub sends it (`session.input.json`), and what the hub
serves for them on `GET /worker/artifacts/{id}` (`artifacts.json`: name, kind, `Content-Type`
and the file in `artifacts/`). Two artifacts share the name `notes.txt`; one name carries a
path and a reserved character.

- `expected-text.txt` — the text the runtime gets: the input text and the block
  "Вложения:" with paths relative to the working directory.
- `expected-stdin.json` — the stream-json user message of Claude Code: a text block and the
  image as a base64 image block (format of the Messages API; **[проверить]** on a real CLI
  run, SPEC §8.1).

Read by `tests/worker.rs` (`inbox_scenario`) and `src/adapter/claude_code/tests.rs`.
