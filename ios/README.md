# hqc for iOS

A native iPhone and iPad client for the hub, in Swift and SwiftUI with the iOS 26 Liquid Glass
design: the projects, their sessions, a session's chat, and the permission requests and
questions of every agent in one place. It talks to the hub's REST API and `/api/ws` exactly as
the web client does (SPEC §6); nothing in the hub or the worker changes for it.

## What it does

- **Projects → sessions → chat.** The session list shows each session's live state, subagents
  and unread count from the `summary` frames; swipe to archive. "New session" picks one of
  the project's workspaces and a permission mode.
- **Chat.** User and peer messages, the agent's Markdown answer streamed from `agent.delta`,
  its thinking (collapsed), tool calls folded with their results (tap to open the arguments and
  output), attachments, and system lines. Retracted events are hidden. The composer is a glass
  capsule whose send button morphs into Stop while the agent works. The "More" menu switches
  the session's permission mode and offers Retry after an error.
- **Requests.** Permission cards carry the agent's real options (Allow once, for the session,
  always, Deny…); question cards (AskUserQuestion) have single- and multi-choice options with
  their descriptions and an "Other" answer. They appear inline in the chat and, for every
  session at once, in the Requests tab, from the `requests` frame.
- **Reading.** Opening a chat moves the read cursor (`PUT /read_cursor`).

Not in this first version: attachments upload and preview, rooms' own controls, schedules,
search, workers and workspace management, and push notifications (the hub sends Web Push, which
a native app does not receive; APNs would need a hub change). The web app covers those; Settings
links to it.

## Signing in

Behind Cloudflare Access (RUNBOOK §1.6) the app opens the hub in a web view, where Access shows
its login page; once Access sets its `CF_Authorization` cookie on the hub's host, the app keeps
that value in its Keychain and sends it as a cookie with every request and the WebSocket
upgrade, with `Origin` set to the hub's origin (SPEC §20). When Access lets the session expire,
the next request comes back as a redirect and the app asks to sign in again. Set the Access
application's **Session Duration** long (RUNBOOK suggests a month).

Google and other identity providers that refuse embedded web views do not work in this login;
Access's one-time PIN by email does. A hub without Access (dev mode on loopback) needs no
sign-in.

## Build and run

Xcode 26 or newer (the iOS 26 SDK), no dependencies:

```sh
open ios/HQC.xcodeproj        # then Run on a simulator or a device (set your team for a device)
```

or from the command line:

```sh
cd ios
xcodebuild -project HQC.xcodeproj -scheme HQC \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' build
```

To try it without a hub, start the web client's mock (`cd client && npm ci && node
mock/server.mjs`) and enter `http://localhost:8787` in the app on a simulator: it speaks the
hub's shapes, streams a turn, and asks a permission request (or questions, when the message
contains "question"). The mock has no `GET /api/requests` and sends no `requests` frame, so the
Requests tab stays empty there.

## Layout

| folder | what |
|---|---|
| `HQC/App` | the app, the root view (setup, sign-in, the tabs) |
| `HQC/Model` | the hub's objects and events, decoded as tolerantly as the web client's `proto.ts` |
| `HQC/Net` | REST client, the `/api/ws` socket with resubscribe-from-`seq` on reconnect, Keychain |
| `HQC/State` | `AppModel` (projects, summaries, requests) and `ChatModel` (one chat's timeline) |
| `HQC/Views` | SwiftUI screens and the feed's rows |

The project uses a file-system synchronized group: new files under `HQC/` join the target
without editing the project file.
