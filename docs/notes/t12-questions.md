# T12 — approval mode: decisions, questions, what was not confirmed

Task: ask only about potentially harmful actions. SPEC 0.14 (§4, §8.1, §10.1, §14, §23),
spike: `docs/notes/claude-control-protocol.md`, section "Permission policy, 2.1.308".
Live claude runs: 7 of the 8 allowed — 6 spike runs through `tools/spike/driver.py` and one
`live_permission_policy`.

## Decisions

- **`mode = "auto"` is allowed.** The spike confirmed that `--permission-mode auto` works in
  `-p` with `--permission-prompt-tool stdio`, and that `deny` from `--settings` overrides it.
  But auto **never asks**: in two runs there was not a single `can_use_tool`; the classifier
  let through `rm -rf ~/…` outside the workspace, `git push --force` and `sudo -n true`,
  because the user asked for them. So the mode is not the default, the worker logs a warning
  at startup, and the RUNBOOK says plainly that in auto the only boundary is `deny`. There is
  no marker of the classifier's decision in `can_use_tool` (there are no requests at all); it
  appears only in `tool_result_meta[].permission_decision.reason_type` (`classifier` / `mode`).
- **The CLI guarantees deny over always** (run `settings-deny`: `Bash(git push:*)` in
  `--settings` deny and `Bash(git push *)` recorded by `always` — the command was denied
  without a request). The adapter still reads the rules itself and denies a `can_use_tool`
  that matches `deny` with `system policy_denied`, without showing a card. Reasons: a hub with
  `auto_approve` would answer such a card with `once`, and the CLI may change its behaviour in
  a new version. So "always is not offered for deny" holds in the strongest form: there is no
  card at all. `always` suggestions (`updatedPermissions`) that overlap deny (e.g.
  `Bash(git push *)` with deny `Bash(git push --force:*)`) are not filtered: deny overrides
  them.
- **`policy_hint`** is `allow` | `deny`; with no match the field is absent (not `null`: the
  protocol omits optional fields; the client understands `null` too). In practice the Claude
  Code adapter sends only `allow` (under `deny` there is no card); `deny` stays in the enum
  for future adapters (ACP) and as a valid value. `allow` appears when an `allow` rule
  matches the whole call and the CLI still asked.
- **A deny is visible in stream-json** (`system/permission_denied` + `tool_result`
  `is_error` + `result.permission_denials`), so item 3 of the task is done: the adapter writes
  `system {code: policy_denied}`, and the client shows the line "Blocked by worker policy" in
  red. A classifier denial in auto (if it ever happens) is `system {code: permission_denied}`
  without that label; no such denial happened in the runs, the code was chosen by analogy.
- **A custom `deny` replaces the default set** rather than extending it: otherwise a default
  rule could not be removed (e.g. `Bash(ssh:*)` on a machine where the agent needs ssh).
  No `deny` field in the block means the default set; `deny = []` means no denials.
- **`--settings` is passed in restricted too** (run `restricted-settings`: allow `Write`
  worked, `Grep` from deny disappeared from the tool list). `mode = "auto"` in restricted
  also goes to the CLI — the combination `--restricted --permission-mode auto` was not
  checked (see below).
- **The worker validates rules more strictly than the CLI**: tool name `[A-Za-z][A-Za-z0-9_]*`
  (a trailing `*` is allowed), content non-empty, in a single pair of parentheses at the end;
  ≤ 256 bytes, no control characters. CLI help: in `-p` "Settings files that fail validation
  are silently ignored" — one bad rule could drop the whole deny.
- **The adapter's hint approximates the CLI**: `Bash(x:*)` is a prefix up to a space,
  `*` is any characters, without `*` an exact command; deny is also matched against the parts
  of a command between `; & | ` `` ` `` `$ ( ) < >`, allow only against the whole command
  without those characters. File rules (`Read(./x/**)`) and `WebFetch(domain:…)` are not
  interpreted by the adapter — the CLI applies them.

## Not confirmed / not checked

- **`curl … | sh` in manual mode**: the model refused to call it on its own (run
  `settings-deny`); the rule was checked only in auto (run `auto-deny`, `reason_type: rule`).
- **Syntax the CLI considers invalid**, and what then happens to `--settings` (the whole
  object is ignored or only the rule) — not checked, not enough runs. The CLI accepted all
  rules from the task and from the default set (deny worked).
- **`Bash(mkfs*)` does not catch `mkfs.ext4`** (`mkfs.ext4` is a separate command, the prefix
  stops at a word boundary) — that is how the adapter sees it; not checked on the CLI. The
  default set from the task was left unchanged; whether to add `Bash(mkfs*)` is a **question
  for the owner**.
- **`Bash(rm -rf /*)` catches any absolute path**, including inside the workspace (run
  `auto-deny`: `rm -rf /…/tmp/nonexistent` was denied). For the agent this means "delete
  recursively only with relative paths". Variants like `rm -fr`, `rm -r -f`, `rm -rf ~` are
  not caught by the set — it is the first line, not the boundary (SPEC §14).
- **`--restricted` + `--permission-mode auto`** was not run. The help only says that
  restricted refuses `bypassPermissions`.
- **`decision_reason` / `decision_reason_type` in `can_use_tool`** (new in 2.1.308:
  `"This command requires approval"`, `"other"`) are not used by the adapter yet.
- **`[permissions]` in `hello`** is not passed to the hub (the hub does not show the worker's
  policy); iteration 2, together with environments.
- **The live test** `live_permission_policy` ran once (green, 4.9 s). The older live tests
  (`live_repo_settings_ignored`, `live_bash_turn_with_approval`) were not rerun: their command
  line now contains `--settings` with the default set.

## Fixture sanitization

`tools/spike/sanitize.py` rewrites whole paths in strings, but `input_json_delta` in
stream-json splits a path into pieces (`"…/Users/"`, `"home-…/work/h"`). In
`stream-restricted-settings-2.1.308.jsonl` there are two such blocks; the fragments of each
block were joined into the first one and rewritten (the other fragments are empty strings).
`sanitize.py` should learn to do this itself.


Decided: the default set has `Bash(mkfs*)` instead of `Bash(mkfs:*)`; the glob also covers `mkfs.ext4`.
