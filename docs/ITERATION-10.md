# Iteration 10: tasks

Goal (SPEC §19.10): the owner runs the newest Google and OpenAI models in their own agent
harnesses from hqc as easily as Claude Code, paid by the owner's Google AI and ChatGPT
subscriptions rather than API keys. Two adapters on the worker, as comparable tools do:

1. a **generic ACP adapter** (SPEC §8.2) that knows no agent by name: everything particular to an
   agent is in its `[acp_agents.<name>]` table of `worker.toml`. Its first agent is Google's
   Antigravity CLI (`agy acp`), as in T3 Code, Happy, Zed and JetBrains; Hermes and Gemini CLI on
   an API key are later presets in RUNBOOK, not code;
2. a **native Codex adapter over `codex app-server`** (JSON-RPC over stdio), as T3 Code, Happy,
   Nimbalyst, Conductor and Vibe Kanban drive Codex; only IDEs (Zed, JetBrains) go through
   `codex-acp`. No `codex-acp`, no Node.

Order: the generic ACP adapter with `agy` first (smaller, and it proves the generic part), then
the Codex adapter.

Criterion, part 1: on a macOS worker, with an `[acp_agents.agy]` block copied from RUNBOOK and agy
signed in on the machine, a new session in a folder whose environment lists `agy` picks it in the
new session window, answers on a Gemini model chosen in the model menu, edits a file with a
permission card (Ask), calls `hqc_attach`, a tool of a hub relay and one of an iteration-9 local
server; a command under the worker's `deny` that agy asks about is refused without a card; the
session continues after a worker restart as the same conversation; the header shows the context
and the turn's tokens; a folder whose environment does not list `agy` does not offer it.

Criterion, part 2: the same with Codex signed in with ChatGPT, plus: a message sent during a
running turn joins it (steer); Auto runs edits in the sandbox without a card; a capped input
(T39) runs its turn in Ask; the model menu lists the account's models and shows the 5-hour and
weekly limits.

Not in this iteration: Hermes (a Linux worker for Hermes and a new §8.2.1); signing an agent in
from hqc (on the machine only, decided below); the hub's skills for these agents; mirror, import
and takeover of the agents' own sessions (SPEC §9); moves (§5.3) and copies of these sessions;
these agents in rooms (§7.1.1), without a workspace (§8.4) or in foreign sessions (§11.1); model
providers (iteration 5) and the owner's Chrome (0.148) for them; the bare model and the personal
assistant session, which leave this item (below).

## What the research found (2026-11-27)

"[verify]" marks what only issue threads or secondary sources say; the live checks of T78 and
T83 confirm it before the code relies on it.

### Google's agent: Antigravity CLI over ACP

- Gemini CLI stopped serving Google AI Pro, Ultra and free personal accounts on **2026-06-18**;
  those accounts moved to **Antigravity CLI** (`agy`, Go). Gemini CLI goes on for API keys, Vertex
  AI and Code Assist Standard/Enterprise
  ([gemini-cli#27274](https://github.com/google-gemini/gemini-cli/discussions/27274)); it speaks
  ACP as `gemini --acp`
  ([acp-mode.md](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/acp-mode.md)).
- `agy acp` is a native ACP v1 server on stdio since agy 1.5.0; **1.5.1** (2026-11-05) offers every
  model of the account to an ACP client ([ACP docs](https://antigravity.google/docs/cli/acp/),
  [IDE ACP page](https://antigravity.google/docs/ide/extensions/acp/)). `session/load` replays from
  agy's own store, `session/resume` does not replay, `session/list` and `session/close` exist;
  model and mode through `session/set_config_option`.
- Sign-in: the account's tokens are in the OS keyring (Keychain; Secret Service on Linux, which
  needs a D-Bus session on a headless box); over ssh the CLI prints a URL and takes the code back;
  `agy acp --login` signs in without a browser ([Authenticate](https://antigravity.google/docs/install/)).
- Permissions, from client trackers and agy's own issues, all **[verify]**:
  - modes are default, accept-edits and plan; **1.5.18+ no longer lists `auto`**, and setting it
    is rejected ([t3code#37082](https://github.com/pingdotgg/t3code/issues/37082)); there is
    `--dangerously-skip-permissions`;
  - agy often offers only `allow_once` and `reject_once`
    ([t3code#40933](https://github.com/pingdotgg/t3code/issues/40933));
  - a detached subagent's permission request never reaches the client, and the subagent times
    out after about 60 s ([antigravity-cli#1658](https://github.com/google-antigravity/antigravity-cli/issues/1658),
    [t3code#39029](https://github.com/pingdotgg/t3code/issues/39029));
  - `agy acp` 1.5.14 did not enforce `permissions.deny` of its own settings
    ([antigravity-cli#1785](https://github.com/google-antigravity/antigravity-cli/issues/1785));
  - some of its `session/request_permission` are its own questions with fixed answers, not tool
    approvals ([acpx notes](https://github.com/openclaw/acpx/blob/main/agents/Antigravity.md)); an
    older community report said agy could not ask over ACP at all. Whether agy asks at all, and
    for what, is the first live check (T78).
- MCP servers given in `session/new`: not documented; per-session workers for HTTP servers exist
  **[verify stdio, tool naming, and which process starts them]**.
- Terms: Google suspended Antigravity and Gemini CLI accounts in February 2026 for using their
  OAuth from third-party tools. Antigravity's Additional Terms §6 forbid "using third party
  software, tools, or services to access the Service"; the FAQ says wrapping agy headless "in
  third-party agents is not supported under consumer plans"; the ACP docs say any ACP client can
  launch it; the question is open
  ([antigravity-cli#1493](https://github.com/google-antigravity/antigravity-cli/issues/1493)).
  Since September the penalty is the Antigravity account, not the Google account.

### Codex: `codex app-server`

- The App Server is OpenAI's JSON-RPC interface for clients (the VS Code extension, the desktop
  app, JetBrains, Xcode), a long-lived process holding Codex threads
  ([docs](https://developers.openai.com/codex/app-server),
  [README](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md),
  [OpenAI's post](https://openai.com/index/unlocking-the-codex-harness)); stdio is the default
  transport. Codex is at **0.172.0** (2026-11-24). The protocol changes between releases; its
  schema is generated by `codex app-server generate-json-schema` (or `generate-ts`) for the
  installed version, and clients pin it.
- Threads and turns: `thread/start`, `thread/resume`, `thread/fork`, `thread/list`; `turn/start`
  (input, and per-turn `approvalPolicy`, `sandboxPolicy`, model, effort); `turn/steer` appends input
  to the running turn; `turn/interrupt`; `turn/completed` with the final status; items stream as
  `item/started`, `item/agentMessage/delta`, `item/completed` and others.
- Approvals are server requests: `item/commandExecution/requestApproval` (the command is explicit),
  `item/fileChange/requestApproval`, answered `accept`, `acceptForSession`, `decline`, `cancel`
  [verify the exact set on the pinned version].
- `account/read`, `account/login/start` (`apiKey`, `chatgpt`, **`chatgptDeviceCode`**),
  `account/login/completed`, `account/updated`
  ([PR #15525](https://github.com/openai/codex/pull/15525)); `account/rateLimits/read` and its
  update notification (5-hour and weekly windows, ChatGPT accounts only); `model/list` (with
  `includeHidden`); token usage notifications [verify the name, `thread/tokenUsage/updated`]; the
  plan as its own notification [verify].
- Subagents: no dedicated RPC; whether their threads or items show in the parent's stream is
  [verify].
- Lessons from T3 Code:
  - `sandboxPolicy` on `turn/start` is an object; resending it on every turn reset
    `network_access` to false ([t3code#13987](https://github.com/pingdotgg/t3code/issues/13987)),
    so it is sent only when the mode changes;
  - a `turn/steer` that a wedged shared app-server never answered left the run "starting" and
    blocked the queue ([t3code#34271](https://github.com/pingdotgg/t3code/issues/34271)); a steer
    after the turn ended is rejected ("no active turn to steer", fixed in Codex's own TUI by starting
    a new turn, [codex#15714](https://github.com/openai/codex/pull/15714)); a steer during review or
    compaction is rejected too;
  - a shared app-server kept launch-time tokens after the credentials were renewed, so every turn
    failed until it restarted ([t3code#21787](https://github.com/pingdotgg/t3code/pull/21787),
    [#23720](https://github.com/pingdotgg/t3code/issues/23720)), and one wedged shared process
    stalled every thread ([#17468](https://github.com/pingdotgg/t3code/issues/17468));
  - an org's `requirements.toml` can refuse `never` with `dangerFullAccess` (`-32600` on
    `turn/start`, [t3code#25674](https://github.com/pingdotgg/t3code/issues/25674)).
- Sign-in on the machine: `codex login --device-auth` (device codes enabled in ChatGPT's security
  settings) or a login elsewhere with `~/.codex/auth.json` copied
  ([Authentication](https://developers.openai.com/codex/auth)). Running the official Codex through
  its App Server, signed in by the user, is the route OpenAI documents for clients.
- Config: `~/.codex/config.toml` (`$CODEX_HOME`), a project's `.codex/config.toml` only in a trusted
  project, `AGENTS.md`; its `mcp_servers` reach the agent whatever hqc passes.

### ACP

- Rust crate `agent-client-protocol` **5.6.1** (2026-11-25): pin `=5.6.1`, v1 module. Stable in v1:
  `session/load`, `resume`, `list`, `close`, `set_config_option`, elicitation, `usage_update`
  (context `used` / `size`, optional `cost`) and `session_info_update` (title)
  ([updates](https://agentclientprotocol.com/updates)). v2 is alpha.
- An agent that needs sign-in answers `session/new` with the auth-required error (`-32000`) and
  lists `authMethods` in `initialize`.

## Decisions

### Both adapters

- **The machine declares each agent, and where it may run** (§2.1): the hub cannot add, start or
  widen one. A start in a folder whose environment does not list the agent is refused.
- **Each agent runs with the machine's own home and sign-in**, one sign-in per machine, made on the
  machine. What the agent connects by itself (MCP servers of its own config, its extensions, its
  hooks) is outside hqc's control; RUNBOOK says so, and hqc does not read the agents' config files.
- **History is hqc's own.** The agent's store only continues a conversation (no mirror, no import,
  no transcript reading); a resumed session's id is checked against the requested one (§8.2).
- **hqc's MCP servers** (`hqc`, the hub's relays with `hub_mcp`, the environment's local servers
  of iteration 9) go to the agent by the launchers `McpLaunch::config` builds now, with the session
  token in each server's `env`; the process check of §7.2 must hold (an MCP peer descends from the
  agent's process) [verify for agy].
- **The worker's check on what the agent asks.** Before a card, the worker's `deny` (and the
  environment's) is matched against the call; a match is answered with the agent's reject, no card,
  `system policy_denied`. The `[permissions] allow` and hqc's folder rules answer "once" without a
  card, by the conservative matcher of §8.1. Calls of hqc's own servers are allowed without a card
  when the agent names the server. **What the agent runs without asking never reaches the
  worker: the agent's own sandbox is the boundary there** (decided with the owner); SPEC §8.2 and
  RUNBOOK say it plainly. Allow all needs `[permissions] allow_all = true`, as for Claude Code.
- **Sign-out** is a `system` line, `agent_signed_out {runtime, agent?}`, in the feed and in
  Settings → Workers, as `claude_signed_out` (0.185), pointing to RUNBOOK; signing in stays on the
  machine.
- **The client** gets a runtime picker in the new session window (Claude Code, Codex if the
  worker has it, and the ACP agents the folder's environment allows, from `hello`); for these
  sessions it hides side questions, background, Stop of one task, move, copy, release to VS Code,
  provider and Chrome; the approval card shows the agent's call (title, kind, command, diff).

### The generic ACP adapter

- **Nothing in the code knows an agent by name.** Everything particular to one is in its table:

  ```toml
  [acp_agents.agy]
  command = "/usr/local/bin/agy"            # absolute (§8.2)
  args = ["acp"]
  env = {}                                  # plain or { vault = … }, as in iteration 9
  environments = ["default"]                # where it may run; none listed: nowhere
  modes = { ask = "default" }               # the agent's own mode or config-option ids
  allow_all_args = ["--dangerously-skip-permissions"]   # for an agent that needs a start flag
  ```

  `modes = { ask, auto, allow_all }` names the agent's ids for hqc's three modes; an agent without
  `modes` runs Ask only and is not switched; a missing `auto` or `allow_all` falls to the next lower
  mode, and the worker reports the mode it runs. `allow_all` (an id or `allow_all_args`) also needs
  `[permissions] allow_all = true`. `isolation` of SPEC §14 goes. RUNBOOK ships ready blocks for
  agy, and later Gemini CLI on an API key and Hermes: the presets live in the docs, not in the code.
- **The command of an `execute` request** is read from common `rawInput` keys, `command` or `cmd`,
  a string or an array of words. When none is found the deny check cannot apply: such a request is
  always a card, never answered by an allow rule.
- **Options by `optionId`, shown by their names**, with `kind` only for the card's colour: agents
  disagree on what `allow_always` means (session for some, forever for others), so hqc writes no
  rule from it.
- **Sign-out is generic**: the auth-required error of `session/new` or `session/prompt`, or
  `authMethods` with no session possible; no agent's message text is parsed.
- **Continuing**: `session/resume` when advertised (hqc has the history), else `session/load` with
  its replay dropped from the feed.
- **Events** (T75): `agent_message_chunk` → `agent.delta`/`agent.message`, `agent_thought_chunk` →
  `agent.thinking`, `tool_call`/`tool_call_update` → `tool.call`/`tool.result` (a diff as a
  `kind: diff` artifact), `plan` → the agent's task list (0.222), `usage_update` → context in
  `usage`, the prompt reply's usage → the turn's tokens, `session_info_update` → the runtime title,
  `current_mode_update`/`config_option_update` → mode and model, `available_commands_update` →
  `command_info`, elicitation → `clarify.request`.
- **The model menu** comes from the agent's `model` config option once a session runs; a new
  session starts on the agent's default.
- **Antigravity on the owner's Google AI subscription, at the owner's risk** (decided with the
  owner): RUNBOOK's agent setup section carries the warning (Google's consumer terms lean against
  third-party clients, accounts were suspended for it in February 2026, a spare account is safer)
  and `hqc-worker env` prints one line next to an agent whose command is `agy`. No warning in the
  client. (The one place a name is matched, and only for that line.)

### The native Codex adapter

- **`runtime: codex`**, a runtime of its own beside `claude_code` and `acp` (proto, hub, client).
  `worker.toml`:

  ```toml
  [codex]
  command = "/opt/homebrew/bin/codex"       # absolute
  environments = ["default"]
  ```

  The worker runs `codex --version` at start and supports one pinned version range; the schema of
  that version (`codex app-server generate-json-schema`) is checked into the repo and the adapter's
  types follow it; a version outside the range is refused with a line in the log and in
  `hqc-worker env`. The protocol is experimental: a Codex update is a worker change, tested before
  the range moves.
- **One app-server process per hqc session**, not one per worker. A session's process carries its
  environment (env, ssh-agent, secrets, §14), which a shared process cannot do per thread; the MCP
  check of §7.2 and `Adapter::pid` need a process per session; a wedged process stalls one session,
  not all (T3 Code's #17468); and each start reads the current sign-in, so a renewed `auth.json`
  reaches the next start (T3 Code's stale-token #21787, #23720). The cost is a process per running
  session, within `max_sessions`. A health check: a request the app-server does not answer within
  a timeout (a steer, an interrupt) ends the process; the session restarts on its next input with
  `thread/resume`.
- **Mapping**:
  - `thread/start` (cwd, model, the mode's policies, hqc's MCP servers as config overrides of the
    thread [verify the field; else `-c mcp_servers.*` at process start]) → a new session,
    `runtime_session_id` = the thread id; `thread/resume` → continuing; `thread/fork` → not used
    yet (copies are out of scope);
  - `turn/start` → a turn; `turn/steer` with the running turn's id → an input joined into it
    (`can_inject`/`inject_with`); a steer rejected (no active turn, review, compaction) leaves the
    input queued for the next turn; a steer not answered in time → the health check above;
  - `turn/interrupt` → Stop; `turn/completed` → `TurnEnded`;
  - `item/*` → `agent.delta`/`agent.message`, reasoning → `agent.thinking`, command executions and
    file changes → `tool.call`/`tool.result` (a diff as an artifact), MCP calls likewise; the plan
    → the task list; token usage → `usage` (context and the turn's tokens);
  - `requestApproval` (command, file change) → `approval.request` after the worker's `deny` and
    allow check on the **explicit** command; "Allow for session" = `acceptForSession`; the user's
    questions (`requestUserInput` [verify]) → `clarify.request`;
  - `account/rateLimits/read` and its updates → `worker.limits` (the 5-hour and weekly windows)
    shown in the model menu as for Claude Code; `model/list` → the models and efforts, read at
    start and sent in `hello`, so the new session window offers them before a session exists;
  - subagents and background items → `LiveSubagents` only if the stream shows them [verify];
    otherwise none.
- **Modes per turn.** Ask = `read-only` sandbox with `on-request` approvals (reads run, anything
  else asks); Auto = `workspace-write` (network as the user's config says) with `on-request` (edits
  and commands run in the sandbox, leaving it asks); Allow all = `danger-full-access` with `never`,
  only with `allow_all = true`. `approvalPolicy` and `sandboxPolicy` go on `turn/start` only when
  they change, so a T39 cap lowers one turn and the next turn restores the session's mode. A
  `-32600` from the machine's `requirements.toml` falls to the next lower mode with a `system` line.
- **Sign-in on the machine** (`codex login --device-auth`); `account/read` at start says whether
  the machine is signed in (`hello`), and an auth error in a turn is `agent_signed_out`.
  `chatgptDeviceCode` is the later way to sign in from hqc (the relay of 0.185).
- **What Codex gets that ACP agents do not**: steering into a running turn, per-turn modes (exact
  T39 caps), the account's limits, models and efforts before a session starts, the explicit command
  in every approval (deny always applies to what it asks), and token usage per turn.

### No speculative code

No ACP v2, no `session/fork` or `thread/fork`, no import of the agents' session lists, no sign-in
from hqc, no Node, no per-agent code in the ACP adapter.

## Decided with the owner (2026-11-27)

1. **Antigravity runs on the Google AI subscription**, at the owner's risk, with the warning in
   RUNBOOK and in `hqc-worker env`, none in the client.
2. **The agent's own sandbox is the boundary** for what it runs without asking; the worker checks
   `deny` and the allow rules on every request it gets; Allow all only with `allow_all = true`.
   Documented in SPEC §8.2 and RUNBOOK.
3. **Sign-in on the machine only**; no relay through hqc for now.
4. **Node on the workers** was accepted for `codex-acp`; with the native Codex adapter nothing
   needs it any more.
5. **The bare model and the personal assistant session go to `docs/BACKLOG.md`**; `hqc_send`
   keeps needing `session`.
6. **Two adapters** (after a survey of comparable tools): a generic ACP adapter with agy first,
   then a native Codex adapter over `codex app-server`.

## Part 1: the generic ACP adapter

### T74. `[acp_agents]` and the probe at start

`config.rs`: `[acp_agents.<name>]` (`command` absolute, `args`, `env` plain or vault,
`environments`, `modes`, `allow_all_args`), checked at load (a name `[a-z0-9_-]{1,32}`, the
environments exist, `allow_all` needs `[permissions] allow_all = true` to be offered); `isolation`
dropped. At start the worker runs `initialize` on each agent (a timeout; the child allowlist plus
`env`), keeps `agentInfo` and capabilities, and sends `hello.capabilities.acp_agents[{name,
version, environments, modes, load_session, resume, image}]`; `hqc-worker env` and `env which`
list the agents a folder may run, with the agy warning line. Touches `crates/hqc-worker/src/
config.rs`, `capabilities.rs`, `env_cmd.rs`, `crates/hqc-proto/src/frame.rs`, wire examples.

### T75. The adapter

`adapter/acp.rs` on `agent-client-protocol = "=5.6.1"` (v1): a process per session in the
workspace's environment (§14), `initialize` with `fs` and `terminal` off and form elicitation on,
`session/new {cwd, mcpServers}`, resume/load with the id check, `session/prompt` per input
(`InputAccepted` on send, images where advertised), `session/cancel`, the events of the decisions,
generic sign-out, `session/close` then kill. `BuiltinAdapters` routes `Runtime::Acp` here. Tests
against a fake ACP agent (a small test binary on the same crate). Touches
`crates/hqc-worker/src/adapter.rs`, `adapter/acp*`, `Cargo.toml`.

### T76. Permissions and modes

`modes` set at `session/new` and on `session.permission_mode` (by `set_mode` or
`set_config_option`, whichever the agent offers), capped by `[permissions]`; `allow_all_args` at
start; the request path (deny → reject, allow rules → once, hqc's servers → allowed, else a card
with the agent's options); `rawInput.command`/`cmd`; `approval.cancel` for requests open at the
end of a turn or at Stop. Touches `adapter/acp/permissions.rs`, `permissions.rs`.

### T77. MCP

`mcpServers` from `McpLaunch` as stdio entries with `env`; the ancestry check held for the agent's
process tree, with a clear failure when it does not; a live check that the session token ends up
in no file of the agent's session store (if it does, the token goes through the daemon's socket
instead of `env`). Touches `adapter/acp/mcp.rs`, `mcp/procs.rs` if needed.

### T78. agy, live

`agy` ≥ 1.5.1, the version pinned in RUNBOOK. Checked live and written into RUNBOOK's block and
SPEC §8.2: **whether agy asks over ACP at all**, for which calls, with which options and `rawInput`;
its modes on the pinned version (Ask = `default`; `auto` gone in 1.5.18+; Allow all by
`--dangerously-skip-permissions`); subagents' requests (if they still never arrive, RUNBOOK says
so); MCP tool naming and process tree; sign-out; the keyring on a headless Linux box. Live test
`live_acp_agy`. Touches `crates/hqc-worker/tests/`, RUNBOOK.

## Part 2: the native Codex adapter

### T79. `[codex]`, the version and the schema

`config.rs` `[codex]` (`command`, `environments`); `codex --version` against the supported range;
the generated schema of the pinned version in the repo with a script to regenerate it; at start
`account/read` (signed in?) and `model/list` (models, efforts) from a short-lived app-server, sent
in `hello.capabilities.codex{version, signed_in, models}`; `hqc-worker env` lists it. Touches
`config.rs`, `capabilities.rs`, proto (`Runtime::Codex`), `adapter/codex/schema/`.

### T80. Threads, turns and steer

`adapter/codex.rs`: the process per session, `initialize`, `thread/start|resume`, `turn/start`,
`turn/steer` (with the expected turn id, a rejected steer left queued), `turn/interrupt`, the item
stream to §4 events, the health check and restart. Tests against a fake app-server speaking the
pinned schema. Touches `adapter/codex*`, `adapter.rs`.

### T81. Approvals and modes

The approval requests with the worker's check on the explicit command and file paths; the modes
per turn sent only on change; T39 caps per turn; `requirements.toml` refusals; user-input requests
to `clarify.request`. Touches `adapter/codex/permissions.rs`, `permissions.rs`.

### T82. Limits, usage and models

`account/rateLimits/*` → `worker.limits`; token usage → `usage`; the model and effort menus from
`hello`; `thread/start` MCP overrides for hqc's servers. Touches `adapter/codex/*`.

### T83. Codex, live

Live test `live_codex` on a macOS worker: a turn, an edit asked (Ask) and run in the sandbox
(Auto), a deny, a steer, a capped turn, a resume after a worker restart, an hqc tool, the limits.
Checks the [verify] items of the Codex research on the pinned version.

## Both

### T84. The hub

`POST /api/projects/{id}/sessions` and `hqc_spawn` take `runtime: "acp"` with `acp_agent`, and
`runtime: "codex"`, checked against the worker's `hello` (present, allowed in the folder's
environment) for an early `400`; the worker decides finally. The model list of an ACP session from
`session.state`, Codex's from `hello`. `agent_signed_out` in Settings → Workers. Touches
`crates/hqc-hub/src/api*.rs`, `delivery.rs`, proto.

### T85. The client

The runtime picker, the model and effort menus per runtime, Ask / Auto / Allow all as the worker
reports them, the approval card for these calls, the hidden Claude-only controls, Settings →
Workers listing each worker's agents and Codex with version, environments and sign-in state.
Touches `client/src/`, `client/mock`.

### T86. SPEC and RUNBOOK

SPEC: §8.2 rewritten (the generic adapter, the boundary of what the worker sees), a new §8.6 for
Codex, §8.2.1 marked as the old in-process Hermes, §4 `usage` and `system agent_signed_out`, §14
`acp_agents` and `codex`, §3 `runtime: codex`. RUNBOOK: installing and signing in agy and Codex on
the machine, the ready `[acp_agents]` blocks (agy now; Gemini CLI on an API key and Hermes later),
the agy terms warning, and what the agents connect by themselves.

## Later

- Hermes on the generic adapter: a Linux worker for it, a new §8.2.1 for the gateway-owned Hermes
  v0.22+, a RUNBOOK block.
- Sign-in from hqc (Codex's `chatgptDeviceCode`, agy's `--login`) through the relay of 0.185.
- `thread/fork` and `session/fork` for copies of these sessions; the hub's skills for them.
