# Iteration 2: tasks

Goal (SPEC §19.2): projects and workspaces, the worker's service session, import and mirror of
VS Code, takeover in both directions, artifacts, `.hqc/inbox`. Criterion: take over from a phone
a session started in VS Code. The iteration 1 simplifications "one project (personal)" and "one
workspace from the worker's config" are lifted in T14.

Each task is self-contained: it can be handed to a separate agent together with `CLAUDE.md`
and the listed sections of `docs/SPEC.md`. Dependencies are stated. The iteration's other tasks
are added here as they are set.

## T14. Projects and several workspaces (done, SPEC 0.16)

Sections: §3 (Project, Workspace, Session, Worker), §5.3, §5.4, §6, §13, §14, §19.2.
- hqc-proto: hub → worker frames `fs.stat {request_id, path}` → `result {exists, is_dir,
  canonical, inside_allowed_roots}` and `fs.list_dirs {request_id, path | null}` → `result
  {entries[{name, path}]}`; REST bodies for projects and workspaces; examples in `examples/wire`;
  `workspace_path` removed from the `hello` example.
- Hub: CRUD for projects (the personal one cannot be deleted or renamed), `default_workspace_id`
  is the project's first workspace; `POST /api/workspaces` checks the path via `fs.stat`
  (offline: `409 worker_offline`), stores `canonical`; on one worker paths are unique and not
  nested (`409 workspace_conflict` with `existing_workspace_id`, `existing_project_id`,
  `existing_path`); `GET /api/workers/{id}/dirs` proxies `fs.list_dirs`; a session is
  bound to a `workspace_id` (required) and a `cwd`; the migration moves the iteration 1
  workspace and archives sessions with no known folder.
- Worker: no `[workspace]` in `worker.toml` and no `workspace_path` in `hello`; the path from
  `session.start` is inside `allowed_roots`, `cwd` is inside the workspace; `fs.*` answers
  only inside `allowed_roots`.
- Client: the left column shows projects and the selected project's sessions; creating a project;
  a "Workspaces" screen (list, adding with a folder tree and a manual path, deleting a record);
  choosing the workspace and `cwd` when creating a session; "this folder is already in project
  <name>" with a link.
- `tools/dev.sh`, `tools/e2e-local.sh`: the workspace is created via REST once the
  worker is online.
Acceptance: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace` (hub integration tests: creating a workspace checked by a
fake worker, 409 on a duplicate, on a nested and on a containing path, 409 with the worker
offline, migration of existing sessions, `list_dirs` only inside `allowed_roots`),
`cd client && npm test && npm run build`, `E2E_CLAUDE_BIN=/usr/bin/false
tools/e2e-local.sh` prints `PASS (rehearsal)`. Questions: `docs/notes/t14-questions.md`.

## T15. Import, mirror and takeover of Claude Code sessions (done, SPEC 0.17 on the branch, 0.19 after the merge)

Sections: §3 (Session, Workspace, Event), §4, §5.2–§5.4, §8.1 (identifiers, resume), §9,
§17.1. Depends on T14. Iteration criterion: take over from a phone a session started in VS Code,
and release it back.
- hqc-proto (additions only): hub → worker `workspace.list_candidates {request_id}` →
  `result {candidates[{path, sessions_count, last_active}]}` and `mirror.set {workspace_path,
  enabled}`; worker → hub `mirror.event {workspace_path, runtime_session_id, cwd, event,
  title?, recent}`; REST `CandidateList`, `CreateWorkspaceImport` (`mirror_enabled`);
  `Event::id_from_claude_block`; examples in `examples/wire`.
- hqc-claude-jsonl: `Message::is_main` (not a sidechain, not `isMeta`, not a `/compact` summary),
  `stop_reason`, `blocks`, `first_cwd`; 2.1.308 fixtures `vscode-takeover` (a `claude -p` session,
  takeover by the adapter via `--resume` and one more turn) and `adapter-then-outside` (an
  adapter session and `claude -p --resume` from outside), recorded with
  `tools/spike/driver.py --arg-prompt / --resume`.
- Worker: the mirror polls `<config>/projects/*/<uuid>.jsonl` every 2 s for folders from
  `mirror.set` (only inside `allowed_roots`), offsets and a "file → cwd" cache in
  `$HQC_HOME/mirror/mirror.sqlite3`, the first pass imports the history; mapping records to §4;
  a registry of its own uuids (`OwnRecords`), filled by the adapter from stream-json; a foreign
  record while the process is alive: after 5 s the agent is stopped after the turn, `session.state
  locked_by: vscode`, input is rejected with `locked_vscode` until the next `session.start`;
  candidates by `cwd`; `mirror.event` through the outbox.
- Hub: a session for an unknown transcript in the workspace's project (`cwd` from the record,
  `ai-title`, `locked_by` from `recent`), authors from the hub, no push; `POST
  /api/sessions/{id}/takeover` (`409 vscode_active` for 30 s, `session.start mode: resume`,
  `system session_taken_over`); `409 locked_vscode` with `hint: takeover`; release (`system
  session_released`, `pending` inputs become `failed`); `mirror.set` on import and after
  `hello`; `GET /api/workers/{id}/candidates`; migration `0008` (`vscode_active_at`;
  it was `0006` on the branch, renamed in T18).
- Client: "Import from Claude Code" on the Workspaces screen (the worker's candidates; the
  chosen folders become workspaces with the mirror on); in the chat `locked_vscode` shows "In VS
  Code", input is closed, a "Take over" button.
Acceptance: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (parser on the T15 fixtures; mirror: own records are not foreign on
fixture a, a foreign writer on fixture b, import, incomplete lines, candidates; worker with a fake
hub: release after the turn and `locked_vscode`; hub with a fake worker: import, `409
locked_vscode`, `409 vscode_active`, takeover, release, `mirror.set` after `hello`), `cd
client && npm test && npm run build`; live `HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test
live_takeover` ("VS Code" session → import → takeover → turn → release). Questions:
`docs/notes/t15-questions.md`.

## T16. Artifacts and `.hqc/inbox` (done, SPEC 0.17 on the branch, 0.18 after the merge)

Sections: §3 Artifact, §4 (`user.message.artifact_ids`, `attachment`), §5.3 `session.input`,
§6, §8.1, §12, §20. No MCP: `hqc_attach` and `PUT /worker/artifacts` are iteration 3.
- hqc-proto: the `Artifact` and `ArtifactUrl` bodies (`api.artifact.json`, `api.artifact.url.json`).
- Hub: on-disk storage `HQC_ARTIFACT_DIR` by sha256, migration `0007` (`artifacts`,
  `artifact_sessions`; it was `0006` on the branch); `POST /api/artifacts` (multipart `file`,
  `kind?`; 100 MB, `413 too_large`; dedup by `(owner, sha256, name)`), `GET /api/artifacts/{id}`,
  `GET /api/artifacts/{id}/url` (HMAC, 10 minutes); `GET /artifacts/{id}?exp=&sig=` (`403`/`410`,
  sandbox CSP, HTML inline only on the `HQC_ARTIFACT_PUBLIC_BASE` host, that host serves
  only artifacts, its origin is in the client's CSP); `GET /worker/artifacts/{id}` with the worker
  token for that worker's sessions; input checks `artifact_ids` (`400 unknown_artifact`)
  and links them to the session.
- Worker: `.hqc/inbox/<event_id>/` in the working directory (sanitized and unique names, no
  symlinks along the path, a temporary file and `rename`), `.hqc/` in `info/exclude`, an
  "Attachments:" block in the text, an image content block in Claude Code's stream-json
  **[verify]**; fixture `crates/hqc-worker/fixtures/inbox`.
- Client: a paperclip, drag-and-drop and pasting files with image previews; upload before
  the input, retry without re-uploading; rendering attachments (image inline, diff, html in an
  iframe only from the artifact domain, text/file as a card); links are re-requested on
  `403`/`410`; paths like `src/main.rs:42` in inline code and relative links are copied
  to the clipboard ("path copied", t11 #1).
- `Dockerfile` (`/var/lib/hqc/artifacts`), RUNBOOK §1.2, §1.6, §1.8, `tools/dev.sh`,
  `tools/e2e-local.sh`.
Acceptance: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace` (hub: `tests/artifacts.rs`: upload and kinds,
the limit, dedup, link signature and expiry, someone else's artifact, headers and the artifact
domain, download by the worker; worker: `inbox_scenario`, `inbox_refusals`,
`image_attachments_are_image_blocks`, `inbox` unit tests), `cd client && npm test &&
npm run build` (`tests/attachments.test.tsx`). Questions: `docs/notes/t16-questions.md`.

## T17. The worker's service session and non-session approvals (done, SPEC 0.17)

Sections: §3 (Worker, Session, PendingRequest), §4, §5.3, §5.4, §10.2, §13.1, §13.2, §16, §17.4.
- hqc-proto (additions only): worker → hub frames `worker.approval.request {request_id,
  kind, description, expires_at}` and hub → worker `worker.approval.response {request_id,
  option_id}` with examples in `examples/wire`; optional `hello.ack.system_session_id` and
  `session.start.requested_by_owner`; `Runtime::None`; `Session.system`.
- Hub: every worker has a service session (`workers.system_session_id`; on enrollment,
  via migration `0006` for existing ones, on `hello` if missing) in the owner's personal
  project, with no workspace, `runtime: none`; input, stop, retry, archiving, deletion give
  `409 system_session`. `worker.approval.request` → an `approval.request` event with
  `once`/`deny` and a `worker_approval` `PendingRequest`: only the worker owner answers, the
  answer goes out as a `worker.approval.response` frame (also after a reconnect), expiry gives
  `approval.cancel timeout` and `deny`, a new `boot_id` gives `worker_restart`; push as for a
  regular approval. `worker_log` lines do not count as unread.
- Worker: a `tracing` layer forwards warn/error to the service session (`system worker_log`,
  `level`) through the outbox, the same text at most once a second, ≤ 60 per minute in total,
  the rest as a "skipped N" line. `session.start` of someone else's session
  (`owner_is_worker_owner: false`, without `requested_by_owner`) in a folder not in
  `$HQC_HOME/known_workspaces.json` waits for the owner's approval (`workspace.use`): `once`
  remembers the folder, `deny` or a timeout gives `error {invalid_frame}`. Partly closes audit
  item A2.
- Client: a "Workers" group in the left column, the service session's feed without an input
  field, approval cards, `worker_log` lines with time and level, a "requests only" filter; a hub
  mock with a service session.
Acceptance: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace` (hub: `tests/system_session.rs`: creating the service
session and the migration, input refused, `worker.approval` there and back, owner only,
expiry and restart, resending the answer after `hello`, push; worker:
`tests/system_session.rs`: approval of a new folder for someone else's session, denial, no
approval for the owner, the request repeated after a reconnect, rate-limited logs; unit tests
for `worker_log` and `known_paths`), `cd client && npm test && npm run
build` (rendering `worker_log` and the cards, the filter, a scenario through the mock). Questions:
`docs/notes/t17-questions.md`.

## T18. Merging T15 with T16 and T17 (done, SPEC 0.19)

Sections: §3 (Session, Agent), §4, §5, §5.3, §5.4, §6, §9, §10.2, §12, §16, §23. Depends on
T15, T16, T17. T16 and T17 were merged earlier (SPEC 0.18), T15 is merged last.
- Code: both sides of every conflict. hqc-proto: the `worker.approval.*` frames next to
  `workspace.list_candidates`, `mirror.set`, `mirror.event`; the example lists cover all of them.
  Hub: the session has both `system` and `vscode_active_at` (in `SESSION_COLS` too); `worker_ws`
  parses `worker.approval.request` and `mirror.event`, after `hello` it resends the answers to
  the worker's requests and sends `mirror.set`; `POST /takeover` on a service session gives `409
  system_session` (new, tested in `tests/system_session.rs`). Worker: `HubLink::event_json`
  and `mirror_event`, the supervisor holds the pending `workspace.use`, the artifact client and
  the mirror. Client: the `Composer` with attachments (T16) is not rendered for the service
  session (T17), and for `locked_vscode` it is closed with the caption "In VS Code" and a "Take
  over" banner (T15); the hub mock answers input with `409 system_session` and `409 locked_vscode`.
- T15's migration `0006_mirror.sql` → `0008_mirror.sql` (`0006` is T17, `0007` is T16); it
  only adds `sessions.vscode_active_at` and an index and does not depend on its neighbours.
- SPEC 0.19: §23 entries 0.18 → 0.19 (T15), 0.17 → 0.18 (T16), 0.16 → 0.17 (T17); what the three
  tasks deferred is moved into §3, §4, §5, §6, §10.2, §16 (the list is in the 0.19 entry). T16's
  decision "the user's files are always in `user.message.artifact_ids`" is recorded in §4.
Acceptance: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace`, `cd client && npm run typecheck && npm test && npm run
build`, `E2E_CLAUDE_BIN=/usr/bin/false tools/e2e-local.sh` → `PASS (rehearsal)`, live
`HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_takeover` → PASS (44 s, 2026-10-23).
Open items: the questions in `docs/notes/t15-questions.md`, `t16-questions.md`, `t17-questions.md`.

## T19. Worker re-enrollment without losing bindings, and cleanup of revoked workers (done, SPEC 0.21)

Sections: §3 (Worker, Workspace, Session), §4, §5.1, §6, §10.2, §17.4. Background: enrolling the
same machine again (a new binary, a new token store) gave a new `worker_id`; sessions,
workspaces and the service session stayed on the old one, "worker offline" forever, and the
service sessions of revoked workers hung around in the "Workers" group.
- hqc-proto: `EnrollInfo.replaceable[]` (`ReplaceableWorker {id, name, os, status, last_seen?,
  sessions_count}`), `EnrollConfirmRequest.replace_worker_id?`, `EnrollConfirmed.replaced?`,
  `EnrollPoll::Confirmed.replaced?`; examples `api.enroll.*.replace*.json`.
- Hub: `GET /api/workers/enroll/{code}` lists the user's offline and revoked workers;
  `POST /api/workers/enroll/confirm` with `replace_worker_id` checks the owner
  (`400`) and that the worker is not connected (`409 worker_online`), the code gets the existing
  `worker_id`, the old token is reset at once, `name`/`os` are new, `revoked_at` and `boot_id`
  are reset, the service session is unarchived and renamed and gets
  `system worker_replaced` (`old_name`, `new_name`); polling returns `replaced`. `DELETE
  /api/workers/{id}` archives the service session with `system worker_revoked`, its requests
  are closed with `archived`. `DELETE /api/workers/{id}?purge=1` deletes a revoked worker,
  its service session and workspaces (`409 worker_not_revoked`, `409 worker_has_sessions` with
  `sessions_count`). `GET /api/workers?include_revoked=1`, `status: revoked`. Migration `0009`:
  `enroll_codes.replaced_name`, archiving the service sessions of already revoked workers.
- Worker: `enroll` prints `Enrolled as worker <id> (replaced <name>).`.
- Client: the confirmation screen offers "New worker" / "The same machine, replace
  <name>" (status, last seen, number of sessions); in "Settings" revoked workers have
  "Delete" with an explanation of `worker_has_sessions`; the "Workers" group has no archived
  service sessions; hub mock.
- Docs: SPEC §3 Worker, §4, §5.1, §6, §10.2, §17.4, §23 (0.21); RUNBOOK §2.3
  (re-enrolling the same machine), §2.6 (revocation and deletion).
Acceptance: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace` (hub: `tests/worker_replace.rs`: replacement keeps the id,
sessions and workspaces, the old token stops working, a new `hello` with the same
`system_session_id`; a connected worker and someone else's worker cannot be replaced;
revocation archives the service session, replacing a revoked worker brings it back; purge with
sessions gives `409`, without them it deletes; migration `0009`; worker:
`enroll_reports_the_replaced_worker`), `cd client && npm test && npm run build`
(`tests/enroll.test.tsx`: the replacement dialog; `tests/workers.test.tsx`: "Delete", 409, the
"Workers" group without revoked workers; e2e through the mock), `E2E_CLAUDE_BIN=/usr/bin/false
tools/e2e-local.sh` → `PASS (rehearsal)`. Questions: `docs/notes/t19-questions.md`.

## T20. Takeover without forks (done, SPEC 0.22)

Sections: §3 (Worker, Session), §4, §5.2, §5.4, §6, §9.2, §9.3. Background (2026-10-23, the
user's Mac): a session was open in VS Code; hqc took it over after 30 s of silence in the file,
the worker appended turns via `--resume`; then the user wrote into the same open session in VS
Code, and the extension attached the turn to its own last record in memory, not to the tail of
the file. In the jsonl that record has two children, VS Code sees only its own branch, and the 37
hqc records never reached its context. The "30 s of silence" rule cannot tell a closed session
from an open, idle one.
- hqc-proto (additions only): `HeldBy {entrypoint, status}`; `held_by?` in `session.state` and
  `mirror.event` (field absent / `null` / object); `held_by?` in the session object and
  `session.summary`; `system` with the `session_forked` fields (`SessionForked {at_uuid,
  branch_from, orphaned_turns}`, `Event::id_of_fork`); examples
  `worker.session.state.held.json`, `worker.session.state.released.json`,
  `worker.mirror.event.forked.json`, `api.session.vscode.json`.
- hqc-claude-jsonl: `Link` (`uuid`, `parentUuid`, `isSidechain` of any record); fixture
  `fork-2.1.308` (a live run without VS Code: `--resume --resume-session-at`).
- Worker: a registry module (`mirror/holders.rs`) reads `<config>/sessions/<pid>.json` once per
  mirror pass, live pids (`kill -0`), excluding its own pids and process groups (the pids of the
  adapter's children are in `OwnRecords`), tolerant of unknown fields and broken files; holder
  changes are sent as a holder report (`session.state` with a nil `session_id`) through the
  outbox, at most once per 2 s per transcript, and again after every `hello`; a session whose
  transcript was opened outside hqc is stopped after the turn; `held_by` in `mirror.event`;
  `capabilities.claude_code.sessions_registry`. The transcript's record tree (`mirror/fork.rs`)
  and `system session_forked` through `mirror.event`.
- Hub: migration `0010` (`sessions.held_by`, `vscode_held_at`); a holder → `locked_by: vscode`
  (from `hqc`: `session_released` with `reason: vscode_open`, `pending` → `failed`), the holder
  is gone → `none` at once if there is no hqc process; with registry data `recent` does not lock;
  `POST /takeover` with a holder gives `409 vscode_open` with `held_by`, without a holder on a
  worker with a registry it is immediate, 30 s is the fallback rule; `held_by` on `409
  locked_vscode`; holders are forgotten on `hello`; `system` from the mirror has author `system`.
- Client: the banners "The session is open in VS Code … close it there" (no button) and "In VS
  Code" (a "Take over" button); `409 vscode_open` is shown with the hub's text; a
  `session_forked` line with a warning icon; mock: the import brings a session open in VS Code
  with a fork, `closeVsCode` for tests.
- Docs: SPEC 0.22 (§3, §4, §5.2, §5.4, §6, §9.2, §9.3, §19, §23);
  `docs/notes/t15-questions.md`: the question about release is closed with a link; RUNBOOK §4:
  the takeover scenario with a "close the session in VS Code" step.
Acceptance: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (worker: `mirror::holders`: live/dead pid, own pids and group,
`sdk-cli` of its own transcript, broken files; `mirror::fork`: parallel calls are not a fork,
a fork is reported once, a cycle; `mirror::tests::fork_is_reported_once`, `holder_in_registry`;
`tests/mirror.rs::holder_releases_the_session`; hub: `tests/mirror.rs`:
`holder_blocks_takeover_until_closed`, `holder_releases_hqc`, `forked_event_is_stored`; parser:
`t20_fork_in_one_transcript`), `cd client && npm test && npm run build` (`projects.test.tsx`:
an open session without "Take over", `409 vscode_open` as text, release; `render.test.tsx`:
the fork line), `E2E_CLAUDE_BIN=/usr/bin/false tools/e2e-local.sh` → `PASS (rehearsal)`.
The live `HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_holder` confirmed the holder and
`409 vscode_open`; release after the process exits failed and was fixed (the order of reports
and `recent`); the live run was not repeated. Questions: `docs/notes/t20-questions.md`.

## T21. Five user decisions: mirror by default, titles, machine fingerprint, cleanup (done, SPEC 0.23)

Sections: §3 (Worker, Workspace, Session, Artifact), §5.1, §6, §9.1, §9.2, §12, §17.4, §23. The
user's decisions, not up for discussion; T22 (limits) ran in parallel, so hqc-proto changes are
additions only, migration `0011`, §5.3 and §5.4 were not edited.
- hqc-proto (additions only): `MirrorSet.sessions[] {runtime_session_id, since_ts}`, a hub →
  worker frame `session.cleanup {session_id, workspace_path | null, cwd, event_ids[]}`,
  `Hello.machine_id?`, `EnrollRequest.machine_id?`, `ReplaceableWorker.same_machine`,
  `PatchWorkspace.mirror_enabled?`; `CreateWorkspaceImport.mirror_enabled` defaults to `true`;
  examples `hub.mirror.set.sessions.json`, `hub.session.cleanup*.json`,
  `worker.hello.machine.json`, `api.enroll.request.machine.json`,
  `api.workspace.patch.mirror.request.json`, `same_machine` in `api.enroll.info.replace.json`.
- Hub: migration `0011` (`mirror_enabled` on by default and for existing ones,
  `workers.machine_hash`, `enroll_codes.machine_hash`); `PATCH /api/workspaces/{id}
  {mirror_enabled}` → `mirror.set`; `mirror.set` with the workspace's sessions and the time of
  their last event (after `hello`, on creation and on enabling); an `llm` module
  (`complete(system, user, max_tokens)`, providers `anthropic` and `openai`), titles after the
  first final reply (`titles.rs`); `machine_hash` from `enroll` and `hello`, ordering and
  `same_machine` in `replaceable`; artifact cleanup once a day (`HQC_ARTIFACT_RETENTION_DAYS`),
  the file is moved aside and moved back if a record appeared; `session.cleanup` on archiving
  and deleting a session or a project.
- Worker: the mirror's first pass over a transcript from `mirror.set.sessions` skips records with
  `timestamp` <= `since_ts`; `cleanup.rs`: `session.cleanup` (the inputs' inbox folders or
  scratch) and, at startup, cleanup of inbox folders older than 14 days by `known_workspaces.json`
  and `inbox_dirs.json` (the working directories attachments were written to), with paths checked
  against `allowed_roots` and the workspace, without following symlinks; `machine.rs`: the
  machine fingerprint for `enroll` and `hello`.
- Client: a "VS Code sessions" switch on each workspace; preselecting the replacement of a worker
  on the same machine, marked "same machine"; parsing the `session.cleanup` frame; hub mock.
- Docs: SPEC 0.23; RUNBOOK §1.2 (`HQC_TITLE_*`, `HQC_ARTIFACT_RETENTION_DAYS`), §4;
  questions of T15, T16, T19, T20 marked "Resolved in T21".
Acceptance: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (hub: `tests/t21.rs`: mirror by default and `PATCH` (including
offline), `mirror.set` with the `since_ts` of the last event, migration `0011`, `same_machine` and
ordering, `machine_hash` from `hello`, titles through fake Anthropic- and OpenAI-format servers,
fallback with no model, on `500` and on timeout, not overwriting a user's title or an `ai-title`,
the title of an imported session, artifact cleanup (shared sha256, `attachment`, fresh ones),
`session.cleanup` on archiving and deletion; worker:
`mirror::tests::enabling_on_a_workspace_with_hqc_sessions_brings_no_duplicates`
on the `adapter-then-outside` fixture, `cleanup::tests`: only the listed folders, refusal outside
`allowed_roots`, `..`, symlinks on `cwd`, `.hqc` and the event folder, scratch, startup cleanup
only of old uuid folders in known places; `machine::tests`; `inbox_scenario`:
`session.cleanup` through a fake hub; `enroll_reports_the_replaced_worker`: `machine_id`), `cd
client && npm test && npm run build` (`enroll.test.tsx`: preselection, `projects.test.tsx`: the
switch), `E2E_CLAUDE_BIN=/usr/bin/false tools/e2e-local.sh` → `PASS (rehearsal)`. No live
`claude` runs. Questions: `docs/notes/t21-questions.md`.

## T24. Titles on the worker's subscription, no model providers in the hub (done, SPEC 0.25)

Sections: §3 Session, §5.3, §5.4, §8.1, §23. The user's rule: anything that has no consumer
now is deleted, not kept for the future.
- Hub: removed the `llm` module, `HQC_TITLE_MODEL`, `HQC_TITLE_BASE_URL`, `HQC_TITLE_API_KEY`,
  the model call semaphore, their tests and the fake model servers (the hub's dependencies did not
  shrink: `reqwest` with `json` is needed for JWKS and push). After the first final reply of a
  session with `title_source: default`, a `title.request` goes to the session's worker, waiting
  `timing.title_request` (30 s); offline, an error, an empty answer and a timeout give a title
  from the first prompt. Imported sessions work the same way.
- hqc-proto (additions): `HubFrame::TitleRequest {request_id, session_id, user_text,
  agent_text}`, `ResultBody::Title {title}`, `MAX_TITLE`, `MAX_TITLE_INPUT`; examples
  `hub.title.request.json`, `worker.result.title.json`.
- Worker: `title.rs`: a one-off `claude -p` with fixed flags in `$HQC_HOME/titles/<slot>`,
  at most 2 at a time, a 25 s timeout, cleaning up the answer.
- Client: parsing the `title.request` frame in `proto.ts` (the examples contract test).
- Docs: SPEC 0.25; RUNBOOK §1.2 without `HQC_TITLE_*`, §2.2, §2.5; the command line is in
  `docs/notes/claude-control-protocol.md`; `docs/notes/t21-questions.md`: providers removed.
Acceptance: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (hub: `tests/t21.rs`: `title.request` once after the first turn,
the answer sets the title, a user's title and an `ai-title` are not overwritten, an error, an
empty answer, a timeout and offline give a title from the prompt, an imported session; worker:
`title::tests`: the command line, cleaning up the answer, the limit of 2, the folder is removed
on error and on timeout too; `title_request_runs_claude_once` through a fake hub), `cd client &&
npm test && npm run build`, `E2E_CLAUDE_BIN=/usr/bin/false tools/e2e-local.sh` →
`PASS (rehearsal)`. Live run: `live_session_title` (`HQC_LIVE_CLAUDE=1`).

## T25. False release of a session while hqc is working (done, SPEC 0.26)

Sections: §9.2, §9.3, §23. Production log of 2026-10-24 (drzlo-mac): the first input after
`--resume` and a `/compact` from hqc produced a "foreign writer" with no foreign holder, and the
session was released in the middle of work.
- Worker, mirror: when the holder registry is available, a record with an unknown `uuid` while
  an hqc process is working is not a foreign writer; after 5 s it is emitted with `held_by`. Only
  a foreign live holder releases (T20). Without a registry: the old `uuid` check; without an hqc
  process the rules are unchanged.
- Worker, `OwnRecords`: the adapter stores the texts of inputs; a prompt record with an unknown
  `uuid` is its own by its text (for `<command-name>`: the command with its arguments).
- Fixture `resume-compact-2.1.308` (three live runs of `driver.py`).
- The hub did not change.
Acceptance: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (worker: `registry_unknown_record_without_holder_keeps_the_session`,
`resume_prompt_without_its_echo_uuid`, `no_registry_keeps_the_uuid_check`, `claim_by_text`,
`typed_text_of_prompts_and_commands`; `hqc-claude-jsonl`: `t25_resume_prompt_and_compact`).
Questions: `docs/notes/t25-questions.md`.

## T26. Client redesign from the mockups (done, SPEC 0.27)

Sections: §4, §5.3, §6, §10.1, §17.1, §23. Mockups: `docs/design/redesign-2026-10-24` (Main,
Permission, Mobile-List, Mobile-Session).
- Protocol (additions): `approval.response.message`, a text reply instead of the call, only with
  the deny option; the hub checks it and passes it to the worker, the Claude Code adapter puts the
  text into the `message` of the `deny` response with Claude Code's wording (two live runs of
  `driver.py`, 2.1.308).
- Client: a project strip, a session column with "State"/"Recent" modes (localStorage) and
  search, workers with active sessions; the session header (pill, worker, path, model, cost,
  "Stop", "More"); a feed without bubbles for the agent, groups of tool calls with a caption,
  a "Running … timer" line; a permission card with Enter/Esc and a text reply; two takeover
  banners instead of the input field; on a phone, the list on one screen and a bottom sheet for
  the request; the Inter and JetBrains Mono fonts served by the hub.
Acceptance: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (hub: `approval_deny_with_message`; worker:
`deny_with_message_reaches_control_response`), `cd client && npm run typecheck && npm test && npm
run build`, `tools/e2e-local.sh` with `E2E_CLAUDE_BIN=/usr/bin/false`. Questions and
discrepancies with the mockups: `docs/notes/t26-questions.md`.

## T27. Per-turn cost, slash command completion, shrinking layout (done, SPEC 0.28)

Sections: §4, §5.4, §6, §8.1, §23. Three problems reported by the user.
- Cost: `result.total_cost_usd` of Claude Code is the session's running total (live runs on
  2.1.308, fixtures `cost-turns`, `cost-resume`); the adapter emits the turn's share in
  `usage.cost_usd` (baseline: the previous `result`, or the transcript's last `cost-state` after
  `--resume`) and the total in the new `usage.session_cost_usd`; the header shows the latest total.
- Slash commands: `slash_commands` of `system/init` (without the terminal-only and internal
  ones) go to the hub in `session.state.commands`, the hub keeps them on the session (migration
  0013) and in the session object; the composer completes `/` from that list.
- Layout: the session column narrows with the window, markdown tables scroll inside themselves;
  measured in Chrome from 1440 to 390 px.
Acceptance: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (worker: `turn_cost_is_the_growth_of_the_session_total`,
`resumed_turn_cost_starts_from_the_last_cost_state`, `adapter_resumes_with_the_transcript_cost`,
`command_list_is_reported_when_it_changes`, `runtime_commands_go_in_session_state`; hub:
`runtime_commands_are_kept_on_the_session`), `cd client && npm run typecheck && npm test && npm
run build` (`chat.test.tsx` cost and completion, `layout.test.ts`), `tools/e2e-local.sh` with
`E2E_CLAUDE_BIN=/usr/bin/false`. Notes — `docs/notes/t27-questions.md`.

## T28. Translation to English (done, SPEC 0.29)

Sections: all, §23. Every Russian text in the repository is translated to English, and English
becomes the project's language.
- Spec, docs, notes, READMEs: `docs/SPEC.md`, `docs/SPEC-CHANGELOG.md`, `docs/ITERATION-*.md`,
  `docs/RUNBOOK.md`, `docs/notes/*`, `client/README.md` and the rest; the terms and UI labels
  follow the glossary in `docs/notes/t28-questions.md`.
- Client UI: every label, message and hint in English.
- Hub and worker: user-visible texts (default session titles, `system` lines, error texts,
  worker log lines, the block listing attachments in the agent's input, the title prompt) in
  English.
- `CLAUDE.md`: English is the language of the spec and docs too.
Acceptance: no Cyrillic outside fixtures, the design mockups and test data; `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`,
`cd client && npm run typecheck && npm test && npm run build`, `E2E_CLAUDE_BIN=/usr/bin/false
tools/e2e-local.sh` → `PASS (rehearsal)`; SPEC version +1 (0.29) with a §23 entry. Notes:
`docs/notes/t28-questions.md`.
