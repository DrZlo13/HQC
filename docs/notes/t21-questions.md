# T21: decisions along the way and open questions

Five decisions by the user (mirror on by default, no duplicates when enabling it, titles
through a provider, machine fingerprint, cleanup), SPEC 0.23. There were no live `claude`
runs: everything on fixtures, a fake worker, a fake hub and fake model servers.

> **T24 (SPEC 0.25): the hub's model providers are removed entirely** — the `llm` module,
> `HQC_TITLE_MODEL`, `HQC_TITLE_BASE_URL`, `HQC_TITLE_API_KEY`, the limit of 4 concurrent
> calls, the fake model servers in tests. The title is now made up by the session's worker on
> the Claude Code subscription (`title.request`, SPEC §3 Session, §8.1); the fallback title
> from the first prompt remains. The items below about the title model and providers are
> history.

## Decisions made along the way

- **`session.cleanup` is a new frame, not a flag on `session.kill`.** The frame was only
  added; `session.kill` did not change (its semantics are "kill the process", cleanup is a
  different action, and `kill` is sent even when there is no process). Besides
  `{session_id, workspace_path, cwd}` from the task statement, the frame has `event_ids[]` —
  the session's inputs with attachments: the worker itself does not know which
  `.hqc/inbox/<event_id>` folders belong to the session (after a restart there is no memory),
  and deleting all of `.hqc/inbox` is not allowed — other sessions work in the same `cwd`. The
  frame is sent on archiving, deleting a session and deleting a project (its sessions are
  deleted too); a session without attachments in the workspace gets no frame.
  `workspace_path: null` is a session without a workspace; the worker deletes
  `<scratch_root>/<session_id>`. Service sessions get no frame.
- **The worker is offline at the moment of archiving**: the frame is not resent (the hub does
  not store it anywhere); the remainder is removed by the startup cleanup after 14 days.
- **"Known workspaces" for the startup cleanup**: `known_workspaces.json` plus a new
  `$HQC_HOME/inbox_dirs.json`: the working directories where the worker wrote attachments
  (sessions with `cwd` in a subfolder keep their inbox not at the workspace root, and walking
  the workspace tree is expensive). A directory from `inbox_dirs.json` counts only if it lies
  inside a workspace from `known_workspaces.json` and inside `allowed_roots`. Only children of
  `.hqc/inbox` with a uuid name (that is how the worker creates them) are deleted; age is by
  the modification time of the event's folder. Empty `.hqc/inbox` and `.hqc` stay.
- **The mirror's "first pass"** is the first read of a file the mirror has not read yet
  (offset 0 in `mirror.sqlite3`), to the end of the file. A repeated `mirror.set` after
  `hello` does not touch files already read. `since_ts` is the `max(ts)` of all of the
  session's events in the hub, including the hub's own events.
- **`mirror.set.sessions`**: at most the 5000 most recent sessions of the workspace (the frame
  must stay far from the WebSocket message limit, §20); sessions without events are not
  listed.
- **A title from the prompt is also set with `title_source: runtime`** (otherwise every
  following answer would trigger another attempt). There is one attempt per session per hub
  process; after a hub restart, a session whose title stayed `default` (no prompt with text)
  gets one more on the next answer. Model calls: at most 4 at a time: an import brings dozens
  of sessions at once. *(T24: this limit is gone; instead, at most 2 `claude` runs per
  worker.)*
  An `ai-title` that arrives later replaces the hub's title by the general `runtime` rule.
- **Truncation to 60 characters at a word boundary**: the last space before the 59th
  character, trailing punctuation is stripped, then "…"; a single long word is cut hard.
  Markdown is removed line by line: headings, quotes, list markers, `**`, `__`, backticks,
  links `[text](url)`; a line starting with triple backticks is skipped.
- **The `anthropic` provider**: `POST <base>/v1/messages`, `anthropic-version: 2023-06-01`,
  the key in `x-api-key`; **`openai`**: `POST <base>/chat/completions`, the key as
  `Authorization: Bearer`, no header without a key. Redirects are not followed (the key does
  not go to another host). *(Removed in T24.)*
- **The hub accepts a machine fingerprint** only as 64 hex characters (lowercase); anything
  else is silently ignored. On replacement the fingerprint is taken from the new enrollment,
  if it has one.
- **Artifacts**: age is counted from `updated_at`, which is now updated by a repeated upload of
  the same file and by an input with it — otherwise an artifact uploaded 31 days ago and just
  attached again could vanish between the upload and the input.
- **The client's mock hub**: `mirror_enabled` defaults to `true`, but fake sessions are
  imported only with an explicit `mirror_enabled: true` (the "Import from Claude Code"
  button), so as not to change the other scenarios.
- **SPEC §5.3 and §5.4** were not edited in this version (T22 changes them too): the
  `session.cleanup` frame, `mirror.set.sessions` and `hello.machine_id` are described in
  §17.4, §9.1 and §5.1. On merge, add them to the frame lists of §5.3/§5.4.

## Open questions

1. **Mass import after the update.** Migration `0011` enabled the mirror on all workspaces: on
   the first `hello` after the update the worker imports all past Claude Code transcripts in
   those folders (each a separate session in the project, each with a title — with a model,
   one call each). hqc sessions are not duplicated (`since_ts`). Is a limit "import only
   transcripts younger than N days" or a confirmation in the client needed?
   *Merge with T22 (SPEC 0.24):* the wave hits the T22 limit — 200 new sessions per 24 h per
   worker; beyond it the hub answers `error mirror_quota` (retryable), and the worker rereads
   the transcript at most once an hour, so the import stretches over days without losing
   anything. The question about "younger than N days" and confirmation remains.
2. **Duplicates after a worker crash** (T15 question 1, second part) remain: the registry of
   own uuids is in memory, and the adapter's records that the mirror did not pass before the
   crash look foreign after a restart. With the mirror on by default this affects all
   workspaces. The window is up to 2 s; the registry could be stored on disk, or `since_ts`
   applied to the first pass after a restart too.
3. **`since_ts` from hub events.** If the session's last event in the hub is a system one,
   written later than the VS Code records (e.g. `turn_lost` on reconnect), those records will
   not arrive when the mirror is enabled. `max(ts)` could be taken over worker events only.
4. **Windows `MachineGuid` and Linux without `/etc/machine-id`** were not checked on a live
   machine; on home-mac `ioreg` works (test `this_machine_has_a_fingerprint`). Cloned VM images
   with the same `machine-id` will give the same fingerprint — it is only a hint in the client.
5. **A title without a final answer.** A session where the agent gave no final text (the turn
   failed) stays "Session N". Take the title from the prompt right at the first input?
6. **An artifact quota** (T16 question 5) was not done.
