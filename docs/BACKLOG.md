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

## Attachments whose file is gone (2026-11-03)

A deploy without the `hqc-artifacts` volume lost the files of earlier attachments; their rows
stayed, and Open/Download on those cards fail (T53 now warns at start). Open: what the client
shows for such an attachment, and whether to remove the rows (the owner decides; nothing is
deleted automatically).

## Paste long text as an attachment (2026-11-04)

Pasting a large block of text into the composer (Ctrl+V / Cmd+V) turns it into a text attachment
of the message instead of inserting it into the input; the composer shows it as a chip with the
size and the first line. Ctrl+Shift+V (Cmd+Shift+V) always pastes into the text, whatever the size.

Open: the threshold. Candidates: by characters (e.g. > 2–4 KB) and/or by lines (e.g. > 40–50
lines), whichever comes first; maybe a setting later.
Specs: §6 (composer), §12 (artifacts and attachments).
Acceptance: a paste over the threshold becomes an attachment the agent receives like any other
text attachment; below it, and with Shift always, the text lands in the input; the attachment
can be removed before sending.

## From T3 Code, Orca and Claude Code (2026-11-16)

What T3 Code, Orca, and Claude Code with its VS Code extension have and hqc does not, picked
from the notes of #93 (`docs/notes/hqc-vs-t3code.md`, `docs/notes/hqc-vs-orca.md`) and from
"Not used" in `docs/notes/hqc-vs-claude-code.md`; the second list comes after the first.

First:
- A side question to a running session, answered without touching its turn: T3 Code and Orca ask
  it in a copy beside the session (`/side`, `/btw`); Claude Code has `side_question`, with no turn
  and no trace in the agent's context.
- `/goal` (T3 Code, Orca): the agent keeps working until a condition holds; a banner with the
  goal, pause and reset.
- The agent's task list (TodoWrite) as a pinned block: "3 of 7 tasks completed" (T3 Code, Orca).
- How full the context window is, before the auto-compaction (T3 Code, Orca; Claude Code:
  `get_context_usage`).
- The prompt-cache timer: how long until Claude's cache of the session expires (Orca).
- A secret from Bitwarden (Vaultwarden): the `hqc_secret` card (SPEC 0.147) takes the secret from
  Bitwarden instead of a typed value, as T3 Code's card takes a 1Password reference.
- "Send now" (Claude Code: `priority: "now"` with `origin: {kind: "human"}`): the CLI moves the
  running Bash, MCP call or subagent to the background and takes the message into the same turn,
  without waiting for the call to end. `origin` matters by itself too: without it the message is
  "unattributed and fails closed at strict isHuman() trust gates".
- A queued message taken back before the agent picks it up, and a Stop that clears the queue
  (Claude Code: `cancel_async_message`, `interrupt {cancel_queued: true}`); now a message that
  waited still runs after Stop.

Second:
- Mermaid diagrams in Markdown (T3 Code, Orca).
- A "New" mark above what came while the owner was away (Orca).
- A "Jump to latest" button while the feed is scrolled up (Orca).
- Link previews on hover: a page, a GitHub issue or PR (Orca).
- The session's local servers and ports, with Stop (T3 Code, Orca).
- Going on after a usage limit resets: the session itself tells the agent to continue (T3 Code,
  Orca).
- Reasoning effort and Fast in the model picker, the thinking budget (T3 Code, Orca; Claude Code:
  `--max-thinking-tokens`, `set_max_thinking_tokens`, `effort`).
- The auto-compaction threshold, a compaction before the cache expires (T3 Code), and a manual
  compaction (the VS Code extension's "N% of context remaining until auto-compact").
- The machine kept awake while its agents work (T3 Code, Orca; desktop apps).
- MCP Apps: an interactive page an MCP server returns with a tool result, shown under the call in
  the feed (T3 Code; Claude Code since 2.1.301).
- Signing in to Claude Code from hqc, a phone included, when the CLI on a node is signed out
  (T3 Code; Claude Code: `--enable-auth-status`).
- The last 8 KiB of a background build's or server's output on a click (Claude Code:
  `get_task_output`).
- A turn cut by a worker restart goes on after `--resume` instead of ending in "Agent process
  exited during a turn" (Claude Code: `CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1`; the VS Code
  extension sets it with a limit of 1 hour); the hub then must not close a turn the worker will
  continue.
- The mirror from the CLI itself (Claude Code: `--session-mirror`, frames `transcript_mirror`):
  the worker gets every record its process writes to the transcript, and the new file's path right
  after `/clear`, so the mirror needs no matching by text with a 5 s wait; the class of bugs like
  #42 and the extra session after `/clear` go away. Checked live that the CLI does it.
- Notes to an answer to the agent's questions ("skipped", "answered in the chat") instead of the
  general "The user did not answer the questions." (Claude Code: `annotations`).
- The agent's MCP servers: which are up, reconnect a failed one or turn one off without a restart
  (Claude Code: `mcp_status`, `mcp_reconnect`, `mcp_toggle`).
- The session's state from the CLI itself (idle, running, waiting for a person) instead of the
  worker's guesses (Claude Code: `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS=1`).
- Why the agent failed to start, instead of the last line of stderr in 500 ms (Claude Code:
  `CLAUDE_CODE_STARTUP_FAILURE_RESULTS=1`).
- A clean exit of the process on unload and kill instead of SIGTERM with code 143 (Claude Code:
  `end_session`).
- The subscription's spending and limits, as `/usage` shows them (Claude Code: `get_usage`,
  `get_session_cost`).
- Plugins and skills reloaded without restarting the session (Claude Code: `reload_plugins`,
  `reload_skills`).
- One TodoWrite instead of four task tools, as the VS Code extension does (Claude Code:
  `CLAUDE_CODE_ENABLE_TASKS=0`).
- The calls of a subagent's own subagents in the subagent's history (Claude Code: `initialize`
  with `forwardNestedSubagentToolCalls`, `forwardSubagentText`).
- A slash command the CLI runs at once in the middle of a turn, where hqc holds every command
  until the turn ends (Claude Code: `command_lifecycle queued` with `ran_ahead`); by the code of
  2.1.312, in stream mode only a plugin's local command marked `immediate` runs so.

## From the Claude app's Code tab (2026-11-18)

What the Claude app for macOS has in its Code tab and hqc does not, picked from
`docs/notes/hqc-vs-claude-desktop.md`; the second list comes after the first. The bugs that note
lists are not here: they are being fixed.

First:
- "Waiting for you" on a session that ended its turn with a question in its text, from the
  CLI's turn summary (`system/post_turn_summary`: blocked / completed). The CLI sends it only
  with the `claude-desktop` entry point, and switching the worker to that entry point breaks
  sessions with a model provider, adds the login shell's variables to the agent's Bash and gives
  the agent `SendUserFile`, which bypasses the chat.
- Going on after the laptop slept: a turn that ended with `api_error: "computer_slept"` before
  the answer began is continued by the worker with "the computer was asleep, go on"; macOS
  workers only.
- Going on after the context filled up mid-turn: the worker sends `/compact`, then "go on", one
  attempt per turn.
- A spare process started ahead, so the first message of a new session does not wait for the CLI
  and its MCP servers to start; the price is one more process per worker.
- An API error that says why and what to do (529, 429, a full context, the network, the
  sign-in) as one line, with Retry of the same message and the model menu, and a compaction for a
  full context.
- A refusal answered inside the turn: declare the `refusal_fallback_prompt` dialog to the CLI
  and answer it with a card "retry on the fallback model / edit"; "Edit and retry" is a fork from
  the refused message with its text in the composer.
- The turn's time, tokens, stage and "Stopping…" in the "Agent is working" line.
- How long the model thought: "Thought for 12s".
- Bash output: the exit code and "no output" as marks, diff lines coloured, and the full output
  the CLI saved on the worker as an attachment.
- An edit's diff with the file's line numbers and "+N −M", from the edit result's
  `structuredPatch`.
- A workflow: a line above the composer with Stop, and the call's card with its phases and
  agents.
- WebSearch links as a "title — domain" list, a WebFetch page as Markdown.
- TodoWrite in its opened line as a list with check marks.
- In the mirror, the interactive CLI's service lines from the transcript: "while you were away",
  "saved to memory", "background agents stopped".
- Which notifications to send: permission requests, questions, "the agent finished".
- A message queued for after the turn (⌘Enter): the hub holds it until the turn ends, and it can
  be removed or edited until it is delivered.
- "Compact now" after a long idle time: a bar above the composer after 70 min idle with the
  context at 75 % of the window or more; needs the context fill after the last answer.
- Links and `#123` in the person's own messages open on a click.

Second:
- What the usage limit went on over 24 hours, as the app says it: context over 150k, 4+
  sessions in parallel, subagent-heavy sessions, sessions active for 8+ hours, cache misses over
  100k tokens; the hub has all of it per turn.
- Rewinding the conversation in place, without rolling files back.
- Big images in attachments downscaled before they go to the agent (to 2000 px).

## Earlier

- Tombstones of deleted sessions, so the mirror does not bring them back:
  `docs/notes/t24-questions.md`.
- "Deny always" for a new folder in the worker's service session: iteration 6.
- The personal assistant session: iteration 5 (SPEC §19).
