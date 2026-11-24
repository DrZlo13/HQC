# Iteration 7: tasks

Goal (SPEC §19.7): the owner adds a remote MCP server (one reached by URL: a wiki, search,
GitHub, Linear) once in hqc, and agents of the projects it is given to get it on every worker,
under its usual name, while its token or sign-in never leaves the hub. Issue #59, with one change
of design: the hub does not hand the server's address and headers to the worker, it relays the
calls itself.

Criterion: add GitHub's remote MCP server with a token in Settings → MCP servers and give it to
one project; a new session of that project on home-mac lists `mcp__github__*` among its tools and
calls one, with the usual permission card and "Always" writing an `mcp__github__…` rule; a
session of another project has no `github`; the token is in no worker's file, process
environment or command line, nor in the transcript, the feed or the logs; add a server that
signs in with OAuth (Linear or Notion) from a phone and call it from a session; take the token
away (revoke it at the service) and the next call says to sign in again, and Settings says the
same.

Not in this iteration: MCP servers that run on the machine (`stdio`: KiCad, Fusion 360, a phone),
which come later (below); skills from the hub (#62); a server's tools added to a running
process without a restart (`mcp_set_servers`); MCP prompts and resources; a server that only
speaks the old HTTP+SSE transport.

## Decisions

- **The hub relays, the worker never gets the token.** #59 proposed putting the server's URL and
  headers into `session.start` and a 0600 config file on the worker. That puts the token within
  reach of the agent: it runs as the same user, reads the file, `ps` shows a command line (keys
  of Tavily and context7 already showed up in a transcript that way), and every worker holds
  every token. Instead the worker gives the CLI, for each server, a stdio process of its own,
  `hqc-worker mcp --upstream <name>`, as it gives `hqc` now. That process sends each request
  over the session's existing connection to the hub. The hub calls the server with the token it
  keeps and returns the answer. The token stays on the hub: the user accepted that on
  2026-11-19. The workers need no route to the services.
- **The server keeps its own name.** The CLI names tools `mcp__<config key>__<tool>`, and the key
  is the server's name in Settings. A tool through the hub is therefore `mcp__github__create_issue`,
  exactly as with a direct connection. READMEs, skills, `CLAUDE.md` and rules in `settings.json`
  or `worker.toml` that name it keep working. A name is `[a-z0-9_-]{1,32}` and not `hqc`.
- **The worker decides whether it takes them.** In line with §2.1, `hub_mcp = true` in an
  environment of `worker.toml` (off by default) makes the worker add the hub's servers for
  sessions in that environment's folders. Without it the worker ignores them and the agent starts
  as now. Restricted sessions (§8.4) and sessions whose owner is not the worker's owner get none.
  The worker's `deny` and the CLI's permission requests apply to these tools as to any MCP tool:
  the hub adds no approval of its own.
- **Which sessions get which server.** Each server has "all projects" or a list of projects, as
  #59 proposed. The hub puts the names of the session's servers into `session.start`; a change
  reaches a session at its next process start.
- **The hub is the MCP client.** It speaks the Streamable HTTP transport (MCP 2025-06-18 and
  2025-11-25: `POST` with JSON or an SSE answer, `Mcp-Session-Id`, a new session on `404`).
  - **One MCP session per hqc session and server**, as today, where every CLI process opens its
    own. A server may keep state in it: gitmost keeps its "new comments" mark there, and curator
    writes the session id into its audit log. The hub opens the session on the first call of that
    hqc session to that server. It closes it (`DELETE` with `Mcp-Session-Id`) when the agent's
    process stops, or after an hour without a call.
  - `serverInfo`, `instructions` and `tools/list` are the same for every agent, so the hub caches
    them per server. The cache lasts until the server says `notifications/tools/list_changed` or
    an hour passes. The relay gives the CLI the server's `instructions`, which the CLI puts into
    the agent's context: 7 of vvzvlad's 9 servers have them, such as portainer's "call
    `get_guidance` first".
- **Two ways to sign in.**
  - Static headers, one or more (`Authorization: Bearer …`, or any name), for a personal token
    (GitHub PAT, context7, Tavily). Portainer needs two: `Authorization` for the MCP server and
    `X-Portainer-API-Key`.
  - OAuth 2.1 with PKCE, as MCP's authorization spec has it:
    - discovery through the server's `401` and its protected-resource metadata (RFC 9728), then
      the authorization server's metadata (RFC 8414);
    - dynamic client registration (RFC 7591) when the server offers it, else a client id (and
      secret) the owner enters;
    - the redirect goes to `https://<hub>/api/mcp/oauth/callback`, behind Access like the rest of
      `/api/*`.

  The hub refreshes access tokens and stores the rotated refresh token. When a refresh fails, the
  server shows "Sign in again" in Settings, and a tool call answers with an error that says so.
- **Secrets at rest [decision].** Header values, client secrets and OAuth tokens are encrypted in
  the database with a key from the hub's environment (`HQC_MCP_KEY`, 32 bytes, XChaCha20-Poly1305
  per value). A database backup alone then gives nothing. Without the key, the hub keeps no MCP
  server that needs a secret: `422 mcp_key_missing` on adding one. The client never reads a
  value back once stored. Values appear in no log line, event or error.
- **What passes through.** `tools/list` and `tools/call` with their content blocks (text,
  image, resource links, structured content) as the server returns them, and
  `notifications/cancelled` from the CLI. No progress notifications in this iteration: a server
  sends them to keep an HTTP call alive past the CLI's 300 s of silence, and a stdio relay does
  not need that.
- **A large result comes whole.** Real results are large: a screenshot in base64 up to 1.5 MB, a
  page read up to 5.8 MB, a whole publication of 2 MB. A cut JSON or base64 is useless, and for an
  over-limit text result the CLI already saves it to a file and gives the agent the path. So
  nothing is cut:
  - a result up to 512 KiB goes in the frame;
  - a larger one, up to 64 MiB, the hub stores as a short-lived artifact of the owner (kind
    `mcp_result`, removed after the worker downloads it or after an hour), and the frame carries
    its id and sha256;
  - the relay downloads it as it downloads attachments and hands the CLI the result whole;
  - over 64 MiB the call is a tool error that says how large the result was.
- **A timeout per server.** Some calls are slow by design: vvzvlad's research tries up to 13
  providers, 25 s each, and 5 % of its `search_and_read` calls take over 120 s. Each server has a
  timeout, 300 s by default, 30 s to 1800 s, so that a server with its own deadline (portainer
  waits 120 s on Portainer) answers with its own error first. The hub's timeout error says that
  the call may still have completed on the server, and to check before retrying.
- **Errors are the tool's, not the session's.** If the server is unreachable, returns 5xx or has
  to be signed in again, the call is a tool error with hqc's text, so the agent tells the user. A
  server that fails `initialize` at a session's start is still listed, and its calls answer with
  the same error. The other servers and `hqc` are not affected.

## T63. The relay on the worker (done, SPEC 0.235)

Sections: §5.3, §5.4, §7.2, §14.
- `worker.toml`: `hub_mcp = true` in `[environments.<name>]`; `hqc-worker env` and `env which`
  show it.
- `session.start.mcp_servers?: [{name}]`, names only. The worker takes them only where
  `hub_mcp` holds and the session is the owner's and not restricted. Otherwise it drops them and
  logs the names.
- `--mcp-config` gets, beside `hqc`, one entry per name:
  `{"command": "<hqc-worker>", "args": ["mcp", "--upstream", "<name>"]}`. The process reaches
  the worker's daemon over its socket with the session token, as `hqc-worker mcp` does.
- The stdio process:
  - implements `initialize`, giving the hub's cached `serverInfo` and `instructions` of the
    server;
  - forwards `ping`, `tools/list` and `tools/call`, and `notifications/cancelled` for an open
    call;
  - answers anything else with `-32601`.
- A worker → hub frame `mcp.upstream {request_id, session_id, server, method, params}`.
  - It is answered `result {request_id, upstream?: <the JSON-RPC result>, upstream_artifact?:
    {id, size, sha256}}`, or `error` with a code: `mcp_unavailable`, `mcp_sign_in`,
    `mcp_timeout`, `mcp_too_large`.
  - It is never queued in the outbox: an upstream call while the hub is unreachable fails at
    once.
  - `mcp.upstream_cancel {request_id}` carries a cancel.
  - The worker's report that the agent's process stopped also lets the hub close that session's
    MCP sessions.

Acceptance: the common checks. Worker tests:
- without `hub_mcp`, the names are dropped and only `hqc` is in `--mcp-config`;
- a restricted session and a foreign session get none;
- `--mcp-config` holds no URL or header;
- the stdio process round trip against a fake hub (`initialize` with `instructions`,
  `tools/list`, `tools/call`, a cancel, an error code shown as a tool error, a result through an
  `mcp_result` artifact);
- a live test behind `HQC_LIVE_CLAUDE=1` (`claude-haiku-5-5`, max-turns ≤ 2): a fake upstream
  tool shows as `mcp__<name>__<tool>` in `system/init`.

## T64. Servers on the hub: storage, the client, static headers (done, SPEC 0.235)

Sections: §3, §5.3, §6, §20.
- Migration:
  - `mcp_servers`: owner, name, url, auth kind, encrypted headers (one or more), timeout, all
    projects or not, status, last error, and the cached `serverInfo`, `instructions` and tools;
  - `mcp_server_projects`;
  - the MCP sessions are kept in the hub's memory, not in a table: after a restart of the hub the
    next request opens a new one, which a server must handle anyway (`404`).
- REST, owner only:
  - `GET/POST/PATCH/DELETE /api/mcp/servers`;
  - `POST /api/mcp/servers/{id}/test`: `initialize` and `tools/list` now, returning the status
    and the tools' names.
- Rules for a server:
  - only `https` URLs (`http` only for a loopback host in dev);
  - the name is unique per owner;
  - header values are write-only.
- The hub's Streamable HTTP client:
  - the server's timeout; the calls are capped by the per-session and per-worker rates of
    `mcp.call` (SPEC §20), not by a count in flight;
  - a large result stored as an `mcp_result` artifact (above);
  - no redirects to another host;
  - private and loopback addresses refused unless `HQC_MCP_ALLOW_PRIVATE=1`, so a server's URL
    cannot reach the hub's own network by surprise **[decision]**.
- `session.start.mcp_servers` from the session's project. The `mcp.upstream` relay checks:
  - that the session belongs to the sending worker;
  - that the server is among the names its last start carried;
  - the owner.

Acceptance: the common checks. Hub tests against a fake MCP server:
- list and call through a fake worker;
- a server of another project is refused;
- the header value is in no row unencrypted, no event, no log line (a test greps the captured
  log);
- without `HQC_MCP_KEY`, adding a server with a header is `422`;
- a private address is refused;
- a 3 MiB result reaches the fake worker whole through an `mcp_result` artifact, which is gone
  afterwards;
- over 64 MiB, a tool error;
- the timeout's error text;
- two hqc sessions get two MCP sessions, and stopping one process closes its own;
- `instructions` reach the relay's `initialize`;
- a new MCP session after the server's `404`.

## T65. OAuth (done, SPEC 0.235)

Sections: §6, §20.
- `POST /api/mcp/servers/{id}/sign_in` returns the authorization URL, after discovery and dynamic
  registration or the owner's client id.
- `GET /api/mcp/oauth/callback` stores the tokens and closes the window back to Settings.
- Refresh before expiry and on a `401`. A failed refresh sets the status `sign_in` and the
  calls' error.
- Sign out removes the tokens.
- The state parameter is bound to the owner and expires in 10 minutes.

Acceptance: the common checks. Hub tests against a fake authorization server:
- discovery from a `401`;
- dynamic registration;
- a manual client id;
- PKCE checked;
- a wrong or expired state refused;
- refresh with a rotated token;
- a failed refresh shown on the next call.

By hand: Linear or Notion signed in from a phone.

## T66. The client (done, SPEC 0.235)

Sections: §6 client layout.
- Settings → MCP servers:
  - the list with each server's status (ok, failing with the error, sign in) and its tools;
  - add: name, URL, timeout, sign in with none, headers (name and value, the value a password
    field, more than one) or OAuth (with an optional client id and secret);
  - "Sign in", "Test", delete;
  - "All projects" or the chosen projects.
- A project's settings list the servers it gets.
- The mock and its routes.

Acceptance: the common checks. Client tests:
- the form;
- a header value never shown again nor kept in a draft;
- the statuses;
- the project picker.

Order: T63 and T64 together (the frames are agreed here), then T65 and T66.

## Later: MCP servers on the machine (a sketch)

What MCPProxy does for local servers (looked at on 2026-11-22, not adopted) fits the rule that
the worker does not trust the hub:
- the hub may propose a `stdio` server to a worker;
- the worker starts it only after its owner approves it there;
- a changed command or changed tool descriptions (a hash of them) need approval again;
- the worker reports which servers it has, so a session goes to a machine that has KiCad, or does
  not get it, instead of failing.

To be planned with skills (#62).

## Done so far

T63 and T64 in #164 (SPEC 0.235). Proto: `session.start.mcp_servers`, `mcp.upstream`,
`UpstreamAnswer`, the error codes `mcp_*`, `valid_server_name`, the API types `McpServer*`.
Worker: `hub_mcp` (`config.rs`, `env_cmd.rs`), the relay mode of `hqc-worker mcp --upstream`
(`mcp/server.rs`), `ToDaemon::Upstream` (`mcp/daemon.rs`, the large result downloaded and
checked), `HubLink::mcp_upstream` and the online flag (`link.rs`, `connection.rs`),
`McpCalls::set_upstreams`/`fail_upstream`, the `--mcp-config` entries (`adapter/claude_code.rs`).
Hub: migration `0057_mcp_servers`, `mcp_servers.rs` (sealing, the Streamable HTTP client, the
relay, REST), `artifacts::store_for_session`/`forget`, the frames in `worker_ws.rs`, the names in
`delivery.rs`, `HQC_MCP_KEY` and `HQC_MCP_ALLOW_PRIVATE` (`config.rs`). Tests: hub
`tests/mcp_servers.rs` (a fake Streamable HTTP server: the relay with the token sealed, an SSE
answer, a 3 MiB result as an artifact, one MCP session per hqc session closed when the process
stops, the refusals, the checks of what the owner gives, no key, a private address) and
`mcp_servers::tests`; worker `tests/mcp.rs` (`a_relay_of_the_hubs_server_goes_to_the_hub_and_back`,
`without_hub_mcp_no_server_is_relayed`), `config::tests::the_hubs_mcp_servers_only_where_hub_mcp_is_on`,
`adapter::claude_code::tests::the_hubs_mcp_servers_are_relays_under_their_own_names`.

T65 and T66 in #164 too. Hub `mcp_servers/oauth.rs` (discovery, registration, PKCE, the
callback, sealed tokens, refresh with a lock per server), `credentials` and one retry with a
forced refresh in the relay; test `oauth_signs_in_with_pkce_and_refreshes_the_token` against a
fake authorization server. Client `ui/McpServers.tsx` (Settings → MCP servers, the project's
list), `api.ts`, `proto.ts`, the mock; tests `mcp-servers.test.tsx`.

Left for the acceptance by hand: GitHub with a token and Linear or Notion with OAuth from a phone.
