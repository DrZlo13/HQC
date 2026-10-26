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

## Permission rules per folder in `worker.toml` (2026-10-26)

The worker's `[permissions]` applies to all its sessions, and its `deny` beats any project
settings, so one folder cannot get `ssh` (or another blocked command) without unblocking it for
every session on the machine. The case: a session in `~/work/lab/infra` was refused `ssh
debian@192.168.1.20` by `Bash(ssh:*)`.

```toml
[[permissions.folder]]
path = "/Users/drzlo/work/lab/infra"
unblock = ["Bash(ssh:*)"]          # lift a worker-wide deny rule here only
allow = ["Bash(ssh debian@192.168.1.20:*)", "Bash(curl http://192.168.1.1:*)"]
```

Matched by the workspace path, the longest match wins (like `env_bindings`, SPEC §14). Only
the worker's config sets it: the hub cannot grant a folder `ssh`, so the trust boundary of SPEC
§2.1 holds. A session's mode still applies on top ("Ask" asks, "Auto" lets the classifier
decide). It is a first piece of the environments of iteration 5 (`ssh` keys are credentials).

## Earlier

- Tombstones of deleted sessions, so the mirror does not bring them back:
  `docs/notes/t24-questions.md`.
- "Deny always" for a new folder in the worker's service session: iteration 6.
- The personal assistant session: iteration 4 (SPEC §19).
