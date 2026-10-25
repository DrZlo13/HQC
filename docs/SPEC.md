# hqc — technical specification

Version 0.41, 2026-10-25. Status: the architecture agreed after six reviews and a survey of
similar systems (§22), before implementation started. Changes are in §23, old versions in
`SPEC-CHANGELOG.md`. Marks: **[decision]** — decided, needs confirmation;
**[verify]** — needs checking on a real build.

Terms: **turn** — one cycle "input → agent work → final answer"; it starts with a
`user.message` or a `peer.message` with `mode=ask` and ends with an
`agent.message` with `final: true`. **Bare model** — an LLM with no access to the file
system. **Takeover** — continuing (resuming) a session somewhere else: from VS Code to hqc
or back. **Hermes** — Hermes Agent by Nous Research; on llm-server it runs as a
daemon (messaging gateway) and can also be started as a separate process acting as an
ACP server (`hermes acp`). **ACP** — Agent Client Protocol (Zed), JSON-RPC over stdio
between a client and an agent: `initialize`, `session/new`, `session/prompt`,
`session/update`, `session/request_permission`, `session/cancel`, `session/load`.
**Worker owner** — the user who enrolled the machine. **Session
owner** — the user who created the session.

## 1. Goals and scope

hqc is a single interface for running projects with LLM agents from any device.
The agents physically run on different machines and operating systems; the user talks to them
through a Discord-like web client: projects, sessions inside projects, an agent in a session.

What hqc does:

- keeps projects, sessions and the full event history in one place;
- starts and drives agents on the machine where the code lives and the hardware is attached;
- shows sessions started in VS Code and lets you take them over from a phone;
- gives agents tools to exchange messages, artifacts and requests with each other;
- lets colleagues in as separate users with explicit sharing.

What hqc does not do:

- it does not implement the agent loop for Claude Code and ACP agents: the loop lives in the
  runtime. For a bare model the hub runs a minimal loop "model ↔ hqc MCP tools" (§8.3);
- it does not duplicate what Hermes offers (agent task cron, kanban, memory, Telegram);
- it is not an IDE: no file editor, diffs only as artifacts;
- it gives no hard isolation between sessions on one machine without a container or a
  separate OS user (§14).

## 2. Topology

**Hub** — one always-available service. Keeps state, routes events, serves the
API to clients and workers. Runs only bare models (§8.3).

**Worker** — a daemon on every machine where agents should run. Connects to the hub
outbound only, over WebSocket. Inside: a native Claude Code adapter, a generic ACP adapter
for Hermes and any agent from the ACP registry, the hqc MCP server, the VS Code session mirror,
workspace operations, a local config with restrictions and environments.

**Client** — a web app, later Tauri and mobile. Talks only to the hub.

Deployment: the hub sits in the home DMZ behind Cloudflare Tunnel. People sign in through
Cloudflare Access (SSO), workers through a separate route with their own tokens (§6, §20).

### 2.1 Threat model

- The worker does not trust the hub for `allowed_roots`, environments, bindings and
  `allow_foreign_sessions`; the user's identity and answers to permission requests come from
  the hub and are trusted **[decision]**. The hub is the root of trust for conversations and
  permissions: whoever controls the hub can answer `allow` to any `approval.request`, write to
  any session and pass any session off as the worker owner's (`owner_is_worker_owner`).
- Consequences of a hub compromise: remote code execution on every connected
  worker as its owner, within `allowed_roots` and whatever the agent's tools allow
  (Bash can leave the workspace) — the hub starts a session, sends input and answers `once`
  to the permission request itself; plus reading all conversations, the transcripts
  in events and the push subscriptions. The protection is the hub itself: Access, tunnel only,
  no open ports (§20). Signing input and answers with device keys registered on the worker
  locally is iteration 3+.
- The hub **cannot**: go outside the worker's `allowed_roots` (also not through an agent's
  `hqc_attach`: the worker resolves and reads the path itself, §7), pick an environment or a
  Hermes profile for a path the worker owner has not bound locally, or enable
  the machine's MCP servers. These are set only by the worker's local config (§14); the worker
  ignores diverging values from `session.start` and answers with an error.
- The promise about local MCP servers holds for Claude Code (`--strict-mcp-config`)
  and Hermes (`HERMES_ACP_SKIP_CONFIGURED_MCP=1`, the worker always sets it). For other
  ACP agents there is no general way: in the local config each `acp_agent` declares
  `isolation: verified | none`; an agent with `none` is not allowed into foreign sessions
  (§11.1) and the promise does not extend to it.
- A worker is remote code execution on the owner's machine. Anyone who can write
  to a session on this worker runs code as the owner within the owner's policy.
- Between sessions of one worker the boundary is soft (§14): the processes run as the same
  OS user and can read each other's environment and files. Since 0.39 (T36) the hqc tools are
  not part of that softness: the session token is on the agent's command line, readable by the
  other sessions' agents, but the worker's MCP socket accepts a session's token only from a
  process that descends from the agent process the worker started for that session (§7.2).
  What stays soft: files, the environment, and anything an agent can do through another
  session's own processes (ptrace where the OS allows it to the same user; macOS restricts it,
  Linux with `ptrace_scope` 1 limits it to descendants).
- An agent acts with its owner's rights (§7), so a prompt-injected or compromised agent can do
  whatever the owner's hqc tools allow on the owner's sessions, and no more: every tool
  resolves its targets among the owner's projects and sessions only (`not_found` otherwise).
  What it can spend is capped (since 0.39, T36; §20): `mcp.call` per session and per worker,
  open asks, peer messages per session and minute and per chain, spawned sessions, schedules
  and their runs, searches, the bytes a worker uploads. Chains (§7.1) bound loops between
  agents; a schedule an agent creates continues its chain, so an agent cannot keep itself
  running by scheduling its own next turn.
- A worker is trusted for its own sessions only (§5.2, §20): it writes their agent events and
  mirrored records, but no event that only the hub writes, nothing in another worker's
  sessions, and no `chain` or `delivery` (since 0.39: the hub drops them from a worker's
  events and keeps a mirrored record's `delivery` only as `delivered` on a typed prompt), so
  it can neither reset the chain limits of its sessions' turns nor queue input. A worker of the
  same owner can still use the owner's hqc tools through its sessions' tokens, within the caps.
- A colleague with access to a shared project is limited by grants (§11) and by the worker
  owner's confirmations (§10).

## 3. Data model

Every entity has `id` (uuid v7), `created_at`, `updated_at`. `owner_id` exists on Worker,
Project, Workspace, Session, Artifact, Grant, ScheduledMessage. An Event has no owner;
it has `author`.

### User
`email` (from the verified Cloudflare Access JWT), `display_name`, `api_keys`
(OpenRouter, Anthropic API; encrypted with the hub's master key, which lives in the hub's
env/file, not in the database), `timezone`, `personal_project_id` (the implicit personal
project, created on first sign-in; the personal assistant session lives in it).

`timezone` (since 0.38, T35) — an IANA name, `UTC` until set; the default zone of new
schedules (§16). The client sets it to the browser's zone (`PATCH /api/me`, §6) when it starts
and the two differ.

### Worker
`owner_id`, `name`, `os`, `token_hash`, `status` (online/offline), `last_seen`,
`protocol_version`, `boot_id` (from the last `hello`, §5.2), `capabilities` (`claude_code: {version, sessions_registry?}`, `acp_agents[{name,
version, load_session, resume, image, isolation}]` — the result of each agent's `initialize`
when the worker starts), `local_models[]`, `share_local_models`, `allowed_roots[]`,
`environments[]` (names), `env_bindings[]` (path → environment, Hermes profile),
`allow_foreign_sessions`, `idle_timeout`, `system_session_id` (§10.2), `machine_hash` (since 0.23:
the machine fingerprint from enrollment and the last `hello`, §5.1; not part of the API worker object).

`system_session_id` — the worker's service session (§10.2). It is created together with the
worker when enrollment is confirmed (`POST /api/workers/enroll/confirm`); for workers older than
0.17 by hub migration `0006`, and if it is still missing, on `hello`. The hub tells the worker in
`hello.ack` (`system_session_id`); `GET /api/workers` returns it in the worker object. Revoking
the worker does not delete the session but archives it (the history stays, §10.2); enrollment
with replacement (§5.1) unarchives it; deleting a revoked worker (§6) deletes it together with
the worker.

Enrollment with replacement (§5.1) keeps the worker's `id`: `name`, `os` and `token_hash` come
from the new enrollment, `revoked_at` and `boot_id` are reset; sessions, workspaces and the
service session stay on this `id`.

The fields `allowed_roots`, `environments`, `env_bindings`, `allow_foreign_sessions`,
`share_local_models`, `acp_agents` — **the source of truth is the worker's local config**;
the hub keeps a copy from `hello` for display and early validation.

### Project
`owner_id`, `name` (1–100 characters), `slug` (unique per owner, does not change on
rename), `description`, `default_workspace_id` (nullable),
`session_template` (§13.3), `is_personal`. A project's first workspace becomes its
`default_workspace_id`; when the default workspace is deleted, the earliest of the remaining
ones takes its place. The personal project (`is_personal`) cannot be deleted or renamed (`409
personal_project`); its other fields can change; any number of new projects can be created.
Deleting a project deletes its workspace records and sessions (with events, as in §17.4); agent
processes get `session.kill`; folders and transcripts on the workers are left untouched.

### Workspace
`project_id`, `worker_id`, `path`, `label`, `environment` (a name; a display of the binding
from the worker's local config), `ephemeral`, `created_by_hqc`, `kind`
(dir/clone/worktree), `origin_workspace_id`, `mirror_enabled` (since 0.23 `true` by default: the
Claude Code session mirror is on for every workspace; the switch is `PATCH`, §6, §9.2).
A workspace is a **logical** boundary: all hqc operations (session cwd, inbox, mirror,
deletion) are confined to its path. It gives no enforced file system isolation.
A workspace is a hub object **[decision]**: all the worker knows about it is that its path lies
inside `allowed_roots` (checked on `fs.stat` and on every `session.start`, §5.3).
`path` — the canonical path the worker returned for `fs.stat` (symlinks resolved).
A folder belongs to exactly one project: on one worker workspace paths are unique and not
nested in each other (compared by path components: `/a/b` and `/a/bc` do not overlap);
a new path that equals an existing workspace, lies inside it or contains it gets
`409 workspace_conflict` (§6). On different workers the same path means different folders.

### Session
`project_id`, `owner_id`, `workspace_id` (nullable; since 0.16 required on creation —
sessions without a workspace (§8.4) cannot be created through the API yet; only the worker's
service session has no workspace, §10.2), `worker_id` (the workspace's worker;
required for Claude Code without a workspace; null for a bare model; ACP agents without a
workspace are not allowed, §8.4), `cwd` (a relative path inside the workspace, `.` by default),
`title`, `title_source` (`default` | `runtime` | `user` — who set the title), `slug`
(unique in the project, does not change on rename), `agent`,
`auto_approve`, `wake_on_send` (false by default), `locked_by` (none/hqc/vscode),
`last_seq`, `archived`, `vscode_active_at` (nullable; when the hub last received a mirror
event with `recent` — a write to the file within the last 30 s; it is how quiet is measured for
a takeover without registry data, §9.3; not part of the API session object), `held_by` (nullable
`{entrypoint, status}`: a live Claude Code process outside hqc that has the session's transcript
open, according to the live sessions registry on the worker, §9.3; in the API session object and
in `session.summary` it is the field `held_by?`), `vscode_held_at` (nullable; since when the
transcript has been held; not part of the API session object), `system` (bool: the worker's
service session, §10.2; the API session object carries only `system: true`), `spawned_by`
(nullable; since 0.37, T34: the session whose agent created this one with `hqc_spawn`, §7;
cleared when that session is deleted) and `spawn_chain_id` (nullable: the chain of that call,
§7.1, kept for the spawn limits of §20; not part of the API session object). `state` and
`queue_len` (§17.1) are **not stored**; the hub computes them when serving. In the first version
a session has exactly one agent; people's access to a session is determined by Grant only.

Title: creating without `title` gives "Session N" (N — the session's ordinal number in the
project, archived ones included; `slug` — `session-N`) with `title_source: default`; `title` on
creation and `PATCH title` set `user`. The hub applies a title from the runtime
(`session.state.title`, §5.4; for Claude Code — `ai-title`, §8.1) and sets `runtime` only if
`title_source` is not `user`: the runtime does not rename a session the user has renamed.

Title from the worker (since 0.23, T21; since 0.25, T24 — on the worker's Claude Code
subscription; the hub has no model providers of its own). After the first final `agent.message`
of a session with `title_source: default` (from the worker or from the mirror: an imported
session likewise, by the first `user.message` records and the final `agent.message`) the hub
sends the session's worker a `title.request` in the background (§5.3) with the first message and
the first answer, each cut to 2000 characters. The worker runs Claude Code once outside the
session (§8.1) and answers with a title of up to 60 characters in the user's language. The hub
waits 30 s for the answer; worker offline, `error`, an empty title or a timeout go only to the
log, and the title becomes the first non-empty line of the first prompt without Markdown markup,
cut at a word boundary to 60 characters with "…". The hub folds the worker's answer to one line
of up to 60 characters. The title is set with `title_source: runtime` and only over `default`
(checked at write time): a user's or runtime (`ai-title`) title that arrived earlier or while
the worker was thinking is not overwritten; an `ai-title` that arrives later replaces it by the
general rule. One `title.request` per session per hub process.

Mirror (§9): `locked_by` — who drives the session: `hqc` — the worker's process (from a takeover
or the first input until it stops), `vscode` — a foreign holder or writer of the transcript
(VS Code, `claude` in a terminal), `none` — nobody. A session created by the mirror gets
`runtime_session_id` = the file's uuid, `cwd` from the record relative to the workspace, the
title from `ai-title` (`title_source: runtime`) or "Session N", `locked_by` per §9.1. It is
looked up by the pair (`worker_id`, `runtime_session_id`).

### Agent (embedded in Session)
`runtime` (claude_code / acp / bare / none; `none` — only for the worker's service session:
there is no agent and no process starts, §10.2), `acp_agent` (the agent's name from the
worker's local config, for `acp`), `model`, `runtime_session_id` (a string: the jsonl uuid for
Claude Code, an arbitrary id for ACP agents), `process_state`
(stopped/starting/running/stopping), `system_prompt`, `disallowed_tools[]`,
`restricted` (§8.4), `start_failures` (a counter, §5.2). Changing the runtime between turns
means **a new runtime session with an empty context**: `runtime_session_id`
is reset, the hqc history stays, and the user is offered a recap of the
last N messages as the starting prompt.

### Event
`session_id`, `event_id` (uuid; for events produced from Claude Code jsonl records —
by the adapter, the mirror and the import alike — it is a uuid v5 of the record's `uuid`, so
the same fact from two sources yields one `event_id`; if a record has several content
blocks, block 0 gets v5 of `uuid` and block `i > 0` gets v5 of the string `"<uuid>/<i>"`
in the same namespace; the rule is mandatory for all three sources (§9.2);
for everything else — v7 from the sender), `seq` (assigned by the hub, monotonic within the
session), `ts`, `author` (`{kind: user|agent, id}` or `{kind: system}` without `id`; for an
agent `id` = its `session_id`), `type`, `payload`, `chain` (`{chain_id, depth, hops}` for
agent→agent chains, otherwise null), `client_op_id` (echoed back to the client), `delivery` and
`delivered_at` — two flat event fields, only on `user.message` and `peer.message`:
`delivery` ∈ `pending | delivered | failed`, `delivered_at` — the delivery time.

### ReadCursor
`user_id`, `session_id`, `last_read_seq`. The source of "unread" (the rule is in §6).

### Artifact
`owner_id`, `session_id` (the first session whose input carried the artifact; null until then),
`kind` (file/image/diff/html/text), `mime`, `size`, `sha256`, `name` (the last path component
without control characters, up to 255 bytes), `storage_key` (= `sha256`: the content on
disk is shared by all records with the same hash). A record is unique by `(owner_id, sha256,
name)`: the same file from the same user under the same name is the same record. The hub
separately remembers which sessions received the artifact in their input: their workers may
download it (§5.3, §12). Events refer only to `artifact_id`; download links are issued at render
time (§12). Unused artifacts are removed by the hub's cleanup (since 0.23, §12); `updated_at` is
the last upload of the same file or the last input with it.

### Grant
`owner_id` (who granted), `resource_type` (project/session), `resource_id`, `user_id`,
`level` (view/write/manage).

### PendingRequest
Open requests that survive a hub restart: `kind`
(approval/clarify/ask/worker_approval/mcp_call), `session_id`, `request_id`,
`payload`, `expires_at`, `from_session_id`, `from_event_id`, `worker_id`
(for ask and mcp_call: whom to give the result to), `result` (filled in when the answer has
been received but not yet delivered to the worker).

An `ask` (since 0.35, T32) is a row of the **asking** session: `session_id` — the asking
session, `request_id` — the `mcp.call`'s, `worker_id` — the asking session's worker,
`payload` — `{target_session_id, event_id, to}` (`event_id` — the `peer.message` it put
into the target, `to` — the target's address), `expires_at` — the ask's timeout, `result` —
the `mcp.result` frame once it is answered, timed out or closed by a worker restart (§7.1).
`from_session_id`/`from_event_id` stay unused; `mcp_call` is not used: `hqc_send` keeps no
row of its own (§5.2).

### ScheduledMessage
`owner_id`, `session_id` or (`project_id`, `workspace_id`, template), `cron` or `at`,
`timezone`, `message`, `enabled`, `last_run`, `catch_up`.

Since 0.38 (T35; table `scheduled_messages`, migration `0018`): `session_id` only — the
project-template variant waits for session templates (iteration 5); exactly one of `cron` (5
fields) and `at` (a time); `timezone` — an IANA name, the owner's `timezone` when not given;
`next_run` — when it is due next (UTC), null once it will not run again (an `at` that ran or
was skipped); `catch_up` false by default. Deleting the session deletes its schedules.

## 4. Event schema

One schema for all runtimes. The adapter translates the runtime's stream into these events;
the client renders only them. The "source" column is the component that produces the event;
`author` is set per §3. `?` marks an optional field: it is absent when there is no value
(`null` reads the same).

| type | payload | source |
|---|---|---|
| `user.message` | `text`, `artifact_ids[]`, `schedule_id?` (since 0.38: sent by the hub cron for that `ScheduledMessage`, §16; the client marks it "scheduled") | client; hub cron (§16, authored by the owner; since 0.39 the run of an agent's schedule has its `chain`, §7.1); mirror — prompts from the Claude Code transcript (§9.1) |
| `agent.delta` | `text` | adapter; transport only, not written to the database |
| `agent.message` | `text`, `final`, `in_reply_to?` (event_id of the input that started the turn) | adapter. `final: true` — end of the turn; `final: false` — text between tool calls |
| `agent.thinking` | `text` | adapter, if the runtime provides it |
| `tool.call` | `call_id`, `tool`, `args_preview` | adapter |
| `tool.result` | `call_id`, `result_preview`, `error` (bool), `error_text?` (short error text), `duration_ms?`, `artifact_id?` (a diff or a large result, §12) | adapter |
| `approval.request` | `request_id`, `tool`, `description`, `options[{id, label, canonical}]`, `expires_at?`, `policy_hint?` (allow/deny) | adapter; `options` are **the agent's real options**, `canonical` ∈ once/session/always/deny/deny_always for the client's buttons; `policy_hint` — the call matches an `allow` or `deny` rule of the worker's `[permissions]` block (§14); no field — it matches neither; the client labels the card with it |
| `approval.response` | `request_id`, `option_id`, `by` (user id or `auto`), `message?` (the user's text instead of the call, only with a `deny`/`deny_always` option, §10.1) | client, or the hub with `auto_approve` |
| `approval.cancel` | `request_id`, `reason` (timeout/stopped/worker_restart/archived) | adapter, worker or hub. An answer from another device is not a cancellation: the first answer closes the request, the second gets `409 request_closed` (§10.1) |
| `clarify.request` | `request_id`, `questions[{id, text, options?: [string], multi?: bool}]` | Claude Code adapter: the `AskUserQuestion` tool arrives as `can_use_tool`, the answer goes into `updatedInput.answers` (verified on 2.1.307 with `--permission-prompt-tool stdio`, §8.1). ACP agents have no source for it |
| `clarify.response` | `request_id`, `answers` (`{<question id>: string \| [string]}`) | client |
| `clarify.cancel` | `request_id`, `reason` (the same set as for `approval.cancel`) | adapter, worker or hub |
| `attachment` | `artifact_id`, `kind`, `name`, `caption?` | the agent via `hqc_attach` (since 0.34, T31): the hub writes it, `author: {kind: agent, id: <session id>}`, `kind` and `name` of the artifact; the user's files go into `user.message.artifact_ids` (rule below) |
| `peer.message` | `from_session_id`, `from?` (the sender's address `<project-slug>/<session-slug>` when it sent, since 0.35), `text`, `mode` (send/ask), `request_id?` (for ask: the `mcp.call`'s) | the hub on `hqc_send`/`hqc_ask`/`hqc_spawn` (since 0.37, the new session's first input); `author` is `{kind: agent, id: <the sending session>}`, `chain` is set (§7.1); `delivery: pending` in the input queue, none while in the send buffer (§7.1) |
| `usage` | `input_tokens`, `output_tokens`, `cache_read?`, `cache_write?`, `cost_usd?`, `session_cost_usd?`, `model?` | Claude Code adapter on `result`; for ACP agents optional and without `cost_usd` (ACP v1 has no standard usage). Since 0.28 (T27): `cost_usd` is the cost of this turn only; `session_cost_usd` is the runtime's own running total of the whole runtime session after the turn (Claude Code: `result.total_cost_usd`, carried over `--resume`), absent if the runtime has none (§8.1). The client's header shows the latest `session_cost_usd`, else the sum of `cost_usd` |
| `status` | `state` (§17.1), `detail?` | worker or hub |
| `system` | `text`, `code`, `level?` (`warn` \| `error`, only on `worker_log`) | hub or worker; codes below |

`system` codes (the list is open; the client shows an unknown code as text):

| code | source | meaning |
|---|---|---|
| `session_created` | hub | the session was created (for one created by the mirror: "Session imported from Claude Code", §9.1) |
| `turn_lost` | hub | the turn was lost: the agent process stopped mid-turn, the worker restarted, or there have been no turn events from the worker for a long time (watchdog, "no events for N s"; §17.3) |
| `agent_exited` | worker | the agent process exited on its own with an error or mid-turn (§17.3) |
| `policy_denied` | worker | a tool call was rejected by a `deny` rule from the worker's `[permissions]`, without asking the owner (§8.1, §14); the client shows "Blocked by worker policy" |
| `agent_start_failed` | hub | the agent failed to start 3 times in a row (or non-retryably); `pending` inputs became `failed` (§5.2) |
| `input_rejected` | hub | the worker rejected an input other than as a failed start; the input became `failed` (§5.2) |
| `ask_cancelled` | hub | in the asking session: a deferred `hqc_ask` was closed by `mcp.cancel` from the worker (§5.2), since 0.35 |
| `peer_evicted` | hub | in the target session: the send buffer was over its limit and its oldest messages were dropped unread (§7.1), since 0.35 |
| `session_taken_over` | hub | the session was taken over (`POST /takeover`, §9.3) |
| `session_released` | hub | the session was continued or opened outside hqc (VS Code, `claude` in a terminal): the worker stopped the agent at once, interrupting a turn in progress (since 0.30), `pending` inputs became `failed`; field `reason`: `vscode_open` — a live foreign process holds the transcript, `foreign_write` — the file was written from outside (§9.3) |
| `session_forked` | worker (mirror) | a fork in the transcript: the writer continued from an earlier record, and the other line's turns did not get into its context; fields `at_uuid` (the record with two lines), `branch_from` (`vscode` \| `hqc` — who started the new line), `orphaned_turns` (turns in the other line). The client draws it as a line with a warning (§9.3) |
| `worker_log` | worker | a warn/error line of the worker's log in its service session, the level is in the `level` field (§10.2); not counted as unread (§6), no push (§16) |
| `workspace_unknown` | hub (0.16 migration) | an iteration 1 session without a workspace whose worker did not report a folder: archived when the hub was upgraded |
| `worker_replaced` | hub | in the service session: the worker was re-enrolled with replacement (§5.1); extra fields `old_name`, `new_name` |
| `worker_revoked` | hub | in the service session: the worker's token was revoked, the session archived (§5.1, §10.2) |

Rules:

- Events are tolerant: unknown fields in `payload` are ignored; an event of an unknown
  `type` is parsed as "unknown" and stored and forwarded as is; clients do not
  render it but do not crash; an unknown enum value inside an event
  (`state`, `canonical`, `mode`, `kind`, `reason`, `policy_hint`) reads as "unknown". An event
  of a known `type` with an invalid `payload` is an error.

- The user's files always go into `user.message.artifact_ids[]`, including without text
  (`text` empty): this is input, and the agent receives them (§8.1, §12) **[decision]** (T16). An
  `attachment` event is not delivered to the agent — it is a file shown in the feed (`hqc_attach`,
  §7); the client does not write `attachment` events of its own.
- `*_preview` fields are cut by the worker to 4 KB; the full tool result is not sent to the hub,
  except diffs and results saved as artifacts.
- `agent.delta` goes to clients over WebSocket and is not saved; `agent.message` is saved.
- History is never rewritten. Protection against forged peer-message headers is applied
  on delivery to the runtime (§7.1).

## 5. Worker ↔ Hub protocol

Transport: one persistent WebSocket connection from the worker to the hub, JSON frames,
20 s heartbeat, reconnection with exponential backoff.

Every frame: `{id, type, payload}`. The response to a request frame:
`{id, type: "ack"|"result"|"error", payload}` with the same `id` (to `hello` — `hello.ack`
or `error`). Fields with `?` — as in §4.

- `ack` {event_id?, seq?}: to an `event` — `{event_id, seq}` (§5.2), to a `session.input` —
  `{event_id}` of the delivered input, otherwise an empty `{}`.
- `error` {code, message, retryable, details?}. The initial list of `code`:
  `protocol_version_unsupported` (`details: {min_version}`), `unauthorized`,
  `unknown_session`, `invalid_frame`, `agent_start_failed`, `internal`, `locked_vscode`
  (the worker's answer to a `session.input` of a session it has released, §9.3), `mirror_quota`
  (the hub's answer to a `mirror.event` of a new transcript over the daily limit, §9.1, §20).
  Codes are strings, the list is open; an unknown code is handled as `internal`.

### 5.1 Enrollment and version
The hub URL on the worker (`hub_url` in `worker.toml` and the `enroll` argument) is `https://`
only; `http://` is allowed for loopback hosts (`localhost`, `127.0.0.0/8`, `[::1]`) or with an
explicit `insecure_http = true` in `worker.toml` (for `enroll` — the `--insecure-http` flag);
otherwise the worker does not start: the token and `poll_token` would travel in plain text. The
HTTP client of `enroll` does not follow redirects (the body with `poll_token` does not go to
another address).

`hqc-worker enroll <hub-url>` prints a code and a ready link
`<hub-url>/#/enroll?code=XXXX-XXXX` (the client's confirmation screen with the code filled in),
the user confirms the code in the web UI under their own login, and the worker gets a long-lived
token. Token storage: on macOS — Keychain (generic password, service `hqc-worker`,
account = the hub URL); on other OSes in the first version — a file `$HQC_HOME/token` with mode
0600 (directory 0700, written through a temporary file and rename); libsecret and DPAPI — later.
The token is revoked from the UI. In `hello` the worker sends
`protocol_version`; the hub answers `hello.ack {protocol_version, worker_id, server_time,
system_session_id?}` (the worker's service session, §10.2)
or `error {code: protocol_version_unsupported, details: {min_version}}`. The hub checks the
version before strictly parsing `hello`: a `hello` of another version may have another shape,
but must still get `protocol_version_unsupported`, not `invalid_frame`. On
incompatibility the worker writes "update required" to the log and the tray, retries
once an hour and does nothing else; updating is manual or via `hqc-worker update`.

Enrollment over HTTP (route `/worker/*`, without Access, rate-limited; the bodies are
`hqc_proto::api`, examples in `examples/wire/api.enroll*.json`):
- `POST /worker/enroll {name, os, protocol_version, machine_id?}` → `{code, poll_token,
  expires_at, poll_interval_s}`; `protocol_version` is informational (`hello` decides), `machine_id` is
  the machine fingerprint (below, since 0.23), the hub ignores unknown body fields; `code` is 8 characters `XXXX-XXXX` without look-alike letters and lives 10 minutes;
  `poll_token` is the worker's secret, sent only in the body (not in the URL), only its hash in the database.
  With the code the hub stores `CF-IPCountry` and the request address (limits, §20).
- The user enters the code in the web UI. First `GET /api/workers/enroll/{code}` →
  `{name, os, created_at, country?, replaceable?}` (404 — no such live code): the client
  shows the name, OS, time and country, and the confirm button becomes active only after they
  have loaded — someone else could have sent the link with the code. Then `POST
  /api/workers/enroll/confirm {code, name?, replace_worker_id?}` → `{worker_id, name,
  replaced?}`; case and the hyphen in the code do not matter. Code lookups (both requests): at
  most 30 per minute per user.
- The worker polls `POST /worker/enroll/poll {poll_token}` → `{status: pending}`,
  `{status: confirmed, worker_id, token, replaced?}` (the token is issued exactly once, only its
  hash in the database) or `{status: expired}`.
- `/worker/ws` — header `Authorization: Bearer <token>`; an unknown or revoked
  token gets 401 before the upgrade; a revocation between the upgrade and `hello` gets
  `error {unauthorized}` to `hello` and the connection closed. `DELETE /api/workers/{id}` revokes
  the token, drops the connection and archives the worker's service session with
  `system worker_revoked` (§10.2).

**Enrollment with replacement** (since 0.21). A repeated `enroll` of the same machine (a new
binary, a different token store, a revoked token) would otherwise get a new `worker_id`, while
the sessions, workspaces and service session stayed on the old one, offline forever. So
`replaceable[]` in the response of `GET /api/workers/enroll/{code}` lists the user's workers that
are currently not connected or are revoked: `{id, name, os, status: offline|revoked, last_seen?,
sessions_count}` (`sessions_count` — unarchived sessions on the worker, the service session
excluded); empty — the field is absent. The client offers "the same machine, replace
<name>" (the default is a new worker). With `replace_worker_id`, on confirmation the hub:
- checks that the worker belongs to the user (otherwise `400`) and that it is not connected (`409
  worker_online`; a revoked worker does not count as connected); while another code that is
  confirmed but not yet collected by polling already replaces this worker — `409 conflict`. A
  refusal does not use up the code;
- creates no new record: the code gets this `worker_id`; the old token stops working
  immediately (`token_hash` is reset, a remaining connection is dropped), `name` (from `name?` or
  the one sent by the worker) and `os` come from the new enrollment, `revoked_at` is cleared,
  `boot_id` is reset (the new process's first `hello` is a restart, §5.2);
- the service session stays (it is unarchived and gets the new name as its title), and
  `system {code: worker_replaced, old_name, new_name}` is written to it; sessions and workspaces
  do not change;
- `replaced` in the response is the worker's previous name. The new token is written to this
  `worker_id` on polling; the poll response carries the same `replaced`, and `hqc-worker enroll`
  prints "Enrolled as worker <id> (replaced <name>)".

Confirming a replacement is the same owner decision as an ordinary enrollment: a worker
that replaced another through someone else's link would get the replaced worker's sessions.
So the client shows the replacement as an explicit choice with the name and the number of
sessions, not as a checkbox ticked by default.

**Machine fingerprint** (since 0.23, T21). The worker sends `machine_id` in `POST /worker/enroll`
and in every `hello` (the field is optional; old workers do not have it): a stable machine
identifier — macOS `IOPlatformUUID` (`ioreg`), Linux `/etc/machine-id`, Windows `MachineGuid` from
the registry, otherwise a random one saved in `$HQC_HOME/machine_id` — hashed with sha256 salted
with `hqc` (64 hex digits); the identifier itself never leaves the machine. The hub stores it
with the enrollment code and on the worker (`machine_hash`; a value of the wrong shape is
ignored); existing workers get it with the first `hello` after the update, a replacement gets it
from the new enrollment. In `replaceable[]` workers with the same `machine_hash` as the code come
first and are marked `same_machine: true` (the field is sent only as `true`); the client
preselects replacing such a worker, "New worker" stays a choice.
The fingerprint is a hint, not a check: it does not prevent replacing another worker and does
not replace the owner's confirmation.

### 5.2 Delivery guarantees
Worker → hub:
- The worker keeps an **outbox** on disk: an event is first written to the outbox, then sent.
  The outbox carries `event`, `mirror.event`, holder reports (`session.state` with a nil
  `session_id`, §5.4, §9.3) and `mcp.call` (stored without `session_token`, see "MCP calls"
  below).
- The hub answers `ack {event_id, seq}`; until the ack the worker resends after reconnecting.
- Duplicates are dropped by `event_id` (unique index) and get an `ack` with the `seq` of the
  already stored event. Events for sessions the hub does not know (deleted) get an
  `ack` with `seq: null` (the field may also be absent) and are dropped.
- Worker ↔ hub frames are strict: the version is agreed in `hello`; a frame of an unknown `type`
  or with an invalid `payload` gets `error {code: invalid_frame}`. Tolerance of the
  unknown applies only inside events (§4): the hub stores an event of an unknown
  `type` as is.
- The hub stores an event's `payload` exactly as received (`jsonb` as is) and serves
  clients the same raw JSON; typed parsing is only for the hub's logic, nothing is serialized
  back into storage from the types, so values from a newer
  worker are not lost. The exception is U+0000, which Postgres accepts neither in `jsonb`
  nor in `text`: the hub replaces it with U+FFFD in all strings and keys of an incoming worker
  frame before parsing, as well as in the events it writes itself (client input, answers)
  and in the session `title`. The worker does the same replacement at the source: the Claude
  Code adapter in all event texts (`result_preview`, `error_text`, `args_preview`, `description`,
  `agent.message`, `agent.delta`, `agent.thinking`, `clarify.request` questions).
- An event the database rejected because of the data itself (an encoding error, a value out of
  range, a field too long: SQLSTATE classes 22, 23, 54) gets
  `error {code: invalid_frame, retryable: false}` and the worker discards it; a database
  connection error and anything else gets `error {code: internal, retryable: true}`.
- An `error` with `retryable: true` to an `event` leaves the event in the outbox until the next
  connection; the worker counts such answers per event (a counter in the outbox) and after
  **10** writes an `error` to the log and deletes the event so that it is not resent forever.
- `seq` is assigned by the hub; clients see only events with a `seq`.
- `agent.delta` is not written to the outbox and is lost on a disconnect; that is acceptable. It
  goes in the same `event` frame; the hub answers `ack {event_id}` without `seq` and does not
  store it.
- The `ack` to `session.input` and `agent.delta` bypass the outbox, while the turn's events go
  through it, so the hub may receive the input's `ack` and the deltas later than the
  `tool.call`/`approval.request` of the same turn. The hub does not rely on the order between
  them (verified on the T4 adapter): an input `ack` that arrives after an `agent.message` with
  `final: true` and that input's `in_reply_to` does not reopen the turn. A turn that ended
  without final text (only `status idle|error`) stays "in progress" until the next turn when
  its `ack` is late like this **[verify]**.
- An event for a session of another worker is handled as for an unknown one: `ack` without `seq`.

Hub → worker:
- The hub stores `user.message` and `peer.message` with `delivery: pending` and sends them as
  `session.input`. The worker answers `ack {event_id}` after handing the input to the runtime
  (Claude Code — after the `--replay-user-messages` echo; ACP — after sending `session/prompt`).
  After `hello` the hub sends `session.start` with `mode: resume` to every session of this worker
  that has a `pending` input, an open `approval`/`clarify` or an answer to a request not yet
  acknowledged by the worker; after the `ack` to it — the unacknowledged answers, then the
  `pending` inputs in `seq` order. To an input for a session that has had no `session.start` in
  its process, the worker answers `error {code: unknown_session, retryable: true}`.
- If the agent process fails to start (no binary, profile not configured), the worker
  answers `error {retryable: true}`; after **3 failed starts** in a row the hub sets the
  input's `delivery: failed`, writes a `system` event with the reason, and the session goes to
  `error`; the "retry" button resets the counter and returns the input to `pending`.
- On every connection the hub sends `session.start` before a session's first `session.input`
  and sends inputs only after the `ack` to it; `session.state {process_state: stopped}`
  resets this, and the next input again begins with `session.start`. `mode` in
  `session.start` (§5.3): `new` if the hub has never sent `session.start` to this
  session, otherwise `resume`. The worker acknowledges `session.start` for a live process with
  `ack` without touching the process. An `error` answer
  to `session.input` with `retryable: true` or the code `agent_start_failed` counts as a
  failed start; any other error marks that input `failed` (`system` with the code
  `input_rejected`). A failed start is retried after a pause while there are `pending`
  inputs. After the third in a row all `pending` inputs of the session become `failed`,
  and a `system` with the code `agent_start_failed` is written. A new input after that gets one
  start attempt; "retry" (`POST /retry`) resets the counter.
- The worker's input queue (§17.2) is in memory; the source of truth is the `pending` events
  in the hub, so a worker restart does not lose them.
- The hub detects a worker restart by the `boot_id` in `hello` (§5.4): the worker generates it
  when the process starts, the hub keeps the last one. A new `boot_id` means a restart (§17.3):
  open `approval`/`clarify` requests of the worker's sessions are cancelled with
  `worker_restart` (an event only: no process holds the request any more, no `deny` frame is
  sent), answers the worker has not acknowledged are dropped, an unfinished turn gets a `system`
  with the code `turn_lost`, and the sessions' `process_state` becomes `stopped`. The same
  `boot_id` means a dropped connection: the hub cancels nothing, the process and its requests
  are alive **[decision]**.
- The worker keeps the mirror offsets for each jsonl file and the "file → cwd" cache on disk.

MCP calls (worker → hub → worker):
- `mcp.call {request_id, session_token, tool, args, turn_input?}` → `mcp.result {request_id,
  result | error}`. `request_id` is a uuid the worker makes up per call (since 0.39 the hub
  refuses one that is not 1–128 characters of `A-Z a-z 0-9 . _ : -` with `invalid_args`: it
  reaches another agent's input in the header of an ask, §7.1). `turn_input` (since 0.39) is
  the `event_id` of the input whose turn made the call, as the worker handed it to the runtime;
  the hub takes the call's chain from it (§7.1). Fast tools (`attach`,
  `list`, `search`, `schedule`, `send`, `spawn`) are answered at once: `mcp.result` comes as
  the reply to the call frame, under its `id`. `ask` is deferred: the hub answers the call
  frame with `ack {}`, creates a `PendingRequest`
  with `worker_id` and sends `mcp.result` as a frame of its own only when the answer has arrived or the timeout has
  expired. If the worker is offline at that moment, the result waits in `PendingRequest.result`
  and is sent after `hello`; the worker answers such a frame with `ack`, also when it no
  longer has the blocking call (it restarted) and drops it.
- Since 0.33 (T30): `mcp.call` goes through the outbox keyed by its `request_id`, but the
  outbox stores it **without** `session_token`: the token is never written to disk (§7.2).
  The connection adds the token when it sends the frame and drops instead of sending a call
  that is no longer open: answered, cancelled, or left by a previous run of the worker, whose
  caller died with it. So a call survives a dropped connection (it is sent again after the
  next `hello`) but not a worker restart. A call sent again after its answer was lost runs
  again on the hub, so a tool with side effects acts once per `request_id` (since 0.34, T31;
  0.35, T32): what it writes gets the id `uuid v5(<hqc namespace>, "<tool>\n<calling session id>\n<request_id>")`
  (`once_id` in `crates/hqc-hub/src/mcp.rs`), and a call that finds it already written
  answers from it — the same result, nothing written twice. `hqc_attach` keys its
  `attachment` event so, `hqc_send`/`hqc_ask` their `peer.message` (with `peer.message` as
  the tool), `hqc_schedule` its schedule (since 0.38); a repeated `hqc_send` answers the same `{to, delivery}` from that event, a
  repeated `hqc_ask` finds its `PendingRequest`: still open — `ack {}` again, closed — its
  stored `mcp.result` under the repeat's frame `id`. `hqc_spawn` (since 0.37) keys the new
  session's id (`hqc_spawn` as the tool) and its first `peer.message` (as `hqc_send`): a
  repeat finds the session, stores the message if the first call did not get that far, and
  answers the same `{address}`. The same `request_id` from another
  session is another call.
- A deferred answer (since 0.35): the hub keeps the `mcp.result` frame in
  `PendingRequest.result` and sends it to the asking session's worker, now if it is
  connected, else after its next `hello` (no `session.start` is needed: the connection routes
  `mcp.result` itself), until the worker acks it. A worker restart (a new `boot_id`) closes
  the worker's open asks with `error {code: worker_restart}` and drops their unsent answers:
  the blocking calls died with the old process.
- The token (since 0.33). The hub issues a new token with every `session.start`, also for an
  agent the worker keeps running (after a reconnect), and keeps the hashes of the latest and
  the previous one (`sessions.session_token_hash`, `prev_session_token_hash`, migration
  `0014`). The worker sends the latest token it got; a call already on its way when the next
  `session.start` arrives still carries the previous one, which the hub accepts. The hub
  authenticates `mcp.call` by these two hashes **and** the worker the session belongs to; an
  unknown token, a token of another worker's session and a token of the wrong shape all get
  `mcp.result` with `error {code: unauthorized}`, never a frame `error`. The worker's
  service session has no token.
- Errors of a tool have one shape, `error {code, message}`; `code` ∈ `unauthorized`,
  `unknown_tool`, `invalid_args`, `not_found`, `unavailable` (the worker lost the call: the
  agent stopped, the hub refused the frame), `internal`; the list is open. `args` is at most
  64 KB serialized (§20), else `invalid_args`. Since 0.39 (T36): calls over the caps of §20
  get `limit_exceeded` (the worker's calls per minute are counted before the token is looked
  up), and a call of an archived session `unavailable`.
- `mcp.cancel {request_id, reason}` → `ack {}` — the worker reports that a call died on its
  side (§7.2), `reason` ∈ `cancelled` (`notifications/cancelled`), `eof` (the MCP process
  exited with the call open). It is sent directly, not through the outbox; a call the hub has
  not received yet leaves the outbox instead. For a deferred call the hub closes the `PendingRequest` (without a result: nobody waits for one) and writes a
  `system` event with the code `ask_cancelled` to the sending session before it acks; it sends nothing to the
  target session, whose turn continues. For a fast call it only acks (T30).

### 5.3 Hub → worker frames
- `session.start` {session_id, mode, runtime, acp_agent?, model?, workspace_path | null,
  cwd, environment, runtime_session_id?, system_prompt?, disallowed_tools[], restricted,
  auto_approve, session_token, owner_is_worker_owner, ask_timeout_s, requested_by_owner?}
  — `mode`: `new` | `resume` (§5.2); `requested_by_owner` (sent only as `true`) — the worker
  owner added the session's workspace: no confirmation of a new folder is needed (§10.2); the worker calls the adapter's `start` or `resume` (§8);
  on `new` the Claude Code adapter still checks for a jsonl with this uuid and, if there is one,
  continues it with `--resume` (§8.1); `owner_is_worker_owner` is needed by the worker for `allow_foreign_sessions`, the choice
  of a Hermes profile and admitting `isolation: none`; `ask_timeout_s` — to set the
  runtime's MCP tool timeout (§7.1); without `model` the worker uses the runtime's default
  model. `workspace_path` is the `path` from the session's workspace record; the worker accepts
  it only if the path, with symlinks resolved, is a directory inside `allowed_roots`, and
  checks `cwd` against it (§8.1), otherwise `error {invalid_frame}`.
- `session.input` {session_id, event_id, text, artifact_ids[],
  peer_prefix[{from: "<project-slug>/<session-slug>", text, ts}]} —
  the worker downloads the artifacts via `GET /worker/artifacts/{id}` with its token
  (no TTL) into `<workspace>/<cwd>/.hqc/inbox/<event_id>/` and substitutes the paths; the
  `.hqc/` folder is added to the workspace's `.git/info/exclude`. `peer_prefix` — accumulated
  `send` messages (§7.1). `peer?` {from, mode, request_id?} (since 0.35) — only for a
  `peer.message`: the sender's address, `send`/`ask` and the ask's `request_id`, from which
  the worker writes the header (§7.1); `text` is the message's text without it.
- `session.stop` {session_id} — interrupt the current turn.
- `session.steer` {session_id, text} — intervene in a turn without interrupting it, if the
  runtime supports it (Hermes: `/steer`, §8.2.1); otherwise `error`.
- `session.kill` {session_id} — terminate the agent process.
- `session.cleanup` {session_id, workspace_path | null, cwd, event_ids[]} → `ack` (since 0.23) —
  after `session.kill` when a session or its project is archived or deleted: remove the inbox
  folders of the inputs `event_ids` (for a session without a workspace — its scratch folder);
  path checks and details are in §17.4.
- `title.request` {request_id, session_id, user_text, agent_text} → `result {request_id, title}`
  or `error` (since 0.25, T24) — the session title (§3 Session): `user_text` — the first
  `user.message`, `agent_text` — the first final `agent.message`, each cut by the hub to 2000
  characters. The worker runs Claude Code once outside the session and its process (§8.1), at most
  2 runs at a time: beyond that — `error {internal, retryable: true}` at once; a failed run or a
  25 s timeout — `error {internal}`. `title` — one line of up to 60 characters without quotes.
  `request_id` equals the frame's `id`; the hub waits 30 s for the answer.
- `approval.response` {session_id, request_id, option_id, message?} (`message` — an answer
  as text with a deny option, §10.1),
  `clarify.response` {session_id, request_id, answers}; there is no `by` field in the frames, it
  is only in the events. There is no `approval.cancel` frame: when cancelling a request itself
  (timeout, `stop`, archiving), the hub writes an `approval.cancel` event and sends the worker
  an `approval.response` with the `option_id` of the `canonical: deny` option, so that the
  adapter closes the request in the runtime. Likewise for `clarify`: the hub writes a
  `clarify.cancel` event and sends `clarify.response` with `answers: {}` — an empty object means
  cancellation; the Claude Code adapter answers the `can_use_tool` for `AskUserQuestion` with
  the `deny` behavior, and the model continues the turn without answers.
- `fs.stat` {request_id, path} → `result {request_id, exists, is_dir, canonical?,
  inside_allowed_roots}` — checking a folder before creating a workspace (§13.1). `canonical` —
  the path with symlinks resolved, present if the path exists; `inside_allowed_roots` is computed
  from it (for a non-existent path — from the nearest existing ancestor). Outside `allowed_roots`
  the worker reports nothing about the path: `{exists: false, is_dir: false, inside_allowed_roots:
  false}`. A relative path gets `error {invalid_frame}`.
- `fs.list_dirs` {request_id, path | null} → `result {request_id, entries[{name, path}]}`
  — the folder tree for picking a workspace in the client: with `path: null` — `allowed_roots`,
  otherwise the subdirectories of `path` (directories only, no hidden ones, symlinks only to
  directories inside `allowed_roots`, sorted by name case-insensitively, at most 500). A path
  outside `allowed_roots` (after resolving symlinks) gets `error {invalid_frame}`.
  For both frames `request_id` equals the frame's `id`; the answer is `result` or `error` with
  the same `id`. The hub waits 10 s for the answer (`504 worker_timeout` in REST, §6).
- `workspace.create` {request_id, kind: clone|worktree|dir, path, origin_url?,
  origin_path?, branch?, requested_by_owner} → `result {ok, path, error?}`
- `workspace.delete` {request_id, path, requested_by_owner} → `result` (§13.2)
- `workspace.list_candidates` {request_id} → `result {request_id, candidates[{path,
  sessions_count, last_active}]}` — folders with Claude Code transcripts (§9.1): `path` is the
  records' `cwd`, only inside `allowed_roots`, sorted by path; `last_active` is the modification
  time of the newest file. `request_id` equals the frame's `id`; the hub waits 10 s, as for
  `fs.*`. A worker without a mirror answers `error {invalid_frame}`.
- `mirror.set` {workspace_path, enabled, sessions?[{runtime_session_id, since_ts}]} → `ack` —
  start or stop watching the folder's transcripts (§9.2). The hub sends it after every `hello`
  for all of the worker's workspaces with `mirror_enabled`, when such a workspace is created, on
  `PATCH /api/workspaces/{id} {mirror_enabled}` (§6), and with `enabled: false` when one is
  deleted or switched off. `sessions` (since 0.23, only with `enabled: true`) — the hub's sessions
  in the folder and the time of their last event: the first pass over these transcripts does not
  send what the hub already has (§9.1). With `enabled: true` the worker accepts only an existing
  folder inside `allowed_roots` (after resolving symlinks), otherwise `error {invalid_frame}`; it
  keeps the list in memory.
- `model.proxy` {request_id, model, body} → a stream of `model.proxy.chunk {request_id, data}`
  and `model.proxy.end {request_id, error?}`
- `worker.approval.response` {request_id, option_id} → `ack` — the worker owner's answer to a
  `worker.approval.request`, or `deny` when the hub cancels it (expiry); the worker answers `ack`
  even when it no longer has the request. The hub resends an unacknowledged answer after `hello`
  (without `session.start`, §10.2).
- `mcp.result` {request_id, result | error} — `error` is `{code, message}` (§5.2); the
  reply to `mcp.call` under its `id`, or, for a deferred call, a frame of its own that the
  worker acks (since 0.33, T30).

### 5.4 Worker → hub frames
- `hello` {protocol_version, boot_id, capabilities, environments[], env_bindings[],
  allowed_roots[], allow_foreign_sessions, share_local_models, local_models[], machine_id?}
  — `boot_id`: a uuid, new on every start of the worker process, the same in every `hello`
  of that process (§5.2, §17.3); `machine_id` — the machine fingerprint (since 0.23, §5.1). `capabilities` — `{claude_code?: {version,
  sessions_registry?}, acp_agents[]}`; `sessions_registry: true` — the worker reads the Claude
  Code live sessions registry and reports holders (§9.3).
  The worker does not report workspaces (since 0.16; the hub no longer reads the iteration 1
  field `capabilities.workspace_path`): workspaces are created in the hub (§3, §6) and checked by
  the worker against `allowed_roots`.
- `event` {event} → `ack` (`session_id` is inside the event)
- `session.state` {session_id, process_state, runtime_session_id?, locked_by, title?, held_by?, commands?}
  — `title`: the title the runtime gave the session (§8.1); the worker repeats the last one in
  every following `session.state`, the hub applies it by the `title_source` rule (§3 Session),
  folding it to one line of up to 200 characters. `held_by` — the transcript's holder (§9.3): no
  field — the worker has no registry data, `null` — nobody, `{entrypoint, status}` — a live foreign
  process.
  **Holder report** — a `session.state` with a nil `session_id`, the transcript's
  `runtime_session_id` and `held_by` (`process_state: stopped`, `locked_by: none` — the hub does
  not read them): the hub looks the session up by worker and `runtime_session_id` (none — only
  `ack`) and applies `held_by` per §9.3. Reports go through the outbox in the same order as
  `mirror.event`; the answer is `ack {}`.
  `commands` (since 0.28, T27) — the slash commands the runtime offers, names without `/`
  (Claude Code: from `system/init`, §8.1). Absent — not known yet; like `title`, the worker
  repeats the last list in every following `session.state`. The hub stores it on the session
  (only when it changed) and shows it in the session object (§6): names are trimmed, a
  leading `/` dropped, names with whitespace or control characters or longer than 100
  characters dropped, duplicates removed, at most 500 kept.
- `mirror.event` {workspace_path, runtime_session_id, cwd, event, title?, recent, held_by?} — events
  from the mirror (§9.1, §9.2), through the outbox; the answer is `ack {event_id, seq}` as for
  `event` (a duplicate — the `seq` of the stored one; a folder not mirrored by this worker, a `cwd`
  outside it and the other refusals of §20 — without `seq`); a record of a new transcript over the
  daily limit gets `error mirror_quota`, `retryable: true` (since 0.24, §9.1).
  `workspace_path` — the path from `mirror.set`, `cwd` — the absolute path from the record,
  `title` — the file's last `ai-title`, `recent` — the file was modified less than 30 s before the
  worker read the record, `held_by` — the transcript's holder at the time of reading, as in
  `session.state`. The hub looks the session up by worker and `runtime_session_id` and creates it
  for an unknown one; it ignores `event.session_id` (the worker sets the nil uuid) and `author.id`
  and fills them in itself (for `system` — `{kind: system}`).
- `worker.approval.request` {request_id, kind, description, expires_at} → `ack {}` — an owner
  confirmation outside a session (`kind`: `workspace.use`, later `workspace.create`,
  `workspace.delete`; the list is open). The hub writes an `approval.request` to the worker's
  service session; a repeat with the same `request_id` after a reconnect writes no new event
  (§10.2).
- `mcp.call` {request_id, session_token, tool, args} → `mcp.result` (fast tools) or `ack {}`
  (deferred, §5.2); through the outbox without the token (since 0.33, T30)
- `mcp.cancel` {request_id, reason} → `ack {}` — a call that died on the worker's side
  (§5.2, §7.2)
- `result` {request_id, ...} / `error` to the hub's requests (`fs.stat`, `fs.list_dirs`,
  `workspace.list_candidates`, `title.request`, §5.3), with the request's `id`.
- Artifact upload: HTTP PUT `/worker/artifacts` with the worker's token, not over WebSocket.
  Artifact limit **100 MB**, no chunking. Since 0.34 (T31), for `hqc_attach` (§7, §12): the
  body is the file, streamed, with `Content-Length`; `x-hqc-session-id` — the session it is
  for, which must be one the hub has on this worker (else `404`; the service session too);
  `x-hqc-artifact-name` — the name, percent-encoded as in the download; `x-hqc-artifact-kind?`
  and `Content-Type` as in `POST /api/artifacts`. The answer is the artifact (§6), `201`, or
  `200` for the same content under the same name. A bad token `401`, over the limit `413
  too_large`, an unknown `kind` `400`.

## 6. Client ↔ Hub protocol

Authentication: the hub verifies the signature of the `Cf-Access-Jwt-Assertion` JWT against
Cloudflare's JWKS (issuer and audience from the config) and takes the email from the token. An
email header without a JWT is not accepted. The worker route `/worker/*` is excluded from Access
and accepts only worker tokens; the human route `/api/*` accepts only Access JWTs. The hub
listens only on the tunnel interface.

Dev mode (local development without Access): `HQC_DEV=1` together with `HQC_DEV_USER=<email>`;
all `/api/*` requests come from that user. The hub refuses to start if `HQC_ACCESS_*` is also
set or `HQC_HUB_LISTEN` is not loopback (`127.0.0.0/8`, `[::1]`, `localhost`); on start it
logs a loud warning (§20).

REST bodies and `/api/ws` frames are typed in `hqc-proto` (`api`, `client_frame`); the
contract examples are `examples/wire/api.*.json` and `client.*.json`; the hub and client tests
read them. Unknown fields are ignored in both directions.

HTTP API (iteration 1 and projects with workspaces from iteration 2; the list is not complete,
the rest is marked "later"):

- `GET /api/me` → `{id, email, display_name, timezone, personal_project_id}`; `PATCH
  /api/me` {timezone?} (since 0.38, `api.me.patch.request.json`; an unknown zone: `400`) → the
  same object; later keys
- `GET /api/projects` → `{projects[]}`, `POST /api/projects` {name, description?} → `201`
  and the project, `GET/PATCH/DELETE /api/projects/{id}` (`PATCH` {name?, description?,
  default_workspace_id?} — a workspace of this project, otherwise `400`; the personal project:
  `409 personal_project` on rename and delete; `DELETE` → `204`, §3 Project).
  The project object is `{id, owner_id, name, slug, description, default_workspace_id?,
  is_personal, created_at, updated_at}` (`api.project.json`)
- `GET /api/workspaces?project_id=` → `{workspaces[]}` (without `project_id`: all of the
  user's workspaces); `POST /api/workspaces` {project_id, worker_id, path, label?,
  mirror_enabled?} → `201` and the workspace (`kind: dir`, §13.1): the hub sends the worker
  `fs.stat` and stores `canonical`; `mirror_enabled` (since 0.23 defaults to `true`; `false`
  means no mirror) controls the mirror and the import from Claude Code
  (`CreateWorkspaceImport`, §9.1): the hub sends the worker `mirror.set`, and when such a
  workspace is deleted, `mirror.set` with `enabled: false`. Worker not connected: `409
  worker_offline`, the workspace is not created; no reply within 10 s: `504
  worker_timeout`; path outside `allowed_roots`: `400 outside_allowed_roots`; no such path:
  `400 path_not_found`; not a directory: `400 not_a_directory`; the worker rejected the frame:
  `400 invalid_path`; the path equals a workspace of the same worker, lies inside one or
  contains one: `409 workspace_conflict` with body `{error, message, existing_workspace_id,
  existing_project_id, existing_path}` (`api.workspace.conflict.json`; the client says "this
  folder is already in project <name>" with a link). `PATCH /api/workspaces/{id}` {label?,
  mirror_enabled?} → workspace (`api.workspace.patch*.request.json`): changing `mirror_enabled`
  is the "VS Code sessions" switch (since 0.23, §9.2); the hub sends the worker `mirror.set`
  (at once if connected, otherwise after `hello`); the same value sends nothing. `DELETE
  /api/workspaces/{id}` → `204`: only the record, the folder is left alone
  (§13.2); while it has non-archived sessions: `409 workspace_in_use`; archived ones lose
  `workspace_id` and cannot be unarchived (`409 no_workspace`). The workspace object is `{id,
  project_id, worker_id, path, label, environment, kind, ephemeral, created_by_hqc,
  mirror_enabled, created_at, updated_at}` (`api.workspace.json`)
- `GET /api/workers/{id}/dirs?path=` → `{entries[{name, path}]}`: an `fs.list_dirs` frame to
  the worker (§5.3); without `path`: `allowed_roots`; outside them: `400 invalid_path`;
  offline: `409 worker_offline`
- `GET/POST /api/projects/{id}/sessions` (list: `{sessions: [<session>]}`; create:
  {title?, workspace_id?, worker_id?, cwd?, model?, auto_approve?} → `201` and the session
  object, `api.session.request.json`), `GET/PATCH/DELETE /api/sessions/{id}` (§17.4)
- `GET /api/sessions/{id}/events?after_seq=&limit=` — pagination by `seq`, response
  `{events[], last_seq}`
- `POST /api/sessions/{id}/input` {text, artifact_ids[], client_op_id} → `201 {event}`:
  the stored `user.message` with `client_op_id` and `delivery: pending`; a repeat with the same
  `client_op_id` (deduplicated by `(session_id, client_op_id)`) → `200 {event}` with the event
  stored the first time
- `POST /api/sessions/{id}/stop`, `/steer`, `/takeover`, `/archive`, `/unarchive`, `/retry`
  (§5.2). `/takeover` (§9.3) continues in hqc a session that VS Code was driving: `locked_by:
  hqc`, `system session_taken_over`, `session.start` with `mode: resume` to the worker; the
  response is the session object; with `locked_by: hqc` it does nothing. A live foreign process
  holds the transcript: `409 vscode_open` with `held_by`; no registry data and VS Code wrote
  less than 30 s ago: `409 vscode_active`; worker offline: `409 worker_offline`; archived:
  `409 archived`; service session: `409 system_session`
- `POST /api/sessions/{id}/requests/{request_id}` {option_id, message? | answers} → `{event_id, seq}`
  of the stored answer; closed request: `409 request_closed` (§10.1). The same call answers a
  request wherever the client shows it (its own feed, the feed of a session waiting on it, the
  "Requests" list, §10.1): `{id}` is always the request's own session. `message` is a text reply
  instead of the call (§10.1): only for an `approval` in a session and only with an option of
  `canonical: deny`/`deny_always`, non-empty, up to 16 KiB, otherwise `400`
- `POST /api/artifacts` (multipart: field `file`, optional `kind`) → `201` and the artifact
  object `{id, kind, name, mime, size, sha256, created_at}` (`api.artifact.json`); the same
  file under the same name again → `200` and the earlier object; over 100 MB: `413
  too_large`; unknown `kind` or no `file`: `400`. Without the field, `kind` is derived from the
  type and extension (§12). `GET /api/artifacts/{id}` → the artifact object; `GET
  /api/artifacts/{id}/url` → `{url, expires_at}` (`api.artifact.url.json`), a signed link
  valid for 10 minutes (§12). Someone else's or a nonexistent artifact: `404`. An input (`POST
  .../input`) with `artifact_ids` of someone else's or nonexistent artifacts: `400
  unknown_artifact`; more than 32: `400`. Download by link: `GET /artifacts/{id}?exp=&sig=`
  (outside `/api`, §12); for the worker: `GET /worker/artifacts/{id}` (§5.3)
- `GET /api/requests` → `{requests[]}` (`api.requests.json`, since 0.41, T37): every open
  request (`approval`, `clarify`, `worker_approval`) in the user's sessions that are not
  archived, oldest first, the workers' service sessions included. An item is `{session_id,
  request_id, kind, address, title, worker_id?, worker_name?, system?, event, waiting[]}`:
  `address` (`<project-slug>/<session-slug>`), `title` and the worker of the request's session,
  `system: true` for a worker's own request (§10.2), `event` — the `approval.request` /
  `clarify.request` as stored (§4), `waiting` — `{session_id, address, title}` of the user's
  sessions that wait on this one through open `hqc_ask`s (§7.1), directly or along a chain,
  the nearest first, so the last is the top of the chain (absent when nobody waits). Only the
  user's own sessions and asks count: another user's request, or another user's ask aimed at
  the user's session, is never listed. The same list comes over `/api/ws` as `requests` (below)
- `PUT /api/sessions/{id}/read_cursor` {last_read_seq}
- `GET /api/workers` → `{workers[]}`: workers that are not revoked; `?include_revoked=1` adds
  the revoked ones (the "Settings" screen). The worker object is `{id, name, os, status: online
  | offline | revoked, revoked_at?, last_seen?, system_session_id, ...}` (§3 Worker).
  `GET /api/workers/enroll/{code}` → `{name, os, created_at, country?, replaceable?}`
  (`EnrollInfo`; `country` is `CF-IPCountry` of the `enroll` request; `replaceable`: §5.1, workers
  of the same machine first and with `same_machine: true`), `POST
  /api/workers/enroll/confirm` {code, name?, replace_worker_id?} → `{worker_id, name,
  replaced?}` (the client also opens the screen from the link `#/enroll?code=XXXX-XXXX`;
  replacement and `409 worker_online`: §5.1; examples `api.enroll.*.replace*.json`).
  `DELETE /api/workers/{id}` → `204`: revocation (§5.1), the service session is archived (§10.2);
  already revoked or someone else's: `404`.
  `DELETE /api/workers/{id}?purge=1` → `204`: deletes a **revoked** worker: the worker record,
  its service session (with events, as in §17.4) and its workspace records (folders are left
  alone, §13.2; a project's default workspace is replaced with the earliest remaining one). Not
  revoked: `409 worker_not_revoked`; while the worker has non-archived sessions (other than the
  service one): `409 worker_has_sessions` with body `{error, message, sessions_count}`; the client
  offers to move them by enrollment with replacement (§5.1) or to archive them. Archived sessions
  survive the deletion without `worker_id` and `workspace_id` (unarchiving: `409 no_workspace`).
  `GET /api/workers/{id}/candidates` → `{candidates[{path, sessions_count, last_active}]}`
  (`api.candidates.json`): a `workspace.list_candidates` frame to the worker (§5.3, §9.1); offline:
  `409 worker_offline`; no reply within 10 s: `504 worker_timeout`; a worker without a mirror
  refused: `400 invalid_path`
- `GET /api/search?q=&project=&fuzzy=&cursor=&limit=` → `{results[{session_id, address,
  title, archived, seq?, type?, ts, snippet}], next_cursor?}` (`api.search.json`, since 0.36,
  T33; §15): `q` 1–200 characters after trimming; `project` an id or slug of the user's
  project (another user's or an unknown one: `404`); `fuzzy=1` (or `true`) needs at least 3
  characters; `limit` 1–50, 20 by default; `next_cursor` is opaque, passed back as `cursor`
  for the next page, absent on the last one. A hit without `seq` is a title hit. Refused
  queries (empty, too long, too short for fuzzy, a foreign cursor, over the time limit): `400
  bad_request` with the reason in `message`. The client opens a hit at
  `#/s/<session>?seq=<seq>` (a title hit at `#/s/<session>`)
- `GET/POST/DELETE /api/grants`
- `POST /api/push/subscribe` (body is `PushSubscription.toJSON()`: `{endpoint,
  expirationTime?, keys: {p256dh, auth}}`), `DELETE /api/push/subscribe {endpoint}`,
  `GET /api/push/vapid` → `{public_key}` (base64url of the uncompressed P-256 point; `null` if
  push is not configured; the client then shows "notifications are off on the hub"). The push
  body itself: §16. `endpoint`: only `https://` on push service hosts (§20), otherwise `400`;
  at most 10 subscriptions per user (`409 push_limit`); someone else's `endpoint`: `409
  conflict`, not a takeover; a repeated request for one's own subscription updates it.
- `/api/schedules` (since 0.38, T35; §3 ScheduledMessage, §16): `GET
  /api/schedules?session_id=` → `{schedules[]}` (`api.schedules.json`; without `session_id`:
  all of the user's), the next due first, finished ones last; `POST /api/schedules`
  {session_id, cron? | at?, timezone?, message, catch_up?} → `201` and the schedule
  (`api.schedule.request.json`, `api.schedule.json`; the checks of §16 and §20 give `400`, an
  archived session `409 archived`, over the limit `409 schedule_limit`); `PATCH
  /api/schedules/{id}` {enabled?} → the schedule (pause and resume; a `cron` resumed runs next
  after now); `DELETE /api/schedules/{id}` → `204`. Another user's schedule or session: `404`.
  The schedule object is `{id, owner_id, session_id, cron?, at?, timezone, message, enabled,
  catch_up, next_run?, last_run?, created_at, updated_at}`. The client lists, pauses, resumes
  and deletes a session's schedules from the session's "More" menu ("Schedules")
- later: `/api/projects/{id}/template`

Session object (`GET /api/sessions/{id}`, list, create, `PATCH`, `archive`):
`{id, project_id, owner_id, workspace_id?, worker_id?, cwd, title, title_source, slug, agent: {runtime,
acp_agent?, model?, runtime_session_id?, process_state, system_prompt?,
disallowed_tools[], restricted, start_failures}, auto_approve, wake_on_send, locked_by,
last_seq, archived, system?, held_by?, state, queue_len, last_read_seq, unread, created_at,
updated_at, commands?, spawned_by?}`. `spawned_by` (since 0.37, T34) — `{session_id, address}`
of the session whose agent started this one with `hqc_spawn` (§7), absent otherwise or once
that session is deleted; the client shows "started by <address>" in the chat header as a
link to it. `held_by` (`{entrypoint, status}`): the session is currently held open
by Claude Code outside hqc (§9.3): `state: locked_vscode`, takeover gives `409 vscode_open`
until it is closed there.
`system: true`: the worker's service session (§10.2): input, `stop`, `retry`, `takeover`,
`archive` and `DELETE` give `409 system_session`, `PATCH auto_approve: true` gives `403`.
`state`, `queue_len` (§17.1), `last_read_seq` and `unread` are computed for the requesting
user.
`commands` (since 0.28, T27) — the session's slash commands as the worker last reported them
(`session.state.commands`, §5.4), absent until a process of the session has reported a list. It
is not in `session.summary`: the client fetches the session object when it opens an idle chat
without a list. When the composer text is `/` followed by a name so far, the client lists the
commands starting with that name (ignoring case): arrows move, Tab or Enter insert `/<name> `,
Esc closes, a click or tap inserts. No list — no popup; only commands from the list are offered.

WebSocket `/api/ws` (upgrade only with the hub's `Origin`, §20; messages up to 1 MiB): the client sends `subscribe {session_id, after_seq}` and receives
events with `seq > after_seq`, then the live stream; `agent.delta` has no `seq`. An event
stored from a client input carries its `client_op_id`, so the client shows the message at
once and matches it with the echo (a technique from Agent Host Protocol, §22). A separate
subscription, `subscribe_summary`, gives `state`, `queue_len` and unread counts per session,
and the user's open requests (`requests`, since 0.41).
After a reconnect the client repeats `subscribe` with the last `seq` it has seen, or from
before its first input still `pending` if that is earlier (since 0.40): a `delivery` frame sent
while the socket was down is lost, and an event sent again carries its current `delivery`,
which the client takes over the stored copy (the reply to `POST /input` never overrides a
newer copy).

`/api/ws` frames are flat JSON objects `{type, ...}` with no `id` and no `payload` wrapper:
- client → hub: `{type: subscribe, session_id, after_seq?}`, `{type: unsubscribe,
  session_id}`, `{type: subscribe_summary}`, `{type: ping}`;
- hub → client: `{type: event, event}`: the event as stored (§3), `agent.delta` without
  `seq`; `{type: subscribed, session_id, last_seq}`: the history after `after_seq` has been
  sent, the live stream follows; `{type: delivery, session_id, event_id, seq, delivery,
  delivered_at?}`: the delivery of an input changed; the event itself is not resent, the
  client updates `delivery`/`delivered_at` of its event with this `seq`;
  `{type: summary, sessions[]}`: the reply to `subscribe_summary`, all of the user's sessions;
  `{type: session.summary, session}`: one session changed; an element is `{session_id,
  project_id, title, state, queue_len, unread, last_seq, last_read_seq, held_by?}` (the same as
  in the session object); `{type: requests, requests[]}` (since 0.41, T37,
  `client.requests.json`): the list of `GET /api/requests`, sent right after `summary` and then
  again, whole, whenever one of the user's requests opens or closes (an event of §4
  `approval.*`/`clarify.*`, a deleted session) or an `hqc_ask` between the user's sessions opens
  or closes; no polling. `{type: pong}`; `{type: error, code, message, session_id?}`.
  The client ignores an unknown `type`.
- `unread` is the number of events with `seq > last_read_seq` among: `agent.message` with
  `final: true`, `approval.request`, `clarify.request`, `peer.message`, `system`, except
  `system` with code `worker_log` (worker logs in the service session, §10.2). Intermediate
  `agent.message` (`final: false`), `status`, `usage`, tool calls and inputs do not count as
  unread.

A session is created in a workspace of its project (§13.4) and on that workspace's worker:
`workspace_id` is that workspace (of another project, or not on `worker_id` if that is given:
`400`); without `workspace_id` but with `worker_id`: the project's workspace on that worker (the
default one if it is there, otherwise the earliest); without both: the project's
`default_workspace_id`. None: `409 no_workspace`. So a freshly enrolled worker (possibly via a
link someone sent, §5.1) gets no new sessions until the user adds a workspace on it and
selects it. The iteration 1 "default worker" rule and the `409 no_worker` code are removed.

Field limits: `title` up to 200 characters; `model` matches `^[A-Za-z0-9._:-]{1,64}$` and does
not start with `-` (it goes to the agent's command line); `client_op_id` up to 128 bytes;
otherwise `400`.

REST errors: `{error: <code>, message}` with an HTTP status (`workspace_conflict` has extra
fields, `locked_vscode` has `hint` and `held_by?`, `vscode_open` has `held_by`);
`413 too_large`: an artifact over the limit (§12); `409` with codes `archived`, `locked_vscode`
(input to a session VS Code is driving; body `{error, message, hint: "takeover", held_by?}`,
the client offers "Take over" or, with `held_by`, closing the session in VS Code, §9.3),
`vscode_open` (takeover of a session a live foreign process holds; `message` is "the session
is open in VS Code: close it there and take it over again", §9.3), `vscode_active` (takeover
before 30 s of silence without registry data, §9.3), `system_session` (the worker's service
session, §10.2), `request_closed`, `no_workspace`, `worker_offline`, `workspace_conflict`,
`workspace_in_use`, `personal_project`, `push_limit`, `schedule_limit` (§20), `conflict`, `worker_online` (replacing a
connected worker, §5.1), `worker_not_revoked` and `worker_has_sessions` (deleting a worker; the
latter with `sessions_count`); `400`: `bad_request`,
`unknown_artifact` (§12) and the path check codes (`outside_allowed_roots`, `path_not_found`,
`not_a_directory`, `invalid_path`); `504
worker_timeout`: the worker did not answer a hub request; `403`: a foreign `Origin` (§20); `429` and
`503`: limits (§20). An Access JWT verification error
is returned with a generic text; the reason goes to the hub log. `GET .../events`
also accepts `before_seq` (the last `limit` events before it).

## 7. MCP tools for agents

The worker runs the hqc MCP server and connects it to the runtime of every session.
Identification: the MCP server is a stdio process the runtime starts itself (via
`--mcp-config` for Claude Code, the JSON passed as a string, no file; via the
`mcpServers` field of `session/new` for ACP agents); the worker passes it the `session_token`
from `session.start` in the environment variable `HQC_SESSION_TOKEN` of that server only, and
the MCP process talks to the worker daemon over a local socket with this token (§7.2); the
daemon forwards the call to the hub in an `mcp.call` frame (§5.2). The hub issues the token.
Hermes keeps `mcpServers` from `session/new` only in the running agent's memory and does not
write them to its session database (checked in the source).

How the token reaches the MCP process (since 0.33; checked on 2026-10-25 with Claude Code
2.1.308 on macOS 26.6, live test `crates/hqc-hub/tests/live_mcp.rs`, notes in
`docs/notes/t30-questions.md`): in the `env` of the server `hqc` inside the `--mcp-config` JSON.
The CLI starts the server with its own environment plus that `env`; the token is not in the
CLI's environment (the worker's `HQC_*` never are, §8.1), so the agent's Bash tool does not see
it (`env | grep -c '^HQC_'` in the agent's Bash gave `0`), and it was not in the transcript,
`~/.claude.json`, the CLI's stream-json output, the hub's events or the worker's outbox. The
JSON is on the CLI's command line: on macOS another, non-root OS user cannot read a process's
argv or environment (`sysctl KERN_PROCARGS2` fails with `EINVAL` for 116 of 118 processes of
other users; the two it read were the login session's), but on Linux `/proc/<pid>/cmdline` is
readable by every user, so there the token is visible to other local users — who still cannot
use it: the socket is 0600 and the hub accepts the token only from the worker that runs the
session (§5.2). Rejected: `${VAR}` expansion in the MCP config and plain inheritance (the
variable would have to be in the CLI's environment, which Bash inherits), a config file (a
file holding the token). Limitation: another process of the same OS user (the agents of other
sessions on the worker included) can read the CLI's command line and the MCP process's
environment; this is the same soft boundary as in §14. Since 0.39 (T36) such a process cannot
use the token: the daemon accepts it only from a process inside the session's agent (§7.2). The bare model on the hub gets the same
tools directly. hqc tools are called without confirmation: Claude Code is started with
`--allowedTools mcp__hqc` (§8.1).

Tools act with the session owner's rights; the agent has no rights of its own.

- `hqc_attach(path?, content?, name?, kind?, caption?)` — upload a file as an artifact
  to the current session; `path` for runtimes with a file system, `content` for the bare model.
  Since 0.34 (T31) the worker's MCP server lists `hqc_attach(path, name?, kind?, caption?)`;
  `content` comes with the bare model (iteration 4) and is refused until then
  (`invalid_args`). The worker daemon does the file part itself, trusting neither the hub nor
  the agent's path (§2.1): `path` is absolute or relative to the session's working directory;
  its canonical form (symlinks resolved) must lie inside the session's workspace and inside
  `allowed_roots`; only a regular file of at most 100 MB, opened without following a symlink
  in the last component and checked again on the open handle, and exactly the size seen there
  is sent. Refusals come from the worker: `invalid_args` (outside, not a regular file, over the
  limit, a bad `kind`), `not_found` (no such file). Then the daemon uploads it (`PUT
  /worker/artifacts`, §5.4) and only then sends the `mcp.call`, whose `args` are `{artifact_id,
  caption?}` (`hqc_proto::mcp::AttachCall`) — the hub never sees the path. A cancel or EOF of
  the MCP process during the upload stops it, and no `mcp.call` is sent. The hub accepts only
  an artifact of the session's owner uploaded for this session (else `not_found`) and writes
  the `attachment` event (§4) once per `request_id` (§5.2); `name` absent — the last component
  of `path`; `kind` absent — by type and extension (§12). The result is `{event_id,
  artifact_id, name, kind}`. Checked live on Claude Code 2.1.308 (`crates/hqc-hub/tests/
  live_attach.rs`): the event lands between the call's `tool.call` and `tool.result`.
- `hqc_send(text, session?)` — a message to another session without waiting for a reply;
  without `session`, to the owner's personal assistant session. Since 0.35 (T32): `session`
  is required until the personal session exists (iteration 4); the result is `{to,
  delivery}`, `to` — the target's address, `delivery` — `queued` (the input queue, the
  target wakes on sends) or `buffered` (the send buffer, §7.1).
- `hqc_ask(text, session, timeout_s?)` — a message that waits for a reply. To the agent's own
  session: an immediate error. Since 0.35 (T32): the result is `{from, reply}` — the
  target's address and the text of its final answer; `timeout_s` is at least 1 and at most
  `ask_timeout_s` (a larger value is cut to it), by default `ask_timeout_s`.
- `hqc_spawn(project, workspace, runtime?, model?, prompt)` — create a session and an agent,
  return the address. `workspace` is required if the project has no `default_workspace_id`.
  Since 0.37 (T34): `hqc_spawn(project, workspace?, runtime?, model?, prompt)`; the session is
  created by the same path as `POST /api/projects/{id}/sessions` (§6), owned by the calling
  session's owner, in the workspace's folder (`cwd` `.`) on the workspace's worker, titled
  "Session N" until a title comes (§3), without `auto_approve`. `project` — a slug or an id
  of the owner's project; `workspace` — an id, a label or a path of a workspace of that
  project (else `not_found`), absent — the project's `default_workspace_id` (none:
  `invalid_args`, §13.4); `runtime` — `claude_code` only until iteration 4 (else
  `invalid_args`); `model` absent — the calling session's model (a cheap spawner does not
  start an expensive agent by default), present — checked as in §6. `prompt` (not blank)
  becomes a `peer.message` with `mode: send` from the calling session in the new session's
  input queue (`delivery: pending`, like a send to a session with `wake_on_send`), so it
  starts the first turn with the header `[hqc peer from <spawner> | mode=send]` and the new
  agent knows whom to answer with `hqc_send`; its chain is the caller's moved on (§7.1). The
  result is `{address}` at once; the call does not wait for the turn. The new session
  records the spawner (`spawned_by`, §3); spawns are limited per chain and per owner and
  hour (§20, `limit_exceeded`). A worker that is offline gets the session when it connects,
  as for the API.
- `hqc_list_sessions(project?)` — addresses, titles, statuses. Since 0.33 (T30): `project` is
  a slug or an id of the owner's project (another owner's or an unknown one: `not_found`); the
  result is `{sessions: [{address, title, status, current?}], truncated}`: the owner's
  sessions, the most recently active first, at most 200 (`truncated: true` beyond), archived
  sessions and the workers' service sessions left out; `status` is the session's `state`
  (§17.1); `current: true` marks the calling session.
- `hqc_search_history(query, project?, fuzzy?)` — history search, respecting grants. Since
  0.36 (T33): `hqc_search_history(query, project?, fuzzy?, cursor?)` runs the query of `GET
  /api/search` (§6, §15) for the calling session's owner, 20 hits per call; the result is
  `{results: [{address, title, archived, seq?, type?, ts, snippet}], next_cursor?}` (no
  `session_id`: the address is what the other tools take). `project` is a slug or an id of
  the owner's project (else `not_found`); a refused query is `invalid_args`. No side effects,
  so a repeated call simply runs again.
- `hqc_schedule(session, at|cron, message)` — the hub's cron (§16). Since 0.38 (T35):
  `hqc_schedule(session, message, at?, cron?, timezone?, catch_up?)`; `session` — an address or
  an id of the owner's session (the calling one too), by the target rules of §7.1 except that a
  session driven from VS Code is accepted (its runs are skipped while it is, §16); `at` — RFC
  3339 with an offset, in the future; `cron` — 5 fields in `timezone` (the owner's by default);
  exactly one of the two, else `invalid_args`, as are a bad pattern, zone or time and the limits
  of §20 except the count (`limit_exceeded`). The result is `{schedule_id, session, next_run,
  timezone}` (`session` — the target's address). Once per `request_id` (§5.2): the schedule's id
  is `once_id("hqc_schedule", …)`, and a repeat answers from that row. The agent can neither
  list nor delete schedules; the owner does that in the client (§6).

Session address: `<project-slug>/<session-slug>` or the id. A slug is unique within the project
and does not change on rename.

A tool is listed in `tools/list` only once its task lands (iteration 3: `hqc_list_sessions`
since 0.33, T30; `hqc_attach` since 0.34, T31; `hqc_send`, `hqc_ask` since 0.35, T32;
`hqc_search_history` since 0.36, T33; `hqc_spawn` since 0.37, T34; `hqc_schedule` since 0.38, T35); the list is shared by the worker's MCP server and the hub
(`hqc_proto::mcp::TOOLS`), the hub's implementations are a registry next to it
(`crates/hqc-hub/src/mcp.rs`). Errors have the one shape `{code, message}` (§5.2); the MCP
server returns them as a tool result with `isError: true` and the text `<code>: <message>`.
Codes added in 0.35: `timeout` (no reply in time), `limit_exceeded` (a chain limit, §7.1),
`worker_restart` (the asking worker restarted; never seen by the agent, whose call died).
Until grants (iteration 6) every tool works only on the calling session's owner's own
projects and sessions; anything else is `not_found`.

### 7.1 Semantics of send and ask **[decision]**

Delivery:
- A `peer.message` is always delivered to the agent as **input text** with the header
  `[hqc peer from <project>/<session> | mode=ask | request_id=... | reply with your final message]`
  or `... | mode=send]`. This way the agent tells ask from send in any runtime.
- Spoofing protection: when building `session.input`, the worker escapes the substring
  `[hqc peer` (anywhere in the text, case-insensitive) to `[hqc-peer-text`
  in **all** the text it passes to the runtime: the user's text, the text of a peer message
  from an agent and the `peer_prefix` block; the real header is added after escaping. The
  history on the hub is not rewritten.
- The runtime's text (since 0.35, T32; the worker builds it from `session.input`, §5.3): the
  "Messages from other sessions" block if `peer_prefix` is not empty, then the header line if
  `peer` is set, then the escaped `text` (with the inbox paths of §12). The block is
  `Messages from other sessions (they await no reply):`, an empty line, then per message
  `[hqc peer from <address> | mode=send | sent=<ts, UTC, seconds>]`, its escaped text and an
  empty line, closed by `---` and an empty line. The sender's address and `request_id` are
  escaped as well. Since 0.39 (T36) the fields inside a header (the
  sender's address, `request_id`) are reduced to one line of at most 200 characters with `[`,
  `]`, `|` and control characters replaced by `_`, so a field can neither end the header nor
  add one; the hub also refuses a `request_id` outside `A-Z a-z 0-9 . _ : -` (§5.2).
- Rights: `hqc_send`/`hqc_ask` to someone else's session require the owner of the sending
  session to have a `write` grant on the target.
- The target (since 0.35): an address or an id of a session of the caller's owner, not
  archived, not a worker's service session, else `not_found`. The calling session itself
  (`invalid_args`) and a target with `locked_by: vscode` (`unavailable`) are refused at once,
  for `send` as well as `ask`: there is no silent queue for a session driven from VS Code
  (§9.3). An empty (blank) `text` is `invalid_args`.

Queues (two different ones):
- The session's **input queue** (§17.2): `user.message` and `peer.message` with `mode=ask`.
  Each item starts its own turn, strictly in `seq` order. The prompt of `hqc_spawn` (§7) is a
  `peer.message` with `mode=send` that goes to this queue too.
- The **send buffer**: a `peer.message` with `mode=send` under `wake_on_send: false` does not
  start a turn; the hub accumulates such messages and passes them in the `peer_prefix` of the
  next `session.input`; the worker prepends them to the input text as a "Messages from other
  sessions" block. The buffer is cleared by the `ack` of the input that carried it. Buffer
  limit: 50 messages or 64 KB; the oldest are evicted with a `system` event. With
  `wake_on_send: true`, a send goes to the input queue like an ask that waits for no reply.
  Since 0.35 (T32): a buffered send is stored without `delivery` and kept in the table
  `peer_buffer` (migration `0015`; 64 KB is the sum of the texts in bytes); the eviction event
  is `system peer_evicted` in the target. When the hub sends an input it marks the buffered
  messages not carried by another input still `pending` as carried by this one and puts them
  in its `peer_prefix`, oldest first; the input's `ack` removes them from the buffer (their
  events keep no `delivery`), a send that arrives meanwhile waits for the next input. The
  input queue takes `wake_on_send` as it is when the message is sent; there is no API to set
  it yet. Since 0.39 (T36): an input sent again (after a reconnect) carries exactly the
  buffered messages it carried when it was first sent and that are still buffered, never
  newer ones (table `input_prefix`, migration `0019`): the worker answers a resent input it
  already gave to the agent with an `ack`, and that `ack` must not clear a message the agent
  never got.

The reply to an ask:
- The reply is the `agent.message` with `final: true` and `in_reply_to` equal to the
  `event_id` of that `peer.message`. It is returned as `mcp.result` to the blocking call.
- Target session `locked_vscode`: an immediate error. `worker_offline`: waits until the
  timeout. The default timeout is 10 minutes (`ask_timeout_s` in `session.start`).
- The worker sets the runtime's tool timeout to no less than `ask_timeout_s` plus a
  minute: Claude Code — `MCP_TOOL_TIMEOUT` in the process env, in milliseconds; by default
  it is practically unlimited (1e8 ms), so the worker always sets it explicitly to
  `(ask_timeout_s + 60) * 1000` (checked on 2.1.307). Separately, Claude Code applies a 30-minute
  idle timeout to stdio MCP servers (`CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT`): a call with no reply
  and no progress notifications is cut off after 1800 s even with a large `MCP_TOOL_TIMEOUT`, so
  the hqc MCP server sends progress while waiting on `hqc_ask` (§7.2; checked on 2.1.307);
  Hermes — `mcp.tool_call` in the profile config (300 s by default, checked in the source); other
  ACP agents — per their documentation **[verify]**.
- Open `ask`s are stored in `PendingRequest`; after the sending worker restarts, the
  blocking call is lost and the `ask` is closed with an error (§5.2).
- Since 0.35 (T32): the reply counts from any final `agent.message` the worker or its
  mirror stores in the target; it closes the ask with `{from, reply}` (§7). The timeout gives
  `error {code: timeout}`; a reply after it, or after `mcp.cancel`, goes nowhere. A target
  whose turn on the question is lost (`system turn_lost`: its process stopped, its worker
  restarted, the watchdog) gives no reply; since 0.39 (T36) the ask then fails at once with
  `unavailable` instead of waiting for its timeout (the question is not delivered again). A
  turn that ends without a final message for another reason (stopped by the user) still
  waits for the timeout. The asking session's turn stays `running` while it waits; the
  turn watchdog (§17.3) leaves it alone. In the asking session's feed the client shows the
  waiting call as "Waiting for a reply from <session>" with its timer (the runtime's
  `tool.call` of `hqc_ask`, no event of its own).

Chains:
- `chain_id` is created on the first `ask`/`send` from a turn started by a human; `depth`
  grows on every `ask` (limit 3), `hops` on every `peer.message` in the chain
  (limit 20). The hub keeps the `chain` on the input that started each turn and sets it on
  outgoing messages itself (since 0.35, T32; the MCP server is not involved): the chain of a
  call is that of the calling session's last `delivered` input (inputs reach the agent one
  turn at a time, in `seq` order, so that input started the current turn). A `user.message`
  has no chain: a new one starts with `depth` = 1 for an ask, 0 for a send, and `hops` = 1;
  otherwise the chain goes on with `hops` + 1 and, for an ask, `depth` + 1. Since 0.37 (T34)
  the `chain_id` started by a human's turn is derived from that turn's input (uuid v5), so
  all calls of one turn share one chain, as the first sentence says (before, each call of a
  human's turn started a chain of its own). `hqc_spawn` moves the chain like a send (`hops`
  + 1) and counts the sessions it creates per chain (§20). Buffered sends
  carried in `peer_prefix` do not start a turn and lend it no chain.
- Since 0.39 (T36): the hub takes the call's chain from the input the worker names in
  `mcp.call.turn_input` (§5.2), if it is an input of the calling session (a `user.message` or a
  `peer.message`, `pending` or `delivered`) not older than its last delivered one; otherwise from
  the last delivered input as before. An input's `ack` bypasses the worker's outbox and is lost
  with a dropped connection, so the hub could be a turn behind and give a call the previous
  turn's chain; the worker cannot name an older turn to get a fresh chain.
- Since 0.39 (T36): `hqc_schedule` moves the chain like a send (`hops` + 1, refused at the
  limit) and stores it with the schedule; its runs carry it (a `user.message` with a `chain`), so
  a turn started by an agent's schedule continues the agent's chain instead of starting a
  human's. A schedule made by a person (the API) carries none. A chain carries at most 50 peer
  messages in all (§20): `hops` grows only along a path, and one turn could otherwise fan out
  without end.
- When a limit is exceeded, the tool returns an error (`limit_exceeded`, since 0.35); only a human's turn can continue the
  chain. A deadlock (A waits for B, B waits for A) is resolved by the timeout.

### 7.2 Requirements for the hqc MCP server
- A stdio process `hqc-worker mcp [--socket <path>]` that the runtime starts itself: Claude
  Code via `--mcp-config` as a JSON string, ACP agents via `mcpServers` in `session/new` (§7).
  JSON-RPC 2.0, one message per line; it implements `initialize` (it answers the runtime's
  `protocolVersion` if it knows it — `2025-11-25`, `2025-06-18`, `2025-03-26`, `2024-11-05` —
  else the newest; Claude Code 2.1.308 asks for `2025-11-25`), `ping`, `tools/list`,
  `tools/call`, `notifications/cancelled`; another method gets `-32601`, an unlisted tool
  `-32602`. Hand-written (no MCP SDK crate) since 0.33, T30.
- Gets `session_token` only from `HQC_SESSION_TOKEN`; does not write it to tool
  results, runtime logs or files (§7).
- Forwards every call to the worker daemon over a Unix socket `$HQC_HOME/mcp.sock` (mode
  0600, one per daemon: a second daemon with the same `HQC_HOME` does not start; a peer of
  another OS user is refused) with this token; the daemon sends it to the hub in an
  `mcp.call` frame (§5.2). Keeps no state of its own except its open calls (since 0.35 not
  the `chain` either: the hub sets it, §7.1).
- The daemon accepts on the socket only the token a running agent of one of its sessions was
  started with, and since 0.39 (T36) only from a process that descends from that session's
  agent process: the peer's pid comes from the kernel (`LOCAL_PEEREPID` on macOS,
  `SO_PEERCRED` on Linux) when the connection is accepted, and its parents are walked
  (`proc_pidinfo` / `/proc/<pid>/stat`, at most 32 levels) up to the pid the adapter started
  (`Adapter::pid`; a hello waits up to 10 s for it while the agent starts; an adapter that
  reports no process gets no MCP server). The token is on the agent's command line, which every
  process of the OS user can read; another session's agent that uses it is refused (T30
  question 2). Claude Code starts its stdio MCP servers inside its own process tree
  (checked on 2.1.308 on 2026-10-25: `live_mcp` and `live_peers` pass with the check on). It checks the token again on every call (the agent may have stopped since), and sends
  `mcp.call` with the session's latest token from the hub (§5.2): an agent that keeps running
  across a reconnect's `session.start` keeps its MCP process and its token. When the agent
  stops, its open calls fail with `unavailable`.
- The server name in the runtime config is `hqc`, the tools are `hqc_*`; in Claude Code they
  are allowed up front via `--allowedTools mcp__hqc` (§8.1).
- While a call waits for a reply (`hqc_ask`), sends `notifications/progress` with the
  `progressToken` from the call's `params._meta` at least once a minute. Without it Claude Code
  cuts the call off after 30 minutes of idleness (§7.1); the server gets no
  `notifications/cancelled` in that case. Since 0.35 (T32): every 30 s for every open call
  that has a `progressToken`, with `progress` 1, 2, … and the message "waiting for the hub",
  until the answer or the cancel.
- If the runtime cancelled the call (`notifications/cancelled`, e.g. on `MCP_TOOL_TIMEOUT`)
  or the MCP process exited with an open call (EOF on stdio: the process exits and the daemon
  sees its socket close), the worker daemon closes
  the corresponding ask `PendingRequest` with an `mcp.cancel {request_id, reason}` frame (§5.4);
  it sends `mcp.cancel` for every call open at that moment, fast ones included, and the
  cancelled call gets no response (MCP);
  the idle cutoff without a notification is prevented by the progress notifications, so
  there is no separate detection for it.

## 8. Runtime adapters

Two adapters on the worker: a native one for Claude Code (§8.1) and a generic ACP one (§8.2)
for Hermes and any agent from the ACP registry. The third, the bare model, lives in the hub
(§8.3). All implement one interface: `start`, `input`, `stop`, `steer?`, `kill`, `resume`,
`answer_approval`, `answer_clarify`, plus the event stream of §4. The input queue is the
worker's responsibility, not the adapter's (§17.2).

### 8.1 Claude Code
The process `claude -p --output-format stream-json --input-format stream-json
--include-partial-messages --replay-user-messages --permission-prompts host
--permission-prompt-tool stdio --strict-mcp-config --mcp-config '<json>'
--allowedTools mcp__hqc [--disallowedTools <rules…>] [--settings '<json [permissions]>']
(--setting-sources user,local | --restricted) --permission-mode <"ask" mode | auto>
[--append-system-prompt <text>] --session-id <uuid>` for a new session
(the hub assigns the uuid and puts it in `runtime_session_id`) and `--resume <uuid>` to
continue. The choice between them depends on whether the transcript
`<config>/projects/<encoded cwd>/<uuid>.jsonl` exists (§9), not on `mode` from `session.start`:
both `start` and `resume` of the adapter use `--resume` when the file exists, otherwise
`--session-id`. `--permission-prompt-tool stdio` is required: without it the CLI asks the host
nothing and rejects outright everything that needs confirmation
(`system/permission_denied`, `permission_decision.source: config`), and `AskUserQuestion`
is not in the tool list (checked on 2.1.307). `--allowedTools mcp__hqc`
allows all tools of the hqc MCP server so their calls do not go through `can_use_tool`
(§7); on 2.1.307 the forms `mcp__hqc` and `mcp__hqc__*` work the same; without the flag an
MCP tool call arrives as `can_use_tool`. Checked on 2.1.300: `--resume` continues the same uuid and appends to the same
jsonl file. `--strict-mcp-config` disables MCP servers from `~/.claude.json`
and the project's `.mcp.json`; they can be allowed only through the environment (§14,
`allow_local_mcp`). In the process env: `MCP_TOOL_TIMEOUT` (§7.1): milliseconds, default
1e8 ms, minimum 1000 ms (checked on 2.1.307).

The hqc MCP server (since 0.33, T30): `--mcp-config '{"mcpServers": {"hqc": {"type": "stdio",
"command": "<the worker's own binary>", "args": ["mcp", "--socket", "<$HQC_HOME/mcp.sock>"],
"env": {"HQC_SESSION_TOKEN": "<session_token>"}}}}'`, built per process start with the
session's latest token (§7, §7.2), and `MCP_TOOL_TIMEOUT = (ask_timeout_s + 60) * 1000` in
the CLI's environment on every start. Checked live on 2.1.308 (`live_mcp`): `system/init`
lists `mcp_servers: [{name: hqc, status: connected, source: dynamic}]` and nothing else — with
`--strict-mcp-config` neither the user's servers nor the claude.ai connectors of the logged-in
account reach the session — and the tool shows as `mcp__hqc__hqc_list_sessions`; the model
loads it with `ToolSearch` first (one model turn), then calls it, so a turn that calls an hqc
tool needs at least two model turns. With `--allowedTools mcp__hqc` the call runs without
`can_use_tool`. The title runs (`title.request`) get no `--mcp-config`.

Session restrictions are never dropped silently **[decision]**: `disallowed_tools` → `--disallowedTools`
(each rule as a separate argument), `system_prompt` → `--append-system-prompt`,
`restricted` → `--restricted` (§8.4). The worker rejects `session.start` (`invalid_frame`)
if `model` does not match `^[A-Za-z0-9._:-]{1,64}$` or starts with `-`, or a
`disallowed_tools` rule is empty, starts with `-` or contains control characters.
Settings of someone else's repository: `-p` has no folder trust dialog, so
`.claude/settings.json` from the workspace (hooks, `defaultMode: bypassPermissions`) would apply
without asking. The adapter passes `--setting-sources user,local` (the project file is not read;
`settings.local.json` is read: `always` writes there) and an explicit `--permission-mode` with
the "ask" mode: in the help of CLI 2.1.308 this is `manual` (`default` is not among the options),
before that `default`; on start the worker reads `claude --help` and picks the one present
(`[permissions] mode = "auto"` replaces it with `auto`, §14). Checked on 2.1.308 (T12,
fixture `always-git-push`): with `--setting-sources user,local --permission-mode manual`,
`always` is still offered and writes the rule to `settings.local.json`; `manual` shows in
`system/init` as `"permissionMode": "default"`.
The process environment is scrubbed: from the worker's environment only `PATH`, `HOME`, `USER`,
`LOGNAME`, `SHELL`, `LANG`, `LC_*`, `TERM`, `TMPDIR`, `SSH_AUTH_SOCK`, `CLAUDE_*`,
`ANTHROPIC_*` pass, then the workspace's environment variables (§14); `HQC_*` never. The worker
stores the working directory in canonical form, requires it to exist and rechecks it before
every process start (a symlink could have changed). At most `max_sessions`
(local config, default 8) agent processes at once; the next start gets
`error {agent_start_failed, retryable: true}`.

Identifiers. Checked on 2.1.307: the `uuid` of `assistant` and `user` messages
(including `tool_result`) in stream-json **equals** the `uuid` of the corresponding records
in jsonl. So the adapter derives `event_id` as a uuid v5 of this `uuid`, as the mirror does
(§3 Event, §9.2). The record of the user's original prompt does not come in stream-json
without the echo: the adapter takes its `uuid` from the `--replay-user-messages` echo
(`isReplay: true`); checked on 2.1.307: the echo's `uuid` equals the `uuid` of the prompt
record in jsonl.

Input attachments (`session.input.artifact_ids`, §5.3, §12). Before passing the input to the
runtime, the worker downloads each artifact via `GET /worker/artifacts/{id}` with its token (no
redirects) to `<workspace>/<cwd>/.hqc/inbox/<event_id>/<name>`. The name comes from the
response header `x-hqc-artifact-name` (percent-encoded UTF-8) and is sanitized by the worker:
the last path component, control characters and `:*?"<>|` → `_`, no leading or trailing dots
and spaces, up to 120 bytes keeping the extension, empty → `file`; names that collide within
one input (case-insensitively) get `-2`, `-3`… before the extension. The directories `.hqc`,
`inbox`, `<event_id>` are created one at a time inside the canonical working directory; none
may be a symlink; the file is written to a temporary file and renamed into
place. If the workspace is a git repository (a `.git` directory, or a `.git` file of a
worktree, in which case the common git directory from `commondir`), the line `.hqc/` is added to
its `info/exclude`. A block is appended to the input text: an empty line, `Attachments:` and a
`- <path>` line per file, paths relative to the working directory
(`.hqc/inbox/<event_id>/<name>`); an empty text gives only the block. Images (`kind: image`,
`image/png|jpeg|gif|webp`, up to 5 MB) also go into the stream-json user message: `content` is
an array of a text block and blocks `{type: image, source: {type: base64, media_type, data}}`
(Messages API format; **[verify]** on a live CLI, fixture `crates/hqc-worker/fixtures/inbox`).
An artifact the hub does not serve (`401`, `403`, `404`, `410`): the input is rejected with
`error {internal, retryable: false}` (on the hub: `input_rejected`); network and disk errors:
`retryable: true`; whatever was partly downloaded is deleted.

Permissions: with `--permission-prompts host --permission-prompt-tool stdio` a request arrives on stdout as
a `control_request` with `subtype: can_use_tool` (`tool_name`, `input`,
`permission_suggestions`); the adapter replies with a `control_response` with `behavior:
allow|deny`. Options for the client: `once`, `session`, `always`, `deny`
(`always` only if `permission_suggestions` contains `addRules` in `localSettings`). Mapping: `once` → allow;
`deny` → deny; `always` → allow plus `updatedPermissions`; Claude Code itself writes the
rule to `.claude/settings.local.json` **at the root of the git repository containing the cwd**
(or of the cwd itself if there is no repository; checked on 2.1.308). Adapter rule: only the
`permission_suggestions` items with `type: addRules` and `destination: localSettings` go into
`updatedPermissions`; the other suggestions (`setMode`, `addDirectories`
etc.) are dropped — otherwise the session switches to `acceptEdits` and extends
its directory list (checked on 2.1.307). If there are no such items, `always`
is not offered; `session` → allow and a rule in the adapter
until the process ends; for `Bash` and other tools that run commands, a suggestion without
`ruleContent` ("the whole tool") is not turned into a session rule — only the exact input is
remembered. Under `--restricted` (§8.4) local settings do not apply, so
`always` maps to `session`. The hub does not store permanent permissions. The
`AskUserQuestion` tool arrives through the same `can_use_tool`: the adapter turns it into
a `clarify.request`, assigning its own `id`s to the questions, and returns the answers via
`updatedInput.answers`, whose keys are the question text and whose values are the option's
`label` (the adapter maps `id` to text itself). The
`control_request`/`control_response` format was checked on 2.1.307 with `--permission-prompt-tool stdio`;
exact JSON: `docs/notes/claude-control-protocol.md`; fixtures:
`crates/hqc-claude-jsonl/fixtures/*-2.1.307.jsonl`.

Mapping of stream-json to §4: `stream_event` text delta → `agent.delta`; `assistant`
with text → `agent.message`, where the adapter holds the last text until the
next stream item: `tool_use` → `final: false`, `result` → `final: true`;
`tool_use` → `tool.call`; `tool_result` → `tool.result`; `result` → `usage` and
`status idle`; the `--replay-user-messages` echo, a `user` message with `isReplay: true`,
→ `ack` of `session.input` (this field tells the echo from a `user` with `tool_result`).
The CLI repeats on stdout every `control_response` the adapter sent to stdin;
the adapter skips those lines. `in_reply_to` = the `event_id` of the input the adapter
sent last.

Cost (since 0.28, T27; checked live on 2.1.308, fixtures `cost-turns`, `cost-resume`):
`result.total_cost_usd` is the cumulative cost of the whole Claude Code session, not of the turn:
in one process it grows from turn to turn (0.00216749, then 0.00398629), and after `--resume`
it starts from `totalCostUSD` of the transcript's last `cost-state` record (written when a
process ends) — the resumed turn reported 0.0043361 = 0.00398629 + its own cost. `modelUsage`
is cumulative the same way; the token counts in `result.usage` are the turn's. The adapter emits
`usage.cost_usd` = this `total_cost_usd` minus the previous total of the same session: the
previous `result` of the process or, for the first turn after `--resume`, the last `cost-state`
of the transcript read when the process starts (none — 0). The difference is never negative,
and a total below that baseline does not lower it. `usage.session_cost_usd` is
`total_cost_usd` itself.

Slash commands (since 0.28, T27): `system/init` comes before every turn of a process and lists
`slash_commands` (built-in commands, skills and plugin commands, e.g. `compact`, `context`,
`code-review`, `anthropic-skills:pdf`; skills are also listed in `skills`, subagent types in
`agents`, which are not commands) and `terminal_slash_commands` (those that only work in the
terminal UI: `doctor`, `color`, `focus`, `reload-plugins` on 2.1.308). The adapter reports
`slash_commands` without the terminal-only ones and the CLI's internal `__name` ones to the hub
in `session.state.commands` (§5.4): once per process start, and again only when the list
changes.

Not every tool call asks for permission: the CLI allows read-only commands (e.g.
`echo`) itself, without `can_use_tool`, so an `approval.request` does not come for
every `Bash` (checked on 2.1.308 in the live run of T4).

The worker's permission policy (`[permissions]`, §14). The `allow` and `deny` lists go to the CLI
on every process start as `--settings '{"permissions":{"allow":[…],"deny":[…]}}'`,
including with `--restricted` (the CLI applies `--settings` there too). Checked on 2.1.308
(T12, `docs/notes/claude-control-protocol.md`, "Permission policy, 2.1.308"):

- a call under `allow` runs without `can_use_tool`; one outside both lists is asked about as
  usual;
- a call under `deny` does not run and **is not asked about**: there is no `can_use_tool`;
  stream-json brings `system` `permission_denied` (`tool_name`, `tool_use_id`, `message`,
  `decision_reason_type` `rule`/`subcommandResults`), a `tool_result` with `is_error: true` and
  an entry in `result.permission_denials`. The adapter turns `permission_denied` into a
  `system` event with code `policy_denied` (§4); the `tool.result` with the error comes as usual;
- a deny from `--settings` beats an allow that `always` wrote to `settings.local.json`, and
  beats the `auto` mode; the CLI guarantees this;
- a tool denied as a whole (`WebFetch`) disappears from the session's tool list;
- Bash rules: `Bash(x:*)` is a prefix up to a word boundary, `*` inside matches any
  sequence of characters (`Bash(curl * | sh)`), a command with `&&`, `;`, `|` is checked
  subcommand by subcommand.

The adapter reads the same rules itself (a second line of defence): a `can_use_tool` that
matches `deny` it rejects without a card (neither `always` nor the hub's `auto_approve` can
answer it) and writes `system policy_denied`; in the other `approval.request`s it sets
`policy_hint`: `allow` if the call matches an `allow` rule entirely (the CLI asked anyway),
otherwise the field is absent. The adapter understands a tool name, an MCP server
(`mcp__hqc`) and `Bash` rules (prefix, `*`, exact command); it leaves the rest
(`Read(./x/**)`, `WebFetch(domain:…)`) to the CLI.

The `auto` mode (`--permission-mode auto`, checked on 2.1.308): the CLI decides itself and
**never asks**: the classifier let through `rm -rf` outside the workspace, `git push --force`
and `sudo` when the user had asked for it; the decision is visible only in
`tool_result_meta[].permission_decision.reason_type` (`classifier`, `mode`). The only
boundary in this mode is `deny`.

Interrupting a turn (`session.stop`): the adapter sends `control_request {subtype:
"interrupt"}` to stdin; the CLI ends the turn with a `result` record with `subtype:
error_during_execution`, the partial reply text comes as a separate `assistant` record, and
the process stays alive (checked on 2.1.308). If there is no `result` within 10 s, the process
group gets SIGINT; the CLI exits on it, and the next input brings the process up via `--resume`.

The working directory is `<workspace>/<cwd>`. The process environment comes from the binding
of the workspace path in the worker's local config (§14). Authentication is the subscription
already logged in on the machine (Keychain on a Mac); the hub neither stores nor sees it.

Session title. Claude Code writes a record
`{"type": "ai-title", "aiTitle": "<title>", "sessionId": "<uuid>"}` to the transcript: a title
the model made up from the first prompt, sometimes several times per session (the last one
wins; fixture `tool-call-2.1.307`, the records come right after the first prompt, before the
model's reply). After every finished turn the adapter reads the transcript and, if the
last non-empty `aiTitle` differs from the one already sent, sends the hub `session.state`
with a `title` field (§5.4); the hub applies it by the `title_source` rule (§3 Session).
Checked on 2.1.308 (T13, live run `live_ai_title`): with `--input-format stream-json`
the CLI **does not write** `ai-title`, neither by the end of the turn nor 5 s after it; none of
the T0 (2.1.307) and T12 (2.1.308) transcripts recorded in this mode have it either — it is
there only in the run with the prompt as an argument (`tool-call-2.1.307`). So in iteration 1
adapter sessions in practice keep the default title until the CLI starts writing
`ai-title` in this mode or the user renames them **[verify]** on
later CLI versions. Such sessions get a title from the worker via `title.request` (§3 Session,
since 0.25).

The worker runs `title.request` (T24) as a one-shot process — not through the session's adapter
and not in its process: `claude -p --model claude-haiku-5-5 --max-turns 1 --no-session-persistence
--strict-mcp-config --setting-sources "" --tools "" --system-prompt "<title prompt>"
--output-format json`, with the first message and the first reply on stdin. Binary and
environment as for the adapter, but without `CLAUDE_CODE_ENTRYPOINT`; the working directory is
an empty worker folder `$HQC_HOME/titles/<slot>` (not a workspace), created before the start and
removed afterwards; slot numbers (0 and 1) are reused. Without `--no-session-persistence` the
transcript would land in the mirror and in the VS Code history; with it the CLI writes neither a
transcript in `<config>/projects` nor a record in `<config>/sessions`, and the launch folder does
not appear in `~/.claude.json` (checked in the live run `live_session_title` on 2.1.308, T24;
the CLI accepts `--setting-sources ""` and `--tools ""`; command line and results:
`docs/notes/claude-control-protocol.md`). From the JSON reply the worker takes `result` (with
`is_error`: an error), strips quotes, line breaks and a trailing period, and cuts it to 60
characters. Start timeout 25 s (the process and its group are killed); at most 2 runs at once
per worker.

Claude Code's built-in tools `ListAgents` and `SendMessage` find other Claude sessions
on the same machine and inject messages into them directly, bypassing the hub: the second
session gets a turn with no input event in its history. The worker always adds them to
`--disallowedTools`; `allow_native_messaging = true` in `worker.toml` brings them back.
Agents talk to each other through `hqc_send`/`hqc_ask` (§7).

### 8.2 ACP agents (generic adapter)
The adapter starts **a separate ACP process for each hqc session** (stdio JSON-RPC)
with the workspace environment (§14). The start command comes from the worker's local config
by `acp_agent` name, as an absolute path (in a non-interactive shell `hermes` is not on PATH).
On start the worker calls `initialize` on every configured agent and puts its `load_session`,
`resume`, `image`, `isolation` and version into `hello.capabilities.acp_agents`; a missing
binary is visible before a session is created.

Initialization: the adapter declares `clientCapabilities.fs` and `terminal` **unavailable**:
the worker does not implement reading/writing files or a terminal on the agent's behalf. If
`initialize` requires `authenticate` (the provider is not configured), the adapter writes a
`system` event "the agent is not configured on the worker" and fails the start (§5.2).

Mapping to §4: `session/new {cwd, mcpServers}` → creation; `session/prompt` → a turn,
`ack` of `session.input` right after sending; `session/update` with
`agent_message_chunk` → `agent.delta`/`agent.message`, `agent_thought_chunk` →
`agent.thinking`, `tool_call`/`tool_call_update` → `tool.call`/`tool.result`.
The end of a turn is the reply to `session/prompt` with a `stopReason`: the adapter holds
the last text, sets `final: false` on a `tool_call` and `final: true` on the reply to
`session/prompt`; `stopReason: cancelled` → no final message, `status idle`.
Diffs from `tool_call` are stored as an artifact of `kind: diff` and referenced from
`tool.result.artifact_id`, with the first 4 KB in `result_preview`. `session/request_permission`
→ `approval.request` with the agent's **real** `options[]` (by `optionId`), `canonical`
by `kind` and name; the answer is the chosen `optionId`. `session/cancel` → `session.stop`.
Resume is `session/load` if the agent declared `loadSession`; otherwise `session/new`
and a retelling of the history by the adapter. The adapter **checks the `sessionId` in the reply
to `session/load`/`session/resume` against the requested one** and stops with an error on a
mismatch: the agent may have silently created a new session. `usage` only if the agent reports
tokens; ACP has no `clarify`. The adapter never sends a second `session/prompt` during a
turn: the worker runs the input queue (§17.2), even if the agent can queue prompts itself —
otherwise `in_reply_to` goes out of sync.

Sessions without a workspace are not allowed for ACP agents (§8.4): they have no equivalent of
`--restricted`.

Claude Code via ACP (`@agentclientprotocol/claude-agent-acp`, Node, on top of the Agent
SDK) is **not used**: the native adapter of §8.1 is proven, gives `--restricted`,
`--strict-mcp-config`, `--session-id` and the control protocol directly, without Node
on the worker.

#### 8.2.1 Hermes specifics
Checked in the source on llm-server, canary version
`v0.21.4+canary.20261005T071900Z-434-g4787e4d56f` (commit `4787e4d5`); canary has no
stable number, so the hash is recorded.

- Start: `~/.local/bin/hermes-acp` (or `hermes acp`) with the profile via `HERMES_HOME`.
  The profile is set by a **local binding** in `env_bindings`, not by the hub (§14).
  This is not the messaging gateway (`hermes-gateway.service`); that one is left alone.
- `initialize` declares `load_session`, `image`, `fork`, `list`, `resume`
  (ACP v2 methods enabled via `use_unstable_protocol`).
- The worker **always** sets `HERMES_ACP_SKIP_CONFIGURED_MCP=1` unless the environment
  allows `allow_local_mcp`. The hqc MCP is passed in `session/new` in the
  `mcpServers` field; Hermes registers them per session, reconnects them on
  `session/resume` and does not persist them.
- Permission options are built dynamically: `allow_once` always; `allow_session`
  and `deny_always` unless `once_only`; `allow_always` only with `allow_permanent`;
  file edits come as a separate request with only `allow_once`/`deny`. Mapping
  by `option_id`: `allow_once` → once, `allow_session` → session, `allow_always` →
  always, `deny` / `deny_always` → deny. `allow_session` has `kind: allow_always`,
  so mapping by `kind` is wrong.
- The `approvals.timeout` timeout (300 s, read on every request) →
  `approval.cancel {timeout}`.
- `session/resume` with an unknown id **silently creates a new session** with a warning in the
  log **[verify]** — hence the id check of §8.2 is mandatory.
- Hermes's `usage_update` in ACP is an estimate of context occupancy, not the turn's tokens:
  it is not translated into `usage`.
- Steer: the prompt text `/steer <text>` intervenes in the active turn, `/queue <text>`
  queues it in Hermes. The adapter implements `session.steer` via `/steer`;
  `/queue` is not used (the worker runs the queue).
- The `hermes-acp` toolset has no messaging, cron or `clarify` (checked in `toolsets.py`).
  Hindsight memory and the profile's skills work as usual, so a separate profile is used in
  other people's sessions (§11.1, §14).
- A Hermes session has a single writer: hqc does not drive a session the Telegram gateway is
  driving in parallel; its transcript can only be continued by a new hqc session.
- On the gateway, `api_server` is also enabled (needed by `hermes peer` and the Jobs API);
  the adapter does not use it.
- Each session is a separate Python process; recommended `idle_timeout` is 30 minutes.

### 8.3 Bare model
Runs in the hub: OpenRouter, the Anthropic API or a local model through the worker's
proxy (§8.5). Loop: model ↔ hqc MCP tools (§7); no file system.
The default runtime of the personal assistant session if there is no online worker with
Claude Code when it is created; can be changed later in the session settings.

### 8.4 Session without a workspace
For architecture discussions. Options: the bare model (§8.3) or Claude Code
on worker `worker_id` in a temporary empty folder `<worker-data>/scratch/<session_id>`
(`$HQC_HOME/scratch/<session_id>`, 0700; `session.start` with `workspace_path: null` and
`restricted: true`; without `restricted`: `invalid_frame`) with the `--restricted` flag (removes the tools that run commands and confines
the file tools to the working directory). Runs on the subscription. The user picks the worker
at creation; by default, the last used online worker
with Claude Code. ACP agents are not allowed here.

### 8.5 Local models
The worker announces local OpenAI-compatible endpoints in `hello` (on llm-server this is
`ninfer` on 127.0.0.1:8082, model qwen3.8-27b). The hub proxies bare-model requests
in `model.proxy` frames with streamed chunks. By default local models are available
only to the worker owner's sessions; `share_local_models: true` in the local config
opens them to colleagues' bare models in shared projects.

## 9. Mirror and import of Claude Code sessions

The VS Code extension and the CLI write the same files
`<config>/projects/<encoded-cwd>/<uuid>.jsonl`, where `<config>` is `$CLAUDE_CONFIG_DIR` or
`~/.claude`. Folder name encoding (as in CLI 2.1.308, checked against the folder names of the
T0 runs and against the CLI function itself): the real path of the working directory is
encoded (`process.cwd()`, symlinks resolved: `/tmp` → `/private/tmp` on macOS); every UTF-16
unit that is not an ASCII letter or digit is replaced with `-`; if the result is longer than
200 characters, it becomes the first 200 characters, `-` and base36 of the absolute value of a
32-bit hash of the string (`h = (h << 5) - h + unit | 0` over UTF-16 units). This is how the
adapter and the mirror find the file for a path. The name is **not decoded** back (`/a/b.c` and
`/a/b-c` collide), so the worker takes the session's working directory from the `cwd` field of
the jsonl records, keeps a "file → cwd" cache on disk (§5.2) and uses the folder name only as a
key. Subfolders with subagent transcripts and files that are not `<uuid>.jsonl` are ignored.
Verified on 2.1.307: besides `user`/`assistant` the file has `attachment`,
`file-history-snapshot`, `api-request*`, `last-prompt`, `ai-title`, `cost-state`,
`queue-operation`, `system` and other records; the parser skips them, except `ai-title`, which
it parses separately (the `aiTitle` field); the adapter takes the session title from it (§8.1).

Format on 2.1.308 (T15, fixtures `vscode-takeover-2.1.308` and `adapter-then-outside-2.1.308`,
`docs/notes/t15-questions.md`):

- `claude -p "<prompt>"` (prompt as an argument, as a person in a terminal would) and
  `--resume <uuid>` in the adapter's stream-json mode append to **the same** file: both parts
  carry the same `cwd` and `sessionId`, `entrypoint: sdk-cli`, `version`, `gitBranch`. Each part
  starts with a pair of `queue-operation` records (`enqueue`/`dequeue`, no `uuid`), then the
  `user` prompt record.
- Every content block is a **separate record** with its own `uuid` (`thinking`, `text`,
  `tool_use` of one model response are three records with a shared `message.id` and an
  `apiBlockIndex` field); the record's `message.content` holds one block. In jsonl every record
  of a response has `message.stop_reason`: `tool_use` before a tool call, `end_turn` on the
  records of the turn's last response; in stream-json this field is `null`.
- `thinking` in jsonl is empty text with a `signature`; in the adapter's stream-json there was no
  such record in either run (an `attachment` `thinking_drop` instead), so it does not count for
  detecting a foreign writer (§9.3).
- `attachment` records (environment, tool list, CLAUDE.md, skills) have a `uuid` and sit in the
  `parentUuid` chain between `user` and `assistant`; the main chain is determined by record
  type, not by `parentUuid`.
- New types without `uuid`: `api-request-shape`, `atis-latch`, `mode`; service records
  `cost-state`, `last-prompt`. `ai-title` was **not written** in either 2.1.308 run, not even
  for a prompt passed as an argument (on 2.1.307 it was), so an imported session's title is
  usually "Session N".
- Prompt records have `promptId`, `promptSource: sdk`, `turnOrigin`, `turnPosition`,
  `permissionMode`; records injected by the CLI have `isMeta: true` (the parser does not show
  them); the summary after `/compact` has `isCompactSummary: true`.

### 9.1 Import
Candidates come from `workspace.list_candidates` (§5.3, `GET /api/workers/{id}/candidates`):
the worker walks `<config>/projects/*/<uuid>.jsonl`, takes `cwd` from the first record that
has one (the "file → cwd" cache on disk, §5.2), groups by `cwd` with the session count and
`last_active` (modification time of the newest file) and returns only folders inside
`allowed_roots`. In the client, the "Import from Claude Code" button on the Workspaces screen
shows the candidates; the selected folders become workspaces of the project through the
ordinary `POST /api/workspaces` with `mirror_enabled: true` (body `CreateWorkspaceImport`). The
hub sends the worker `mirror.set` (§5.3), and does the same for every workspace with the mirror
after each `hello`; when a workspace is deleted, `mirror.set` with `enabled: false`. The worker
accepts only a folder inside `allowed_roots` (§2.1). Since 0.23 the mirror is on for every
workspace by default (hub migration `0011` turned it on for existing ones), so importing
candidates is just a way to add a folder; on the Workspaces screen each folder has a "VS Code
sessions" switch (`PATCH /api/workspaces/{id} {mirror_enabled}`, §6).

**No duplicates when turned on for a non-empty workspace** (since 0.23). The hqc sessions in
the folder are already in the hub, and their prompts are hub `user.message` events with their
own `event_id` (v7), not v5 from the record's `uuid`, so importing their transcripts would
produce duplicates. So `mirror.set` with `enabled: true` carries `sessions?:
[{runtime_session_id, since_ts}]`: the hub's sessions in this workspace with a known transcript
and the time of their last event in the hub (at most 5000, the most recent; empty means the
field is absent; example `hub.mirror.set.sessions.json`). The worker's first pass over such a
transcript (a file the mirror has not read yet, offset 0), up to the end of the file, does not
send events for records with `timestamp` <= `since_ts`; the record tree and the last prompt are
built as usual. Transcripts unknown to the hub are imported in full, as before; the list does
not affect files already read.

Import is the mirror's first pass over a file (offset 0): the worker sends the whole history of
the main chain as §4 events through `mirror.event`, with the original `ts` and with `event_id`
per §3 Event (`Event::id_from_claude_block`). Mapping: `user` with text → `user.message`
(`delivery: delivered`), `tool_result` → `tool.result`; `assistant` with text →
`agent.message`, `final: true` when `stop_reason: end_turn`, `in_reply_to` is the file's last
prompt; `tool_use` → `tool.call`; non-empty `thinking` → `agent.thinking`. Records that yield no
events (`attachment`, snapshots, `api-request*`, `queue-operation`, empty `thinking`, images)
are skipped.

For an unknown `runtime_session_id` of this worker the hub creates a session in the
workspace's project: `cwd` is the record's path relative to the workspace, the title is the
`ai-title` (the frame's `title` field) if present, otherwise "Session N" (§3 Session),
`system session_created` "Session imported from Claude Code". The initial `locked_by` follows
the frame's `recent` flag: the worker sets it if the file was modified less than 30 s before
it was read (the worker's clock, not the hub's); then `vscode`, otherwise `none`. An author the
worker does not know is filled in by the hub: the workspace owner for `user.message`, the
session's agent for the rest. No push is sent for mirror events (an import is hundreds of old
replies).

**Import limits** (§20, since 0.23 T22; behaviour at the daily limit since 0.24). New sessions
from the mirror: at most 200 per 24 h per worker and 2000 in total per workspace;
`runtime_session_id` is a uuid, `cwd` is up to 4096 bytes. The first wave after the update to
0.23 (migration `0011` turned the mirror on for every workspace) may bring more transcripts than
fit in a day: the hub does not accept a record of a new transcript over the daily limit:
`error mirror_quota`, `retryable: true`. On such a reply the worker removes the record from the
outbox (the §5.2 attempt counter does not grow) and rewinds the transcript to the start: the file
is not read until an **hour** has passed since the last rejection, then it goes again in full
with the same `event_id`s. So the wave spreads over several days (200 sessions per 24 h) without
losing anything; what is deferred lives in the worker's memory, and after a worker restart such
a transcript is read at once (and deferred again if the limit is used up). Over 2000 per
workspace and with an invalid `runtime_session_id`/`cwd` the record is dropped (`ack` without
`seq`, warn), as in §20.

### 9.2 Mirror
Since 0.23 the mirror is on for every workspace by default **[decision]** (T21): otherwise a
session started in hqc and continued in VS Code is not released, and forks are not visible
(questions of T15 and T20). For a workspace with `mirror_enabled` the worker watches every file
`<config>/projects/*/<uuid>.jsonl` whose `cwd` is inside the workspace path (compared by path
components). Polling every **2 s**, not file system notifications **[decision]**: a pass is a
`read_dir` of the projects folder and a `stat` of every file, and only the bytes after the
offset are read from disk; notifications would not always be enough (FSEvents coalesces events
and reports directories, inotify needs a watch per folder and does not see what was created
before the watch was set), offsets and rescans are needed anyway, and 2 s is much less than the
30 s that matter for takeover. The offset advances only over processed complete lines; an
incomplete line waits for the next pass; a file that got shorter is read again (`event_id`
absorbs duplicates). Offsets, the file's last prompt and `ai-title` are stored next to the `cwd`
cache (`$HQC_HOME/mirror/mirror.sqlite3`); `mirror.event` goes through the outbox, like `event`.

The worker recognises its own records by `uuid`: the Claude Code adapter puts into a shared
in-memory registry (by `runtime_session_id`) the uuid of every `user`/`assistant` message of the
main chain from stream-json, including the `--replay-user-messages` echo of its own prompts, and
marks whether the session's process is alive. Since 0.26 (T25) the adapter also puts there the
text of every input written to the process's stdin; a prompt record (a main-chain `user` with
text) whose `uuid` stream-json did not name is its own if its typed text matches an input not
yet matched (the input is consumed; the last 32 are kept). The typed text is the first text
block, and for a slash command, which the CLI writes as
`<command-name>/compact</command-name>…<command-args>…</command-args>`, the command with its
arguments. This makes own records of the `/compact` command record (it is not in stream-json,
2.1.308) and of the prompt if its echo comes with a different `uuid` (see §9.3). The mirror
**never emits** own records (the adapter has sent them, including the prompt, as a hub
`user.message` with its own `event_id`). The rule for foreign records:

- the hqc process of this session is alive (`locked_by: hqc`): the mirror **does not emit** and
  does not move the offset past the first foreign record while it stays foreign for less than
  **5 s** (stream-json may lag behind the file). After that: on a worker with the holder
  registry (§9.3) the record is emitted like any other, with `held_by` (the hub does not lock
  the session because of it); only a holder releases the session; without the registry this is
  a foreign writer (§9.3);
- there is no process (`locked_by: vscode` or `none`): the record is emitted at once through
  `mirror.event`; with `recent` the hub moves the session to `locked_by: vscode` (if it was
  `hqc`, this is a release, §9.3).
- Since the adapter and the mirror derive `event_id` from the same record `uuid`, the seam
  when `locked_by` switches either way is absorbed by the unique index.
  For a record with several content blocks the §3 Event rule is mandatory for the
  mirror too: block `i > 0` is v5 from `"<uuid>/<i>"`. The CLI has so far written one block per
  record (all fixtures and live runs, including 2.1.308), but otherwise the seam would produce
  duplicates.

A session with `locked_by: vscode` is read-only in the hub: `state: locked_vscode`, the input
field is closed. The client has two banners: with `held_by`, "The session is open in VS Code …
close it there" without a button (takeover would get `409 vscode_open` anyway); without
`held_by` (recent records, no holder or no registry data), "In VS Code" and a "Take over"
button.

### 9.3 Takeover **[decision]**
The T15 rule "30 s of silence in the file" does not tell a closed session from an open idle
one. In practice (2026-10-23) hqc took over a session open in VS Code after 30 s of silence and
appended turns through `--resume`; then the user wrote in VS Code, and the extension attached
its turn to its own last record in memory, not to the tail of the file: the record had two
children, VS Code saw only its own branch, and 37 hqc records never made it into its context.
Since 0.22 (T20) takeover is decided by the transcript's **holder**, and forks, if they still
happen, are visible in the feed.

**Holder.** Claude Code (2.1.308) keeps a registry of live sessions
`<config>/sessions/<pid>.json` (`<config>` as in §9): `pid` (a number; it has also been seen as
a string), `sessionId`, `cwd`, `entrypoint` (`claude-vscode` for the VS Code extension,
`sdk-cli` for `claude -p` and the adapter's processes, there are others), `kind`, `status`
(`idle` | `busy`), `updatedAt` (ms), `version` and other fields. Next to them lie
`<pid>.<hash>.key` files; the worker does not read them. A process that crashed without cleaning
up leaves its file behind, so a record is live only if process `pid` exists (`kill -0`). The
worker reads the directory once per mirror pass (2 s; holders for all transcripts, not only
those under the mirror), excludes its own processes (agent pids and their process groups: the
adapter starts the CLI as a group leader) as well as the `sdk-cli` record of a transcript its
own process is currently driving (in case of a wrapper with a different pid), and skips unknown
fields and broken files. The remaining records are holders; of several for one transcript, the
`busy` one is taken, then the most recent. The worker reports whether the registry exists in
`hello` (`capabilities.claude_code.sessions_registry: true` if the directory exists, §5.4).

- The worker reports a change of holder (appeared, disappeared, changed `status`) to the hub
  with a holder report: `session.state` with a nil `session_id`, the transcript's
  `runtime_session_id` and `held_by: {entrypoint, status} | null` (§5.4), at most once per 2 s
  per transcript, through the outbox, in the same order as `mirror.event` (a direct frame would
  overtake events in the outbox carrying the old `held_by`; this is how the first live run of
  T20 failed). Every `mirror.event` carries the holder the worker saw when reading. On `hello`
  the hub forgets the holders of the worker's sessions (it does not touch `locked_by`); after
  `hello.ack` the worker reports the current ones again.
- A session with a holder is `locked_by: vscode` regardless of writes to the file (including
  the import of an open idle session). The holder disappeared: `locked_by: none` at once,
  without the 30 s, if there is no hqc process for the session (`process_state: stopped`);
  otherwise it stays `vscode`. With registry data (`held_by` in `mirror.event`), `recent` does
  not lock the session: a record without a holder was written by a process that has already
  exited.
- A holder appeared while hqc is driving the session: the hub sets `locked_by: vscode` at once
  and writes `system session_released` with `reason: vscode_open` ("The session was opened in
  VS Code: hqc stopped the agent, interrupting a turn in progress. To drive it from here, close
  it in VS Code and take it over again."), `pending` inputs become `failed`; the worker stops
  the agent at once, a turn in progress included, and clears its input queue, as on a foreign
  write below. The other process continues the transcript from what it read when it opened
  it: whatever hqc's agent did after that would land on a branch nobody continues, and its
  tools would change files behind the other agent's back. VS Code 2.1.310 does not start its
  process for a session another process holds busy: it waits and offers "take over"
  (`open_held_session` in its log), so a holder appearing mid-turn is a deliberate takeover;
  older versions start it on opening. The hub cancels the open requests of the stopped
  process and closes the turn with `turn_lost` (§17.3).
- The "take over" button (`POST /api/sessions/{id}/takeover`). A live holder: `409 vscode_open`
  with `message` "the session is open in VS Code: close it there and take it over again" and a
  `held_by` field. No holder: on a worker with the registry the takeover goes through at once;
  without registry data (old CLI, worker without the registry) a fallback rule applies: with
  `locked_by: vscode`, 30 s of silence are needed since the hub received the last
  `mirror.event` with `recent` (the `vscode_active_at` field); earlier, `409 vscode_active`.
  Takeover sets `locked_by: hqc`, writes `system session_taken_over` ("hqc took over the
  session. If it is open in VS Code, its context there is stale: reopen the session in VS Code
  after you switch back.") and sends the worker `session.start` with `mode: resume` and
  `runtime_session_id`, the file's uuid; since the file exists, the adapter uses `--resume`
  (§8.1). Worker offline: `409 worker_offline`; archived: `409 archived`. For `locked_by: none`
  takeover also goes through; for `hqc` it does nothing. The response is the session object.
- A message to a session with `locked_by: vscode` is **rejected** with `409 locked_vscode` and
  a `hint: "takeover"` field (and `held_by` if the holder is alive; the client offers "Take
  over" or closing the session in VS Code); there is no silent queue. To an input for a
  released session the worker replies `error {code: locked_vscode, retryable: false}`.
- Detecting a foreign writer by record `uuid` is **only for a worker without the registry**
  (the `sessions` directory does not exist or cannot be read) **[decision]** (T25). On a worker
  with the registry, a session driven by an hqc process is released only by the appearance of
  a foreign live holder (the item above), and records with an unknown `uuid` are emitted after
  5 s with `held_by` (§9.2). The reason is the case of 2026-10-24 (worker with the registry, no
  holder): `/compact` from hqc wrote to jsonl the command `<command-name>/compact</command-name>`,
  which is not in stream-json, and before that the prompt of the first input after `--resume`
  had turned out foreign; the mirror released the session twice in the middle of work. On
  2.1.308 the echo after `--resume` carries the record's `uuid` (fixture `resume-compact`, test
  `t25_resume_prompt_and_compact`); why it differs on the user's machine is not known:
  **[verify]** the CLI version there; for this case an own prompt is also recognised by the
  input text (§9.2). The cost: a writer the registry did not show (a process shorter than a
  pass, `claude -p --resume` from a terminal while hqc is driving a turn, whose `sdk-cli` record
  the worker excludes) does not release the session while hqc is working; its records are
  visible in the feed, and a fork, if there is one, will be shown by the mirror. When there is
  no hqc process, the rules are the same as below. Without the registry the following applies:
  only `type: user` and `type: assistant` records of the main chain count
  (`isSidechain: false`, not `isMeta`) that yield at least one event (empty `thinking` does
  not count, §9). A record with a `uuid` the adapter has not seen in stream-json (and that the
  adapter did not recognise by the text of its input, §9.2), while an hqc process is working,
  means that the session was continued in VS Code: the worker stops its agent at once, a turn
  in progress included (since 0.30, for the reason above), the worker's input queue is cleared,
  `session.state` carries `locked_by: vscode`; the hub writes `system` with code
  `session_released` and `reason: foreign_write` ("The session was continued in VS Code: hqc
  stopped the agent, interrupting a turn in progress…"), and `pending` inputs become `failed`. Then the mirror
  sends the held-back records. While no hqc process is running, any such record with `recent`
  and without registry data moves the session to `locked_by: vscode` (from `hqc`, also with
  `session_released`). The hub applies `locked_by` from `session.state` as follows: the
  worker's `vscode` always wins; the hub's `vscode` is not cleared by a process stop (`none`)
  or by a process the hub did not start through a takeover (`hqc`); otherwise as the worker
  reported. Verified on 2.1.307 with the T0 fixtures (test
  `t0_stream_and_transcript_share_uuids`) and on 2.1.308 with the T15 fixtures
  (`t15_two_writers_share_one_transcript`, mirror tests `own_records_are_not_foreign`,
  `outside_writer_is_detected`, live `live_vscode_import_takeover_release`): the uuids of all
  main-chain `user`/`assistant` records, including `tool_result` and the prompt echo, match in
  stream-json and jsonl; with `--resume` stream-json does not repeat the old history. The
  exception is slash commands (T25, 2.1.308, fixture `resume-compact`): of the `/compact`
  records in the main chain, only the `<command-name>` command does not reach stream-json (the
  command's output arrives as an echo, the summary as `isCompactSummary`, the explanation as
  `isMeta`). The T25 rule is verified by the mirror tests
  `registry_unknown_record_without_holder_keeps_the_session`,
  `resume_prompt_without_its_echo_uuid`, `no_registry_keeps_the_uuid_check`.
- **Fork.** A writer that does not continue from the end of the file (VS Code, from its own
  last record in memory after hqc's turns; `claude --resume <uuid> --resume-session-at <record
  uuid>`) starts a second line of turns from an earlier record in the same file; the turns of
  the other line do not get into its context. The mirror keeps the transcript's record tree
  (`uuid` → `parentUuid` of all records with a `uuid`, including `attachment`, except
  `isSidechain`) and the number of prompts (records that yield a `user.message`) under each
  record. Two children by themselves are not a fork: parallel tool calls produce them as
  `tool_use` records (fixtures `auto-mode`, `auto-classifier`). A fork is a prompt above which
  some record has another child with prompts; the point `at_uuid` is the nearest such record,
  `orphaned_turns` is the prompts of its other children, `branch_from` is `hqc` if the new
  prompt is own (the adapter's uuid registry, §9.2), otherwise `vscode`. Before the events of
  the new prompt the worker sends through `mirror.event` a `system session_forked` (`author:
  {kind: system}`, payload `{text, code, at_uuid, branch_from, orphaned_turns}`, `event_id` is
  v5 from the string `"<at_uuid>/fork"`, one event per point) with the text "VS Code continued
  from an earlier point: N turn(s) from hqc are not in its context" (or "hqc continued from an
  earlier point: N turn(s) from VS Code are not in its context"). A record enters the tree once
  the mirror has passed it (a held-back foreign record may turn out to be own). After a restart
  the worker rebuilds the tree from the already-read part of the file and does not repeat old
  forks. The hub stores the event as an ordinary `system` event; the client draws it as a
  warning line. Only for workspaces under the mirror. Verified on the fixture `fork-2.1.308`
  (live run of T20 without VS Code, mirror test `fork_is_reported_once`).
- At any moment exactly one of the two drives the session.

### 9.4 Parser
The jsonl format is unofficial and changes between versions. The parser tolerates unknown
record types, logs them and does not fail. One parser (`hqc-claude-jsonl`) serves import, the
mirror and the adapter's title: `Message::is_main`, `stop_reason`, `blocks`, `first_cwd`,
`last_ai_title`. It is covered by fixtures from real files (one set per CLI version).

## 10. Permissions

### 10.1 In a session
`approval.request` is a first-class event with buttons in the client. The client shows **the
options from the request's `options[]`**, labelled by `canonical`; for Claude Code these are
once / session / always / deny, for ACP agents whatever the agent offered. The request goes to
**the worker owner**; a collaborator with `write` sees the request but cannot answer it.
The first answer wins: it closes the request, every device sees the `approval.response`
event, and the client removes the buttons. A second answer (from another device) gets
`409 request_closed`; no cancel event is written in that case. Expiry, `POST /stop`, a worker
restart and archiving produce `approval.cancel`, and the client removes the buttons.
All these cancellations are written by the hub: on expiry, `stop` and archiving it writes the
`approval.cancel` event and sends the worker an `approval.response` frame with the
`canonical: deny` option, so the adapter closes the request in the runtime (§5.3); on a worker
restart, only the event (§5.2). On `POST /stop` the hub first cancels open requests with
`stopped`, then sends `session.stop`; the adapter and the worker's supervisor do not emit a
cancel on `session.stop` (the Claude Code adapter silently forgets requests still open at the
end of an interrupted turn). The hub cancels an open `clarify` the same way: a
`clarify.cancel` event and a `clarify.response` frame with `answers: {}`; the adapter answers
the runtime `deny`, and the turn continues without answers.
**Where requests are shown** (since 0.41, T37). A request is shown and answered not only in its
own feed:
- In every session that waits on its session through an open `hqc_ask` (§7.1), directly or
  along a chain (A asks B, B asks C: C's request shows in B and in A), under that session's
  "Waiting for a reply from …" line. The card is the same component as in the request's own
  feed (permission card, text reply, clarify form), framed and headed by the session it
  belongs to (title, address, link) and its worker, so it cannot be taken for a request of
  the session the user is looking at; Enter and Esc do not answer it. Other sessions waiting
  on it are named under the header.
- In the "Requests" list: every open request of the user (`GET /api/requests`, the `requests`
  frame, §6), the workers' own requests (§10.2) included; on a desktop an entry with the count
  in the left column (shown while something is open), on a phone a "Requests" button with the
  count in the session list's header; both open `#/requests`. Each item is the same card with
  the same header and the sessions waiting on it, answers in place and links to its session.

An answer from any of these places is the ordinary answer of the request's own session (`POST
/api/sessions/{session}/requests/{request_id}`, §6): the `approval.response` /
`clarify.response` event, its audit (`by`) and the frame to the worker are those of that
session, the worker's permission policy (§14) is untouched, and nothing is answered on the
user's behalf. Once a request is answered or cancelled anywhere (its feed, another session's
feed, the list, another device, expiry, stop, archive), the hub sends the new list and every
place drops the card at once; an answer that loses the race gets `409 request_closed`, and the
client drops the card too.

**Reply with text** (T26). Instead of a button the user can write what to do instead of the
call: the client answers with the `canonical: deny` option (if there is none, `deny_always`)
and a `message` field; the hub puts `message` into the `approval.response` event and into the
`approval.response` frame to the worker (§5.3), and the client shows it in the feed as a user
message. The Claude Code adapter answers with a `control_response` `deny` whose `message` is
Claude Code's own wording for a denial with an explanation ("The user doesn't want to proceed
with this tool use. … To tell you how to proceed, the user said:") followed by the user's text
on a new line; the model gets it as the tool result and continues the turn. Verified by live
runs on 2.1.308 (`claude-haiku-5-5`): with this wording the model does what is written; with a
plain "The user denied this tool call and wrote instead:" it saw the text but took it for an
instruction from a tool result and did not follow it. This is input into the current turn, not
a new `user.message`: it has no queue and no `delivery`. For ACP agents (§8.2), later; worker
requests (§10.2) and `clarify` have no such field (`400`).

With `auto_approve` the hub answers with the first option with `canonical: once`, `by: auto`
(calls under `deny` in the worker's `[permissions]` never reach the hub, §8.1, §14);
only the worker owner can turn the mode on, and it is highlighted in the UI; on a shared
project it means collaborators run code without confirmation, and the UI warns about this when
the mode is turned on and when a `write` grant is given. A push notification for every
request is mandatory (§16).

### 10.2 Outside a session
Every worker has a service session (`system_session_id`, §3 Worker), visible only to the
owner. `worker.approval.request` frames arrive there as `approval.request` events, and the
worker itself writes its logs of level warn and above there. Bindings of environments and
profiles are never confirmed through the hub at all: they change only locally (§14).

**Service session** (since 0.17). It lives in the worker owner's personal project: `owner_id`
is the owner, `worker_id` is the worker, no workspace, `title` is the worker's name
(`title_source: user`), `slug` is `worker-<worker_id>`, there is no agent (`runtime: none`, the
process never starts, `session.start` is never sent for it). The session object (§6) has the
field `system: true` (only `true` is ever sent); service sessions are not counted in the
"Session N" numbering (§3 Session). `state` is as for an ordinary session (§17.1):
`worker_offline` while the worker is not connected, `waiting` while a worker request is open,
otherwise `idle`. Input, `POST /stop`, `/retry`, `/takeover`, `/archive` and `DELETE` for it
give `409 system_session`; `PATCH auto_approve: true` gives `403`. The client shows service
sessions not in the session list of the personal project but as a separate "Workers" group in
the left column (worker name, "waiting for reply", "offline", unread); the feed is ordinary,
without the input field and the "Stop"/"Retry" buttons, with a "requests only" filter
(`approval.request` and `clarify.request` cards).

**Revocation, replacement, deletion** (since 0.21). Revoking a worker
(`DELETE /api/workers/{id}`, §5.1) writes `system worker_revoked` into its service session, and
the hub archives the session itself (the `/archive` ban applies only to users); its open
requests are closed with `approval.cancel {archived}` without a frame to the worker: the
process that asked is cut off. The "Workers" group shows only non-archived service sessions,
that is, workers that are not revoked; service sessions of workers revoked before 0.21 are
archived by hub migration `0009`. Enrollment with replacement (§5.1) unarchives the service
session, renames it to the new name and writes `system worker_replaced`; deleting a revoked
worker (§6) deletes it (§17.4).

**Frames** (§5.3, §5.4; added in 0.17, same protocol version):
- worker → hub `worker.approval.request {request_id, kind, description, expires_at}` → `ack
  {}`. `kind`: `workspace.use` (below), `workspace.create`, `workspace.delete` (iteration 5,
  §13); the list is open. The hub writes into the service session an event `approval.request
  {request_id, tool: <kind>, description, options: [{id: once, canonical: once}, {id: deny,
  canonical: deny}], expires_at}` with `author: {kind: system}` and opens a `PendingRequest`
  with `kind: worker_approval` and the worker's `worker_id`; `description` is stored up to 2000
  characters. A repeated frame with the same `request_id` (the worker sends it again after
  every reconnect while it waits) writes no new event; if there is already an answer, the hub
  sends it again after the `ack`. Repeats are checked before the limits. A new request with
  `expires_at` in the past or more than 24 h after the hub's time (since 0.23 rejected rather
  than cut to a day; there is no allowance for clock skew: the worker asks for 10 minutes),
  with `description` over 4096 bytes, a `request_id` or `kind` of the wrong length, or over 20
  open requests per worker gets `error invalid_frame`, and nothing is written (table §20).
  Push for such requests: at most once per 10 s per worker (the request is written, the
  notification is skipped). Open worker requests are also in the user's "Requests" list
  (§10.1, since 0.41) and answered there in place.
- hub → worker `worker.approval.response {request_id, option_id}`: the owner's answer, or
  `deny` when the hub cancels; the worker replies `ack` even when it no longer has the
  request. The answer is kept in `PendingRequest.result` until the `ack`; an answer the worker
  has not acknowledged is resent by the hub after `hello` (no `session.start` precedes it).
- `hello.ack` gained an optional `system_session_id`, and `session.start` gained
  `requested_by_owner` (only `true` is ever sent): the hub sets it if the session's workspace
  was added by the worker owner.

**Answer and cancellation** work as for `approval` in a session (§10.1), but only the worker
owner can answer (`POST /api/sessions/{system_session_id}/requests/{request_id}` with
`option_id`, otherwise `403`); the first answer closes the request, the second gets
`409 request_closed`. Expiry of `expires_at`: an `approval.cancel {timeout}` event and a
`worker.approval.response` frame with `deny`. A new worker `boot_id` (§5.2):
`approval.cancel {worker_restart}` without a frame, since the process that was waiting no
longer exists. Push as for any `approval.request` (§16).

**New folder** (`workspace.use`, since 0.17; the only use until iteration 5). The worker keeps
a cache of folders in which it has agreed to run sessions: `$HQC_HOME/known_workspaces.json`
(an array of canonical paths, mode 0600, written through a temporary file). A `session.start`
with a `workspace_path` whose canonical path is not in the cache, for a session with
`owner_is_worker_owner: false` and without `requested_by_owner`, is not executed by the worker
right away: it sends `worker.approval.request {kind: workspace.use}` with the description "the
hub wants to run sessions in <path>" and `expires_at` 10 minutes ahead, and defers the `ack` to
`session.start`. `once`: the folder is written to the cache, and the waiting `session.start`
frames run as usual; `deny`, expiry at the hub, or the worker's own wait for `expires_at` +
30 s without an answer: `error {invalid_frame, retryable: false}` for every waiting
`session.start` (the hub marks the `pending` input `failed` with `system agent_start_failed`,
§5.2). Several `session.start` frames for one folder wait for one request. Sessions of the
worker owner and sessions in a workspace added by the owner need no confirmation, and their
folder also goes into the cache. The flags `owner_is_worker_owner` and `requested_by_owner` are
the hub's statement about the users' identity, which the worker trusts (§2.1); the hub cannot
change the cache. So another user cannot start a session in a new folder without the consent of
the machine's owner (audit, A2, in part).

**Worker logs.** The worker's `tracing` events of level `WARN` and `ERROR` (except lines of the
forwarder itself) go to the service session as an event `system {text, code: worker_log,
level: warn|error}` with `author: {kind: system}`, through the outbox, like other events
(§5.2); `text` is `<target>: <message> <fields>`, up to 2000 characters. The worker learns the
session from `hello.ack`; lines before the first `hello.ack` wait in memory (up to 60). Rate:
the same text at most once per second, at most 60 lines per minute in total (a 60 s window
from the first line); what is dropped is counted and reported with one line `skipped N`
(`level: warn`) as soon as the limit allows; that line counts toward the same 60. `worker_log`
lines are not counted as unread (an exception to the §6 rule), and there is no push for them.
The client shows them as a monospace line with the time (to the second), the level and the text
as is, without Markdown.

## 11. Access and sharing

The Google Docs model: everything is private, a Grant gives access. The function
`can(user, action, resource)` guards every request to the hub from day one.

| action | view | write | manage | project owner |
|---|---|---|---|---|
| read history, artifacts | yes | yes | yes | yes |
| write to a session, attach files | | yes | yes | yes |
| create sessions in the project | | yes | yes | yes |
| stop a turn, steer | | yes | yes | yes |
| answer `clarify` | | yes | yes | yes |
| `hqc_send`/`hqc_ask` to a session (through one's own agent) | | yes | yes | yes |
| `hqc_schedule` to a session | | yes | yes | yes |
| answer `approval` | | | | worker owner only |
| rename, archive, unarchive, delete a session | | | yes | yes |
| create/delete a workspace | | | yes (with the worker owner's confirmation) | yes |
| give grants | | | yes | yes |
| delete the project | | | | yes |
| change a workspace's environment and profile | | | | worker owner only, locally |

- A grant on a project covers all its sessions; a grant on a session covers only that session.
- Workers are not shared. A collaborator gets access to a machine only through a workspace of a
  shared project, inside its path.
- The agent acts with the session owner's rights.
- Revoking a grant: the current turn is not interrupted, the next input is rejected, the
  client's subscription is closed.
- A personal project is not shared. Several agents of different users in one
  session are forbidden.
- There are no organisations or roles.

### 11.1 Who pays, and foreign sessions **[decision]**
- **All runtimes on a worker** (Claude Code, Hermes, any ACP agent) work
  with the machine's local credentials, that is, **the worker owner** pays, whoever
  created the session. A collaborator's keys must not be passed to someone else's machine: the
  machine's owner would see them.
- **Bare model**: the session owner's (`owner_id`) keys in the hub.
- Sharing a project with a workspace on a worker means "I am lending my credentials";
  when a `write`/`manage` grant is given, the UI says so explicitly. The worker owner can
  forbid foreign sessions with the flag `allow_foreign_sessions: false` (local config,
  visible to the hub through `hello`); the flag does not forbid foreign input into the owner's
  own sessions; that is covered by the warning when a grant is given.
- A foreign session (`owner_is_worker_owner: false`) on an ACP agent with `isolation: none`
  is rejected by the worker. A foreign session on Hermes gets the profile set
  in the binding as `foreign_profile`; if there is none, it is rejected: otherwise the
  collaborator would get read and write access to the owner's long-term memory (Hindsight).
- The `usage` event is written where the runtime provides it; for ACP agents without usage
  the session's spend is shown in the UI as "no data".

## 12. Artifacts

Storage: the hub's disk (`HQC_ARTIFACT_DIR`, file `<dir>/<sha256[0..2]>/<sha256>`; S3 —
later), keyed by sha256, one content per hash. Kinds: file, image, diff, html, text.
Artifact limit 100 MB (the stream is written to a temporary file while sha256 is computed;
exceeding it gives `413 too_large` and the file is deleted). Record: §3 Artifact; uploading
the same file again by the same user under the same name returns the existing record.

`kind` on upload: the client's explicit `kind` field; otherwise from the multipart type (empty
and `application/octet-stream` are guessed from the extension): `image/*` → image; `.diff`,
`.patch`, `text/x-diff`, `text/x-patch` → diff; `text/html` → html; other `text/*` → text;
anything else → file.

Links. Events store only `artifact_id`; when rendering, the client requests
`GET /api/artifacts/{id}/url` and gets `{url, expires_at}`: `<base>/artifacts/{id}?exp=<unix
s>&sig=<base64url HMAC-SHA256 of "{id}.{exp}">`, TTL 10 minutes. Base:
`HQC_ARTIFACT_PUBLIC_BASE` (a public domain `artifacts.<hub-domain>` pointing at the same hub,
outside Cloudflare Access); if unset, the link is relative, on the hub's domain (behind Access).
Key: `HQC_ARTIFACT_KEY`, or a random one per start. `GET /artifacts/{id}` without a signature
or with a wrong one gives `403`, an expired one `410`; on `403`/`410` the client requests the
link again (for `<img>`, once, on `onerror`). Only the artifact's owner gets a link (`404` for
everyone else); the link itself is the access right, no cookies needed.

Response headers (both via the link and to the worker): `Content-Security-Policy: sandbox;
default-src 'none'; img-src data:; style-src 'unsafe-inline'; frame-ancestors 'none'`,
`X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`,
`Cache-Control: private, max-age=600` (to the worker `private, no-store`), via the link also
`Access-Control-Allow-Origin: *` and `Cross-Origin-Resource-Policy: cross-origin` (the client
reads diffs and text via `fetch`). `Content-Type` by `kind`: image — `mime` if it is
`image/*`; text and diff — `text/plain; charset=utf-8`; html — `text/html`; file —
`application/octet-stream`. `Content-Disposition` (`filename` and `filename*=UTF-8''…`):
image, text, diff — `inline`, file — `attachment`, `&dl=1` — always `attachment`. HTML is
`inline` only if the request came to the `HQC_ARTIFACT_PUBLIC_BASE` host: then the CSP is
`sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; style-src
'unsafe-inline'; img-src data:; font-src data:` without `X-Frame-Options`, and the client
renders it in an iframe `sandbox="allow-scripts"` without `allow-same-origin`: the page makes
no network requests and sees neither cookies nor the hub's API. On the hub's domain HTML is
always `attachment` with a script-less `sandbox` CSP, and the client shows an "Open" link. On
the `HQC_ARTIFACT_PUBLIC_BASE` host the hub serves only `/artifacts/*` (and `/health`), anything
else is `404`; the hub's own CSP (§20) adds this origin to `img-src`, `frame-src` and
`connect-src`.

The worker downloads artifacts via `GET /worker/artifacts/{id}` with its token (§5.3), no TTL:
only artifacts that came in the input of a session of this worker (others get `404`); the name
and `kind` are in the headers `x-hqc-artifact-name` (percent-encoded) and `x-hqc-artifact-kind`.

Client. User → agent: files are attached to a message (paperclip, drag and drop, paste;
several; image previews as `data:` URLs); first each goes through `POST /api/artifacts`, then
the input with `artifact_ids` (the text may be empty); retrying a failed send does not upload
what is already uploaded. The worker puts them into `.hqc/inbox/` and substitutes the paths
(§8.1); for runtimes with vision, images are also passed as images. In the feed, attachments
of `user.message` and `attachment` events render the same way: image — inline, diff — a code
block with added and removed lines highlighted (up to 512 KB, larger as a card), html — an
iframe from the artifacts domain or an "Open" link, text and file — a card with the name,
size and "Open" (except file) and "Download" buttons. Agent → user: `hqc_attach` (§7, since
0.34): the worker uploads the file with `PUT /worker/artifacts` (§5.4) under the worker token
for one of its sessions; the owner is the session's owner, dedup and `kind` as for the client's
upload; the artifact is linked to the session in `artifact_sessions`, which is what the tool
checks (and the session's worker may download it back). The `attachment` event renders like a
user's attachment, standing in the feed after the tool group of its call; diffs of ACP agents
are saved as artifacts by the adapter itself (§8.2).

**Cleanup** (since 0.23, T21). Once a day (the first time right after start) the hub deletes
artifact records that no event references (`user.message.artifact_ids`, `artifact_id` of
`attachment` and `tool.result`) and that are older than `HQC_ARTIFACT_RETENTION_DAYS` (default
30, `0` means never delete) by `updated_at`, the last upload of the same file or input with it.
The file on disk is deleted only when no records are left for its sha256: first it is moved
aside, then the hub checks the records again and, if one has appeared (an upload of the same
file at that moment), puts it back; an upload, in turn, puts the file in place after writing the
row if it is missing. `artifact_sessions` records go together with the artifact. There are no
per-user quotas.

## 13. Workspace lifecycle

### 13.1 Creation
`dir` (an existing folder), `clone` (git clone into a new folder), `worktree` (a git worktree
of an existing workspace on a branch). Only inside `allowed_roots`, with confirmation by the
worker owner through the service session (§10.2), unless the owner is the one who
initiated it.
Since 0.16 `dir` is implemented (`POST /api/workspaces`, §6): the folder is picked in the client
from the `fs.list_dirs` tree or its path is typed in, the worker checks it with `fs.stat` (§5.3),
the hub stores the canonical path. Only the worker's owner can add a workspace to a worker (until
the sharing of iteration 6 a worker has no other users), so there is no confirmation through the
service session yet; if the worker is offline, the workspace is not created. `clone` and
`worktree`: iteration 5.

### 13.2 Deletion
`dir`: only the hub's record is deleted, the folder is left alone; while the workspace has
non-archived sessions — `409 workspace_in_use` (§6), archived ones stay without a workspace. `clone` and `worktree`
are deleted from disk only with `created_by_hqc: true`: `worktree` via
`git worktree remove`, `clone` by deleting the folder. Before deletion the worker checks for
uncommitted changes and unpushed commits; if there are any, the confirmation request
contains a warning. An `ephemeral` workspace is **not deleted automatically** when its last
session is archived; it is marked "for deletion" and shows a button;
deletion is always confirmed.

### 13.3 Project session template
An optional project setting: `{workspace_id, cwd_pattern: "<slug>", prompt}`.
A new session from the template creates the subfolder `<workspace>/<slug>` and starts with the
starting prompt. The workspace in the template is required. Meant for projects like cad and
KiCad, where a session corresponds to a part or a board.

### 13.4 Choosing a workspace
When a session is created from the UI, the workspace is chosen explicitly (the form preselects
the project's `default_workspace_id`, `cwd` is an optional subfolder); `hqc_spawn` and cron require
`workspace` or use the project's `default_workspace_id`; with neither, it is
an error (`409 no_workspace` in REST, §6; `invalid_args` from `hqc_spawn`, since 0.37).

## 14. Local config and worker environments

The worker's local config is the only source of truth for:

- `allowed_roots[]` — the only thing the worker knows about workspaces: every workspace from
  `session.start`, every `fs.*` path and every file of an agent's `hqc_attach` (§7, also
  inside the session's workspace) must lie inside (§5.3). Since 0.16 `worker.toml` has no
  `[workspace]` block: workspaces are created in the hub (§3, §13.1); the iteration 1 block in an
  old file is ignored with a warning in the log at start;
- `environments{}` — named environments;
- `env_bindings[]` — records `{path, environment, hermes_profile?, foreign_profile?}`.
  A binding is looked up **by the workspace path** (not by the session's `cwd`); the longest
  match wins, otherwise `default`. `environment` from `session.start` is only checked against it;
- `acp_agents{}` — `{name: {command: <absolute path>, args[], isolation:
  verified|none}}`;
- `allow_foreign_sessions`, `share_local_models`;
- `max_sessions` (agent processes at once, default 8) and `insecure_http`
  (`http://` to a non-loopback hub, §5.1);
- `[permissions]` — the Claude Code permission policy (below; not sent in `hello`
  yet).

The hub gets a copy in `hello` and shows it; only the worker owner can change it, on the
machine (`hqc-worker env ...`).

Permission policy `[permissions]` (in effect since 0.14 for all of the worker's Claude Code
sessions; environments of iteration 5 will be able to extend it via `settings`):

```toml
[permissions]
mode = "ask"     # "ask" | "auto"
allow = ["Bash(cargo test:*)", "Read"]   # run without asking
deny = ["Bash(sudo:*)", "Bash(git push:*)"]  # never run and never ask
```

- `mode = "ask"` (default): calls outside both lists are asked about (`approval.request`);
  `mode = "auto"`: `--permission-mode auto`, the CLI decides by itself and never asks (§8.1),
  the only boundary is `deny`; the worker writes a warning to the log at start;
- `allow` is empty by default;
- `deny`, if the field (or the block) is absent: `Bash(sudo:*)`, `Bash(ssh:*)`, `Bash(scp:*)`,
  `Bash(rm -rf /*)`, `Bash(curl * | sh)`, `Bash(curl * | bash)`, `Bash(dd:*)`, `Bash(mkfs*)`,
  `Bash(git push --force:*)`, `Bash(git push -f:*)`; `deny = []` turns it off
  **[decision]**;
- a rule is `Tool` or `Tool(content)`: the name of Latin letters, digits and `_`
  (`mcp__hqc`, a trailing `*` is allowed), non-empty content in a single pair of parentheses at
  the end of the rule, at most 256 bytes, no control characters. An invalid rule or an unknown
  field is a config read error: in `-p` the CLI silently ignores settings that failed its
  validation, and one bad rule would drop the whole `deny`.

Three levels, from strongest to weakest: the worker's `deny` (not run, not asked about, `always`
and `auto` do not override it) → the worker's `allow` and `always` rules from
`settings.local.json` (run without asking) → everything else (the owner is asked in `ask`, the
CLI's classifier decides in `auto`).

An environment defines what the agent process is launched with (Claude Code or an ACP process,
both launched by the worker):

- `ssh_keys`: keys handed out through a separate ssh-agent socket to this process only;
  by default there is no agent;
- `env`: variables and tokens (GitHub etc.);
- `settings`: additional Claude Code settings via `--settings` (for example,
  `permissions.deny` for `Bash(ssh*)`, `Bash(scp*)`); **Claude Code only**,
  ACP agents have no equivalent, so for them the `default` environment is weaker: the only
  boundary is the absence of keys;
- `allow_local_mcp`: allow MCP servers from the machine (Claude Code: without
  `--strict-mcp-config`; Hermes: without `HERMES_ACP_SKIP_CONFIGURED_MCP`); default
  false;
- `network`: policy for Claude Code's bash sandbox mode, if enabled;
- `roots`: additional allowed paths;
- `launch`: `process` (the only value in the first version; `os-user` and `container`
  are reserved).

The `default` environment: no keys, no tokens, `deny` from `[permissions]` (by default it
includes ssh/scp too), local MCP off. Keys of one project (for example `infra`) are moved out of
`~/.ssh` into the worker's store (Keychain on a Mac) and are available only to sessions on paths
with that environment.

Limitation: without a container or a separate OS user the agent sees the files
and the process environment of the user, including other workspaces and session tokens on the
same machine. The boundary is the absence of keys; prohibitions in settings are the first line,
not a boundary. Windows in the first version: only `env` and `settings`, no `ssh_keys`.

## 15. Search

Postgres: `tsvector` with the `simple` configuration (mixed Russian and English text)
plus `pg_trgm` for fuzzy search over `event.payload.text` and session titles. Results
are filtered by grants. Limitation: `simple` has no stemming, word forms are found
only by the trigram search. One index, two consumers: the search bar in the client
and `hqc_search_history`.

Since 0.36 (T33; `crates/hqc-hub/src/search.rs`, migration `0016`, measurements in
`docs/notes/t33-questions.md`):
- **What is indexed.** The text a human reads: `payload.text` of `user.message`,
  `agent.message` (final or not), `peer.message` and `system`; not tool calls, tool results,
  thinking, requests or attachments. Two stored generated columns of `events`, `search_text`
  (that text or null) and `search_words` (its `simple` tsvector), so Postgres keeps them on
  every insert and the migration's table rewrite backfills old rows; GIN indexes on
  `search_words` and on `search_text` with `gin_trgm_ops`, and on session titles (expression
  indexes, as titles change). `pg_trgm` is a trusted extension, created by the migration.
- **Whose.** Until grants (iteration 6): the user's (the calling session's owner's) sessions,
  archived ones included and marked `archived: true`, the workers' service sessions left out.
- **Matching.** Exact: `search_words @@ websearch_to_tsquery('simple', q)` (words in any
  case; `"a phrase"`, `or`, `-word`). Fuzzy adds `q <% text` (trigram word similarity) with
  `pg_trgm.word_similarity_threshold` 0.5 for the query (the default 0.6 misses one typo in
  a short word); it finds other word forms and typos, at least 3 characters. Titles are
  matched the same way; a title hit has no `seq` and stands at the session's creation time.
- **Order and pages.** Newest first by `(ts, id)` (the event's time and id, for a title the
  session's creation and id); the cursor is that pair of the page's last hit, so pages do not
  repeat or skip hits when new events arrive. No ranking by relevance.
- **Snippet.** About 160 characters on one line around the first query word found in the text
  (ignoring case), else its beginning; `…` marks a cut.
- **Cost.** A query runs with `statement_timeout` 5 s; over it the query is refused. Over 100k
  events a rare or common exact word takes 1–20 ms, a phrase 10–20 ms, fuzzy with a rare word
  4–90 ms, fuzzy with a word in most events about 1 s (measured on home-mac). Since 0.39 (T36,
  §20): per owner 30 searches a minute, 10 of them fuzzy (`GET /api/search` and
  `hqc_search_history` together; `429 search_limit` / `limit_exceeded`), and at most 4 searches
  run at once on the hub (`503 busy` / `unavailable`), so one owner's agent cannot keep the
  database busy with fuzzy queries.
- **Client.** Desktop: the left column's search field filters the project's sessions by title
  and path at once (as before) and, from 2 characters and 300 ms after typing, shows the hub's
  hits below them ("In messages", "Similar words" for fuzzy, "More" for the next page). Phone:
  the list's search button opens `#/search`, a screen with the field, the sessions of all
  projects matching by title or path, and the hub's hits. A hit opens `#/s/<session>?seq=<seq>`:
  the chat loads its history as always (in full, page by page) and, once the event is there,
  scrolls the block that holds it (the last block starting at or before `seq`) to the middle
  and marks it.

## 16. Notifications and hub cron

Web Push on events: `approval.request`, `clarify.request`, `worker.approval.request`
(to the worker owner; since 0.17 this is an `approval.request` event in the worker's service
session, §10.2: `title` is the worker's name, `tag` is `<system_session_id>:<request_id>`; no
push for `worker_log` lines); the final `agent.message`, `peer.message` and `system` about
undelivered input (except mirror events: those get no push at all, §9.1) — to the session owner and to users with a grant who have written
in this session, if they have no active WS subscription to it. On iOS Web Push works
only from an installed PWA, a limitation of the first version. A Telegram bridge is
optional, via `hermes send`.

Push body (JSON, `hqc_proto::api::PushMessage`, example `examples/wire/api.push.json`):
`{session_id, type, title?, body?, tag?, payload?}`, where `type` is the event type and
`payload` is the §4 event itself as stored. The hub sets `title` (the session title), `body`
(a short text), `tag` (`<session_id>:<request_id>` for requests, otherwise
`<session_id>:<type>`: a notification with the same tag replaces the previous one) and omits
`payload` if the message does not fit into a push (about 4 KB). The client's service worker,
without `title`/`body`, builds them from `type` and `payload`, shows `approval.request` and
`clarify.request` with `requireInteraction`, and on click opens `#/s/<session_id>`, or
`#/s/<open_session_id>` when that is set.

A request of a session that other sessions of its owner wait on through open `hqc_ask`s
(since 0.41, T37; §7.1, §10.1): still one notification per request, with the request's own
`session_id` and `tag` (so a later notification of the same request replaces it), plus
`open_session_id` — the top of the chain (the session the user started the work in, where the
card is shown too). No push while the user watches the request's session or any session
waiting on it. Only the session owner gets `open_session_id`; a worker owner who is not the
session owner gets the push as before.

Hub cron: `ScheduledMessage` — at `at` or by `cron` in the user's `timezone`,
send `message` to `session_id`, or create a session from the project template
in `workspace_id` and send it there. Missed runs (the hub was down): with `catch_up`
it runs once, otherwise they are skipped. No task scheduler for agents;
anything more complex is Hermes cron.

Since 0.38 (T35, `crates/hqc-hub/src/schedules.rs`):
- A run is a `user.message` of the owner (`author: {kind: user, id: <owner>}`) with
  `schedule_id` in the payload, stored `pending` and delivered like the user's own input (§5.2):
  it queues behind a running turn and waits for an offline worker. Its `event_id` is derived
  from the schedule and the run's time, so a run is stored at most once.
- `cron` is 5 fields (minute, hour, day of month, month, day of week; `L`, `W`, `#` and the
  `@daily`-style names are accepted, seconds and years are not), evaluated in `timezone` with
  the crate `croner` and the IANA database of `chrono-tz`. DST as in Vixie cron: a fixed time
  in the skipped hour runs right after the gap (02:30 on a spring-forward night runs at 03:00), a
  fixed time in the repeated hour runs once, the first time; a daily 09:00 stays 09:00 local
  time across the change.
- `next_run` is kept in the table, so the loop survives a restart. Every 10 s the hub sends what
  is due. A run found more than 2 minutes late was missed (the hub was down): with `catch_up`
  it is sent once, late, otherwise skipped; either way the next run is the first one after now
  (several missed runs of a `cron` give one message at most). An `at` runs or is skipped once
  and is then finished (`next_run` null), kept for the list until deleted.
- A paused (`enabled: false`) or deleted schedule never fires; the hub locks the row while it
  sends, so a pause or a delete during a run waits for it. Resuming a `cron` skips the runs
  missed while paused. Deleting the session deletes its schedules. A session that is archived or
  driven from VS Code takes no input (§6, §9.3): its runs are skipped (`next_run` moves on, a
  warn in the hub's log), not queued.
- Since 0.39 (T36): a schedule made by an agent (`hqc_schedule`) keeps the chain of its call,
  and its runs carry it (§7.1). The hub sends at most 60 runs per owner and hour; a run over
  that is skipped like a run of a target that takes no input (a warn, `next_run` moves on). The
  count of schedules that may still run and the insert are made under a per-owner lock.
- The hub's clock is injectable for tests (`AppState::clock`).

## 17. States and lifecycle

### 17.1 Session state
`state` is computed by the hub when serving, from `archived`, `locked_by`, the worker's status,
open requests, `process_state` and delivery. Priority top to bottom, the first true one wins;
`queue_len`, the number of inputs in `pending`, is served separately:

| state | when |
|---|---|
| `archived` | archived |
| `locked_vscode` | `locked_by: vscode` |
| `worker_offline` | the worker is unreachable |
| `waiting` | there is an open `approval`/`clarify` |
| `error` | there is an input with `delivery: failed`, or the last turn ended with a process error |
| `running` | a turn is in progress |
| `queued` | `queue_len > 0`, no turn (the agent is starting) |
| `idle` | otherwise |

The `status.state` event uses the same set.

The client shows the turn from `state` (from `summary`/`session.summary` and the session object),
with no separate events: `running` — a pulsing "Running" pill in the header and an "Agent is
working" line at the bottom of the feed ("Running <command or tool> <time since the call's
`ts`>" while the last `tool.call` has no `tool.result`, T26), `queued` — "Input queued",
`waiting` — "Waiting for approval" if the open request is not in the feed (otherwise its card is
that line).
A `status` event is not drawn as a feed line, except `error` with `detail`.

The hub tracks "a turn is in progress" itself: a turn is opened by the worker's `ack` of
`session.input` (the worker passes input to the runtime one per turn, §17.2) and closed by
`agent.message` with `final: true`, `status` with `idle` or `error` (the latter means "the turn
ended with an error"), the process stopping or the watchdog (§17.3, also "the turn ended with
an error").
"The turn ended with an error" keeps `error` until the next input: a new `user.message`
clears the flag. `worker_offline`: the session has a worker and it is not connected now.

### 17.2 Input during a turn **[decision]**
Input from the input queue (§7.1) that arrives during a turn waits for the final answer
and starts the next turn; one turn per item, in `seq` order. The
"interrupt and send" button does `session.stop` and then the input. For runtimes with `steer`
(Hermes) a "steer" button is available: `session.steer` without interrupting. Two users
with `write` share the same queue.

### 17.3 Processes
- An idle agent is left alone by default. With the worker's `idle_timeout` > 0 the
  process exits, and the next input brings it back via resume.
- Worker offline: input accumulates in the hub (`pending`), the session is `worker_offline`,
  delivery on connect in `seq` order. Open `approval`s on an offline worker
  stay open until it returns or until `expires_at`.
- The hub detects a worker restart by a new `boot_id` in `hello` (§5.2): all agents are
  `stopped`; open `approval`/`clarify` get `approval.cancel`/`clarify.cancel
  {worker_restart}`; an unfinished turn is lost, the hub writes `system turn_lost`;
  `pending` inputs are resent after `hello` (via `session.start` with `mode: resume`).
  A reconnect with the same `boot_id` is not a restart, nothing is cancelled.
- An agent process that exits within 500 ms of launch is a failed
  start: the adapter's `start`/`resume` returns an error (the worker replies
  `agent_start_failed`, retryable, §5.2), not "process exited"; otherwise a worker with a
  non-empty queue would restart it in a loop. A process that dies later with an error or
  in the middle of a turn gives `system agent_exited`; the worker brings it back via `resume`
  if there is input in the queue.
- Hub restart: workers reconnect and resend their outbox; `PendingRequest`s
  are restored from the database, undelivered `mcp.result`s are resent.
- Turn watchdog **[decision]**. If a session is `running` (§17.1) and its worker has sent no
  event for it, `agent.delta` included, for longer than `HQC_TURN_WATCHDOG_S` (default
  900 s, `0` turns it off), the hub considers the turn lost: it cancels open
  `approval`/`clarify` with `timeout`, closes the turn without a final message
  (the input stays `delivered`), writes `system turn_lost` with the text "no events for N s",
  and the session shows `error` until the next input. The countdown runs from the worker's last
  event or from the moment the session became `running` again: waiting for the user's
  answer (`waiting`), waiting for the reply to its own `hqc_ask` (an open ask of the session,
  since 0.35) and the worker being offline (`worker_offline`) do not count. The hub keeps
  event times in memory; after a hub restart every open turn gets the full
  period. The hub sends nothing to the worker: the process and its input queue are left alone.

### 17.4 Archiving and deletion
- Archiving: the hub sends `session.kill`, open requests get
  `approval.cancel`/`clarify.cancel {archived}`, `pending` inputs are marked `failed`,
  the send buffer is cleared. Unarchiving returns `idle`; the process comes back via
  resume on the first input.
- Session deletion: events, `ReadCursor`, `PendingRequest`, the session's artifacts
  (unless another session references them) are deleted; jsonl files on the worker
  are **left alone**; late events from the worker's outbox get `ack` and are dropped
  (§5.2).
- Cleanup on the worker (since 0.23, T21). When a session (or its project) is archived or deleted,
  the hub sends the worker, after `session.kill`, the frame
  `session.cleanup {session_id, workspace_path | null, cwd, event_ids[]}`
  → `ack` (example `hub.session.cleanup*.json`; added purely additively, `session.kill` did not
  change): `event_ids` are the session's inputs with attachments. The worker deletes
  `<workspace>/<cwd>/.hqc/inbox/<event_id>` of these inputs, and for a session without a workspace
  (`null`, §8.4), its folder `<scratch_root>/<session_id>`. The worker does not trust the hub's
  paths (§2.1): the workspace is inside `allowed_roots`, `cwd` is relative without `..` and is
  resolved inside the workspace, `.hqc` and `inbox` are real directories, not symlinks; only
  direct children of `.hqc/inbox` named with an event uuid and only `<scratch_root>/<uuid>` are
  deleted, a symlink there is deleted as a link. The worker deletes nothing outside `.hqc/inbox`
  and `scratch_root`. If the worker is offline the frame is not resent; the cleanup at start
  covers this: inbox folders (children of `.hqc/inbox` named with a uuid) older than 14 days by
  modification time in workspaces known to the worker —
  `known_workspaces.json` and the working directories it wrote attachments to
  (`$HQC_HOME/inbox_dirs.json`), only inside a known workspace and `allowed_roots`.
- Worker deletion (`DELETE /api/workers/{id}?purge=1`, §6; only a revoked one with no
  non-archived sessions): its service session is deleted like a session above, its workspace
  records as in §13.2 (archived sessions lose `workspace_id` and `worker_id`, the history stays),
  then the worker's record. Files on the machine are left alone.

## 18. Stack

- Hub: Rust, axum, tokio, sqlx, Postgres. Artifacts: disk or S3.
- Worker: Rust, one static binary on macOS/Linux/Windows, systemd/launchd/
  Windows service, optional tray icon. MCP server on `rmcp` (subcommand
  `hqc-worker mcp`). ACP client: the `agent-client-protocol` crate (the protocol's official
  Rust SDK), **[verify]** that it is current. Secure store for tokens and keys: Keychain
  on macOS; on Linux and Windows in the first version the worker token is a 0600 file (§5.1),
  libsecret and DPAPI later.
- Client: TypeScript, responsive web, PWA; Tauri on the same code later.
- Shared crates: `hqc-proto` (event and frame schema, versioned),
  `hqc-claude-jsonl` (transcript parser), adapters as separate crates.

## 19. Iterations

1. Hub with Postgres and Access JWT verification, worker with enrollment and outbox, delivery
   both ways with ack and the `failed` policy, Claude Code adapter with `can_use_tool`,
   web chat, permission buttons, Web Push. Simplifications: a single workspace set by
   the worker config, a single (personal) project — both lifted in 0.16 (T14), no hqc MCP.
   Criterion: start a session from the web, continue it from a phone, approve a permission.
2. ~~Projects and workspaces~~ (done in 0.16, T14: projects, several `dir` workspaces in a
   project, `fs.stat`/`fs.list_dirs`, sessions in the chosen workspace; the iteration 1
   simplification "a single workspace from the worker config" lifted), ~~the worker's service
   session~~ (0.17, T17), ~~artifacts, `.hqc/inbox`~~ (0.18, T16, without `hqc_attach`),
   ~~VS Code import and mirror, takeover both ways~~ (0.19, T15; consolidation — T18; takeover
   by the registry of live sessions and forks — 0.22, T20). Criterion: take over from a phone
   a session started in VS Code (live test `live_takeover`; checked by hand through the UI on a phone on 2026-10-25).
   Closed on 2026-10-25.
3. hqc MCP: attach, send, ask, spawn, list, search, schedule; `mcp.call` frames;
   MCP server per §7.2. Tools work on the owner's own sessions only (grants are iteration 6).
   Tasks: `docs/ITERATION-3.md`.
4. Generic ACP adapter (the first agent is Hermes, including steer); bare model
   and local models through a proxy; the personal assistant session (moved from 3: its default
   runtime is the bare model, §8.3), until then `hqc_send` needs `session`.
5. Workspaces: clone, worktree, ephemeral, session template. Environments, bindings
   and `acp_agents` on the worker.
6. Sharing: Grant, the "share" dialog, the "who pays" and foreign session rules. ~~Hub cron~~
   (0.38, T35, with `hqc_schedule`).
7. Tauri, native notifications, several agents in one session.

## 20. Hosting and security

- The hub listens only on the tunnel interface; `/api/*` is behind Cloudflare Access with JWT
  verification; `/worker/*` is outside Access, worker tokens only, rate limit on enroll;
  `artifacts.<hub-domain>` is public, signed links with a TTL only.
- The hub's dev mode (`HQC_DEV=1`, no Access) is allowed only on a loopback
  `HQC_HUB_LISTEN` address and without `HQC_ACCESS_*`; otherwise the hub does not start (§6).
- Worker tokens: random 256 bits, only the hash in the database; revocation from the UI.
- `Origin` **[decision]**: a WebSocket upgrade of `/api/ws` without `Origin`, or with an `Origin`
  that does not match the request's `Host`, gets `403`; non-GET `/api/*` requests with a foreign
  `Origin` get `403` (without `Origin` they pass: that is how non-browser clients come). A match
  is scheme `https`, or `http` for a loopback host (dev), with host and port equal to `Host`.
  Protects against cross-site WebSocket hijacking and CSRF with an Access cookie with
  `SameSite=None`; Access additionally has SameSite Lax (RUNBOOK §1.6).
- Headers on all of the hub's responses: `Content-Security-Policy: default-src 'self';
  connect-src 'self' wss: https:; img-src 'self' data:; style-src 'self' 'unsafe-inline';
  object-src 'none'; base-uri 'none'; frame-ancestors 'none'`, `X-Frame-Options: DENY`,
  `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`. The client build has
  no inline scripts (a client test checks this). HSTS is set by Cloudflare.
- Limits (in the hub process's memory; the address is `CF-Connecting-IP`, otherwise the
  connection's address): `POST /worker/enroll` — 10 per minute and 10 live unconfirmed codes per
  address, 1000 in total; `POST /worker/enroll/poll` — 60 per minute per address; `/worker/ws` —
  20 invalid tokens per minute per address (beyond that `429` without a database query), a token
  of the wrong shape does not reach the database, at most 16 token checks at once (`503`); client
  and worker WebSocket messages up to 1 MiB, the send queue to a worker is bounded (overflow drops
  the connection, the state is resent from the database after `hello`). On top of that, a
  Cloudflare WAF Rate Limiting rule on `/worker/*` (RUNBOOK §1.6).
- Limits against a compromised worker or its token (since 0.23, T22; threat model
  §2.1). The numbers are the hub's `Limits` (`config.rs`); there are no environment variables for
  them. Where a rule differs from the text of the section given in parentheses, this table wins:

  | what | limit | over the limit |
  |---|---|---|
  | `worker.approval.request` (§10.2): open per worker | 20 | `error invalid_frame`, nothing is written |
  | … `request_id` / `kind` | 1–128 / 1–64 bytes | `error invalid_frame` |
  | … `description` | up to 4096 bytes (stored up to 2000 characters, as in §10.2) | `error invalid_frame` |
  | … `expires_at` | in the future, at most 24 h ahead (before 0.23 it was truncated to a day; §10.2 aligned in 0.24) | `error invalid_frame` |
  | push on worker requests (§16) | 1 per 10 s per worker | the request is written, the push is skipped |
  | `mirror.event` (§9.2): new sessions per worker | 200 per 24 h (sliding window) | the record of a new transcript is not accepted: `error mirror_quota`, `retryable: true`, warn in the hub's log; the worker removes it from the outbox and rereads the transcript from the start no sooner than an hour later (§9.1, since 0.24) |
  | … mirror sessions per workspace | 2000 in total | the event is dropped, `ack` without `seq`, warn in the hub's log |
  | … `runtime_session_id` | lowercase uuid with hyphens | same |
  | … `cwd` | up to 4096 bytes and inside the workspace path | same |
  | a worker event of type `user.message`, `approval.response`, `clarify.response`, `peer.message` (§4: only the hub writes them) | forbidden in any session | `error invalid_frame` |
  | a worker event in its service session (§10.2) | only `system` with `code: worker_log` | `error invalid_frame` |
  | a worker event in an archived session (§17.4) | only one that ends a turn: the final `agent.message`, `status idle`/`error` | `ack` without `seq` |
  | `fs.stat`, `fs.list_dirs`, `workspace.list_candidates` from the API (§5.3, §6) | 4 at once per worker | `429 worker_busy`, no frame goes to the worker |
  | takeover without a registry (§9.3): `locked_by: vscode` without `vscode_active_at` | treated as "written just now" | `409 vscode_active`; the transition to `vscode` sets the time, migration `0012` fills it in for old rows |
  | turn watchdog (§17.3) | a pass every `limit / 10`, at least every 10 s, over the partial index `turn_open` | — |
  | attachment download by the worker (§12, `.hqc/inbox`) | 100 MB per artifact, 500 MB per input; streamed to disk | the download is aborted, the partial file is deleted, `error` on `session.input` without retry |
  | attachment display by the client (§12) | a diff body is fetched only once `size` is known; text and diffs over 2 MB get "Download" only; a body with `Content-Length` over the display limit is not read | — |
  | `mcp.call.args` (§5.2, since 0.33) | 64 KB serialized; the worker refuses a bigger call before sending it, the hub checks again | `mcp.result` `error invalid_args` |
  | `mcp.call.session_token` (§5.2) | `hqcs_` + 43 base64url characters, checked before the database | `mcp.result` `error unauthorized` |
  | send buffer of a session (§7.1, since 0.35) | the newest 50 messages and 64 KB of their texts | the oldest dropped, `system peer_evicted` in the target |
  | chain of peer messages (§7.1) | `depth` ≤ 3 nested asks, `hops` ≤ 20 messages | `mcp.result` `error limit_exceeded` |
  | `hqc_ask.timeout_s` (§7) | 1 … `ask_timeout_s` (600 s), larger cut to it | `0`: `error invalid_args` |
  | `hqc_list_sessions` (§7) | 200 sessions per call | `truncated: true` |
  | search query (§15, since 0.36): `GET /api/search`, `hqc_search_history` | `q` 1–200 characters, fuzzy at least 3; 50 hits per page (the tool 20); 5 s `statement_timeout` | `400 bad_request` / `mcp.result` `error invalid_args` |
  | `hqc_spawn` (§7, since 0.37): sessions spawned in one chain (§7.1; a human's turn is one chain) | 5 | `mcp.result` `error limit_exceeded`, nothing is created |
  | … sessions spawned per owner | 20 in the last hour (sliding, by `created_at`; deleted sessions no longer count); since 0.39 counted and created under a per-owner lock, so parallel calls cannot overshoot | same |
  | `hqc_attach` file (§7, since 0.34) | a regular file of at most 100 MB, inside the session's workspace and `allowed_roots` after resolving symlinks; streamed, exactly the size of the open handle | `mcp.result` `error invalid_args` from the worker, nothing is uploaded |
  | `PUT /worker/artifacts` (§5.4) | 100 MB (`Content-Length` checked first, then the stream); a session of this worker | `413 too_large` / `404` |
  | the worker's MCP socket (§7.2) | a line up to 64 KB + 4 KB; peers of the same OS user only | the connection is closed |
  | schedules per owner (§16, since 0.38) | 100 that may still run (`next_run` set, paused ones included); since 0.39 counted and inserted under a per-owner lock | `hqc_schedule`: `error limit_exceeded`; `POST /api/schedules`: `409 schedule_limit` |
  | `cron` of a schedule (§16) | runs at least 5 minutes apart, checked over its next 100 runs; a pattern that never runs is refused | `error invalid_args` / `400` |
  | `at` of a schedule | in the future | `error invalid_args` / `400` |
  | `message` of a schedule | non-blank, up to 16 KiB | `error invalid_args` / `400` |
  | `mcp.call` per worker (§5.2, since 0.39) | 300 a minute, counted before the token is looked up | `mcp.result` `error limit_exceeded` |
  | `mcp.call` per calling session | 60 a minute | same |
  | `mcp.call.request_id` | 1–128 characters of `A-Z a-z 0-9 . _ : -` | `error invalid_args` |
  | `mcp.call` of an archived session | refused | `error unavailable` |
  | open `hqc_ask`s per session (§7.1) | 5 | `error limit_exceeded`, nothing is stored |
  | peer messages a session sends (`hqc_send`, `hqc_ask`, the prompt of `hqc_spawn`) | 20 a minute | same |
  | peer messages in one chain (§7.1) | 50 in all | same |
  | `hqc_schedule` from an agent (§7.1, §16) | moves the chain on like a send; refused at the chain's limits | `error limit_exceeded` |
  | runs of scheduled messages (§16) | 60 per owner and hour | the run is skipped, warn in the hub's log |
  | searches (§15) | 30 per owner and minute, 10 of them fuzzy; 4 at once on the hub | `429 search_limit` / `503 busy`; `error limit_exceeded` / `unavailable` |
  | `PUT /worker/artifacts` per worker | 2 GB per hour (sliding; `Content-Length` checked against what is left, the body counted once read); an archived session takes none | `429 upload_limit` (the worker's tool: `limit_exceeded`) / `404` |
  | `chain`, `delivery`, `delivered_at` of a worker's `event` (§5.2) | dropped: only the hub writes inputs | stored without them |
  | `delivery` of a `mirror.event` (§9.2) | `delivered` on a `user.message`, none otherwise, whatever the worker sent | — |
  | the worker's MCP socket (§7.2, since 0.39) | a session's token only from a process inside that session's agent | the connection is refused (`unauthorized`) |
  | `hqc_attach` paths (§7) | a `path` outside the workspace as written is refused before the file system is asked; no refusal names a resolved path | `error invalid_args` |
- Push: the hub POSTs to the subscription's `endpoint`, so only `https://` without
  userinfo and without a non-standard port on push service hosts is accepted: `fcm.googleapis.com`,
  `updates.push.services.mozilla.com`, `*.push.services.mozilla.com`, `*.push.apple.com`,
  `*.notify.windows.com`; redirects are not followed when sending.
- Access JWKS: keys are refreshed outside the cache lock, on a refresh error the previous set
  stays, retries with a growing pause (5 s … 5 min).
- Worker: `hub_url` is `https` only (§5.1), the agent's environment is an allowlist (§8.1), the
  outbox is a 0700 directory, files 0600.
- Users' `api_keys` are encrypted with a master key from the hub's env.
- Cloudflare Tunnel: request body limit ~100 MB (matches the artifact limit),
  long-lived connections need heartbeats and reconnects.
- Backup: Postgres and the artifact store; workers' outboxes survive a hub restart.

## 21. Risks

- The Claude Code jsonl format and the stream-json/control protocol are unofficial; keep
  the parser and the adapter tolerant, cover them with fixtures, pin the CLI version.
- Takeover relies on the plugin and the CLI sharing the session file; if the plugin changes
  its storage, the mirror breaks.
- ACP in Hermes is stable, but the ACP v2 methods (`session/resume`, `session/fork`) are still
  a draft; the adapter sticks to v1 plus `session/load` and pins the Hermes commit hash
  (`4787e4d5` on llm-server).
- The boundary between sessions on one machine without a container is soft (§14); for
  ACP agents other than Hermes it is weaker still (§2.1).
- Agent→agent chains are bounded by timeouts and budgets, not by semantics; with
  abuse of `ask` money is spent up to the limit.

## 22. Prior art and borrowings

Checked on 2026-10-22.

- **Codync** (a Rust host on every machine, ACP to 40+ agents, SQLite, iPhone/Electron/TUI,
  Cloudflare relay with E2E). Closest in spirit: bots with a folder and a permission policy,
  group chats with @ and a limit of 3 rounds, a built-in `team` MCP for asking other
  bots. The architecture is the reverse of ours: many hosts, the client connects to each.
  No web or Android, no VS Code mirror, sharing, shared search or offline queue.
- **Paseo** (TS daemon, Expo clients, relay). More mature clients, no Hermes and no
  single hub over many machines.
- **Claude Code Remote Control**: official, Claude Code only, no projects
  and no Hermes.
- **CloudCLI / claudecodeui**: one machine, reads `~/.claude/projects`; the only
  one with a mirror of VS Code sessions.
- **Agent Host Protocol (Microsoft, MIT)**: a protocol for "a host between many clients
  and ACP agents": actions with `serverSeq`, reconnect by `lastSeenServerSeq`,
  write-ahead with an `origin` echo, "the first `toolCallConfirmed` wins", one turn
  per chat. That is exactly our §5–§6 and §17, but without workers, users, grants,
  agent→agent and storage. We do not take it whole; taken: the `client_op_id` echo (§6), the
  first-answer rule for approvals (§10.1), the action vocabulary for naming events.
  An AHP-compatible facade over `/api/ws` is possible later, if third-party clients appear.
- **OpenAB / OpenACP**: bridges from ACP agents to Discord, Telegram, Slack. A fallback UI:
  if the web client is delayed, a service bot in Telegram with forum topics covers
  notifications and approval answers. Not in scope.

### 22.1 Rejected alternative: no hub
The Codync model: every worker is a host itself, the client connects to all of them. Rejected
because `ask`/`send` between machines, shared history and search, sharing with colleagues,
the input queue for an offline worker and a single address for push need shared state;
without a hub each of these would have to be done peer-to-peer.

## 23. Change history

### 0.40 → 0.41 (T37, requests where the user is looking)
§6: `GET /api/requests` (every open request of the user with its session, worker, event and the
sessions waiting on it through `hqc_ask`s, transitively), the `requests` frame of the summary
subscription, `open_session_id?` in the push body, the answer endpoint used from any view.
§10.1: a request is also shown, as the same card under a header naming its session and
worker, in every session waiting on it and in the "Requests" list; answers stay the request
session's own. §10.2: workers' requests are in that list. §16: the push of a waited-on request
opens the top of the chain, one notification per request, none while the user watches the
chain. Migration `0020` (an index to find a request's event). Notes:
`docs/notes/t37-questions.md`.

### 0.39 → 0.40 (a delivery lost while the socket was down)
§6: a resubscription starts before the first input still `pending`, and an event sent again
updates the client's copy's `delivery`. Before, a message sent while the hub restarted stayed
at "awaiting delivery" until a reload: its `delivery` frame went out while the browser was
reconnecting, and the copy the hub sent again was dropped as already seen (2026-10-25).

### 0.38 → 0.39 (T36, hardening of the MCP surface; iteration 3 closed)
§2.1: what a prompt-injected or compromised agent can spend, the MCP socket bound to the
agent's process tree, a worker trusted for its own sessions only (no `chain`/`delivery`).
§5.2: `mcp.call.turn_input?`, the shape of `request_id`, `limit_exceeded` and archived callers.
§7: the token on the command line no longer usable by other sessions' agents. §7.1: header
fields reduced to one line without `[`, `]`, `|`; a resent input keeps the `peer_prefix` of its
first send (migration `0019`, `input_prefix`); an ask fails at once when its target's turn is
lost; the chain of a call from `turn_input`; an agent's schedule continues its chain; 50 peer
messages per chain. §7.2: the process-ancestry check (verified live on 2.1.308 after the merge). §15, §16: search and
scheduled-run caps, per-owner locks on the counted creations. §20: the new rows. Notes:
`docs/notes/t36-questions.md`.

### 0.37 → 0.38 (T35, `hqc_schedule` and the hub cron)
§3 ScheduledMessage: the table `scheduled_messages` (migration `0018`) with `next_run`, session
targets only; User `timezone` set by the client. §4: `user.message.schedule_id?`, the hub cron
as a source. §6: `PATCH /api/me {timezone}`, `/api/schedules` (list, create, pause/resume,
delete), `409 schedule_limit`, the client's "Schedules" panel. §7: `hqc_schedule(session,
message, at?, cron?, timezone?, catch_up?)` defined and listed, once per `request_id`, no
listing or deleting by the agent. §16: how runs are sent, the 5-field cron with `croner` and
`chrono-tz`, DST, the 2-minute "missed" rule and `catch_up`, pause, delete and targets that take
no input. §19: the hub cron moved from iteration 6 to 3. §20: limits of schedules. Notes:
`docs/notes/t35-questions.md`.

### 0.36 → 0.37 (T34, `hqc_spawn`)
§3 Session: `spawned_by` and `spawn_chain_id` (migration `0017`). §5.2: a repeated
`hqc_spawn` finds its session and message by `once_id`. §6: the session object gets
`spawned_by? {session_id, address}`; the client's "started by" link. §7: `hqc_spawn(project,
workspace?, runtime?, model?, prompt)` defined and listed: the API's session creation, the
workspace by id, label or path, `claude_code` only, the caller's model by default, the prompt
as a `peer.message` (`mode: send`) that starts the first turn, `{address}` at once. §7.1: the
prompt joins the input queue; a human's turn is one chain (its id derived from the input),
spawns move it like a send. §4: `peer.message` from `hqc_spawn`. §13.4: `invalid_args` from `hqc_spawn`. §20: spawn limits, 5 per
chain and 20 per owner per hour. Notes: `docs/notes/t34-questions.md`.

### 0.35 → 0.36 (T33, search)
§15: what is indexed (the text of `user.message`, `agent.message`, `peer.message`, `system`
and session titles; no tool output), whose (the owner's sessions, archived ones marked,
service sessions left out), exact and fuzzy matching (word similarity threshold 0.5), newest
first with a cursor, the snippet, the 5 s limit and the measured cost over 100k events, the
client (the left column's field, the phone's `#/search` screen, `#/s/<id>?seq=` opens a hit).
Migration `0016`: `pg_trgm`, stored generated columns `events.search_text` and
`search_words` (backfilled by the table rewrite), GIN indexes. §6: `GET /api/search` defined
(`api.search.json`). §7: `hqc_search_history(query, project?, fuzzy?, cursor?)` defined and
listed. §20: limits of a search query. Notes: `docs/notes/t33-questions.md`.

### 0.34 → 0.35 (T32, `hqc_send` and `hqc_ask`)
§3 PendingRequest: an `ask` is a row of the asking session with the target and the
`peer.message` in `payload`. §4: `peer.message` gets `from?` (the sender's address), is
authored by the sending agent and carries the chain; buffered sends have no `delivery`; new
`system` code `peer_evicted`. §5.2: repeated calls act once (the message's `event_id` is T31's
key with `peer.message` as the tool; a repeated ask finds its `PendingRequest`);
deferred answers are sent after `hello` without `session.start`; a worker restart closes its
open asks with `worker_restart`; `mcp.cancel` is acked after the ask is closed. §5.3:
`session.input.peer {from, mode, request_id?}` for the header. §7: results of `hqc_send`
(`{to, delivery}`) and `hqc_ask` (`{from, reply}`), `session` required until iteration 4,
`timeout_s` bounds, error codes `timeout`, `limit_exceeded`, `worker_restart`. §7.1: the
runtime text (block, header, escaping of the address too); the target rules (own session,
`locked_vscode` refused for `send` too); the send buffer in `peer_buffer` (migration `0015`)
and how an input carries it; the hub sets the chain from the calling session's last delivered
input — the plan of giving the chain to the MCP server is dropped, the MCP process cannot
tell turns apart; a lost target turn leaves the ask to its timeout; the client's "Waiting
for a reply" line. §7.2: progress every 30 s; the MCP server keeps no chain. §17.3: an open
ask is not silence for the turn watchdog. §20: limits of the buffer, chains and
`timeout_s`. Notes: `docs/notes/t32-questions.md`.

### 0.33 → 0.34 (T31, `hqc_attach`)
§7: `hqc_attach(path, name?, kind?, caption?)` defined and listed; the worker resolves the path
itself (working directory, canonical inside the workspace and `allowed_roots`, regular file,
100 MB, opened with `O_NOFOLLOW` and checked on the handle), uploads, then sends `mcp.call
{artifact_id, caption?}`; `content` refused until the bare model; checked live on 2.1.308.
§5.2: a tool with side effects acts once per `request_id` — its writes are keyed by `uuid
v5(tool, calling session, request_id)` and a repeat answers from them. §5.4, §12: `PUT
/worker/artifacts` defined (headers, owner = the session's owner, the client's dedup and
`kind`, linked to the session). §4: `attachment` written by the hub with the agent as author.
§2.1, §14, §20: the path rules and limits of `hqc_attach`. Notes in
`docs/notes/t31-questions.md`.

### 0.32 → 0.33 (T30, the MCP pipe and `hqc_list_sessions`)
§5.2, §5.3, §5.4: `mcp.call` goes through the outbox without `session_token` (the token is added
when sent; a call no longer open, e.g. from a previous run, is dropped, not sent); a fast tool's
`mcp.result` is the reply under the call's frame `id`, a deferred one is acked by the hub and
answered by a frame of its own; `mcp.cancel` is answered with `ack {}`, sent for every call open
when the runtime cancels it or the MCP process exits; the hub accepts the latest and the previous
session token (migration `0014`, `prev_session_token_hash`) and only from the session's worker;
errors in the one shape `{code, message}` with the codes listed in §5.2. §7: the token is passed
in the `env` of the server `hqc` inside `--mcp-config` — the claim that it "does not reach the
agent's context or the transcript" replaced by what was checked on 2.1.308 / macOS 26.6
(`live_mcp`, `docs/notes/t30-questions.md`), including that Linux exposes the command line to
other users; the tool list is shared by worker and hub, a tool appears only when its task lands;
`hqc_list_sessions` result defined. §7.2: the socket `$HQC_HOME/mcp.sock`, the protocol versions,
the daemon's token check. §8.1: the `--mcp-config` JSON, `MCP_TOOL_TIMEOUT` on every start,
`--strict-mcp-config` checked live (only `hqc`, no claude.ai connectors). §20: limits of the MCP
path.

### 0.31 → 0.32 (iteration 3 planned)
§19: iteration 3 also gets `hqc_spawn`; the personal assistant session moves to iteration 4
with the bare model; tools act on the owner's own sessions until grants (iteration 6).

### 0.30 → 0.31 (iteration 2 closed)
§19: iteration 2 is closed; its criterion was checked by hand through the UI on a phone.

### 0.29 → 0.30 (a session taken over in VS Code mid-turn)
§4 `session_released`, §9.3: when a holder appears or a foreign writer is found while hqc's
agent is in a turn, the worker stops the agent at once instead of after the turn. The 2026-10-25
case: VS Code 2.1.310 took over the session during a 30 s `Bash` call, closed the call with its
own "Tool call interrupted" result and continued from there, while hqc's agent finished the call
and answered on a branch VS Code never saw. The `session_released` texts say the turn was
interrupted; the hub's `turn_lost` and the cancelling of open requests on `stopped` apply as
before.

### 0.28 → 0.29 (T28, translated to English)
Translated to English, with no change in meaning; English is now the language of the whole
project (spec, docs, UI, user-visible texts of the hub and the worker). The marks are
**[decision]** and **[verify]** (formerly their Russian equivalents). The texts the hub and the
worker write change with it, codes stay: the default session title is "Session N", `system`
texts of `session_taken_over`, `session_released`, `session_forked`, `worker_replaced`,
`worker_revoked` and the import, the `vscode_open` error, the worker log line "skipped N"; the
block of attachment paths in the agent's input (§8.1) is headed `Attachments:`; the worker's
title prompt (§8.1, T21) is in English and asks for a title in the language of the
conversation. Sessions created before keep their stored titles. Glossary and notes:
`docs/notes/t28-questions.md`.

### 0.27 → 0.28 (T27, per-turn cost, slash command completion, shrinking layout)
§4: `usage.cost_usd` is the turn's cost; new optional `usage.session_cost_usd`, the runtime's
running total. In a resumed session every turn showed the session's total ("$44.401") as its
cost and the header summed those totals. §8.1: `result.total_cost_usd` of Claude Code is
cumulative over the session and over `--resume` (from the transcript's last `cost-state`),
checked live on 2.1.308; the adapter subtracts the previous total. §5.4, §6: additive
`session.state.commands` and `commands` in the session object — the runtime's slash commands
from `system/init`, stored by the hub (migration `0013_session_commands.sql`); §8.1: what the
adapter takes from `system/init`. Client: `/` completion in the composer, the header shows the
latest `session_cost_usd`; the session column narrows with the window
(`clamp(220px, 25vw, 300px)`), markdown tables scroll inside themselves.

### 0.26 → 0.27 (T26, client redesign and a text reply to a request)
§4, §5.3, §6, §10.1: `approval.response` (the event, the frame to the worker and the body of `POST
.../requests/{id}`) gets an optional `message` field: a text reply instead of the call, only with
a `deny`/`deny_always` option of a request in a session (otherwise `400`; `clarify` and worker
requests have no such field). The hub passes it to the worker; the Claude Code adapter puts it into
the `message` of a `deny` answer, using Claude Code's own wording for a denial with an explanation;
the client shows the text as a user message. Two live runs on 2.1.308 (haiku): the model sees the
denial text in the tool result; with plain wording it did not do what was asked, taking it for an
instruction from the result, with Claude Code's wording it did. §17.1: the client's turn lines in
the new look ("Running <command> <time>", a "Running" pill in the header).

### 0.25 → 0.26 (T25, false release while hqc is working)
§9.3 **[decision]**: a worker with a holder registry does not detect a foreign writer by record
`uuid`s; a session driven by an hqc process is released only by a foreign live holder (as since
T20). A record with an unknown `uuid` while the process is alive waits 5 s and is emitted through
`mirror.event` with `held_by` (the hub does not lock on it; the hub's behaviour did not change).
The `uuid` check remains a fallback for a worker without a registry; without an hqc process the
mirror rules are the same as before. §9.2: the adapter also puts input texts into its registry of
own records; a prompt record with an unknown `uuid` is its own if its typed text (for a slash
command, the command with arguments from `<command-name>`/`<command-args>`) matches an unmatched
input. The trigger was a production log of 2026-10-24 (`/compact` from hqc and the first input
after `--resume` produced a "foreign writer" with no foreign holder). Live runs (3, 2.1.308): the
echo after `--resume` carries the record's `uuid`, the `/compact` command does not reach
stream-json; fixture `resume-compact`.

### 0.24 → 0.25 (T24, titles on the worker's subscription)
§3 Session: the session title is generated by the session's worker on its Claude Code
subscription, not by the hub; the hub's model providers are removed entirely: the `llm` module,
`HQC_TITLE_MODEL`, `HQC_TITLE_BASE_URL`, `HQC_TITLE_API_KEY`, the limit of 4 concurrent calls
(they had no other consumers; the bare model of §8.3 will get its own client when its turn
comes). After a session's first final answer with `title_source: default`, the hub sends the
worker `title.request` and waits 30 s; offline, an error, an empty answer and a timeout give a
title from the first prompt, as before. §5.3: frame `title.request {request_id,
session_id, user_text, agent_text}` → `result {request_id, title}` or `error`; §5.4: `result` to
it. §8.1: a one-off `claude -p` with fixed flags (`--no-session-persistence`,
`--setting-sources ""`, `--tools ""`, `--max-turns 1`, `claude-haiku-5-5`), an empty folder
`$HQC_HOME/titles/<slot>`, 25 s timeout, at most 2 at once — checked with a live run.

### 0.23 → 0.24 (merge of T21 and T22)
T21 and T22 were done in parallel from 0.22, each bumped the version to 0.23; both entries are
kept below as they were. There is one point where they meet in substance: T21 turns on the mirror
for all workspaces (migration `0011`), T22 limits new sessions from the mirror to 200 per day per
worker. Dropping those over the limit with an `ack` would permanently lose part of the first
import wave, so §9.1, §20: a record of a new transcript over the daily limit gets
`error mirror_quota` (`retryable: true`, a new code in §5); the worker removes it from the outbox
and rereads the transcript from the start at most once an hour. The limit of 2000 per workspace
and the `runtime_session_id`/`cwd` checks still drop with an `ack`. Migrations `0011`
and `0012` are independent (different tables and columns) and apply after `0010` in any order.
Carry-overs left by both tasks: `session.cleanup`, `mirror.set.sessions`, `hello.machine_id` are
added to the frame lists of §5.3 and §5.4, `machine_id` in `POST /worker/enroll` to §5.1,
`mirror_quota` to the reply to `mirror.event`; §10.2 is aligned with the §20 table (`expires_at`
in the past or more than 24 h ahead is rejected, length limits and open request limits, push once
per 10 s); §9.1 refers to the mirror limits of §20.
Outside the branches, on master between 0.22 and the merge (685997d, described in RUNBOOK §2.2,
the spec text did not change): sessions started by hqc are visible in VS Code's history
(`CLAUDE_CODE_ENTRYPOINT=claude-vscode`, worker setting `show_in_vscode`, default `true`),
`--settings` always has `voiceEnabled: false`, a `policy_hint: allow` caption in the client
explains why Claude Code asks anyway.

### 0.22 → 0.23 (T21)
§9.2: mirror on for all workspaces by default **[decision]**; §3 Workspace, §6: `mirror_enabled`
defaults to `true` in `POST /api/workspaces`, `PATCH /api/workspaces/{id} {label?, mirror_enabled?}`
with `mirror.set` to the worker, a "VS Code sessions" switch in the client; hub migration `0011`
turns the mirror on for existing workspaces. §9.1: no duplicates when the mirror is turned on for a
non-empty workspace: `mirror.set.sessions?: [{runtime_session_id, since_ts}]`, the first pass over
such a transcript does not send records with `timestamp` <= `since_ts`. §3 Session: the title from
the hub is either from the title model (`HQC_TITLE_MODEL`, `HQC_TITLE_BASE_URL`,
`HQC_TITLE_API_KEY`; the hub's `llm` module) or the first line of the first prompt,
`title_source: runtime`, only over `default`. §3 Worker, §5.1, §6: machine fingerprint
`machine_id` in `POST /worker/enroll` and `hello` (sha256 salted with `hqc`),
`workers.machine_hash`, `same_machine` and the order of `replaceable[]`. §3 Artifact, §12: daily
artifact cleanup (`HQC_ARTIFACT_RETENTION_DAYS`, default 30). §17.4: hub → worker frame
`session.cleanup` on archiving and deletion (inbox folders of inputs, scratch of sessions without
a workspace), cleanup of inbox folders older than 14 days at worker start. The `session.cleanup`
and `mirror.set.sessions` frames and the `hello.machine_id` field are described in §9.1, §17.4 and
§5.1; the frame lists of §5.3 and §5.4 were not edited in this version (the parallel task T22):
add references to them at the merge. Questions:
`docs/notes/t21-questions.md`.

### 0.22 → 0.23 (T22)
Hardening before rollout. §20: a table of limits against a compromised worker or its token and a malicious
user (§2.1): `worker.approval.request` (open per worker, lengths, `expires_at` is
rejected rather than truncated), push on worker requests once per 10 s, mirror sessions (per day
per worker and in total per workspace, `runtime_session_id` is a uuid, `cwd` up to 4096 bytes),
events that only the hub writes, the service session takes only `worker_log`, events of archived
sessions, `fs.*` — 4 at once per worker (`429 worker_busy`), takeover without `vscode_active_at`,
the watchdog step, limits on attachment downloads by the worker, display of large attachments by
the client. Hub migration
`0012`: `sessions.imported`, indexes `sessions_imported` and `sessions_turn_open`,
`vscode_active_at` for old rows with `locked_by: vscode`. Questions:
`docs/notes/t22-questions.md`.

### 0.21 → 0.22 (T20, takeover without forks)
§9.3 rewritten: the "30 s of silence" rule does not tell a closed session from an open idle one
(the 2026-10-23 case: a fork in the transcript, 37 hqc records outside VS Code's context). The
worker reads the registry of live Claude Code sessions `<config>/sessions/<pid>.json` and reports
transcript holders; a session with a holder is `locked_by: vscode` regardless of records; when the
holder disappears, `none` at once; with registry data `recent` does not lock; `POST /takeover`
with a holder gives `409 vscode_open` with `held_by`, without a holder on a worker with a registry
it goes through at once, the 30 s stay as the fallback rule without a registry; a holder appearing
while hqc drives the session gives `session_released` with `reason: vscode_open`, the agent stops
after the turn. A fork in the transcript gives `system session_forked` from the mirror.
§3 Worker: `capabilities.claude_code.sessions_registry`; §3 Session: `held_by`, `vscode_held_at`,
the meaning of `locked_by: vscode`. §4: code `session_forked` (`at_uuid`, `branch_from`,
`orphaned_turns`), `reason` of `session_released` (`vscode_open` | `foreign_write`). §5.2: holder
reports through the outbox. §5.4: `held_by?` in `session.state` and `mirror.event` (field absent /
`null` / object), a holder report is a `session.state` with a nil `session_id`,
`sessions_registry` in `hello`. §6: `held_by?` in the session object and `session.summary`,
`409 vscode_open`, `held_by?` on `409 locked_vscode`. §9.2: two client banners. Hub migration
`0010`: `sessions.held_by`, `sessions.vscode_held_at`. Fixture
`fork-2.1.308`. Questions: `docs/notes/t20-questions.md`.

### 0.20 → 0.21 (T19, worker re-enrollment and cleanup of revoked workers)
§5.1: enrollment with replacement: `replaceable[]` in `GET /api/workers/enroll/{code}`,
`replace_worker_id` in `POST /api/workers/enroll/confirm`, `replaced` in the confirm and poll
responses; on replacement the `worker_id` stays the same, the old token stops working on
confirmation, `name`/`os` come from the new enrollment, `revoked_at` and `boot_id` are reset,
`409 worker_online`, `400` on another user's worker, `409 conflict` on a second replacement of the
same worker.
§6: `GET /api/workers?include_revoked=1` and `status: revoked`, `revoked_at`; `DELETE
/api/workers/{id}?purge=1` (`409 worker_not_revoked`, `409 worker_has_sessions` with
`sessions_count`); error codes. §3 Worker, §10.2: revocation archives the service session
(`system worker_revoked`, requests get `approval.cancel {archived}` without a frame), replacement
unarchives it (`system worker_replaced` with `old_name`/`new_name`), the "Workers" group shows only
non-revoked ones. §4: codes `worker_replaced`, `worker_revoked`. §17.4: worker deletion. Hub
migration `0009`: `enroll_codes.replaced_name`, archiving of the service sessions of already
revoked workers. Questions: `docs/notes/t19-questions.md`.

### 0.19 → 0.20 (Origin behind a proxy)
§20: the `Origin` check on `/api/*` takes `X-Forwarded-Host` and the `HQC_PUBLIC_ORIGINS` list
into account; found on the production hub behind Caddy, where `Host` is rewritten to the
container's name and the client's WebSocket got 403.

### 0.18 → 0.19 (T15, import, mirror and takeover of Claude Code sessions; merged after T16 and T17)
§9 refined with the facts of 2.1.308 (T15 live runs, fixtures `vscode-takeover` and
`adapter-then-outside`): `claude -p` and `--resume` write one file, a block is a separate record,
`stop_reason` in jsonl, an empty `thinking` with a signature, `ai-title` is not written. §9.1:
candidates by `cwd` inside `allowed_roots`, import via `POST /api/workspaces` with
`mirror_enabled`, mapping of records to §4, a session for an unknown transcript, `locked_by` by
`recent`, authors from the hub, no push. §9.2: polling every 2 s **[decision]**, offsets and cache
in `$HQC_HOME/mirror`, the adapter's registry of its own uuids, own records are not emitted,
foreign ones are held back 5 s while the process is alive.
§9.3: `POST /takeover` (`409 vscode_active`, `worker_offline`, `archived`), `session.start mode:
resume`, `system session_taken_over`; `409 locked_vscode` with `hint: takeover`; release: the
agent stops after the turn, `system session_released`, `pending` inputs `failed`;
the rule for applying `locked_by` from `session.state`. §3 Session: `vscode_active_at`, the meaning
of `locked_by`, mirror session fields. §5.3: `workspace.list_candidates {request_id}`, `mirror.set`
in detail. §5.4: `mirror.event` with `title?`, `recent` and an `ack` reply. The `system` code
`session_released`, the `vscode_active` error and `hint` on `locked_vscode`, `GET
/api/workers/{id}/candidates` and `mirror_enabled` in `POST /api/workspaces` are described in §9
(`docs/notes/t15-questions.md`). Hub
migration `0008` (renamed from `0006` at the merge; `0006` is taken by T17, `0007` by T16):
`sessions.vscode_active_at`, an index on (`worker_id`, `runtime_session_id`).
Consolidation of T15–T17 (T18): what the three tasks deferred moved into the common sections.
§3 Session: `system`, the "no workspace" exception for the service session, `runtime: none`. §4:
the source of `user.message` from the mirror, `system` codes `session_released`, `worker_log`
(the `level` field), `session_taken_over` without the iteration note, the rule "the user's files
are always in `user.message.artifact_ids`, `attachment` is only `hqc_attach`" **[decision]** (T16,
question 1).
§5: error code `locked_vscode`, `hello.ack.system_session_id`,
`session.start.requested_by_owner`, `worker.approval.request`/`response` in detail, `result` to
`workspace.list_candidates`. §6: `mirror_enabled` in `POST /api/workspaces`, `GET
/api/workers/{id}/candidates`, `POST /takeover` with errors, the `system` field and `409
system_session` (now on `/takeover` too), `409 locked_vscode` with `hint`, `409 vscode_active`,
`400 unknown_artifact`, `worker_log` excluded from unread. §10.2: `/takeover` in the list of
what the service session forbids. §16: no push on mirror events. §19: what of iteration
2 is done.

### 0.17 → 0.18 (T16, artifacts; merged after T17)
§3 Artifact: `session_id` is the first session of the input, `storage_key` = sha256, uniqueness
of `(owner_id, sha256, name)`. §6: bodies of `POST /api/artifacts` (`file`, `kind?` → artifact
object, `200` on a repeat, `413 too_large`), new `GET /api/artifacts/{id}`, `GET
.../url` → `{url, expires_at}`, `400 unknown_artifact` in input, up to 32 artifacts. §8.1:
a paragraph on `.hqc/inbox`: download, name sanitizing, `info/exclude`, the "Attachments:" block,
the stream-json image content block **[verify]**, download errors. §12 with facts: on-disk
storage, the `kind` rule, link format and signature, `403`/`410`, response headers, HTML inline
only on `HQC_ARTIFACT_PUBLIC_BASE`, the hub's CSP with the artifacts domain's origin, the worker's
rights, client behaviour. Hub migration `0007` (renamed from `0006`, taken by T17, at the merge). Questions and deferred items:
`docs/notes/t16-questions.md`.

### 0.16 → 0.17 (T17, the worker's service session and non-session confirmations)
§3 Worker: `system_session_id`, when it is created (enrollment, migration `0006`, `hello`), where
it is visible (`hello.ack`, `GET /api/workers`). §10.2 rewritten with facts: the service session
(personal project, no workspace, `runtime: none`, `system: true`, `409 system_session` on input,
stop, retry, archiving and deletion, the "Workers" group in the client), frames
`worker.approval.request`/`worker.approval.response` through `PendingRequest` `worker_approval`
(only the worker owner answers, expiry, `worker_restart`, resending after `hello`),
confirmation of a new folder `workspace.use` with the cache `$HQC_HOME/known_workspaces.json` and
the `requested_by_owner` flag in `session.start`, worker logs `system worker_log` with `level` and
rate limiting, `worker_log` excluded from unread. §16: push on a worker request
in the service session, no push on `worker_log`. Sections that 0.17 also affects but that
were not edited in T17 (parallel tasks) are described in §10.2 and are to be carried over: §4
(`system` code `worker_log`, the `level` field), §5.3/§5.4 (new frames, `hello.ack.system_session_id`,
`session.start.requested_by_owner`), §6 (the session's `system` field, code `409 system_session`,
the unread rule), §3 Session (`runtime: none`), §3 PendingRequest (`kind:
worker_approval` was already in the list); carried over in 0.19 (T18). Questions:
`docs/notes/t17-questions.md`.

### 0.15 → 0.16 (T14, projects and several workspaces)
The iteration 1 simplification "a single workspace from the worker config" is lifted
**[decision]**. §3 Project: CRUD, the first workspace is the default, the personal project cannot
be deleted or renamed. §3
Workspace: a hub object, the worker knows only `allowed_roots`; `path` is canonical from
`fs.stat`; a folder belongs to one project: on a worker paths are unique and not nested. §3
Session: `workspace_id` is required on creation. §4: `system` code `workspace_unknown`. §5.3:
frames `fs.stat`, `fs.list_dirs` (the reply is a `result` with `request_id`), the worker checks
`workspace_path` against `allowed_roots`. §5.4: `capabilities.workspace_path` removed
from `hello`, `result` to hub requests. §6: REST for projects and workspaces, `GET
/api/workers/{id}/dirs`, choosing the session's workspace instead of a "default worker" (`409
no_worker` dropped), codes `worker_offline`, `workspace_conflict` (with a body), `workspace_in_use`,
`personal_project`, `400` for path checks, `504 worker_timeout`. §13.1, §13.2, §13.4: `dir`
via `fs.stat`, deleting the record, choosing a workspace. §14: `worker.toml` without `[workspace]`
(the old block is ignored). §19: part of iteration 2, "projects and workspaces", is done. Hub
migration `0005`: iteration 1 workspaces from `workers.capabilities`, sessions bound to them,
sessions without a known folder are archived with `system workspace_unknown`. Questions and
deferred items: `docs/notes/t14-questions.md`.

### 0.14 → 0.15 (T13)
Session title from the runtime. §3 Session: field `title_source` (`default` | `runtime` |
`user`), default title "Session N" instead of the one passed by the client, the rule "the runtime
does not rename a session the user has renamed". §5.4: `session.state.title?`.
§6: `title_source` in the session object. §8.1, §9: the `ai-title` record of a Claude Code
transcript (`aiTitle`), the adapter reads it after a turn. Checked on 2.1.308: in adapter mode
(`--input-format stream-json`) the CLI does not write `ai-title`, marked **[verify]** for
later versions.
Turn watchdog (§17.3, new item): `running` without worker events for longer than
`HQC_TURN_WATCHDOG_S` (900 s) gives `system turn_lost` "no events for N s", cancellation of open
requests with `timeout`, the turn is closed, `error` until the next input. §17.1: "the turn ended
with an error" is now cleared by a new input, not only by the next turn opening. §4:
`turn_lost` from the watchdog too. §17.1: the client's turn indicator from `state`, `status` in
the feed only for `error` with `detail`.

### 0.13 → 0.14 (T12, permission policy)
Iteration 1 is closed: on 2026-10-22, login through Access, installing the PWA and push on
approval.request were checked from a phone; SameSite Lax, Binding Cookie and Rate Limiting on
/worker/* are set in Cloudflare; HQC_VAPID_SUBJECT is set. The §19.1 criterion is met in full.
From the spike on Claude Code 2.1.308 (`docs/notes/claude-control-protocol.md`, "Permission
policy, 2.1.308", fixtures `*-2.1.308.jsonl`). §14: the `[permissions]` block of the local config
(`mode` ask/auto, `allow`, `deny` with a default set **[decision]**, rule validation)
moved from the environments of iteration 5 into the current version, three permission levels.
§8.1: `--settings` with these lists on every launch, including with `--restricted`;
`--permission-mode auto` with `mode = "auto"`; how the CLI applies allow/deny, that deny is
stronger than `always` and `auto`, what a denial looks like in stream-json; the adapter itself
denies `can_use_tool` under `deny`; the **[verify]** mark about `always` with
`--setting-sources user,local` is removed. §4: `approval.request.policy_hint?`, `system` code
`policy_denied`. §10.1: `auto_approve` does not see calls under `deny`. Questions and what was
not confirmed: `docs/notes/t12-questions.md`.

### 0.12 → 0.13 (T10, U+0000 in events)
Addition: the worker disables Claude Code's ListAgents and SendMessage by default,
`allow_native_messaging` in the worker config (§8.1).
A finding on the production hub (`docs/notes/deploy-findings.md`, item 7): a `tool.result` with
U+0000 in `result_preview` was not inserted into Postgres, the hub replied `internal` retryable,
and the worker resent the event forever. §5.2: an exception to "payload as received": the hub and
the Claude Code adapter replace U+0000 with U+FFFD; an event the database rejects because of its
data gets `invalid_frame` without retry; at most 10 retryable errors per event, then the worker
deletes it (the `attempts` column in the worker's outbox, added to an old database on open).

### 0.11 → 0.12 (T9, hardening before use)
Clarification after a live test: an `always` rule is written to `.claude/settings.local.json`
of the git repository root of the cwd, not of the workspace as such (§8.1).
From two security audits. §2.1: a precise statement of what the worker does not trust the hub
with (`allowed_roots`, environments, bindings, `allow_foreign_sessions`) and what it trusts it
with (identity, answers to permission requests), and the consequences of a hub compromise
**[decision]**.
§5.1: `hub_url` and `enroll` are `https` only (except loopback and `insecure_http`), no
redirects; `GET /api/workers/enroll/{code}` and showing the worker before confirmation;
`CF-IPCountry`; rejecting `hello` from a revoked worker. §6: a new route, the default worker
rule, limits on `title`/`model`/`client_op_id`, push errors (`400`, `409
push_limit`, `409 conflict`), a common JWT error text. §8.1: `--disallowedTools`,
`--append-system-prompt`, `--restricted`, `--setting-sources user,local`, an explicit
`--permission-mode` (`manual`/`default` per `claude --help`, **[verify]**), checking
`model` and rules on the worker, an environment allowlist, a canonical `cwd` re-checked,
`max_sessions`; no session rule "for the whole tool" for Bash. §8.4: the scratch folder.
§14: `max_sessions`, `insecure_http`. §20: `Origin`, security headers, limits on
`/worker/*` and WebSocket, the push service allowlist, JWKS when Cloudflare is unreachable.
Hub migration `0003` (`enroll_codes.country`, `client_ip`). Questions and deferred items:
`docs/notes/t9-questions.md`.

### 0.10 → 0.11 (T7, reconciling the hub, worker and client protocols)
The T2–T5 branches diverged in wire details; the 0.10 form (T2) won, the worker and the client
adapted, exceptions below. `hqc-proto` changes: `hello.boot_id`, `session.start.mode`,
the `answered_elsewhere` reason removed, new modules `client_frame` and `api` with examples
`client.*.json` and `api.*.json`; a new hub migration `0002` (`workers.boot_id`,
`sessions.start_sent`).
§5.1: the body of `POST /worker/enroll` is `{name, os, protocol_version}`, unknown fields are
ignored; the worker polls by `poll_token` (not `enroll_id`) and prints the link
`<hub>/#/enroll?code=`; the `hello` version is checked before strict parsing; outside macOS the
token is a 0600 file, libsecret/DPAPI later (also §18). §5.2: a worker restart is only a change
of `boot_id` (requests cancelled with `worker_restart` without a frame, unconfirmed answers reset,
`turn_lost`), the same `boot_id` cancels nothing; the former "does not distinguish" is dropped
**[decision]**; after `hello`, `session.start` (`mode: resume`) to sessions with a `pending`
input, an open request or an unconfirmed answer, then answers, then inputs;
the input `ack` and `agent.delta` bypass the outbox, the hub does not rely on their order (a late
`ack` does not open a turn that already has a final answer).
§5.3: `mode` (`new` | `resume`). §5.4: `boot_id`, the temporary
`capabilities.workspace_path` (TODO §19). §3: the `event_id` rule for a record with
several blocks (also §9.2); `Worker.boot_id`. §4: the list of `system` codes;
`answered_elsewhere` removed. §6: dev mode only on loopback (also §20); bodies and frames from
`hqc-proto`; flat `/api/ws` frames, `delivery` instead of resending the event,
`delivered_at?`; the session object; lists `{sessions}`, `{events, last_seq}`; the reply to
`POST /input` is `{event}` (201, a repeated `client_op_id` gives 200); `unread` counts only the
final `agent.message`; `public_key: null`. §8.1: interrupting a turn with `control_request
interrupt` with a fallback SIGINT after 10 s; read-only commands the CLI allows by itself;
`--resume`/`--session-id` depending on whether the transcript exists. §9: the exact folder
encoding rule (CLI 2.1.308) instead of a bare "not decodable". §10.1: a second answer gets `409
request_closed` without a cancel event; cancellations on `stop` are written only by the hub. §16:
push body `{session_id, type, title?, body?, tag?, payload?}` with the event in `payload`
(an exception to the principle: the client's shape). §17.3: `boot_id`; a process that dies within
the first 500 ms is a failed start. Questions and decisions: `docs/notes/t2..t5-questions.md`
("Resolved in 0.11").

### 0.9 → 0.10 (T2)
Recorded the hub's behaviour that the spec did not pin down. §5.1: HTTP worker enrollment
(`/worker/enroll`, `/worker/enroll/poll`, `/api/workers/enroll/confirm`), the token in
`Authorization: Bearer`. §5.2: `agent.delta` is confirmed with an `ack` without `seq`; an event
for a session of another worker is treated like one for an unknown session; `session.start`
before inputs on every connection, errors on `session.input`, one start attempt for a new input
after `failed`; `hello` counts as a worker restart (requests cancelled with `worker_restart`,
`system turn_lost`) **[decision]**. §6: `/api/ws` frames, the REST error format,
`before_seq`, idempotency by `client_op_id`, `GET /api/push/vapid` and
`DELETE /api/push/subscribe`, the definition of `unread`. §17.1: how the hub determines "a turn is
in progress". Questions and decisions taken: `docs/notes/t2-questions.md`.

### 0.8 → 0.9 (T1)
The `hqc-proto` schema checked against the spec, T1 questions closed. §3: `author` with
`kind: system` has no `id`; `delivery` and `delivered_at` are flat event fields. §4: the `?` mark
for optional fields (`in_reply_to`, `duration_ms`, `expires_at`, `caption`, `detail`,
`request_id` of `peer.message`, everything in `usage` except `input_tokens`/`output_tokens`); new
`tool.result.error_text?`; the shape `clarify.request.questions[{id, text, options?, multi?}]`
and `clarify.response.answers {id: string | [string]}`, `clarify.cancel {request_id, reason}`;
the event tolerance principle (an unknown `type` is stored as is, unknown enum values are
"unknown"). §5: the common shapes `ack {event_id?, seq?}` and `error {code,
message, retryable, details?}` with an initial list of codes; `hello.ack {protocol_version,
worker_id, server_time}` (§5.1); an `ack` to a duplicate carries the `seq` of the stored event, to
an unknown session `seq: null`; `ack {event_id}` to `session.input`; frames are strict,
an unknown one is `invalid_frame` (§5.2); `peer_prefix[{from, text, ts}]`, `model?`
in `session.start`, the exact fields of `approval.response`/`clarify.response`, there is no
`approval.cancel` frame: the hub sends `approval.response` with the `deny` option (§5.3, §10.1);
`runtime_session_id?` in `session.state` (§5.4). §8.1: the adapter maps the question `id`
to its text for `updatedInput.answers`. The hub stores the events' `payload` as raw JSON (`jsonb`
as is) and serves it to clients without reserializing from types (§5.2). Cancelling a `clarify`
by the hub: a `clarify.cancel` event and a `clarify.response` frame with `answers: {}`, the adapter
answers the runtime with `deny` (§5.3, §10.1).

### 0.7 → 0.8 (T0 findings in the text)
The discrepancies of 0.7 are closed in §8.1: the launch command has `--permission-prompt-tool
stdio` and `--allowedTools mcp__hqc` (checked with a live run: both `mcp__hqc` and
`mcp__hqc__*` work); the adapter's rule for `always`: only `addRules` with
`destination: localSettings` in `updatedPermissions`, `always` is offered only when they exist;
the echo of its own `control_response` is skipped, `ack` is by `user` with `isReplay: true`.
§7: hqc tools without confirmation; a new §7.2 "Requirements for the hqc MCP server".
§7.1: `MCP_TOOL_TIMEOUT` in ms is set to `(ask_timeout_s + 60) * 1000`; the idle timeout of
stdio MCP servers is 30 minutes (checked: a silent call is cut off after 1800 s,
progress every 60 s keeps it alive), hence the mandatory progress notifications in §7.2.
§19: iteration 3, the MCP server per §7.2. A new frame `mcp.cancel {request_id, reason}`
(§5.4): the worker closes a deferred `ask` if the runtime cancelled the call or the MCP process
exited (§7.2); the hub closes the `PendingRequest` with an error, writes a `system` with code
`ask_cancelled` into the sending session, and sends nothing to the target session (§5.2).

### 0.6 → 0.7 (T0)
Checked on Claude Code 2.1.307 (`docs/notes/claude-control-protocol.md`, fixtures
`*-2.1.307.jsonl`): the shapes of `can_use_tool` and of allow/deny/`updatedPermissions` answers,
`always` writes the rule to `.claude/settings.local.json` of the working folder (§8.1); the
`--replay-user-messages` echo carries the prompt's `uuid` from jsonl (§8.1, §9.3);
`AskUserQuestion` comes as `can_use_tool`, answers go in `updatedInput.answers` (§4, §8.1);
`MCP_TOOL_TIMEOUT` is in ms, default 1e8 ms (§8.1). **Discrepancies; the §8.1 text has not
changed yet:**
(1) `--permission-prompts host` alone is not enough: without `--permission-prompt-tool stdio`
requests are denied at once (`permission_denied`, `source: config`), and `AskUserQuestion` is not
in `tools`; (2) `updatedPermissions` built from all `permission_suggestions` also includes
`setMode acceptEdits` and session-wide `addDirectories`; for `always` only `addRules` with
`destination: localSettings` must be passed; (3) MCP tool calls also go through `can_use_tool`,
so hqc tools (iteration 3) will have to be allowed in advance.

### 0.5 → 0.6 (check against the code skeleton)
The `event` frame has no duplicate `session_id`: it is taken from the event (§5.4);
`runtime_session_id` is a string, not necessarily a uuid for ACP agents (§3 Agent);
the CLI version in the uuid checks corrected to 2.1.307 per the fixtures (§8.1, §9, §9.3).

### 0.4 → 0.5 (per reviews 5 and 6)
Checked on builds: stream-json message uuids match jsonl record uuids,
including `tool_result`; Hermes does not persist `mcpServers` from `session/new`; the
`hermes-acp` toolset has no `clarify`; Hermes permission options are dynamic; the Hermes version
is canary `4787e4d5`; `MCP_TOOL_TIMEOUT` exists in Claude Code, in Hermes
`mcp.tool_call` is 300 s. Changes: the mirror by `locked_by`, a shared deterministic
`event_id` for the adapter and the mirror (§9.2, §3, §8.1); `mcp.call`/`mcp.result` frames,
a deferred `ask` and resending after `hello` (§5.2, §3 PendingRequest); §2.1 narrowed,
`isolation` on `acp_agents`, `HERMES_ACP_SKIP_CONFIGURED_MCP` always (§2.1, §14);
the Hermes profile from the local binding, `foreign_profile` for foreign sessions (§11.1,
§14, §8.2.1); §8.2 split into generic ACP and Hermes: end of turn by `stopReason`,
ack after `session/prompt`, `fs`/`terminal` unavailable, permission options
from the agent, `session/load` by capability, checking `sessionId` after resume,
`usage` optional, diffs as artifacts, `acp_agent` in `session.start`,
absolute command path, capabilities in `hello` (§8.2, §5.3, §3 Worker); `clarify`
from Claude Code's `AskUserQuestion` (§4, §8.1); MCP tool timeout ≥
`ask_timeout_s` (§7.1); escaping `[hqc peer` on delivery throughout the text
(§7.1, §4); the `delivery: failed` policy after 3 failed starts, a
"retry" button (§5.2, §17.1); changing the runtime = a new runtime session (§3 Agent);
artifacts to the worker via `/worker/artifacts/{id}` without TTL, links to the client at
render time (§5.3, §12); `running` above `queued`, `queue_len` (§17.1); environment binding
by workspace path (§14); `hqc_ask` into one's own session is an error;
`share_local_models` (§8.5); `client_op_id` in POST (§6); the purpose of
`owner_is_worker_owner` (§5.3); send buffer: cleared on ack, limit 50/64 KB (§7.1);
ACP without a workspace is forbidden (§3, §8.4); "all runtimes on the worker" in §11.1; behaviour
on a protocol version mismatch (§5.1); §17.4 archiving and deletion; steer for
Hermes in iteration 4 (§19, §17.2); the "file → cwd" cache (§9); the term "turn" refined;
links in the header fixed; §2 and §7 rephrased; old history moved
to `SPEC-CHANGELOG.md`.

### 0.3 → 0.4 (prior art review)
ACP as the generic adapter instead of a hand-written `tui_gateway` for Hermes; hqc MCP via
`mcpServers` in `session/new`; runtime `acp` with `acp_agent`; Claude Code stays
on the native adapter; `client_op_id`; the prior art section and the rejected alternative
without a hub; the ACP v2 risk.
