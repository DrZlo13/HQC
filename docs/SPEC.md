# hqc — technical specification

Version 0.107, 2026-11-10. Status: the architecture agreed after six reviews and a survey of
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
  in events and the push subscriptions. The protection is the hub itself: Access, tunnel only
  (in OIDC mode since 0.68: the provider's sign-in, only the reverse proxy reaches the hub), no
  open ports (§20). Signing input and answers with device keys registered on the worker
  locally is iteration 3+.
- The hub **cannot**: go outside the worker's `allowed_roots` (also not through an agent's
  `hqc_attach` or the `files` of `hqc_send`/`hqc_ask`: the worker resolves and reads the path
  itself, §7; files that reach a worker from the hub are written only into its own
  `.hqc/inbox`, §5.3, §12; a clone goes only into a new folder the worker checks itself, from
  `https`/`ssh` or a `file` URL inside `allowed_roots`, and only a folder the worker created
  and listed itself is ever deleted, never one because the hub named it, §5.3, §13; since 0.58,
  T45; a worktree only of a git workspace the worker knows, into a new folder checked the same
  way, and it is removed only by `git worktree remove` in the origin the worker recorded, since
  0.60, T46; a moved session's transcript goes only to the place of a folder the worker accepts
  for `session.start`, only from where Claude Code wrote it inside `allowed_roots`, and never
  over another file, §8.1, since 0.75), pick an environment or a
  Hermes profile for a path the worker owner has not bound locally, hand a session an ssh key
  (since 0.57 only an environment's `ssh_keys` give a process an ssh-agent, and without them it
  gets none, not even the user's), or enable
  the machine's MCP servers. These are set only by the worker's local config (§14): since 0.56
  (T43) `session.start` carries no environment, the worker resolves it from the canonical
  workspace path on every start, and what the hub shows about environments is a display. Nor
  can it run a
  session in the `auto` permission mode on a worker with `[permissions] allow_auto = false`
  (since 0.42, T38): the worker runs `ask` and reports it. Where `auto` is allowed, a hub that
  switches a session to it gains nothing it did not have (it can answer `once` itself); the
  worker's `deny` stays in force in both modes. The same holds for hqc's allow rules of a folder
  (since 0.52, T48): standing answers the worker applies after its `deny`, never over it. In an
  environment with `project_settings` (since 0.83, §14) a repository's own hooks run outside the
  worker's `deny` and without an approval request, so a hub that clones a repository of its
  choice into one of that environment's folders runs that repository's code there.
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
  running by scheduling its own next turn. A message cannot raise permissions (since 0.43,
  T39; §7.1): a turn an agent starts in another session (a peer message, the prompt of
  `hqc_spawn`, a run of its schedule) runs in `auto` only if that session and every session
  up the chain run in `auto`, so a session in `ask` gets no work done without asking through
  one in `auto`; since 0.44 (T40) neither through a buffered send nor through a schedule
  made while it ran in `auto`. The hub computes the cap and the worker applies it; a compromised hub could
  lift it, as it can answer requests itself (above). On a worker with `allow_subagent_messages`
  (§8.1, since 0.84) an agent can also start a turn in another Claude session that accepts
  messages from other sessions (a local one of the same machine; per §8.1 **[verify]** also the
  account's cloud and Remote Control sessions), past the hub and these limits.
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
`email` (from the verified Cloudflare Access JWT, in OIDC mode the provider's ID token, §6),
`oidc_sub` (since 0.68, OIDC mode only: the provider's `sub` the user is bound to on the first
OIDC sign-in, §6; hub migration `0031_oidc_subject`), `display_name`, `api_keys`
(OpenRouter, Anthropic API; encrypted with the hub's master key, which lives in the hub's
env/file, not in the database), `timezone`, `personal_project_id` (the implicit personal
project, created on first sign-in; the personal assistant session lives in it), `theme`.

`theme` (since 0.51, T49) — the client's base palette: three dark ones, `warm` (default) |
`graphite` | `contrast`, and since 0.88 their light counterparts `warm_light` |
`graphite_light` | `contrast_light`; one for the whole app and the same on every device of the
user (`PATCH /api/me`, §6). The client applies it at start (until `/api/me` answers, the one
last applied on this device, kept in the browser's storage as `hqc.theme`; without it `warm`)
and after a change.

`timezone` (since 0.38, T35) — an IANA name, `UTC` until set; the default zone of new
schedules (§16). The client sets it to the browser's zone (`PATCH /api/me`, §6) when it starts
and the two differ.

### Worker
`owner_id`, `name`, `os`, `token_hash`, `status` (online/offline), `last_seen`,
`protocol_version`, `boot_id` (from the last `hello`, §5.2), `capabilities` (`claude_code: {version, sessions_registry?, moves?}`, `acp_agents[{name,
version, load_session, resume, image, isolation}]` — the result of each agent's `initialize`
when the worker starts), `local_models[]`, `share_local_models`, `allowed_roots[]`,
`environments[]` (since 0.56 `{name, allow_local_mcp?, permissions {unblock?, allow?, deny?}}`,
never the values of `env`; `default` is implied), `env_bindings[]` (`{path, environment}`; the
Hermes profile in iteration 5),
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
`session_template` (§13.3), `is_personal`, `color`, `icon`. A project's first workspace becomes its
`default_workspace_id`; when the default workspace is deleted, the earliest of the remaining
ones takes its place. The personal project (`is_personal`) cannot be deleted or renamed (`409
personal_project`); its other fields can change; any number of new projects can be created.
Deleting a project deletes its workspace records and sessions (with events, as in §17.4); agent
processes get `session.kill`; folders and transcripts on the workers are left untouched.

`color` and `icon` (since 0.51, T49) — the project's appearance in the client; the personal
project has them too. `color` is one of `teal`, `plum`, `blue`, `rose`, `moss`, `coral`, `cyan`,
`slate` (the client's `theme.ts`; no yellows, amber means "waiting for you"); not set, the API
gives the one at index FNV-1a-32(the id's 16 bytes) mod 8 of that list, so it is stable.
`icon` is `{kind: emoji, value}` (1–8 characters, at most 32 bytes), `{kind: letters, value}`
(1–2 characters, kept in upper case) or `{kind: image, url}`; absent, the client shows the first
letters of the name. The image is stored on the hub in its own table (not an artifact: it
belongs to no session, the artifact cleanup does not touch it, it goes with the project), one
per project; `url` is `/api/projects/{id}/icon?v=<first 16 hex of its sha256>`. Setting an emoji
or letters drops a stored image. Limits: §12.

### Workspace
`project_id`, `worker_id`, `path`, `label`, `environment` (a name; a display of the binding
from the worker's local config: since 0.56 the hub sets it from the worker's `env_bindings` when
the workspace is created and on every `hello`, by the rule of §14; the worker never reads it), `ephemeral`, `created_by_hqc`, `kind`
(dir/clone/worktree), `origin_workspace_id` and `branch` (since 0.60, T46; hub migration `0029`),
`mirror_enabled` (since 0.23 `true` by default: the
Claude Code session mirror is on for every workspace; the switch is `PATCH`, §6, §9.2), hqc's allow
rules, `state` and `error` (since 0.58, T45; hub migration `0028`).

`state` — `ready` (the folder exists; every workspace before 0.58), or for a folder hqc makes
(`kind: clone` or `worktree`, `created_by_hqc: true`) `creating` while the worker clones or adds
the worktree and `failed` when it
could not, with the worker's reason in `error`; only the record is left then (the worker removed
what it had created). Only a `ready` workspace takes sessions (`409 workspace_not_ready`) and gets
`mirror.set` and `workspace.rules`; a `creating` one is not deleted (`409 workspace_creating`).

`origin_workspace_id` (since 0.60, T46) — for `kind: worktree`, the workspace it is a git worktree
of: a `ready` workspace of the same project and worker that is not a worktree itself (decided for
T46: one level, so the tree in the client is two levels and deleting goes worktrees first). While
any worktree record names it, the origin is not deleted (`409 workspace_in_use`, §6). `branch` —
the worktree's branch, or the branch a clone was asked for (T45's `clone_branch`, renamed by
`0029`).

`ephemeral` (since 0.61, T47) — a worktree `hqc_spawn` made with `isolation: "worktree"` for the
session it started (§7); otherwise like any worktree (sessions can be started there, the same
deletion). It is **never deleted automatically**. `marked_for_deletion` — not stored: the hub
computes it when serving, true for an `ephemeral`, `ready` workspace without a session that is
not archived. So archiving its last session marks it, unarchiving (or a new session there) clears
the mark, and a worktree whose spawn gave up before the session was created (§7) is marked from
the start. The client shows a marked workspace for deletion (§6), and deleting it goes through
the dialog of §13.2.

hqc's allow rules (since 0.52, T48; decided 2026-10-31: per folder, not per project or worker) —
standing answers of the workspace owner (the worker owner in iteration 4) to the permission
requests of every session in the folder, stored on the hub (table `workspace_rules`, migration
`0025`) and applied by the worker in `can_use_tool` after its `deny` (§8.1, §14). A rule is a
strict subset of Claude Code's syntax, so the worker's matcher reads every rule exactly: `Tool`
(a whole tool, `mcp__server` or `mcp__server__tool`; never `Bash` or another tool that runs
commands), `Bash(<words>)` (exactly that command), `Bash(<words> *)` or `Bash(<words>:*)` (the
command with any arguments); `<words>` are words of letters, digits and `-_./=+,@%:~` separated by
single spaces, without quotes, shell operators or a `*` other than the final one
(`hqc_proto::rules`). Limits: §20. The rules never go into `--settings`, so a change needs no
restart and reaches running sessions at their next request. The rules screen (§6) shows them with
the rules the worker reads for the folder (`worker.toml`, `~/.claude/settings.json`, in an
environment with `project_settings` (§14) the folder's `.claude/settings.json`,
`.claude/settings.local.json`); the hub keeps no copy of those.
A workspace is a **logical** boundary: all hqc operations (session cwd, inbox, mirror,
deletion) are confined to its path. It gives no enforced file system isolation.
A workspace is a hub object **[decision]**: all the worker knows about it is that its path lies
inside `allowed_roots` (checked on `fs.stat` and on every `session.start`, §5.3), and since 0.58
whether it created the folder itself (`created_workspaces.json`, §14).
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
(unique in the project, does not change on rename; a move to another project keeps it when it
is free there and appends `-2`, `-3`… otherwise), `agent`,
`auto_approve`, `wake_on_send` (true by default since 0.55, T52, and on every session made before it, hub migration `0027`; false before), `locked_by` (none/hqc/vscode),
`last_seq`, `archived`, `archived_at` (nullable; since 0.53, T50: when it was archived, cleared
on unarchive; null for sessions archived before hub migration `0026`, the client then shows no
time; in the API session object as `archived_at?`), `vscode_active_at` (nullable; when the hub last received a mirror
event with `recent` — a write to the file within the last 30 s; it is how quiet is measured for
a takeover without registry data, §9.3; not part of the API session object), `held_by` (nullable
`{entrypoint, status}`: a live Claude Code process outside hqc that has the session's transcript
open, according to the live sessions registry on the worker, §9.3; in the API session object and
in `session.summary` it is the field `held_by?`), `vscode_held_at` (nullable; since when the
transcript has been held; not part of the API session object), `system` (bool: the worker's
service session, §10.2; the API session object carries only `system: true`), `spawned_by`
(nullable; since 0.37, T34: the session whose agent created this one with `hqc_spawn`, §7;
cleared when that session is deleted) and `spawn_chain_id` (nullable: the chain of that call,
§7.1, kept for the spawn limits of §20; not part of the API session object),
`permission_mode` (nullable `ask` | `auto`; since 0.42, T38: the mode hqc asks the worker to
run the agent in, chosen on creation and switched with `PATCH`; null — the worker's
`[permissions] mode`, §14) and `effective_permission_mode` (nullable: the mode the worker last
reported the agent runs in, `session.state.permission_mode`, §5.4; what the client shows),
`permission_capped_by` (nullable; since 0.43, T39: while the worker runs the current turn in
`ask` under its input's cap, §7.1, the address of the session up the chain whose `ask` set it,
from `session.state.capped_input`, §5.4), `subagents` (since 0.70: the subagents the agent
runs now, from `session.state.subagents`, §5.4; 0 after a worker restart). In
`ask` a tool call outside the worker's rules is asked about (§10.1); in `auto` Claude Code's
classifier decides and never asks (§8.1); the worker's `deny` stops a call in both. `state` and
`queue_len` (§17.1) are **not stored**; the hub computes them when serving. In the first version
a session has exactly one agent; people's access to a session is determined by Grant only.

Title: creating without `title` gives "Session N" (N — the session's ordinal number in the
project, archived ones included; `slug` — `session-N`) with `title_source: default`; `title` on
creation, `PATCH title` and an agent's `hqc_rename` (§7) set `user`. The hub applies a title from the runtime
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

Move (since 0.75): a Claude Code session continues in another folder of its worker, usually in
another project, as the same conversation (`POST /api/sessions/{id}/move`, §6; the agent itself
with `hqc_move`, §7, since 0.76). Its `project_id`, `workspace_id`, `cwd` (`.`) and possibly
`slug` change, so its address `<project-slug>/<session-slug>` (§7) changes too; the old
project's saved list order forgets it (a move to another folder of the same project keeps its
place there). Its `runtime_session_id` stays: the worker moves the transcript to the new folder
(§8.1). Not to another worker.

### Agent (embedded in Session)
`runtime` (claude_code / acp / bare / none; `none` — only for the worker's service session:
there is no agent and no process starts, §10.2), `acp_agent` (the agent's name from the
worker's local config, for `acp`), `model` (since 0.74 also switched with `PATCH`, §6),
`runtime_session_id` (a string: the jsonl uuid for
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
in the same namespace; the rule is mandatory for all three sources (§9.2); since 0.98 the
question card the mirror builds from block `i` gets v5 of `"<uuid>/<i>/clarify.request"` and
`"<uuid>/<i>/clarify.response"` (§9.1);
for everything else — v7 from the sender), `seq` (assigned by the hub, monotonic within the
session), `ts`, `author` (`{kind: user|agent, id}` or `{kind: system}` without `id`; for an
agent `id` = its `session_id`), `type`, `payload`, `chain` (`{chain_id, depth, hops,
permission_cap?, capped_by?}` for agent→agent chains, otherwise null; the cap since 0.43, §7.1), `client_op_id` (echoed back to the client), `delivery` and
`delivered_at` — two flat event fields, only on `user.message` and `peer.message`:
`delivery` ∈ `pending | delivered | failed`, `delivered_at` — the delivery time.

### ReadCursor
`user_id`, `session_id`, `last_read_seq`. The source of "unread" (the rule is in §6).

### Favorite
Since 0.74: `user_id`, `session_id` | `project_id`, `created_at` — a session or a project the
user starred; two tables, a row goes with its user, session or project (hub migration
`0033`). Favorites are the user's own: another user's star on the same object is never shown,
and starring does not touch the session's `updated_at`. The session object, `session.summary`
and the project object carry `favorite_at?` (the row's `created_at`) for the requesting user.

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
(approval/clarify/ask/worker_approval/move/mcp_call), `session_id`, `request_id`,
`payload`, `expires_at`, `from_session_id`, `from_event_id`, `worker_id`
(for ask and mcp_call: whom to give the result to), `result` (filled in when the answer has
been received but not yet delivered to the worker).

An `ask` (since 0.35, T32) is a row of the **asking** session: `session_id` — the asking
session, `request_id` — the `mcp.call`'s, `worker_id` — the asking session's worker,
`payload` — `{target_session_id, event_id, to}` (`event_id` — the `peer.message` it put
into the target, `to` — the target's address; since 0.47, T42, `joined?` — the `event_id`s of
the target's `hqc_send`s to the asker that go with the reply, §7.1), `expires_at` — the ask's
timeout, `result` —
the `mcp.result` frame once it is answered, timed out or closed by a worker restart (§7.1).
`from_session_id`/`from_event_id` stay unused; `mcp_call` is not used: `hqc_send` keeps no
row of its own (§5.2).

A `move` (since 0.76) is a row of the session whose agent's `hqc_move` waits for the owner's
answer (§7, §10.1): `request_id` — the `mcp.call`'s, `worker_id` — the session's worker,
`payload` — the hub's `approval.request` in the session's feed plus `move: {path, project,
turn}` (the folder, the call's `project`, the chain of the call's turn, §7.1), no `expires_at`
(the call itself ends at the runtime's tool timeout, `ask_timeout_s` plus a minute, §7.2, and its
`mcp.cancel` closes the row),
`result` — the `mcp.result` frame once the move was carried out or refused, the owner denied it
or the hub cancelled the request. An allowed move closes the row first, without a `result`, and
fills it in when the move is done. On start, before it serves anything, the hub finds every
such row closed without a `result` whose session's feed has the owner's `approval.response`
with `once` (the previous process stopped while it carried the move out; a row a cancellation
closed without a frame has no such answer), keeps them in memory by the row's `worker_id`, and
carries each out once that worker sends `hello`, when the move can be done; rows of this
process are never among them. A hub that failed to store the result carries the move out again
at once. Either way a move already done answers as a repeat, and the input after it is stored
once (§7).

### ScheduledMessage
`owner_id`, `session_id` or (`project_id`, `workspace_id`, template), `cron` or `at`,
`timezone`, `message`, `enabled`, `last_run`, `catch_up`.

Since 0.38 (T35; table `scheduled_messages`, migration `0018`): `session_id` only — the
project-template variant waits for session templates (backlog); exactly one of `cron` (5
fields) and `at` (a time); `timezone` — an IANA name, the owner's `timezone` when not given;
`next_run` — when it is due next (UTC), null once it will not run again (an `at` that ran or
was skipped); `catch_up` false by default. Deleting the session deletes its schedules.
Since 0.44 (T40, migration `0023`): `created_by` — the session whose agent made the schedule
(`hqc_schedule`), not part of the API object; null for a person's schedule, and once that
session is deleted (the schedule stays). Its runs are capped by that session's mode (§16).

## 4. Event schema

One schema for all runtimes. The adapter translates the runtime's stream into these events;
the client renders only them. The "source" column is the component that produces the event;
`author` is set per §3. `?` marks an optional field: it is absent when there is no value
(`null` reads the same).

| type | payload | source |
|---|---|---|
| `user.message` | `text` (since 0.69 the client shows it as typed, line breaks kept, without Markdown), `artifact_ids[]`, `schedule_id?` (since 0.38: sent by the hub cron for that `ScheduledMessage`, §16; the client marks it "scheduled") | client; hub cron (§16, authored by the owner; since 0.39 the run of an agent's schedule has its `chain`, §7.1); hub after `hqc_move` (§7, since 0.76: authored by the owner, with the call's chain); mirror — prompts from the Claude Code transcript (§9.1) |
| `agent.delta` | `text` | adapter; transport only, not written to the database |
| `agent.message` | `text`, `final`, `in_reply_to?` (event_id of the input that started the turn), `runtime?` (since 0.69: `true` for a reply the runtime wrote itself instead of the model — an API error, "Credit balance is too low"; absent when false) | adapter. `final: true` — end of the turn; `final: false` — text between tool calls. A `runtime` message is held and ended like any other text (§8.1), so it is the turn's final message when it ends the turn; the client shows it as a service line, not as the agent's text. Since 0.54 (T51) the final message that answered an `hqc_ask` carries, from the hub to clients only, `reply_delivered_at` beside `delivered_at` once the asking worker acknowledged the reply (§6, §7.1) |
| `agent.thinking` | `text` | adapter, if the runtime provides it |
| `tool.call` | `call_id`, `tool`, `args_preview` | adapter |
| `tool.result` | `call_id`, `result_preview`, `error` (bool), `error_text?` (short error text), `duration_ms?`, `artifact_id?` (a diff or a large result, §12) | adapter |
| `approval.request` | `request_id`, `tool`, `description`, `options[{id, label, canonical}]`, `expires_at?`, `policy_hint?` (allow/deny), `rule_suggestions?[{part, rule?}]` | adapter; `options` are **the agent's real options**, `canonical` ∈ once/session/always/deny/deny_always for the client's buttons; `policy_hint` — the call matches an `allow` or `deny` rule of the worker's `[permissions]` block (§14); no field — it matches neither; the client labels the card with it; `rule_suggestions` (since 0.52, T48) — the hqc rules the worker suggests for "Allow in this folder" (§10.1): one per simple part of a Bash command (`rule` absent when it has none for that part), the tool name for another tool; absent for a session without a workspace and for a command the worker's matcher cannot split |
| `approval.response` | `request_id`, `option_id`, `by` (user id or `auto`), `message?` (the user's text instead of the call, only with a `deny`/`deny_always` option, §10.1) | client, or the hub with `auto_approve` |
| `approval.cancel` | `request_id`, `reason` (timeout/stopped/worker_restart/archived) | adapter, worker or hub. An answer from another device is not a cancellation: the first answer closes the request, the second gets `409 request_closed` (§10.1) |
| `clarify.request` | `request_id`, `questions[{id, text, header?, options?: [string], option_descriptions?: [string], multi?: bool}]` | Claude Code adapter: the `AskUserQuestion` tool arrives as `can_use_tool`, the answer goes into `updatedInput.answers` (verified on 2.1.307 with `--permission-prompt-tool stdio`, §8.1). Since 0.69 `header` is the question's short label and `option_descriptions` what each option means, in the order and number of `options`, `""` for an option without one; each is absent when empty. The client shows `header` above the question, each description under its option, and an "Other" field: its text is the answer (with `multi`, an item of the array beside the checked labels); "Other" chosen with nothing typed is sent as `"Other"`, as the VS Code tab does. ACP agents have no source for it |
| `clarify.response` | `request_id`, `answers` (`{<question id>: string \| [string]}`) | client |
| `clarify.cancel` | `request_id`, `reason` (the same set as for `approval.cancel`) | adapter, worker or hub |
| `attachment` | `artifact_id`, `kind`, `name`, `caption?` | the agent via `hqc_attach` (since 0.34, T31): the hub writes it, `author: {kind: agent, id: <session id>}`, `kind` and `name` of the artifact; the user's files go into `user.message.artifact_ids` (rule below) |
| `peer.message` | `from_session_id`, `from?` (the sender's address `<project-slug>/<session-slug>` when it sent, since 0.35), `text`, `mode` (send/ask), `request_id?` (for ask: the `mcp.call`'s), `artifact_ids[]?` (since 0.46, T41: the files the sender gave with `files`, artifacts of the owner; absent when none; the target's worker downloads them like a user's attachments, §5.3, §12) | the hub on `hqc_send`/`hqc_ask`/`hqc_spawn` (since 0.37, the new session's first input); `author` is `{kind: agent, id: <the sending session>}`, `chain` is set (§7.1); `delivery: pending` in the input queue, none while in the send buffer (§7.1) or (since 0.47, T42) when it goes with the reply to the target's ask instead (§7.1); since 0.54 (T51) a buffered one gets `delivered_at` (still without `delivery`) when the input that carried it is acknowledged |
| `usage` | `input_tokens`, `output_tokens`, `cache_read?`, `cache_write?`, `cost_usd?`, `session_cost_usd?`, `model?` | Claude Code adapter on `result` (since 0.69 not on one with `num_turns: 0`, no tokens and no cost, §8.1); for ACP agents optional and without `cost_usd` (ACP v1 has no standard usage). Since 0.28 (T27): `cost_usd` is the cost of this turn only; `session_cost_usd` is the runtime's own running total of the whole runtime session after the turn (Claude Code: `result.total_cost_usd`, carried over `--resume`), absent if the runtime has none (§8.1). The client's header shows the latest `session_cost_usd`, else the sum of `cost_usd` |
| `status` | `state` (§17.1), `detail?`, `in_reply_to?` (since 0.69: on the `idle` that ends a turn, the event_id of the turn's input once the runtime echoed it (§8.1; in 0.69 the input the adapter sent last once echoed), as on `agent.message`; a turn the runtime starts by itself after a background task carries the input before it; absent on other statuses and from older workers; the hub fails an `hqc_ask` by it, §7.1) | worker or hub; the Claude Code adapter sends `idle` with `in_reply_to` on `result` (§8.1) |
| `system` | `text`, `code`, `level?` (`warn` \| `error`, only on `worker_log`) | hub or worker; codes below |

`system` codes (the list is open; the client shows an unknown code as text):

| code | source | meaning |
|---|---|---|
| `session_created` | hub | the session was created (for one created by the mirror: "Session imported from Claude Code", §9.1) |
| `turn_lost` | hub | the turn was lost: the agent process stopped mid-turn, the worker restarted, or there have been no turn events from the worker for a long time (watchdog, "no events for N s"; §17.3) |
| `agent_exited` | worker | the agent process exited on its own with an error or mid-turn (§17.3) |
| `policy_denied` | worker | a tool call was rejected by a `deny` rule from the worker's `[permissions]`, without asking the owner (§8.1, §14); the client shows "Blocked by worker policy" |
| `task_notification` | worker (adapter; mirror) | since 0.69: a background task of the agent (a shell command, a subagent) finished or stopped; `text` is the runtime's summary of it (§8.1, §9.1). It comes when the task ends, before the answer of the turn the runtime then starts by itself. The client folds it into a "Background task" line with the first line of the text; opened, it shows all of it |
| `context_compacted` | worker (adapter) | since 0.69: the runtime compacted the session's context (`/compact`); `text` "Context compacted" (§8.1); the client draws it as a divider |
| `agent_start_failed` | hub | the agent failed to start 3 times in a row (or non-retryably); `pending` inputs became `failed` (§5.2) |
| `input_rejected` | hub | the worker rejected an input other than as a failed start; the input became `failed` (§5.2) |
| `ask_cancelled` | hub | in the asking session: a deferred `hqc_ask` was closed by `mcp.cancel` from the worker (§5.2), since 0.35 |
| `peer_evicted` | hub | in the target session: the send buffer was over its limit and its oldest messages were dropped unread (§7.1), since 0.35 |
| `session_taken_over` | hub | the session was taken over (`POST /takeover`, §9.3) |
| `forked_from` | hub | since 0.80: the end of a forked session's copied history (§6 `POST /api/sessions/{id}/fork`); `text` "Forked from <address> before the message of <time>" |
| `session_moved` | hub | since 0.75: the session moved to another folder and project (`POST /api/sessions/{id}/move`, §6, or `hqc_move`, §7); `text` names the old project, the new one and the folder |
| `session_released` | hub | the session was continued or opened outside hqc (VS Code, `claude` in a terminal): the worker stopped the agent at once, interrupting a turn in progress (since 0.30), `pending` inputs became `failed`; field `reason`: `vscode_open` — a live foreign process holds the transcript, `foreign_write` — the file was written from outside (§9.3) |
| `session_forked` | worker (mirror) | a fork in the transcript: the writer continued from an earlier record, and the other line's turns did not get into its context; fields `at_uuid` (the record with two lines), `branch_from` (`vscode` \| `hqc` — who started the new line), `orphaned_turns` (turns in the other line). The client draws it as a line with a warning (§9.3) |
| `transcript_missing` | worker (adapter) | since 0.100: a session with `session.start.has_history` has no transcript to resume (Claude Code deletes transcripts older than its `cleanupPeriodDays`, 30 days by default), so the agent starts without the earlier conversation (§8.1); the feed before it stays |
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
  §7); the client does not write `attachment` events of its own. Since 0.46 (T41) an agent's
  files for another session go into `peer.message.artifact_ids[]` the same way, and the
  `attachment` events of a turn that answers an `hqc_ask` go back to the asker with the
  reply (§7.1).
- `*_preview` fields are cut by the worker to 4 KB; the full tool result is not sent to the hub,
  except diffs and results saved as artifacts.
- `agent.delta` goes to clients over WebSocket and is not saved; `agent.message` is saved. The
  hub keeps the text of a session's deltas in memory until its `agent.message` or a `status`
  other than `running` (up to 512 KiB, and not past the worker's disconnect), so a client that
  subscribes mid-answer gets all of it (§6, since 0.72).
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
  With the code the hub stores `CF-IPCountry` and the request address (limits, §20); with
  `HQC_CLIENT_IP_HEADER` (since 0.68, §20) it does not read `CF-IPCountry`, which a client
  behind another reverse proxy could set, and there is no country.
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
- Since 0.87 the worker keeps at most 256 outbox entries un-acked: with that many in flight it
  reads the hub's frames (the acks) before sending the next one, and when no answer frees a
  slot within its dead time (60 s, pings do not count) it drops the connection and sends the
  outbox again after reconnecting. The window counts on the hub answering each outbox entry
  under its frame id (`ack`, `error` or `mcp.result`); an entry it leaves unanswered (a holder
  report whose database write fails) holds its slot until the connection ends. A worker that
  sent a large outbox (40 000 mirrored records after an import) without reading filled the
  hub's bounded send queue (§20); the hub dropped it, and both sides stayed blocked writing to
  a socket neither read, so the worker never reconnected.
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
- A session's `session.state` bypasses the outbox too, but since 0.75 the worker sends it only
  after the outbox entries written before it, also in what it sends at once when it stops. The
  hub ends an open turn as lost on `stopped` (§17.3), and a stop right after a turn (a move,
  §5.3) used to reach it before that turn's final `agent.message` in a few restarts of a
  hundred.
- An event for a session of another worker is handled as for an unknown one: `ack` without `seq`.

Hub → worker:
- The hub stores `user.message` and `peer.message` with `delivery: pending` and sends them as
  `session.input`. The worker answers `ack {event_id}` after handing the input to the runtime
  (Claude Code — after the `--replay-user-messages` echo; ACP — after sending `session/prompt`).
  Since 0.70 `ack {event_id, joined: true}` says the input went into the turn already running
  (§17.2): the hub marks it `delivered` and does not open a turn for it (the `ack` goes past the
  worker's outbox and may come after that turn's final answer).
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
  the reply to the call frame, under its `id`. Since 0.61 (T47) `spawn` with `isolation:
  "worktree"` waits for the worktree (§7): its `mcp.result` also comes under the call frame's
  `id`, but once the worker made the worktree; the hub reads the worker's other frames
  meanwhile (its `fs.stat` answers and `workspace.created` come on the same connection). If
  the connection closes first nothing is sent, and the call comes again after the next `hello`
  (below). `ask` is deferred: the hub answers the call
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
  the tool), `hqc_schedule` its schedule (since 0.38); a repeated `hqc_send` answers the same `{to, delivery}` from that event (since 0.47
  `with_reply` when an ask's `payload.joined` names it, §7.1), a
  repeated `hqc_ask` finds its `PendingRequest`: still open — `ack {}` again, closed — its
  stored `mcp.result` under the repeat's frame `id` (since 0.46 with the reply's `files`, which
  the worker downloads again into the same folder). `hqc_spawn` (since 0.37) keys the new
  session's id (`hqc_spawn` as the tool) and its first `peer.message` (as `hqc_send`): a
  repeat finds the session, stores the message if the first call did not get that far, and
  answers the same `{address}`; since 0.61 (T47) the ephemeral worktree of a spawn with
  isolation is keyed too (`hqc_spawn worktree` as the tool), so a repeat waits for the same
  worktree instead of making another. `hqc_rename` (since 0.103) keys nothing: a repeat
  writes the same title again. The same `request_id` from another
  session is another call.
- A deferred answer (since 0.35): the hub keeps the `mcp.result` frame in
  `PendingRequest.result` and sends it to the asking session's worker, now if it is
  connected, else after its next `hello` (no `session.start` is needed: the connection routes
  `mcp.result` itself), until the worker acks it. Since 0.46 (T41) the result of `hqc_ask` may
  carry `files: [{artifact_id, name, kind}]`; the worker acks the frame at once, downloads the
  files into the asking session's inbox and only then answers the MCP call, with local paths
  instead of the ids (§7). A worker restart (a new `boot_id`) closes
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
  target session, whose turn continues; for a deferred `hqc_move` (since 0.76, §7) it closes
  the open `move` request with `approval.cancel {stopped}`, and nothing moves. For a fast call
  it only acks (T30).

### 5.3 Hub → worker frames
- `session.start` {session_id, mode, runtime, acp_agent?, model?, workspace_path | null,
  cwd, runtime_session_id?, system_prompt?, disallowed_tools[], restricted,
  auto_approve, session_token, owner_is_worker_owner, ask_timeout_s, requested_by_owner?,
  permission_mode?, allow_rules?[], fork?}
  — `mode`: `new` | `resume` (§5.2); `fork` (since 0.80) — {runtime_session_id, at_event_ids[]}
  of a session forked before a message of its user (§6): the Claude Code adapter, while the
  session has no transcript yet, reads transcript `runtime_session_id` in the session's own
  folder, finds the record holding the first of `at_event_ids` (by the event ids of §3, newest
  first) and starts the fork there (§8.1); with none of them in it the start fails; once the
  transcript is there, the start resumes it; `has_history` (since 0.100) — `true` once the
  session's feed has an `agent.message`, absent otherwise: the runtime should have a transcript
  to resume (§8.1 `transcript_missing`); `allow_rules` (since 0.52, T48) — hqc's allow rules of the
  session's workspace (§3 Workspace), absent when none; the worker keeps the latest list per
  canonical workspace, shared by its sessions there, and drops a rule that is not valid hqc
  syntax; `permission_mode` (since 0.42, T38) — `ask` | `auto`, the
  session's `permission_mode` (§3); absent — the worker's `[permissions] mode`; the worker runs
  `auto` only with `allow_auto` (§14) and reports the mode it runs in `session.state`; `requested_by_owner` (sent only as `true`) — the worker
  owner added the session's workspace: no confirmation of a new folder is needed (§10.2); the worker calls the adapter's `start` or `resume` (§8);
  on `new` the Claude Code adapter still checks for a jsonl with this uuid and, if there is one,
  continues it with `--resume` (§8.1); `owner_is_worker_owner` is needed by the worker for `allow_foreign_sessions`, the choice
  of a Hermes profile and admitting `isolation: none`; `ask_timeout_s` — to set the
  runtime's MCP tool timeout (§7.1); without `model` the worker uses the runtime's default
  model. `workspace_path` is the `path` from the session's workspace record; the worker accepts
  it only if the path, with symlinks resolved, is a directory inside `allowed_roots`, and
  checks `cwd` against it (§8.1), otherwise `error {invalid_frame}`. The session's environment
  is the binding of that canonical path in the worker's local config (§14), resolved by the
  worker on every `session.start`; until 0.56 the frame had an `environment` the worker
  checked, now a worker ignores one an older hub still sends. A `session.start` for a running
  agent with another `workspace_path` or `cwd` is a move (since 0.75, §6): the worker checks the
  new folder as above, asks for the owner's consent to it as for a new session (§10.2), acks,
  and restarts the agent there: at once when no turn runs, a turn the runtime started by itself
  included (Claude Code after a background task's end, §8.1), no subagent runs (`subagents`,
  §5.4), no input is written into the running turn and no switch to `ask` is pending, otherwise
  at the end of the first turn after which all of these hold (after a background subagent the
  runtime runs a turn of its own on its result, §8.1, so the subagent's end does not move the
  agent: the CLI reports it before that turn begins), or, when no such turn comes, at the next
  input that finds them all holding; the same folder again before that cancels the waiting
  move. Meanwhile no input starts a turn in the old folder or is written into the running one
  (§17.2): it waits and goes to the new process, or, the move cancelled, to the running one.
  Background commands and monitors of the agent itself do not hold
  the move and stop with the process; when it stopped any, every input but a slash command
  the runtime is given in the new folder, until it takes one, begins with a line telling the
  agent how many (`[hqc] Moving this session to <folder> stopped N background task(s) …`, then
  `---` and an empty line, before the block of §7.1), but not one written into a turn whose
  input has it; the hub's history keeps the input as sent. Before every start
  of a Claude Code agent its transcript is moved to the place of the working directory (§8.1).
- `session.input` {session_id, event_id, text, artifact_ids[],
  peer_prefix[{from: "<project-slug>/<session-slug>", text, ts, event_id?, artifact_ids[]?}]} —
  the worker downloads the artifacts via `GET /worker/artifacts/{id}` with its token
  (no TTL) into `<workspace>/<cwd>/.hqc/inbox/<event_id>/` and substitutes the paths; the
  `.hqc/` folder is added to the workspace's `.git/info/exclude`. `peer_prefix` — accumulated
  `send` messages (§7.1); since 0.46 (T41) each with its `event_id` and its files
  (`artifact_ids`, absent when none), which the worker downloads into
  `.hqc/inbox/<that event_id>/` and lists after that message's text in the block, the limit of
  500 MB per input (§20) counting them together with the input's own; a file the hub does not
  give fails the input as for its own files. `peer?` {from, mode, request_id?} (since 0.35) — only for a
  `peer.message`: the sender's address, `send`/`ask` and the ask's `request_id`, from which
  the worker writes the header (§7.1); `text` is the message's text without it.
  `permission_cap?` (since 0.43, T39) — `ask` | `auto`, the cap of the input's chain (§7.1): with
  `ask`, the worker runs this input's turn in `ask` whatever the session's mode. It only ever
  lowers: a session in `ask` stays in `ask` under an `auto` cap, and the worker's own rules
  (`allow_auto`, `deny`) are untouched. Before the runtime gets the input, a running process
  in `auto` is switched to `ask` and the input waits for the runtime's confirmation (Claude
  Code: the answer to `set_permission_mode`, §8.1); with no confirmation in 10 s, or a runtime
  that cannot switch, the process is restarted in `ask` (resuming its transcript). A process
  started for a capped input starts in `ask`. When the turn ends (a final answer, `stop`), the
  session's own mode is asked for again unless the next queued input is capped too; a turn
  lost with its process leaves nothing to undo, and the next process starts in the mode of the
  input it runs. A switch of the session's mode (`session.permission_mode`, or a
  `session.start` for a running agent) during a capped turn changes the session's mode at once
  but reaches the runtime when the capped turn ends. Since 0.44 (T40) it is `ask` also when
  one of the buffered messages in `peer_prefix` has an `ask` cap, whatever the input (§7.1).
  Absent: no cap (a person's input that carries no such message, a run of a person's
  schedule, a message from before 0.43).
  `schedule_id?` (since 0.70) — the `schedule_id` of a `user.message` the hub cron sent (§4,
  §16): such a run waits for the end of a running turn instead of joining it (§17.2).
- `session.stop` {session_id} — interrupt the current turn.
- `session.steer` {session_id, text} — intervene in a turn without interrupting it, if the
  runtime supports it (Hermes: `/steer`, §8.2.1); otherwise `error`.
- `session.kill` {session_id} — terminate the agent process.
- `session.permission_mode` {session_id, permission_mode} → `ack` (since 0.42, T38) — the user
  switched the session's mode (§6). The worker decides as for `session.start` (`auto` only with
  `allow_auto`); a running agent switches at once, also mid-turn (Claude Code: §8.1), a stopped
  one starts in the mode; a session the worker has not started is only acked (the next
  `session.start` carries the mode). The hub sends it when the worker is connected and
  otherwise relies on `session.start`; a `session.start` for a running agent whose mode differs
  switches it too.
- `session.model` {session_id, model | null} → `ack` (since 0.74) — the user changed the
  session's model (§6); `null` — the runtime's default. A running Claude Code agent switches at
  once, also mid-turn (`set_model`, §8.1); `null` and a stopped agent take effect at the next
  start. A session the worker has not started is only acked; an invalid name is
  `error {invalid_frame}`. A `session.start` for a running agent with another model switches it
  too.
- `session.cleanup` {session_id, workspace_path | null, cwd, event_ids[]} → `ack` (since 0.23) —
  after `session.kill` when a session or its project is archived or deleted: remove the inbox
  folders of the inputs `event_ids` (for a session without a workspace — its scratch folder;
  since 0.46 also the folders of `peer.message`s with files and of `hqc_ask` replies with files,
  named by the ask's `request_id`);
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
- `fs.stat` {request_id, path, create?} → `result {request_id, exists, is_dir, canonical?,
  inside_allowed_roots, environment?}` — checking a folder before creating a workspace (§13.1).
  `canonical` — the path with symlinks resolved, present if the path exists;
  `inside_allowed_roots` is computed from it (for a non-existent path — from the nearest existing
  ancestor). Since 0.76 `environment` — inside `allowed_roots`, the name of the environment a
  session in the folder runs in (§14), by the worker's bindings as it resolves them at that
  moment, as on a `session.start` (for a non-existent path — of the path it would be made at);
  an older worker does not send it. Outside `allowed_roots`
  the worker reports nothing about the path: `{exists: false, is_dir: false, inside_allowed_roots:
  false}`. A relative path gets `error {invalid_frame}`. Since 0.64 `create: true` (only when
  the user asked for it) makes a missing folder with its missing parents first, at the path its
  existing ancestor resolves to and only when that is inside `allowed_roots` (the worker's own
  check, §2.1); the answer is then the made folder's, and a folder that cannot be made gets
  `error {invalid_frame}` with the reason. An older worker ignores the field and answers
  `exists: false`.
- `fs.list_dirs` {request_id, path | null} → `result {request_id, entries[{name, path}]}`
  — the folder tree for picking a workspace in the client: with `path: null` — `allowed_roots`,
  otherwise the subdirectories of `path` (directories only, no hidden ones, symlinks only to
  directories inside `allowed_roots`, sorted by name case-insensitively, at most 500). A path
  outside `allowed_roots` (after resolving symlinks) gets `error {invalid_frame}`.
  For both frames `request_id` equals the frame's `id`; the answer is `result` or `error` with
  the same `id`. The hub waits 10 s for the answer (`504 worker_timeout` in REST, §6).
- `workspace.create` {request_id, kind: clone, path, url, branch?} or {request_id, kind:
  worktree, path, origin, branch, new_branch?} → `ack`, then
  `workspace.created` (§5.4) (since 0.58, T45; `kind: worktree` since 0.60, T46) — a new folder for a
  workspace (§13.1). `request_id` is the workspace's id, not the frame's. The worker checks
  `path` itself (§2.1): absolute without `.`/`..`, its parent with symlinks resolved a folder
  inside `allowed_roots` (a parent outside and a missing one get the same answer), the path not
  existing and not overlapping a workspace it knows (from `session.start`, `workspace.rules`,
  `known_workspaces.json`, or a folder it created). `url` passes `hqc_proto::frame::check_clone_url`
  on both sides: `https://`, `ssh://`, scp-like `[user@]host:path`, or `file:///<path>` whose
  path resolves inside `allowed_roots`; refused are other schemes (`http`, `git`), `ext::` and
  every `<transport>::` remote helper, plain local paths, a leading `-`, a host starting with
  `-`, whitespace and control characters; `branch` letters, digits and `._/-` without a leading
  `-` (`check_branch`). The worker creates the folder (failing if it appeared meanwhile), records
  it as creating in `$HQC_HOME/created_workspaces.json` (§14) and runs `git clone --quiet
  [--branch <branch>] -- <url> <path>` without a shell, in the environment of `path` (§14: the
  child allowlist and the environment's `env`, the same as an agent's), with
  `GIT_ALLOW_PROTOCOL` (`https:ssh`, or `file`), `GIT_TERMINAL_PROMPT=0`, `GIT_ASKPASS=true`,
  `GCM_INTERACTIVE=never` and `ssh -o BatchMode=yes` unless the environment sets its own ssh
  command, stdin closed, its own process group, killed after `clone_timeout` (§14). A failed or
  timed-out clone removes the folder and its entry. A request the worker knows is not started
  again: it is acked, and a finished one is answered with its `workspace.created` again; the
  hub repeats the request after every `hello` while the workspace is `creating`, so a clone
  survives a dropped connection. A worker that starts and finds a clone recorded as creating
  removes its folder; the hub fails such a clone on the new `boot_id` (§17.3). A worker before
  0.58 answers `error {invalid_frame}`, which fails the workspace.
  `kind: worktree` (since 0.60, T46): `origin` is the canonical path of the origin workspace; the
  worker takes it only if it is a workspace it knows (as above, so the hub sends the origin's
  `workspace.rules` right before the request), still resolves to itself inside `allowed_roots` and
  is the top folder of a git repository (`git rev-parse --show-toplevel`). `branch` passes
  `check_branch` and `git check-ref-format --branch`; with `new_branch` it must not exist and is
  made from the origin's `HEAD` (`-b <branch> ... HEAD`); without, it is a local branch that no
  worktree has checked out (the worker reads `git worktree list --porcelain` and answers "the
  branch … is already checked out at …"), or a branch of exactly one remote, which becomes a
  local branch tracking it (`--track -b <branch> ... refs/remotes/<remote>/<branch>`). `path` is
  checked and created as for a clone and recorded with its origin; then `git worktree add --quiet
  [<options>] -- <path> <commit>` runs in the origin, without a shell, in the environment of
  `path` (its `env`, and an ssh-agent of its own if the environment has `ssh_keys`, as for a
  clone, although the command needs no network) and with the same prompt rules and timeout. A
  failed one removes the folder and runs `git worktree prune` in the origin; so does a worker that
  starts and finds one recorded as creating. A worker before 0.60 refuses the frame (`error
  {invalid_frame}`), which fails the workspace.
- `workspace.inspect` {request_id, path} → `result {request_id, git, dirty[], dirty_count,
  unpushed[{branch, commits}]}` (since 0.58, T45) — what deleting a folder would lose, for the
  confirmation (§13.2). `path` a folder inside `allowed_roots`, otherwise `error {invalid_frame}`;
  without `.git` it is `git: false` with empty lists. `dirty` — the paths of `git status
  --porcelain` (untracked ones included; at most 200, all counted in `dirty_count`); `unpushed`
  — local branches with commits that are on no remote branch (`git rev-list --count <branch>
  --not --remotes`; at most 100); in a linked worktree (`.git` is a file; since 0.60) only the
  branch it has checked out, since the others belong to the repository (none when `HEAD` is
  detached). Run read-only (`GIT_OPTIONAL_LOCKS=0`) in the folder's
  environment. The hub waits 10 s, as for `fs.*`, and counts it among them (§20).
- `workspace.delete` {request_id, path, force?} → `result {request_id, removed}` (since 0.58, T45;
  §13.2) — the worker deletes `path` only if it is in its `created_workspaces.json` as written
  there (canonical, the clone finished) and still resolves to itself inside `allowed_roots`: a
  path it did not create, one swapped for a symlink since, or a clone still running gets `error
  {invalid_frame}`. A listed path that is gone already is `removed: false` and leaves the list.
  The hub waits 60 s. A worktree (since 0.60, T46) is removed by `git worktree remove [--force]
  -- <path>` in the origin recorded with it (which must still resolve to itself inside
  `allowed_roots`), then `git worktree prune`; its branch stays. Without `force` git refuses a
  worktree with uncommitted or untracked files and the worker answers `error {invalid_frame}`
  with git's message; `force` (only when the user confirmed the files the inspection listed, §6)
  removes them. A worktree folder that is gone already is pruned and `removed: false`. `force`
  changes nothing for a clone.
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
- `workspace.rules` {workspace_path, allow_rules[]} → `ack` (since 0.52, T48) — the whole current
  list of hqc's allow rules of a workspace (§3 Workspace). The hub sends it when the list
  changes (at once if the worker is connected) and after every `hello` for every workspace of the
  worker, an empty list included, so a list emptied while the worker was away is emptied there.
  The worker accepts only a folder inside `allowed_roots` (after resolving symlinks), otherwise
  `error {invalid_frame}`; it replaces the list it keeps for the folder in memory, which running
  sessions use at their next permission request.
- `permissions.inspect` {request_id, path, command?, tool?} → `result {request_id, mode,
  allow_auto, sources[{source, allow[], deny[], ask[], error?}], hqc[], check?{verdict, rule?,
  source?, parts, uncovered, suggestions[]}}` (since 0.52, T48) — the rules screen (§6). `path` must
  be a workspace the worker knows (from `session.start` or `workspace.rules`) inside
  `allowed_roots`, otherwise `error {invalid_frame}` (the worker does not trust the hub's path,
  §2.1). `sources` — `worker.toml` (`[permissions]` allow/deny, the default deny included;
  since 0.56 without the rules the folder's environment `unblock`s), then for a folder bound to
  an environment other than `default` its own `worker.toml [environments.<name>]` (its `allow`
  and `deny`, §14), `~/.claude/settings.json` (the user's Claude Code settings in its config
  folder), in an environment with `project_settings` (since 0.83, §14)
  `<workspace>/.claude/settings.json` (source `.claude/settings.json`, the file the CLI then
  reads too) and
  `<workspace>/.claude/settings.local.json`, in this order: the `permissions.allow`, `deny`,
  `ask` arrays of each file (non-strings skipped; a missing file has empty lists; a file over
  1 MB or that is not JSON has `error`); nothing else of the files leaves the worker. `hqc` — the
  hqc rules the worker holds for the folder; `mode`/`allow_auto` — `[permissions]`. With
  `command` or `tool` the worker's matcher (§8.1) checks the call: `deny` with the first deny rule
  of any source that matches the command or any part of it; else `allow` when every simple part
  is covered by an allow rule of any source or hqc (`rule`/`source` for a single part); else
  `ask`, with `uncovered` parts and the hqc rules suggested for them (`parts: 0`: the matcher could
  not split the command). `verdict` ∈ `deny` | `allow` | `ask`. The hub waits 10 s, as for `fs.*`,
  and counts it among the `fs.*` requests in flight (§20).
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
  sessions_registry?, moves?}, acp_agents[]}`; `sessions_registry: true` — the worker reads the Claude
  Code live sessions registry and reports holders (§9.3); `moves: true` (since 0.75) — the worker
  moves a moved session's transcript (§8.1), reported only when Claude Code's config folder
  (`$CLAUDE_CONFIG_DIR`, else `~/.claude`) exists at the worker's start, where the transcripts
  are; the hub moves only sessions of such a worker (§6).
  `environments` (since 0.56, T43) — the environments of the local config (§14) as `{name,
  allow_local_mcp?, permissions {unblock?, allow?, deny?}}`, without `default` and never with the
  values (or the names) of `env`; a plain string, as workers before 0.56 send, reads as an
  environment without rules. `env_bindings` — `{path, environment}`, the path canonical when it
  existed at the worker's start (`worker.hello.json`). The hub keeps both on the worker and sets
  each workspace's `environment` from the bindings (§3 Workspace).
  The worker does not report workspaces (since 0.16; the hub no longer reads the iteration 1
  field `capabilities.workspace_path`): workspaces are created in the hub (§3, §6) and checked by
  the worker against `allowed_roots`.
- `event` {event} → `ack` (`session_id` is inside the event)
- `session.state` {session_id, process_state, runtime_session_id?, locked_by, title?, held_by?, commands?,
  permission_mode?}
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
  `permission_mode` (since 0.42, T38) — `ask` | `auto`, the mode the agent runs in: the one the
  worker started or switched it with, then what the runtime reports (Claude Code:
  `permissionMode` of `system/init` and `system/status`, §8.1); absent — not known yet;
  repeated in every following `session.state` like `title`. The hub stores it as the session's
  `effective_permission_mode` (§3).
  `capped_input` (since 0.43, T39) — the `event_id` of the input whose `ask` cap the current turn
  runs under while the session's own mode is `auto` (`permission_mode` is `ask` meanwhile);
  absent — no cap holds the session down now. Unlike `title`, absence means none: every report
  says it. The hub stores the `capped_by` of that input's chain as the session's
  `permission_capped_by` (§3) and clears it on a report without the field and on a worker
  restart.
  `subagents` (since 0.70) — how many subagents the agent runs now (Claude Code: §8.1), 0 when
  it is stopped; absent — 0, and like `capped_input` every report says it. The hub stores it as
  the session's `subagents` and sets 0 on a worker restart.
- `mirror.event` {workspace_path, runtime_session_id, cwd, event, title?, recent, held_by?} — events
  from the mirror (§9.1, §9.2), through the outbox; the answer is `ack {event_id, seq}` as for
  `event` (a duplicate — the `seq` of the stored one; a folder not mirrored by this worker, a `cwd`
  outside it and the other refusals of §20 — without `seq`); a record of a new transcript over the
  daily limit gets `error mirror_quota`, `retryable: true` (since 0.24, §9.1).
  `workspace_path` — the path from `mirror.set`, `cwd` — the transcript's folder by the rule of
  §9 (an absolute path),
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
- `worker.limits` {five_hour?, seven_day?} → `ack {}` (since 0.81) — the usage limits of the
  account the worker's agents run on, each window `{utilization (0..1), resets_at (Unix
  seconds)}`: the 5-hour window ("Current session" of Claude Code's `/usage`) and the 7-day
  window of all models. Sent whenever the runtime reports them (Claude Code: every
  `rate_limit_event`, §8.1). The hub keeps the latest per worker in memory, with when it came,
  and shows it in `GET /api/workers`; after a hub restart there is none until the next turn.
- `workspace.created` {request_id, canonical | error} → `ack {}` (since 0.58, T45) — the outcome
  of `workspace.create`: the folder is ready at `canonical`, or it was not made (`error`, a
  message for the user; nothing is left on disk). Sent directly when the clone (or since 0.60 the
  `git worktree add`) ends and again
  for every repeated `workspace.create`; the hub makes the workspace `ready` at `canonical` (and
  sends `mirror.set` when it is mirrored) or `failed` with the error (up to 2000 characters), and
  ignores an outcome for a workspace that is not `creating` any more.
- `result` {request_id, ...} / `error` to the hub's requests (`fs.stat`, `fs.list_dirs`,
  `workspace.list_candidates`, `title.request`, `permissions.inspect`, `workspace.inspect`,
  `workspace.delete`, §5.3), with the request's `id`.
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
and accepts only worker tokens; the human route `/api/*` accepts only Access JWTs (in OIDC mode,
below, the provider's ID tokens). The hub listens only on the tunnel interface (in OIDC mode,
only the reverse proxy reaches it).

OIDC mode (since 0.68), for a hub behind a reverse proxy that signs people in with an OIDC
provider instead of Cloudflare Access (Traefik forwardAuth with oauth2-proxy and Keycloak, for
example): `HQC_OIDC_ISSUER`, `HQC_OIDC_AUD`, `HQC_OIDC_JWKS_URL` and `HQC_OIDC_ALLOWED_EMAILS`,
all four, and `HQC_CLIENT_IP_HEADER` (§20). `/api/*`, the `/api/ws` WebSocket included, then
takes the provider's ID token from `Authorization: Bearer` and checks it the same way: an RS256
signature by a key of that JWKS, exactly that `iss`, the `aud`, `exp` and `nbf` (60 s of
leeway; `nbf` in Access mode too), and the email from the `email` claim;
`Cf-Access-Jwt-Assertion` is not read. On top of that the email counts only with
`email_verified: true` (Keycloak puts it in the ID token with the `email` scope), otherwise
`403`: the provider may let every account set its own email, and an account that set the
owner's would become the owner's hub user, with their workers. A token with a `typ` other than
`ID` gets `401` (Keycloak's access tokens are `Bearer`; other providers may leave `typ` out,
which passes), and so does one whose `aud` is an array without `azp` equal to `HQC_OIDC_AUD`
(OIDC Core 3.1.3.7). Only the emails of `HQC_OIDC_ALLOWED_EMAILS` (comma-separated, compared
case-insensitively) get in; another one gets the same `401` as an invalid token, before the hub
looks up or creates a user: any account of the realm with an ID token for the client, through
the proxy's sign-in or past the proxy, would otherwise become a hub user that enrolls workers of
its own. The hub user is keyed on the token's `sub` (§3 User `oidc_sub`); the issuer is fixed by
`HQC_OIDC_ISSUER`, so `sub` alone is the key. The user bound to that `sub` signs in; else the
user of the email is bound to it on this first OIDC sign-in (one from Access mode too), or is
created with it; if the user of the email is bound to another `sub`, the token gets the same
`401`. So an account whose email changes at the provider keeps its hub user (when the new email
is on the allow-list too), and another account that gets the same email verified does not take it
over. The proxy puts the token there (oauth2-proxy
`--set-authorization-header`) and lets `/worker/*` through without its sign-in. It must also renew the token before it expires: the hub refuses an
expired one with `401`, and oauth2-proxy keeps handing over the token it got at sign-in unless
`--cookie-refresh` is shorter than the token's lifetime (in Keycloak the Access Token Lifespan,
5 minutes by default). Only some of the four variables, an empty allow-list, no
`HQC_CLIENT_IP_HEADER`, an issuer or JWKS URL that is not `https://` (`http://` only to a
loopback host, the rule of `HQC_PUBLIC_ORIGINS`), or the variables together with `HQC_ACCESS_*`
or `HQC_DEV=1`, and the hub does not start.

Dev mode (local development without Access): `HQC_DEV=1` together with `HQC_DEV_USER=<email>`;
all `/api/*` requests come from that user. The hub refuses to start if `HQC_ACCESS_*` or
`HQC_OIDC_*` is also set or `HQC_HUB_LISTEN` is not loopback (`127.0.0.0/8`, `[::1]`, `localhost`); on start it
logs a loud warning (§20).

REST bodies and `/api/ws` frames are typed in `hqc-proto` (`api`, `client_frame`); the
contract examples are `examples/wire/api.*.json` and `client.*.json`; the hub and client tests
read them. Unknown fields are ignored in both directions.

HTTP API (iteration 1 and projects with workspaces from iteration 2; the list is not complete,
the rest is marked "later"):

- `GET /api/me` → `{id, email, display_name, timezone, personal_project_id, theme}`
  (`api.me.json`); `PATCH /api/me` {timezone?, theme?} (since 0.38, `api.me.patch.request.json`;
  an unknown zone: `400`; `theme` since 0.51, §3 User, another value: `422`) → the same object;
  later keys; `PUT /api/me/project_order` {project_ids} (since 0.64,
  `api.project_order.request.json`) → `{projects[]}` in that order: the order of the project
  bar, kept per user (`users.project_order`); ids that are not the user's projects and repeats
  are dropped
- `GET /api/projects` → `{projects[]}` in the user's order, the projects missing from it last,
  oldest first (since 0.64; before it oldest first), `POST /api/projects` {name, description?} → `201`
  and the project, `GET/PATCH/DELETE /api/projects/{id}` (`PATCH` {name?, description?,
  default_workspace_id?, color?, icon?} — `default_workspace_id` a workspace of this project,
  otherwise `400`; `color` one of the 8 names and `icon` an emoji or letters within the limits
  of §3 Project, otherwise `422` (`{kind: image}` too: an image comes only by `PUT .../icon`);
  the personal project: `409 personal_project` on rename and delete, its `color` and `icon`
  change; `DELETE` → `204`, §3 Project).
  The project object is `{id, owner_id, name, slug, description, default_workspace_id?,
  is_personal, color, icon?, created_at, updated_at}` (`api.project.json`; `color` and `icon`
  since 0.51)
- `PUT /api/projects/{id}/icon` (since 0.51, T49; multipart: field `file`, like an attachment)
  → the project object with `icon: {kind: image, url}`: the image is checked as in §12
  ("Project icons"), anything else is `422 invalid_icon` with the reason in `message`. `GET
  /api/projects/{id}/icon` → the image (§12). Another user's or a nonexistent project, or no
  image: `404`
- `GET /api/workspaces?project_id=` → `{workspaces[]}` (without `project_id`: all of the
  user's workspaces); `POST /api/workspaces` {project_id, worker_id, path, label?,
  mirror_enabled?, create?} → `201` and the workspace (`kind: dir`, §13.1): the hub sends the worker
  `fs.stat` (with `create` since 0.64: the worker makes a missing folder; it is an ordinary
  `dir`, `created_by_hqc: false`) and stores `canonical`; `mirror_enabled` (since 0.23 defaults to `true`; `false`
  means no mirror) controls the mirror and the import from Claude Code
  (`CreateWorkspaceImport`, §9.1): the hub sends the worker `mirror.set`, and when such a
  workspace is deleted, `mirror.set` with `enabled: false`. Worker not connected: `409
  worker_offline`, the workspace is not created; no reply within 10 s: `504
  worker_timeout`; path outside `allowed_roots`: `400 outside_allowed_roots`; no such path:
  `400 path_not_found`; not a directory: `400 not_a_directory`; the worker rejected the frame:
  `400 invalid_path`; the path equals a workspace of the same worker, lies inside one or
  contains one: `409 workspace_conflict` with body `{error, message, existing_workspace_id,
  existing_project_id, existing_path}` (`api.workspace.conflict.json`; the client says "this
  folder is already in project <name>" with a link). Since 0.58 (T45) `{kind: clone,
  project_id, worker_id, path, url, branch?, label?, mirror_enabled?}`
  (`api.workspace.clone.request.json`): `path` is the new folder; the hub checks `url` and
  `branch` as the worker does (`400 invalid_url`, `400 invalid_branch`), sends `fs.stat` of the
  parent (the codes above) and of the new path (`400 path_exists`), stores the canonical parent
  plus the name as `creating` with `kind: clone`, `created_by_hqc: true` (the same
  `workspace_conflict` check) and sends `workspace.create` (§5.3); `201` with the workspace
  `creating`, which the client reads again until it is `ready` or `failed`. Since 0.60 (T46)
  `{kind: worktree, project_id, worker_id, origin_workspace_id, branch, new_branch?, path?,
  label?, mirror_enabled?}` (`api.workspace.worktree.request.json`): the origin must be the
  user's `ready` workspace of this project and worker and not a worktree (`400 invalid_origin`,
  also without `origin_workspace_id`), `branch` passes `check_branch` (`400 invalid_branch`);
  without `path` the new folder is `<origin-parent>/<origin-name>-<branch>` with the branch's
  `/` as `-`. The path is checked as a clone's, the workspace stored `creating` with `kind:
  worktree`, `created_by_hqc: true`, `origin_workspace_id`, `branch`, and the hub sends the
  origin's `workspace.rules` and then `workspace.create` (also when it repeats it after
  `hello`); the worker's refusals (not a git repository, a branch checked out elsewhere, one
  that exists or does not) come back as `failed` with its reason. Another `kind`:
  `400`. `PATCH /api/workspaces/{id}` {label?,
  mirror_enabled?} → workspace (`api.workspace.patch*.request.json`): changing `mirror_enabled`
  is the "VS Code sessions" switch (since 0.23, §9.2); the hub sends the worker `mirror.set`
  (at once if connected, otherwise after `hello`); the same value sends nothing. `DELETE
  /api/workspaces/{id}?remove_folder=&force=` → `204`: the record, the folder is left alone
  (§13.2); while it has non-archived sessions: `409 workspace_in_use`, unless
  `archive_sessions=true` (since 0.64: the user confirmed it), which archives them first as
  `POST /archive` does (§17.4), before the folder is removed; archived ones lose
  `workspace_id` and cannot be unarchived (`409 no_workspace`); while it is `creating`: `409
  workspace_creating`; while worktree records name it as their origin (since 0.60, any state):
  `409 workspace_in_use` ("N worktree(s) of this workspace exist; delete them first"), checked
  before the worker is asked. `remove_folder=true` (since 0.58, T45) only for a `ready` workspace with
  `created_by_hqc` (else `400`): after the session check the hub sends `workspace.delete` (§5.3)
  and deletes the record once the worker answered; worker offline `409 worker_offline`, no
  answer in 60 s `504 worker_timeout`, refused by the worker `400 invalid_path` (also a worktree
  with uncommitted files without `force`) — the record stays in all three. `force=true` (since
  0.60, T46) goes into `workspace.delete` as `force`: the client sets it only when the dialog
  showed uncommitted files. `GET /api/workspaces/{id}/inspect` (since 0.58, T45) → the worker's
  `workspace.inspect` without `request_id` (`api.workspace.inspect.json`), only for a `ready`
  workspace with `created_by_hqc` (else `400`); offline `409 worker_offline`, no answer `504
  worker_timeout`. The workspace object is `{id, project_id, worker_id, path, label,
  environment, kind, origin_workspace_id?, branch?, ephemeral, marked_for_deletion,
  created_by_hqc, mirror_enabled, hqc_rules, state, error?, created_at, updated_at}`
  (`api.workspace.json`, `api.workspace.worktree.json`, `api.workspace.ephemeral.json`;
  `hqc_rules` since 0.52 — how many hqc rules
  the folder has; `state`, `error` since 0.58, §3 Workspace; a client reads a missing `state`
  as `ready`; `origin_workspace_id`, `branch` since 0.60; `marked_for_deletion` since 0.61, §3
  Workspace, a client reads a missing one as `false`). The client (project settings, `ProjectSettings.dc.html`): "Clone a repository"
  next to "Add a workspace" — the worker, the URL, the parent folder from the same folder tree
  (or typed), the folder name (the repository's by default) and an optional branch; a clone's
  row shows "Cloning…" (the list is read again every second meanwhile), then the workspace, or
  "Clone failed" with the worker's error and "Remove" (the record only). "Remove" on a ready
  folder hqc created opens a dialog with the worker's inspection — the uncommitted files ("and N
  more"), the branches with their commits on no remote, "not a git repository", or that nothing
  would be lost — and "Delete folder", "Remove from project only", "Cancel"; with the worker
  offline the check fails and only the record can be removed. Since 0.64 "Remove" on a
  workspace with non-archived sessions or worktrees opens the dialog too (for a folder hqc did
  not create: "Remove from project", "Cancel"): it lists the sessions, says they are archived
  with it and marks one with a turn on "running, will be stopped", and the removal sends
  `archive_sessions=true`; worktrees of it are listed to delete first, with the buttons off. Since 0.60 (T46): "New worktree"
  on a ready workspace that is not a worktree (the client cannot tell a git folder; the worker
  can, and the worktree fails otherwise) opens a form under its row — the branch, "New, from the
  current commit" or "Existing branch", and the folder, showing the hub's default until edited
  (sent only when edited). Worktrees are listed right under their origin, indented, with their
  branch; while the worker adds one the row says "Adding worktree…", then the workspace or
  "Worktree failed" with the reason. The deletion dialog of a worktree lists the worktree's own
  branch with its commits on no remote and says they stay (the branch stays in the origin, only
  the folder goes with `git worktree remove`; a clone's dialog says its unpushed commits go with
  the folder; each warning says what "Delete folder" does with what it lists, since 0.64), and
  deletes with `force=true` only when it listed uncommitted files.
  Since 0.61 (T47): an `ephemeral` worktree has the tag "spawned" on its row; a marked one also
  "for deletion", the line "Its sessions are archived. hqc keeps the worktree and its branch
  until you delete it." and "Delete" instead of "Remove", which opens the same dialog (it adds
  that hqc made the worktree for a session an agent started)
- hqc's allow rules of a workspace (since 0.52, T48; §3 Workspace), the workspace owner only
  (another user's: `404`): `GET /api/workspaces/{id}/rules` → `{rules[{id, rule, created_at}]}`,
  newest first (`api.workspace.rules.json`); `POST /api/workspaces/{id}/rules` {rules[]}
  (`api.workspace.rules.request.json`) → the same list: rules the folder has are kept as they are,
  an empty list is `400`, a rule that is not valid hqc syntax `422 invalid_rule` (nothing saved),
  more than 200 per folder `422 too_many_rules`; `DELETE /api/workspaces/{id}/rules/{rule_id}` →
  `204` (unknown: `404`). After a change the hub sends the worker `workspace.rules` (§5.3).
  `GET /api/workspaces/{id}/permissions?command=|tool=` → the worker's `permissions.inspect`
  result without `request_id` (`api.workspace.permissions.json`); worker offline `409
  worker_offline`, no answer `504 worker_timeout`, refused by the worker `400 invalid_path`, the
  check over 16 KiB `400`.
  The client: the rules screen `#/p/<project>/rules/<workspace>` (`?session=<id>` when opened
  from a session), from "Rules · N hqc" on the workspace's row in the project settings and
  "Folder rules" in the session header; it shows the worker, the path, the workspace's
  environment (since 0.56), the worker's default mode and the session's; "Check a command" (a
  command, or a tool name such as `Edit`; a multi-line field since 0.62, so a pasted script keeps
  the line breaks that separate its commands; the worker's verdict with the rule and its source, "Will
  ask" with "Allow…" and the suggested rule); then denied, allowed (hqc's rules first, with the
  date and a remove cross confirmed in the row; the files' rules read-only) and everything else
  (Ask or Auto, the files' `ask` rules), each rule with its source. "Allow in this folder" adds a
  rule with a live format check (an error blocks saving), a duplicate is an error, and a deny that
  would win (checked on the worker) is a warning. Worker offline: hqc's rules only, editable, the
  check disabled. The permission card's "Allow in this folder": §10.1
- `GET /api/workers/{id}/dirs?path=` → `{entries[{name, path}]}`: an `fs.list_dirs` frame to
  the worker (§5.3); without `path`: `allowed_roots`; outside them: `400 invalid_path`;
  offline: `409 worker_offline`
- `GET /api/sessions` (since 0.89) → `{sessions: [<session>]}`: every session of every project
  of the user in one list, the objects of `GET /api/projects/{id}/sessions` (archived and
  service sessions included), the newest `updated_at` first. The client fetches its list with
  it and `GET /api/projects`, not one request per project
- `GET/POST /api/projects/{id}/sessions` (list: `{sessions: [<session>]}`; create:
  {title?, workspace_id?, worker_id?, cwd?, model?, auto_approve?, permission_mode?} → `201` and
  the session object, `api.session.request.json`), `GET/PATCH/DELETE /api/sessions/{id}` (§17.4).
  `permission_mode` (since 0.42, T38): `ask` | `auto` (anything else: `422`, the body cannot be read); absent — the
  worker's default. `PATCH /api/sessions/{id}` {title?, auto_approve?, permission_mode?, model?}:
  `model` (since 0.74) — absent: unchanged, `null`: the runtime's default; it is checked as on
  creation before anything is written, the service session answers `409 system_session`, and
  the hub sends the worker `session.model` (§5.3). It is not in `session.summary`: another
  device sees the change when it fetches the session object. A message typed as
  `/model <name>` (since 0.77; `POST /input`, the text being that command alone) does the same
  once stored: `default` stores `null`, a name outside the field limits (§20) is not stored and
  goes to the runtime only; the client fetches the session object after sending one. The chat
  header names the model the last turn used, or the stored one when the session was switched to
  another model since (an alias `opus` matches `claude-opus-…`). `permission_mode` in `PATCH`:
  `permission_mode` switches the session's mode and sends the worker `session.permission_mode`
  (§5.3); `auto` only by the worker owner (`403`, as for `auto_approve`), `ask` by whoever may
  manage the session; the service session: `409 system_session`. The reply carries the stored
  `permission_mode` and the `effective_permission_mode` the worker reported so far; its
  confirmation comes as `session.summary`.
- `GET/PUT /api/projects/{id}/session_order` (since 0.53, T50): the order of the project's
  session list for the requesting user, one record per user and project → `{mode, session_ids}`
  (`api.session_order.json`). `mode` is `state` | `recent` | `custom` (the list's switch; it
  lived in the browser's `localStorage` before); `session_ids` are every unarchived session of
  the project except the service sessions, in the user's order: those not placed yet (a new
  session, a restored one) first, newest created first, then the stored order; archived and
  deleted ones are left out. No record: `state` and the sessions newest first. `PUT` {mode?,
  session_ids?} (`api.session_order.request.json`; either or both, a missing one keeps its
  stored value) stores the order as the reply gives it: ids that are not unarchived sessions of
  this project (another project's, another user's, archived, unknown, repeated) are ignored, the
  project's sessions missing from the list go first; an unknown `mode` is `422`. Another user's
  or a nonexistent project: `404`
- `GET /api/sessions/{id}/events?after_seq=&limit=` — pagination by `seq`, response
  `{events[], last_seq}`. Since 0.54 (T51) a final `agent.message` that answered an
  `hqc_ask` has `reply_delivered_at` when the asking worker acknowledged the reply (§7.1);
  the field is not stored, the hub adds it here and on `/api/ws`
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
- `POST /api/sessions/{id}/fork` {event_id} → `201` and the new session object (since 0.80;
  `manage` level): a new session in the same project, workspace, worker and `cwd`, with the
  source's model, `permission_mode` and `auto_approve`, titled "<title> (fork)", whose
  conversation is the source's before the user's message `event_id` (a `user.message` of a
  person in this session, else `404`); a session that is not Claude Code or has no workspace on
  a worker of its owner: `409 not_forkable`. The cut is the source's last `agent.message`
  events before the message (up to 20, newest first): they go as `session.start.fork` (§5.3)
  with the source's `runtime_session_id`; with none (the first message) the session starts
  with no conversation and no fork. Its feed gets the source's `user.message`,
  `agent.message`, `agent.thinking`, `tool.call`, `tool.result`, `attachment`, `peer.message`
  and `system` events before the message under new ids, at `seq` 1.., none of them an input to
  deliver, then `system forked_from`, all read by the owner; requests, statuses and usage stay
  with the source. Since 0.82 a copy keeps its origin (`events.origin_id`, `origin_runtime`,
  not on the wire): the id the runtime's record gave the event and the transcript that record
  was first written in. The cut's events are each named by their origin, and the fork goes to
  the transcript of the newest one with only the events in it: a fork of a fork before a
  message of its copied history cuts in the transcript that history came from, which stays on
  the worker when the sessions between never ran (the CLI writes a fork's transcript at its
  first message) or were deleted (§17.4). The agent starts at once. The client puts the message's text into the new
  session's input as its draft and opens it.
- `POST /api/sessions/{id}/move` {path, project?, create?} → the session object (since 0.75;
  `manage` level, §3 Grant): the session continues in folder `path` of its worker, in that
  folder's project (§3 Session). The hub checks the folder with `fs.stat` as `POST
  /api/workspaces` does, with the same codes (`create`: the worker makes a missing folder; the
  overlap and `ready` checks below run on the requested path before that, so a `409` leaves no folder,
  unless the path reaches a workspace through a symlink, which only the canonical path shows; `400
  path_not_found`, `outside_allowed_roots`, `not_a_directory`, `invalid_path`, `409
  worker_offline`, `504 worker_timeout`), and takes the canonical path. A workspace of the
  session's owner on that worker at exactly that path is taken with its project (`project`, when
  given, must be that one: `400` otherwise), unless it is not `ready` (§3 Workspace): `409
  workspace_not_ready`, as for a new session; one that lies inside the path or contains it, or
  another owner's, is `409 workspace_conflict` with the body of `POST /api/workspaces`.
  Otherwise the folder becomes a new `dir` workspace, mirrored (§9.2), of `project` (an id or
  slug of the owner's projects, `404` otherwise) or of a new project named after the folder's
  last component; a project's first workspace becomes its default. Then the session gets that
  project and workspace, `cwd: .` and its slug (§3 Session), the old project's saved order drops
  it (the same project's keeps it), the feed gets `system session_moved` (§4), the workspace's
  `mirror.set` goes out, and a running agent gets `session.start` with the new folder, which the
  worker applies between turns (§5.3). No input is queued (`hqc_move` queues one, §7): a stopped agent starts there with its
  next input. A session already in that workspace is answered as it is, nothing written and no
  second `session_moved`; the workspace's `mirror.set` and the `session.start` of a running agent
  go out again, as the first call may have ended between the move and them (the client went
  away, a database error). `409 archived`: an
  archived session, as for `/takeover`. `409 unavailable`: not a Claude Code session, the
  service session, no workspace, driven from VS Code (`locked_by: vscode`), held open by a live
  process outside hqc (`held_by`, §9.3), a session whose subagents still run (`subagents`, §5.4;
  a foreground one too, as the hub sees only the count: a background one would keep the agent in
  its old folder), a worker that is not the session owner's (as for `POST /api/workspaces`), or
  a worker that does not report
  `capabilities.claude_code.moves` (§5.4). These refusals are checked before `fs.stat` and again
  after it, on the session read anew: it may change meanwhile. The client offers it as "Move to
  project…" in the chat's "More" menu: the project (one of the user's, or a new one named after the folder), one
  of that project's folders on the session's worker, the folder tree or a typed path, and
  "Create the folder and move" after `path_not_found`, as when a workspace is added; after the
  move the session leaves the old project's list and opens in the new one, which joins the rail.
  A `session.summary` with another `project_id` (a move made elsewhere) does the same.
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
  request (`approval`, `clarify`, `worker_approval`, since 0.76 `move`) in the user's sessions that are not
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
- `PUT/DELETE /api/sessions/{id}/favorite`, `PUT/DELETE /api/projects/{id}/favorite` (since
  0.74, §3 Favorite): for whoever may read the session or the project, as `read_cursor`;
  `PUT` → `200 {favorite_at}` (again: the same time), `DELETE` → `204` (also when there was
  none). A session's change reaches the user's clients as `session.summary`; a project's has no
  frame, another device sees it at the next `GET /api/projects`. The client shows a Favorites
  block of the starred sessions of every project above the session groups, newest first, and the
  starred projects first on the rail, each part in the hub's project order.
- `GET /api/workers` → `{workers[]}`: workers that are not revoked; `?include_revoked=1` adds
  the revoked ones (the "Settings" screen). The worker object is `{id, name, os, status: online
  | offline | revoked, revoked_at?, last_seen?, system_session_id, limits, ...}` (§3 Worker);
  `limits` (since 0.81) — `{five_hour?, seven_day?, reported_at}` of the latest
  `worker.limits` (§5.4), `null` while there is none. The chat's "More" menu shows them for
  the session's worker under the model field: a bar, the share used and when it resets.
  `GET /api/workers/enroll/{code}` → `{name, os, created_at, country?, replaceable?}`
  (`EnrollInfo`; `country` is `CF-IPCountry` of the `enroll` request, absent with
  `HQC_CLIENT_IP_HEADER`, §5.1; `replaceable`: §5.1, workers
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

Client layout (since 0.51, T49, `docs/design/redesign-2026-11-01`): on a desktop the project
bar (72 px: a tile per project with its icon on its color at ~24 %, the current one more
saturated with a pill of its color at the left edge, an amber "needs you" badge, else, since
0.67, a dot when a session of the project has unread events), the sidebar
(280–310 px: the project's tile, name and counters, a gear to the project settings, "New";
rows with a status dot — amber waiting, the project color pulsing while running, an empty ring
idle — title, time, status and `worker · path` without `~/work/`) and the main column, whose
feed and input are at most 780 px wide; on a phone one screen at a time, the projects as chips
with their icons, the same badge or dot. Since 0.73 the tab's icon marks the same across all
projects: an amber dot while a session waits, else a light one while one has unread events. Since 0.64 the project bar has "Reorder projects" (⇅ under "+"): the tiles
are dragged (or moved with ↑/↓ when focused) instead of opened, ✓ stores the order with `PUT
/api/me/project_order`, ✕ drops it; a phone's chips follow the same order. Since 0.64 a project opened on a desktop opens the session last open in it
on this device (kept in the browser's storage; none, archived or deleted: the empty main
column); a phone's project screen stays its session list. Since 0.78 a message typed into a
session and not sent stays its draft on this device (the browser's storage, `hqc.drafts`): the
input shows it again when the session is opened, and the session's row, unless it is the one
open, says "Draft"; sending the message or clearing the input drops it. Since 0.89 the
projects, the sessions and the list orders the hub gave on this page load are kept on this
device as well (the
browser's storage, `hqc.list`, with the id of the personal project), written after each fetch
of the list and when the page is hidden or left, dropped when a write fails: on a reload the
project bar and the list (of the project of the open session or project, else the personal
one) show them at once, and the hub's answer replaces them; a kept list order is asked from
the hub again once per page load. Before any list (nothing kept, no answer yet) the list's head has no counts, and
"offline" (the list's head, the chat's header) appears only once the socket's first connect has
ended. The theme is the user's `theme` plus the current project's `color`, as CSS
variables on the document root recomputed when either changes; status colors do not depend on
the project (amber waiting, green online, red danger, violet between sessions); since 0.88 the
light themes have darker variants of them, and there the project's color is darkened where it
is text. Since 0.88 the app theme is picked on the Settings page (`#/settings`, the first
section "Theme": a row of dark and a row of light tiles, a click applies it at once). On a
light theme the client switches `apple-mobile-web-app-status-bar-style` from
`black-translucent` (white status text) to `default` (dark text) **[verify]**: whether an
iPhone's Home Screen app picks the change up at once, at its next start or only when added
again. The project settings (`#/p/<id>/settings`, the gear) have Appearance (the "Project appearance"
dialog: the name, icon as an emoji, letters or an image, the color, a live preview in the app
theme; before 0.88 it also had the app theme; on a phone a
full-screen sheet; "Delete project" next to the button that opens it; since 0.64, before it a
section "Project" had "Rename" and "Delete project"; the personal project has neither the name
field nor the delete) and Workspaces (the workspace records with "VS Code
sessions", "Make default", "Remove"; "Add a workspace", "Import from Claude Code"; they are no
longer in the sidebar).

The session list (since 0.53, T50): a State / Recent / Custom switch (State groups Needs you /
Running / Idle, Recent Today / Yesterday / Earlier, Custom is one list in the saved order;
since 0.102 an `idle` session whose subagents still work after its turn ended, `subagents` > 0,
is under Running, and its row and the chat header say "Running") and a Reorder button; Reorder
replaces the switch with "Reorder · Cancel · Done" and gives each row a handle (on the left
on a desktop, on the right on a phone), dragged with pointer events (mouse and finger) or moved
with ↑/↓ while focused; Done stores the order with `mode: custom`, Cancel
restores the previous one. Both are `session_order` above. A row shows, on hover or focus, an
archive button and "⋯" (Rename, Delete with a confirmation in the row); on a phone a long press
(or a right click) opens a bottom sheet with Archive, Rename and Delete (confirmed in a second
step). Archiving from a row hides it at once and shows "Archived «title» · Undo" for 5 s; the
`/archive` call goes only when the toast ends, when another row is archived (the previous one is
sent first), before Reorder or archive mode opens, or when the page is hidden or left (`fetch` with
`keepalive`); Undo cancels it, so nothing reaches the hub and the row is back in its place. The
session header has no archive, restore or delete. "Archive N" under the list opens archive mode:
the sidebar without the project tint, a hatched header with a back arrow, "Archive", "<project>
· N sessions" and "Read-only…", a search, and rows with "Archived <when>" and "Restore" and
"Delete" always shown (delete confirmed in the row). An open archived session has an "Archived"
chip in the header, a hatched band "Archived <when> · read-only. Restore or delete it from the
list on the left." (the time from `archived_at`, none without it), the feed dimmed and no input.

Messages between sessions in the feed (since 0.54, T51; mockups `Session`, `AskingSession`,
`MSession2`): violet blocks (`--relay`) with an arrow, ↗ for what left this session and ↙
for what came into it, and the other session's title as a link to it, its address in the
tooltip (since 0.64; plain address text when the user has no such session: deleted or unknown). In the answering session a `peer.message` is
"↙ Question from X" (`mode: ask`) or "↙ Message from X" with its text and files, and the final
`agent.message` whose `in_reply_to` is a question is "↗ Reply to X · delivered <time>"
(`reply_delivered_at`) with the turn's `attachment`s and token line inside the block; a send
of that turn that went `with_reply` (§7.1) follows the text after the separator line "sent
with hqc_send while answering". In the asking session the agent's own `hqc_send` / `hqc_ask`
call is "↗ Sent to X · delivered <time>" with the call's `text` and files: the `delivered_at`
and `artifact_ids` of the `peer.message` in X, which the client reads at `message_seq` of the
result (`GET .../events?after_seq=<message_seq - 1>&limit=1`), else the names in `files`, and
until then "waiting for a reply" (an ask), "queued", "waits for its next turn" (buffered) or
"goes with the reply"; the ask's result is "↙ Reply from X" with `reply` (the parts after the
separator under the same separator line) and `files` (Open / Download by `artifact_id`, the
local `path` under each). Such a send is also a `peer.message` in the asker; the client leaves
it out there, as the reply shows it.

Session object (`GET /api/sessions/{id}`, list, create, `PATCH`, `archive`):
`{id, project_id, owner_id, workspace_id?, worker_id?, cwd, title, title_source, slug, agent: {runtime,
acp_agent?, model?, runtime_session_id?, process_state, system_prompt?,
disallowed_tools[], restricted, start_failures}, auto_approve, wake_on_send, locked_by,
last_seq, archived, archived_at?, system?, held_by?, state, queue_len, last_read_seq, unread, created_at,
updated_at, commands?, spawned_by?, permission_mode?, effective_permission_mode?,
permission_capped_by?, subagents?}`.
`subagents` (since 0.70): the subagents the agent runs now, from `session.state` (§5.4);
absent — 0. The client shows "N agents" ("1 agent") next to the state in the chat header and in
the session's row of the list while it is above 0; a background subagent keeps it up after the
turn that started it has ended.
`permission_mode` and `effective_permission_mode` (since 0.42, T38): §3 Session; absent —
null. The client shows the mode in the chat header: `effective_permission_mode`, else
`permission_mode`; "Auto" as a marked badge, "Ask" as plain text, "Ask (Auto asked)" while
the session asked for `auto` but its worker runs `ask` (not restarted yet, or `allow_auto =
false`), nothing while both are absent; since 0.43 (T39), while `permission_capped_by` is set,
"Ask (asked by <address>) · Auto after this turn": the turn runs in Ask under the cap, the
session's own Auto comes back when it ends. The session's "More" menu has "Switch to Auto" /
"Switch to Ask"; the new-session form has "Permissions": the worker's default, Ask or Auto.
Going to Ask asks nothing; going to Auto asks one line: "Switch to Auto? Claude Code then runs
tool calls without asking; the worker's deny list is the only limit." `spawned_by` (since 0.37, T34) — `{session_id, address}`
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
events with `seq > after_seq`, then the live stream; `agent.delta` has no `seq`. Since 0.72
the answer being written comes right after those events, before `subscribed`, as one
`agent.delta` with its text so far, and the stream carries only the deltas after it. On open
the chat loads the newest 100 events (`before_seq`), shows them and subscribes, then loads the
older history behind them, newest first; it renders the last 60 blocks of the feed and 60 more
each time the user scrolls to the top, and while the feed is shorter than its window (which
cannot be scrolled); "Requests only" of a worker's service session, on or off, starts it again at
the last 60 blocks of the new view. An event
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
  `seq`; since 0.54 (T51) the hub sends the final `agent.message` that answered an `hqc_ask`
  again, the same `seq`, with `reply_delivered_at` once the asker's worker acknowledged the
  reply, to the subscriptions that already had it (the client takes the field over its copy);
  `{type: subscribed, session_id, last_seq}`: the history after `after_seq` has been
  sent, the live stream follows; `{type: delivery, session_id, event_id, seq, delivery,
  delivered_at?}`: the delivery of an input changed; the event itself is not resent, the
  client updates `delivery`/`delivered_at` of its event with this `seq`;
  `{type: summary, sessions[]}`: the reply to `subscribe_summary`, all of the user's sessions;
  `{type: session.summary, session}`: one session changed; an element is `{session_id,
  project_id, title, state, queue_len, unread, last_seq, last_read_seq, held_by?,
  permission_mode?, effective_permission_mode?, permission_capped_by?, subagents?}` (the same as
  in the session object; the modes since 0.42, so the header follows a switch the worker
  confirms, the cap since 0.43, `subagents` since 0.70); `{type: requests, requests[]}` (since 0.41, T37,
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
`422`: a body that cannot be read as its type (an unknown `permission_mode`, `theme`, `color`),
and since 0.51 `invalid_icon` (a project icon refused, §3 Project, §12);
`413 too_large`: an artifact over the limit (§12); `409` with codes `archived`, `locked_vscode`
(input to a session VS Code is driving; body `{error, message, hint: "takeover", held_by?}`,
the client offers "Take over" or, with `held_by`, closing the session in VS Code, §9.3),
`vscode_open` (takeover of a session a live foreign process holds; `message` is "the session
is open in VS Code: close it there and take it over again", §9.3), `vscode_active` (takeover
before 30 s of silence without registry data, §9.3), `system_session` (the worker's service
session, §10.2), `request_closed`, `no_workspace`, `worker_offline`, `workspace_conflict`,
`workspace_in_use`, `workspace_creating` and `workspace_not_ready` (since 0.58, §3 Workspace),
`personal_project`, `push_limit`, `schedule_limit` (§20), `conflict`, `worker_online` (replacing a
connected worker, §5.1), `worker_not_revoked` and `worker_has_sessions` (deleting a worker; the
latter with `sessions_count`); `400`: `bad_request`,
`unknown_artifact` (§12) and the path check codes (`outside_allowed_roots`, `path_not_found`,
`not_a_directory`, `invalid_path`, and since 0.58 `path_exists`, `invalid_url`,
`invalid_branch`, since 0.60 `invalid_origin`); `504
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
  `content` comes with the bare model (iteration 5) and is refused until then
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
  is required until the personal session exists (iteration 5); the result is `{to,
  delivery}`, `to` — the target's address, `delivery` — `queued` (the input queue, the
  target wakes on sends; by default since 0.55) or `buffered` (the send buffer, §7.1); since 0.47 (T42) also
  `with_reply`: the send goes back with the reply to the target's open ask (§7.1). Since 0.54
  (T51) also `message_seq`: the `seq` of the `peer.message` in the session it went to (the
  asker's for `with_reply`), from which the sender's client shows its delivery (§6).
- `hqc_ask(text, session, timeout_s?)` — a message that waits for a reply. To the agent's own
  session: an immediate error. Since 0.35 (T32): the result is `{from, reply}` — the
  target's address and the text of its final answer; `timeout_s` is at least 1 and at most
  `ask_timeout_s` (a larger value is cut to it), by default `ask_timeout_s`. Since 0.54 (T51)
  the result also has `message_seq`, the `seq` of the question in the target (§6).
- Files between sessions (since 0.46, T41): `hqc_send(text, session, files?)` and
  `hqc_ask(text, session, timeout_s?, files?)`. `files` is a list of at most 32 strings, each
  a path or an artifact id (a uuid, e.g. from an `hqc_attach` result). The sender's worker
  daemon resolves, reads and uploads every path by the rules of `hqc_attach` (inside the
  session's workspace and `allowed_roots`, symlinks resolved, a regular file of at most 100
  MB each, `PUT /worker/artifacts`); a uuid passes as it is. Then it sends the `mcp.call` with
  `artifact_ids` instead of `files` (the hub never sees a path; a `files` that reaches the hub
  is an unknown argument, `invalid_args`). The first refusal fails the call before anything
  goes to the hub (a path uploaded before it stays an unreferenced artifact, removed by the
  cleanup of §12). The hub accepts only artifacts of the owner linked to the calling session
  in `artifact_sessions` (uploaded for it, or given to it in an input or a reply), at most 32,
  each once; anything else is `not_found` and nothing is stored. The `peer.message` keeps them
  in `artifact_ids` and they are linked to the target, whose worker downloads them into its
  own `.hqc/inbox` and lists the paths after the message's text, under the peer header
  (§5.3, §12). A repeated call acts once (§5.2). The reply to an `hqc_ask` carries the files
  the target attached during its answering turn (§7.1): the result is `{from, reply,
  files?}`, `files` absent when there are none; the asking worker gives the agent `files:
  [{artifact_id, name, kind, path, size}]` (`artifact_id` since 0.54, T51: the asker's client
  opens the file by it, and the agent may pass it on in `files`) with `path` absolute, in
  `<workdir>/.hqc/inbox/<request_id>/`, or `{artifact_id, name, kind, error}` for a file it could not download (the reply text comes back
  anyway). The tool descriptions tell the agent to hand files over with `files` and never
  paste them into `text`, and that they arrive as local paths. Since 0.47 (T42) they also say
  that while an agent answers an `hqc_ask`, an `hqc_send` to the asker goes back with the reply
  (its text after the final message, its files with the reply's files), and `hqc_attach` and
  the ask's header say so too (§7.1).
- `hqc_spawn(project, workspace, runtime?, model?, prompt)` — create a session and an agent,
  return the address. `workspace` is required if the project has no `default_workspace_id`.
  Since 0.37 (T34): `hqc_spawn(project, workspace?, runtime?, model?, prompt)`; the session is
  created by the same path as `POST /api/projects/{id}/sessions` (§6), owned by the calling
  session's owner, in the workspace's folder (`cwd` `.`) on the workspace's worker, titled
  "Session N" until a title comes (§3), without `auto_approve` and (since 0.42, T38) with
  `permission_mode: ask`, whatever mode the caller runs in or the worker's default is: an
  agent never starts a session in `auto`, and there is no argument for it (an unknown
  argument is `invalid_args`); the owner may switch it later. A peer message (§7.1) never
  changes the mode of the session it wakes; since 0.43 (T39) the turn it starts runs under the
  cap of its chain (§7.1), and so does the spawned session's first turn. `project` — a slug or an id
  of the owner's project; `workspace` — an id, a label or a path of a workspace of that
  project (else `not_found`), absent — the project's `default_workspace_id` (none:
  `invalid_args`, §13.4); `runtime` — `claude_code` only until iteration 5 (else
  `invalid_args`); `model` absent — the calling session's model (a cheap spawner does not
  start an expensive agent by default), present — checked as in §6. `prompt` (not blank)
  becomes a `peer.message` with `mode: send` from the calling session in the new session's
  input queue (`delivery: pending`, like a send to a session with `wake_on_send`), so it
  starts the first turn with the header `[hqc peer from <spawner> | mode=send | …]` (§7.1) and the new
  agent knows whom to answer with `hqc_send`; its chain is the caller's moved on (§7.1). The
  result is `{address}` at once; the call does not wait for the turn. The new session
  records the spawner (`spawned_by`, §3); spawns are limited per chain and per owner and
  hour (§20, `limit_exceeded`). A worker that is offline gets the session when it connects,
  as for the API.
  Since 0.61 (T47): `hqc_spawn(project, workspace?, runtime?, model?, prompt, isolation?,
  branch?)`. `isolation: "worktree"` (the only value; another is `invalid_args`, as is
  `branch` without isolation) puts the new session in an `ephemeral` worktree of its own (§3
  Workspace), so a helper does not edit the checkout its spawner works in. The origin is the
  workspace the call would use without isolation; T46's rules apply to it (a `ready` workspace
  that is not a worktree itself, else `invalid_args`; only the worker can tell it is a git
  repository), and its worker must be online (else `unavailable`). The branch is new, from the
  origin's `HEAD`: `branch` (checked like T46's, `invalid_args`), by default `hqc/<slug>` with the
  slug the new session gets, `session-N` (N past the slugs of the project and the branches of
  its workspaces, so parallel spawns get their own); the folder is T46's default
  `<origin-path>-<branch with / as ->`. The hub stores the worktree `creating` (`created_by_hqc`,
  mirror off) and sends `workspace.create` as for the API (§5.3); the call waits until the worker
  answers, then creates the session there (titled "Session N" with that N) and its first input,
  and returns `{address, worktree: {path, branch}}`. A failed worktree fails the call with the
  worker's reason (`unavailable`, "the worktree could not be made: …") and leaves neither a
  session nor the record; a worktree not made in 15 minutes fails the call too and stays (it is
  marked for deletion once ready, as it has no session). The description of the tool says that
  the helper's commits stay on its branch for the owner to merge and that the worktree is not
  deleted automatically. A branch outlives its worktree (§13.2), so a default name used again
  (a slug taken again after its session was deleted) is refused by the worker; the agent gives
  `branch` then.
- `hqc_list_sessions(project?)` — addresses, titles, statuses. Since 0.33 (T30): `project` is
  a slug or an id of the owner's project (another owner's or an unknown one: `not_found`); the
  result is `{sessions: [{address, title, status, subagents?, current?}], truncated}`: the owner's
  sessions, the most recently active first, at most 200 (`truncated: true` beyond), archived
  sessions and the workers' service sessions left out; `status` is the session's `state`
  (§17.1); `subagents` (since 0.79) the subagents running now, when any, also of an `idle`
  session whose background subagents outlive its turn; `current: true` marks the calling session.
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
- `hqc_move(path, project?)` (since 0.76) — the calling session moves to folder `path` of its
  worker, by the core of `POST /api/sessions/{id}/move` (§6). The path is the agent's text the
  owner may be shown: one with a control character or a bidirectional control (U+061C, U+200E,
  U+200F, U+202A–U+202E, U+2066–U+2069, which reorder the path shown) is `invalid_args` before
  anything else, and so is a missing folder's path with a `.` or `..` component. The hub first
  looks at the folder with the worker's `fs.stat` without `create`, on the connection the call
  came by, as a spawn with a worktree does; a folder the worker refuses is the call's error at
  once, and so is one whose canonical path has such a character (that path goes on the card).
  The session moves at once (`create: false`) only into a ready workspace (not `creating` or
  `failed`) of its owner on that worker at exactly that canonical path whose environment is
  that of the session's folder now, both as the worker names them in `fs.stat` (`environment`,
  §5.3: its bindings as it resolves them then, which the agent starts by; not the hub's copy of
  `env_bindings` from `hello`, which may differ, e.g. for a binding through a symlink made
  since): the hub sends a second `fs.stat`, of the session's folder, for it, and an environment
  the worker does not name (an older worker) counts as another one. Anything else — a missing
  folder, a folder that is no workspace of the owner, another environment — waits for the
  owner's answer, unless the move would be refused anyway: before
  it opens a request the hub runs the endpoint's refusals of the calling session (its `409`s,
  below), resolves `project` (unknown: `not_found`), checks the path for an overlap as the
  endpoint does before it makes a missing folder (`workspace_conflict`) and checks a workspace
  of the owner at exactly that path: not ready, it is `409 workspace_not_ready` as for a new
  session there (`unavailable`, with the reason); of another project than `project` names,
  `invalid_args`; any of them is the call's error at once, and no card is shown. A session has
  at most one such request open: while the request of another call waits, the call is
  `unavailable` ("a move already waits for the user's answer") and no card is shown; a repeat
  of the waiting call is not refused (below). The request is
  a PendingRequest `move` of the calling session (§3) with the hub's `approval.request` in its
  feed (§10.1), `author {kind: system}`, `tool: hqc_move`, a `description` saying why it asks
  (a new folder, made if allowed; a folder that is no workspace of the owner yet; the
  environment of the target and that of the folder now, or that the worker does not name one)
  and where the session lands ("in
  project <name>", of `project` or of the workspace at the path, or "in a new project <folder
  name>"), with the path on a line of its own — the canonical path of that `fs.stat`, or the
  path as given when the folder is missing — and, for a folder that is no workspace yet, the
  line "It becomes a mirrored workspace: Claude Code sessions in it are imported into hqc." (it
  becomes a `dir` workspace with the mirror on, §9.1); options `once` / `deny`. The call is
  deferred as `hqc_ask` is: `ack` now, the `mcp.result` a frame of its own later, kept in the
  request until the worker acknowledges it and sent again after `hello`. The call waits at most
  the runtime's tool timeout (`ask_timeout_s` plus a minute, 11 minutes by default, §7.2): then
  the runtime cancels it, its `mcp.cancel` closes the request (below), and an answer after that
  gets `409 request_closed`. `once`: the hub moves
  the session into the path on the card with `create: true` (a missing folder is made, inside
  `allowed_roots` only) and goes on as after a move at once; a move that fails then gives the
  call its error; a result the hub fails to store is stored again after a pause (1 s at first,
  doubled each time up to 60 s), and the move is not carried out again for it; a hub that stops
  before it has stored the call's result carries the move out
  again once the session's worker sends `hello` to the hub started anew (§3). `deny`:
  `unavailable` ("the user did not allow the move"), nothing is made. A cancellation of
  the request (§10.1) gives the call `unavailable`; after a worker restart nothing is sent, the
  call died with it. `mcp.cancel` of the call (§5.2) closes the request with `approval.cancel
  {stopped}` and sends nothing: nothing moves, and an answer after it gets `409
  request_closed`. The move lands only in the path it was decided on — the canonical path of a
  move at once, the path on the card after `once`: right after the move's own `fs.stat` (the
  one that makes a missing folder) the hub compares the canonical path the worker reports with
  that path and, when they differ (the folder may have been swapped for a symlink meanwhile),
  refuses with `invalid_args` telling the agent to call again with the canonical path; nothing
  is recorded (a folder that `fs.stat` made stays). A missing folder under a symlinked parent is
  refused so as well, after the owner allowed it; the call with the canonical path asks again.
  `project` and the folder rules are the
  endpoint's; the result is `{address, project, path}` — the session's new address, its
  project's slug and the canonical path. The caller must be run by hqc (`locked_by: hqc`), and
  the chain of the input its turn runs on (the one the call's chain is taken from, §7.1) must
  not be capped to `ask`, whoever capped it and whatever the session's own mode: such a turn
  moves nothing (`unavailable`, naming `capped_by`), before `fs.stat` — a turn another agent in
  `ask` started, and as well one the session capped itself: the input after its own move in
  `ask`, another agent's answer to a message the session sent in `ask`, a run of its own
  `hqc_schedule` made in `ask`. A person's input has no cap of its own (§7.1): a session in `ask`
  moves when its user asks in the chat, unless the input carries buffered messages of a session
  in `ask` (T40), which cap it. Errors: `invalid_args` with the worker's reason for a folder
  outside the roots or one it cannot make, for a path or a canonical path with a control
  character or a bidirectional control or a missing folder's path with `.` or `..`, and for a
  path that is another
  project's workspace while
  `project` names another; `invalid_args` with the text `workspace_conflict` for a folder
  inside or around another workspace; `not_found` for an unknown project; `unavailable` for a
  session that may not move (the endpoint's `409`s: not Claude Code, no workspace, VS Code or a
  live holder, a subagent still running, a worker without `claude_code.moves`, archived), for a
  workspace that is not ready, for a session not run by hqc or under an `ask` cap, and while
  another move of the session waits for the owner; `limit_exceeded` when the call's chain is at
  its limit, before anything moves. A move that
  waited for the owner meets the endpoint's refusals again after the answer. Then the hub queues
  the owner's `user.message` "You moved to <path>, project <name>; continue the task." in the
  session, as a run of an agent's `hqc_schedule` does (§16): no `peer`, so it waits for no
  answer as an ask would; its chain is the chain of the call's turn (kept in the request while
  it waits; it has no `ask` cap, which refused the call) moved on (§7.1), built after the move,
  so the cap of a session in `ask` is named by its address in the new project; that turn runs
  in the session's own mode: in `ask` a move from it is refused for that cap, in `auto` moves
  after moves end at the hops limit. The worker writes no input into the
  running turn while a move waits (§17.2), so that input starts the agent's first turn in the
  new folder. Once per `request_id` (§5.2): the input's `event_id` is `once_id("hqc_move wake",
  …)`; a repeat of a move made at once finds that input and answers from where the session is,
  before anything that depends on the turn's chain (that input may have started the turn the
  repeat is said to run in), and a repeat of a call that asked finds its request: still waiting
  (deferred again, no second card) or its stored result. The description tells the agent to call it only when the user asks, that
  a folder other than one of the user's workspaces in the same environment waits for the user
  to allow it in the chat and the call fails when the user denies it, to end its turn after it,
  and that from the next turn it works in `path`, the conversation goes on and its address is
  `address`; `INSTRUCTIONS` (§7.2) say the same in short. Only this tool queues an input; a move
  from the client does not (§6).
- `hqc_rename(title, session?)` (since 0.103) — renames a session as the client's Rename does
  (`PATCH /api/sessions/{id}` with `title`, §6): the calling session when `session` is absent,
  otherwise the session `session` names, an address or an id of a session of the owner, not
  archived, not a worker's service session, else `not_found` (the target rules of §7.1, except
  that the calling session itself and a session driven from VS Code are accepted). The title
  is made one line (runs of whitespace, line breaks included, become one space, none at the
  ends); an empty one or one longer than 200 characters is `invalid_args`. It is stored
  with `title_source: user` (§3 Session), so the runtime's title no longer replaces it; the
  address does not change. The result is `{address, title}` — the session's address and the
  title as stored. A repeat of the call (§5.2) sets the same title again.

Session address: `<project-slug>/<session-slug>` or the id. A slug is unique within the project
and does not change on rename.

A tool is listed in `tools/list` only once its task lands (iteration 3: `hqc_list_sessions`
since 0.33, T30; `hqc_attach` since 0.34, T31; `hqc_send`, `hqc_ask` since 0.35, T32;
`hqc_search_history` since 0.36, T33; `hqc_spawn` since 0.37, T34; `hqc_schedule` since 0.38, T35;
`hqc_move` since 0.76; `hqc_rename` since 0.103); the list is shared by the worker's MCP
server and the hub (`hqc_proto::mcp::TOOLS`), the hub's implementations are a registry next
to it (`crates/hqc-hub/src/mcp.rs`). Errors have the one shape `{code, message}` (§5.2); the MCP
server returns them as a tool result with `isError: true` and the text `<code>: <message>`.
Codes added in 0.35: `timeout` (no reply in time), `limit_exceeded` (a chain limit, §7.1),
`worker_restart` (the asking worker restarted; never seen by the agent, whose call died).
Until grants (iteration 6) every tool works only on the calling session's owner's own
projects and sessions; anything else is `not_found`.

### 7.1 Semantics of send and ask **[decision]**

Delivery:
- A `peer.message` is always delivered to the agent as **input text** with the header
  `[hqc peer from <project>/<session> | mode=ask | request_id=... | reply with your final message]`
  or `... | mode=send]`. This way the agent tells ask from send in any runtime. Since 0.55
  (T52) the send's header is `... | mode=send | another session of your owner: do what it asks
  as if your owner asked, within your permissions; answer with hqc_send only if it needs an
  answer]`: a send wakes its target, which should neither stop to ask the user nor answer
  every message (the turn's permission cap still holds). Since 0.47
  (T42) the ask's header ends `| reply with your final message | return files with hqc_attach
  or in files of hqc_send]`: both reach the asker with the reply (below).
- Spoofing protection: when building `session.input`, the worker escapes the substring
  `[hqc peer` (anywhere in the text, case-insensitive) to `[hqc-peer-text`
  in **all** the text it passes to the runtime: the user's text, the text of a peer message
  from an agent and the `peer_prefix` block; the real header is added after escaping. The
  history on the hub is not rewritten.
- The runtime's text (since 0.35, T32; the worker builds it from `session.input`, §5.3): the
  "Messages from other sessions" block if `peer_prefix` is not empty (after a move that
  stopped background tasks, the line of §5.3 comes first), then the header line if
  `peer` is set, then the escaped `text` (with the inbox paths of §12). The block is
  `Messages from other sessions (they await no reply):` (since 0.55 `Messages from other
  sessions (they await no reply; each is from <the same note as the send's header>):`), an empty line, then per message
  `[hqc peer from <address> | mode=send | sent=<ts, UTC, seconds>]`, its escaped text (since
  0.46 followed by the `Attachments:` block of its own files, `.hqc/inbox/<its event_id>/…`) and an
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
  `peer_buffer` (migration `0015`; 64 KB is the sum of the texts in bytes; files do not
  count, they are not in the frame but downloaded by the worker); the eviction event
  is `system peer_evicted` in the target. Since 0.46 (T41) a buffered message carries its files
  with it: its entry in `peer_prefix` has its `event_id` and `artifact_ids` (§5.3). When the hub sends an input it marks the buffered
  messages not carried by another input still `pending` as carried by this one and puts them
  in its `peer_prefix`, oldest first; the input's `ack` removes them from the buffer (their
  events keep no `delivery`; since 0.54, T51, they get `delivered_at`), a send that arrives meanwhile waits for the next input. The
  input queue takes `wake_on_send` as it is when the message is sent; there is no API to set
  it yet. Since 0.55 (T52) it is on by default and on every existing session, so a send wakes
  its target (2026-11-01: sends to idle sessions waited until the user typed there); the buffer
  is left for a session with it off and for the sends joined to an ask that closed without a
  reply (below). Since 0.39 (T36): an input sent again (after a reconnect) carries exactly the
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
  mirror stores in the target; it closes the ask with `{from, reply}` (§7). Since 0.46 (T41)
  the reply also carries the files of the answering turn: the artifacts of the target's
  `attachment` events between the question (the `peer.message`) and that final message,
  oldest first, each once, at most 32, only artifacts of the session's owner (a worker can
  write an `attachment` event itself, and another owner's id in one is left out), as
  `files: [{artifact_id, name, kind}]` in the stored `mcp.result`; the hub links them to the
  asking session so that its worker may download them, and that worker answers the agent with
  local paths (§7, §5.2). The final message has no files of its own: what the target wants to
  hand back it attaches with `hqc_attach` (or, since 0.47, sends to the asker with `hqc_send`
  and `files`, below). The timeout gives
  `error {code: timeout}`; a reply after it, or after `mcp.cancel`, goes nowhere. Since 0.54
  (T51) the ask keeps the reply's `event_id` (`payload.reply_event_id`), and the asking
  worker's `ack` of the `mcp.result` (or a repeat of the ask answered from the stored result)
  is when the reply was delivered: the first such time is kept on the ask
  (`payload.reply_delivered_at`) and shown on the reply as `reply_delivered_at` (§6). A
  worker's `error` for the frame and the results dropped after its restart are not a
  delivery. A target
  whose turn on the question is lost (`system turn_lost`: its process stopped, its worker
  restarted, the watchdog) gives no reply; since 0.39 (T36) the ask then fails at once with
  `unavailable` instead of waiting for its timeout (the question is not delivered again).
  Since 0.69 a turn on the question that ends with `status idle` and no final message (the
  user's "No" to a permission request ends it so, §8.1, as `POST /stop` does; or the turn
  ended by itself without text) fails the ask at once too: `unavailable`, "the turn of
  <session> on the question ended without an answer"; before, it waited for the timeout. The
  turn is the one the status names with `in_reply_to` (§4), not the target's last delivered
  input: the worker acks the next input outside its outbox (§5.2), so that ack may reach the
  hub before the `status idle` of the turn before it, and an ask queued behind that turn would
  fail as its own turn starts. A `status idle` without `in_reply_to` (an older worker, or a
  turn that names no input, §8.1) fails nothing, and the ask waits for its timeout. The asking
  session's turn stays `running` while it waits; the turn watchdog (§17.3) leaves it alone. In the asking session's feed the client shows the
  waiting call as "Waiting for a reply from <session>" with its timer (the runtime's
  `tool.call` of `hqc_ask`, no event of its own).
- Since 0.47 (T42; the 2026-10-31 run, where B sent a changed file back with `hqc_send`
  instead of `hqc_attach`, the reply had no file and the send waited in A's buffer): an
  `hqc_send` from the target to the asker, made from the target's turn on the question (the
  turn whose input is the ask's `peer.message`, by the turn rules of the chain below) while the
  ask is open, goes with the reply instead of the asker's send buffer. Its `peer.message` is
  stored in the asker without `delivery` and not buffered, the ask keeps its `event_id` in
  `payload.joined`, and the send's result is `{to, delivery: with_reply}`. The reply's text is
  the final message followed, for each such send in `seq` order, by the separator
  `\n\n--- sent with hqc_send while answering ---\n` and the send's text; its files are those
  of the turn's `attachment`s, then the sends' `artifact_ids`, in order, each once, at most 32
  in all, owner's only, downloaded by the asking worker like any reply file. The hub locks the
  ask's row from the check until the send is on it, and the reply locks it from reading the
  joined sends until it closes the ask, so a send either is in the reply or finds the ask
  closed and is buffered. A send from the target outside that turn (an earlier turn still
  running when the question came, a later one, after the final message), to another session,
  or from another session to the asker is unchanged. An ask that closes without a reply
  (timeout, the target's turn lost, `mcp.cancel`, the asking worker's restart) puts its joined
  sends into the asker's send buffer, so they still reach the agent with its next input. A
  repeated send acts once and answers `with_reply` again (§5.2).

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
  carried in `peer_prefix` do not start a turn and lend it no chain (since 0.44, T40, only
  their permission cap, below).
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
- Since 0.43 (T39), a message cannot raise permissions (user decision, 2026-10-25): a turn woken
  by a peer message (`hqc_ask`, `hqc_send` with `wake_on_send`, the prompt of `hqc_spawn`) or by
  a run of an agent's schedule runs in `auto` only if the target and every session up the chain
  run in `auto`; otherwise in `ask`. A turn started by a person's own message runs in the
  session's mode. The hub keeps this as the chain's `permission_cap` (`ask` | `auto`), set
  where it sets the chain: the cap of an outgoing message, spawn or schedule is the lower of
  the calling session's mode and the cap of the input that started the calling turn (a
  person's input has none). The session's mode here is `ask` if the user asked for `ask`
  (`permission_mode`, §3; since 0.45, so a switch to `ask` counts at once, before the worker
  confirms it), otherwise its `effective_permission_mode` (what its worker reported; `ask`
  before any report). With `ask` the chain also carries `capped_by`, the address of the session that set it:
  the earliest session of the chain found in `ask` (kept down the chain), for the header. An
  agent's schedule stores the cap with its chain when it is made, and its runs carry it.
  `session.input.permission_cap` takes it to the worker, which applies it to that turn only
  (§5.3); the target's own mode does not enter the cap, since a target in `ask` runs in `ask`
  anyway. A chain from before 0.43 has no cap.
- Since 0.44 (T40; user decision, 2026-10-26), two gaps of that rule are closed:
  - A buffered `send` (no wake) puts its text in front of the target's next input
    (`peer_prefix`), so that input's turn takes the lowest cap of the buffered messages it
    carries together with its own (a person's input has none of its own): text from a session
    in `ask` never runs in `auto`. With `ask`, `capped_by` is that of the input's own chain
    if it has an `ask` cap, else that of the first such buffered message. The hub decides it
    when it first hands the buffer to the input (the same messages go with a resent input) and
    keeps it with the input (`input_caps`, migration `0023`; the input's stored `chain` is not
    changed): it is the cap of the turn, so the header shows it and what the turn sends,
    spawns or schedules carries it on. A buffered message from before 0.43 has no cap.
  - An agent's schedule caps each run with the lower of the cap stored when it was made and
    its creator's `effective_permission_mode` at run time (`ask` before any report, as above),
    `capped_by` the creator's address when it is the creator that lowers it. A deleted
    creator counts as `ask` (`capped_by`: "a deleted session"), and so does the unknown
    creator of a schedule made before 0.44. The run's `user.message` stores the chain with
    that cap.
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
- `initialize` gives `instructions` (since 0.66): that the tools act with the owner's rights,
  that the user reads the session in the hqc chat, that files made for the user (images,
  charts, reports, diffs) go through `hqc_attach` rather than as a path, and every other tool by
  its use (`hqc_proto::mcp::INSTRUCTIONS`, a test checks each tool is named). Claude Code keeps
  server instructions in the system prompt while the tools wait behind `ToolSearch` (§8.1), so
  the agent knows them from the first turn; without them it did not attach files unasked
  (checked on 2026-11-04 with Claude Code 2.1.310: haiku quoted them with only the tool names
  visible).
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
--permission-prompt-tool stdio --thinking-display summarized [--strict-mcp-config] --mcp-config '<json>'
--allowedTools mcp__hqc [--disallowedTools <rules…>] [--settings '<json: the environment's rules, crossSessionInbound?>']
(--setting-sources user,local | --setting-sources user,project,local | --restricted)
--permission-mode <"ask" mode | auto>
[--append-system-prompt <text>] --session-id <uuid>` for a new session
(the hub assigns the uuid and puts it in `runtime_session_id`) and `--resume <uuid>` to
continue. The choice between them depends on whether the transcript
`<config>/projects/<encoded cwd>/<uuid>.jsonl` exists (§9), not on `mode` from `session.start`:
both `start` and `resume` of the adapter use `--resume` when the file exists, otherwise
`--session-id`. Since 0.100 a session with `has_history` that gets `--session-id` (its
transcript is gone: the CLI deletes those older than `cleanupPeriodDays` at its start, 30 days
by default; RUNBOOK sets it high) has `system transcript_missing` in its feed once the process
started: the agent goes on without the conversation, and the owner knows. A session that moved (§5.3, §6) finds no file there: since 0.75, before every
start, when the transcript is not at the place of the working directory, the worker looks for
`<uuid>.jsonl` in every folder of `<config>/projects`, by uuid rather than by the old folder, so
a move also survives a worker restart between the hub's `session.start` and the agent's start,
and moves it there with its `<uuid>/` folder (subagents): only when exactly one file has that
uuid, only one written in another folder (the `cwd` of its mirror row, which a move sets, else
the folder its records give, §9; symlinks resolved, through its deepest existing ancestor when
the folder is gone, as a worktree deleted after the move) inside `allowed_roots`, never over an
existing file or folder (a hard link, then the old name is removed), and not while a live Claude
Code process
outside the worker holds it: such a process would go on appending to the moved file while the
agent appends too. The worker reads Claude Code's registry of live sessions (§9.3) at that
moment, its own agent processes left out as the mirror leaves them out, and does not rely on the
hub's `held_by`, which may lag; without the registry folder there is no such check. Otherwise the
start fails (`agent_start_failed`, §5.2; for a holder the message names its `entrypoint` and
pid) and the file stays where it was. A transcript written in the
working directory itself, under another name of it (a path through a symlink), is not moved. The mirror's row for
the new path is written first (§9.2) and removed again when the file does not get there. This is
the one place where hqc moves a Claude Code transcript; elsewhere it never modifies or deletes
one **[decision]**. That `--resume` in the new folder keeps the conversation is
what the live test `live_move_resumes_in_the_new_folder` checks **[verify]**.
`--permission-prompt-tool stdio` is required: without it the CLI asks the host
nothing and rejects outright everything that needs confirmation
(`system/permission_denied`, `permission_decision.source: config`), and `AskUserQuestion`
is not in the tool list (checked on 2.1.307). `--allowedTools mcp__hqc`
allows all tools of the hqc MCP server so their calls do not go through `can_use_tool`
(§7); on 2.1.307 the forms `mcp__hqc` and `mcp__hqc__*` work the same; without the flag an
MCP tool call arrives as `can_use_tool`. `--thinking-display summarized` (since 0.69) is what the
VS Code extension passes: without it the `thinking` blocks of opus 5.5 come with empty text on
2.1.312, so there was no `agent.thinking`; with it they carry the summarized thinking, in
stream-json and in the transcript under the same `uuid` (checked live on 2.1.312 with haiku).
The flag is not in `--help` (`choices: summarized, omitted, highlights`); the binaries of
2.1.233, 2.1.277 and 2.1.312 have it. The worker passes it without a version check, and the CLI
refuses an option it does not know (`error: unknown option`, 2.1.312), so a CLI that drops the
flag fails every session start. Checked on 2.1.300: `--resume` continues the same uuid and appends to the same
jsonl file. `--strict-mcp-config` disables MCP servers from `~/.claude.json`
and the project's `.mcp.json`; they can be allowed only through the environment (§14,
`allow_local_mcp`: since 0.56 such a session runs without the flag, the hqc server still comes
in `--mcp-config`). `--settings` carries the effective rules of the session's environment
(§14: `[permissions]` merged with the environment's; since 0.84 with `allow_subagent_messages`
also `crossSessionInbound`, below), and the adapter's own policy (deny
before asking, `policy_hint`) reads the same merged lists. In the process env: `MCP_TOOL_TIMEOUT` (§7.1): milliseconds, default
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

A tool loaded with `ToolSearch` keeps the schema it had then (since 0.47, T42; checked on
2.1.308 with `ENABLE_TOOL_SEARCH=true`, live test `live_tool_schema`, notes in
`docs/notes/t42-questions.md`): the CLI writes the loaded tool's whole definition (description,
`input_schema`) into the transcript as an attachment `deferred_tools_record`, and the
`ToolSearch` result itself is only a `tool_reference`. A `--resume` with a restarted MCP server
that lists a changed schema (here `hqc_send` without and then with `files`) still sends the
recorded definition to the model (the request's shape hash did not change); the model named
the old parameters and the old last sentence. A call with a new argument still goes through:
the model may pass it anyway and the server checks its own schema. So after a worker update a
resumed session does not learn about new tool arguments; a new session does. hqc does not
rewrite Claude Code's transcripts; what an agent must know about returning files is also in the
ask's header (§7.1), which is built fresh for every input.

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
(a session in `auto`, §3, §14, gets `auto` instead). Checked on 2.1.308 (T12,
fixture `always-git-push`): with `--setting-sources user,local --permission-mode manual`,
`always` is still offered and writes the rule to `settings.local.json`; `manual` shows in
`system/init` as `"permissionMode": "default"`. Since 0.83 the worker owner can lift this for
one environment (§14, `project_settings`): its sessions get `--setting-sources
user,project,local`, so the CLI also reads the project source, the repository's `CLAUDE.md`
and `.claude/settings.json` (checked on 2.1.312 with `claude -p --model haiku`: with
`user,local` the agent does not see the project's `CLAUDE.md`, with `user,project,local` it
does). Turning it on
trusts every repository in that environment's folders with everything its
`.claude/settings.json` sets: hooks that run outside the worker's `deny` and approval requests,
permission rules, `env` that reaches the CLI itself, `defaultMode` as far as the explicit
`--permission-mode` does not override it (§14). A restricted session still gets `--restricted`
and no `--setting-sources`.
The process environment is scrubbed: from the worker's environment only `PATH`, `HOME`, `USER`,
`LOGNAME`, `SHELL`, `LANG`, `LC_*`, `TERM`, `TMPDIR`, `CLAUDE_*`,
`ANTHROPIC_*` pass, then the `env` of the session's environment (§14; it may replace one of
those, e.g. `PATH`), then the worker's own `SSH_AUTH_SOCK` (only in an environment with
`ssh_keys`: the socket of this process's own ssh-agent, §14; since 0.57 the user's
`SSH_AUTH_SOCK` no longer passes), `CLAUDE_CODE_ENTRYPOINT` and `MCP_TOOL_TIMEOUT`;
`HQC_*` never. The worker
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
its `info/exclude`; since 0.59 also on every input of a workspace that has a `.hqc/` folder, files
or not, so a repository made after the first file came (the agent's `git init` writes a fresh
`info/exclude`) gets it before the next turn. A block is appended to the input text: an empty line, `Attachments:` and a
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
`deny` → deny as the "No" of the VS Code tab (since 0.69; extension 2.1.309): `message` "The user
doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit,
the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell
you how to proceed." with `interrupt: true`, so the CLI ends the turn at once: the `tool_result`
with that text, a `user` text `[Request interrupted by user for tool use]` (mapped to nothing)
and `result` `error_during_execution` (`terminal_reason: aborted_tools`), so the turn ends with
`status idle` as after a stop, and the process takes the next input (checked live on 2.1.312);
since 0.71 `interrupt` is `false` for a request of a background subagent (one not started in
the foreground), which outlives its turn: the call is denied, the turn running by then goes
on; before 0.69 it was "The user denied this tool call." and the model went on; `always` → allow plus `updatedPermissions`; Claude Code itself writes the
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
`always` maps to `session`. The hub stores no permissions of Claude Code's own; its only
standing ones are hqc's allow rules of a folder (since 0.52, below). The
`AskUserQuestion` tool arrives through the same `can_use_tool`: the adapter turns it into
a `clarify.request`, assigning its own `id`s to the questions, and returns the answers via
`updatedInput.answers`, whose keys are the question text and whose values are the option's
`label` (the adapter maps `id` to text itself), a typed text instead of a label (the tab's
"Other"), or for `multiSelect` the labels joined with `", "`, as the VS Code tab answers. Since
0.69 the question carries `header` (from `questions[].header`) and `option_descriptions` (from
`options[].description`, paired with the labels: an option without a `label` is dropped with its
description). The
`control_request`/`control_response` format was checked on 2.1.307 with `--permission-prompt-tool stdio`;
exact JSON: `docs/notes/claude-control-protocol.md`; fixtures:
`crates/hqc-claude-jsonl/fixtures/*-2.1.307.jsonl`.

Mapping of stream-json to §4: `stream_event` text delta → `agent.delta`; `assistant`
with text → `agent.message`, where the adapter holds the last text until the
next stream item: `tool_use` → `final: false`, `result` → `final: true`;
`tool_use` → `tool.call`; `tool_result` → `tool.result`; `result` → `usage` and
`status idle`. Since 0.69 a `result` with `num_turns: 0`, no tokens and no cost (a local slash
command, the empty second `result` the CLI sends after a turn it started by itself) gives no
`usage`, which the feed would show as an empty `0 → 0 tokens` line; `/compact` has no tokens
either but costs, and keeps its `usage`. Since 0.69: an `assistant` text whose `message.model` is `<synthetic>` — a reply the
CLI wrote itself (an API error, "Credit balance is too low"; on 2.1.312 with `is_api_error_message`
and the same `uuid` as the transcript record) — → `agent.message` with `runtime: true`, held
and ended like any other text: the turn it ends has it as the final `agent.message`; `system`
`task_notification` (`task_id`,
`status`, `summary`, `output_file`; checked live on 2.1.312 with a `run_in_background` Bash: it
comes when the task ends, after the `result` of the turn that started it and before the turn
the CLI then starts by itself, which brings `system/init`, the answer and its own `result`) →
`system task_notification` with its `summary`, "Background task" without one, unless a subagent
started the task (its `system` `task_started` has `owned_by_subagent: true`, also after a move to
the background) or its `task_started` had `is_backgrounded: false` and no `system`
`task_updated` with `patch.is_backgrounded: true` moved it to the background later (2.1.312
sends both lines for a foreground subagent, a foreground Bash call that runs for more than about
2 s and a subagent's Bash calls too, background ones included; a task the process did not see
start counts as a background one); since 0.70 a subagent's `task_notification` without its
report (no `handback` field) that comes while the latest `background_tasks_changed` list
(below) holds the subagent or a task of it is no end: the adapter keeps the latest one and
handles it (the line, the cancel of the subagent's open requests) when the subagent stops
counting or the process exits; a notification with the report replaces it and is handled at
once. 2.1.312 sends two for a subagent whose command it moved to the background: "This agent
has not reported yet…" without `handback` while the list holds the subagent and its commands,
and its real end, `handback: "send"` and `handback_report {text}`, after the subagent's
`SubagentHandback` call. A notification with `handback` is the end at once, also while a
command the subagent left running is still listed (live on 2.1.312: a subagent's wait loop
that never ended). A task gives one line: a later `task_notification` of the same `task_id`
only cancels the subagent's open requests (the CLI's own description of the field: any
notification "can be followed by a later one for the same task_id when the agent is resumed or
finishes its wait; the later one replaces what the earlier one showed"; checked live on 2.1.312:
a subagent that reported while its 200 s `ping`, moved to the background, ran on is listed and
started again when the `ping` ends, and sends a second notification with `handback: "send"` and
the same summary); `system` `compact_boundary` (`compact_metadata.trigger: manual`
after `/compact`, 2.1.308 and 2.1.312; an automatic compaction was not checked; its `uuid` is
that of the transcript's `system compact_boundary` record) →
`system context_compacted` "Context compacted" with the `event_id` derived from that `uuid`; the `--replay-user-messages` echo, a `user` message with `isReplay: true`,
→ `ack` of `session.input` (this field tells the echo from a `user` with `tool_result`).
Since 0.70 every user message the adapter writes carries a fresh v4 `uuid` (not the input's
`event_id`: 2.1.312 drops a line whose `uuid` it has seen, echo and all, without a turn); its
echo and its `command_lifecycle {command_uuid, state}` lines carry it (2.1.312: `queued` when
the line is read, `started` when the CLI takes it, right after the echo for one that went into a
running turn, then `completed` or `cancelled`; a turn the CLI begins by itself has none). The
input of a turn, for `in_reply_to`, is the one whose `started` or echo begins a turn or comes
during a turn the CLI began by itself (the first input of a process, and every input with a CLI
that sends no such lines: the one written). At `result`
an input written and not echoed is taken when its last line is `started` or `completed` (a
local slash command), carried into the next turn, which the CLI then runs by itself, when it is
only `queued`, and not taken when `cancelled`; without lifecycle lines it is carried when it was
written during the turn, taken otherwise. A carried input whose line after that `result` is
`cancelled` (a second Stop before it started) ends the turn it was carried into, for which no
`result` comes: it is not taken, the other inputs carried with it stay carried, and with none
left no turn runs. A carried local slash command has a `result` of its own, which takes it
(`/name` without an echo, fixture of `local_command_without_echo_is_taken`; `/cost` on 2.1.312). `system` `task_started` with
`task_type: local_agent` (a subagent in the background or not, a subagent's own too) counts as
a running subagent until `task_notification`, or a `task_updated` whose `patch.status` is not
`running`, of the same `task_id`; a foreground one (`is_backgrounded: false`, not moved to
the background since) cannot outlive the main turn and stops counting at its `result` at the
latest, as an interrupt, a denied call or `error_max_turns` during it may bring neither line;
the count goes into `session.state.subagents` (§5.4). `system` `background_tasks_changed`
(2.1.312) carries `tasks[]` (`task_id`, `task_type`, and `parent_task_id` for a task a subagent
started, as in its `task_started`): the CLI's whole list of the session's running background
tasks, subagents and shell commands, sent on every change and `[]` at the end. A subagent also
counts while the latest list holds it (`task_type: local_agent`) or a task of the list has it
as `parent_task_id`, the latter until a `task_notification` with its report (`handback`) and
again from its next `task_started`: a command a subagent that reported left running is a
background task of the session, not a running subagent. With `CLAUDE_CODE_ENTRYPOINT=claude-vscode` 2.1.312 moves a subagent's
Bash call that runs for more than about 180 s to the background (the agent's own 240 s Bash
call stays in the foreground) and reports the subagent ended (`task_updated` `completed`,
`task_notification` "This agent has not reported yet: it is waiting on its own background
work…") while the command still runs; when the command ends it lists the subagent again and
starts the same `task_id` again (checked live on 2.1.312 with a background subagent's 240 s
`ping`). With one such command the list goes `[]` first and holds the subagent again only on
the next line **[verify]** (inferred from that capture, where a second task of the subagent, a
Monitor waiting for the `ping`, kept the list from going empty), so a drop of the count that a
`background_tasks_changed` alone brings is reported at the next line of another kind (at a
`result` before the end of the turn). The
number of tasks in the list stays in the worker: the agent is not idle while it is above 0
(§17.3), and a move tells the agent how many it stopped (§5.3).
The CLI repeats on stdout every `control_response` the adapter sent to stdin;
the adapter skips those lines. `in_reply_to` = the `event_id` of the turn's input, as above,
once its echo has come (since 0.69; before that, none); an input written into a running turn
does not change it.

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
(`Read(./x/**)`, `WebFetch(domain:…)`) to the CLI. A `Bash` deny rule is matched against the
whole command and against each piece between `;`, `&`, `|`, backticks, `$`, parentheses,
redirections and new lines; since 0.62 the shell's words that may stand before a command in a
piece (`do`, `then`, `else`, `elif`, `if`, `while`, `until`, `!`, `{`, `time`) are skipped first,
so `for h in a; do ssh $h; done` hits `Bash(ssh:*)` as the CLI's own parser does.

hqc's allow rules of the folder (since 0.52, T48; §3 Workspace) never go to `--settings`: the
adapter applies them in `can_use_tool`, after the deny check above and the `session` rules and
before a card, and answers `allow` without an `approval.request`; the list is read at every
request, so a change (`workspace.rules`, §5.3) reaches a running process at once. The matcher is
the worker's own and conservative (`crates/hqc-worker/src/permissions.rs`): a command is split
into simple parts at `;`, `&&`, `||`, `|`, `&` and new lines, honouring quotes; it gives up — no
rule covers the command, the card comes — on command substitution (`$( )`, backticks, `$` inside
double quotes), subshells and `{ }` groups, here-documents, process substitution, comments and a
redirection to anything but `/dev/null`, another descriptor or a path that stays inside the
workspace. Every part must be covered by an hqc rule or an `allow` rule of `[permissions]`, at
least one of them by an hqc rule (a call the CLI asked about although `[permissions]` alone
allows it is still asked, as before). A rule's words are compared with the part's first words;
`git -C <path>` is skipped when `<path>` stays inside the workspace (absolute under it, or
relative without `..` while no earlier part is `cd`, `pushd` or `popd`). Suggestions for the card
(`approval.request.rule_suggestions`, §4): `Bash(git <subcommand>:*)` for git (none when an option
stands where the subcommand should, or `-C` points outside), `Bash(<first word> *)` otherwise
(none for an assignment such as `FOO=1 make` or a word a rule cannot hold), the tool name for
another tool. In `auto` the CLI sends no `can_use_tool`, so hqc's rules play no part there.
Checked live on 2.1.308 (`claude_live::live_folder_rules`): `Bash(touch *)` answered the request
for `touch a && touch b`, no card.

The `auto` mode (`--permission-mode auto`, checked on 2.1.308): the CLI decides itself and
**never asks**: the classifier let through `rm -rf` outside the workspace, `git push --force`
and `sudo` when the user had asked for it; the decision is visible only in
`tool_result_meta[].permission_decision.reason_type` (`classifier`, `mode`). The only
boundary in this mode is `deny`.

Switching a running process (since 0.42, T38): the adapter writes `{"type": "control_request",
"request_id": "hqc-mode-<uuid>", "request": {"subtype": "set_permission_mode", "mode": "auto" |
"default"}}` to stdin (`default` for `ask`: the CLI takes it and reports `manual` as `default`
anyway). Verified on Claude Code **2.1.308** (fixture `permission-mode-switch`, live test
`live_permission_mode_switch`): the CLI answers `control_response {subtype: success, response:
{mode}}`, sends `{"type": "system", "subtype": "status", "permissionMode": "<mode>"}`, and the
next `system/init` carries the new `permissionMode`; a `touch` in the working folder then ran
without `can_use_tool` in `auto` and was asked about again after the switch back. The CLI applies
the mode to its next permission check, so the worker does not wait for the end of a turn (a
request already open stays open); the `--settings` deny list stays in force (it was given at
start). The adapter reports `permissionMode` of `system/init`, of `system/status` and of the
answer to its request as the session's mode (`auto` is `auto`, any other name `ask`) when it
changes; a refusal (`subtype: error`) is logged and changes nothing, and the next start uses the
new flag. No restart is needed on 2.1.308. Since 0.43 (T39) every success answer to the
adapter's own request is reported, also of the mode the session already had: a capped input
(§5.3) waits for that confirmation before it is written to stdin. Verified live on 2.1.308
(`live_cap`): a process started in `auto` confirmed `default` within milliseconds of the
request, and the next turn's `touch` was asked about.

Model (since 0.74). `agent.model` goes to `--model`; `session.model` for a running process is
`{"type": "control_request", "request_id": "hqc-model-<uuid>", "request": {"subtype":
"set_model", "model": "<name>"}}`, whose answer is only logged. Checked live on 2.1.312 with
haiku: the CLI answers `success` with the full model name at once, also during a Bash call, and
the next model call of the same turn already uses the new model (the turn's `modelUsage` has
both).

Limits (since 0.81). The CLI writes a `rate_limit_event` before each turn's answer (2.1.310):
`rate_limit_info.unifiedWindows.{five_hour, seven_day}` with `utilization` and `resetsAt`; the
adapter sends them as `worker.limits` (§5.4). Per-model weekly windows (`/usage`'s "Current
week (Fable)") are not in it.

Fork (since 0.80). A session with `session.start.fork` and no transcript yet starts with
`--resume <source> --fork-session --session-id <its uuid> --resume-session-at <record>`: the CLI
writes a new transcript with the source's records up to and including `<record>` (their
`uuid`s kept) and continues there; the source is not touched. Checked live on 2.1.310 with
haiku in stream-json mode: the forked conversation knows only what came before the cut. The
adapter registers the copied main-chain records as its own (§9.3), so the mirror neither
imports them nor takes them for another writer. The CLI writes the switch into the transcript as a typed `/model <name>` record; the
adapter counts it as its own input (§9.3), so the mirror imports no prompt from it (since 0.77).

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
session gets a turn with no input event in its history. By default the worker adds both to
`--disallowedTools`; `allow_native_messaging = true` in `worker.toml` brings them back.
Agents talk to each other through `hqc_send`/`hqc_ask` (§7).

`SendMessage` is also how an agent continues its own subagent: with `--disallowedTools
ListAgents SendMessage`, `system/init` of `claude -p` 2.1.312 lists neither tool (`Task` and
`TaskStop` are there). `allow_subagent_messages = true` in `worker.toml` (since 0.84; default
false, `allow_native_messaging` wins over it) keeps `SendMessage` for that: only `ListAgents`
goes to `--disallowedTools`, and `--settings` carries `"crossSessionInbound": "refuse"`, the
CLI setting (`accept` | `hold` | `refuse`) for messages from other sessions, so no other local
session starts a turn in an hqc session past the hub. The setting is in the binaries of
2.1.304, 2.1.312 and 2.1.318. Checked live on 2.1.312 (`claude -p` with haiku,
`--disallowedTools ListAgents`, `--settings
'{"voiceEnabled":false,"crossSessionInbound":"refuse"}'`): the agent started a subagent and
continued it with `SendMessage` (the subagent answered the second message); of two background
`claude -p --input-format stream-json` sessions, both listed by another session's
`ListAgents`, the one with `refuse` ran no turn and the sender got the delivery notice "that
session is not accepting cross-session messages", the one with `accept` ran a turn and
answered. The other direction stays open, and that is what the worker owner accepts by turning
the option on: checked live on 2.1.312, a sender `claude -p --permission-mode manual
--disallowedTools ListAgents --settings '{…"crossSessionInbound":"refuse"}'` called
`SendMessage` to a background session with `accept`; it was delivered without a permission
request and the receiver ran a turn. So an hqc agent in `ask` can hand work to any local
session that accepts such messages (a VS Code tab in `auto`, say) past the hub, its permission
requests and the chain limits (§7.1); without `ListAgents` it has to know the session's name.
Per the 2.1.312 binary, not checked live **[verify]**: the permission check of `SendMessage`
allows the call in every mode but `auto`; the permission-mode parity hold stops only a receiver
in `bypassPermissions`; the tool can also reach the account's cloud and Remote Control
sessions. A restricted session (§8.4) keeps `SendMessage` disallowed under the option too:
`--restricted` does not remove it (it is not among the tools `--help` of 2.1.312 names for the
flag). Which
`crossSessionInbound` applies, per the 2.1.312 binary **[verify]**: the first one set of
managed policy, `--settings` and user settings; project settings and `settings.local.json` can
only tighten it (`accept` < `hold` < `refuse`).

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
the jsonl records, keeps a "file → cwd" cache on disk (§5.2) and uses the folder name as a key
and, since 0.95, to tell which recorded `cwd` is the transcript's folder. A record's `cwd` is
where the agent was when it was written: it changes inside one transcript when the agent `cd`s
(worktrees, sibling projects, `/private/tmp`), and a chat moved to another project by copying
its transcript into that project's folder (same file name, so the same uuid) and continued there
starts with the old folder's records. So the transcript's folder is the first recorded `cwd`
whose encoding equals the name of the transcript's folder under `projects/`; when no record has
such a `cwd`, the first recorded `cwd`. Subfolders with subagent transcripts and files that are
not `<uuid>.jsonl` are ignored.
Verified on 2.1.307: besides `user`/`assistant` the file has `attachment`,
`file-history-snapshot`, `api-request*`, `last-prompt`, `ai-title`, `cost-state`,
`queue-operation`, `system` and other records; the parser skips them, except `ai-title`, which
it parses separately (the `aiTitle` field; the adapter takes the session title from it, §8.1),
and since 0.98 `attachment`, whose `queued_command` is a message typed during a turn (§9.1).

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
the worker walks `<config>/projects/*/<uuid>.jsonl`, takes each transcript's folder by the rule
of §9 (the "file → cwd" cache on disk, §5.2), groups by `cwd` with the session count and
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
prompt; `tool_use` → `tool.call`; non-empty `thinking` → `agent.thinking`. Since 0.69: `assistant`
text with `message.model` `<synthetic>` → `agent.message` with `runtime: true` (§8.1), `final` by
its `stop_reason` as any other; a `user` record with `origin.kind: task-notification` (2.1.312:
`origin {kind, producer: session-task, runId}`, `promptSource: system`, `turnOrigin:
task_notification`, text `<task-notification><task-id>…<status>…</status><summary>…</summary>…`)
→ `system task_notification` with the `<summary>`, else "Background task", never a `user.message`
and not a prompt (an answer after it points at the last prompt a person typed). Since 0.98: a
message the person typed during a turn — an `attachment` record with `attachment.type:
queued_command`, `commandMode: prompt`, `origin.kind` `human` or no `origin` (before 2.1.210,
and on some later ones that carry `delivery_id`),
not `isMeta` (another session's message) and not a sidechain; its `prompt` is a list of blocks or
(2.1.296+) a string — → a `user.message` (`delivery: delivered`) per non-empty text block, at the
attachment's own `timestamp` (when it was typed); it is not a prompt either (an answer after it
points at the turn's prompt). An answered `AskUserQuestion` — a `tool_result` whose record has
`toolUseResult {questions, answers}`, `answers` keyed by the question's text — gives after its
`tool.result` a `clarify.request` with the questions as the adapter parses them (§8.1) and a
`clarify.response` with the answers by question id, so the client shows the closed card with the
chosen options; the CLI joins a multi-select answer with ", ", which the mirror splits back by the
question's option labels, the longest label first, and any rest is one answer typed as "Other".
A declined question (`is_error`, no `questions` in the record) stays a `tool.result`. Records
that yield no events (other `attachment`s, snapshots, `api-request*`, `queue-operation`, empty
`thinking`, images) are skipped.

For an unknown `runtime_session_id` of this worker the hub creates a session in the
workspace's project: `cwd` is the record's path relative to the workspace, the title is the
`ai-title` (the frame's `title` field) if present, otherwise "Session N" (§3 Session),
`system session_created` "Session imported from Claude Code". The initial `locked_by` follows
the frame's `recent` flag: the worker sets it if the file was modified less than 30 s before
it was read (the worker's clock, not the hub's); then `vscode`, otherwise `none`. An author the
worker does not know is filled in by the hub: the workspace owner for `user.message` (and
since 0.98 `clarify.response`), the session's agent for the rest. No push is sent for mirror
events (an import is hundreds of old replies). Since 0.86 a mirrored record whose `ts` is before its session's `created_at` on the
hub (the record's time is the worker's clock, `created_at` the hub's) is read for the session's
owner (§3 ReadCursor), and so is the `session_created` of an
imported session: an imported session comes in read, with the history the owner has seen in VS
Code; a record written after the import counts as unread by the rule of §6.

**Import limits** (§20, since 0.23 T22; behaviour at the daily limit since 0.24). New sessions
from the mirror: at most 200 per 24 h per worker (since 0.85 `HQC_MIRROR_SESSIONS_PER_DAY`;
`0` imports no new transcript, known ones go on) and 2000 in total per workspace;
`runtime_session_id` is a uuid, `cwd` is up to 4096 bytes. The first wave after the update to
0.23 (migration `0011` turned the mirror on for every workspace) may bring more transcripts than
fit in a day: the hub does not accept a record of a new transcript over the daily limit:
`error mirror_quota`, `retryable: true`. On such a reply the worker removes the record from the
outbox (the §5.2 attempt counter does not grow) and rewinds the transcript to the start: the file
is not read until an **hour** has passed since the last rejection, then it goes again in full
with the same `event_id`s. So the wave spreads over several days (200 sessions per 24 h by default) without
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
A transcript the worker moves for a moved session (§8.1, since 0.75) gets its row at the new path
before it is moved: `cwd` the new folder, the offset at the file's end, the old row's last prompt
and `ai-title`. The mirror reads the row before the records, so it neither imports the file
again nor counts it to the old folder by the `cwd` of its old records. When the file does not
get there (its folder or the link fails), the row is removed again.

**The session follows its transcript** (since 0.95). A row whose `cwd` is not its file's folder
by the rule of §9 (a copy read before the new window wrote to it; a copy's row written before
0.95) is not settled: on every pass, before the workspace is picked (so a copy out of a folder
that is not mirrored is found too), the worker searches the file for a `cwd` that is, the whole
file the first time in its process and afterwards only the bytes appended since the last search
(the size searched is kept in memory); a `cwd` found becomes the row's, and the records of that
pass go out with it. The hub keeps one session per transcript and finds it by worker and
`runtime_session_id`, not by workspace; when a record the session does not have yet comes from
another workspace of the session's owner than the session's, the session moves there once the
record is stored: into that workspace's project, `cwd` the transcript's folder (the frame's
`cwd`) relative to the workspace, as an import there would get it, the connection's start state
back to "not started" as after a move (§6). A record the session already has does not move it:
a copy read from its start brings all of the old file's records again. Nor does one older than
the session's newest event that is not `system` (most `system` events are the hub's own, with
its clock): right after a copy the old window still writes a few records into the old file, and
both files may be read again from the start, so the session ends where the newest record was
written. The hub keeps the newest time it read per session for the connection: it only grows,
so an older record needs no query (a copy out of a folder that is not mirrored has no `since`
and comes whole). No event goes into the feed: right after a copy both files get records for a
while, and the session may switch a few times.

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
2.1.308) and of the prompt if its echo comes with a different `uuid` (see §9.3). Since 0.105 a
prompt record whose typed text is several unmatched inputs in a row joined by `\n` is own too,
and consumes them: the CLI writes inputs it takes from its queue together, such as a message
typed while a fresh process starts, as one prompt record with a new `uuid` (2.1.312); before, the
mirror took that record for another writer's and the chat showed it as a third message. Since 0.69 a
background task's notification record (§9.1) is own while the session's hqc process is alive:
the adapter reported its stream-json `system task_notification` line, which has another `uuid`
(2.1.312), so the record is neither emitted nor held nor taken for another writer. Since 0.98 a
message typed during a turn (§9.1) is matched by its typed text (its first text block) like a
prompt, and while the session's hqc process is alive it is own even when unmatched: stream-json
does not carry the attachment, and only hqc writes to that process's stdin. Its `timestamp`
counts for the first pass's `since_ts` like any record's. The mirror
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
once / session / always / deny, for ACP agents whatever the agent offered. Since 0.69 the card of an
Edit or MultiEdit call shows the change as a diff of `old_string` → `new_string` (each edit of a
MultiEdit), of a Write the content, read from the call's `tool.call`; a preview cut at 4 KB is
shown as far as it goes and marked cut. The request goes to
**the worker owner**; a collaborator with `write` sees the request but cannot answer it.
The first answer wins: it closes the request, every device sees the `approval.response`
event, and the client removes the buttons. A second answer (from another device) gets
`409 request_closed`; no cancel event is written in that case. Expiry, `POST /stop`, a worker
restart and archiving produce `approval.cancel`, and the client removes the buttons.
All these cancellations are written by the hub: on expiry, `stop` and archiving it writes the
`approval.cancel` event and sends the worker an `approval.response` frame with the
`canonical: deny` option, so the adapter closes the request in the runtime (§5.3; since 0.69
that deny also ends the runtime's turn, §8.1); on a worker restart, only the event (§5.2). On `POST /stop` the hub first cancels open requests with
`stopped`, then sends `session.stop`; the adapter and the worker's supervisor do not emit a
cancel on `session.stop`. Since 0.69 the Claude Code adapter cancels a request of the main agent
or of a foreground subagent still open when the runtime's turn ends with
`approval.cancel`/`clarify.cancel {stopped}`: after "No" to one of two parallel calls the CLI
ends the turn and no longer waits for the other (before, the adapter forgot it and the card
stayed open, since Claude's requests do not expire). A subagent's request carries `agent_id`,
the `task_id` of the subagent's `task_started` (the main agent's has none; checked live on
2.1.312). A foreground subagent (`task_started` with `is_backgrounded: false`, not moved to the
background since) cannot outlive the main turn, so its request goes as the main agent's; every
other subagent's request (a background one's, a nested one's) stays open: a background
subagent's outlives the main turn's `result`, and the CLI still waits for its answer (checked
live on 2.1.312). A subagent's request still open when its `task_notification` comes (the
subagent ended) is cancelled there the same way, whichever comes first, that or the main
turn's `result`; a notification without the subagent's report that comes while the latest
`background_tasks_changed` list holds the subagent or a task of it is no end (§8.1), and the
cancel waits with it. The hub stores
a worker's cancel only if it closes an open request, in the transaction that closes it, so a
cancel that fails to be stored leaves the request open for the worker's resend: one it closed
already (`stop`, an answer that crossed the cancel) keeps its own event. The hub cancels an open `clarify` the same way: a
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
message. The Claude Code adapter answers with a `control_response` `deny` with `interrupt:
false` whose `message` is the VS Code tab's wording for "Tell Claude what to do instead" (since
0.69; extension 2.1.309): "The user doesn't want to proceed with this tool use. … The user
provided the following reason for the rejection:" and two spaces before the user's text; the
model gets it as the tool result and continues the turn. Checked live on 2.1.312
(`claude-haiku-5-5`): the model did what the text said and did not run the call. Before 0.69 the
wording ended "To tell you how to proceed, the user said:" and the text followed on a new line;
on 2.1.308 a plain "The user denied this tool call and wrote instead:" was seen but taken for an
instruction from a tool result and not followed. A deny without text stops the turn (§8.1). This is input into the current turn, not
a new `user.message`: it has no queue and no `delivery`. For ACP agents (§8.2), later; worker
requests (§10.2) and `clarify` have no such field (`400`).

A session in the `auto` permission mode (since 0.42, T38; §3, §8.1) produces no
`approval.request` for tool calls: Claude Code's classifier decides them on the worker, and only
the worker's `deny` stops one; `AskUserQuestion` still reaches the user as `clarify.request`.
Only the worker owner switches a session to `auto` (§6); spawned sessions start in `ask` (§7).
This is the worker's mode, unlike `auto_approve`, which the hub applies to requests that do
reach it. A message cannot raise it (since 0.43, T39; §7.1): a turn woken by another agent runs
in `ask` unless every session up the chain runs in `auto`, so a session in `auto` asks about the
tool calls of a turn that a session in `ask` started, directly or along a chain; the header
says so ("Ask (asked by <address>)", §6). Requests of such a turn go to the worker owner as
always and are answered in the target's feed or from the sessions waiting on it. Since 0.44
(T40) the same holds for a turn whose input carries a buffered send from a session in `ask`,
a person's own message included, and for a run of an agent's schedule whose creator runs in
`ask` now or was deleted.

**Allow in this folder** (since 0.52, T48). Next to "Always allow" (the exact command,
`.claude/settings.local.json`) the card offers "Allow in this folder" when the request has
`rule_suggestions` (§4) and the session a workspace: a field per part with the rule the worker
suggests, editable (an invalid one blocks saving; an empty one adds no rule for its part), and a
note that the rule applies only when every part is allowed; on a phone a full-screen page from
"In this folder…". "Save and allow" is one user action: the client saves the rules (`POST
/api/workspaces/{id}/rules`, §6) and only when they are stored answers the request with the
`canonical: once` option; if saving fails, nothing is answered. This keeps §2.1: a rule is a
standing answer of the owner within the worker's policy, the hub could answer `once` anyway, and
no rule lifts the worker's `deny`.

**A move of the session** (since 0.76). An agent's `hqc_move` into anything but a ready
workspace of the owner in the environment of the session's folder (§7) is an
`approval.request` the hub writes in the calling session, `author {kind: system}`, `tool:
hqc_move`, a description with the reason, the project the session lands in and the path it
moves into once allowed, and, for a folder that is no workspace yet, that it becomes a
mirrored workspace whose Claude Code sessions are imported into hqc; options `once` / `deny`, no
`expires_at`, with a PendingRequest `move` (§3); a move that would be refused anyway gets no
card, the call gets the error (§7). Only the session owner answers it (`403` for anyone else);
it takes no `message` (`400`); `auto_approve` never answers it. It counts as any open request:
the session is `waiting` (§17.1), it is in the Requests list and in the feeds of the sessions
waiting on this one, it pushes (§16). The answer is the usual `approval.response` event; no
`approval.response` frame goes to the worker, the call gets its `mcp.result` (§7). It has no
`expires_at`, but the call waits at most the runtime's tool timeout (`ask_timeout_s` plus a
minute, §7.2), whose `mcp.cancel` closes it (below); the turn watchdog (§17.3) leaves a session
waiting on it alone, as one waiting on an `hqc_ask`; `POST /stop`, archiving and the agent's process stopping cancel it as an
approval, with `approval.cancel` and the call's `mcp.result` error; a worker restart writes
only the event, and so does `mcp.cancel` of the call (§5.2), with `approval.cancel {stopped}`.
The client draws it with the card of a worker's request (§10.2): the description, "Allow" and
"Deny", no "Reply with text"; outside a service session its header is "The agent asks to move
this session".

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
  {}`. `kind`: `workspace.use` (below), `workspace.create`, `workspace.delete` (iteration 4,
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

**New folder** (`workspace.use`, since 0.17; the only use until iteration 4). The worker keeps
a cache of folders in which it has agreed to run sessions: `$HQC_HOME/known_workspaces.json`
(an array of canonical paths, mode 0600, written through a temporary file). A `session.start`
with a `workspace_path` whose canonical path is not in the cache, for a session with
`owner_is_worker_owner: false` and without `requested_by_owner`, is not executed by the worker
right away (since 0.75 also when a running session moves to such a folder, §5.3): it sends `worker.approval.request {kind: workspace.use}` with the description "the
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
| answer a `move` request (an agent's `hqc_move`, §7, §10.1) | | | | session owner only |
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
At start (since 0.63, T53) the hub writes and removes a probe file in the store and, in a
container (`/.dockerenv`), compares the store's device with the root's: a store it cannot
write to, or one on the root's device (the container's own layer, gone with the next deploy
while the rows stay), is a warning in the log. The hub starts anyway.

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
The rule is the link in `artifact_sessions` to a session of this worker; since 0.46 (T41) a
`peer.message` with files links them to its target, and a reply with files to the asking
session (§7.1), so the target's and the asker's workers may download them.

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
are saved as artifacts by the adapter itself (§8.2). Agent → agent (since 0.46, T41): the
`files` of `hqc_send`/`hqc_ask` go up by the same worker upload and down into the target's
`.hqc/inbox` like a user's files; the files of a reply into the asker's (§7, §7.1). In the
feed a `peer.message` shows its files under its text like a `user.message`.

**Project icons** (since 0.51, T49, §3 Project). `PUT /api/projects/{id}/icon` takes PNG, JPEG
or WebP only, recognized by the content's magic bytes (the name and the declared type do not
count: a PNG named `.webp` is a PNG; SVG and anything else is refused), square, 128 to 1024 px
on a side by the image's header, at most 256 KB (the upload stops as soon as it passes that);
a refusal is `422 invalid_icon`. `GET /api/projects/{id}/icon` is for the project's owner (under
Access, like the API, not by a signed link) and answers with the checked `Content-Type`,
`X-Content-Type-Options: nosniff`, the artifact `sandbox` CSP, `ETag` (the sha256; `304` on
`If-None-Match`) and `Cache-Control: private, max-age=31536000, immutable`: the `?v=` of the
project's `icon.url` changes with the image.

**Cleanup** (since 0.23, T21). Once a day (the first time right after start) the hub deletes
artifact records that no event references (`artifact_ids` of `user.message` and, since 0.46,
`peer.message`, `artifact_id` of
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
the hub stores the canonical path. Since 0.64, when the folder is missing (`400
path_not_found`) the form offers "Create the folder and add" in place of "Add" (until the path
or the worker changes), which sends `create: true`. Only the worker's owner can add a workspace to a worker (until
the sharing of iteration 6 a worker has no other users), so there is no confirmation through the
service session yet; if the worker is offline, the workspace is not created. Since 0.58 (T45)
`clone`: the client gives the URL, the parent folder and a name, the hub checks the parent and
stores the workspace `creating`, the worker checks the new path itself and clones into it in its
environment (§5.3); the workspace becomes `ready` or `failed` (§3 Workspace). Since 0.60 (T46)
`worktree`: the client gives the origin workspace, the branch (new from the origin's `HEAD`, or
an existing one) and optionally the folder (by default next to the origin,
`<origin-name>-<branch>`); the hub checks the origin (§3 Workspace) and the parent, the worker
checks the origin, the branch and the new path itself and runs `git worktree add` in the origin
with the environment of the new path (§5.3), through the same `creating` → `ready` | `failed`.
Since 0.61 (T47) `hqc_spawn` with `isolation: "worktree"` makes an `ephemeral` worktree the same
way for the session it starts (§7); the owner's agent asks for it, so there is no confirmation.

### 13.2 Deletion
`dir`: only the hub's record is deleted, the folder is left alone; while the workspace has
non-archived sessions — `409 workspace_in_use` (§6) unless the user confirmed archiving them
(`archive_sessions`, since 0.64), archived ones stay without a workspace. `clone` and `worktree`
are deleted from disk only with `created_by_hqc: true`: `worktree` via
`git worktree remove`, `clone` by deleting the folder. Before deletion the worker checks for
uncommitted changes and unpushed commits; if there are any, the confirmation request
contains a warning. Since 0.58 (T45), for `clone`: the confirmation is the owner's dialog in the
client (iteration 4, `docs/ITERATION-4.md`), fed by `workspace.inspect`; deleting the folder is
`DELETE ...?remove_folder=true`, which sends `workspace.delete`, and the worker deletes only a
canonical path of its own `created_workspaces.json` (§5.3, §14). Without `remove_folder` the
folder stays, as for `dir`. A `failed` clone is only a record. Since 0.60 (T46), for
`worktree`: the same dialog and `remove_folder`; the worker runs `git worktree remove` in the
origin (`--force` only with `force`, which the client sends when the dialog listed uncommitted
files) and `git worktree prune`, and keeps the branch; the inspection lists only the worktree's
own branch. An origin is not deleted, record or folder, while worktree records of it exist
(`409 workspace_in_use`): worktrees go first. An `ephemeral` workspace is **not deleted automatically** when its last
session is archived; it is marked "for deletion" and shows a button;
deletion is always confirmed. Since 0.61 (T47): the mark is `marked_for_deletion`, computed by
the hub (§3 Workspace), and the button is "Delete" on the workspace's row in the project
settings, which opens the dialog above; the folder goes only through it.

### 13.3 Project session template (backlog since 0.50)
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
- `environments{}` — named environments (`[environments.<name>]`, below; since 0.56, T43);
- `env_bindings[]` — records `{path, environment, hermes_profile?, foreign_profile?}`
  (`[[env_bindings]]`; the profiles are iteration 5). A binding is looked up **by the workspace
  path** (not by the session's `cwd`), canonical (symlinks resolved; the binding's own path is
  resolved again at every lookup, so a binding through a symlink names its target); the longest
  binding that equals or contains the path wins, compared by path components (`/a/b` does not
  contain `/a/bc`), otherwise `default`. A binding may name `default` to give a subfolder of a
  bound folder back. The worker resolves it itself on every `session.start` (since 0.56 the frame
  carries no environment). A binding to an environment the file does not define, or the same
  path bound twice, is a config error;
- `acp_agents{}` — `{name: {command: <absolute path>, args[], isolation:
  verified|none}}`;
- `allow_foreign_sessions`, `share_local_models`;
- `max_sessions` (agent processes at once, default 8) and `insecure_http`
  (`http://` to a non-loopback hub, §5.1);
- `clone_timeout` (since 0.58, T45) — seconds a `git clone` (since 0.60 also a `git worktree add`) of a new workspace may take, default
  600, at least 1 (§5.3);
- `[permissions]` — the Claude Code permission policy (below; not sent in `hello`; the
  rules screen gets it through `permissions.inspect`, §5.3).

The hub gets a copy in `hello` (environments with their rules but never the values of `env`,
bindings; §5.4) and shows it; only the worker owner can change it, by editing `worker.toml` on
the machine and restarting the worker. `hqc-worker env` lists the environments (variables as
`NAME=***`, ssh keys by name) and the bindings; `hqc-worker env which <path>` prints the
environment of a folder (and the binding that decided it), its keys by name and its effective
rules, each with its source. `hqc-worker env add-key <name> <file>` puts a key into the store
(below; since 0.57, T44).

Permission policy `[permissions]` (in effect since 0.14 for all of the worker's Claude Code
sessions; since 0.56 an environment can extend it with its own `permissions`, below):

```toml
[permissions]
mode = "ask"     # "ask" | "auto": sessions that do not ask for a mode
allow_auto = true  # false: a session that asks for "auto" runs in "ask"
allow = ["Bash(cargo test:*)", "Read"]   # run without asking
deny = ["Bash(sudo:*)", "Bash(git push:*)"]  # never run and never ask
```

- `mode = "ask"` (default): calls outside both lists are asked about (`approval.request`);
  `mode = "auto"`: `--permission-mode auto`, the CLI decides by itself and never asks (§8.1),
  the only boundary is `deny`; the worker writes a warning to the log at start. Since 0.42
  (T38) `mode` is the default of a session that does not ask for a mode: hqc may ask for
  `ask` or `auto` per session (`session.start.permission_mode`, `session.permission_mode`,
  §5.3);
- `allow_auto` (default `true`; since 0.42, T38): `false` — the worker runs every session in
  `ask` whatever hqc asks for, and reports `ask` (§5.4); `mode = "auto"` with `allow_auto =
  false` is a config error;
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
and `auto` do not override it) → the worker's `allow`, the `always` rules from
`settings.local.json` and (since 0.52, T48) hqc's allow rules of the folder (§3 Workspace, applied
by the worker in `can_use_tool`, §8.1) (run without asking) → everything else (the owner is asked
in `ask`, the CLI's classifier decides in `auto`). hqc's rules come from the hub, like the
owner's answers to requests (§2.1); they cannot lift a `deny`.

An environment defines what the agent process is launched with (Claude Code or an ACP process,
both launched by the worker):

```toml
[environments.infra]
env = { GITHUB_TOKEN = "..." }        # variables on top of the child allowlist
ssh_keys = ["infra_ed25519"]          # keys of the worker's store, $HQC_HOME/keys/
allow_local_mcp = false
project_settings = false
[environments.infra.permissions]
unblock = ["Bash(ssh:*)"]             # lift worker-wide deny rules here only
allow = ["Bash(ssh debian@192.168.1.20:*)"]
deny = ["Bash(git push:*)"]           # added to the worker's deny

[[env_bindings]]
path = "/Users/drzlo/work/lab/infra"
environment = "infra"
```

- `ssh_keys` (since 0.57, T44): names of keys in the worker's store (below). Every agent process
  of a session in the environment gets an `ssh-agent` of its own (one per process, never shared,
  not even between sessions of one environment) on a socket in `$HQC_HOME/agents/` (0700), with
  the keys loaded by `ssh-add` (a key with a passphrase fails the start, nothing prompts), and
  `SSH_AUTH_SOCK` of that socket in its environment only. The ssh-agent stops when the process
  ends (stop, kill, crash, idle stop, worker shutdown); a worker that died leaves sockets behind,
  and its next start empties those agents of their keys and removes the sockets. Without keys
  `SSH_AUTH_SOCK` is not passed at all: since 0.57 a process the worker runs no longer gets the
  user's agent (§8.1), so `git` over ssh works only in a folder whose environment has a key.
  Processes the user starts (a terminal, VS Code) are not touched. A key name is letters,
  digits, `_`, `-` and `.`, not starting with `.` or `-`; the names are not in `hello`;
- `env`: variables and tokens (GitHub etc.), put into the agent's process on top of the child
  allowlist (§8.1); names of letters, digits and `_`, never `HQC_*` and never `SSH_AUTH_SOCK`
  (since 0.57). The values stay on the worker: not in `hello`, logs, errors or `hqc-worker env`
  (`***`);
- `permissions` (since 0.50, replaces the earlier free-form `settings`; in effect since 0.56):
  `unblock` — rules of the worker's `deny` lifted for this environment only (a rule not in that
  `deny` is a config error), `allow` and `deny` — added to the worker's lists. The effective
  rules of a session: the worker's `deny` minus `unblock`, plus the environment's `deny`; then
  the worker's `allow` and the environment's `allow`; then hqc's folder rules (applied in
  `can_use_tool` below every deny, §3 Workspace); then the session's mode. They go to
  `--settings` and to the adapter's policy (§8.1); the three levels below stay. **Claude Code
  only**, ACP agents have no equivalent, so for them the `default` environment is weaker: the
  only boundary is the absence of keys;
- `allow_local_mcp`: allow MCP servers from the machine (Claude Code: without
  `--strict-mcp-config`, the hqc server stays; Hermes: without
  `HERMES_ACP_SKIP_CONFIGURED_MCP`); default false;
- `project_settings` (since 0.83): Claude Code reads the project setting source too,
  `--setting-sources user,project,local` instead of `user,local` (§8.1): the repository's
  `CLAUDE.md` and `.claude/settings.json` (and, per Claude Code's documentation, the skills,
  agents and commands in its `.claude`). Turning it on trusts every repository in the
  environment's folders as Claude Code's folder-trust dialog would: everything its
  `.claude/settings.json` sets applies — hooks (shell commands the CLI runs itself, at session
  start too; the worker's `deny`, `--disallowedTools` and approval requests do not see them),
  permission rules, `env` (per Claude Code's documentation it reaches the CLI itself, e.g.
  `ANTHROPIC_BASE_URL`) and the rest; `defaultMode` as far as the explicit `--permission-mode`
  does not override it. A repository hqc clones into such a folder (§5.3 `workspace.create`) is
  trusted the same way. Claude Code reads the project source from the session's working
  directory, while the environment is chosen by the workspace: a subfolder bound back to
  `default` is still read when a session of the bound workspace runs in it. A restricted
  session (§8.4) still gets `--restricted`. Kept on the worker like `ssh_keys`: not in `hello`,
  `hqc-worker env` shows it. **Claude Code only**, ACP agents ignore it; default false;
- `launch`: `process` (the only value in the first version; `os-user` and `container`
  are reserved).

`launch` is not accepted in `worker.toml` yet. `default` cannot be
defined in the file; the name of an environment is letters, digits, `_`,
`-` and `.`; an unknown field is a config error (as in `[permissions]`). An environment no
`[[env_bindings]]` names (since 0.62) is not an error, but no folder runs in it: the worker
logs a warning at start and `hqc-worker env` marks it, since it is most often a table under the
wrong name (`[environments.x.permissions]` after `[environments.y]`).

The folders the worker created for workspaces (since 0.58, T45) are in
`$HQC_HOME/created_workspaces.json` (canonical paths, a clone that runs marked as creating;
since 0.60 a worktree with the canonical path of its origin;
written to a temporary file and renamed, 0600), next to `known_workspaces.json`: the only
folders `workspace.delete` removes; at start the worker removes the folder of every entry still
marked as creating (and prunes a worktree's origin). `git` for a workspace (§5.3) runs with the same process environment as an
agent of that folder's environment (`Environment::process_env`), where its keys come in (T44);
`git worktree add` with the environment of the new folder, not the origin's. A worktree shares
the origin's `.git` (config, hooks, filters), so whatever runs in one folder's sessions can
plant what git runs in the other's environment: two folders bound to different environments are
not separated by making one a worktree of the other.

The `default` environment: no keys, no tokens, `deny` from `[permissions]` (by default it
includes ssh/scp too), local MCP off. Keys of one project (for example `infra`) are moved out of
`~/.ssh` into the worker's store (files in `$HQC_HOME/keys/`, 0700 and 0600, decided on
2026-10-31: Keychain is locked in ssh sessions on a Mac) and are available only to sessions on paths
with that environment. The store holds one private key per file, named as in `ssh_keys`;
`hqc-worker env add-key <name> <file>` creates it with these modes and copies the key in with
0600, and never replaces a key of the same name. The worker does not start (and `hqc-worker env`
does not print) when the store, a file in it or `$HQC_HOME/agents/` is open to group or others,
is a link or belongs to another user (the message names the `chmod`), or when a name in
`ssh_keys` is not in the store. The worker reads key material only in `add-key`; logs, frames
and `hqc-worker env` carry key names, never keys.

Limitation: without a container or a separate OS user the agent sees the files
and the process environment of the user, including other workspaces and session tokens on the
same machine. The boundary is the absence of keys; prohibitions in settings are the first line,
not a boundary. So without a separate OS user the agent can still read `~/.ssh` and use the keys
there directly: a project's keys must be moved out of it into the store, and only then does a
folder without them lose ssh to that host. Likewise a session can reach another session's
ssh-agent socket while that process runs (same OS user, §2.1). Windows in the first version:
only `env` and `permissions`, no `ssh_keys`.

## 15. Search

Postgres full-text search over what a session said and did, and session titles. Results are
filtered by grants. One index, two consumers: the search bar in the client and
`hqc_search_history`.

Since 0.36 (T33; `crates/hqc-hub/src/search.rs`, its measurements in
`docs/notes/t33-questions.md`), reworked in 0.97 (migration `0037`) so that
it finds commands, their output and word forms and puts the best match first:
- **What is indexed.** `payload.text` of `user.message`, `agent.message` (final or not),
  `peer.message` and `system`; since 0.97 also a `tool.call` (the tool's name and its
  `args_preview`), a `tool.result` (`result_preview`) and a `clarify.response` (the strings of
  its `answers`); in previews the JSON escapes `\n`, `\r`, `\t` are read as spaces, so a word
  after a line break stays a word, and everywhere `/` and `.` are, so a path, a file name, an
  address or a URL is a sequence of words (`search` finds `crates/hqc-hub/src/search.rs`). Not
  thinking, requests or attachments. One stored generated column of `events`, `search_words`,
  the `tsvector` of that text in the `hqc` configuration, weighted A for messages and answers,
  B for calls, D for output, so Postgres keeps it on every insert and the migration's table
  rewrite backfills old rows; a GIN index on it and one on session titles (an expression
  index, as titles change).
- **Configuration `hqc`.** A copy of `russian` whose Latin words go to an English and the rest
  to a Russian Snowball stemmer, both without stop words, so every word stays searchable and
  "переезд" finds "переезда", "deploy" finds "deployed".
- **Whose.** Until grants (iteration 6): the user's (the calling session's owner's) sessions,
  archived ones included and marked `archived: true`, the workers' service sessions left out.
- **Matching.** The hub parses the query itself into a `to_tsquery('hqc', …)`: every word must
  be there, `"a phrase"` in sequence, `or` either side, `-word` without it, `word*` a prefix
  of the word as typed or of its stem; a prefix of Latin letters also finds the whole stems,
  of 4 letters at least, shorter than it and beginning it, that the stemmer gives it with
  common English endings (`s`, `ed`, `ing`, `ion`, `ment`, `er` and the like, not `y`, `ies`,
  `ly`), as the stem of a longer word may be shorter than the prefix (`connecti*` finds
  "connection", whose stem is `connect`), while `worker*` does not find "work"; `/` and `.`
  in a word are spaces, as in the index, so `docs/SPEC.md` is a phrase; each word and phrase
  is quoted, so punctuation cannot become an operator. A `-` excludes only before a quoted
  phrase or four letters or more (`-test`, `-staging`; a shorter word is excluded in quotes,
  `-"ok"`); before anything else it is part of the word, so a command pasted with its flags
  finds its call: `-rf`, `-lah`, `--force` are the words `rf`, `lah`, `force`, as the index
  reads them. Fuzzy (at least 3 characters) widens each plain word of 3 or more characters,
  leading dashes not counted, with up to 6 similar words (`pg_trgm` `similarity` 0.4 to its
  stem) from `search_vocab`, the words of the index (letters only, 3–40 characters) that the
  migration collects and a trigger on `events` adds to; it finds typos (`pg_trgm` is a trusted
  extension, created by migration `0016`). Titles are matched with the same query; a title hit
  has no `seq`.
- **Order and pages.** Best match first: `ts_rank(search_words, q, 1)` (the query without its
  `-word`s), then newest; a title is ranked as text of weight A. The cursor is the number of
  hits already shown, so a page after new events may repeat or skip a hit.
- **Snippet.** About 160 characters on one line around the first query word (or fuzzy
  alternative) found in the text (ignoring case), else its beginning; `…` marks a cut. The
  text of a call is the tool and its arguments, of an answer its strings joined with "; ".
- **Cost.** Each statement of a search (reading the query, then finding the hits) runs with
  `statement_timeout` 5 s; over it the search is refused. Over our hub's 916k events (720k
  indexed): a rare or ordinary word or phrase 14–80 ms, a word in 11–36k events 0.2–0.6 s,
  "the" (229k events) about 2 s; fuzzy 40–560 ms, with a word in most events 2–3 s (SPEC
  0.97, a copy on a shared build host). The migration took 4–5 minutes there, holding
  `events` locked. Since 0.39 (T36, §20): per owner 30 searches a minute, 10 of them fuzzy
  (`GET /api/search` and `hqc_search_history` together; `429 search_limit` /
  `limit_exceeded`), and at most 4 searches run at once on the hub (`503 busy` /
  `unavailable`), so one owner's agent cannot keep the database busy with fuzzy queries.
- **Client.** Desktop: the left column's search field filters the project's sessions by title
  and path at once (as before) and, from 2 characters and 300 ms after typing, shows the hub's
  hits below them ("In messages", "Similar words" for fuzzy, "More" for the next page). Phone:
  the list's search button opens `#/search`, a screen with the field, the sessions of all
  projects matching by title or path, and the hub's hits. A hit opens `#/s/<session>?seq=<seq>`:
  the chat loads its history as always (the newest page, then the older ones, §6) and, once the
  event is there, renders the feed from the block that holds it if it did not and scrolls the block that holds it (the last block starting at or before `seq`) to the middle
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
  and its runs carry it (§7.1); since 0.43 (T39) with the chain's permission cap, fixed when
  the schedule is made: a run of a schedule made by a session in `ask` runs in `ask` (§5.3).
  Since 0.44 (T40) the cap of each run is the lower of that cap and the creator's
  (`created_by`, §3) mode at run time, taken as in §7.1: a run of a schedule whose creator has moved
  from `auto` to `ask` since runs in `ask`, and so does one whose creator was deleted, or was
  not stored (a schedule from before 0.44). The hub sends at most 60 runs per owner and hour; a run over
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
`session.input` (the worker passes input to the runtime one per turn, except an input that joins
a running turn, §17.2: its `ack` has `joined: true` and opens none, since 0.70) and closed by
`agent.message` with `final: true`, `status` with `idle` or `error` (the latter means "the turn
ended with an error"), the process stopping or the watchdog (§17.3, also "the turn ended with
an error").
"The turn ended with an error" keeps `error` until the next input: a new `user.message`
clears the flag. `worker_offline`: the session has a worker and it is not connected now.

### 17.2 Input during a turn **[decision]**
Input from the input queue (§7.1) that arrives during a turn waits for the final answer
and starts the next turn; one turn per item, in `seq` order. Since 0.70 a Claude Code turn
takes a person's input as the VS Code tab does: while a turn runs, the worker writes the head
of the queue into it when all of these hold: the input has no `peer` (an `hqc_ask` waits for
the answer of its own turn) and no `schedule_id` (a run of a schedule, §16), its `text` does not
start with `/` after leading whitespace (a slash command; since 0.71), the turn's own input has no `peer` either (the answer of an
`hqc_ask` goes to its sender: a person's input would shape it or, carried with the ask into a
turn the CLI runs by itself, take it), neither the input nor the turn is capped to `ask`
(§5.3), and the agent has no open `approval` or `clarify`. A person's input that carries
buffered sends of other sessions (`peer_prefix`, §7.1) may join: the cap of §7.1 makes it `ask`
whenever one of them comes from a session in `ask`, and a capped input never joins. The hub
honours `joined` only for a `user.message` without `schedule_id`; a `joined` on any other input
is the worker's mistake, logged, and the input's turn opens (since 0.71). Otherwise it waits for the end of
the turn as before (scheduled runs and slash commands always do), and so does everything behind
it, so `seq` order holds. Since 0.75 nothing is written into the turn either while a move of
the session waits (§5.3): the input is for the agent in the new folder. The CLI takes the input
at its next step, after the result of the
tool call running then, and the worker answers `ack {event_id, joined: true}` on its echo; the
turn's final `agent.message` stays `in_reply_to` the turn's own input. When the turn ends
before that (the input came during the final answer, or before an interrupt), the CLI runs it
as the next turn by itself (§8.1): the first such input becomes that turn, its echo gives a
plain `ack`; the others stay written into it and are acked with `joined: true` when the CLI
takes them (that turn's `in_reply_to` is then the last of them the CLI took; such inputs are a
person's, which nothing answers by `in_reply_to`). The same holds for an input sent during a
turn the CLI began by itself after a background task; since 0.71 `session.stop` interrupts such
a turn too (the adapter knows it from the CLI's `system/init` with no input of hqc's). An input
the CLI cancels after a `session.stop` (a carried input on a second Stop) fails, not retryable,
instead of going back to the queue: the person cancelled it. The "interrupt and send" button does
`session.stop` and then the input. For runtimes with `steer` (Hermes) a "steer" button is
available: `session.steer` without interrupting. Two users with `write` share the same queue.
Each turn runs in its own input's mode (since 0.43, T39): a person's input queued behind a turn
capped to `ask` runs in the session's mode (§5.3), unless (since 0.44, T40) it carries a
buffered send from a session in `ask` (§7.1).

### 17.3 Processes
- An idle agent is left alone by default. With the worker's `idle_timeout` > 0 the
  process exits, and the next input brings it back via resume. Since 0.70 an agent is not idle
  while its subagents run (`session.state.subagents` > 0, §5.4) or while Claude Code lists
  background tasks of the session (`background_tasks_changed`, §8.1). Since 0.70 the worker's
  reconnect to the hub and a change of the holder of any transcript (§9.3: Claude Code in VS
  Code or a terminal on the same machine) do not count as the agent's activity; a holder of
  the session's own transcript stops the agent anyway.
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
- Archiving: the hub sets `archived_at` (since 0.53; unarchiving clears it), sends `session.kill`, open requests get
  `approval.cancel`/`clarify.cancel {archived}`, `pending` inputs are marked `failed`,
  the send buffer is cleared. Unarchiving returns `idle`; the process comes back via
  resume on the first input. Since 0.61 (T47): archiving the last unarchived session of an
  `ephemeral` workspace marks the workspace for deletion, and unarchiving one clears the mark (`marked_for_deletion`, §3 Workspace); nothing is sent to the worker for it.
- Session deletion: events, `ReadCursor`, `PendingRequest`, the session's artifacts
  (unless another session references them) are deleted; jsonl files on the worker
  are **left alone**; late events from the worker's outbox get `ack` and are dropped
  (§5.2).
- Cleanup on the worker (since 0.23, T21). When a session (or its project) is archived or deleted,
  the hub sends the worker, after `session.kill`, the frame
  `session.cleanup {session_id, workspace_path | null, cwd, event_ids[]}`
  → `ack` (example `hub.session.cleanup*.json`; added purely additively, `session.kill` did not
  change): `event_ids` are the session's inputs with attachments (since 0.46 also its
  `peer.message`s with files, buffered ones included, and the `request_id`s of its asks whose
  reply had files, §7.1). The worker deletes
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
  Rust SDK), **[verify]** that it is current. Secure store for the worker token: Keychain
  on macOS (ssh keys of environments are 0600 files, §14); on Linux and Windows in the first version the worker token is a 0600 file (§5.1),
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
   Tasks: `docs/ITERATION-3.md`. Criterion checked by hand on the production hub on 2026-10-31.
   Closed on 2026-10-31.
4. Workspaces: ~~clone~~ (0.58, T45), ~~worktree~~ (0.60, T46), ~~ephemeral~~ (0.61, T47). Environments and bindings on the worker. Moved
   ahead of the agents on 2026-10-31; the session template (§13.3) went to `docs/BACKLOG.md`.
   Tasks: `docs/ITERATION-4.md`. Criterion checked by hand on the production hub on 2026-11-03.
   Closed on 2026-11-03.
5. Generic ACP adapter (the first agent is Hermes, including steer) and `acp_agents` on the
   worker; bare model and local models through a proxy; the personal assistant session
   (moved from 3: its default runtime is the bare model, §8.3), until then `hqc_send` needs
   `session`.
6. Sharing: Grant, the "share" dialog, the "who pays" and foreign session rules. ~~Hub cron~~
   (0.38, T35, with `hqc_schedule`).
7. Tauri, native notifications, several agents in one session.

## 20. Hosting and security

- The hub listens only on the tunnel interface (in OIDC mode, only the reverse proxy reaches
  it); `/api/*` is behind Cloudflare Access with JWT verification (in OIDC mode, behind the
  proxy's sign-in, the hub verifying the provider's ID token, §6); `/worker/*` is outside
  Access, worker tokens only, rate limit on enroll;
  `artifacts.<hub-domain>` is public, signed links with a TTL only.
- The hub's dev mode (`HQC_DEV=1`, no Access) is allowed only on a loopback
  `HQC_HUB_LISTEN` address and without `HQC_ACCESS_*` or `HQC_OIDC_*`; otherwise the hub does
  not start (§6).
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
  no inline scripts (a client test checks this). HSTS is set by Cloudflare (in OIDC mode, by
  the reverse proxy, RUNBOOK §1.9).
- Limits (in the hub process's memory; the address is `CF-Connecting-IP`, otherwise the
  connection's address; with `HQC_CLIENT_IP_HEADER` (since 0.68, required in OIDC mode, §6), for a reverse proxy that puts
  the TCP peer's address in a header of its own, such as Traefik's `X-Real-Ip`, it is that header,
  otherwise the connection's address, and neither `CF-Connecting-IP` nor `CF-IPCountry` (§5.1)
  is read, since there the client could set them): `POST /worker/enroll` — 10 per minute and 10 live unconfirmed codes per
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
  | `mirror.event` (§9.2): new sessions per worker | 200 per 24 h (sliding window); since 0.85 `HQC_MIRROR_SESSIONS_PER_DAY` | the record of a new transcript is not accepted: `error mirror_quota`, `retryable: true`, warn in the hub's log; the worker removes it from the outbox and rereads the transcript from the start no sooner than an hour later (§9.1, since 0.24) |
  | … mirror sessions per workspace | 2000 in total | the event is dropped, `ack` without `seq`, warn in the hub's log |
  | … `runtime_session_id` | lowercase uuid with hyphens | same |
  | … `cwd` | up to 4096 bytes and inside the workspace path | same |
  | a worker event of type `user.message`, `approval.response`, `clarify.response`, `peer.message` (§4: only the hub writes them) | forbidden in any session | `error invalid_frame` |
  | a worker event in its service session (§10.2) | only `system` with `code: worker_log` | `error invalid_frame` |
  | a worker event in an archived session (§17.4) | only one that ends a turn: the final `agent.message`, `status idle`/`error` | `ack` without `seq` |
  | `fs.stat`, `fs.list_dirs`, `workspace.list_candidates`, `permissions.inspect` (since 0.52), `workspace.inspect` and `workspace.delete` (since 0.58) from the API (§5.3, §6) | 4 at once per worker | `429 worker_busy`, no frame goes to the worker |
  | a clone (§5.3, since 0.58, T45) | `url` up to 2048 bytes by `check_clone_url`, `branch` up to 255 by `check_branch`; `clone_timeout` (default 600 s) on the worker | `400 invalid_url` / `invalid_branch`; the worker kills `git` and removes the folder, `failed` |
  | a worktree (§5.3, since 0.60, T46) | `branch` by `check_branch` and `git check-ref-format --branch`; `git worktree add` within `clone_timeout` | `400 invalid_branch`, or `failed` with the worker's reason; on a timeout the worker kills `git`, removes the folder and prunes the origin |
  | `workspace.inspect` | 200 dirty paths (all counted), 100 branches | the rest left out |
  | `workspace.created.error` | stored up to 2000 characters | cut |
  | hqc's allow rules of a workspace (§3 Workspace, since 0.52, T48) | 200 per workspace; a rule at most 256 bytes, hqc syntax only | `422 too_many_rules` / `422 invalid_rule`, nothing saved; the worker drops an invalid rule of `session.start` or `workspace.rules` with a warning |
  | the check of `GET /api/workspaces/{id}/permissions` | `command` / `tool` up to 16 KiB, no NUL | `400` |
  | settings files `permissions.inspect` reads | 1 MB each; only the `permissions.allow/deny/ask` strings leave the worker | the source has `error` |
  | takeover without a registry (§9.3): `locked_by: vscode` without `vscode_active_at` | treated as "written just now" | `409 vscode_active`; the transition to `vscode` sets the time, migration `0012` fills it in for old rows |
  | turn watchdog (§17.3) | a pass every `limit / 10`, at least every 10 s, over the partial index `turn_open` | — |
  | attachment download by the worker (§12, `.hqc/inbox`) | 100 MB per artifact, 500 MB per input (since 0.46 the files of its buffered messages included); streamed to disk | the download is aborted, the partial file is deleted, `error` on `session.input` without retry |
  | attachment display by the client (§12) | a diff body is fetched only once `size` is known; text and diffs over 2 MB get "Download" only; a body with `Content-Length` over the display limit is not read | — |
  | `mcp.call.args` (§5.2, since 0.33) | 64 KB serialized; the worker refuses a bigger call before sending it, the hub checks again | `mcp.result` `error invalid_args` |
  | `mcp.call.session_token` (§5.2) | `hqcs_` + 43 base64url characters, checked before the database | `mcp.result` `error unauthorized` |
  | send buffer of a session (§7.1, since 0.35) | the newest 50 messages and 64 KB of their texts | the oldest dropped, `system peer_evicted` in the target |
  | chain of peer messages (§7.1) | `depth` ≤ 3 nested asks, `hops` ≤ 20 messages | `mcp.result` `error limit_exceeded` |
  | `hqc_ask.timeout_s` (§7) | 1 … `ask_timeout_s` (600 s), larger cut to it | `0`: `error invalid_args` |
  | `hqc_list_sessions` (§7) | 200 sessions per call | `truncated: true` |
  | search query (§15, since 0.36): `GET /api/search`, `hqc_search_history` | `q` 1–200 characters, fuzzy at least 3; 50 hits per page (the tool 20); 5 s `statement_timeout` | `400 bad_request` / `mcp.result` `error invalid_args` |
  | `hqc_spawn` (§7, since 0.37): sessions spawned in one chain (§7.1; a human's turn is one chain); since 0.61 (T47) the owner's ephemeral worktrees still being made count in both spawn limits, in every chain, as the sessions they will get (no limit of their own) | 5 | `mcp.result` `error limit_exceeded`, nothing is created |
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
  | `files` of `hqc_send`/`hqc_ask` (§7, since 0.46) | at most 32; each path by the rules of `hqc_attach` (above), on the sender's worker | `error invalid_args` / `not_found` from the worker, no call is sent |
  | `artifact_ids` of `hqc_send`/`hqc_ask` on the hub | at most 32, each an artifact of the owner linked to the calling session | `error invalid_args` / `not_found`, nothing is stored |
  | files of an `hqc_ask` reply (§7.1) | at most 32, artifacts of the target's owner only; downloaded by the asking worker with the limits of an input (100 MB each, 500 MB per reply) | the rest left out (warn in the hub's log) / that file reported with an `error` |
- Push: the hub POSTs to the subscription's `endpoint`, so only `https://` without
  userinfo and without a non-standard port on push service hosts is accepted: `fcm.googleapis.com`,
  `updates.push.services.mozilla.com`, `*.push.services.mozilla.com`, `*.push.apple.com`,
  `*.notify.windows.com`; redirects are not followed when sending.
- Access JWKS: keys are refreshed outside the cache lock, on a refresh error the previous set
  stays, retries with a growing pause (5 s … 5 min).
- Worker: `hub_url` is `https` only (§5.1), the agent's environment is an allowlist (§8.1) plus
  the `env` of its environment, whose values never leave the worker (§14), the outbox is a 0700
  directory, files 0600. The ssh key store `$HQC_HOME/keys/` is 0700 with 0600 files and the
  agent sockets live in `$HQC_HOME/agents/` (0700); wider modes, links or another owner stop the
  worker at start (since 0.57, T44); a process gets `SSH_AUTH_SOCK` only of its own ssh-agent,
  and only with `ssh_keys` (§14).
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

### 0.105 → 0.107 (the older history of a short feed)
§6 WebSocket: while the rendered feed is shorter than its window, the chat renders 60 more
blocks without a scroll, and "Requests only", on or off, starts the window again at the last 60
blocks of the new view. Before, a chat whose newest 100 events made a few blocks (a long run of
tool calls) opened on those few, with no scroll bar and no way to the older history. 0.104 and
0.106 are taken by other pull requests.

### 0.103 → 0.105 (a prompt record of inputs the CLI took together is the worker's own)
§9.2: a prompt record whose text is several of the worker's unmatched inputs in a row joined by a
newline is the worker's own and consumes them. Claude Code 2.1.312 writes the messages it takes
from its queue together (one sent to start a fresh process, one typed while it started) as one
prompt record with a uuid stream-json did not name; the mirror took it for another writer's, and
the chat showed the joined text as a third message after the two. 0.104 is taken by a pull request
in preparation.

### 0.102 → 0.103 (an agent renames a session)
§7: the tool `hqc_rename(title, session?)` renames the calling session, or another session of
the owner by its address or id, as the client's Rename does (`PATCH /api/sessions/{id}` with
`title`): the title is made one line, at most 200 characters, and stored with
`title_source: user`, so the runtime's title no longer replaces it; the address stays. The
result is `{address, title}`. Before, an agent could not rename a session. `INSTRUCTIONS`
(§7.2) name it.

### 0.100 → 0.102 (a session with a subagent at work is Running)
§6 client layout: an `idle` session whose subagents still run (`subagents`) is in the list's
Running group, and its row and the chat header say "Running"; before, a session whose main agent
had ended its turn sat under Idle while its background subagent worked.

### 0.89 → 0.100 (a missing transcript is reported)
§5.3: `session.start.has_history` — the session's feed has an `agent.message`. §8.1, §4: the
Claude Code adapter that starts such a session anew because its transcript is gone (Claude
Code's `cleanupPeriodDays`, 30 days by default) writes `system transcript_missing`; before, the
agent silently went on without the conversation. 0.90–0.99 are taken by open pull requests.

### 0.97 → 0.98 (messages typed during a turn and answered questions in a mirrored chat)
§9, §9.1, §9.2, §3 Event: a chat mirrored or imported from a Claude Code transcript lacked the
messages the person typed while a turn ran, which the CLI writes as an `attachment` record of
type `queued_command`, and showed an answered `AskUserQuestion` only as a folded tool row. The
mirror now maps such a message to a `user.message` at the time it was typed, and an answered
question to a closed `clarify.request`/`clarify.response` card built from the record's
`toolUseResult`, a multi-select answer split back by the option labels; the hub authors a
mirrored `clarify.response` as the owner. While the session's hqc process is alive a typed
message is the adapter's own input, so it is neither sent twice nor taken for another writer.

### 0.96 → 0.97 (search finds commands, output and word forms, best match first)
§15: search found only messages, word by exact form, newest first, and a fuzzy query on a
common word ran into the 5 s timeout. Migration `0037` indexes tool calls, tool output and
answers to questions as well, with `/` and `.` read as spaces, in a configuration `hqc` that
stems Russian and English without stop words; the query parser adds `word*` (also the
shorter stems the stemmer gives the prefix with common English endings), and `-`
excludes only before a phrase or four letters, so a command pasted with its flags finds its
call; hits come by `ts_rank`, paged by an offset; fuzzy widens the query with similar words
from a vocabulary of the index instead of a trigram search over all text. The
`hqc_search_history` description says so.

### 0.94 → 0.95 (a session follows its transcript)
§9: a transcript's folder is the first recorded `cwd` that encodes to the name of its folder
under `projects/`, else the first recorded `cwd` as before. A chat moved to another project by
copying its transcript there and continued in that project's folder starts with the old folder's
records, so the mirror sent the copy's records with the old workspace, and the hub, which finds
the session by `runtime_session_id` alone, showed the new dialog in the session of the old project
(53 sessions on one laptop). §9.2: a row whose `cwd` is not its folder is searched for one that is
(the whole file once per worker process, then what is appended); the hub moves a session whose
new record comes from another workspace of its owner into it and its project, unless the record
is older than the session's newest non-system event, without a line in the feed.

### 0.88 → 0.89 (the list at once on a reload)
§6: `GET /api/sessions` gives every session of every project of the user in one list; the
client fetches its list with it and `GET /api/projects` instead of one request per project
(with 37 projects, 37 requests over HTTP/1.1, six at a time, the last answered after 1.5 s).
§6 client layout: the list is kept on this device (`hqc.list`) and shown at once
on a reload until the hub answers; before any list the head says neither "No sessions yet" nor
"offline", and "offline" (there and in the chat's header) waits for the end of the socket's
first connect.

### 0.87 → 0.88 (light themes, the app theme in Settings)
§3 User: `theme` gets the light counterparts of the three dark bases, `warm_light`,
`graphite_light`, `contrast_light` (migration 0036 widens the check). §6: the app theme moved
from the "Project appearance" dialog to the Settings page, a row of dark and a row of light
tiles applied at once; on light themes the status colors are darker and the project's color is
darkened where it is text; until `/api/me` answers the client starts with the theme last
applied on the device instead of `warm`; on a light theme the client switches the iPhone
status bar style to `default` (dark text) instead of `black-translucent` **[verify]**.

### 0.86 → 0.87 (at most 256 outbox entries un-acked)
§5.2: the worker keeps at most 256 outbox entries un-acked (`MAX_IN_FLIGHT` in the worker's
`connection.rs`); with that many in flight it reads the hub's frames before sending the next
one, and drops the connection when no answer frees a slot within its dead time; an entry the
hub leaves unanswered holds its slot only until then. A worker that sent 40 000 mirrored
records at once without reading the acks filled the hub's bounded send queue (§20) and was
dropped, and both sides stayed blocked writing to a socket neither read, so the worker never
reconnected.

### 0.85 → 0.86 (an imported session comes in read)
§9.1: the hub moves the owner's read cursor over the `session_created` of an imported session
and over every mirrored record older than the session on the hub, so adding a folder with a long
history of Claude Code sessions does not mark them all unread; what is written after the import
is unread as before.

### 0.84 → 0.85 (the mirror's daily limit is a hub setting)
§9.1, §20: `HQC_MIRROR_SESSIONS_PER_DAY` (a whole number, default 200) sets how many sessions the
mirror may create per worker in 24 h, so an owner who adds the folders of many projects at once
can take their history in one go instead of 200 sessions a day.

### 0.83 → 0.84 (opt-in `SendMessage` for an agent's own subagents)
§8.1: `allow_subagent_messages = true` in `worker.toml` (default false; `allow_native_messaging`
wins over it) keeps `SendMessage`, which is also how an agent continues its own subagent: only
`ListAgents` goes to `--disallowedTools` (a restricted session keeps both disallowed), and
`--settings` carries `"crossSessionInbound": "refuse"`, so messages other local sessions send to
an hqc session are refused (checked live on 2.1.312). The agent can then hand work to another
local session that accepts such messages, past the hub (checked live; §2.1). Without the option
the command line is as before.

### 0.82 → 0.83 (project settings per environment)
§14: `[environments.<name>] project_settings = true` (default false) makes the Claude Code
sessions of that environment's folders read the project setting source: §8.1, the adapter passes
`--setting-sources user,project,local` instead of `user,local`, so the repository's `CLAUDE.md`
and `.claude/settings.json` apply: its hooks (outside the worker's `deny` and approval
requests), permission rules, `env`, and `defaultMode` as far as the explicit `--permission-mode`
does not override it; a repository hqc clones into such a folder is trusted the same way. §2.1:
what that means for a hub. A restricted session still gets `--restricted`. The field stays on
the worker, like `ssh_keys`: not in `hello`; `hqc-worker env` shows it. ACP agents ignore it.
§3 Workspace, §5.3 `permissions.inspect`: in such an environment the rules screen lists the
folder's `.claude/settings.json` as a source of its own, between `~/.claude/settings.json` and
`.claude/settings.local.json`, and its check counts that file's allow and deny rules.

### 0.81 → 0.82 (a fork of a fork)
§6 fork: a copied event keeps its origin (migration 0035: `events.origin_id`,
`origin_runtime`), and the cut goes to the transcript the newest cut event was first written
in, by the ids the runtime gave it. Before, a fork of a fork failed: the copies' ids are in no
transcript, and a fork that never ran has none of its own. Copies made before 0.82 have no
origin; a fork in that copied history still fails.

### 0.80 → 0.81 (usage limits in the "More" menu)
§5.4: the worker frame `worker.limits {five_hour?, seven_day?}`, from Claude Code's
`rate_limit_event` (§8.1). §6: `GET /api/workers` has the latest as `limits`; the chat's "More"
menu shows the session's worker account's 5-hour and 7-day windows under the model field.

### 0.79 → 0.80 (fork a session before a message)
§6: `POST /api/sessions/{id}/fork {event_id}` makes a new session in the same folder whose
conversation is the source's before that message of the user; its feed gets the source's
history before it under new ids and `system forked_from`; the client opens it with the message
in its input. §5.3: `session.start.fork {runtime_session_id, at_event_ids}`. §8.1: the Claude
Code adapter forks with `--fork-session --resume-session-at`, the copied records counted as its
own. A message of the user in the feed has "Fork from here". Migration 0034 adds
`sessions.fork`.

### 0.78 → 0.79 (a session with running subagents is active)
§7: `hqc_list_sessions` names the subagents running in a session (`subagents`, when any), so
an `idle` session whose background subagents still run no longer looks like it does nothing.
§6 client layout: a worker's row in the sidebar counts such a session as active.

### 0.77 → 0.78 (drafts)
§6 client layout: what is typed into a session and not sent stays its draft on this device;
the input shows it again when the session opens, and the session's row in the list says
"Draft" while another session is open. Sending or clearing the input drops it.

### 0.76 → 0.77 (the model: Fable, a typed `/model`, the header)
§6: a message typed as `/model <name>` stores the session's model as `PATCH {model}` does
(`default`: `null`) and sends the worker `session.model`, so the next `session.start` no longer
brings the old model back; the client fetches the session object after sending it. The menu's
model field offers Fable with Opus, Sonnet and Haiku. The header names the stored model while
the last turn used another one. §8.1: the transcript record of a `set_model` switch is the
adapter's own, not a prompt the mirror shows as the user's.

### 0.75 → 0.76 (an agent moves its own session: `hqc_move`)
§7, §7.2: the tool `hqc_move(path, project?)` moves the calling session, run by hqc, by the core
of `POST /api/sessions/{id}/move` and answers `{address, project, path}`; its errors map the
endpoint's codes (`invalid_args`, with `workspace_conflict` in the text for an overlap;
`not_found`; `unavailable`; `limit_exceeded` for a chain at its limit, before anything moves).
After the security review: it looks at the folder with `fs.stat` without `create` and moves at
once only into a ready workspace of the owner at exactly that path in the environment of the
session's folder now, both as the worker names them in `fs.stat` (§5.3: the new `environment`
of its answer, by its bindings as it resolves them then, not the hub's copy from `hello`; an
answer without it, from an older worker, asks); a missing folder, one that is no workspace of the owner and another
environment wait for the owner's answer, a card in the session's own chat (not the worker's
flow of §10.2) that names the project the session lands in and, for a folder that is no
workspace yet, that it becomes a mirrored workspace whose Claude Code sessions are imported,
and the call is deferred as `hqc_ask` is; `once` moves with `create: true`, `deny` and a
cancellation are `unavailable`; a turn whose chain is capped to `ask` moves nothing, whoever
capped it (the session itself too) and whatever the session's own mode; a person's input has no
cap of its own (one that carries buffered messages of a session in `ask` has theirs). A session
has at most one move request open: another call meanwhile is `unavailable`. A path or a
canonical path with a control character or a bidirectional control, and a missing
folder's path with `.` or `..`, is `invalid_args`. A move the endpoint would refuse anyway (its
`409`s, an unknown project, an overlap of the path, a workspace at the path that is not ready
or of another project than `project`) is the call's error before any card. The move lands only
in the canonical path it was decided on (the one on the card): a different canonical path from
the move's own `fs.stat`, a folder swapped for a symlink meanwhile, is `invalid_args` and
records nothing. `mcp.cancel` of a waiting call closes its request with `approval.cancel
{stopped}`; so does the runtime's tool timeout (`ask_timeout_s` plus a minute), the most the
call waits for the owner. §3 PendingRequest, §6 `GET /api/requests`, §10.1, §11: the request kind `move`,
answered by the session owner only, without `message`, never by `auto_approve`, counted as any
open request; the client's card says "The agent asks to move this session"; an allowed move
whose result the previous hub process did not store (it stopped meanwhile) is found on start
and carried out again once the session's worker sends `hello`; a result the hub fails to store
is stored again after a pause growing from 1 s to 60 s, the move not carried out again. It then
queues the owner's input "You moved to …; continue the task." as a run of an agent's
`hqc_schedule` (§16): a `user.message` in the call's chain moved on, built after the move (the
cap of a session in `ask` is named by its new address), so the turn runs in the session's own
mode; once per `request_id`, a repeat of a move made at once answered from that input before
anything the turn's chain decides. `INSTRUCTIONS` name it: only when the user asks, and
a folder other than one of the user's workspaces in the same environment waits for the user. A
move from the client still queues nothing (§6). §4: that input is a `user.message` the hub
writes. §17.2, §5.3: while a move waits, the worker writes no input into the running turn, so
the input after `hqc_move` starts the first turn in the new folder.

### 0.74 → 0.75 (a session moves to another folder and project)
§3 Session, §6: `POST /api/sessions/{id}/move {path, project?, create?}` continues a Claude Code
session in another folder of its worker, as the same conversation: an exact workspace of the
owner with its project, or a new mirrored workspace of `project` or of a new project named after
the folder; the slug is kept when free, otherwise `-2`…; the old project's saved order drops the
session (a move within the project keeps its place); `system session_moved` (§4); `409
archived`; `409 unavailable` for a session driven from VS Code, held by a live process or with
subagents still running (a foreground one too), for a worker that is not the session owner's
and for a worker without
`capabilities.claude_code.moves` (§5.4, reported only when Claude Code's config folder exists),
these refusals checked again on the session read anew after `fs.stat`; the folder checks and
codes of `POST /api/workspaces`; `409 workspace_not_ready` for an exact workspace that is not
`ready`; with `create` the overlap and `ready` checks on the requested path before the worker
makes the folder, so a `409` leaves none. Nothing is queued. A repeat writes nothing, but sends the workspace's
`mirror.set` and the start of a running agent again. The client: "Move to project…" in
the chat's "More" menu; a summary with another `project_id` moves the session in the lists.
§5.3, §10.2: a `session.start` for a running agent with another folder is a move, applied
between turns, a turn the runtime started by itself waited for too, once no subagent runs (not
at the end of the last one: the end of the runtime's turn on its result moves the agent, or,
with no such turn, the next input), with
the owner's consent to a new folder of a foreign session; while it waits no input starts a turn
in the old folder or is written into the running one (§17.2); the agent's own background commands
stop with it, and its first input in the new
folder that the runtime takes and is no slash command says how many. §8.1, §9.2, §2.1: before every start of a Claude Code agent the worker moves its
transcript by uuid to the place of the working directory, within `allowed_roots`, never over
a file and not while a live process outside the worker holds it (Claude Code's registry read at
that moment, §9.3), the mirror's row first and removed again when the file does not get there;
the folder of a transcript is judged through its deepest existing ancestor when it is gone; this
is the one place where hqc moves a Claude Code transcript **[decision]**. §5.2: a `session.state` goes after the outbox entries written
before it, also when the worker stops (a stop right after a turn could overtake its final answer
and lose the turn).

### 0.73 → 0.74 (favorites, a session's model, quoting)
§3 Favorite, §6: a user stars sessions and projects; favorites are personal, in two tables
(migration `0033_favorites`), `PUT/DELETE /api/sessions/{id}/favorite` and
`/api/projects/{id}/favorite`, `favorite_at?` in the session object, `session.summary` and the
project object; the client shows a Favorites block above the session groups and starred
projects first on the rail. §3 Agent, §5.3, §6, §8.1: the model of a session, set on
creation before, is now also changed with `PATCH`; the frame `session.model` (a running Claude Code agent
switches with `set_model`, also mid-turn). Client only: a Quote button under a selection in
the feed puts the selected text into the message field as `> ` lines.

### 0.72 → 0.73 (a dot on the tab's icon)
§6 client layout: the tab's icon (the mark) gets a small amber dot in its lower right corner, with a clear gap cut around it, while a session of any project
waits for the user and a light one while nothing waits but one has unread events, as the
project tiles do.

### 0.71 → 0.72 (a long chat opens at once, the answer being written in full)
§6: the chat loads the newest 100 events first, shows them and subscribes, then loads the older
history behind them; it renders the last 60 blocks of the feed and more as the user scrolls up.
Before, it loaded the whole history from the start, page by page, before it showed anything.
§4, §6: the hub keeps the text of the answer being written and sends it on `subscribe`, so a
chat opened mid-answer shows all of it, not only what came after it opened. §15: a search hit
in the older history renders the feed from its block.

### 0.70 → 0.71 (after the security pass of #4 and #5)
§17.2: `session.stop` interrupts a turn the CLI began by itself; an input the CLI cancels after
a Stop fails instead of running again; the slash check skips leading whitespace; inputs with
`peer_prefix` may join (the §7.1 cap still holds); the hub honours `joined` only for a person's
`user.message` without `schedule_id`. §8.1: "No" without text interrupts the turn only for a
request of the main agent or of a foreground subagent; for a background subagent's request,
which may outlive its turn, it only denies the call.

### 0.69 → 0.70 (the agent at work: subagents and a message during a turn)
§17.2, §5.2, §8.1: a person's message sent during a Claude Code turn goes into that turn, as in
the VS Code tab, instead of waiting for its final answer. This changes the **[decision]** of
§17.2 for Claude Code: input with a `peer`, a run of a schedule, a slash command, input or a
turn capped to `ask`, any input during the turn of an input with a `peer`, and input while an
`approval` or `clarify` is open still wait. The worker answers `ack {event_id, joined: true}`
for an input that goes into the turn, which marks it `delivered` and opens no turn in the hub
(§17.1). §5.3: `session.input.schedule_id?`, which the hub copies from the `user.message`, so
the worker tells a run of a schedule from a person's input. The adapter writes every user
message with a fresh `uuid` and reads `command_lifecycle`, so it knows at `result` whether an
input not echoed was taken, carried into the turn the CLI then runs by itself (an input sent
during the final answer, before an interrupt, or during a turn the CLI began after a background
task) or not taken; before, every such input counted as taken (a local slash command), and the
turn the CLI then ran went untracked. A carried input `cancelled` after that `result` ends the
turn it was carried into (§8.1). §5.4, §6, §8.1:
`session.state.subagents`, `subagents` in the session object and `session.summary`: the
subagents the agent runs now, from `task_started` (`local_agent`) to its end, a foreground one
at the latest to the `result` of the main turn; the client shows "N agents". Migration
`0032_session_subagents`. A subagent also counts while the CLI's `background_tasks_changed`
list holds it or a task of it (a task of it only until it hands its report over): 2.1.312
under `claude-vscode` moves a subagent's long Bash call to the background and meanwhile
reports the subagent ended (§8.1). A `task_notification` of such a subagent without its report
(`handback`) is no end: it gives no line and cancels nothing until the report comes, the
subagent stops counting or the process exits. A task gives one line, instead of one per
notification.
§17.3: the worker's
`idle_timeout` no longer stops an agent whose subagents still run or for which the CLI lists
background tasks; a reconnect and a holder change of any transcript no longer restart its idle
time, so an agent on a machine where Claude Code runs outside hqc is stopped too.

### 0.68 → 0.69 (the feed of a Claude Code session as the VS Code tab shows it)
§8.1: the adapter passes `--thinking-display summarized`, as the VS Code extension does (opus 5.5
gave thinking without text, so no `agent.thinking`), without a version check: a CLI that drops
the flag fails every start. A deny without text is the tab's "No": its
wording with `interrupt: true`, the CLI ends the turn at once (`result error_during_execution`,
`status idle`); before, the model went on after "The user denied this tool call.". The hub's
cancellation of an approval request (stop, archiving, §10.1) is answered with that deny
too, so it ends the turn as well; a cancelled `clarify` is still denied without `interrupt`, and
the turn goes on without answers (§10.1). §10.1: a request of the main agent or of a foreground
subagent still open when the turn ends (the other of two parallel calls after "No") gets
`approval.cancel`/`clarify.cancel {stopped}` from the adapter, before dropped silently; a
background subagent's stays open, since the CLI still waits for it, until the subagent's own
`task_notification`, which cancels its requests still open; the hub stores a worker's cancel only if it closes an open request, in the same
transaction.
§4, §7.1: an `hqc_ask` whose turn on the question ends with `status idle` and no final message
(after "No", `POST /stop`, a turn without text) fails at once with `unavailable`, "the turn of
<session> on the question ended without an answer", before it waited for its timeout. The turn
is the one named by the new `status.in_reply_to?`, which the Claude Code adapter sets on the
`idle` that ends a turn (the next input's `ack` may come before that status); a status without
it fails nothing. §8.1: `in_reply_to` names the input sent last only once its echo has come, so a
turn the CLI starts by itself names no input once an input is written during it, until that
input is echoed. §4, §8.1: no `usage` for a `result` with `num_turns: 0`, no tokens and no
cost (a local slash command, the empty second `result` after a turn the CLI starts by itself),
before shown as `0 → 0 tokens`; `/compact` keeps its `usage`. §8.1, §10.1: a deny with text uses the tab's wording ("The user provided the following reason for the
rejection:", two spaces, the text) with `interrupt: false`. §4, §8.1: `clarify.request`
questions carry `header?` and `option_descriptions?` from `AskUserQuestion`. §4, §8.1, §9.1,
§9.2: new `system` codes from the worker: `task_notification` (stream-json `system
task_notification` with its `summary`, "Background task" without one, only for a background task
of the agent itself; in the mirror the transcript's `user` record with `origin.kind:
task-notification`, which before became a `user.message` with the raw `<task-notification>`
text, 5 s late, in sessions hqc runs too) and `context_compacted` (`system compact_boundary`).
§4, §8.1, §9.1: `agent.message` carries `runtime?`, `true` for an `assistant` text with `model:
<synthetic>` (a reply the CLI wrote itself: an API error, "Credit balance is too low"); it stays
the turn's final message when it ends the turn, and the client shows it as a service line
instead of the agent's text. No migration, no frame change; old clients show the new codes as
text and ignore `runtime` and `status.in_reply_to` (§4). Client: the clarify form shows the header, the option descriptions and an "Other"
field (§4); the permission card shows an edit's diff or a write's content (§10.1); the user's own
text is shown as typed, not as Markdown (§4); a tool group's header shows its last call; MCP
tools read as `<tool> (<server>)`, paths under the session's folder are relative to it.

### 0.67 → 0.68 (OIDC sign-in behind a reverse proxy)
§6, §20: a third way to authenticate people besides Cloudflare Access and dev mode:
`HQC_OIDC_ISSUER`, `HQC_OIDC_AUD`, `HQC_OIDC_JWKS_URL`, `HQC_OIDC_ALLOWED_EMAILS`, the
provider's ID token from `Authorization: Bearer`, put there by a proxy such as oauth2-proxy with
Keycloak, with `email_verified: true`, when it has a `typ`, `typ: ID`, with an array `aud`,
`azp` equal to the client id, and an email of the allow-list; the issuer and the JWKS URL are
`https://` (`http://` only to loopback), and `HQC_CLIENT_IP_HEADER` is required. §3 User, §6:
the first OIDC sign-in binds the user to the token's `sub` (migration `0031_oidc_subject`,
`users.oidc_sub`); the email of a user bound to another `sub` is refused. §6: `nbf` is checked,
in Access mode too; otherwise Access and dev mode are unchanged. §20: `HQC_CLIENT_IP_HEADER`
names the header per-address limits take the address from instead of `CF-Connecting-IP`; with
it `CF-IPCountry` is not read either (§5.1); in OIDC mode HSTS is the reverse proxy's. RUNBOOK
§1.2, §1.9.

### 0.66 → 0.67 (unread projects)
§6 client layout: a project tile with no session waiting shows a dot when one of its sessions
(not archived, not a service session) has unread events; a phone's chip too.

### 0.65 → 0.66 (MCP server instructions)
§7.2: the hqc MCP server's `initialize` gives `instructions` that name every tool by its use and
send files made for the user through `hqc_attach`; before, it said only that the tools act with
the owner's rights.

### 0.64 → 0.65 (iteration 4 closed)
§19: iteration 4 is closed; its criterion was checked by hand on the production hub. No
behaviour change.

### 0.63 → 0.64 (small UI changes, archiving with a workspace, project order, made folders)
§6, client: a message between sessions names the other session by its title, the address in the
tooltip; a tool group's mark is its last call's; the project is renamed in the appearance dialog
and deleted next to its button (the "Project" section is gone); the deletion dialog says what
"Delete folder" does with each thing it lists (a worktree's unpushed commits stay on its branch,
a clone's go with the folder); a desktop project opens the session last open in it on this
device. §6, §13.2: `DELETE /api/workspaces/{id}?archive_sessions=true` archives the workspace's
sessions first, and the client's dialog lists them (and the worktrees to delete first). §6:
`PUT /api/me/project_order` and `GET /api/projects` in that order, the rail's "Reorder
projects"; migration `0030_project_order` (`users.project_order`). §5.3, §6, §13.1: `fs.stat`
`create` and `POST /api/workspaces` `create`: the worker makes a missing folder inside
`allowed_roots`, offered by the add form after `path_not_found`; it is an ordinary `dir`.

### 0.62 → 0.63 (T53, warn when the artifact store is not a volume)
§12: at start the hub checks that the store is writable and, in a container, not on the root's
device, and logs a warning otherwise (production ran without the `hqc-artifacts` volume, and a
deploy lost the files of every attachment). No migration, no frame change.

### 0.61 → 0.62 (deny matching after shell words, unbound environments, multi-line check)
§8.1: a `Bash` deny rule skips the shell's words before a command in each piece (`do`, `then`,
`else`, `elif`, `if`, `while`, `until`, `!`, `{`, `time`), so `for …; do ssh $h; done` is denied
by the adapter and reported as denied by the rules screen's check, as the CLI already denied it.
§14: an environment no binding names gives a warning at start and a mark in `hqc-worker env`.
§6: "Check a command" is a multi-line field (a pasted script lost its line breaks and was
checked as one line). No migration, no frame change.

### 0.60 → 0.61 (T47, ephemeral worktrees for spawned sessions)
§7: `hqc_spawn` gets `isolation: "worktree"` and `branch?`: an `ephemeral` worktree of the
workspace the spawn would use (T46's origin rules), on a new branch, `hqc/session-N` with the new
session's slug by default; the call waits for the worker, then creates the session there and
returns `{address, worktree: {path, branch}}`; a failed worktree fails the call with the worker's
reason and leaves no session and no record. The tool's description says what isolation does and
that the helper's commits stay on its branch for the owner to merge. §5.2: the result of a spawn
with isolation comes under the call frame's `id` once the worktree is made, while the hub keeps
reading the connection; the worktree is keyed by `once_id` like the session. §3 Workspace:
`ephemeral` in effect, `marked_for_deletion` computed by the hub (ephemeral, ready, no
unarchived session), never deleted automatically. §6: the workspace object's
`marked_for_deletion` (`api.workspace.ephemeral.json`), "spawned", "for deletion" and "Delete" on
the row in the client. §13.1, §13.2, §17.4: the ephemeral worktree, its mark and its deletion
through the dialog. §19: ephemeral done. §20: worktrees being made count in the spawn limits. No
migration (`ephemeral` is in `0001`), no new frame, no worker change.

### 0.60 → 0.60 (T46, worktree workspaces)
§3 Workspace: `origin_workspace_id` and `branch` in effect (hub migration `0029`: the column
`origin_workspace_id` keyed to the origin, `new_branch`, T45's `clone_branch` renamed to
`branch`); a worktree's origin is a ready workspace of the same project and worker that is not a
worktree itself, and is not deleted while worktree records of it exist. §5.3: `workspace.create
{kind: worktree, path, origin, branch, new_branch?}` — the worker takes only an origin it knows,
inside `allowed_roots`, at the top of a git repository, checks the branch (`check_branch`, `git
check-ref-format`, new and absent, or existing and not checked out in another worktree, or on
exactly one remote) and the new path as for a clone, records the worktree with its origin and
runs `git worktree add` in the origin with the environment of the new path; the hub sends the
origin's `workspace.rules` first. `workspace.delete` gets `force`; a worktree goes by `git
worktree remove [--force]` in its origin and `git worktree prune`, its branch stays.
`workspace.inspect` of a linked worktree lists only its own branch. §5.4: `workspace.created`
also ends a worktree. §6: `POST /api/workspaces {kind: worktree, origin_workspace_id, branch,
new_branch?, path?}` (`400 invalid_origin`; the default path `<origin-parent>/<origin-name>-<branch>`),
`DELETE ...&force=true`, `409 workspace_in_use` for an origin with worktrees, the workspace
object's `origin_workspace_id` and `branch`, "New worktree", the worktrees under their origin
and the dialog for a worktree in the client. §2.1: the hub cannot make a worktree of a folder
the worker does not know nor remove one but through git in the recorded origin. §13.1, §13.2:
`worktree` implemented. §14: `clone_timeout` covers `git worktree add`, `created_workspaces.json`
keeps a worktree's origin, and a worktree shares the origin's `.git` across environments. §19:
worktree done. §20: the worktree limits.

### 0.58 → 0.59 (`.hqc/` excluded on every input)
§12: the worker adds `.hqc/` to `info/exclude` on every input of a workspace that has the folder,
not only when a file comes (2026-11-02: a session got an image, then its agent ran `git init`, and
`git add -A` took the inbox in). No protocol change.

### 0.57 → 0.58 (T45, clone workspaces and deleting what hqc created)
§3 Workspace: `state` (`creating` | `ready` | `failed`) and `error` (hub migration `0028`); only
a `ready` workspace takes sessions (`409 workspace_not_ready`), `mirror.set` and
`workspace.rules`. §5.3: `workspace.create {request_id = the workspace id, kind: clone, path,
url, branch?}` → `ack`, the worker checks the new path itself (parent canonical inside
`allowed_roots`, not existing, no overlap with a workspace it knows), creates and records it,
clones without a shell, prompts or other transports in the environment of the path with
`clone_timeout`, and answers a known request with its outcome; the hub repeats it after `hello`
while `creating` and fails it on a new `boot_id`; `workspace.inspect` (dirty paths, branches with
commits on no remote); `workspace.delete` only of a canonical path the worker created — the old
stubs with `origin_url`, `requested_by_owner` and `result {ok, path}` are gone. §5.4:
`workspace.created {request_id, canonical | error}` → `ack`. §6: `POST /api/workspaces {kind:
clone, url, branch?}` (`path_exists`, `invalid_url`, `invalid_branch`), `GET
/api/workspaces/{id}/inspect`, `DELETE ...?remove_folder=true`, `409 workspace_creating`, the
workspace object's `state` and `error`, the clone form, the row and the deletion dialog in the
client. §2.1: the hub cannot clone outside `allowed_roots` nor delete a folder the worker did not
create. §13.1, §13.2: `clone` implemented. §14: `clone_timeout`, `created_workspaces.json`, one
process environment for the agent and `git`, and a clone in an environment with `ssh_keys` gets an
ssh-agent of its own like a session (T44). §20: the clone and inspection limits.

### 0.56 → 0.57 (T44, ssh keys of an environment)
§14: `ssh_keys` of `[environments.<name>]` are read and in effect: names of keys in the store
`$HQC_HOME/keys/` (0700, files 0600), which `hqc-worker env add-key <name> <file>` creates and
fills without overwriting; wider modes, links, another owner or a name not in the store stop the
worker at start. Every agent process with keys gets its own `ssh-agent` on a socket in
`$HQC_HOME/agents/` (0700) with the keys loaded by `ssh-add`; it stops with the process, and the
sockets a dead worker left are emptied and removed at the next start. `hqc-worker env` and
`env which` list key names; `SSH_AUTH_SOCK` in `env` is a config error. The limitation keeps
`~/.ssh` readable by the agent without a separate OS user. §8.1: `SSH_AUTH_SOCK` leaves the child
allowlist; only the session's own agent socket is passed. §2.1: the hub cannot hand a session a
key. §20: the store and the agents folder.

### 0.55 → 0.56 (T43, environments and bindings on the worker)
§14: `[environments.<name>]` (`env`, `allow_local_mcp`, `permissions {unblock, allow, deny}`) and
`[[env_bindings]]` are read and in effect; the effective rules of a session are the worker's deny
minus `unblock` plus the environment's deny, then both allow lists, then hqc's folder rules, then
the mode; `unblock` of a rule not in the worker's deny, a binding to an unknown environment, a
path bound twice, a defined `default` and `HQC_*` in `env` are config errors; `hqc-worker env`
and `env which <path>` (values as `***`). §2.1, §5.3: `session.start` loses `environment`; the
worker resolves it from the canonical workspace path on every start (longest binding by path
components, symlinks resolved). §3 Worker, §5.4: `hello.environments` are objects with name,
`allow_local_mcp` and rules (a plain name from an older worker still reads), `env_bindings` are
`{path, environment}`; never `env` values. §3 Workspace, §6: the hub sets each workspace's
`environment` from the bindings at creation and on every `hello` (no migration: the column was
there); the rules screen shows it, `permissions.inspect` lists the environment's rules as
`worker.toml [environments.<name>]` and checks with the effective rules. §8.1: `--settings` and
the adapter's policy carry the environment's rules, `allow_local_mcp` drops
`--strict-mcp-config`, `env` goes on top of the child allowlist. §20: `env` values stay on the
worker.

### 0.54 → 0.55 (T52, a send wakes its target)
§3 Session: `wake_on_send` is true by default and on every existing session (hub migration
`0027`). §7: `hqc_send` is `queued` by default; its description says it starts a turn there and
points to `hqc_ask` for answers. §7.1: the send's header and the buffered block say the message
is from another session of the owner, to be done as the owner's request within the session's
permissions, answered only when needed; the send buffer is left for sessions with
`wake_on_send` off and for the sends of an ask closed without a reply.

### 0.53 → 0.54 (T51, messages between sessions in the feed)
§6: the client's blocks between sessions (Question from / Reply to in the answering session,
Sent to / Reply from in the asking one, a send that went with a reply inside it, the other
session as a link); the events API and `/api/ws` add `reply_delivered_at` to the final message
that answered an ask, and `/api/ws` sends that event again when the field becomes known. §7:
the results of `hqc_send` and `hqc_ask` have `message_seq`, a file of an ask's reply has
`artifact_id`. §7.1: the asking worker's `ack` of the reply is its delivery time (kept on the
ask); a buffered send gets `delivered_at` with the ack of the input that carried it. §4 follows.
No migration. Notes in `docs/notes/t51-questions.md`.

### 0.52 → 0.53 (T50, archive at hand, archive mode and the custom order)
§3 Session: `archived_at`. §6: `archived_at?` in the session object, `GET/PUT
/api/projects/{id}/session_order` (the list's mode and custom order per user and project; the
mode left `localStorage`), the session list's row actions, the delayed archive with Undo,
Reorder, archive mode and the archived session view; the session header lost archive, restore
and delete. §17.4: `archived_at` set and cleared. Hub migration `0026`.

### 0.51 → 0.52 (T48, permission rules in hqc)
§3 Workspace: hqc's allow rules per folder (syntax, stored on the hub, applied by the worker,
never in `--settings`). §4: `approval.request.rule_suggestions`. §5.3: `session.start.allow_rules`,
the new frames `workspace.rules` (on change and after every `hello`) and `permissions.inspect`
(the three sources and a check by the worker's matcher). §5.4: `result` of `permissions.inspect`.
§6: `hqc_rules` in the workspace object, `GET/POST /api/workspaces/{id}/rules`, `DELETE
.../rules/{rule_id}`, `GET /api/workspaces/{id}/permissions`, the rules screen. §8.1: the matcher,
where it gives up, `git -C`, the suggestions. §10.1: "Allow in this folder" (save, then allow).
§2.1, §14: the rules are a standing answer below `deny`. §20: the limits. Hub migration `0025`.

### 0.50 → 0.51 (T49, the theme and project appearance)
§3 User: `theme` (`warm` | `graphite` | `contrast`). §3 Project: `color` (8 names, by a hash of
the id until set) and `icon` (emoji, letters or an uploaded image), the personal project
included. §6: both in `GET/PATCH /api/me` and the project object, `PUT/GET
/api/projects/{id}/icon`, `422 invalid_icon`, the client layout of round 2 (project bar,
sidebar header and rows, widths, the project settings page with the workspaces moved into it,
the Appearance dialog). §12: the limits of an icon image and its headers. Hub migration `0024`.

### 0.49 → 0.50 (iteration 4 planned)
§14: an environment's free-form `settings` is replaced by `permissions {unblock, allow, deny}`
(per-folder rules, from `docs/BACKLOG.md`); `network` and `roots` are dropped until needed
(backlog); ssh keys of environments live in 0600 files under `$HQC_HOME/keys/`, not in
Keychain (§18 follows). §19: the session template leaves iteration 4 for the backlog; tasks in
`docs/ITERATION-4.md`.

### 0.48 → 0.49 (iterations 4 and 5 swapped)
§19: workspaces, environments and bindings are now iteration 4, the ACP adapter, the bare and
local models and the personal session iteration 5; `acp_agents` on the worker moves with the
ACP adapter. References in the spec follow.

### 0.47 → 0.48 (iteration 3 closed)
§19: iteration 3 is closed; its criterion was checked by hand on the production hub.

### 0.46 → 0.47 (T42, replies that come back as a send)
§7.1: an `hqc_send` from the target of an open ask to the asker, made from the turn on the
question, goes with the reply (its text after the final message behind a separator, its files
after the turn's attachments) instead of the asker's send buffer; an ask that closes without a
reply buffers it after all; the ask's header tells the target how to return files. §7: the
`with_reply` delivery of `hqc_send` and the tool descriptions. §3 PendingRequest:
`payload.joined`. §4: such a `peer.message` has no `delivery`. §8.1: [verify] a tool loaded with
`ToolSearch` keeps its recorded schema across `--resume` (2.1.308); nothing hqc changes. No
migration. Notes in `docs/notes/t42-questions.md`.

### 0.45 → 0.46 (T41, files between sessions)
§7: `hqc_send` and `hqc_ask` take `files` (paths or artifact ids); the sender's worker uploads
paths by the rules of `hqc_attach` and the call carries `artifact_ids`; the result of `hqc_ask`
has `files` with local paths. §7.1: a reply carries the files the target attached during its
answering turn; buffered sends carry their files. §4: `peer.message.artifact_ids`. §5.2: the
asking worker downloads a reply's files before it answers the call. §5.3: `peer_prefix` entries
have `event_id` and `artifact_ids`; `session.cleanup` covers their folders and the replies'.
§12: the link in `artifact_sessions` lets the target's and the asker's workers download; the
feed shows a peer message's files. §17.4, §20, §2.1 accordingly. No migration. Notes in
`docs/notes/t41-questions.md`.

### 0.44 → 0.45 (a switch to `ask` caps at once)
§7.1, §16: the mode of a sender or a schedule's creator for the cap is `ask` as soon as the user
asks for `ask`, not only once its worker reports it; otherwise the reported mode, as before.

### 0.43 → 0.44 (T40, the cap for buffered sends and old schedules)
§7.1: an input's turn takes the lowest cap of the buffered sends it carries in `peer_prefix`
together with its own, so text from a session in `ask` never runs in `auto`, a person's own
input included; the cap is kept with the input (`input_caps`), named in the header and carried
on by what the turn sends. §16: each run of an agent's schedule is capped by the lower of the
stored cap and its creator's effective mode at run time; a deleted creator (or the unknown one
of a schedule from before 0.44) counts as `ask`. §3 ScheduledMessage: `created_by`. §5.3, §10.1,
§17.2, §2.1: the rule where `session.input`, requests and the input queue meet it. Migration
`0023`. The worker is unchanged. Answers T39's open questions 1 and 2
(`docs/notes/t39-questions.md`).

### 0.42 → 0.43 (T39, a message cannot raise permissions)
§7.1: the chain carries a permission cap (`permission_cap`, `capped_by`): a turn woken by a
peer message, the prompt of `hqc_spawn` or a run of an agent's schedule runs in `auto` only if
the target and every session up the chain run in `auto`; the cap is the lower of the sender's
reported mode and the cap of the sender's turn. §3: the cap in `Event.chain`, the session's
`permission_capped_by`. §5.3: `session.input.permission_cap?` and how the worker applies it
(down only, before the runtime gets the input, waiting for the runtime's confirmation or
restarting in `ask`; the session's own mode after the turn). §5.4:
`session.state.capped_input?`. §6: `permission_capped_by?` in the session object and the
summary, "Ask (asked by …)" in the header. §7: a spawned session's first turn is capped too.
§8.1: every confirmation of the adapter's switch is reported. §10.1, §16, §17.2, §2.1: the
rule where requests, schedules and the input queue meet it. Migration `0022`. Notes:
`docs/notes/t39-questions.md`; T38's open question 1 is answered by it.

### 0.41 → 0.42 (T38, a permission mode per session)
§3 Session: `permission_mode` (what hqc asks for, null — the worker's default) and
`effective_permission_mode` (what the worker reports). §5.3: `session.start.permission_mode?`
and the frame `session.permission_mode`. §5.4: `session.state.permission_mode?`. §6: the mode
on creation, `PATCH permission_mode` (`auto` by the worker owner only), both fields in the
session object and `session.summary`, the header mark, the menu switch and the form field
with a one-line confirmation for Auto. §7: `hqc_spawn` starts sessions in `ask` and has no
argument for the mode; a peer message does not change it. §8.1: switching a running Claude
Code process with `control_request set_permission_mode`, verified on 2.1.308 (no restart).
§10.1: what Auto means for requests next to `auto_approve`. §14: `[permissions] allow_auto`;
`mode` is the default of sessions that do not ask. §2.1: the hub cannot run Auto where
`allow_auto = false`. Migration `0021`. Notes: `docs/notes/t38-questions.md`.

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
