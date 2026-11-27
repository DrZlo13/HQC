# Codex App Server: the protocol hqc speaks (SPEC §8.6)

The worker's Codex adapter (`crates/hqc-worker/src/adapter/codex.rs`) speaks the JSON-RPC
protocol of `codex app-server` of **Codex 0.172.x** (`SUPPORTED` in the adapter). The protocol is
experimental and changes between releases, so the worker refuses another version at its start
(`hqc-worker` logs it, and `hello` names no `codex`).

The schema of that version is the one published in the Codex repository at the tag
`rust-v0.172.0`, `codex-rs/app-server-protocol/schema/json/codex_app_server_protocol.v2.schemas.json`
(sha256 `0acee8c04a62c5f78c81d89afae41384833043a1908dee07efb22e4de69df46c`). The same file comes
out of the installed binary:

```sh
codex app-server generate-json-schema --out /tmp/codex-schema
shasum -a 256 /tmp/codex-schema/codex_app_server_protocol.v2.schemas.json
```

To move to a new Codex version: generate the schema of it, diff it against the one of the
pinned version for the methods and fields below, adjust the adapter and its fake App Server in
`adapter/codex/tests.rs`, run the live check (T83), then move `SUPPORTED` and the hash here.

## What the adapter uses

Messages carry no `"jsonrpc"` field.

- Client requests: `initialize {clientInfo}` then the `initialized` notification;
  `account/read` (`requiresOpenaiAuth`, `account`); `model/list` (`data[].model`, `displayName`,
  `isDefault`, `hidden`, `supportedReasoningEfforts[].reasoningEffort`); `thread/start {cwd,
  approvalPolicy, sandbox, model?, config}`; `thread/resume {threadId, cwd, approvalPolicy,
  sandbox, model?, config, excludeTurns: true}`; `turn/start {threadId, input: [{type: "text",
  text, text_elements: []}], approvalPolicy?, sandboxPolicy?, model?}`; `turn/steer {threadId,
  expectedTurnId, input}`; `turn/interrupt {threadId, turnId}`.
- `approvalPolicy`: `on-request` (Ask, Auto) or `never` (Allow all); `sandbox`: `read-only`,
  `workspace-write`, `danger-full-access`; `sandboxPolicy` on a turn: `{type: "readOnly"}`,
  `{type: "workspaceWrite"}`, `{type: "dangerFullAccess"}`, sent only when the mode changed.
- `config.mcp_servers.<name> = {command, args, env_vars: ["HQC_SESSION_TOKEN"]}`: hqc's servers,
  the token taken by name from the App Server's environment (`env_vars`, `RawMcpServerConfig` in
  `codex-rs/config/src/mcp_types.rs` of the tag), so no config Codex may keep holds it; and
  `config.shell_environment_policy.exclude = ["HQC_*"]` **[verify that `thread/start`'s `config`
  takes both as `-c` would, and that the partial policy keeps the user's own]**.
- Notifications read: `turn/started`, `turn/completed` (`turn.status`: `completed`,
  `interrupted`, `failed` with `error.message` and `error.codexErrorInfo`, `unauthorized` read
  as signed out), `item/started` and `item/completed` (`commandExecution`, `fileChange`,
  `mcpToolCall`, `webSearch`, `reasoning`), `item/agentMessage/delta`,
  `item/reasoning/summaryTextDelta`, `turn/plan/updated`, `thread/tokenUsage/updated` (`last`,
  `total`, `modelContextWindow`), `thread/name/updated`, `serverRequest/resolved`. A
  notification of another `threadId` (a subagent's) is not the session's.
- Server requests answered: `item/commandExecution/requestApproval` and
  `item/fileChange/requestApproval` (`decision`: `accept`, `acceptForSession`, `decline`,
  `cancel`), `item/tool/requestUserInput` (`answers.<id>.answers[]`),
  `mcpServer/elicitation/request` (declined). Others get "method not found".

## Not used yet

`account/rateLimits/*` (the plan's 5-hour and weekly windows: `worker.limits` is the account of
the worker's Claude Code, and a second account needs a field of its own), `thread/fork`,
`review/start`, `account/login/start` with `chatgptDeviceCode` (signing in from hqc), the
subagents' threads.
