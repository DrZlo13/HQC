# Backlog

Ideas and requests not planned into an iteration yet. When one is taken, it becomes a task in
the current `docs/ITERATION-*.md` and leaves this list.

## Permissions managed from hqc (2026-10-25)

The rules live in three places today (`[permissions]` of `~/.hqc/worker.toml`,
`~/.claude/settings.json`, `.claude/settings.local.json` of a repository), none of them visible
in hqc; "Always allow" saves the exact command with all its arguments, so it rarely matches
again, and agents that write Python, grep, route boards and convert files ask all the time.
A permission mode per session is T38 (`docs/ITERATION-3.md`).

1. **A pattern instead of the exact command on the card.** Next to "Always allow", "Allow
   `python3 *` in this folder" with a pattern hqc suggests from the command and the user can edit
   before confirming. The hub keeps such rules per workspace and sends them with
   `session.start`; the worker merges them into `--settings`, its `deny` wins.
2. **A rules screen.** Per worker and folder, every rule in force with its source
   (`worker.toml`, Claude Code's user settings, the folder's settings, hqc's rules), so a rule
   that blocks something by accident is easy to find. hqc's rules are added and removed there;
   Claude Code's own files are shown, not edited.

## Earlier

- Tombstones of deleted sessions, so the mirror does not bring them back:
  `docs/notes/t24-questions.md`.
- "Deny always" for a new folder in the worker's service session: iteration 6.
- The personal assistant session: iteration 4 (SPEC §19).
