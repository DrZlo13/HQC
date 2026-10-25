# T36: questions and decisions

Hardening of the MCP surface and the review backlog; closes iteration 3 (SPEC 0.39). Threat
model (SPEC §2.1): the hub is on the public internet behind Cloudflare Access; an agent acts
with its owner's rights, so a prompt-injected or compromised agent (a), a compromised worker of
the same owner (b) and another user of the hub (c) must not exhaust the hub, the worker or the
owner's subscription, nor reach another owner's data. No live `claude` run was made (see
section 3).

Hub tests named below are in `crates/hqc-hub/tests/t36.rs` unless another file is given;
worker tests in `crates/hqc-worker/tests/mcp.rs` or the unit tests of the module named.

## 1. Every item

### 1.1 The MCP surface (task item 1)

| item | status | how | test |
|---|---|---|---|
| `mcp.call` per session | fixed | 60 a minute per calling session (`Limits::mcp_calls_per_session_min`), `limit_exceeded` | `calls_are_limited_per_session_and_per_worker` |
| `mcp.call` per worker | fixed | 300 a minute per worker, counted before the token is looked up, so a flood of calls with bad tokens costs no database lookups | same |
| open asks per session | fixed | 5 open `hqc_ask`s per asking session, `limit_exceeded`, nothing stored | `open_asks_are_limited_per_session` |
| peer messages per session per minute | fixed | 20 a minute per sending session; `hqc_send`, `hqc_ask` and the prompt of `hqc_spawn` count | `peer_messages_are_limited_per_session_and_per_chain` |
| peer messages per chain | fixed | 50 per chain in all, counted from `events` (expression index `events_peer_chain`, migration `0019`). Needed beyond `hops` ≤ 20: `hops` grows along a path only, so one human turn could fan out without end at `hops` 1 | same |
| `hqc_spawn`: limit check and insert not atomic (T34) | fixed | the count and the creation run under a per-owner async lock in the hub process (`AppState::owner_lock`). An advisory lock in Postgres was tried first and dropped: it holds a pooled connection while `new_session` needs others, so enough parallel spawns could exhaust the pool. The hub is one process (SPEC §2) | `parallel_spawns_and_schedules_stay_within_the_caps` (12 calls at once against a cap of 3; without the lock all 12 passed) |
| schedules (T35) | fixed | the count of 100 and the insert under the same per-owner lock; runs sent per owner and hour capped at 60 (a run over it is skipped with a warn); an agent's schedule continues its chain (section 2.1) | `parallel_spawns_and_schedules_stay_within_the_caps`, `scheduled_runs_are_limited_per_owner_and_hour`, `an_agents_schedule_continues_its_chain`, unit `schedules::tests::hostile_patterns_are_cheap` |
| search cost per owner (T33 had only the 5 s timeout) | fixed | 30 searches a minute per owner (REST and tool together), 10 of them fuzzy (the expensive kind: about 1 s on common trigrams, T33 §4); at most 4 searches at once on the hub (`503 busy` / `unavailable`) | `searches_are_limited_per_owner_and_at_once` |
| T32 race 1: a call gets the previous turn's chain | fixed | `mcp.call.turn_input`: the worker sends the `event_id` of the input it handed to the runtime (`McpCalls::set_turn` in the supervisor, before `input_with`); the hub takes the chain from it when it is an input of the caller (`user.message`/`peer.message`, `pending` or `delivered`) not older than the last delivered one, else falls back to the last delivered input. The worker cannot name an older turn to get a fresh chain; naming a newer queued input gives it nothing it could not get by acking that input | hub `a_call_takes_the_chain_of_the_turn_the_worker_names`; worker `calls_carry_the_input_of_their_turn` |
| T32 race 2: a buffered message cleared without reaching the agent | fixed | the first send of an input freezes its `peer_prefix` (table `input_prefix`, migration `0019`): a resend carries exactly the buffered messages it carried then, never newer ones, so its `ack` clears only what the first frame had. The row goes with the input's `ack` | `a_resent_input_carries_the_prefix_of_its_first_send` |
| an ask whose target turn was lost | fixed | `events::turn_lost` (process stopped, worker restart, watchdog) fails the open asks whose question is the target's last delivered input with `unavailable` at once; the question is not delivered again, so no reply could come. A turn that ends without a final message for another reason (the user's stop) still waits for the timeout: no event marks such an end reliably, so it was left as it was | `an_ask_fails_when_the_targets_turn_is_lost` |

### 1.2 The session token on a shared worker (task item 2, T30 question 2)

| item | status | how | test |
|---|---|---|---|
| another session's agent reads the token from argv and calls tools as that session | fixed, **[verify]** | the daemon takes the peer's pid from the kernel at `accept` (`LOCAL_PEEREPID` / `SO_PEERCRED`, via tokio's `peer_cred`) and accepts the hello only if that pid descends from the agent process the adapter started for the token's session (`mcp::procs::descends_from`: `proc_pidinfo(PROC_PIDTBSDINFO)` on macOS, `/proc/<pid>/stat` on Linux, at most 32 parents, never into pid 1). `Adapter::pid` reports the process (Claude Code: the spawned CLI; the fake adapter: the test process); the supervisor sets it after the start, and a hello waits up to 10 s for it (the CLI starts MCP servers during its own start). An adapter that reports no process gets no MCP server (`AgentPid::Unknown`) | unit `mcp::procs::tests::ancestry`, `mcp::tests::tokens_and_calls`; `a_token_used_outside_the_agents_process_tree_is_refused` (the right token from a process outside the "agent" is refused; an agent with no pid too) |

What it does not close: an agent that can ptrace another agent (same user: macOS restricts
`task_for_pid` to the debugger entitlement, Linux with `kernel.yama.ptrace_scope` 1 to
descendants), and the rest of the soft boundary of §14 (files, the environment). A pid reused
between `accept` and the walk would need the peer to exit and its pid to be given to a
descendant of the victim's agent within microseconds; not handled.

### 1.3 The 2026-10-23 backlog (task item 3), re-verified against the current code

| item | status | where / how | test |
|---|---|---|---|
| `requests.rs` `worker_approval`: no cap on open rows, past `expires_at`, `request_id`/`kind` uncapped | fixed in T22, holds | `requests.rs` (20 open per worker, `request_id` ≤ 128, `kind` ≤ 64, `expires_at` in the future and ≤ 24 h) | `hardening.rs::worker_requests_are_limited` |
| `mirror.rs` `ingest`: unlimited sessions | fixed in T22/T21, holds | 200 new sessions per worker per day (`error mirror_quota`, retried by the worker an hour later) and 2000 per workspace; checked: it covers every path that creates a session from `mirror.event` (the only `create_session` in `ingest` is behind both checks) | `hardening.rs::mirror_sessions_are_limited` |
| … `runtime_session_id` not a uuid, `cwd` uncapped | fixed in T22, holds | lowercase hyphenated uuid; `cwd` ≤ 4096 bytes and inside the workspace | `hardening.rs::mirror_checks_transcript_id_and_cwd` |
| `events.rs`: a worker writes any event type into its service session | fixed in T22, holds | only `system worker_log`; hub-only types refused everywhere | `hardening.rs::service_session_takes_only_worker_log`, `worker_cannot_write_hub_events` |
| … (new in T36) a worker sets `chain`/`delivery` on its events | fixed | dropped from `event` frames; a `mirror.event` keeps `delivery` only as `delivered` on a `user.message` (section 2.2) | `hardening.rs::worker_events_carry_no_chain_or_delivery`, `mirrored_events_are_never_pending_input` |
| worker `inbox.rs` download: no byte cap per artifact / per input | fixed in T22, holds | 100 MB per artifact, 500 MB per input, streamed, partial file removed | unit `inbox::tests::downloads_are_limited` |
| … inbox never pruned | fixed in T21, holds | `session.cleanup` on archive/delete, and a sweep of inbox folders older than 14 days at the worker's start | unit `cleanup::tests` |
| client `attachments.tsx`: `DiffView` fetching before `meta.size` | fixed in T22, holds | a diff body only once `size` is known and ≤ the display limit; > 2 MB download only | `client/tests/attachments.test.tsx` "a diff body waits for its size…", "a body announced over the limit…" |
| archived sessions still accept events | fixed in T22, holds; extended | only turn-ending events; since T36 also no `mcp.call` from an archived session (`unavailable`) and no upload for one (`404`) | `hardening.rs::events_for_archived_sessions_are_dropped`; `request_ids_and_archived_callers_are_refused`, `worker_uploads_have_an_hourly_budget` |
| takeover 30 s rule ignores `locked_by=vscode` without `vscode_active_at` | fixed in T22, holds | treated as "just now" | `hardening.rs::takeover_without_vscode_time_is_refused` |
| the watchdog's full scan every second | fixed in T22, holds | step `limit/10` in `[expiry_interval, 10 s]`, partial index `turn_open` | unit `watchdog::tests::step`, `hardening.rs::turn_open_is_indexed` |
| unbounded concurrency of worker fs requests | fixed in T22 (hub side), holds | 4 at once per worker from the API, `429 worker_busy` without a frame. The worker itself does not cap `fs.*` frames: they come only from the hub, which the worker trusts for them (they read no file content and stay in `allowed_roots`) | `hardening.rs::fs_requests_per_worker_are_limited` |
| scratch dirs never pruned | fixed in T21, holds, one gap left | `session.cleanup` removes `<scratch_root>/<session>`; a worker offline at archive/delete time misses the frame (`send_to_worker` is fire-and-forget), and the start-up sweep covers inbox folders only. Low: scratch folders are used only by sessions without a workspace, which have no hqc tools that write files (iteration 4) | unit `cleanup::tests::scratch_folder_of_a_session_without_workspace` |

### 1.4 The adversarial review (task item 4)

| finding | attacker | severity | status | how / test |
|---|---|---|---|---|
| An agent keeps itself running for ever: each turn `hqc_schedule`s its own next turn, which arrives as the owner's `user.message` with a fresh chain (the subscription is spent without end) | a | high | fixed | the schedule keeps the call's chain moved on, its runs carry it, a call in the run's turn continues it; at the hop limit `hqc_schedule` is refused; plus 60 runs per owner and hour. `an_agents_schedule_continues_its_chain`, `scheduled_runs_are_limited_per_owner_and_hour` |
| One human turn fans out unlimited sends/asks (`hops` 1 each) and each target turn fans out again (tree of depth 3) | a | medium | fixed | 50 peer messages per chain, 20 per session per minute (1.1) |
| A worker resets the chain limits of its session's turns by sending an event with `chain: {hops: 0}` and `delivery: delivered` (the hub took the last delivered event's chain), and can mark its own events `pending` input | b | medium | fixed | `chain`, `delivery`, `delivered_at` dropped from worker events; mirror `delivery` normalized (1.3) |
| `request_id` from a worker reaches another worker's agent inside the ask header (`[… request_id=<x> | …]`): with `]` or a newline it ends the header and writes text that looks like the hub's | b | medium | fixed | the hub refuses a `request_id` outside `A-Z a-z 0-9 . _ : -` (1–128); the worker reduces header fields to one line without `[`, `]`, `|`. `request_ids_and_archived_callers_are_refused`; unit `peer::tests::header_fields_stay_inside_the_header` |
| `hqc_attach` in a loop fills the hub's disk (100 MB per call, dedup only for equal content) | a, b | medium | fixed | 2 GB per worker and hour on `PUT /worker/artifacts` (`429 upload_limit`, the tool's `limit_exceeded`). `worker_uploads_have_an_hourly_budget` |
| Fuzzy search on common trigrams costs ~1 s of database per call, uncapped | a, c (own data only) | medium | fixed | 1.1. `searches_are_limited_per_owner_and_at_once` |
| `hqc_attach` error messages named the resolved target of a path outside the workspace and told "not found" from "exists" outside it: a probe of the file system for an agent restricted from Bash | a | low | fixed | a path outside the workspace as written is refused before the file system is asked, no refusal names a resolved path. Unit `mcp::attach::tests::paths_inside_the_workspace_only`, `symlinks_are_resolved` |
| Owner boundaries of T31–T35: `hqc_list_sessions`, `hqc_attach` (artifact of the owner **and** linked to the calling session), `hqc_send`/`hqc_ask` targets, `hqc_search_history` and `GET /api/search` (`owner_id` and the owner's project), `hqc_spawn` (project of the owner, workspace of that project, worker of the owner), `hqc_schedule` and `/api/schedules` (`own_session`, `own_schedule`, list filtered by owner), `PATCH /api/me`, `spawned_by` in the session view, `PUT /worker/artifacts` (a session of this worker) | c | — | checked, no finding | existing tests: `mcp.rs::list_sessions_of_the_owner`, `the_worker_uploads_for_its_own_sessions`, `search.rs::exact_and_fuzzy_hits_of_the_owner_only`, `spawn.rs::spawn_takes_the_owners_project_and_workspace_only`, `peers.rs::refusals_come_at_once`, `schedules.rs::refusals_and_limits` |
| Search query injection | c | — | checked, no finding | the query is a bound parameter of `websearch_to_tsquery` and `<%`; the cursor is parsed into a timestamp and a uuid; `project` resolves among the owner's projects |
| Cron / time zone input | a | — | checked, no finding | 5 fields only, `croner` parse; the 100-run interval check stays under 0.1 s for sparse patterns (Feb 29 on a Monday) and 60-item lists (`hostile_patterns_are_cheap`); time zones are `chrono-tz` names; `message` ≤ 16 KiB |
| The spawned session's model | a | low | open, by design | `model` absent inherits the caller's; present, any valid model of the owner (an agent may start an opus session). Within the owner's rights; the cost is bounded by the spawn caps (5 per chain, 20 per hour). A per-owner allowlist of models for agents would be the next step if needed |
| The spawned session's workspace | a | — | checked, no finding | a workspace of the named project only, on a worker of the owner; `auto_approve` never set |
| `hqc_attach`: a directory component swapped between `canonicalize` and `open` | a | low | open (T31 §2) | the agent races its own worker inside its own workspace, whose files it can read with Bash anyway |
| A worker's `approval.request`/`clarify.request` events in its normal sessions open `pending_requests` rows without a cap, each with a push to the owner | b | low | open | own sessions only (the owner's devices get spammed, nobody else's); T22's cap covers the service session. A per-session cap would refuse the event, which a correct worker never sees; left for when a worker misbehaves |
| `mcp.call` is handled inline in the worker connection's loop: a 5 s search blocks that worker's other frames meanwhile | a | low | open | the caps bound it to 10 fuzzy searches a minute per owner; moving tool calls off the loop would need care with the ordering of `ack`s (§5.2) |
| `caption` of `hqc_attach` has no cap of its own (64 KB `args`) | a | low | open (T31 §8.2) | the owner's own feed; shown in one line |
| An event id predicted by a worker (`once_id` of a schedule run, whose `schedule_id` the target worker sees) and stored first by that worker suppresses the run | b | low | open, harmless | the run goes into that worker's own session, which it controls anyway; ids of other calls derive from the worker's random `request_id`s |
| The in-memory budgets (calls, messages, searches, uploads, runs) reset when the hub restarts | — | low | accepted | a restart is not something an agent can cause; the counted caps (spawns, schedules, asks, messages per chain) are in the database |

## 2. Decisions

### 2.1 An agent's schedule continues its chain (touches §7.1 [decision])

T35 decided "a scheduled turn is a human's turn by proxy" (notes t35 §6). With `hqc_schedule`
open to agents that made the chain limits of §7.1 useless: an agent could schedule its own next
turn, or another session's, every few minutes, each a fresh human chain. The fix keeps T35 for
people (a schedule made in the client carries no chain) and changes it for agents: the schedule
row has the call's chain moved on like a send (`hops` + 1, refused at the limit), and each run
is a `user.message` with that `chain`, which `turn_chain` already reads. §7.1's rule "a
`user.message` has no chain" now has this one exception, written into §7.1 and §4. This is a
change in a **[decision]** section, made without asking because the alternative leaves an
unbounded loop on the owner's subscription; flag it for review.

The runs-per-hour cap (60 per owner) is a second, coarser line: it also bounds a person's own
schedules, which nobody hits by hand (a `*/5` schedule is 12 an hour).

### 2.2 Worker events and the mirror

The hub took `chain` and `delivery` of a worker's `event` frame as sent. No worker code sets
them (`adapter.rs`, `mirror/map.rs`); only inputs, which the hub writes, have them. They are
now dropped (`delivered_at` too). A `mirror.event` keeps `delivery: delivered` on a typed
prompt (what the worker's mirror sends, so `turn_chain` sees the VS Code prompt as a human's
turn) and none on anything else; `pending` from a worker would have made the hub deliver it to
the session's agent.

### 2.3 Where the caps live

- Rates (calls, messages, searches, uploads, runs) are sliding windows in the hub's memory
  (`security::Budget`, weighted, with the time given by the caller so the cron uses its test
  clock). They need no migration, cost no query, and an agent cannot restart the hub to reset
  them.
- Totals that must survive restarts are counted in the database: spawns (sessions), schedules,
  open asks (`pending_requests`), messages per chain (`events`, new expression index).
- All numbers are in `config::Limits` (no environment variables, as for T22), so tests lower
  them; the spawn constants of T34 moved there (`spawns_per_chain`, `spawns_per_hour`).
- The worker-level `mcp.call` rate is checked first and also counts calls that are refused
  afterwards (bad token, bad `request_id`): it is the guard against floods, not a quota.
- A refused search does not count against the owner's general rate (the fuzzy check comes
  first and returns before it).
- Open asks are counted without a lock: all calls of a session come over its worker's one
  connection, which the hub handles a frame at a time. Spawns and schedules are per owner,
  whose sessions may sit on several workers, hence their lock.

### 2.4 Numbers

| cap | value | why |
|---|---|---|
| `mcp.call` per session | 60/min | an agent's turn calls a tool every few seconds at most; Claude Code runs parallel calls, so not lower |
| `mcp.call` per worker | 300/min | five busy sessions |
| open asks per session | 5 | parallel tool calls of one turn |
| peer messages per session | 20/min | a fan-out to a handful of machines |
| peer messages per chain | 50 | above the 20 hops of a path, below a runaway tree |
| searches per owner | 30/min, 10 fuzzy | typing in the search bar sends one per pause (300 ms debounce, T33) |
| searches at once | 4 | the database's cores |
| scheduled runs per owner | 60/h | twelve `*/5` schedules |
| uploads per worker | 2 GB/h | twenty 100 MB files |

## 3. Not verified live

The process-ancestry check rests on Claude Code starting its stdio MCP servers as children of
the CLI process (the MCP SDK's stdio transport uses `child_process.spawn`). A live run of
`crates/hqc-hub/tests/live_mcp.rs` would show it; it could not be run from this session (the
run needs an approval that was not available). The spec marks it **[verify]** in §7.2. If it
fails, every hqc tool fails with `unauthorized: this process is not part of the session's
agent` and the worker logs "a peer outside the session's agent refused" with both pids; the
first live run after this merge (any `live_*` test) settles it.

## 4. Open questions

1. A per-owner model allowlist for `hqc_spawn` (1.4).
2. A cap on open approval/clarify requests per normal session (1.4).
3. Moving `mcp.call` handling off the worker connection's loop (1.4).
4. Resending `session.cleanup` to a worker that was offline (1.3).
