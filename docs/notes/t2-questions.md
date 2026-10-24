# T2: questions and decisions taken

Task T2 (the hub core) was done without a human. Where the spec was silent or allowed two
readings, the conservative option was chosen; it is described here and, if it is on-the-wire
behaviour, entered into `SPEC.md` 0.10 (§23). `hqc-proto` was not changed.

## Open questions (a decision is needed)

1. **`hello` = worker restart.** The hub cannot tell a dropped connection from a worker
   restart. Chosen: after `hello` the open `approval`/`clarify` requests of the worker's
   sessions are cancelled with `worker_restart` (an event plus an `approval.response` frame
   with `deny` / `clarify.response` with `{}`), an unclosed turn gets `system turn_lost`. The
   cost: on a brief disconnect, a request the adapter still holds is closed with a denial.
   Proposal: a field in `hello` (for example `boot_id` or `running_sessions[]`) so the hub
   cancels only after a real restart. That is a `hqc-proto` change, so it was not made.
   Resolved in 0.11: `hello.boot_id`; `worker_restart` cancellation and `turn_lost` only when `boot_id` changes, without a `deny` frame (§5.2, §17.3).
2. **`answered_elsewhere`.** §10.1 lists "an answer from another device" among the hub's
   cancellations with a `deny` frame. Taken literally, this contradicts "the first answer
   wins": after `allow` the hub would send `deny`. Done: the first answer closes the request
   (all devices see the `approval.response` event, the buttons go away), a second answer
   gets `409 request_closed`, and the hub does not write an `approval.cancel {answered_elsewhere}`
   event. The wording of §10.1 needs clarifying.
   Resolved in 0.11: written that way in §10.1 and §4; the `answered_elsewhere` reason was removed from `hqc-proto`.
3. **Cancellation on `POST /stop`.** §10.1 does not count `stop` among the hub's
   cancellations, but T2 requires `approval.cancel` on stop. Done: the hub itself cancels open
   requests with `stopped` (an event plus a frame with `deny`), then sends `session.stop`. If
   the adapter also sends `approval.cancel`, the history will have two cancel events; the hub
   keeps both.
   Resolved in 0.11: only the hub writes the cancellation on `stop`; the T4 adapter and the supervisor emit no cancellation on `session.stop` (verified, §10.1).
4. **Worker enrollment over HTTP** was not described in the spec (only "code, confirmation
   on the web, token"). An `enroll` / `poll` / `confirm` scheme was added (§5.1). T3 is being
   done in parallel: if the worker expects a different shape, the two need reconciling.
   Resolved in 0.11: the worker moved to the hub's shape (`poll_token`), the bodies are shared in `hqc_proto::api`, the hub accepts `protocol_version` (§5.1).
5. **The client's `/api/ws` frames** are not described in `hqc-proto`; the types live in
   `crates/hqc-hub/src/client_ws.rs`, the shape in §6. For TS generation they should move to
   `hqc-proto` (a proto change, not done).
   Resolved in 0.11: `hqc_proto::client_frame` (and the REST bodies in `hqc_proto::api`), examples `client.*.json`/`api.*.json`, the hub and the client use them (§6).
6. **`workspace_path` in `hello.capabilities`** is a temporary iteration 1 field (TODO §19 in
   `worker_ws.rs`). From it the hub creates a workspace in the worker owner's personal
   project; a session takes the worker's most recently announced workspace. Without this field
   a session cannot be created (`409 no_workspace`).
   Resolved in 0.11: the worker sends `capabilities.workspace_path` from its config; TODO §19 stays on both sides (§5.4).
7. **`runtime_session_id`.** The hub assigns a uuid v4 when the session is created and sends
   it in every `session.start`. How the worker chooses `--session-id` or `--resume` (for
   example, by whether the jsonl exists) the spec does not say; a question for T3/T4.
   Resolved in 0.11: `session.start.mode` (`new` on the first `session.start`, otherwise `resume`); the Claude Code adapter picks the flag by whether the jsonl exists (§5.3, §8.1).
8. **Protocol version.** Only `protocol_version == 1` is accepted; both an older and a newer
   one get `protocol_version_unsupported {min_version: 1}`. There is no version downgrade
   negotiation while there is only one version.
9. **Dev mode "not available in production".** Done: only with `HQC_DEV=1` together with
   `HQC_DEV_USER`; if `HQC_ACCESS_*` is also set, the hub refuses to start; a loud warning at
   startup. There is no binding to an address (loopback only); if stricter is needed, dev mode
   could require `127.0.0.1` in `HQC_HUB_LISTEN`.
   Resolved in 0.11: `HQC_DEV=1` requires loopback in `HQC_HUB_LISTEN` (127.0.0.0/8, [::1], localhost), otherwise the hub does not start (§6, §20).

## Decisions without questions (for reference)

- `state`/`queue_len` are computed on every read (§17.1). For "a turn is running" the hub
  keeps the internal fields `turn_open` and `last_turn_error` in `sessions` (this is not
  `state`): opened by the `ack` to `session.input`, closed by `agent.message final`,
  `status idle|error` or `session.state stopped`.
- The worker's `status` (online/offline) is not stored: it comes from the in-memory connection
  registry.
- Failed start: a `start_failures` counter in `sessions`; a retry after
  `start_retry_delay` (2 s), only if there are `pending` inputs; after 3, all of the session's
  `pending` inputs become `failed`, `system agent_start_failed`. A non-retryable start error is
  `failed` at once. A new input after that makes one start attempt; `retry` resets the
  counter and `last_turn_error`.
- An answer to a request given while the worker is offline is kept in
  `pending_requests.result` and re-sent after `hello` until the worker replies `ack` (or
  `error`, after which it is no longer sent). The hub's cancellations are delivered the same way.
- If an `approval.request` has no `canonical: deny` option, `deny_always` is used; if that is
  missing too, the hub writes `approval.cancel` without a frame (with a warning in the log).
- A `clarify.response` with empty `answers` from the client is rejected (`400`): an empty
  object on the wire means a cancellation by the hub.
- `auto_approve`: the hub answers with the first `canonical: once` option, the event has
  `author: system` and `by: "auto"`. Only the worker owner can turn it on.
- `session_token` in `session.start` is new on every start; the database stores only its hash.
- Push: on `approval.request`, `clarify.request`, `agent.message final`, `system` with
  codes `agent_start_failed` / `input_rejected`; recipients are the session owner and (for
  requests) the worker owner, except those who have an open WS subscription to the session. A
  subscription that answers 404/410 is deleted. Without VAPID keys push is off, and
  `GET /api/push/vapid` returns `public_key: null`.
- `POST /api/projects` and `PATCH/DELETE /api/projects/{id}` are not implemented in
  iteration 1 (one personal project); `archive`/`unarchive` are implemented because they are
  needed to cancel requests per §17.4.
- Permissions (§11): the `can` function in `api.rs`; in iteration 1 only the session owner has
  access; for approvals the worker owner is checked in addition. Someone else's session is
  indistinguishable from a non-existent one (`404`).
- Rate limit on `/worker/enroll`: 10 per minute per hub process and no more than 100 active
  codes.
- Postgres without TLS in sqlx (the hub and the database are on one machine in the DMZ). If the
  database moves elsewhere, enable `tls-rustls`.
- Tests: each test instance of the hub creates its own schema in `HQC_TEST_DATABASE_URL`
  (default `postgresql://localhost/hqc_test`) and drops it at the end; a failed test
  leaves a `t_<uuid>` schema behind. A Postgres 17 service was added to CI.
- The test RSA key for signing Access JWTs is in
  `crates/hqc-hub/tests/fixtures/test-access-key.pem`; it is not a credential.
