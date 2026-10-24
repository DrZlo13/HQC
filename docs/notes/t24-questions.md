
## Open: deleting a session and the transcript (2026-10-24)

Deleting a session in hqc erases it in the hub and cleans `.hqc/inbox`; the transcript
`~/.claude/projects/…/<uuid>.jsonl` is not touched (and we keep it that way: hqc does not
delete Claude Code's file). The hub does not remember deleted `runtime_session_id`s, so if the
conversation is continued in VS Code, the mirror will create it in hqc again. An option to
decide on: the hub remembers the worker's deleted transcripts, and the mirror ignores them.
The user will decide later.
