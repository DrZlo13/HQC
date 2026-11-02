# T43: decisions

Environments and bindings on the worker, SPEC 0.56.

## Protocol

- **`session.start` loses `environment`.** The worker resolves the environment itself on every
  start, so a hub value has no use (not even as a check: a hub that computed it differently would
  only block sessions). A new worker ignores the field from an older hub (frames are not
  `deny_unknown_fields`); an **older worker refuses a `session.start` without it**, so update the
  workers before (or with) the hub.
- **`hello.environments` are objects** `{name, allow_local_mcp?, permissions {unblock?, allow?,
  deny?}}`; a plain string (older workers send `["default"]`) still reads, so a new hub accepts
  an old worker. `default` is not listed: it is implied and cannot be defined in `worker.toml`.
  Neither the values nor the names of `env` are sent (no user of the names yet).
  `env_bindings` are `{path, environment}`; the Hermes profiles come with iteration 5.
- One lookup for both sides: `hqc_proto::frame::binding_for` / `environment_for` (longest binding
  by path components). The worker canonicalizes the binding paths again on every lookup, so a
  binding that names a symlink, or a folder made after the start, still matches.

## Worker

- The effective rules are computed once per environment at load (`Permissions::for_environment`)
  and travel in `StartSpec.environment`; the adapter builds `--settings` and its `Policy` from
  them per process start (the factory no longer holds one policy for every session).
- `unblock` compares rules as written: `Bash(ssh:*)` lifts `Bash(ssh:*)` only. An environment's
  rule already in the worker's list is not added twice.
- `env` goes on top of the child allowlist and may replace an allowed variable (`PATH`);
  `CLAUDE_CODE_ENTRYPOINT` and `MCP_TOOL_TIMEOUT` are set by the worker after it. `HQC_*` names,
  names that are not `[A-Za-z_][A-Za-z0-9_]*`, and NUL in a value are config errors. `EnvVars`
  prints `***` in `Debug`, so a `StartSpec` or `Config` in a log shows names only.
- A binding to `default` is allowed (a subfolder of a bound folder given back); a binding to an
  undefined environment and the same path bound twice are config errors. `ssh_keys` and
  `launch` are unknown fields until T44 / later.
- `permissions.inspect`: the `worker.toml` source lists `[permissions]` without the unblocked
  rules; the environment is a source of its own, `worker.toml [environments.<name>]`, placed
  right after it. The unblocked rules themselves are not on the rules screen (no place in the
  mockups); `hqc-worker env which` shows them as `(lifted)`.
- `hqc-worker env` reads `worker.toml` like `run` (`HQC_WORKER_CONFIG` / `$HQC_HOME`) and prints
  plain text; editing stays in the file, a change needs a worker restart.

## Hub

- No migration: `workspaces.environment` existed (default `'default'`). It is set from the
  worker's stored `env_bindings` when a workspace is created and recomputed for all the
  worker's workspaces on every `hello`. Clients see it on their next load (no push).

## Client

- The rules screen header shows the workspace's environment. The project settings row shows a
  tag `env <name>` only for a non-default environment (a second "default" tag next to the
  default-workspace tag would read as the same thing). The session header has no place for it
  in the mockups, so nothing there. No client test for the project-settings tag: the dev mock
  has no bindings; the rules screen test covers the environment and its source.

## Live check

One run of `claude_live::live_environment_ssh` (two `claude -p`, `claude-haiku-5-5`,
`--max-turns 2`, `tmp/spike-t43-*`, 2026-11-01): in the folder bound to an environment that
unblocks `Bash(ssh:*)`, `ssh -o BatchMode=yes -o ConnectTimeout=3 hqc-t43.invalid true` came as
an `approval.request` (answered deny, nothing ran); in the other folder of the same config the
CLI refused it by the worker's deny (`system policy_denied`), no request.
