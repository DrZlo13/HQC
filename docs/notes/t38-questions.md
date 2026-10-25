# T38: a permission mode per session — decisions and questions

SPEC 0.42 (§2.1, §3 Session, §5.3, §5.4, §6, §7, §8.1, §10.1, §14). Live `claude` runs: two of
the three allowed (the spike `tmp/spike-t38-switch` and `live_permission_mode_switch`), both
`claude-haiku-5-5`, `--max-turns 2`, Claude Code 2.1.308. Findings with the frames:
`docs/notes/claude-control-protocol.md`, "Switching the permission mode of a running process".

## 1. The [verify]: runtime switch or restart

Claude Code 2.1.308 takes `control_request {subtype: set_permission_mode, mode}` in `-p`
stream-json and confirms it three ways: `control_response {mode}`, `system/status
{permissionMode}` and `permissionMode` of the next `system/init`. The next tool call is decided
in the new mode (`touch` ran without `can_use_tool` in auto and was asked about after the switch
back). So the worker switches with the control request and never restarts for it; the restart
fallback was not built (no user for it on 2.1.308). If a future CLI refuses the request, the
adapter logs it, the session keeps running in its mode (which `session.state` keeps reporting),
and the next process start uses the new flag — the session shows "Ask (Auto asked)" meanwhile.
A version that silently ignored the request would look the same.

`ask` is sent as `default`: 2.1.308 accepts it although the process was started with
`manual`, and reports `manual` as `default` in any case.

## 2. Decisions

- **Field names.** `session.start.mode` is already `new`/`resume`, so the field is
  `permission_mode` everywhere. The session has two: `permission_mode` (what hqc asks for;
  null — the worker's `[permissions] mode`) and `effective_permission_mode` (what the worker
  last reported). The client shows the effective one and falls back to the asked one before
  any report.
- **A new frame** `session.permission_mode {session_id, permission_mode}` → `ack` rather than
  a second `session.start`: `session.start` for a running agent is acked without touching it
  (SPEC §5.3) and issues a new token. A `session.start` whose mode differs from the running
  agent's still switches it, so a switch made while the worker was offline is not lost when the
  process survived the disconnect.
- **Mid-turn.** The CLI applies the mode at its next permission check, so the worker switches at
  once, in a turn too; the "between turns" rule of the task was for the restart fallback. An
  approval card already open stays open and is answered as usual.
- **Who reports the mode.** The supervisor sets it from the flag when a process starts and
  then takes what the runtime reports; a switch is reported only when the runtime confirms it.
  `session.state` repeats it like `title`, so the hub keeps the last one while the agent is
  stopped.
- **`allow_auto` with `mode = "auto"`** is a config error: a worker cannot both default to
  auto and refuse it.
- **Default of the new-session form** is "The worker's default" (sends nothing), not Ask: a
  worker with `mode = "auto"` keeps working as before T38 for sessions created without a
  choice. The hub does not know the worker's default (`[permissions]` is not in `hello`), so
  the header shows nothing until the first report.
- **Who may switch.** `auto` only by the worker owner (`403` otherwise), as `auto_approve`;
  `ask` by whoever may manage the session. At creation the creator is always the worker owner
  (`new_session` requires it), so no extra check there.
- **`hqc_spawn`** stores `permission_mode: ask` explicitly (not null), so a worker with
  `mode = "auto"` does not run spawned sessions in auto either. Its arguments are
  `deny_unknown_fields`, so `permission_mode` in a call is `invalid_args`.
- **Peer messages** never touch the mode. Since T39 the turn they start is capped instead:
  it runs in auto only if every session up the chain does (`docs/notes/t39-questions.md`).
- **`auto_approve`** is left as it is: in auto no approval reaches the hub, so the two do not
  conflict; `AskUserQuestion` still reaches the user in auto.
- **The summary** (`session.summary`) carries both fields, so the header follows the worker's
  confirmation without a reload.

## 3. Tests

| what | where |
|---|---|
| the flag on the command line (ask mode / `auto`, restricted too, `--settings` kept) | `adapter::claude_code::tests::command_line_carries_permissions` |
| the control request and the reported modes on the fixture; the CLI's answer and a refusal | `…::permission_mode_switch_in_the_stream`, `…::adapter_switches_a_running_process` |
| `allow_auto`, `mode_for`, the config error | `config::tests::session_permission_mode` |
| start in auto, switch, next start in the switched mode, reported mode, unknown session | `tests/worker.rs::permission_mode_is_started_switched_and_reported` |
| `allow_auto = false` runs ask at start and on a switch | `tests/worker.rs::allow_auto_false_runs_ask` |
| create with a mode / without, bad value, reported mode | hub `tests/t38.rs::a_session_is_created_with_a_mode_and_shows_what_the_worker_runs` |
| switch → frame, worker keeps ask, next `session.start`, offline | `…::a_switch_reaches_the_running_agent_and_the_next_start` |
| spawn without inheritance, no argument, peer message | `…::spawned_sessions_and_peer_messages_do_not_carry_auto` |
| another owner's session (404) and worker (auto 403, ask 200) | `…::only_the_worker_owner_switches_to_auto` |
| the mark, the switch with and without confirmation, the summary | `client/tests/chat.test.tsx` "permission mode" |
| the form field against the mock hub | `client/tests/projects.test.tsx` |
| live: ask → auto → ask in one process | `crates/hqc-worker/tests/claude_live.rs::live_permission_mode_switch` |

## 4. Not verified / open questions

1. **A switch in the middle of a turn** was not run live (both runs switched between turns).
   The CLI's input loop handles control requests during a turn (`interrupt` relies on it), so
   it is expected to work.
2. **`--restricted` with `--permission-mode auto`** (T12's open point) is still not run; a
   session without a workspace cannot be created through the API yet.
3. **Showing the worker's default before the first start** would need `[permissions]` in
   `hello` (T12 left it out). Not added: no screen needs it yet beyond this header.
4. **An audit line** in the feed ("switched to Auto by …") was not added; the header shows the
   current mode. With shared projects (§11) a collaborator may want to see who switched it.
