# Iteration 10: tasks

Goal (SPEC §19.10): the owner runs the newest OpenAI and Google models in their own agent
harnesses, Codex and Google's terminal agent, from hqc as easily as Claude Code, paid by the
owner's ChatGPT and Google AI subscriptions rather than API keys. One generic ACP adapter on the
worker (SPEC §8.2) drives both; Hermes comes later on the same adapter.

Criterion: on a macOS worker, with `[acp_agents.codex]` in `worker.toml` and Codex signed in with
ChatGPT on the machine, a new session in a folder whose environment allows `codex` picks "Codex"
in the new session window, gets an answer from a GPT model chosen in the model menu, edits a file
with the usual permission card (Ask) and without one (Auto), calls `hqc_attach` and a tool of a
hub relay and of an iteration-9 local server; a command under the worker's `deny` is refused
without a card; the session survives a worker restart (resumed, not a new conversation); the
header shows the context and the turn's tokens; a folder whose environment does not list `codex`
does not offer it. Then the same with Antigravity on the owner's Google AI subscription.

Not in this iteration: Hermes (it needs a Linux worker for Hermes and a new §8.2.1, below); signing
an agent in from hqc (on the machine only, decided below); the hub's skills for ACP agents; mirror, import and takeover of
an ACP agent's own sessions (SPEC §9); moves (§5.3) and copies of ACP sessions; ACP agents in
rooms (§7.1.1: no system prompt in ACP), without a workspace (§8.4) or in foreign sessions (§11.1);
model providers (iteration 5) and the owner's Chrome (0.148) for them; an agent's subagents in
the feed; the bare model and the personal assistant session, which leave this item (below);
Gemini CLI on an API key.

## What the research found (2026-11-27)

Sources are primary where they could be reached; "[verify]" marks what only secondary sources or
issue threads say, to be checked on the machine before the code relies on it.

### Gemini CLI no longer serves Google accounts: Google's agent is now Antigravity CLI

- Google announced on 2026-05-19 that Gemini CLI stops serving Google AI Pro, Ultra and free
  personal accounts on **2026-06-18**; those tiers moved to **Antigravity CLI** (`agy`, Go).
  Gemini CLI goes on for API keys, Vertex AI and Gemini Code Assist Standard/Enterprise
  ([gemini-cli discussion #27274](https://github.com/google-gemini/gemini-cli/discussions/27274)).
  Gemini CLI itself is alive (v0.71.1, 2026-11-24; ACP as `gemini --acp`,
  [docs/cli/acp-mode.md](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/acp-mode.md)),
  but not with the owner's Google sign-in. So "Gemini in its native harness on the subscription"
  means `agy`, and the plan says **Antigravity** where the task said Gemini CLI.
- `agy acp` is a native ACP server, protocol v1, stdio, since agy **1.5.0** (early November
  2026); 1.5.1 (2026-11-05) offers every model of the account to an ACP client
  ([Antigravity ACP docs](https://antigravity.google/docs/cli/acp/),
  [IDE ACP page](https://antigravity.google/docs/ide/extensions/acp/)). It supports
  `session/load` (replays history from agy's own store), `session/resume` (no replay),
  `session/list`, `session/close`; modes default / accept-edits / plan and the model through
  `session/set_config_option`; steering a running turn [verify how]; prompts with images.
- Sign-in: the account's tokens are in the OS keyring (Keychain on macOS, Secret Service on
  Linux, which needs a D-Bus session on a headless box); over ssh the CLI prints a URL and takes
  the code back; `agy acp --login` signs in for an ACP client without a browser; `agy acp` then
  reuses the cached sign-in and answers "Authentication required" when there is none
  ([Authenticate](https://antigravity.google/docs/install/)). The file path of the token and
  how long the refresh lasts are not documented [verify].
- Permissions: an ACP client gets Allow / Deny cards, agy's own permission rules still apply,
  `--dangerously-skip-permissions` skips the cards. Its settings
  (`~/.gemini/antigravity-cli/settings.json`) have allow / ask / deny rules and an "auto" policy
  that runs commands in its terminal sandbox (off by default, `enableTerminalSandbox`) and judges
  the ones outside it itself ([Agent settings](https://antigravity.google/docs/agent-settings/))
  [verify the CLI's exact keys]. Some of agy's `session/request_permission` are its questions to
  the user with fixed answers, not tool approvals
  ([acpx notes](https://github.com/openclaw/acpx/blob/main/agents/Antigravity.md)) [verify].
- MCP servers given in `session/new` and how agy names their tools are not documented; it runs
  per-session workers for HTTP MCP servers [verify stdio, naming, and which process starts them].
- Models on Google AI Pro: Gemini 3.x Pro and Flash (the docs page changes often; Claude models
  appear for some tiers), sharing one quota ([models](https://antigravity.google/docs/models?app=cli)).
- **Terms.** Google suspended Antigravity and Gemini CLI accounts in February 2026 for using
  their OAuth from third-party tools (OpenClaw, OpenCode). Antigravity's Additional Terms §6 forbid
  "using third party software, tools, or services to access the Service"; the FAQ says
  "wrapping agy headless mode in third-party agents is not supported under consumer plans" and
  points to API keys; the ACP docs say "any ACP client can launch Antigravity CLI". hqc starts the
  official, unmodified `agy` binary on the owner's own machine for the owner alone, as Zed does,
  and never touches its tokens; whether that is allowed is asked in
  [antigravity-cli#1493](https://github.com/google-antigravity/antigravity-cli/issues/1493),
  still open. Since September 2026 the penalty is limited to the Antigravity / Gemini CLI account,
  not the whole Google account. The owner runs it anyway, at their own risk (decided below).

### Codex: `codex-acp` on top of the Codex App Server

- Codex has no ACP mode of its own. Zed's Rust `zed-industries/codex-acp` was **archived on
  2026-07-22**; its successor is
  [agentclientprotocol/codex-acp](https://github.com/agentclientprotocol/codex-acp)
  (`@agentclientprotocol/codex-acp` on npm, Node ≥ 20), which starts `codex app-server`, the JSON-RPC
  interface OpenAI documents for third-party clients (VS Code, JetBrains, Xcode), and translates
  ACP to it. Latest seen: **v2.8.0, 2026-11-19**, on Codex 0.170.0 (ACP SDK 2.8); Codex itself is
  at **0.172.0** (2026-11-24). Node and a pinned npm version therefore come to the worker's
  machine with Codex.
- Sign-in: ChatGPT, API key (`CODEX_API_KEY` over `OPENAI_API_KEY`) or a gateway are the ACP
  auth methods it advertises; `NO_BROWSER=1` hides the browser ChatGPT method. On the machine:
  `codex login --device-auth` (beta; device codes must be enabled in ChatGPT's security
  settings), or a login elsewhere and `~/.codex/auth.json` copied; tokens are in
  `$CODEX_HOME/auth.json` or the OS keyring (`cli_auth_credentials_store = file | keyring | auto`)
  ([Authentication](https://developers.openai.com/codex/auth)). A device-code ACP method exists in
  some builds but needs the client to support URL elicitation [verify].
- Terms: the App Server is the documented surface for clients, and the user signs in to Codex
  itself; OpenAI also opened "Sign in with ChatGPT" for partner tools in September 2026. Running the
  official Codex through its App Server is the sanctioned route; borrowing Codex's OAuth client id
  is not. Usage counts against the plan's Codex limits (5-hour and weekly windows).
- Approval and sandbox: `approval_policy` (`untrusted` (being retired), `on-request`, `never`,
  granular) and `sandbox_mode` (`read-only`, `workspace-write` with network off unless
  `[sandbox_workspace_write] network_access = true`, `danger-full-access`); Seatbelt on macOS,
  Landlock on Linux ([config reference](https://developers.openai.com/codex/config-reference),
  [sandboxing](https://developers.openai.com/codex/concepts/sandboxing)). codex-acp exposes them
  as session modes: **Read-only, Workspace access, Auto review, Full access**
  ([PR #480](https://github.com/agentclientprotocol/codex-acp/pull/480)); in ACP v2 through config
  options; the starting mode ignores `config.toml`
  ([#702](https://github.com/agentclientprotocol/codex-acp/issues/702)). Under Auto review a
  reviewer model approves and the client is not asked
  ([#790](https://github.com/agentclientprotocol/codex-acp/issues/790)): the nearest thing to
  Claude Code's Auto.
- Its permission requests offer `allow_once`, `allow_always`, `reject_once`, where
  `allow_always` is **for this session only** (Codex's `acceptForSession`, kept in the Codex
  process) [verify on 2.8.0], the same trap as Hermes (SPEC §8.2.1): options are mapped per
  agent, not by `kind`.
- Config: `~/.codex/config.toml` (or `$CODEX_HOME`), a project's `.codex/config.toml` only in a
  trusted project, `AGENTS.md`; `mcp_servers` there give the agent servers hqc did not pass.
- Models on ChatGPT sign-in: the GPT-5.6 family (and older ones still listed), with reasoning
  effort ([models](https://learn.chatgpt.com/codex/models)); codex-acp offers model and effort as
  config options [verify the ids].
- Sessions: rollouts under `$CODEX_HOME/sessions/`; codex-acp loads and resumes them, also after
  an App Server restart (2.2.1).

### ACP itself

- Rust crate `agent-client-protocol` **5.6.1 (2026-11-25)**, after nine releases since 5.0.0
  (2026-11-02): pin `=5.6.1` and use its v1 module. Schema v1 is at 1.28 (TypeScript SDK);
  v2 is still alpha (2.0.0-alpha.11).
- Stable in v1 now: `session/load`, `resume`, `list`, `close`, `set_config_option`,
  elicitation, **`usage_update`** (context `used` / `size`, an optional `cost` with a currency
  that may be "credits") and **`session_info_update`** (title)
  ([updates](https://agentclientprotocol.com/updates),
  [session usage RFD](https://agentclientprotocol.com/rfds/session-usage)). So the §4 note "ACP v1
  has no standard usage" is out of date.

## Decisions (proposed; the owner has the final word)

- **The runtime stays `acp` with `acp_agent: <name>`.** SPEC §3, the protocol (`Runtime::Acp`,
  `session.start.acp_agent`), the hub's `sessions.acp_agent` column and the API already have it.
  The name is the worker's table name (`codex`, `agy`), so a new agent needs no enum value in the
  hub, the client or the protocol, and two Codex setups on one machine are two names. Rather than
  `runtime: gemini_cli | codex`.
- **`worker.toml` declares each agent, and which environments may use it.**

  ```toml
  [acp_agents.codex]
  command = "<prefix>/bin/codex-acp"       # an absolute path (§8.2)
  args = []
  env = { NO_BROWSER = "1" }               # plain or { vault = … } as in iteration 9
  environments = ["default", "firmware"]  # where it may run; none listed: nowhere

  [acp_agents.agy]
  command = "/usr/local/bin/agy"
  args = ["acp"]
  environments = ["default"]
  ```

  By §2.1 only the machine's owner decides what runs there: the hub cannot add, start or widen an
  agent. `isolation` of SPEC §14 (a Hermes idea) goes. The worker checks each agent at start
  (`initialize`: `agentInfo` name and version, `loadSession`, `resume`, image, auth methods) and
  names them in `hello.capabilities.acp_agents[{name, version, environments, load_session, resume,
  image}]`; a missing binary or a failed `initialize` is in the log and in `hqc-worker env`, and
  the agent is not offered.
- **Each agent runs with the machine's own home and sign-in**, as Claude Code does: one sign-in
  per machine, made by the owner on the machine. A separate `CODEX_HOME` per hqc would need a
  second sign-in and copied tokens whose refresh then splits. What the agent's own config adds
  behind hqc's back is checked instead (MCP, below).
- **Sign-in only on the machine** (the owner, 2026-11-27): no relay through hqc. RUNBOOK gives
  the commands (`codex login --device-auth`; `agy acp --login`, or `agy` once over ssh). A start or
  a turn that the agent ends with "authentication required" (`-32000` or its auth error) becomes
  `system acp_signed_out {agent, command}` in the feed and in Settings → Workers, as
  `claude_signed_out` (0.185).
- **Modes map to the agent's own modes; the worker's `[permissions]` caps them as for Claude
  Code:**

  | hqc        | Codex (codex-acp mode)             | Antigravity              |
  |------------|------------------------------------|--------------------------|
  | Ask        | Read-only: reads run, writes and commands outside the read-only sandbox ask | default: every non-read call asks [verify] |
  | Auto       | Auto review (sandboxed workspace-write, a reviewer model decides, no card)  | auto (its sandbox, it judges itself) |
  | Allow all  | Full access (`danger-full-access`, never asks), only with `allow_all = true` | `--dangerously-skip-permissions` at start, only with `allow_all = true` |

  The mode ids come from the agent's `modes` / config options; a mode the agent does not offer
  falls to the next lower, and the worker reports the mode it runs (`session.state.permission_mode`).
  `allow_auto = false` keeps the session in Ask, as for Claude Code.
- **The worker's own check on every permission request.** A `session/request_permission` is
  turned into `approval.request` with the agent's real options (by `optionId`), and before that:
  - the worker's `deny` (and the environment's) is matched against the call: an `execute` tool
    call's command (from `rawInput`, by the same matcher as for `Bash`), an MCP tool by
    `mcp__<server>__<tool>` when the agent names it; a match is answered with the agent's reject
    option, no card, `system policy_denied`;
  - hqc's folder allow rules and `[permissions] allow` answer `allow_once` without a card, by
    the same conservative matcher (§8.1);
  - calls of hqc's own servers (`hqc`, the hub's relays, the iteration-9 locals the worker added)
    are allowed without a card, as `--allowedTools mcp__hqc` does for Claude Code, but only when
    the agent says which server asks (agy may not, as Gemini CLI did not:
    [gemini-cli#29595](https://github.com/google-gemini/gemini-cli/issues/29595)); otherwise the
    card comes.
  - "Always" on the card is "for this session" for both agents (their `allow_always` is
    session-scoped); hqc writes no folder rule from it in this iteration.

  **The limit, accepted by the owner (2026-11-27) and said plainly in SPEC §8.2 and RUNBOOK:**
  the worker sees only what the agent asks. What it runs without asking (reads in Ask,
  everything in Auto and Allow all) never reaches the worker's `deny`; the boundary there is the
  agent's own sandbox (Codex: Seatbelt/Landlock, network off in workspace-write; agy: its terminal
  sandbox). The worker checks `deny` and the allow rules on every request it does get, and Allow
  all needs `allow_all = true`, as for Claude Code.
- **MCP: hqc's servers in `session/new.mcpServers`, nothing else.** `hqc`, the hub's relays
  (`hub_mcp`) and the environment's local servers (`mcp_servers`, iteration 9) go as `stdio`
  entries with the same launchers and the session token in their `env` (`McpLaunch::config`
  builds them already). The process check of §7.2 (an MCP peer must descend from the agent's
  process) holds for codex-acp → app-server → server; for agy it must be checked [verify: a warm
  daemon would break it, as Hermes v0.22's did]. Servers the machine's agent config adds on its own
  (`mcp_servers` in `~/.codex/config.toml`, `mcpServers` in agy's settings) are the leak of
  iteration 9 again: the worker reads those files at every start and **refuses the start** with a
  `system` line naming the file unless the environment has `allow_local_mcp = true`. The token
  must not end up in the agent's session store (Codex rollout, agy's database) [verify; if it
  does, the token goes through the daemon socket instead of `env`].
- **History is hqc's own.** The adapter maps `session/update` to §4 events (T75); the agent's
  store is used only to continue: `session/resume` when advertised (no replay: hqc already has
  the history), else `session/load` with the replayed updates dropped rather than shown twice. The
  `sessionId` of the answer is checked (§8.2). No mirror, no import, no transcript reading.
- **What the client shows differently for an ACP session:** the runtime picker in the new
  session window (Claude Code, and the worker's agents the folder's environment allows, by
  `hello`); the model menu filled from the agent's `model` config option once the session runs
  (a new session starts on the agent's default; the owner's last choice per agent is remembered on
  the device); the effort / thinking level from its `thought_level` option where there is one;
  Ask / Auto / Allow all as above; no side question, background, subagent list or history, Stop of
  one task, move, copy, release to VS Code, provider, Chrome; the approval card shows the ACP tool
  call's title, kind, content (diff, text) and `rawInput`; no limit bar (`worker.limits` is Claude
  Code's) unless codex-acp reports the plan's windows (later).
- **Order: generic adapter with Codex first, then Antigravity.** Codex's route is the sanctioned
  one, codex-acp is the more complete adapter (modes, auth methods, resume after an App Server
  restart), and it needs only Node ≥ 20 on the worker (codex-acp declares no `engines`; its dependency
  `open@11` needs 20).
  Antigravity follows on the same adapter; its specifics are one task.
- **Antigravity on the owner's Google AI subscription, at the owner's risk** (the owner,
  2026-11-27). Google's terms for consumer plans lean against third-party clients, and accounts
  were suspended for it in February 2026; hqc says so where the agent is set up, not in the UI:
  RUNBOOK's agent setup section carries the warning (a spare Google account is safer), and
  `hqc-worker env` prints one line next to an `agy` agent. No warning in the client.
- **No speculative code:** no unknown-agent fallbacks beyond what `initialize` tells, no ACP v2,
  no `session/fork`, no `session/list` import, no limits bar for agents, no agent auth from hqc.

## Decided with the owner (2026-11-27)

1. **Antigravity runs on the Google AI subscription**, with the warning above in RUNBOOK and in
   `hqc-worker env`, none in the client; after Codex.
2. **The agent's own sandbox is the boundary** for what it runs without asking; the worker checks
   `deny` and the allow rules on every permission request it gets; Allow all only with
   `allow_all = true`. Documented in SPEC §8.2 and RUNBOOK.
3. **Sign-in on the machine only**; no relay through hqc for now.
4. **Node on the workers** for codex-acp is fine.
5. **The bare model and the personal assistant session go to `docs/BACKLOG.md`**; `hqc_send`
   keeps needing `session`.

## T74. The worker: `[acp_agents]` and the probe at start

`config.rs`: `[acp_agents.<name>]` (`command` absolute, `args`, `env` plain or vault,
`environments`), checked at load (a name `[a-z0-9_-]{1,32}`, environments that exist);
`isolation` dropped. At start the worker runs `initialize` on each agent (a timeout, the child
allowlist environment plus `env`), records name, version and capabilities, and puts them into
`hello.capabilities.acp_agents` (proto, SPEC §5.4); `hqc-worker env` and `env which` list the
agents a folder may run, with one line of warning next to an `agy` agent (Google's consumer terms,
the owner's risk). A `session.start` with an agent the folder's environment does not list
is refused (`agent_start_failed`, not retryable). Touches `crates/hqc-worker/src/config.rs`,
`capabilities.rs`, `env_cmd.rs`, `crates/hqc-proto/src/frame.rs`, wire examples.

## T75. The generic ACP adapter

`adapter/acp.rs` (+ `acp/` submodules) on `agent-client-protocol = "=5.6.1"` (v1): one process
per session in the workspace's environment (§14: env, ssh-agent), `initialize` with `fs` and
`terminal` off and form elicitation on; `session/new {cwd, mcpServers}` → `runtime_session_id`;
`session/resume` or `session/load` to continue, with the id check and replay suppression;
`session/prompt` per input (`InputAccepted` on send, images as content blocks where `image` is
advertised, files by path otherwise), `session/cancel` for Stop, `TurnEnded` on the reply.
`session/update` → §4: `agent_message_chunk` → `agent.delta` / `agent.message` (`final` on the
reply), `agent_thought_chunk` → `agent.thinking`, `tool_call` / `tool_call_update` →
`tool.call` / `tool.result` (a diff as a `kind: diff` artifact), `plan` → the agent's task list
(0.222), `usage_update` → `usage.context_tokens` / `context_limit` (and `cost_usd` only for a
USD cost), the prompt reply's `usage` → the turn's tokens, `session_info_update.title` →
`session.state.title` (`title_source: runtime`), `current_mode_update` / `config_option_update` →
`session.state.permission_mode` and the model, `available_commands_update` →
`session.state.command_info`; elicitation → `clarify.request`. `close` = `session/close` then
kill; a crash is `agent_start_failed` / `system` as for Claude Code. `BuiltinAdapters` routes
`Runtime::Acp` here. Tests against a fake ACP agent (a small test binary speaking the same crate),
no network. Touches `crates/hqc-worker/src/adapter.rs`, `adapter/acp*`, `Cargo.toml`.

## T76. Permissions and modes

The mode table above, set at `session/new` and on `session.permission_mode` (through
`set_mode` or `set_config_option`, whichever the agent offers), capped by `[permissions]`; the
request path of the decisions (deny → reject without a card, allow rules → `allow_once`, hqc's
servers → allowed, else `approval.request` with the agent's options, mapped per agent:
`allow_always` shown as "This session"); `approval.cancel` for requests open at the end of a turn
or at Stop; the `execute` command extracted from each agent's `rawInput` shape. Touches
`adapter/acp/permissions.rs`, `permissions.rs` (matcher reuse), SPEC §8.2, §10.1, §14.

## T77. MCP for ACP agents

`mcpServers` from `McpLaunch` (hqc, relays, locals) as stdio entries with `env`; the peer
ancestry check confirmed per agent; the start refused when the agent's own config adds MCP
servers and the environment lacks `allow_local_mcp` (reads `$CODEX_HOME/config.toml`
`mcp_servers`, agy's settings `mcpServers`); a live check that the token is in no session file
of the agent. Touches `adapter/acp/mcp.rs`, `mcp/procs.rs` if needed.

## T78. Codex

`@agentclientprotocol/codex-acp` pinned (2.8.0 or newer, checked), installed under the worker's
folder by RUNBOOK; its mode and config-option ids, its permission options and `rawInput` of exec
and patch approvals, its auth error; `system acp_signed_out`; live test `live_acp_codex` on
a macOS worker (a turn, an edit asked and allowed, a deny, a resume after a worker restart, an hqc tool).
Touches `adapter/acp/codex.rs` (the per-agent table), `crates/hqc-worker/tests/`, RUNBOOK.

## T79. Antigravity

The same for `agy acp` (pinned version ≥ 1.5.1): its modes, options (questions vs. approvals),
MCP naming and process tree, `--dangerously-skip-permissions` for Allow all, the keyring on a
headless Linux box; live test `live_acp_agy`. Touches `adapter/acp/agy.rs`, RUNBOOK.

## T80. The hub

`POST /api/projects/{id}/sessions` and `hqc_spawn` take `runtime: "acp"` with `acp_agent`,
checked against the worker's `hello` (the agent exists, its `environments` hold the folder's) for
an early `400`; the worker decides finally. The session's model list and current options from
`session.state` (proto: `config_options` with the `model` and `thought_level` choices), kept in
memory like `command_info`; `PATCH model` on an ACP session goes to the worker as now. A
`runtime` change of an existing session stays as SPEC §3 says (a new runtime session, recap
offered), not built here. Touches `crates/hqc-hub/src/api*.rs`, `delivery.rs`, proto.

## T81. The client

The runtime picker in the new session window (only agents of the folder's worker and
environment), the model and effort menus from the session's options, Ask / Auto / Allow all with
the agent's names in the tooltip, the approval card for an ACP tool call, and the Claude-only
controls hidden for an ACP session (list in the decisions). Settings → Workers lists each worker's
agents with version and environments, and "signed out" with the command to run. Touches
`client/src/` (new session window, chat header, approval card, settings), `client/mock`.

## T82. SPEC and RUNBOOK

SPEC §8.2 rewritten from the generic part above; §8.2.1 marked as the old in-process Hermes until
Hermes is redone; §4 `usage` for ACP; §14 `acp_agents`; §3 Worker capabilities; RUNBOOK: install
Node and codex-acp, sign in Codex and agy on the machine, `worker.toml` examples, the warning on
agy and Google's consumer terms (a spare account is safer), and that the worker's `deny` reaches
only the calls the agent asks about.

## Later

- **Hermes** on the same adapter: a Linux worker for Hermes, a new §8.2.1 for the gateway-owned
  Hermes v0.22+ (ACP is a viewer of the profile's daemon, environments map to profiles), steer.
- Sign-in from hqc (Codex's device code would fit the relay of 0.185); Codex's plan windows in the limit bar; the hub's skills for
  Codex and agy (both read `SKILL.md` folders) once a skill is wanted there.
