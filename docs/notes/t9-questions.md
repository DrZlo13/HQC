# T9 — questions, deferred items, what was not verified

Task: hardening before use, per the audits `t9-findings-1.md` and `t9-findings-2.md`
(SPEC 0.12, §23).

## Not doing (deliberately)

- **"resume only our own ids"** (findings-1, low: any UUID from the hub becomes
  `--resume` if this `cwd` has a transcript). Stays as is: taking over sessions started in
  VS Code (§9.3, iteration 2) is exactly a `--resume` of an id foreign to the worker.
  A restriction to "only those created by this worker" would break takeover. The hub is a
  trusted party anyway (§2.1).
- **`always` writes the rule to `<workspace>/.claude/settings.local.json`** (findings-1 #8).
  This is by design (§8.1): "always" must apply to later sessions too. The rule also applies
  in the owner's ordinary local sessions; this is a deliberate choice. A local worker flag
  that turns `always` into `session` for sessions from the hub is iteration 2.
- **A1/A2: the hub can answer permission requests and pass a session off as the owner's.** Per
  §2.1 the hub is already a trusted party for identity and permissions; signing input and
  answers with device keys registered locally on the worker is iteration 3+. The wording was
  corrected: SPEC §2.1 and CLAUDE.md now state exactly what the worker does not trust the hub
  with, and describe the consequences of a compromised hub.

## Not verified in this run

- **A live `claude` run (item 7).** In the autonomous environment, running `claude` with flags
  (and `HQC_LIVE_CLAUDE=1 cargo test`) is not allowed by the tool allowlist; only
  `claude --help` is available. Therefore:
  - `--permission-mode`: the 2.1.308 help lists `acceptEdits, auto,
    bypassPermissions, manual, dontAsk, plan`, without `default` (in the 2.1.307 fixtures
    `system/init` still writes `permissionMode: "default"`). Passing `default`, as the task
    asked, on 2.1.308 most likely = an argument parse error, and no session would start. At
    startup the worker reads `claude --help` and uses `manual` if it is among the
    `--permission-mode` choices, otherwise `default`
    (`capabilities::claude_permission_mode`, log `starting worker … permission_mode=`).
  - That `always` is still offered with `--setting-sources user,local` and writes the rule,
    that hooks and `defaultMode` from the repository's `.claude/settings.json` are not
    applied, and that the CLI works with a cleaned environment (Keychain login), is checked by
    `tests/claude_live.rs::live_repo_settings_ignored`. Run it once by hand:
    `HQC_LIVE_CLAUDE=1 cargo test -p hqc-worker --test claude_live live_repo_settings_ignored -- --nocapture`
    (haiku, `--max-turns 2`). Until then SPEC §8.1 carries **[verify]**.
- **`tools/e2e-local.sh`** was not run either, for the same reason (the script is not on the
  allowlist). To run: `E2E_CLAUDE_BIN=/usr/bin/false tools/e2e-local.sh`, all steps except
  the live turn; the script now also checks CSP / `X-Frame-Options` on `/`.
- **`--restricted` together with `--permission-mode`** and a session without a workspace in a
  scratch folder were checked only by the command line (a test with a fake CLI), not by a
  live run.

## Decisions along the way

- **The default worker (item 6).** The task: "by default, the last worker that was online; one
  that has never connected is not chosen". Implemented: never one with an empty `last_seen`;
  online before offline; among those, a worker on which the user already has sessions before
  a new one; then the most recent `last_seen`. Without the third rule a phishing worker that
  connected last would be "the last online" and would grab new sessions. What remains: if the
  usual worker is offline and a new one is online, the new one is chosen. The protection here
  is the code confirmation screen; choosing the worker in the client when creating a session
  is iteration 2 (the client does not choose a worker now).
- **The per-address limits on `/worker/*`** take `CF-Connecting-IP` without checking that the
  request came from Cloudflare: the hub is reachable only through the tunnel (RUNBOOK §1.8:
  no `ports:`). The global enroll limit "10 per minute per process" was replaced with 10 per
  minute and 10 live codes per address plus 1000 live codes in total: the old one let a
  single client block enrollment for everyone.
- **A `model` with a leading `-`** is rejected by both the hub and the worker, even though the
  regexp from the task lets it through (`--resume` matches `^[A-Za-z0-9._:-]+$`).
- **`disallowed_tools` rules** are passed as separate arguments after `--disallowedTools`
  (the flag is variadic); the worker rejects a rule that starts with `-`, is empty or
  contains control characters.
- **The agent's environment**: `SHELL` (the CLI's Bash tool picks the shell by it) and
  `LOGNAME` were added to the task's allowlist.
- **A foreign repository's `.claude/settings.local.json`** is still applied (`local` in
  `--setting-sources` is needed for `always`). The file is usually in `.gitignore`, but a
  repository may commit it, and then its hooks will run. This is fully closed together with the
  `always` → `session` flag (iteration 2): then `--setting-sources user`.
- **Executing tools** that get no "whole tool" rule: `Bash`, `PowerShell`,
  `BashOutput`, `KillShell`, `KillBash`.
- **Hijacking a push subscription**: with someone else's `endpoint`, `409 conflict`; when the
  limit is reached, the limit check comes first (`409 push_limit`).

## Human actions in Cloudflare

See RUNBOOK §1.6: in the `hqc` Access application, Cookie settings: SameSite Lax, HttpOnly,
Binding Cookie; a WAF Rate Limiting rule on `/worker/*` (and a stricter one on
`/worker/enroll`); URL normalization on. RUNBOOK §1.8: `hqc-hub` and `hqc-db` have no
`ports:`.
