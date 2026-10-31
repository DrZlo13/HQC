# Backlog

Ideas and requests not planned into an iteration yet. When one is taken, it becomes a task in
the current `docs/ITERATION-*.md` and leaves this list.

## Permission rules left out of T48 (2026-10-31)

- Removing "Always allow" rules from a folder's `.claude/settings.local.json` on the rules
  screen (a Claude Code file that VS Code reads too; T48 shows them read-only).
- hqc's own deny rules per folder ("never run `rm` here"); until then deny lives only in
  `worker.toml`.

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
