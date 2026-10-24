# Iteration 1: tasks

Goal (SPEC §19.1): start a Claude Code session from the web, drive it from a phone, approve
a permission request. Simplifications for this iteration: one project (personal), one
workspace from the worker's config, no hqc MCP, no mirror.

Each task is self-contained: it can be handed to a separate agent together with `CLAUDE.md`
and the listed sections of `docs/SPEC.md`. Dependencies are stated; tasks with no shared
dependencies can run in parallel.

## T0. Spikes for the [verify] marks (blocks T4)

Sections: §8.1, §9.3. Result: fixtures in `crates/hqc-claude-jsonl/fixtures/` and a note
`docs/notes/claude-control-protocol.md` with the exact JSON shapes. `claude -p` runs only
with `--model claude-haiku-5-5` and `--max-turns` no more than 2 (runs with MCP need two
turns: the first goes to ToolSearch).

1. `control_request` / `control_response` for `can_use_tool` with `--permission-prompts host`
   and `--input-format stream-json`: the request fields (`tool_name`, `input`,
   `permission_suggestions`), the shape of an `allow`/`deny` response, `updatedPermissions`.
   Check that `always` via `updatedPermissions` writes a rule
   to `.claude/settings.local.json`.
2. The `--replay-user-messages` echo: does it carry the `uuid` of the prompt's jsonl record.
3. `AskUserQuestion` with `--permission-prompts host`: does it arrive as `can_use_tool`,
   does it accept answers via `updatedInput`.
4. `MCP_TOOL_TIMEOUT`: the default value and units.
Acceptance: the fixtures are in the repo, the marks in §8.1/§9.3 are replaced with the
findings, the CLI version is recorded.

## T1. hqc-proto (blocks T2, T3, T4)

Sections: §3, §4, §5.3, §5.4. The skeleton exists. Complete: all iteration 1 frames of
§5.3/§5.4 (`session.start`, `session.input`, `session.stop`, `session.kill`, `approval.response`,
`clarify.response`, `hello`/`hello.ack`, `event`/`ack`, `session.state`, `error`;
`approval.cancel` is a §4 event inside `event`, not a frame),
the types `Author`, `Delivery`, `SessionState`; a serialization test for every frame with
the JSON example from the spec. Acceptance: `cargo test -p hqc-proto`, JSON names match the spec.

## T2. Hub: core (depends on T1)

Sections: §2, §3, §5.1, §5.2, §6, §10.1, §16 (push), §17, §20.
- Postgres, `sqlx` migrations: users, workers, projects, sessions, events (unique
  index on `event_id`, `seq` per session), pending_requests, read_cursors,
  push_subscriptions, enroll_codes.
- Event storage: `payload` is written to `jsonb` exactly as it came from the worker or
  the client, and clients get the same raw JSON back. The `hqc-proto` types are only for
  the hub's logic; nothing is serialized from them back into storage, otherwise enum values
  from a newer worker would turn into `unknown` (§4, §5.2).
- Verification of the `Cf-Access-Jwt-Assertion` JWT against JWKS; dev mode
  `HQC_DEV_USER=<email>` without Access for local development, explicitly only with `HQC_DEV=1`.
- Route `/worker/ws`: worker token, `hello`/`hello.ack` with the version, `event` → `ack`
  with `seq`, dedup by `event_id`, resending `pending` inputs after `hello`, the
  `failed` policy after 3 failed starts (§5.2).
- Route `/api/ws`: `subscribe {session_id, after_seq}`, `subscribe_summary`,
  `agent.delta` without `seq`, `client_op_id`.
- REST from §6, the iteration 1 subset: me, projects (personal), sessions, events with
  pagination, input, stop, retry, requests, read_cursor, workers enroll/confirm/delete,
  push/subscribe.
- Computing `state` and `queue_len` (§17.1), `approval.cancel` on stop/restart.
- Web Push (VAPID) on `approval.request` and on the final `agent.message` when there is no
  active subscription.
Acceptance: an integration test with a fake worker over WebSocket: hello → session.start →
event → ack → the client sees the event; resending the same `event_id` does not create a
duplicate; after a disconnect the `pending` input is resent.

## T3. Worker: core (depends on T1)

Sections: §2, §5.1, §5.2, §5.3, §5.4, §14 (config reading only), §17.3.
- `hqc-worker enroll <hub>`: a code, confirmation on the web, the token in a secure store
  (Keychain on macOS; on other OSes a 0600 file for now, with a note in the spec).
- Local config `~/.hqc/worker.toml`: `allowed_roots`, one workspace for iteration 1,
  `idle_timeout`.
- WebSocket client with a 20 s heartbeat, reconnect with exponential backoff, `hello`.
- On-disk outbox (SQLite or an append log with an index), `ack` by `event_id`, retry after
  reconnect.
- Agent process supervisor: `session.start` → adapter (the interface from §8), `session.input`
  with `ack` only after the adapter confirms it, `session.stop`, `session.kill`,
  `session.state`.
- `error {retryable}` when a process fails to start.
Acceptance: a test with a fake hub: the worker enrolls, survives a disconnect, resends the
outbox, does not lose input across its own restart (the input arrives again from the hub).

## T4. Claude Code adapter (depends on T0, T1; integrates into T3)

Sections: §8.1, §4, §10.1.
- Launching `claude -p` with the flags from §8.1 (including `--permission-prompt-tool stdio`),
  `--session-id` from the hub, `--resume` to continue.
- `--allowedTools mcp__hqc` at launch, so the hqc MCP server's tools do not go through
  `can_use_tool` (§7, §8.1); iteration 1 has no hqc server yet, the flag is put in from the start.
- Mapping stream-json → §4 events, the `final` rule based on the next item in the stream,
  `event_id` = uuid v5 of the message `uuid`, `in_reply_to`.
- `can_use_tool` → `approval.request` with `options` (once/session/always/deny), the response
  is a `control_response`; `session` is a rule in the adapter; `always` is `updatedPermissions`
  only from `addRules` with `destination: localSettings`, other suggestions are dropped.
- The echo of its own `control_response` on stdout is skipped; `ack` for `session.input` is
  sent on a `user` with `isReplay: true`.
- `AskUserQuestion` → `clarify.request`, the answer is `updatedInput.answers` (confirmed by T0).
  A `clarify.response` frame with `answers: {}` is a cancellation by the hub: the adapter
  answers the `can_use_tool` for `AskUserQuestion` with `deny` behaviour, the model continues
  the turn without answers (§5.3, §10.1).
- `usage` from `result`, `status idle`.
- Input during a turn is not sent to the process: the supervisor keeps the queue (§17.2).
Acceptance: tests on stream-json fixtures without running `claude`; one live test behind
`HQC_LIVE_CLAUDE=1`: a turn with a `Bash` call, permission approved through the adapter,
events in the right order, `event_id`s match the uuids from the jsonl.

## T5. Web client (depends on T2 for integration; the UI can start from §4/§6 on mocks)

Sections: §6, §4, §10.1, §16, §19.1.
- TypeScript, PWA, responsive layout for a phone. Framework is the implementer's choice,
  justified in `client/README.md` in one paragraph.
- Screen: the list of the personal project's sessions with `state` and unread; the session
  chat with the `agent.delta` stream, `tool.call`/`tool.result` cards, `approval.request`
  buttons from `options[]`, status; text input; stop and retry buttons.
- `/api/ws` subscription with `after_seq`, reconnect, `client_op_id` for optimistic display.
- Web Push: subscription, showing the notification, opening the session.
- Worker enrollment: the code confirmation screen.
Acceptance: a manual scenario from a phone through Cloudflare Access: open a session, write,
get a push for an approval, approve, see the reply.

## T6. Build and iteration acceptance (depends on all)

- `docs/RUNBOOK.md`: how to bring up the hub in the DMZ (systemd/launchd, Postgres, variables,
  Cloudflare Tunnel and Access, VAPID keys), how to install the worker on home-mac.
- An end-to-end run of the §19.1 criterion from a phone, recording the result and any
  discrepancies with the spec found in §23.

## Order

T0 and T1 first, in parallel. Then T2, T3, T5 in parallel; T4 after T0 and T1,
built into T3. T6 last.
