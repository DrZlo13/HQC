# T4 — open questions and decisions taken

Task T4 (the Claude Code adapter, `crates/hqc-worker/src/adapter/claude_code.rs` and the
submodules `engine`, `rules`, `paths`, `process`, `scripted`) was done without a human. Where
the spec, the T0 note or the task statement are silent, a conservative decision was taken; it
is recorded here and in the code. `hqc-proto` and `docs/SPEC.md` were not changed; items that
need a spec edit are marked **→ spec**.

CLI versions: the T0 fixtures were recorded on **2.1.307**; by the time of T4 the CLI on
home-mac had updated to **2.1.308**, which the live test and the one-off `interrupt` check
(item 5) ran on.

## Launch

1. **Command line** (the order is pinned by the `command_line_new_then_resume` test):
   `claude -p --output-format stream-json --input-format stream-json
   --include-partial-messages --replay-user-messages --permission-prompts host
   --permission-prompt-tool stdio --strict-mcp-config [--mcp-config <json>]
   --allowedTools mcp__hqc (--session-id|--resume) <uuid> --verbose [--model <m>]`.
   The binary is `claude_bin` from the config, otherwise `claude` from `PATH`; the working
   directory is `<workspace>/<cwd>` (`StartSpec::workdir`). `--mcp-config` is not passed: the
   extension point is `ClaudeOptions::mcp_config` (iteration 3). Not passed and not yet
   verified: `system_prompt` (`--append-system-prompt`?), `disallowed_tools`
   (`--disallowedTools`?), `MCP_TOOL_TIMEOUT` from `ask_timeout_s` (§7.1, needed together
   with the hqc MCP). The process environment is the worker's environment (iteration 1 has no
   §14 environments).
2. **`--session-id` or `--resume` is decided by whether the transcript exists, not by the
   trait method.** If `<config>/projects/<encoded cwd>/<uuid>.jsonl` exists: `--resume`,
   otherwise `--session-id`, for both `start` and `resume`. This way `start` after a worker
   restart continues the session (T3 decision, item 7), and `resume` of a process killed by
   `idle_timeout` before the first prompt (the CLI has not created the file yet) does not fail
   on a non-existent id. `<config>` is `$CLAUDE_CONFIG_DIR` or `~/.claude`. Live test: the
   first launch used `--session-id`, the one after `kill` used `--resume`, and the process
   came up.
   Resolved in 0.11: recorded in §8.1; `mode` from `session.start` picks the trait method, the transcript still decides the CLI flag.
3. **Encoding the project folder.** As the `J0` function in the CLI 2.1.308 code: each UTF-16
   unit of the path that is not an ASCII letter or digit → `-`; if the result is longer than
   200 characters, the first 200 + `-` + base36 of the `abs` of the string's 32-bit hash
   (`h = (h << 5) - h + unit | 0`). What gets encoded is `process.cwd()`, i.e. the real path,
   so the adapter does `canonicalize` (`/tmp` → `/private/tmp`; `~/.claude/projects` has
   `-private-tmp`). Cross-checked: the folder names of the T0 runs
   (`-Users-home-mac-work-hqc-tmp-spike-once3` etc.; the fixtures have paths rewritten to
   `/tmp/hqc-fixture/...`, so the check used `~/.claude/projects`), the long-path hash against
   a reference computed by the CLI's own function in node; the live test found the transcript
   at the computed path `-Users-home-mac-work-hqc-t4-tmp-live-<v7>`. **→ spec §9 (the mirror
   will look for the same files).**
   Resolved in 0.11: the encoding rule (CLI 2.1.308) is recorded in §9 as a refinement of "cannot be decoded back".
4. **`runtime_session_id` missing** (per T3 the hub always sends it): the Claude session uuid =
   the hqc `session_id`, and the adapter reports it with `AdapterEvent::RuntimeSessionId`. Not
   a uuid: a start error.
5. **Early exit.** If the process exits within the first 500 ms (`ClaudeOptions::early_exit`),
   `start`/`resume` returns an error with the tail of stderr (→ `agent_start_failed`,
   retryable), not `Exited`: otherwise the supervisor with a non-empty queue would keep
   bringing it up in a loop. The cost is +0.5 s on every start. No binary: a `spawn` error at
   once.
   Resolved in 0.11: recorded in §17.3.

## Stop and exit

6. **`stop` → `control_request {subtype: "interrupt"}`, not SIGINT.** The T0 note has no
   interrupt, but the CLI 2.1.308 code (print.ts) handles a `control_request` with
   `subtype: "interrupt"`. Verified with a one-off live run (haiku, one turn, not
   committed): ~20 ms after the request a `result` with `subtype:
   "error_during_execution"` arrived, `usage` zero, the partial answer text came as a separate
   `assistant` record (the adapter emitted it as `agent.message final: true`), and the process
   stayed alive. Fallback: if there is no `result` within 10 s (`ClaudeOptions::stop_grace`),
   SIGINT goes to the process group (the CLI exits on SIGINT, giving `Exited`, and the
   supervisor brings the process back up via `resume`). **→ spec §8.1: interrupting a turn.**
   Resolved in 0.11: recorded in §8.1 (`control_request interrupt`, SIGINT fallback after 10 s).
7. **`kill`**: SIGTERM to the process group, SIGKILL after 5 s; no `Exited` is sent after
   `kill`. The process runs in its own group (`process_group(0)`): signals reach the `Bash`
   shells it started, and Ctrl-C in the worker's terminal does not reach the agents (the
   supervisor shuts them down). Plus `kill_on_drop`.
8. **Exit on its own** → `Exited {error}`: `None` on code 0, otherwise "exit code N" /
   "killed by signal N" and the last line of stderr. stderr goes to the log at `debug`.
   Held-back text is emitted as `agent.message final: false` on any exit.

## Event mapping

9. **`event_id`.** `Event::id_from_claude_record(record uuid)` for `tool.call`,
   `tool.result`, `agent.message`, `agent.thinking`. The CLI writes one content block per
   record (in all fixtures and in the live run); if a record has several blocks, block
   `i > 0` gets a v5 of the string `"<uuid>/<i>"` in the same namespace; the mirror needs the
   same rule. **→ spec §3 Event / §9.2.**
   Resolved in 0.11: the `"<uuid>/<i>"` rule is mandatory, recorded in §3 Event and §9.2. `usage`, `status`, `agent.delta`,
   `approval.request`, `clarify.request` get a fresh v7 (`result` has a `uuid`, but it is not
   in the jsonl).
10. **`final`.** Text is held back; any following assistant block (text, `tool_use`,
    non-empty `thinking`) or a `tool_result` releases it with `final: false`, `result` with
    `final: true`. A turn without text (for example `error_max_turns` after a tool call)
    produces no `agent.message` at all. `in_reply_to` is the `event_id` of the last input
    sent.
11. **`agent.thinking`** only for non-empty `thinking`: on haiku, blocks arrive with empty
    text and just a signature; they are skipped.
12. **`result`** → `usage` (`input_tokens`, `output_tokens` from `usage`;
    `cache_read` ← `cache_read_input_tokens`, `cache_write` ←
    `cache_creation_input_tokens`; `cost_usd` ← `total_cost_usd`; `model` from
    `system/init`, otherwise the first key of `modelUsage`), then `status {state: idle}`, then
    `TurnEnded`. If `subtype != success` or `is_error`, `status.detail` gets the `subtype`
    (`error_max_turns`, `error_during_execution`). **→ T2: whether this counts as "the turn
    ended with an error" for `state: error` (§17.1).**
13. **The `isReplay: true` echo** → `InputAccepted` for the oldest sent and not yet confirmed
    input (FIFO); the echo's uuid is unrelated to the hub's `event_id` and not needed.
14. **Subagents.** Lines with a non-empty `parent_tool_use_id` (`stream_event`, `assistant`,
    `user`) are not mapped: their records live in separate sidechain files, and the mirror
    will not reproduce an event with their uuid. A `can_use_tool` from a subagent is handled
    as usual.
15. **Previews** are cut to 4 KB at a character boundary; `args_preview` is compact JSON of
    `input`; `result_preview` is the string or the text blocks, other blocks as `[type]`;
    `error_text` is the first 300 bytes of the error text. `duration_ms` is not filled in.
16. `artifact_ids` and `peer_prefix` in `session.input` are not supported in iteration 1: only
    the text goes through, with a warn in the log.
17. stdout lines are read as UTF-8 with broken bytes replaced; an unparseable line: a warn and
    skip. Unknown `type`/`subtype` are skipped silently.

## Permissions

18. **`approval.request`**: `request_id` is the CLI's `request_id`; `tool` is `tool_name`;
    `description` is the request's `description` with the command / `file_path` / `url` /
    `pattern` / `blocked_path` below it (so the phone shows exactly what is being approved);
    `options`: `once` "Allow once", `session` "Allow for this session", `always`
    "Always allow" (only with `addRules` with `destination: localSettings` and not under
    `restricted`), `deny` "Deny"; `expires_at` is not set.
19. **`always`** → `allow` + `updatedPermissions` built only from the `addRules` items with
    `destination: localSettings` (cross-checked with the `permission-always` fixture, where
    the spike passed all suggestions). An `always` that arrives without such items or under
    `restricted` works as `session` (§8.1).
20. **`session`** (`rules.rs`): it remembers the exact `(tool_name, input)` pair and the rules
    from `permission_suggestions[].rules` of the same tool that the adapter can match safely:
    without `ruleContent`, the whole tool; Bash `"<prefix> *"` or `"<prefix>:*"`: the command
    equals the prefix or starts with `"<prefix> "` and **contains none of**
    `; & | \` $ ( ) < >` or a newline (otherwise `touch x && rm -rf ~` would pass the
    `touch x *` rule); Bash without `*`: the exact command. Other forms (path globs, WebFetch's
    `domain:`) are not interpreted, only an exact match. The rules live in `Core`, which is
    created per process, i.e. until the process ends. A matching request is answered `allow`
    at once, no `approval.request` is created.
21. **`deny`** is `{"behavior": "deny", "message": "The user denied this tool call."}`
    (the text differs from the spike; the test compares stdin without the `message` field).
    Without `interrupt: true`: the model continues the turn.
22. **An answer to an unknown or already closed `request_id`**, or an unknown `option_id`, is
    an adapter error (the supervisor replies to the hub with `error {internal}`); with an
    unknown option the request stays open.
23. Requests still open when `result` arrives (after `interrupt`) are silently forgotten:
    the hub writes `approval.cancel` on `stop` (§10.1, T2).
    Resolved in 0.11: only the hub writes cancellations on `stop`; the adapter does not emit them (§10.1).
24. A `control_request` of any other subtype (`hook_callback`, `mcp_message`, …) gets
    `control_response {subtype: "error"}` so the CLI does not wait forever.
25. `--allowedTools mcp__hqc` is always passed; the adapter does not let `mcp__hqc__*` tools
    through separately; the flag was verified in T0 (item 5 of the note).

## AskUserQuestion

26. Recognised by `tool_name == "AskUserQuestion"` with a non-empty `input.questions`
    (`requires_user_interaction` is not required). Questions get `id`s `"1"`, `"2"`, … in
    order; `options` are the options' `label`s, `multi` is `multiSelect`.
27. Answer: `allow` with the original `input` plus `answers {question text: label}` (matches
    the fixture byte for byte as JSON). An array answer for `multi` is joined with `", "`
    (**[verify]** the multi-select format on a live build). Answers to unknown `id`s are
    dropped; if none remain (including `answers: {}`, a cancellation by the hub), `deny` with
    the text "The user did not answer the questions.".
    An `approval.response` with `deny` to a clarify request also closes it with `deny`.

## Interface and tests

28. The `Adapter` trait did not change. `BuiltinAdapters` became a struct with
    `BuiltinAdapters::from_config(&Config)` (it was a stub unit struct, T3 item 23); its only
    user is `main.rs`. New public types for tests and future adapters:
    `ClaudeCodeAdapters::new(ClaudeOptions, Arc<dyn Spawner>)`, the `process::Spawner` trait
    (stdin/stdout as `AsyncWrite`/`AsyncRead`, signals, an exit future),
    `scripted::ScriptedClaude` (replays a fixture's stream, answers a `control_request` only
    after a line on stdin). `start_worker_factory` was added to `tests/common`.
29. **Live test: the prompt was changed.** With the given prompt "Run the shell command: echo
    hqc-live…" CLI 2.1.308 ran `echo` **without** `can_use_tool` (a read-only command is
    allowed automatically); the first run failed on the missing `approval.request`, while the
    order of the other events and the mapping were correct. In the test the command is
    `touch hqc-live && echo hqc-live`. **→ spec §8.1: not every Bash call asks for
    permission.**
    Resolved in 0.11: recorded in §8.1.
30. An observation for T2: the `ack` to `session.input` and `agent.delta` bypass the outbox,
    while the turn's events go through it, so the hub may receive the input's `ack` and the
    deltas after the `tool.call`/`approval.request` of the same turn. The integration test does
    not rely on the order between them.
    Resolved in 0.11: recorded in §5.2; the hub does not open a turn on an `ack` if the final answer to that input already exists (test `late_input_ack_does_not_reopen_the_turn`); a turn without final text remains an open question.
31. Logs: at `info`, process start/exit, input sent (`event_id`), permission request (tool
    name), answer (option), end of turn (`subtype`, `num_turns`, `duration_ms`); there is no
    message content, commands or stderr at `info` (stderr at `debug`, line types at `trace`).
