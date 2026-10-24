# T3 — open questions and decisions taken

Task T3 (the worker core) was done without a human. Where the spec is silent or ambiguous, a
conservative decision was taken; it is recorded here and in the code. `hqc-proto` and
`docs/SPEC.md` were not changed; items that need a spec edit are marked **→ spec**.

## Enrollment (`hqc-worker enroll`)

1. **The worker side of enroll is not described in the spec** (§5.1, §6 only know
   `POST /api/workers/enroll/confirm {code}`). Adopted (`crates/hqc-worker/src/enroll.rs`):
   - `POST {hub}/worker/enroll` `{name, os, protocol_version}` →
     `200 {enroll_id, code, poll_interval_s?, expires_at?}`;
   - `POST {hub}/worker/enroll/poll` `{enroll_id}` →
     `200 {status: "pending"}` | `200 {status: "confirmed", token, worker_id}` |
     any other `status` (`expired`, `rejected`) or HTTP 404/410 ends with an error.
   - `enroll_id` is a secret between the worker and the hub, sent in the body, not in the URL
     (so it does not end up in access logs). By default polling every 2 s, waiting until
     `expires_at` or 15 min.
   **→ spec §5.1/§6, reconcile with T2.**
   Resolved in 0.11: the hub's shape is `{code, poll_token, expires_at, poll_interval_s}` with polling by `poll_token`; the worker also prints a `<hub>/#/enroll?code=` link (§5.1).
2. **Secure store.** macOS: Keychain (generic password, service `hqc-worker`,
   account = `hub_url`). Other OSes: the file `$HQC_HOME/token` with mode 0600 (directory 0700,
   written via a temporary file and rename). libsecret/DPAPI from §5.1 are not done.
   `HQC_TOKEN_STORE=file` turns on the file backend on any OS (the tests use it).
   **→ spec §5.1: a note "outside macOS, a 0600 file for now".**
   Resolved in 0.11: recorded in §5.1 and §18 (a 0600 file in the first version, libsecret and DPAPI later).
3. `enroll` does not write `worker.toml`: `hub_url` is set in the config by hand; if there is
   no config, enroll points to its path.

## Local config (§14, the iteration 1 subset)

4. The `worker.toml` format: see the `config.rs` module and the report below. `idle_timeout` is
   an integer number of seconds, `0` = leave alone (§17.3). Unknown fields are an error
   (`deny_unknown_fields`): a typo in the config must not silently weaken the restrictions.
5. There are no environments or bindings in iteration 1: the only environment is `default`.
   `session.start.environment != "default"` → `error invalid_frame`. `hello` carries
   `environments: ["default"]`, `env_bindings: []`.
6. `session.start` checks (all → `error {code: invalid_frame, retryable: false}`, nothing is
   created or started):
   - `workspace_path` after normalisation (canonicalize) is not equal to `[workspace] path`
     or is outside `allowed_roots`;
   - `workspace_path: null` (a session without a workspace, §8.4) is not supported in
     iteration 1;
   - `cwd` is absolute, contains `..`, or escapes the workspace through a symlink;
   - `acp_agent` is set (there are no ACP agents in the config);
   - `owner_is_worker_owner: false` with `allow_foreign_sessions = false`;
   - a `runtime` that is not in the build. Debatable: it could be `agent_start_failed`, but
     this is not a transient error, and a retry is pointless.

## Protocol and delivery

7. **start or resume.** The hub always puts `runtime_session_id` into `session.start`, and the
   frame does not show whether this is a new session or a continuation. The supervisor calls
   `Adapter::resume` if this worker process has already started the session (after `kill`,
   `idle_timeout`, a crash), otherwise `Adapter::start`. After a worker restart the first
   launch is `start`; the Claude Code adapter (T4) must switch to `--resume` itself if a
   jsonl with this uuid already exists. **→ spec §5.3: a `resume` flag in `session.start`?**
   Resolved in 0.11: `session.start.mode` (`new` → `Adapter::start`, `resume` → `Adapter::resume`); internal process restarts are still `resume` (§5.3).
8. **Input for a session without a `session.start`** in the current process (for example,
   after `hello` the hub re-sends `pending` without sending `session.start`) →
   `error {code: unknown_session, retryable: true}`. We expect the hub to send `session.start`
   before re-sending inputs. **→ reconcile with T2.**
   Resolved in 0.11: after `hello` the hub sends `session.start` (`mode: resume`) and re-sends answers and inputs only after the `ack`; the `unknown_session` retryable reply stays (§5.2).
9. The `ack` to `session.start` is an empty `{}` after a successful `start`/`resume`; a
   repeated `session.start` for a live process also gets `ack {}` (the process is not
   recreated). `ack {}` to `session.stop`, `session.kill`, `approval.response`,
   `clarify.response`; an adapter error on these is `error {internal}`. `session.kill` for an
   unknown session gets `ack {}` (nothing to kill).
   Resolved in 0.11: the `ack` to `session.start` for a live process is recorded in §5.2 (the hub relies on it when reconnecting).
10. **`session.kill` clears the worker's queue.** The inputs stay `pending` in the hub, but the
    hub re-sends them only after the next `hello`. **→ a question for T2/the spec: what the hub
    does with the queue on kill.**
11. **Input without confirmation.** If the turn ended (`TurnEnded`) or the process died and the
    adapter never confirmed the input (`InputAccepted`), the input is considered undelivered
    and goes back to the head of the queue (for Claude the echo arrives as soon as stdin is
    read).
12. A repeated `session.input` with an already known `event_id`: if the input is already
    confirmed, an immediate `ack {event_id}` with the new frame `id`, and it does not go to the
    runtime again; if it is queued or in flight, the ack will go to the new `id`. The memory of
    confirmed inputs is the last 256 per session.
13. On a failed start, `error {agent_start_failed, retryable: true}` goes to the frame that
    triggered the start (`session.start` or `session.input`) and to all queued inputs (the hub
    counts failures per input, §5.2).
14. An unexpected exit of the agent process (`Exited` with an error or during a turn) is a
    `system {code: "agent_exited"}` event via the outbox; if there are queued inputs, the
    process is brought back up via `resume`. The `agent_exited` code is new. **→ spec §4: the
    list of `system` codes.**
    Resolved in 0.11: the list of `system` codes with `agent_exited` is in §4.
15. `session.state`: `locked_by: hqc` while the process is not `stopped`, otherwise `none`.
    All states are re-sent after each `hello.ack`.
16. An `error` from the hub to an `event` frame: `retryable: false` removes the event from the
    outbox (otherwise it would be sent forever), `retryable: true` keeps it until reconnect.
    An `ack` with `seq: null` (the session is unknown to the hub) removes the event (§5.2).
17. Heartbeat: a WebSocket ping every 20 s; if no frame has arrived for 60 s, the connection
    is considered dead. The spec defines no separate application-level heartbeat frame.
18. Incompatible version: an `error` log "worker update needed", a retry after an hour
    (§5.1), nothing else. There is no tray. A `hello.ack` with a different `protocol_version`:
    a warn and carry on.
19. 401/403 on the upgrade or `error {unauthorized}` on `hello`: a log "run enroll again"
    and a reconnect with the usual delay (up to 60 s); the process does not exit: the token
    may have been reissued, and the supervisor (launchd) would restart it anyway.
20. Graceful shutdown (SIGTERM/SIGINT): `kill` to all agents, `session.state stopped`, an
    outbox flush waiting up to 5 s for `ack`; whatever is unconfirmed stays on disk.

## Other

21. `capabilities.claude_code.version` is the first digits-and-dots token from
    `claude --version` (expected `2.1.307 (Claude Code)`). The output format was not checked
    on a live build. **[verify]**. The path is `claude_bin` from the config or `claude` from
    PATH.
22. Token in logs: `tungstenite` at `trace` level logs the upgrade request together with
    `Authorization`. `main` forcibly caps `tungstenite`, `hyper`, `reqwest`, `h2`, `rustls` at
    `debug`, even if `RUST_LOG` asks for more; a test checks the output with
    `RUST_LOG=trace`.
23. Until T4 is merged, `hqc-worker run` uses a stub adapter: every start is
    `agent_start_failed` (retryable).
24. There is no `steer?` in the `Adapter` trait: iteration 1 has no `session.steer` frame.
