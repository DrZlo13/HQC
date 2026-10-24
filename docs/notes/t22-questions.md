# T22 — hardening before rollout: questions and decisions

A security review before rolling the hub out to the internet behind Cloudflare Access (SPEC
0.23, §20 — the limits table, §23). Threat model — §2.1: a compromised worker or its token, a
malicious authenticated user of another account, an external anonymous party on `/worker/*`.
There were no live `claude` runs: everything was checked by tests with a fake worker.

## What was done (the test that fails without the fix)

| item | where | test |
|---|---|---|
| 1. `worker.approval.request`: 20 open per worker, `request_id` ≤ 128, `kind` ≤ 64, `description` ≤ 4096 bytes, `expires_at` in the future and no further than 24 h → `error invalid_frame`; push at most once per 10 s per worker | `requests.rs`, `push.rs`, `worker_ws.rs` | `hardening.rs`: `worker_requests_are_limited`, `worker_requests_push_at_most_every_10_s` |
| 2. Mirror: 200 new sessions per day per worker, 2000 per workspace, `runtime_session_id` is a uuid, `cwd` ≤ 4096 bytes and inside the workspace → `ack` without `seq` and a warn | `mirror.rs`, migration `0012` (`sessions.imported`) | `mirror_checks_transcript_id_and_cwd`, `mirror_sessions_are_limited`, unit `transcript_ids` |
| 3. Service session: only `system worker_log`; `user.message`, `approval.response`, `clarify.response`, `peer.message` from a worker are `error invalid_frame` in any session | `events.rs` | `service_session_takes_only_worker_log`, `worker_cannot_write_hub_events` |
| 4. Worker inbox: 100 MB per artifact, 500 MB per input, abort on `Content-Length` and on the stream, the partial file is deleted, an error without retry | `inbox.rs` (`InboxError::TooLarge`) | unit `inbox::tests::downloads_are_limited` |
| 5. Client: a diff body only after `size`; text and diff > 2 MB only via "Download"; `store.text` does not read a body with a `Content-Length` above the display limit | `artifacts.ts`, `ui/attachments.tsx` | `attachments.test.tsx`: "a diff body waits for its size…", "a body announced over the limit…" |
| 6a. Worker events for archived sessions: `ack` without `seq`, except the final `agent.message` and `status idle/error` | `events.rs` | `events_for_archived_sessions_are_dropped` |
| 6b. Takeover without the registry: `locked_by: vscode` without `vscode_active_at` counts as "active" | `mirror.rs::takeover`, `db.rs` (`set_locked_by`, `set_process_state` set the time on a transition to `vscode`), migration `0012` | `takeover_without_vscode_time_is_refused` |
| 6c. Watchdog: step `limit / 10`, from `expiry_interval` to 10 s; partial index `sessions_turn_open` | `watchdog.rs`, migration `0012` | unit `watchdog::tests::step`, `turn_open_is_indexed` |
| 6d. `fs.*` from the API: 4 at a time per worker, then `429 worker_busy` without a frame to the worker | `api.rs::ask_worker`, `WorkerConn::fs_slots`, `ApiError::Limited` | `fs_requests_per_worker_are_limited` |

## Decisions along the way

- **`expires_at` is rejected, not clamped.** §10.2 says "no further than a day" in the sense
  of clamping; per the task it is now a refusal. There is no allowance for clock skew: the
  worker asks for 10 minutes (§10.2), a day is a large margin. The §10.2 text was not edited
  per the T22 agreement (spec edits only in §20 and §23); the §20 table says explicitly that
  it takes precedence. Align the wording at the next edit of §10.2.
- **A repeat of a known `request_id` is checked before the limits.** The worker resends an open
  request after every reconnect; if the open-request limit were checked before the lookup, the
  20th open request would fail its own repeat. The length of `request_id` is checked before
  going to the database.
- **`description` > 4096 bytes is a refusal**; it is still stored up to 2000 characters.
- **Push throttling** is in process memory, per worker, only for requests in the service
  session; the request itself is written and visible in the client, only the notification is
  skipped.
- **"Answers to its own `worker.approval`" in the service session.** The worker does not write
  them as events: the hub stores and writes the answer, the worker sends only the
  `worker.approval.request` frame. So the only thing accepted from a worker into the service
  session is `system` with `code: worker_log`; `approval.cancel` and everything else are
  `error invalid_frame`.
- **`peer.message` was added to the hub-only events**, although the task did not list it: per
  §4 the hub writes it on `hqc_send`/`hqc_ask`; a forgery from a worker would look like a
  message from another session.
- **Mirror sessions are marked with the `imported` column.** Otherwise they cannot be told
  apart from hqc sessions (both have a `runtime_session_id`). Sessions imported before `0012`
  are not marked: the daily window for them passed long ago, and they do not count toward the
  2000-per-workspace limit — accepted.
- **The mirror limits drop only records of new transcripts**; records of already known
  transcripts go through. An invalid `runtime_session_id` and a long `cwd` are also `ack`
  without `seq`, like the earlier "outside the workspace" check, not `invalid_frame`:
  otherwise the worker would drop the event from the outbox just the same, but with an error
  in its log for every record.
- **Archived sessions.** Only events that close a turn get through: the final `agent.message`
  and `status idle`/`error`. `agent.delta` of an archived session is no longer broadcast to
  clients. The mirror (`mirror.event`) writes to archived sessions as before: it is the
  transcript's history, and the task's restriction concerned agent events.
- **Takeover.** The lock time is now set on any transition to `vscode` (`set_locked_by`,
  `set_process_state`), so "no time" remains only for rows from before `0012`, and the
  migration sets `now()` for them. Consequence: a worker without the registry that reported
  `locked_by: vscode` in `session.state` gives 30 s of silence before takeover — previously
  takeover was immediate. With the registry the 30 s rule does not apply (§9.3), as before.
- **Watchdog**: both the index and the step. The index `sessions (id) WHERE turn_open` — the
  `running_turns` query reads only sessions with an open turn. The step is `limit / 10` within
  `[expiry_interval, 10 s]`: with the default limit of 15 min, a turn is lost no later than
  after 15 min 10 s of silence; in tests with a 600 ms limit the step is 60 ms.
- **`429 worker_busy`** is a new variant `ApiError::Limited(code, …)`; the same limit applies
  to `workspace.list_candidates` (the same `ask_worker` path). The client does not handle the
  code separately — it shows the error message, like other `4xx`.
- **Inbox.** The size-exceeded error is `InboxError::TooLarge`, not retryable: the hub marks
  the input `failed` (§5.2). The rest of the input's budget is passed on to the download of
  the next artifact, so the total does not exceed 500 MB even without `Content-Length`. On
  error the input's directory is deleted entirely, as before.
- **Client.** The diff display limit in the feed is unchanged — 512 KB (§12); 2 MB is the
  boundary beyond which text and diff have no "Open" button (a browser tab would hold the
  whole body). Until the metadata arrives, a diff is a "loading…" card; a metadata error gives
  a plain card.

## Merge with T21

- Spec version: T22 raises 0.22 → 0.23. If T21 also raised the version, merge them into one
  §23 entry on merge (or 0.23 and 0.24 in merge order).
- The T22 migration is `0012_hardening.sql`; number `0011` is left to T21.
- Both tasks edit `mirror.rs` (T21 — mirror on by default): a conflict is expected in
  `ingest`; the T22 checks sit before the session lookup and in the branch that creates a new
  one.
- `hqc-proto` did not change.
- *Done on merge (SPEC 0.24):* the daily limit on mirror sessions no longer drops the record
  with an `ack`, but answers `error mirror_quota` (`retryable: true`, a new code in
  `hqc-proto`); the worker removes the record from the outbox and rereads the transcript from
  the start no earlier than an hour later (`mirror::QUOTA_RETRY`). Otherwise the first T21
  import wave beyond 200 sessions would be lost forever. The 2000-per-workspace limit still
  drops with an `ack`.
