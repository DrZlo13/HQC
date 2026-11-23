# hqc for iOS

A native iPhone and iPad client for the hub, in Swift and SwiftUI with the iOS 26 Liquid Glass
design. It covers what you do with hqc day to day, against the hub's REST API and `/api/ws` as
the web client does (SPEC §6), plus two hub routes made for it: sign-in through the system
browser and Apple push (SPEC 0.217).

## What it does

- **Projects, sessions, rooms.** Live state, subagents and unread counts from the `summary`
  frames; favorites first; swipe to favorite, archive or unarchive; archived sessions on demand.
  New project, new session (workspace and permission mode), new room (its agents with names,
  models and system prompts, conversation or council); rename, favorite and archive a project.
- **Chat.** User and peer messages, the agent's Markdown answer streamed from `agent.delta`,
  its thinking, tool calls folded with their results, images and files, system lines; retracted
  events hidden. The composer is a glass capsule: attach photos or files (uploaded at once,
  `POST /api/artifacts`), complete `/` commands, and the send button turns into Stop while the
  agent works. The feed follows new output until you scroll up; a round arrow above the input
  jumps back to the newest message, with a dot when something arrived meanwhile. Answers have
  copy and share, and ⌘↩ sends from a keyboard. What the agent runs in the background (subagents, commands, wakeups) sits above
  the input, each with Stop where the runtime allows it. Images show inline; any artifact opens
  in QuickLook; a published page opens at its public link.
- **What a message offers.** Quote (its first sentences, or a part selected in a sheet) into
  the input as `> ` lines, copy, share, fork the conversation from a message of the user, keep
  it as a quick message; quick messages in the input's + menu.
- **Secrets.** An agent's `hqc_secret` is a form with secure fields, never stored, in the chat
  and in Requests; the + menu gives a secret or an ssh key to the running agent.
- **Agent at work.** Move a running command to the background, open a subagent's own steps,
  the usage limits of the worker's account (model sheet, and a bar above the input), take over
  a session VS Code drove.
- **Session controls.** Permission mode, model, schedules, pause and resume, restart, retry,
  rename, favorite, archive, unarchive, delete. A room lists its agents, each opening its own
  session.
- **Requests.** Permission cards with the agent's real options and question cards
  (AskUserQuestion) inline in the chat and, for every session, in the Requests tab.
- **Schedules.** A session's schedules: once at a time or on a cron pattern, on and off, delete.
- **Search.** Every session's messages, commands, output and answers (`GET /api/search`).
- **Projects.** Settings (name, description, colour, an icon of letters, an emoji or an
  image, quick messages, delete), archived projects with Restore, the bar's own order kept on
  the hub, the session list by State, Recent or a Custom order. A folder's settings: its label,
  VS Code sessions, hqc's allow rules and the rules the worker reads with a command check, its
  ssh keys, and removing it (for a folder hqc made, with what is uncommitted first); new
  folders by cloning a repository, a git worktree, or importing Claude Code's folders. Move a
  session to another folder or project.
- **Models.** Versions of each family and model providers in a session's model sheet;
  Settings → Models manages the providers.
- **Workers.** Online state, Claude Code's account on each machine and its sign-in from the
  phone (the worker runs `claude auth login`), revoking, and confirming a new machine's
  enrollment code (also from an `hqc-app://enroll?code=XXXX-XXXX` link). Workspaces: add a folder
  of a worker through a browser of its `allowed_roots`, or remove one.
- **Notifications.** Apple push for permission requests, questions, final answers, room posts
  and failed input. A hub gets it with one line, `HQC_APNS_RELAY_URL=https://hqc.drewnja.xyz`,
  the publisher's relay, or with its own key for a self-signed build (RUNBOOK §1.10). "Allow once" and "Deny" on an
  approval's notification answer it without opening the app; a tap opens the session. While a
  chat is on screen, its notices show no banner.

## Sound and haptics

Agents are felt and heard while their chat is open: a glassy click per tool call (a few a second
at most), a low pair when one fails, a knock for a permission request, a rising three notes for
a question, a chime when the turn ends and a falling pair when it fails; a request in another
session knocks too. Sending, answering, copying and moving around tick. The launch follows SwiftBooking's:
the system launch screen (the logo on `LaunchBackground`) hands over to a splash drawn the same,
the logo breathes with a felt swell (`SplashHaptics`) and then shrinks and fades onto the app; on
a first launch the welcome screen draws that moment itself and flies the logo up into place, and
finishing onboarding pushes it through the viewer while the app rises beneath it. A set-up app opens its main screen at once, with skeletons and a
"Connecting…" bubble until the hub answers, and retries while the hub is away; onboarding builds
itself line by line, each line a tap and a note up the scale. Every cue's haptic taps and tones
come from one list of notes (`HQC/App/Feedback.swift`), so they land together; the tones are
synthesized, soft and short, go through the `.ambient` session (they mix with music and follow
the silent switch) and replace the system sound of a notification while the app is open.
Settings has a toggle for each and plays every cue on demand.

## Layouts

The app follows the space it has, not the device. With compact width (an iPhone, iPhone Duo's
outer display, a narrow Split View) it has tabs: Projects, Requests, Settings and Search; on
iPhone Duo's outer display the system lays the tab bar vertically along the side. With regular
width (iPhone Duo's inner display, an iPad) it takes the web client's desktop layout: the project
rail, the project's sessions and the chat side by side. Search (⌘F) and Requests open in the
column beside the rail, not in a sheet: a result or a request opens its chat on the right and
the list stays for the next one; tapping the rail button again goes back to the sessions. When iPhone Duo is half folded, the column
edge moves onto the fold (the active division `ReservedRegion`, iOS 27.1), so the list and the
chat each own one half and nothing sits in the curve.

The look is the web client's: its six palettes (warm, graphite and high contrast, dark and
light), stored on the hub and the same on every device, optionally following the system's light
or dark setting; the chat's rows, tool groups, thinking and Markdown as `client/src/ui/feed.ts`
draws them, LaTeX with the bundled KaTeX (`HQC/Resources/KaTeX`, MIT), and the run ring around a
working project's tile at the speed of its agents' output tokens.

## Signing in

"Sign in" opens the hub in the system browser (`ASWebAuthenticationSession`), so Cloudflare
Access runs with every identity provider, Google included, and with the browser's own Access
session. The hub hands back a one-time code, which the app exchanges with a verifier only it
holds for the Access token (SPEC §6); the token stays in the app's Keychain and goes as the
`CF_Authorization` cookie with every request and the WebSocket upgrade, with `Origin` set to
the hub's origin (SPEC §20). When Access lets the session expire, the next
request comes back as a redirect and the app asks to sign in again. A hub without Access (dev
mode on loopback) needs no sign-in.

## Build and run

Xcode 26 or newer (the iOS 26 SDK), no dependencies:

```sh
open ios/HQC.xcodeproj        # then Run on a simulator, or on a device with your team set
```

or from the command line:

```sh
cd ios
xcodebuild -project HQC.xcodeproj -scheme HQC \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' build
```

The project signs with automatic signing for team `AEHMXBQ3VJ` and bundle id `xyz.drewnja.hqc`,
the published app; Product → Archive in Xcode makes the build for TestFlight and the App Store.
To build under your own team, change both under Signing & Capabilities, and send push with your
own key (`HQC_APNS_TOPIC` = your bundle id, RUNBOOK §1.10). The push entitlement is
in `HQC.entitlements`; the `hqc-app` link scheme and the photo library text in `HQC-Info.plist`.

To try it without a hub, start the web client's mock (`cd client && npm ci && node
mock/server.mjs`) and enter `http://localhost:8787` in the app on a simulator: it streams a
turn and asks a permission request (or questions, when the message contains "question"). The
mock has no requests list, rooms, schedules, search or workers; for those run a hub in dev mode
(`tools/dev.sh up`, or `HQC_DEV=1` by hand, RUNBOOK §0.1) and enter its address.

## Layout

| folder | what |
|---|---|
| `HQC/App` | the app, the app delegate (push, notification actions), the tabs |
| `HQC/Model` | the hub's objects and events, decoded as tolerantly as the web client's `proto.ts` |
| `HQC/Net` | REST client, the `/api/ws` socket with resubscribe-from-`seq`, the browser sign-in, Keychain |
| `HQC/State` | `AppModel` (projects, summaries, requests, navigation, push) and `ChatModel` (one chat) |
| `HQC/Views` | SwiftUI screens: projects, chat and its rows, artifacts, rooms, schedules, search, workers |

The project uses a file-system synchronized group: new files under `HQC/` join the target
without editing the project file.
