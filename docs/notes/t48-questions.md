# T48: decisions and differences from the mockups

Permission rules in hqc, by `docs/design/redesign-2026-11-01` (`Session`, `Rules`,
`ProjectSettings`, `MSession2`, `MRules2`, `MSettings2`), SPEC 0.52. No screenshots were compared
(no headless Chromium here, as in T49): the layout was checked by the CSS and DOM tests only. The
check by hand on a phone (allow `python3 *` from a card, the next `python3` runs without a card,
another folder still asks) is still to do; the worker side of it was checked live (below).

## Worker

- **Where the matcher lives.** `crates/hqc-worker/src/permissions.rs`: `split_command` (a
  quote-aware splitter that gives up on what it does not understand), `RuleSet::cover` (which
  rule allows each part), `Policy::folder_allows` (used in `can_use_tool`), `suggest` (the card's
  patterns), `inspect` (the rules screen). The syntax of an hqc rule is in `hqc_proto::rules`, so
  the hub refuses what the worker would drop; the client has a copy for the live check
  (`client/src/rules.ts`).
- **The syntax is a strict subset** of Claude Code's: a tool name, `mcp__server[__tool]`,
  `Bash(<words>)`, `Bash(<words> *)`, `Bash(<words>:*)`; no quotes, operators or inner `*`, and no
  whole `Bash` (it would allow every command). Words are compared with the part's words, so
  `Bash(git fetch:*)` does not cover `git "fetch origin"` or `git fetchx`.
- **`[permissions]` alone does not answer.** A part may be covered by `worker.toml`'s `allow`
  (so `cargo test && python3 x` runs with `Bash(python3 *)` in hqc and `Bash(cargo test:*)` in
  `worker.toml`), but at least one part must be covered by an hqc rule. Without that, every call
  the CLI asked about despite a `worker.toml` rule (a path outside the folder, for instance) would
  now run without a card; before T48 it was a card with the "a worker rule allows it" note.
- **Files are not used for coverage in `can_use_tool`**: the CLI already applied
  `~/.claude/settings.json` and `settings.local.json` before asking, and reading them per request
  would put I/O into the I/O-free engine. The check of the rules screen does use them, to say what
  the CLI would do.
- **`git -C <path>`** is skipped lexically, not by resolving the path: absolute under the
  workspace, or relative without `..` while no earlier part is `cd`/`pushd`/`popd` (Claude Code's
  shell stays inside the working folder). A symlink inside the workspace that points out is not
  detected; the rule would then allow e.g. `git fetch` in another repository. Other git options
  before the subcommand (`-c`, `--git-dir`) make the command uncovered and give no suggestion.
- **Which workspaces `permissions.inspect` accepts**: one the worker heard of through
  `session.start` or `workspace.rules` (the hub sends the latter for every workspace after
  `hello`), canonical and inside `allowed_roots`. The user's settings file is
  `<claude config>/settings.json` (`$CLAUDE_CONFIG_DIR` or `~/.claude`), labelled
  `~/.claude/settings.json`. Only the three `permissions` arrays leave the worker.
- **Restricted sessions** get the rules too: there is no Bash there, and the owner's answer for
  `Edit` in the folder is the same answer.
- **Live check** (one run, `claude-haiku-5-5`, `--max-turns 2`, 2.1.308, `tmp/spike-t48-*`,
  `claude_live::live_folder_rules`): with `Bash(touch *)` the CLI's request for
  `touch hqc-t48-a && touch hqc-t48-b` was answered by the rule, no `approval.request`, both files
  made.

## Hub

- Table `workspace_rules` (migration `0025`), `ON DELETE CASCADE` from `workspaces`; `hqc_rules`
  (the count) is a subquery of the workspace columns, so every workspace answer carries it.
- `POST` adds a list (the card saves one rule per part at once); rules the folder has are kept,
  so a duplicate from the card is not an error. The form of the rules screen reports a duplicate
  itself, as the mockup asks. `DELETE` goes by the rule's id.
- `workspace.rules` is sent after every `hello` for every workspace of the worker, also empty
  lists: a reconnect with the same `boot_id` keeps the worker's memory, and a list emptied while it
  was away must reach it. Its `ack` is not tracked (as `mirror.set`).
- `GET .../permissions` goes through the `fs.*` slots (4 at once per worker).

## Client

- **The card**: "Allow in this folder" shows only when the request has `rule_suggestions` (the
  worker sends them for sessions with a workspace and commands it can split) and the card is in its
  own session (`ChatActions.folder`); in the "Requests" list and in a waiting session's feed it is
  not offered, since the folder of another session is not at hand there. An empty field adds no
  rule for its part. "Save and allow" saves first and answers `once` only when the hub stored the
  rules; a failure shows the error and answers nothing.
- **Not done from the mockup**: the collapsed line after the answer says "allowed once", not
  "allowed · 2 folder rules saved" (the answer event does not know about the rules; adding that
  would need a field on `approval.response`). The phone sheet keeps T26's layout and only gets
  "In this folder…" among its buttons; the full-screen page of `MSession2` is there.
- **The rules screen** is `#/p/<project>/rules/<workspace>` (`?session=` from a session header),
  so the left column stays on the project. The header has the worker with its dot, the path,
  "Environment default" (T43 later), "<mode> by default" and, from a session, "This session:
  <mode>"; the "Everything else" text follows the session's mode, else the worker's default. The
  files' `ask` rules are listed under "Everything else" (the mockup has none). A file the worker
  could not read is named under "Allowed".
- **The check** asks the worker after 300 ms without typing; a tool name (`Edit`, `mcp__…`) is
  sent as `tool`, anything else as `command`. Example chips are `git status`, `npm install && npm
  test`, `Edit` (the mockup's are its sample data).
- **"Won't take effect"** is the worker's check of the rule's command (`git push` for
  `Bash(git push:*)`) or tool: a deny verdict gives the warning with the rule and its source.
- **Phone** (`MRules2`): one column, the source under each rule in small type, the remove cross
  always visible (also on the desktop, as in `Rules.dc.html`), the add form full screen (Cancel /
  New rule / Save). The phone header leaves out "Environment default" for room.
- The dev mock (`client/mock/server.mjs`) has no rules endpoints: the screen there shows the
  hub's errors. Nothing in the tests needs it.
