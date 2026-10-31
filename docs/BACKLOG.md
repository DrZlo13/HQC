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

## Environment options left out of iteration 4 (2026-10-31)

Return when a case needs them (SPEC §14).
- `network`: a domain allowlist for Claude Code's bash sandbox, per environment, so that an
  agent in `infra` reaches only the local network. A real boundary, unlike rules in settings,
  but the sandbox changes how builds and package managers behave and needs checking on a real
  build.
- `roots`: folders outside the workspace that sessions of an environment treat as their own
  (`--add-dir`, and `hqc_attach` from there), e.g. a shared KiCad library next to the projects.

## Project session template (2026-10-31)

SPEC §13.3: "New <thing>" in a project makes `<workspace>/<slug>` and starts a session there with
a prompt. Left out: sessions in a shared folder create their subprojects themselves.

## Earlier

- Tombstones of deleted sessions, so the mirror does not bring them back:
  `docs/notes/t24-questions.md`.
- "Deny always" for a new folder in the worker's service session: iteration 6.
- The personal assistant session: iteration 5 (SPEC §19).
