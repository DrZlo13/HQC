# hqc

Hub / worker / client system for driving LLM agents on many machines from one chat UI.

## Source of truth
- `docs/SPEC.md` is the specification. Code follows it; when code needs the spec to change,
  change the spec in the same PR, bump its version line and add an entry to §23. Never let
  behaviour drift from the spec silently.
- `docs/ITERATION-2.md` is the current task list (`docs/ITERATION-1.md` — the closed first
  iteration). Each task names the spec sections it implements and its acceptance criteria.
- Marks in the spec: **[decision]** = decided, ask before changing; **[verify]** = verify
  on a real build first, then replace the mark with what you found (version, fixture).

## Layout
- `crates/hqc-proto` — event and frame schema (SPEC §3–§5). Dependency-light; shared by hub,
  worker and (later, via generated TS) the client. Change it first, then the users.
- `crates/hqc-claude-jsonl` — tolerant parser for Claude Code transcripts and stream-json
  (SPEC §8.1, §9). Fixtures in `fixtures/`, one set per CLI version.
- `crates/hqc-hub` — axum + Postgres hub (SPEC §2, §5, §6, §10–§17).
- `crates/hqc-worker` — worker daemon: adapters, MCP server, mirror, workspaces (SPEC §8, §9, §14).
- `client/` — TypeScript PWA (SPEC §6).

## Rules
- Rust stable, `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace` must pass before a commit. CI runs the same.
- The worker does not trust the hub for `allowed_roots`, environments, bindings and
  `allow_foreign_sessions`; the user's identity and answers to permission requests come
  from the hub and are trusted (SPEC §2.1, §14). A compromised hub can therefore run code
  on every connected worker within its owner's policy. Keep that invariant in every worker
  change.
- Events are append-only; `seq` is assigned by the hub only (SPEC §5.2).
- Anything that spawns `claude -p` costs the subscription: keep such tests behind
  `HQC_LIVE_CLAUDE=1` and use `--model claude-haiku-5-5` with `--max-turns` no more than 2
  (runs that call an MCP tool need two turns: the first goes to ToolSearch). Prefer fixtures.
- Never commit `api-request-blob` records, `.credentials.json`, worker tokens or `.env`.
- Postgres for tests: `HQC_TEST_DATABASE_URL` (local `postgresql://localhost/hqc_test`);
  migrations live in `crates/hqc-hub/migrations`.
- Language: everything is in English (spec, docs, UI, code, comments, commit messages).

## Machines
- This repo is developed on `home-mac` (macOS arm64), which is also the first worker.
  Claude Code CLI is installed and logged in there; Postgres 17 runs via Homebrew.
- `llm-server` runs Hermes (ACP agent for iteration 4) and a local model on :8082.
