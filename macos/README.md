# hqc for macOS

A native macOS app that shows the hqc web client in its own window instead of a browser tab:
Swift, AppKit and WKWebView, built with SwiftPM. Logins persist between launches. Links to other
sites open in the default browser; the app never opens a second window.

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
  server is a tab; the tab bar appears only when there are two or more. With no server the app
  opens Settings at launch.
- **Shortcuts**: the app's own shortcuts, working while the app is active. They act on the
  active tab's page through the client's own controls; each is also an item in the File or Go
  menu. To change one, click its button, then press a key with at least one of ⌘ ⌃ ⌥; Esc
  cancels, Delete clears it. A shortcut recorded for one action is taken off any other action
  that had it.

  | action | default | menu | what it does |
  |---|---|---|---|
  | New Session | ⌘N | File | a new session in the current project |
  | Archive Session | ⌃⌘A | File | archives the open session, as its row's menu does (with Undo) |
  | Previous Session / Next Session | ⌥⌘↑ / ⌥⌘↓ | Go | the row above or below in the session column |
  | Previous Project / Next Project | ⌃⌘↑ / ⌃⌘↓ | Go | the project above or below on the project bar |

  Where the session column is not shown (a narrow window, the search page) the session
  shortcuts other than New Session do nothing; below 900 px the client has no project bar, and
  the project shortcuts do nothing either.

Closing the window hides it, so the pages keep their state; a click on the Dock icon shows it
again, ⌘Q quits.

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
from Notification Center.

With the Alerts style (System Settings ▸ Notifications ▸ HQC) a notification stays on screen
until it is dismissed; the app takes every one away after 5 s except permission requests,
which stay until they are answered.
