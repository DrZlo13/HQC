# Findings of the first deployment (2026-10-22, hub ai.ebal.uk in Docker, worker home-mac)

The end-to-end scenario of §19.1 through Cloudflare Tunnel + Access passed: worker online,
session, input, Bash, final answer. Push and the PWA from a phone are not checked yet.

Minor bugs, for iteration 2:

1. **enroll and Keychain.** Over ssh without an unlocked keychain, `enroll` gets the token from
   the hub and only then fails writing to Keychain ("User interaction is not allowed"). The
   token is burned, the worker stays on the hub without a token, and it has to be revoked and
   enrolled again. Needed: check that the store is available before requesting the token (a
   test write), and on failure suggest `security unlock-keychain` or
   `HQC_TOKEN_STORE=file`. Add a note about ssh to Runbook §2.3.

   Resolved: `eba4141` (T13) — `enroll` writes and reads a test entry before requesting the
   code and on failure exits with a hint; RUNBOOK §2.3.
2. **reqwest/hyper DEBUG logs in the enroll output** when RUST_LOG is unset: the default `info`
   filter is not applied in `enroll` (it is in `run`).

   Resolved: `eba4141` (T13) — the `trace` cap for the HTTP crates became a filter rather than
   a `=debug` directive, which raised them above `info` (in `run` too).
3. **A false "hub dropped the event: unknown session"** on every `agent.delta`: the hub
   acknowledges deltas with an ack without `seq` (they are not stored, §4), and the worker
   treats any ack without `seq` as dropped. Either the worker does not log this for deltas, or
   the hub answers deltas with a separate flag.

   Resolved: `eba4141` (T13) — the worker parses `ack` only for events from the outbox; it does
   not log delta `ack`s.
4. **The worker name** is taken from the hostname and comes out as
   `home-mac.tail201738.ts.net`; `enroll` needs a `--name` flag.

   Resolved: `eba4141` (T13) — a `--name` flag, defaulting to the host name without the domain.

5. **Keychain and headless runs.** After Claude Code is first run from a session with an
   unlocked keychain, it moves the credentials from `~/.claude/.credentials.json` into Keychain
   and deletes the file. In a regular ssh session the keychain is locked, and any headless
   `claude -p` (including a worker started over ssh without `security unlock-keychain`)
   answers "Not logged in". Solution: run the worker and other headless processes in the
   user's launchd gui domain (RUNBOOK §2.4) or after `security unlock-keychain` in the same ssh
   session. Write this down in RUNBOOK §2.3/§2.4.
6. **`tools/e2e-local.sh` with `E2E_CLAUDE_BIN=/usr/bin/false`** gets as far as the live turn
   and ends in FAIL on a timeout (three `agent_start_failed`, input `failed`). This is
   expected, but the script needs an explicit rehearsal mode that prints PASS at this point.

   Resolved: `eba4141` (T13) — if `E2E_CLAUDE_BIN --version` is not Claude Code, the scenario
   waits for `agent_start_failed`, input `failed` and `state: error` and prints
   `PASS (rehearsal)`.
7. **U+0000 in an event got stuck in the outbox forever** (fixed in T10, commit `1124bdc`).
   The output of a command with binary data produced a `tool.result` with U+0000 in
   `result_preview`; Postgres does not accept `\u0000` in `jsonb`/`text` ("unsupported Unicode
   escape sequence"), the hub logged "worker frame failed" and answered `internal` with
   `retryable: true`, and the worker resent the event after every reconnect (in the home-mac
   outbox — seq 50, session `01a16533-8d0e-7367-95a4-fcfc52253ef5`). Now (SPEC 0.13, §5.2):
   the hub and the Claude Code adapter replace U+0000 with U+FFFD; an event the database
   rejected because of its data gets `invalid_frame` without retry; after 10 retryable errors
   the worker deletes the event itself. The stuck event will go through after the hub is
   updated (stored with U+FFFD) or, if the hub is old, will be deleted by the worker after 10
   attempts.

8. **Keychain after replacing the binary** (T6 question #15 closed). The item's ACL is tied to
   the binary's signature; an unsigned build after a rebuild triggers a `SecurityAgent` dialog,
   and the worker process blocks until it is answered on screen. The production worker on
   home-mac was switched to `HQC_TOKEN_STORE=file` and re-enrolled (`--name home-mac`).
   RUNBOOK §2.4 was fixed.

9. **WebSocket and HTTP/3 (Chrome).** Half a day of diagnostics on 2026-10-23: the client in
   Chrome on the Mac could not open `/api/ws`, while REST, push, Safari, the phone and the worker
   all worked. Not Origin (T9), not Binding Cookie, not extensions, not Caddy and not the tunnel:
   over HTTP/3 Chrome does WebSocket through extended CONNECT in a QUIC session, and the
   Cloudflare edge does not carry it to the origin, even bypassing Access (`/worker/ws` from the
   console fails the same way, without a 401). Solution: HTTP/3 is turned off in the zone. The
   T9 advice about Binding Cookie is withdrawn as well: with it, WebSocket does not get through
   Access. RUNBOOK §1.6.
