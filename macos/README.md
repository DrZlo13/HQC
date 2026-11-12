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
- **New session hotkey**: a global shortcut, empty by default. Click the button, then press a key
  with at least one of ⌘ ⌃ ⌥; Esc cancels, Delete clears it. The hotkey brings the window
  forward, also when it was closed, and opens a new session in the current project of the
  active tab. It needs no Accessibility permission.

Closing the window hides it, so the pages keep their state and the hotkey still works; a click
on the Dock icon shows it again, ⌘Q quits.
