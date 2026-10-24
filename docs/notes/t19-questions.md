# T19 — worker re-enrollment and cleanup of revoked workers: decisions, questions

Task: enrollment that replaces an existing worker, archiving the service session on
revocation, deleting a revoked worker. SPEC 0.21 (§3 Worker, §4, §5.1, §6, §10.2, §17.4, §23);
task: `docs/ITERATION-2.md`. Live claude runs: zero.

## Decisions made along the way (not covered by the task statement)

- **The replacement happens on confirmation, not on polling.** The user needs the checks
  (`409 worker_online`, `400` for someone else's worker) right away, in the answer to
  "Confirm". So on confirmation the hub already resets the replaced worker's `token_hash` (the
  old token stops working), carries over `name`/`os`, clears `revoked_at`, resets `boot_id`,
  unarchives the service session and writes `system worker_replaced`; on polling, only the new
  token's hash is written to the same `worker_id` — exactly as for a new worker. If after the
  confirmation the worker never picks up the token (the process was killed), the replaced
  worker is left with no token and no revocation — "offline"; the cure is the same replacement
  once more.
- **Migration `0009` is needed.** Polling must report the previous name (`replaced`), and by
  then the name in `workers` is already the new one — the previous one is stored in
  `enroll_codes.replaced_name`. The same migration archives the service sessions of workers
  revoked before T19, otherwise they would stay in the "Workers" group.
- **`boot_id` is reset** on replacement: the new process's first `hello` is a restart (§5.2):
  the sessions' `process_state` is reset, open requests get `worker_restart`. The old run's
  processes no longer exist on the machine (or should not), so this is honest.
- **Online status is checked from the hub's memory; a revoked worker is never online.** The hub
  closes a revoked worker's connection itself, but the connection registry is freed
  asynchronously; a revoked worker always counts as not connected (`status: revoked` takes
  precedence over `online`). If a race still lets a connection through, the replacing
  confirmation drops it (`close`).
- **A second replacement of the same worker before polling has picked up the first** is
  `409 conflict`: otherwise the second poll would overwrite the first one's token, and the
  first process would fall off on reconnect. Refusals (`worker_online`, `400`, `409`) do not
  consume the code.
- **Someone else's or a nonexistent worker in `replace_worker_id` is `400` (`bad_request`,
  "unknown worker")**, as with `worker_id` in `POST /api/workspaces`, not `404`: `404` here
  already means "no such code".
- **`replaceable` lists both revoked and offline workers**, with the number of non-archived
  sessions — so that choosing "the same machine" is deliberate. "New worker" is selected by
  default: a replacement hands all of the replaced worker's sessions to the new process, and
  the link with the code may have been sent by someone else (§5.1) — hence an explicit choice
  by name rather than a checkbox.
- **The name on replacement is the new one** (sent by `enroll`, `--name`); the service
  session's title changes with it (§10.2: the title is the worker's name). The confirmation's
  `name?` field is still not shown in the client.
- **Where revoked workers are shown.** `GET /api/workers` without parameters returns only
  non-revoked ones, as before (choosing a worker for a workspace, import, names); "Settings"
  asks for `?include_revoked=1`. New value `status: revoked` and field `revoked_at`.
- **On revocation the hub archives the service session itself** (the `/archive` ban is for
  the user). The worker's open requests in it are closed with `approval.cancel {archived}`
  without a `worker.approval.response` frame: there is no one to deliver it to, and after a
  replacement the new process has a different `boot_id`, so the answer would be dropped
  anyway. The "Workers" group in the client filters service sessions by `archived`, so
  revocation and replacement are visible right away through `session.summary`, without a
  separate list of workers.
- **"Delete" is `DELETE /api/workers/{id}?purge=1`**, not a separate route: in the style of
  §6 (`DELETE` is the same resource), and revocation stays a mandatory first step
  (`409 worker_not_revoked` for a non-revoked worker) — a working machine cannot be deleted
  by accident with one request.
- **Which sessions block deletion.** "Sessions" in `409 worker_has_sessions` are the
  non-archived ones, other than the service session, with the worker's `worker_id` or in its
  workspace. Archived ones do not block: as when deleting a workspace (§13.2), they lose
  `workspace_id` (and here `worker_id`) and stay as history; they cannot be unarchived
  (`409 no_workspace`). This way "or archive them" from the task statement really does lift
  the refusal. The projects' default workspace is recomputed as in §13.2.

## Questions

1. **Deleting together with archived sessions.** Now the archived sessions of a deleted worker
   remain without a worker and workspace. Is an option "delete them too"
   (`?purge=1&sessions=1`) needed? Not for now: history is cheap, and sessions can be deleted
   one by one.
2. **Replacing an online worker.** Forbidden (`409 worker_online`) — as in the task statement.
   The scenario "I am moving the worker to a new machine, the old one still runs" requires
   stopping or revoking the old one first. If "replace with disconnection" is ever needed, it
   is `?force=1` with revocation.
3. **Confirming a replacement from another worker of the same user.** Replacement is possible
   onto any of the user's machines: the hub does not know whether it is "the same" machine (a
   worker has no persistent machine identifier). A fingerprint could be added to
   `POST /worker/enroll` (a hash of `IOPlatformUUID` / `/etc/machine-id`) and replacement
   offered only for matching ones — but that is one more field leaking from the machine; I did
   not do it without a decision.
   Resolved in T21: `machine_id` (sha256 salted with `hqc` of `IOPlatformUUID` /
   `/etc/machine-id` / `MachineGuid` / a random id in `$HQC_HOME`) in `enroll` and `hello`;
   workers of the same machine come first in `replaceable` with `same_machine: true`, the
   client preselects replacing them, but another worker can be replaced too (SPEC 0.23 §5.1).
4. **The worker's local state on a replacement with a different `$HQC_HOME`.** The outbox, the
   `known_workspaces.json` cache and the mirror offsets live in `$HQC_HOME`; when the
   directory changes, unsent events of the old run are lost, and folders of other people's
   sessions will need confirmation again. The hub has no influence on this; the RUNBOOK
   describes it implicitly (only the token storage changes). Is a separate note needed?
