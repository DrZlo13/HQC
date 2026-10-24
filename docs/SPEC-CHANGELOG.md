# hqc: spec change history (old versions)

Current entries are at the end of docs/SPEC.md.

### 0.3 → 0.4 (review of similar systems)
ACP as a common adapter instead of a home-grown `tui_gateway` for Hermes; hqc MCP via
`mcpServers` in `session/new`, which closes the [verify] from 0.3 (§8.2, §7);
the `acp` runtime with `acp_agent` (§3); Claude Code stays on its native adapter (§8.2);
`client_op_id` (§6); a section on similar systems and the rejected hub-less alternative (§22);
the ACP v2 risk (§21).

### 0.2 → 0.3 (after reviews 3 and 4)
"Path → environment" bindings and `allow_local_mcp` live only in the worker's local config,
§2.1 brought in line (§14, §2.1); the worker owner pays for Hermes (§11.1);
the mirror does not emit sessions started by hqc, a deterministic `event_id` for the mirror
and import (§9.2, §3); a foreign writer is detected only on the main
user/assistant chain, a warning about VS Code's stale context (§9.3); `cwd` from the
jsonl records instead of decoding the folder name, transcripts of subagents
are ignored (§9); `in_reply_to` on `agent.message`, the answer to an ask goes by it (§4, §7.1);
two queues: input and the send buffer, `peer_prefix` in `session.input` (§7.1, §5.3);
hub → worker delivery with `delivered_at` and ack, resending after `hello`, the input queue
is not stored on the worker, mirror offsets on disk (§5.2); signed links
to the public artifact domain (§12, §20); `--strict-mcp-config` by default (§8.1);
state priority, `state` is not stored (§17.1, §3); `final` by the next
item in the stream, `--replay-user-messages` (§8.1); `allow_foreign_sessions` and
`env_bindings` in `hello` (§5.4, §3); `dir` is deleted only as a record (§13.2); rows of the
access table: clarify, delete project, unarchive, schedule, send/ask
(§11); the approval is built into the worker owner's request, the service session
moved to iteration 2 (§10.2, §19); `locked_by` on import (§9.1); the `chain`
of the current turn is given to MCP (§7.1); `always` → `session` under `--restricted` (§8.1);
the API is marked incomplete, "later" items added (§6); `reply_to` removed, `session`
is optional for `hqc_send`, `content` for `hqc_attach` (§7); protection against a forged
peer header (§4); an ask to a `locked_vscode` session is an error, to a `worker_offline` one it
waits; a `write` grant for send/ask (§7.1); `approval.cancel {worker_restart}` (§4, §17.3);
`worker_id` for any runtime without a workspace (§3); the assistant's runtime is changed
in the settings (§8.3); push goes to the owner and to those who wrote, when they have no active
subscription (§16); the stemming limitation (§15); a warning about `auto_approve` on a shared
project (§10.1); the session token: no config file, no system prompt,
the visibility limitation is stated (§7, §8.2); wording: "Limitation:", the column
"project owner", User without `owner_id`.

### 0.1 → 0.2 (after reviews 1 and 2)
Claude Code's permission mechanism via `control_request`; `owner_id` on
Session/Workspace/Grant and the "who pays" rule; outbox, ack, `event_id`, `seq` from
the hub; send/ask semantics, the peer message header, `chain`; the session token for MCP;
takeover: `--resume` keeps the uuid (verified); environments apply to Hermes through
a separate `tui_gateway` process; Access JWT verification and route separation;
threat model; state table; `approval.cancel`; the input queue; protocol version
and frame format; minimal client API, `ReadCursor`; access level table;
rules for deleting and choosing a workspace; `--restricted`; mirror by path;
address slugs; cron with a time zone; search with grants; the 100 MB limit; HTML isolation;
the `usage` event; the worker's service session; a personal project for the assistant;
definitions of terms.
