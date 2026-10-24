# T5: decisions and open questions

The client was built without the hub (T2 ran in parallel) and with no one to ask. Below are the
places where the spec (0.9) is silent or allows different readings, and what the client chose.
The mock (`client/mock/server.mjs`) implements exactly these shapes. **SPEC.md was not changed
in this branch**, to avoid conflicts with T2, which owns §6; items marked "into the spec" need
to go into §6 when T2 and T5 are merged (with an entry in §23). Merged in 0.11 (T7): the client
and the mock moved to the hub's shapes.

## The `/api/ws` protocol

1. **Message envelope** (into the spec). The client sends `{id?, type, payload}`, like the §5
   frames, with `id` optional:
   - `subscribe {session_id, after_seq}`;
   - `subscribe_summary {}`;
   - `unsubscribe {session_id}`: **added by the client**, it is not in §6; the client sends it
     when leaving a chat. The hub may ignore it, but then events of closed chats keep coming
     until the connection ends.

   The hub sends:
   - `{type: "event", payload: {event}}`, like the worker's `event` frame; the client also
     accepts the event directly in `payload`;
   - `{type: "summary", payload: {sessions: [{session_id, state, queue_len, unread, last_seq?}]}}`:
     the full list on `subscribe_summary`, then only the sessions that changed; the client
     merges by `session_id`, and on an unknown `session_id` re-reads the session list;
   - `{type: "error", payload: {code, message}}`.

   The client ignores unknown `type`s.
   Resolved in 0.11: the hub's shape is flat frames without `id` and `payload`, `unsubscribe` exists, plus `subscribed`, `delivery`, `session.summary`, `ping`/`pong` (§6, `hqc_proto::client_frame`).
2. **`agent.delta` over WS** arrives as a regular `event` with the full event envelope and no
   `seq` (like `examples/wire/event.agent.delta.json`). The client accumulates deltas in a
   buffer and flushes it on any `agent.message`, on a `status` other than `running` and when
   the socket drops (deltas after a disconnect do not continue the old text).
   Resolved in 0.11: that is how it works, `{type: event, event}` without `seq`, example `client.event.delta.json` (§6).
3. **A change of `delivery`.** Events are append-only, but `delivery`/`delivered_at` of a
   `user.message` change. The client assumes the hub **sends the same event again** (the same
   `seq` and `event_id`, new fields) and replaces it by `seq`. T2 has to choose: this, a
   separate event, or a field in `summary`. Without any of them "awaiting delivery" will not
   change until a reload.
   Resolved in 0.11: a separate frame `{type: delivery, session_id, event_id, seq, delivery, delivered_at?}`; the client and the mock were reworked, the event is not sent again (§6).

## REST

4. **Lists.** `GET /api/projects/{id}/sessions` and `GET /api/sessions/{id}/events`:
   the client accepts both a bare array and `{sessions: [...]}` / `{events: [...]}`. The mock
   returns arrays.
   Resolved in 0.11: only `{sessions: [...]}` and `{events: [...], last_seq}`, and the client and the mock likewise (§6).
5. **The session object** (into the spec): `id, title, slug, state, queue_len, last_seq,
   archived, updated_at` and either `unread` or `last_read_seq` (then the client computes
   `last_seq - last_read_seq`). §3 Session has no `unread`.
   Resolved in 0.11: the hub's session object (§6, `hqc_proto::api::Session`) has everything in the list plus `last_read_seq`; the client reads it strictly.
6. **What counts as unread.** The client just shows the number from the hub. The mock counts
   all events after `last_read_seq`, including `status` and `usage`, which is noisy. A proposal
   for T2: count only `agent.message final`, `approval.request`, `clarify.request`,
   `peer.message` and `system`.
   Resolved in 0.11: accepted, the rule is in §6, implemented in the hub and the mock.
7. **Creating a session**: `POST /api/projects/{id}/sessions {title}`; the hub picks the worker
   and the workspace (iteration 1: the only workspace from the worker's config). The response
   is the session object.
   Resolved in 0.11: that is how the hub does it (`201` and the session object), and so does §6.
8. **Input**: `POST /api/sessions/{id}/input {text, artifact_ids: [], client_op_id}`. The
   client does not read the response body and waits for the echo over WS. If the POST fails,
   the message is marked "not sent" with a "Retry" button that sends **the same
   `client_op_id`**. A proposal for T2 (into the spec): the hub deduplicates input by
   `(session_id, client_op_id)` and answers a repeat with the already stored event; otherwise
   a lost response to the POST gives a double turn. The mock does so.
   Resolved in 0.11: the hub and the mock answer `{event}` (201, a repeat gets 200 with the same event); the client shows the event from the response at once, without moving `lastSeq` (§6).
9. **Errors**: body `{code, message}`; the client shows `message`. An answer to a closed
   request (`POST .../requests/{id}`) gets 409 `request_closed` in the mock; the client shows
   the error under the buttons, and only an `approval.response`/`approval.cancel` event removes
   the buttons.
   Resolved in 0.11: body `{error, message}` (the client keeps `error` as the code), a second answer gets 409 `request_closed` without an event (§6, §10.1).
10. **History**: the client reads it in pages of 200 from `after_seq=0`, then does `subscribe`
    from the last `seq`. Long sessions need loading from the tail (`before_seq` or `limit`
    from the end); §6 has only `after_seq`. Not needed yet.
11. **Artifact link**: `GET /api/artifacts/{id}/url` → `{url}`.
12. **Worker enrollment**: `POST /api/workers/enroll/confirm {code}`; the client strips spaces
    and upper-cases the code, the response is `{worker_id, name?}` (the name is shown).
    The screen also opens from `#/enroll?code=ABCD-1234`; a proposal for T3: the worker could
    print a ready link `<hub>/#/enroll?code=...`.
    Resolved in 0.11: the worker prints the link; the hub's response is `{worker_id, name}` (§5.1, §6).

## Web Push

13. **`GET /api/push/vapid`** → `{public_key}` (base64url, an uncompressed P-256 point): the
    route is not in the §6 list (into the spec). `POST /api/push/subscribe` receives
    `PushSubscription.toJSON()`: `{endpoint, expirationTime, keys: {p256dh, auth}}`.
    Resolved in 0.11: so in §6 and the hub; `public_key: null` means push is off on the hub, and the client shows that.
14. **Push body** (to agree with T2): JSON `{session_id, type, title?, body?, tag?, payload?}`.
    If there is no `title`/`body`, the service worker builds them from `type` and `payload`
    (so the hub can send the §4 event itself). A click opens `#/s/<session_id>`: it focuses an
    open client window or opens a new one. `approval.request` and `clarify.request` are shown
    with `requireInteraction`.
    Resolved in 0.11: the client's shape is accepted, `payload` is the §4 event itself (omitted if it does not fit into the push); the hub sends that, and sw.js reads the event from `payload` (§16).
15. The service worker **does not cache** the app: without the hub the client is useless, and a
    stale cached build against a new hub is worse than an explicit network error.

## UI behaviour

16. **Permission button labels** follow `canonical` (Allow / Allow for this session /
    Always allow / Deny / Always deny); for an unknown `canonical`, the agent's `label`; the
    agent's label is always in the button's `title`. There are exactly as many buttons as
    `options[]`.
17. **`expires_at`**: once it passes by the device clock, the buttons are hidden with an
    "expired" note, without waiting for `approval.cancel {timeout}` from the hub. The risk is a
    wrong phone clock; if that turns out to be a problem, keep the buttons until the cancel
    event.
18. **Who answers a request** (§10.1, only the worker owner answers): iteration 1 has one user,
    so the client does not hide the buttons; the hub's refusal is shown as an error.
19. **Read state**: while a chat is open and the tab is visible, the cursor moves to the last
    `seq` (`PUT read_cursor`), and the badge in the list goes out at once, without waiting for
    `summary`.
20. **Strictness**: `author.kind` and `delivery` are parsed strictly, as in `hqc-proto` (they
    are not in the §4 list of tolerant enums). An event that fails to parse does not crash the
    chat; it is shown as a collapsed "malformed event" card.
21. A `tool.result` is shown inside the card of its `tool.call` (by `call_id`); a result
    without a call gets a card of its own.
22. The interface language is Russian, no i18n.

## Not verified

- The layout was not looked at in any browser: the T5 environment has no browser. The UI was
  checked by tests in happy-dom (a full scenario through `<App>`), by the build and by types.
  A manual run on a phone is needed (T6).
- Push from a real hub was not verified: the mock does not encrypt push.
