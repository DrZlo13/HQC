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
  read by `client/public/sw.js`), `api.enroll*.json` (worker enrollment).

Optional fields are omitted when absent; readers must accept `null` for them as well.

Replies reuse the request `id`: `hub.hello.ack.json` and `hub.error.json` answer
`worker.hello.json` (`w-1`), `hub.ack.json` answers `worker.event.json` (`w-2`),
`worker.ack.json` answers `hub.session.input.json` (`f-3`). `hub.ack.unknown-session.json`
is the ack for an event dropped because the hub does not know its session (no `seq`).
`hub.clarify.response.cancel.json` — empty `answers`: the hub cancelled the clarify request.
