# hqc client

The hqc web client (PWA): projects and their sessions, the project workspace (T14), the session chat with §4 event cards,
permission buttons, worker enrollment, Web Push. Task T5 in `docs/ITERATION-1.md`,
protocol: `docs/SPEC.md` §4, §6, §10.1, §16, §17.1.

## Stack

TypeScript + Vite + Preact (no UI libraries, our own styles in `src/styles.css`), tests on
vitest and happy-dom. Preact was chosen as the smallest component renderer with the familiar
JSX and hooks model: the whole client without markdown is about 18 KB gzip, which matters for the first load from a
phone through a tunnel, and components and hooks keep the event cards
independent and let us test them by rendering into the DOM. Plain TypeScript without a framework would need
its own mechanism for re-rendering the delta stream and the button state, and React/Solid/Svelte
give nothing needed here beyond Preact, at a larger size or with their own compiler.
Message markdown (T11) is parsed by [marked](https://marked.js.org), only by its lexer
(`Lexer.lex`): it returns tokens, and `src/ui/Markdown.tsx` builds Preact elements from them, so
there is no `innerHTML` anywhere and raw HTML in the text stays text. marked was chosen over
micromark + mdast-util-from-markdown because it is a single file with no dependencies and no DOM,
with GFM (tables, task lists, autolinks, strikethrough) out of the box, and it has no `eval` or
`new Function`, so CSP §20 does not get in the way. The micromark stack gives stricter CommonMark,
but with GFM it is 59 packages and 110 KB (26 KB gzip) unminified in a trial build; marked is
47 KB of minified code, and the client grew from 18.6 to 34.1 KB gzip. marked's parser and renderer
end up in the bundle too (the package is one file, tree-shaking does not remove them); if size
starts to matter, the lexer can be replaced without touching the rendering.
The T26 redesign added about 7.5 KB gzip of JS (44.5 → 52.1 KB) and 3.6 KB gzip of CSS (4.2 → 7.7 KB);
the fonts (≈180 KB woff2) are loaded by the browser by `unicode-range` and weight, only the ones needed.
The service worker (`public/sw.js`) is written in plain JS without a build: it is small and handles
only push.

## Scripts

| command | what it does |
|---|---|
| `npm run dev` | Vite dev server on :5173, `/api` (REST and WebSocket) is proxied to `HQC_HUB_URL` (default `http://127.0.0.1:8080`) |
| `npm run dev:mock` | the mock hub (`mock/server.mjs`, :8787) and the Vite dev server proxying to it, in one process |
| `npm run mock` | the mock hub only, without Vite |
| `npm test` | all tests: the parser over all of `examples/wire`, rendering of cards and markdown, the scenario on the mock through the client transport and through the UI, the dev server proxy |
| `npm run test:scenario` | only the scenario tests on the mock |
| `npm run build` | `tsc --noEmit` and the build into `dist/` |
| `npm run typecheck` | type checking only |
| `npm run icons` | regenerate the PNG icons in `public/` |

## Structure

- `src/proto.ts` — types and parsing of §4 events, §5 frames and `/api/ws` messages; tolerance
  per §4 (unknown `type` → `UnknownEvent`, unknown enum value →
  `"unknown"`, invalid payload of a known type → `ProtoError`).
- `src/api.ts` — REST §6; `src/ws.ts` — `/api/ws` with reconnect and `subscribe` repeated
  from the last `seq`; `src/timeline.ts` — what the chat shows (events by `seq`, the
  `agent.delta` stream, optimistic messages by `client_op_id`, the fate of requests);
  `src/session.ts` — the open chat: history over REST, the tail over WS, actions.
- `src/ui/` — the interface per the T26 mockups (`docs/design/redesign-2026-10-24`, dark theme):
  - `App.tsx` — layout: on desktop the project rail (`ProjectRail`), the session column
    (`SessionList`) and the screen; on a phone (narrower than 900px, `useIsPhone`) one screen at a time;
    widths (T27): the rail is 68px, the session column `clamp(220px, 25vw, 300px)` (narrows
    with the window), the chat `minmax(0, 1fr)` with no minimum; nothing may set a width, a
    minimum width or a flex basis of 300px or more (`tests/layout.test.ts`); long code
    (`.md-code pre`) and wide tables (`.md-table`) scroll inside themselves, everything else
    wraps;
  - `SessionList.tsx` + `sessions.ts` — the session column: "Status"/"Recent" modes (the choice is in
    `localStorage`, key `hqc.sessionOrder`), search by title and path, project counters,
    workers with the number of active sessions (a click opens the service session), the new session form; on a phone
    projects as chips and a "New" button at the bottom;
  - `Chat.tsx` — the composer completes `/` from the session's `commands` (T27: arrows, Tab or
    Enter, Esc, a tap);
  - `Chat.tsx` — the header (state pill, worker, path, model and cost from `usage`,
    "Stop", the "More" menu), the feed, VS Code takeover banners in place of the input field, the input field; on a
    phone an open request is a bottom sheet in place of the input field;
  - `feed.ts` — pure feed logic: tool call groups captioned by the intermediate
    reply, `usage` under the final answer, `tool.call` inside the card of its request;
  - `cards.tsx` — messages, tool groups, service lines, the agent's question;
    `permission.tsx` — the permission card (Enter/Esc, reply with text, collapsed line);
  - `Workspaces`, `Settings`, `Enroll` — pages; `Markdown.tsx` — markdown and code blocks with
    "Copy"; `icons.tsx` — icons; `labels.ts` — labels and formats.
  `src/clipboard.ts` — copying through `navigator.clipboard`, or a hidden textarea if the API is missing
  (http in a LAN).
- `public/` — `manifest.webmanifest`, `sw.js`, icons, `fonts/` — Inter and JetBrains Mono
  (woff2 400/500/600, Latin and Cyrillic, from `@fontsource/*` 5.3.0, OFL licenses alongside): the hub
  serves them itself, CSP `font-src 'self'` lets no external fonts in.
- `mock/` — the mock hub for development and tests.

Routes are in the hash, so the hub can serve the client as static files: `#/` — the list, `#/s/<id>` —
a chat, `#/settings`, `#/enroll?code=ABCD-1234`.

## Mock

`npm run dev:mock` and open `http://127.0.0.1:5173`. The mock serves REST and `/api/ws` from §6
(shapes the spec lacks are in `docs/notes/t5-questions.md`) and builds events from
`crates/hqc-proto/examples/wire/*.json`. It has three sessions: one with history containing every kind of
card (including an unknown event); one with an undelivered input (the "Retry" button);
an empty one. The turn scenario for any message: `status running` → `agent.delta` →
`agent.message` (not final) → `tool.call`/`tool.result` (Read) → `tool.call` (Bash) →
`approval.request` → waits for a button → `approval.response` → `tool.result` → deltas →
final `agent.message` → `usage` → `status idle`. A message containing the word "question" or
"clarify" asks a `clarify.request` instead of the permission. A text reply to a request (the deny option with `message`)
is accepted by the mock and quoted in the final answer. "Stop" during a turn gives
`approval.cancel {stopped}`. A message containing the word "markdown" gets
a streamed answer with every markdown element (headings, task lists, a table, code,
links, a quote, HTML and a `javascript:` link as text) — to check the layout by eye. The mock accepts a worker enrollment code in the form `ABCD-1234`.
`MOCK_SPEED=0.3 npm run dev:mock` speeds up the scenario, `MOCK_PORT` changes the mock's port.

## Testing on a phone in the local network

1. `HQC_DEV_LAN=1 npm run dev:mock` — **with the mock only**. By default Vite (and the mock)
   listen on `127.0.0.1` only: the dev hub (`HQC_DEV=1`) runs without authentication, and Vite
   open on the LAN would hand it to anyone on the same Wi-Fi network. With `HQC_DEV_LAN=1` Vite listens on
   all interfaces and prints the address `Network: http://192.168.x.x:5173/`. On the first
   run macOS may ask whether to allow `node` incoming connections — allow it.
   With a real hub the LAN address will not work anyway: the hub accepts an `http` `Origin` only
   from loopback (SPEC §20).
2. Put the phone on the same Wi-Fi network and open that address. The list, the chat, the permission buttons,
   stop/retry and worker enrollment work over plain HTTP.
3. Push and the service worker **do not work** over HTTP on an IP address: browsers allow them only in a
   secure context (HTTPS or `localhost`). The "Settings" screen says so in that case.
   Testing push from a phone needs HTTPS, for example a tunnel:
   `cloudflared tunnel --url http://localhost:5173` and
   `HQC_DEV_ALLOWED_HOSTS=<name>.trycloudflare.com npm run dev:mock` (the tunnel is also mock-only:
   the trycloudflare address is public and has no Access). On an iPhone push works
   only from an app added to the Home Screen (Share → Add to Home Screen).
4. The mock accepts a subscription (`POST /api/push/subscribe`, writes it to the log) but does not
   send push itself: Web Push encryption is the hub's job (T2). Showing a notification and opening the
   session on click can be tested in Chrome on a computer: DevTools → Application →
   Service workers → Push, with a body like
   `{"session_id":"0199f3a2-7c10-7000-8000-000000000001","type":"approval.request","payload":{"tool":"Bash","description":"cargo test"}}`.

In production the hub serves the client (`HQC_CLIENT_DIR=<path to dist>`) behind Cloudflare Access, see
`docs/RUNBOOK.md`; the client does no authorization and only reads `GET /api/me`.
