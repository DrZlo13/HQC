# Iteration 9: tasks

Goal (SPEC §19.9): an agent gets the MCP servers that run on its machine (`stdio` programs such as
KiCad, Fusion 360, a phone bridge, Framelink for Figma, and local HTTP servers such as Figma's
Dev Mode at `127.0.0.1:3845`), one by one and only in the environments that name them, without
opening everything else the machine's Claude Code knows. The rest of issue #59, after the
remote servers of iteration 7.

Criterion: on drzlo-mac, `[environments.firmware.mcp_servers.figma]` runs Framelink with a token
read from Vaultwarden; a new session in the firmware folder lists `mcp__figma__*` and calls one,
with the usual permission card; it lists no `claude.ai *` connector and no server of
`~/.claude.json`; a session in another folder has no `figma`; the token is in no `hello`, log,
transcript or feed; Settings shows that `figma` is on drzlo-mac in the environment `firmware`.

Not in this iteration: a `stdio` server the hub proposes to a machine (below, "Later"); a
server's tools added to a running process without a restart; ACP agents (Hermes), which have
their own MCP setup.

## Why now

On 2026-11-25 Figma had to go in by hand: `claude mcp add` in the folder, and
`allow_local_mcp = true` in its environment, the only way today to let a session see a server of
the machine. That flag drops `--strict-mcp-config`, and checked live on 2.1.328 the agent then
gets every server the machine's CLI knows: 32 on drzlo-mac, among them the account's
`claude.ai` connectors (Gmail, Google Drive, Calendar, Slack, Notion…). One server cost the
agent of that folder the owner's mail. With `--strict-mcp-config` it gets none of them.

## Decisions (proposed)

- **The machine declares its servers, in `worker.toml`.** A local server's program, its login
  files and its tokens live on the machine, and a command the CLI starts runs outside the
  worker's `deny` and its permission requests. By §2.1 only the machine's owner may decide that,
  so the definition is local, per environment, like `env` and `ssh_keys` (§14):

  ```toml
  [environments.firmware.mcp_servers.figma]
  command = "npx"
  args = ["-y", "figma-developer-mcp@0.6.4", "--stdio"]
  env = { FIGMA_API_KEY = { vault = "figma token" } }   # or a plain value

  [environments.firmware.mcp_servers.figma-dev]
  type = "http"
  url = "http://127.0.0.1:3845/mcp"
  ```

  `type` is `stdio` (the default: `command`, `args`, `env`, `cwd`?) or `http` (`url`, `headers`
  with the same value-or-vault form). A name is `[a-z0-9_-]{1,32}`, not `hqc`, as the hub's
  servers (§7.3). The hub cannot add, change or start one.
- **They go into `--mcp-config` next to `hqc` and the hub's relays, and `--strict-mcp-config`
  stays.** So an environment gets exactly the servers it names: no `~/.claude.json`, no
  `.mcp.json` of the repository, no `claude.ai` connector. The tools are `mcp__<name>__<tool>`
  and go through the worker's `deny` and the permission cards like any MCP tool.
- **Secrets are resolved at every agent start and never sit in a file or `hello`.** A value
  `{ vault = "<entry>", field? }` is read with `rbw` as a provider's `api_key` is (SPEC 0.243).
  To keep it off the CLI's command line (`ps` showed MCP keys to an agent before, #59), the
  worker does not put the server itself into `--mcp-config` but a launcher,
  `hqc-worker mcp --local <name>`, as it does for the hub's servers (`--upstream`): the launcher
  gets the resolved command and environment from the worker's daemon over its socket and `exec`s
  the program. For `http` the launcher is a small relay that adds the headers.
- **`hello` names the servers, nothing more.** `environments[].mcp_servers: [name]`, as
  `providers` are named (SPEC 0.144). The hub keeps them per worker, and Settings → MCP servers
  shows "on the machine" beside the hub's servers: `figma` · drzlo-mac · env firmware. The hub
  routes nothing by them: a session runs where its workspace is.
- **A local server wins over the hub's server of the same name**, and the worker logs it: the
  machine decides (§2.1), and the owner gave it there on purpose.
- **`allow_local_mcp` stays as it is**, all of the machine's servers, but `hqc-worker env` and
  RUNBOOK say that this includes the account's `claude.ai` connectors, and point to
  `mcp_servers` instead.

## Open questions (for vvzvlad and the owner)

1. **Pinning the tools (from MCPProxy).** The launcher sees `tools/list`. It could keep a hash of
   the tool names and descriptions per server, and on a change refuse the server until the owner
   runs `hqc-worker mcp approve <name>` on the machine: a package update that changes a tool's
   description (a prompt injection in it) would not reach the agent silently. Worth it now, or
   later? It costs the launcher parsing the protocol instead of `exec`.
2. **Also take named servers from `~/.claude.json`?** `use_machine_mcp = ["kicad"]` would pass
   through what `claude mcp add` already set up, without repeating it in `worker.toml`. It ties
   the worker to the CLI's file format and its project scopes. Proposed: no, `worker.toml` only.
3. **`cwd` of a stdio server**: the session's folder (as the CLI does now) or the worker's home?
   Proposed: the session's working folder, as the CLI does.

## T70. The worker: `mcp_servers` per environment

`config.rs`: `[environments.<name>.mcp_servers.<server>]` (`type`, `command`, `args`, `env`,
`cwd`, `url`, `headers`), values plain or `{vault, field?}`; checked at load (names, an absolute
or `PATH` command, an http(s) URL); `hqc-worker env` and `env which` list them without values.
The `default` environment has none. `StartSpec` carries them.

## T71. The launcher and `--mcp-config`

`hqc-worker mcp --local <name>`: asks the daemon for the spec of this session's server, resolves
vault values, `exec`s a `stdio` program with its environment (the child allowlist of §8.1 plus
the server's `env`), or relays `http` with its headers. `adapter/claude_code.rs` adds an entry per
server. A server that cannot start (a missing program, a locked vault) fails that server only and
says so in a `system` line, as a refused provider key does.

## T72. `hello` and the hub

`environments[].mcp_servers` in `hello` (proto, SPEC §5.3); the hub stores them with the worker's
environments; `GET /api/mcp/servers` adds `local[{name, worker_id, worker_name, environment}]`.

## T73. The client

Settings → MCP servers: a second list "On machines" (name, worker, environment, read-only, a line
on how to add one in `worker.toml`). The project settings' MCP section shows the local ones of
the project's workers too.

## Later: a server the hub proposes

The MCPProxy idea stays possible on top of this: the hub keeps a server's description and offers
it to a worker; the worker owner accepts it on the machine (`hqc-worker mcp accept <name>`), which
writes it into `worker.toml`. Not planned until a second machine needs the same local server.
