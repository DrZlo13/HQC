# hqc for macOS

A native macOS app that shows the hqc web client in its own window instead of a browser tab:
Swift, AppKit and WKWebView, built with SwiftPM. Logins persist between launches. Links to other
sites open in the default browser; the app never opens a second window.

The window's title bar is the colour of the page's project bar (of the page itself where the
client shows none) and holds, after the window buttons, ‹ › (the page's back and forward, as
Back and Forward in the Go menu) and the breadcrumbs of the open session, `project › session › state`, or the open
project's name; with two or more servers, a switch between them on the right.

## Requirements

macOS 14 or later and Swift 5.9 or newer (Xcode or its command line tools). No dependencies.

## Build and run

| command | what it does |
|---|---|
| `make build` | builds `HQC.app` here (release build, icon from `../client/public/icon-512.png`, ad-hoc signature; `SIGN_IDENTITY="<certificate>"` signs it with a certificate of yours, so Full Disk Access given to one build stays for the next) |
| `make run` | quits a running copy, builds, then opens `HQC.app` |
| `make clean` | removes `.build` and `HQC.app` |

## Settings

Settings… (⌘,) holds:

- **Servers**: the hubs, by name and address (`https://` is added when no scheme is typed). Each
  server is a tab, picked by the switch in the window's bar when there are two or more. With
  no server the app opens Settings at launch.
- **Shortcuts**: the app's own shortcuts, working while the app is active; New Session in
  Project works from any app, and brings the window up. They act on the active tab's page
  through the client's own controls (New Session in Project: the client's own function, which
  opens the project picked in the client's Settings; Back and Forward: the page's history, as
  ‹ ›); each is also an item in the File or Go menu. To change one, click its button, then
  press a key with at least one of ⌘ ⌃ ⌥; Esc cancels, Delete clears it. A shortcut recorded
  for one action is taken off any other action that had it.

  | action | default | menu | what it does |
  |---|---|---|---|
  | New Session | ⌘N | File | a new session in the current project |
  | New Session in Project | ⌃⌥⌘N | File | a new session in the project picked in the client's Settings (on the hub; the personal project until one is picked), in the active tab |
  | Archive Session | ⌃⌘A | File | archives the open session, as its row's menu does (with Undo) |
  | Previous Session / Next Session | ⌥⌘↑ / ⌥⌘↓ | Go | the row above or below in the session column |
  | Previous Project / Next Project | ⌃⌘↑ / ⌃⌘↓ | Go | the project above or below on the project bar |
  | Back / Forward | ⌘[ / ⌘] | Go | the page's previous or next address, as ‹ › in the window's bar |

  Where the session column is not shown (a narrow window, the search page) the session
  shortcuts other than the two New Session ones do nothing; below 900 px the client has no
  project bar, and the project shortcuts do nothing either.
- **Notifications**: Stay on screen, how long a notification card other than a permission
  request stays (1–120 s, 5 s by default); a new card takes the new time, a card replaced in
  place keeps its own.

Closing the window hides it, so the pages keep their state; a click on the Dock icon shows it
again, ⌘Q quits. The Dock icon shows how many sessions have something unread on all servers,
archived ones, the workers' own and the agents of a room aside: the sessions the client's tab
icon marks with its light dot.

## Notifications

The app draws its own notification cards for the events the hub sends a browser push for:
permission requests, questions, the agent's final reply (not of a room's agent), a post in an
agents' room and input that failed. Every server notifies, also one whose tab is not open. They
come only while the app runs (its window may be closed), each with the sound macOS plays for a
notification.
Nothing is shown for the session you are looking at. The cards are not macOS notifications:
they do not go to Notification Center, and System Settings ▸ Notifications does not change them
(macOS asks once for permission only for the number on the Dock icon).

While a Focus is on (Do Not Disturb or another; turned on here or on another device that shares
Focus, by hand, by the Sleep schedule or another trigger; a Focus's own time schedule on this Mac
is not seen), or the screen is shared, recorded, sent over AirPlay or mirrored, no card is on screen
and none makes a sound: a card that goes after its time is dropped, also one already on screen,
and a permission request waits and comes when that ends, if it is still open. The app sees the
Focus only with Full Disk Access (System Settings ▸ Privacy & Security ▸ Full Disk Access); without
it a Focus hides nothing.

A card looks like a macOS banner before Big Sur: a translucent rounded card at the top right of
the screen with the menu bar, under it, in the system's light or dark appearance, over every app
and full-screen space; a new one slides in on top and the others move down. It shows the
project's tile (its colour and letters), the project's name and what happened (Permission,
Question, Reply, Message or Failed), the session's title, and then a permission request's tool
and its whole command or description (up to 6 lines; the command or path first, from the
request's call, or a fixed line saying the app cannot tell the command apart; for a publication,
the path, how many files, then the files, hidden ones first, as many as fit), a question and how many more there are, or
the reply, post or failure (up to 4 lines). A click on a card opens the session.

A permission request, a publication included, has Allow and Deny always visible in a column on
the right, which answer it without opening the app or taking the focus from the app you are in,
and the card goes; when the request has no such option or the hub refuses the answer, the
session opens instead. Its card stays until the request is
answered, here, in the client or elsewhere, also one shown before the page reloaded, once the page
is connected again. Every other card goes after the time set in Settings (5 s by default) from
when it appears; while the pointer is over a card it stays, and the time starts again when the
pointer leaves. With the pointer over a card, × at its top left corner closes it without an
answer. While the pointer is over the cards they hold still, so a button never moves under it: a
new card waits, a card that goes leaves its gap, a card replaced by a longer one that no longer
fits its place waits too, and the cards settle when the pointer leaves or the last one goes;
cards that do not fit on the screen wait in order and come in as room frees up, and a waiting
card's time starts when it is shown.
