# Wire examples

Contract examples for SPEC §4, §5 and §6, read by `hqc-proto` tests via `include_str!`
and by the TypeScript client tests (`client/tests/proto.test.ts`) as the same contract.
Rust tests require that each file deserializes and serializes back to exactly the same JSON.

- `event.<type>.json` — a stored event (SPEC §3 Event, §4), as the hub hands it out.
  `event.unknown.json` — an event of a type this build does not know: kept as is,
  not rendered (SPEC §4).
- `hub.<type>[.<case>].json` — a frame hub -> worker (SPEC §5.3), envelope `{id, type, payload}`.
- `worker.<type>[.<case>].json` — a frame worker -> hub (SPEC §5.4).
- `client.<type>[.<case>].json` — a frame on `/api/ws` (SPEC §6, `hqc_proto::client_frame`):
  flat `{type, ...}`, no `id`, no `payload`. Client -> hub: `subscribe`, `unsubscribe`,
  `subscribe_summary`, `ping`; hub -> client: `event` (`client.event.delta.json` — an
  `agent.delta` without `seq`), `subscribed`, `delivery`, `summary`, `session.summary`, `pong`,
  `error`.
- `api.<name>[.request].json` — REST bodies (SPEC §5.1, §6, `hqc_proto::api`): `api.session.json`
  (the session object; its summary is `client.session.summary.json`), `api.events.json`,
  `api.input.request.json` / `api.input.json`, `api.error.json`, `api.vapid.json`,
  `api.push.subscribe.json` (`PushSubscription.toJSON()`), `api.push.json` (Web Push body,
  read by `client/public/sw.js`), `api.enroll*.json` (worker enrollment),
  `api.session.request.json` (creating a session in a workspace), `api.project*.json` and
  `api.projects.json` (projects), `api.workspace*.json` and `api.workspaces.json`
  (workspaces; `api.workspace.conflict.json` — the `409 workspace_conflict` body),
  `api.dirs.json` (`GET /api/workers/{id}/dirs`).

Optional fields are omitted when absent; readers must accept `null` for them as well.

Replies reuse the request `id`: `hub.hello.ack.json` and `hub.error.json` answer
`worker.hello.json` (`w-1`), `hub.ack.json` answers `worker.event.json` (`w-2`),
`worker.ack.json` answers `hub.session.input.json` (`f-3`). `hub.ack.unknown-session.json`
is the ack for an event dropped because the hub does not know its session (no `seq`).
`hub.clarify.response.cancel.json` — empty `answers`: the hub cancelled the clarify request.
`worker.result.fs.stat.json` answers `hub.fs.stat.json` (`h-7`),
`worker.result.fs.list_dirs.json` answers `hub.fs.list_dirs.json` (`h-8`); in hub requests
`request_id` equals the frame `id`. `hub.fs.list_dirs.roots.json` (`path: null`) asks for
`allowed_roots`. `worker.worker.approval.request.json` (`w-9`) is a confirmation the worker
asks its owner for outside a session (SPEC §10.2); the owner's answer comes later as
`hub.worker.approval.response.json` (a new frame `h-12` with the same `request_id`).
