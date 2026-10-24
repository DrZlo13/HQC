# T6 — end-to-end run, runbook: decisions and open questions

Task T6 was done without a human and without a phone: the end-to-end run is local, on one
machine (`tools/e2e-local.sh`); the runbook is `docs/RUNBOOK.md`. `docs/SPEC.md` was not
changed; items that might be worth entering into the spec are marked **→ spec**.

## End-to-end run (`tools/e2e-local.sh`)

1. The hub in dev mode on `127.0.0.1:<a free port ≥ 18000>`, a separate database `hqc_e2e`
   (`dropdb --if-exists --force` + `createdb` at the start, `dropdb --force` at the end), push
   on: VAPID keys are generated with `openssl` for each run, and the script checks that
   `GET /api/push/vapid` returns the public key (i.e. the hub accepted the private one). Push
   delivery itself is not checked: there is no browser subscription.
2. Worker: `HQC_HOME=tmp/e2e-<date>/home`, `HQC_TOKEN_STORE=file`, workspace
   `tmp/e2e-<date>/workspace`; the directory is recreated on each run, `~/.hqc` is not
   touched, `HQC_WORKER_CONFIG` from the environment is cleared.
3. The session is created with `model: claude-haiku-5-5`. The body of
   `POST /api/projects/{id}/sessions` is not described in §6; the hub accepts
   `{title, worker_id, model, cwd, auto_approve}` and ignores the `runtime` field (iteration 1
   has one runtime). **→ spec §6: the session creation body.**
4. `--max-turns` cannot be set: neither the hub nor the adapter passes it (T4, item 1). The
   run's turn is two model calls (a `Bash` call, the answer `DONE`), but there is no limit. If
   the CLAUDE.md rule ("`--max-turns` no more than 2") should apply to e2e, a way to set it is
   needed (a session field or a worker setting). Open question.
5. If `client/dist` is built, the hub starts with `HQC_CLIENT_DIR` and the script checks that
   `/` is the client's `index.html` and that `/sw.js` is served.
6. The runs' transcripts stay in
   `~/.claude/projects/-Users-home-mac-work-hqc-tmp-e2e-<date>-workspace/`: the script checks
   them but does not delete them (this is Claude Code's data, not hqc's).
7. `E2E_CLAUDE_BIN=/usr/bin/false` runs everything except the live turn without spending the
   subscription: that is how the bug in item 8 was found.

## Found and fixed

8. **Hub: a failed start was retried without a pause.** On a failed start the worker sends
   `session.state` `starting` → `stopped` and `error {agent_start_failed}`. The
   `session.state stopped` handler reset the wait for the answer to `session.start` and sent a
   new `session.start` at once, bypassing `start_retry_delay` (§5.2: "retried after a
   pause"). The result on a live worker: three starts in ~10 ms, the input `failed`, and a
   fourth start produced a second `system agent_start_failed` event. Fixed (`worker_ws.rs`,
   `on_session_state`): `stopped` does not reset a `session.start` that awaits an answer and
   does not bring up a session whose failed starts are being counted; `on_start_failed`
   drives the retry. Test `failed_start_with_state_frames_waits_between_retries` (both
   `error`/`stopped` orders, a pause between starts, a single `agent_start_failed`); it fails
   without the fix. The spec did not change: the code now does what it says.

## Runbook changes

9. **The hub serves the client**: `HQC_CLIENT_DIR` is the `client/dist` directory, served at
   `/` (no SPA fallback: the client's routes are in `#`), `/api/*` and `/worker/*` are never
   taken from disk, escaping the directory gives 404, `index.html`/`sw.js`/the manifest are
   `no-cache`, `assets/*` are immutable. Without the variable the behaviour is as before.
   Before this there was nothing to serve the client from the same origin, while it calls
   `/api` by a relative path (the client's README already said "the hub serves the client").
   Test `client_files::tests`. **→ spec §6/§20: "the client is served by the hub from the
   same domain, behind the same Access"?**
10. **`hqc-hub --version`** (and an error on any other argument instead of silently ignoring
    it). Test `tests/cli.rs`.
11. **Worker revocation in the client**: a "Revoke" button in "Settings" (`DELETE
    /api/workers/{id}`; the hub already had the route). §5.1 says "the token is revoked from
    the UI", but T5 had no button. The mock supports `DELETE`; the test is in
    `client/tests/e2e.test.tsx`.
12. **The manifest behind Access**: `<link rel="manifest" crossorigin="use-credentials">`.
    Without the attribute the browser requests the manifest without cookies, and behind Access
    it would get the login page instead, breaking PWA installation. Not verified on a real
    Access **[verify]**.

## Open questions

13. **The run from a phone** (T6, §19.1) was not done: no device and no real Cloudflare Access
    in an autonomous run. Not verified: login through Access, the `/api/ws` WebSocket through
    the tunnel, PWA installation, real push on iOS and Android, the layout by eye (T5 "Not
    verified"). The result goes into §23 after a manual run per `docs/RUNBOOK.md` §4.
14. **`HQC_VAPID_SUBJECT` defaults to `mailto:hqc@localhost`.** Apple's push service
    reportedly rejects signatures with an invalid `sub`; the runbook says to set a real
    address. It may be worth making the variable mandatory when the keys are set.
15. **Keychain and binary updates.** Resolved 2026-10-23: see deploy-findings #8, RUNBOOK §2.4. The Keychain item is created by `hqc-worker enroll`;
    after an unsigned binary is replaced, macOS may ask for permission to access it, and under
    launchd there is no one to ask. Not verified **[verify]**; the runbook has a workaround
    (one run in Terminal or a repeated `enroll`).
16. **There is no `status running` event.** The adapter sends only `status idle` at the end of
    a turn; the client takes "running" from the session's computed `state` (§17.1). There is
    no discrepancy with the spec (the source of `status` is "the worker or the hub", nothing
    is mandatory); noted for reference.
