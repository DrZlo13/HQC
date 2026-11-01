# hqc client redesign, round 2 (2026-11-01)

Mockups of the new interface and how it should behave. The interface is in English; session
content can be in any language. Round 1 is `docs/design/redesign-2026-10-24` (T26), now
replaced and kept for history only. All session names, paths, sums and times in the mockups
are sample data.

## Files

- `*.dc.html`: one screen per file, in the Claude Design canvas format. They **do not open in
  a browser directly** (they need the canvas runtime); read them as sources:
  - the markup inside `<x-dc>` is plain HTML with inline styles: take layout, spacing, sizes
    and fonts from it;
  - `{{name}}` is a value from `renderVals()`; `<sc-for list="{{items}}" as="item">` is a loop;
    `<sc-if value="{{flag}}">` a condition;
  - behaviour (what changes on a click, validation, rule matching) is in the `Component` class
    at the end of the file;
  - shared CSS classes (`.btn`, `.row`, `.ib`, `.seg`, `.field`, …) are in `<helmet><style>`.
- `canvas.json`: the layout of the screens on the canvas; ignore it.
- `theme.js`: theme tokens and the function that turns a base theme and a project color into
  CSS variables. Ready to use.

Desktop:

| File | Screen |
|---|---|
| `Session.dc.html` | The answering session (esp/session-5): sidebar, "Question from / Reply to", permission card v2 |
| `AskingSession.dc.html` | The asking session (esp/session-4): "Sent to / Reply from" |
| `Archive.dc.html` | Archive mode |
| `Rules.dc.html` | Folder rules |
| `ProjectSettings.dc.html` | Project settings: appearance, workspaces, project |
| `Appearance.dc.html` | The "Project appearance" dialog: icon, color, theme |

Phone:

| File | Screen |
|---|---|
| `MList2.dc.html` | Session list: projects as chips with their icons, State / Recent / Custom and the Reorder button (in Reorder mode a handle on the right of the row, dragged with a finger), a floating "New". A long press (or right click) on a row opens a bottom sheet: Archive, Rename, Folder rules, Delete… (delete is confirmed in a second step of the sheet). Archiving shows a toast with Undo. |
| `MSession2.dc.html` | The answering session: "Question from / Reply to" in a compact form; a permission request replaces the input with a bottom panel: "Allow once" large, "Always allow" and "In this folder…", "Deny" and "Reply instead". "In this folder…" opens a full-screen page with a pattern per part of the command and "Save and allow". |
| `MArchive2.dc.html` | Archive: hatched header, "Restore" and "Delete" always visible on every row (44 px), delete confirmed in the row. |
| `MSettings2.dc.html` | Project settings: appearance, workspaces (each with "VS Code sessions", "Rules", "Remove"), project. |
| `MRules2.dc.html` | Folder rules: one column, the source in small type under each rule, the remove cross always visible, the add form full screen (Cancel / New rule / Save). Same logic as `Rules.dc.html`. |

Not drawn for the phone: the Appearance dialog (a full-screen sheet with the same sections) and
the asking session (the same blocks as the answering one, with ↗/↙ arrows).

## Theme and color

Theme = a base palette (Warm by default, Graphite, High contrast; one for the whole app) plus
the project's color. The project color tints the project bar and the sidebar (like a Slack
workspace theme) and colors primary buttons, the "running" indicator, the selected row and the
user's bubble. The formulas are in `theme.js`. All three bases are dark; there is no light
theme (decided 2026-11-01).

Status colors do not depend on the project: "waiting for you" is always amber, online/success
green, dangerous actions red, messages between sessions violet. So the project palette has no
yellows.

Fonts: Inter (interface), JetBrains Mono (paths, commands, rules, sha). Body text 14 px, meta
12–12.5 px.

Desktop layout: project bar 72 px → sidebar 280–310 px → main column; the feed and the input
at most 780 px wide (rules 820 px), centered.

## Behaviour

### Project bar (left)
A 44×44 tile with the project icon (an emoji, 1–2 letters or an image) on the project color at
~24 % opacity. The current project: a vertical pill of the project color at the bar's left
edge (4×28 px, rounded on the right) and a more saturated tile (`--mark-bg` in `theme.js`); no
rings around the tile. An amber badge with the number of sessions waiting for the user.

### Session sidebar
- Header: project tile, name, summary ("1 needs you · 2 idle"), a gear → project settings, the
  "New" button.
- Session row: status dot (amber: waiting; project color, pulsing: running; empty ring: idle),
  title, time, status, `worker · path` (the path without `~/work/`).
- **Archive from the row:** on hover or focus the time is replaced by an archive button and
  "⋯" (rename, rules, delete). Archiving shows a toast at the bottom of the list
  "Archived «…» · Undo".
- **Order:** a State / Recent / Custom switch and a Reorder button (⇅) next to it. State groups
  into Needs you / Running / Idle, Recent into Today / Earlier, Custom shows one list in the
  saved order.
- **Reorder mode:** the switch is replaced by a "Reorder · Cancel · Done" bar, rows get a ⠿
  handle on the left, hover actions are hidden. A row is dragged by its handle (pointer events,
  mouse and finger); ↑/↓ on the focused handle move it from the keyboard. Done saves the order
  and turns Custom on, Cancel restores the previous one. Logic: `dragVals`, `handleVals`,
  `sortVals`, `groupsFor` in any desktop file. New sessions appear at the top in Custom. A
  session brought back with Undo after archiving returns to its place.
- Under the list, an "Archive N" button (enters archive mode). The "Project workspaces" link
  goes: workspaces are in the project settings now.
- No archive button in the session header: the action is on the row only.

### Archive mode (`Archive.dc.html`)
Must look different at once:
- the sidebar without the project tint (`--side-plain`), hatching on top, a back arrow, the
  title "Archive", "ESP · 3 sessions", the note "Read-only…";
- "Restore" and "Delete" **always visible** on every row; delete is confirmed in the row
  ("Delete forever? Delete / Cancel");
- an open archived session: an "Archived" chip in the header, a hatched band "Archived today at
  23:45 · read-only. Restore or delete it from the list on the left.", the feed slightly dimmed
  (opacity .82), **no input**;
- Restore and Delete are **only on the list row**, not in the header or at the bottom.

### Messages between sessions
Violet blocks, unlike both agent replies and user messages. The exchange shows in both
sessions, mirrored. The arrow shows the direction: ↗ left this session, ↙ came into it. The
other session's name links to it.

In the answering session (`Session.dc.html`):
- "↙ Question from `esp/session-4`": the incoming question with its text and attachments
  (Open / Download);
- "↗ Reply to `esp/session-4` · delivered 23:11": the reply that went to the other session:
  text, a key-value table if any, an attachment with its caption, a footnote, the token line.

In the asking session (`AskingSession.dc.html`):
- "↗ Sent to `esp/session-5` · delivered 23:11": the question sent, with its attachment;
- "↙ Reply from `esp/session-5`": the reply received, with the same content as "Reply to" on
  the other side.

### Permission card v2
Buttons: "Allow once ⏎" (primary), "Always allow" (the exact command), "Allow in this folder",
"Deny Esc" (quiet, on the right). The reason for the request is a grey line with an "i" icon,
not green. "Allow in this folder" opens fields with the patterns hqc suggests; for a compound
command (`a && b`, `a | b`) one field per part and a note that the rule applies only when every
part is allowed. "Save and allow" saves the rules and runs the call. After the answer the card
collapses into a line with the outcome.

Pattern suggestion: Bash → `Bash(<first word> *)`; git → `Bash(git <subcommand>:*)`, skipping
`-C <path>` only when the path is inside the folder (decided 2026-11-01; otherwise no pattern is
suggested for that part); a non-Bash tool → its name.

### Folder rules (`Rules.dc.html`)
Entries: project settings → a workspace row → "Rules", and "Folder rules" in the session header.
- Header: worker (with an online dot), path, environment (`default` until T43, decided
  2026-11-01), default mode.
- "Check a command": a field, quick examples, the result "Denied / Allowed / Will ask" with
  the rule and its source; "Will ask" has an "Allow…" button that opens the form with the
  suggested pattern. The matching in the mockup (`matches`, `norm`, `check`, `suggest`) is a
  simplified model; the product uses the worker's matcher.
- "Denied" and "Allowed" groups: the rule in monospace, a source chip (hqc in the project
  color, files neutral). hqc rules first, with the date and a cross; removal is confirmed in
  the row. An empty group shows "none".
- The "+ Allow in this folder" form: live format check (an error blocks saving), the warning
  "Won't take effect: … in worker.toml" (saving is allowed), a duplicate is an error.
- Under "Allowed", where to edit the rules from files. "Everything else" explains Ask and Auto.
- Worker offline: hqc rules are shown and editable; instead of the rules from files "Worker
  unavailable…", and the check is disabled.

### Project settings (`ProjectSettings.dc.html`)
Opened with the gear in the sidebar header. Sections: Appearance (current icon and color,
"Change icon and color" → the Appearance dialog), Workspaces (folders on workers with "VS Code
sessions", a "Rules · N hqc" button and "Remove"; "Add a workspace", "Import from Claude
Code"), Project ("Rename", "Delete project").

### Project appearance (`Appearance.dc.html`)
Icon: Emoji / Letters / Image (upload PNG, JPEG or WebP only, square, at least 128 px, at most
256 KB; no SVG). Project color: 8 options. App theme (all projects): Warm / Graphite / High
contrast. A live preview of the project bar and the sidebar on the right.

## Decisions made with the user (2026-11-01)

- The app theme is a user setting on the hub, the same on every device.
- Archiving from a row happens when the Undo toast goes away (5 s); Undo cancels it, so nothing
  is stopped or cancelled until then.
- The Custom order is stored on the hub per user and project.
