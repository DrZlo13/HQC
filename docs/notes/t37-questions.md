# T37: questions and decisions

The user's case: session A asked session B on another worker with `hqc_ask`; B needed
permission for Bash; the card showed only in B's feed while the user looked at A, which said
nothing but "Waiting for a reply". SPEC 0.41 (§6, §10.1, §10.2, §16).

## 1. One list, computed on the hub

`requests::open_of(owner)` is the only query: every open `approval` / `clarify` /
`worker_approval` row of `pending_requests` in the owner's sessions that are not archived, with
the session's address, title and worker, the request event as stored, and `waiting` — the
sessions waiting on it through open asks. `GET /api/requests` returns it; the client gets the
same list over `/api/ws` as `requests`, right after `summary` and again whenever it may have
changed. There is no per-session endpoint: "the requests of the sessions A waits on" is the
list filtered by `waiting` containing A, which the client does (`AppState.waitedRequests`).

Whose requests: the session owner's (`sessions.owner_id`), the same rule as the session feed
(`can()` is owner only until grants, iteration 6). A request in a session of mine on another
user's worker is listed, and answering it gives the same `403` as in its feed; that cannot
happen today (sessions run on the owner's workers).

The transitive waits are a recursive CTE over the owner's open asks (`asker → target`), cut at
8 steps: one chain nests at most 3 asks (§7.1), but asks of separate chains can join into a
longer line or a cycle (A asks B in one chain while B asks A in another), and the cut bounds
both. A session never counts as waiting on itself. An ask of another user's session aimed at
mine is not followed (the asks are filtered by the asker's owner; the hub refuses such asks
anyway, T32).

The request's event is found by `(session_id, payload->>'request_id')` among
`approval.request` / `clarify.request` events: migration `0020` adds a partial index for it,
so a list of a few requests does not scan long feeds.

## 2. Live changes without polling

A new bus notice, `RequestsChanged {owner_id}`, makes every summary-subscribed client of that
owner re-run the query and send the list whole. It is raised:
- after any stored `approval.*` / `clarify.*` event (the hub writes the event after it opened
  or closed the row, and the worker's own `approval.request` / `.response` / `.cancel` open or
  close the row in the same `after_store`), so answers from any device, `auto_approve`,
  expiry, stop, archive, worker restart and the worker's own cancellations are all covered;
- when an ask opens, is answered, times out, fails on a lost turn, is cancelled by
  `mcp.cancel` or is closed by a worker restart (the waits changed);
- when a session is deleted (its rows go with it).

The whole list, not a diff: it is small (open requests of one person), and a client that missed
a frame is right again with the next one; a bus overflow (`Lagged`) resends it with the
summary. Bursts are not coalesced; with the handful of requests a person has open this was not
worth a timer.

## 3. The client

- The card in the asking session is `ForeignRequest`: the same `PermissionBlock` /
  `ClarifyCard` as in the request's own feed, fed with an empty `Timeline` (so it is open) and
  actions that answer through `AppState.approveRequest` / `answerRequest` — the existing
  `POST /api/sessions/{B}/requests/{id}` of B. It sits under the activity line ("Waiting for a
  reply from …") in A's feed, inside a dashed accent-coloured frame (the session's own requests
  are amber cards without a frame) headed "Request of <title> <address> on <worker>" with a
  link to B, and "Also waiting: …" for the other sessions of the chain.
- Enter and Esc never answer another session's request: the hotkeys stay with the chat's own
  first approval. A stray Enter in A must not allow a command in B.
- The phone's bottom sheet stays for the session's own approval; a waited-on request is a card
  in the feed. Answering it does not need the composer, and two sheets would compete.
- After an answer the client drops the card at once (and on `409 request_closed`: someone
  answered first); the hub's next `requests` says the same. An answer given in B, on another
  device or by expiry reaches A through that frame.
- "Requests": `#/requests`. Desktop: an entry with the count above "Workers" in the left
  column, shown while something is open (or while the screen is open). Phone: a "Requests N"
  button in the session list's header, likewise only while something is open. The screen lists
  every request as the same framed card ("Waiting on it: …" names the sessions of the chain),
  answering in place, the title linking to the session.
- The card has no `workspaceRoot` of B, so a path outside B's working folder is not pointed out
  in A's view (B's own feed still does). The project's workspaces of B are often not loaded in
  A's view; loading them for this note was not worth another request per card.

## 4. Push

One notification per request, as before: `session_id` and `tag` (`<B>:<request_id>`) stay the
request's own, so a re-sent notification replaces it. New: `open_session_id`, the top of the
chain, which the service worker opens instead of `session_id`. It is set only for the session
owner (the waits are theirs); a worker owner who is not the session owner gets the old message.

No push while the user watches the request's session or any session waiting on it: the card is
on that screen (decided here; the old rule was "not watching the request's session").

## 5. Security

- Only the owner's sessions, asks and requests are read (`another_owners_requests_are_never_listed`:
  another user's open request and another user's ask aimed at my session do not show; their
  change sends my clients nothing; their request answered through my session URL is `404`).
- Nothing is approved automatically: the list only shows; an answer is the user's click,
  through the existing endpoint with its checks (worker owner, options, `409` on a closed
  request). The worker's permission policy and its trust in the hub are untouched; no worker
  code changed.

## 6. Tests

- Hub (`crates/hqc-hub/tests/t37.rs`): `the_owners_requests_are_listed_and_followed_live`
  (approval, clarify and worker request listed with address, title, worker and event, REST and
  the frame agree, each disappears on answer / worker cancel), `another_owners_requests_are_never_listed`,
  `requests_show_where_the_user_waits_along_the_chain` (A on one worker asks B on another, B asks
  C; C's request lists B and A nearest first; one push with `open_session_id` = A and C's tag;
  no push while A is watched; answered from A's view the answer is C's event and frame; a second
  answer `409`; C's reply ends B's wait and the list follows).
- Client (`client/tests/requests.test.tsx`): the card in A under "Waiting for a reply", named
  and framed, B's worker and the other waiting session; Enter does not answer it; allow and
  text reply go to B's endpoint and the card leaves; it leaves when the hub's list drops it
  (answered elsewhere) and on `409`; a clarify the same way; the list with the worker request,
  answering in place, the empty state; the left-column entry with its count and the phone
  button; the `requests` frame through `HubSocket`.

## 7. Open

- The list carries no B workspace path (section 3).
- A `requests` frame per change per connection: fine for one person's handful of requests; if
  a hub ever serves many busy users, coalescing per connection (a short timer) is the next step.
