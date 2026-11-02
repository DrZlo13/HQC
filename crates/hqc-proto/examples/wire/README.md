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
  `agent.delta` without `seq`), `subscribed`, `delivery`, `summary`, `session.summary`,
  `requests` (the user's open requests, T37), `pong`, `error`.
- `api.<name>[.request].json` — REST bodies (SPEC §5.1, §6, `hqc_proto::api`): `api.session.json`
  (the session object; its summary is `client.session.summary.json`; `api.session.vscode.json` —
  one held open in VS Code, `held_by`, T20), `api.events.json`,
  `api.input.request.json` / `api.input.json`, `api.error.json`, `api.vapid.json`,
  `api.push.subscribe.json` (`PushSubscription.toJSON()`), `api.push.json` (Web Push body,
  read by `client/public/sw.js`), `api.enroll*.json` (worker enrollment),
  `api.session.request.json` (creating a session in a workspace), `api.project*.json` and
  `api.projects.json` (projects, with `color` and `icon` since T49), `api.me.json` and
  `api.me.patch.request.json` (`GET`/`PATCH /api/me`, the app `theme`), `api.workspace*.json` and `api.workspaces.json`
  (workspaces; `api.workspace.conflict.json` — the `409 workspace_conflict` body),
  `api.dirs.json` (`GET /api/workers/{id}/dirs`), `api.requests.json` (`GET /api/requests`,
  T37: a request two sessions wait on and a worker's own request), `api.artifact.json` (`POST /api/artifacts`,
  `GET /api/artifacts/{id}`) and `api.artifact.url.json` (`GET /api/artifacts/{id}/url`).

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

T21: `hub.mirror.set.sessions.json` — `mirror.set` with the hub's sessions of the workspace and
the time of their last event (`since_ts`): the worker's first pass over these transcripts skips
older records. `hub.session.cleanup.json` / `hub.session.cleanup.scratch.json` — the session was
archived or deleted: remove its inbox folders, or the scratch folder of a session without
workspace. `worker.hello.machine.json` and `api.enroll.request.machine.json` carry the machine
fingerprint `machine_id`; in `api.enroll.info.replace.json` a worker of the same machine has
`same_machine`; `api.workspace.patch.mirror.request.json` switches the mirror of a workspace.

T24: `hub.title.request.json` — the hub asks the session's worker for a title by the first prompt
and the first final answer (each cut to 2000 characters); `worker.result.title.json` is the
answer, one line of at most 60 characters. A failure is a plain `error` with the same `id`.

T48: `hub.session.start.json` carries the workspace's hqc rules (`allow_rules`);
`hub.workspace.rules.json` is the whole list after a change or `hello`;
`hub.permissions.inspect.json` (`h-16`) asks for a folder's rules and a check, answered by
`worker.result.permissions.inspect.json` (a settings file that is not JSON has `error`);
`event.approval.request.json` has `rule_suggestions`. REST: `api.workspace.rules.json` (`GET` /
`POST /api/workspaces/{id}/rules`), `api.workspace.rules.request.json`,
`api.workspace.permissions.json` (`GET /api/workspaces/{id}/permissions`); the workspace object
has `hqc_rules`.

T45: `hub.workspace.create.json` asks the worker to clone into a new folder (`request_id` is the
workspace's id; repeated after `hello` while it is `creating`), `worker.workspace.created.json`
and `worker.workspace.created.error.json` are the two outcomes; `hub.workspace.inspect.json` /
`worker.result.workspace.inspect.json` and `hub.workspace.delete.json` /
`worker.result.workspace.delete.json` come before and with the deletion of a folder hqc created.
REST: `api.workspace.clone.request.json` (`POST /api/workspaces` with `kind: clone`),
`api.workspace.clone.json` (a clone that failed: `state` and `error`),
`api.workspace.inspect.json` (`GET /api/workspaces/{id}/inspect`); the workspace object has
`state`.

T39: the chain of `event.peer.message.json` carries a cap (`permission_cap: ask`, `capped_by`),
which `hub.session.input.peer.json` hands to the worker; `worker.session.state.capped.json` —
the worker runs that input's turn in `ask` under the cap; `api.session.json` and
`client.session.summary.json` show the session meanwhile (`permission_capped_by`).
