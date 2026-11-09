# hqc

Self-hosted control of Claude Code sessions from a phone or a browser.

- **hub** (`crates/hqc-hub`): the server with Postgres, behind Cloudflare Access; it stores sessions and relays events.
- **worker** (`crates/hqc-worker`): runs on your machines, starts the agents and streams their sessions to the hub.
- **client** (`client/`): the web app (PWA) for projects, session chats, permission prompts and push notifications.

Setup: [`docs/RUNBOOK.md`](docs/RUNBOOK.md). Protocol and design: [`docs/SPEC.md`](docs/SPEC.md).

MIT licensed, see [`LICENSE`](LICENSE).
