# hqc for macOS

A native macOS app that shows the hqc web client in its own window instead of a browser tab:
Swift, AppKit and WKWebView, built with SwiftPM. Logins persist between launches. Links to other
sites open in the default browser; the app never opens a second window.

The window's title bar is the colour of the page's project bar (of the page itself where the
client shows none) and holds, after the window buttons, ‹ › (the page's back and forward, as
⌘[ ⌘]) and the breadcrumbs of the open session, `project › session › state`, or the open
project's name; with two or more servers, a switch between them on the right.

## Requirements

macOS 14 or later and Swift 5.9 or newer (Xcode or its command line tools). No dependencies.

## Build and run

| command | what it does |
|---|---|
| `make build` | builds `HQC.app` here (release build, icon from `../client/public/icon-512.png`, ad-hoc signature) |
| `make run` | quits a running copy, builds, then opens `HQC.app` |
| `make clean` | removes `.build` and `HQC.app` |

## Settings

Settings… (⌘,) holds:

- **Servers**: the hubs, by name and address (`https://` is added when no scheme is typed). Each
  server is a tab, picked by the switch in the window's bar when there are two or more. With
  no server the app opens Settings at launch.
- **Shortcuts**: the app's own shortcuts, working while the app is active; New Session in
  Project works from any app, and brings the window up. They act on the active tab's page
  through the client's own controls (New Session in Project: on its project's server tab, by
  the page address); each is also an item in the File or Go menu. To change one, click its
  button, then press a key with at least one of ⌘ ⌃ ⌥; Esc cancels, Delete clears it. A
  shortcut recorded for one action is taken off any other action that had it.

  | action | default | menu | what it does |
  |---|---|---|---|
  | New Session | ⌘N | File | a new session in the current project |
  | New Session in Project | ⌃⌥⌘N | File | a new session in the project picked for it, on that project's server |
  | Archive Session | ⌃⌘A | File | archives the open session, as its row's menu does (with Undo) |
  | Previous Session / Next Session | ⌥⌘↑ / ⌥⌘↓ | Go | the row above or below in the session column |
  | Previous Project / Next Project | ⌃⌘↑ / ⌃⌘↓ | Go | the project above or below on the project bar |

  Where the session column is not shown (a narrow window, the search page) the session
  shortcuts other than the two New Session ones do nothing; below 900 px the client has no
  project bar, and the project shortcuts do nothing either. The project of New Session in
  Project is picked in the menu next to its shortcut, from the projects of every server the app
  is signed in to; with None it only brings the window up.

Closing the window hides it, so the pages keep their state; a click on the Dock icon shows it
again, ⌘Q quits. The Dock icon shows how many sessions have something unread on all servers,
archived ones and the workers' own aside: the sessions the client's tab icon marks with its
light dot.

## Notifications

The app shows macOS notifications for the events the hub sends a browser push for: permission
requests, questions, the agent's final reply and input that failed, worded as the push, with
the project's name on top and the session's title under it. Every server notifies, also one
whose tab is not open. They come only while the app runs (its window may be closed); a click
opens the session. Nothing is shown for the session you are looking at. macOS asks once for
permission.

A permission request has Allow and Deny buttons (not a publication: that one is allowed only
on its card), which answer it without opening the app; when the request has no such option or
the hub refuses the answer, the session opens instead. A request answered elsewhere disappears
from Notification Center, also one shown before the page reloaded or the app last quit, once
the page is connected again.

With the Alerts style (System Settings ▸ Notifications ▸ HQC) a notification stays on screen
until it is dismissed; the app takes every one away after 5 s except permission requests,
which stay until they are answered.
