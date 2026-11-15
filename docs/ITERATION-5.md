# Iteration 5: tasks

Goal (SPEC §19.5): other models for Claude Code, picked per session the way a Claude model is
picked now. A **provider** is an Anthropic-compatible endpoint (`/v1/messages`) with one model
behind it: a local model behind gpu-arbiter, a GLM at z.ai, an Anthropic API key. Claude Code
stays the harness: the mirror, permissions, subagents, the hqc tools, forks and moves work as
they do, since for hqc it is the same CLI with another `ANTHROPIC_BASE_URL`.

Criterion: from a phone, start a session on home-mac with "Qwen 3.8 (local)" picked in the model
menu; the agent reads and edits files of the repository and runs a command in Auto; switch the
same session to Opus and back, the conversation goes on; a worker that does not allow the
provider does not offer it; a worker whose own environment sets `ANTHROPIC_BASE_URL` (a gateway
of the owner's) and a session without a provider run exactly as before.

Why this and not ACP first (2026-11-15): Claude Code over `qwen3.8-27b` through gpu-arbiter's
`/v1/messages` was checked end to end (tool calls, tool results, streaming, prefix cache, auto
mode's classifier, compaction at the window it assumes) and the user found it good. A provider
is a small change in hqc; an ACP adapter is a new runtime with its own mirror, permissions and
subagents. ACP and Hermes move to iteration 6, sharing to 7 (§19).

Not in this iteration: the pool of subscriptions of issue #43. A hub that holds several
accounts' OAuth tokens and routes requests through them against their limits is what Anthropic's
terms rule out for Claude Code (consumer OAuth is for the purchaser's own first-party use;
routing third-party traffic against subscription limits and proxying through consumer
subscriptions are prohibited). An owner's own gateway set in the worker's environment keeps
working (below).

## Decisions

- **The description is on the hub, the permission on the worker.** A provider's endpoint sees
  the agent's whole context and decides which tools it calls, and in Auto and Allow all the
  safety classifier runs on the same endpoint: a hub that could point a worker at any endpoint
  would get an agent that runs whatever the worker's `deny` lets through. So the hub keeps the
  owner's providers (name, label, `base_url`, model, context window: nothing secret) in one
  place, and a worker runs a provider only if its `worker.toml` names it with the same
  `base_url`; a key, if the endpoint needs one, is only in `worker.toml` (§2.1, §14).
- **No provider, no change.** A session without a provider starts with the CLI's environment
  exactly as now: the worker's own `ANTHROPIC_*` variables and the environment's `env` pass as
  they do (§8.1). An owner whose workers reach Anthropic through a gateway of their own keeps it
  without touching anything, and may add that gateway as a provider to pick it per session.
- **One model per provider.** Every model name the CLI uses (the main model, the `opus`,
  `sonnet`, `haiku` and `fable` aliases, the small fast model, subagents) goes to the provider's
  model: a local server that swaps models on one GPU must not be asked for another. Two models of
  one server are two providers.
- **Switching a provider restarts the process** with `--resume` (an environment cannot change
  in a running CLI); the prompt cache of the new endpoint starts cold. A model switch within
  Claude models stays as now.
- **The session's title** is still made by the worker's own `claude` with Haiku (§8.1 title
  generation), not by the provider.
- **Cost.** The CLI prices an unknown model at a default rate (`costBasis: unknown`): a
  provider session's header and usage lines show tokens, not dollars.

## T54. Providers on the worker

Sections: §2.1, §5.1 (`hello`), §5.3 (`session.start`), §8.1, §14.
- `worker.toml`:
  ```toml
  [providers.qwen-local]                  # the provider's name on the hub
  base_url = "http://192.168.1.229:8082"  # must equal the hub's description
  # api_key = "..."                       # for an endpoint that needs one; never leaves the machine
  ```
- `hello` carries `providers[]`: the names the worker allows (never `base_url` or keys).
- `session.start` gets `provider?` {name, base_url, model, context_tokens?}. The worker runs it
  only if `name` is allowed and `base_url` is the one in `worker.toml`; then the CLI gets
  `ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN` (the key, or a placeholder), `ANTHROPIC_MODEL`,
  `ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU,FABLE}_MODEL`, `ANTHROPIC_SMALL_FAST_MODEL` and
  `CLAUDE_CODE_SUBAGENT_MODEL` set to `model`, `CLAUDE_CODE_MAX_CONTEXT_TOKENS` from
  `context_tokens`, and `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`, on top of the
  environment's `env`. Otherwise it starts without a provider and writes `system
  provider_refused` with the reason (not allowed, another `base_url`).
- Without `provider` the CLI's environment is what it is today.
- `hqc-worker env` lists the allowed providers (keys as `***`).
Acceptance: the common checks; worker tests: the variables of an allowed provider, a refused
one (unknown name, another `base_url`) starts without it, the key never in `hello` or logs, and
a session without a provider passes the worker's own `ANTHROPIC_BASE_URL` unchanged; a live
run against a local endpoint behind `HQC_LIVE_CLAUDE=1`.

## T55. Providers on the hub

Sections: §3 (a new object Provider, Session `provider_id`), §6 (REST), §5.3.
- A provider of the owner: `name` (a slug, unique per owner), `label`, `base_url`, `model`,
  `context_tokens?`. `GET/POST /api/providers`, `PATCH/DELETE /api/providers/{id}`; a provider
  that sessions use is not deleted (`409`).
- A session's `provider_id?`: set at creation (`POST /api/sessions {provider_id?}`) and by
  `PATCH /api/sessions/{id} {provider_id}` (null: back to Claude Code's default); only a
  provider the session's worker allows (`hello.providers`), else `422`. A change of a running
  session restarts its agent (`session.kill`, then `session.start` with the new provider), as
  the restart of SPEC 0.130 does.
- `session.start` carries the provider's description; the session object shows `provider`
  {id, label}.
Acceptance: the common checks; hub tests: the API and its owner checks, a session refused a
provider its worker does not allow, a change restarts the agent with the new `session.start`,
a worker's new `hello` without the provider leaves its sessions on it but offers it no more.

## T56. The model menu and Settings

Sections: §6 client layout.
- Settings has a section "Models": the owner's providers, each with its fields and the workers
  that allow it (from their `hello`); add, edit, delete.
- The model menu of a session (the new-session form and the chat's menu) lists, below the Claude
  models, the providers the session's worker allows; picking one asks once ("The agent restarts
  on <label>; its context is read again"), like the switch to Auto.
- A provider session's header names the provider instead of the model and shows no cost; its
  usage lines show tokens only.
Acceptance: the common checks; client tests: the menu lists only the worker's providers, a pick
sends the `PATCH` after the confirmation, the header and the usage lines of a provider session.

Order: T54 and T55 can run in parallel once `session.start.provider` and `hello.providers` are
agreed (this file); T56 needs T55. The `local-qwen` environment on home-mac (a binding of one
folder, 2026-11-15) is the experiment this replaces: it goes when T54 is in.

Notes for the RUNBOOK: on macOS the worker needs Local Network access (System Settings →
Privacy & Security → Local Network → `hqc-worker`) to reach an endpoint on the LAN; without it
the CLI fails with `EHOSTUNREACH`. A rebuilt `hqc-worker` may need it granted again.
