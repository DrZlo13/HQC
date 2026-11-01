# T50: decisions and differences from the mockups

Archive at hand, archive mode and the custom order by `docs/design/redesign-2026-11-01`
(`Session`, `Archive`, `MList2`, `MArchive2`), SPEC 0.52. As in T49, no screenshots were
compared (no headless Chromium on this machine): the layout was checked by the CSS rules and the
DOM tests. The check by hand on a phone (long press and Undo, restore and delete from the
archive, reorder with a finger) is still to do.

## Hub

- **The order record is normalized on write and on read.** `PUT` keeps only the unarchived,
  non-service sessions of the project (so another project's, another user's, an archived, an
  unknown or a repeated id is dropped) and puts the project's sessions missing from the body
  first; it stores exactly what it returns. `GET` applies the same rule to the stored list, so a
  session created since goes on top and an archived or deleted one drops out without touching
  the record. Ids are not foreign keys (an array, `uuid[]`), which keeps deletion free of extra
  work.
- **"Missing ones go first" means newest created first.** The task does not say among several
  missing ones; a new session is the newest, so this puts the latest on top.
- **A restored session goes on top**, not back to its old place: archiving drops it from the
  list the client sends next, and the hub does not keep a separate memory of old positions.
  Undo within the 5 s is different: nothing reached the hub, so the row is simply back where it
  was.
- **The mode and the order are one record**, `PUT {mode?, session_ids?}`; either field alone
  keeps the other. No record reads as `state` with the sessions newest first. An unknown mode is
  `422`, like other enums.
- `archived_at` is also set when a revoked worker's service session is archived and cleared when
  enrollment with replacement brings it back; the API never shows those in the archive, but the
  column stays consistent.

## Client

- **The delayed archive lives in `AppState`**, not in the list component: the timer must survive
  the phone switching from the list to a session. The pending session is left out of
  `sortedSessions` (so the counters and "needs you" drop at once) and is in neither list until
  sent. It is sent when the toast ends (`ARCHIVE_UNDO_MS = 5000`), when another row is archived,
  before Reorder starts (the order sent must hold every live session, or the pending one would
  come back on top after an Undo), when archive mode opens (so the archive lists it), and on
  `pagehide` or `visibilitychange` to hidden with `fetch(..., {keepalive: true})`. When the hub
  refuses, the row comes back and the list shows "Not archived «…»: …".
- **The chosen mode is shown at once** and stored with `PUT`; a refusal restores the previous
  record and shows "Order not saved: …". The old `hqc.sessionOrder` value in `localStorage` is
  not migrated: one click sets it again, and keeping a reader for it would be code without a
  user.
- **Reorder ignores the search** (it starts with the query cleared) and works on the whole live
  list; the drag uses the row height (or 64 px when there is no layout) as in `dragVals`, the
  handle has `touch-action: none` and pointer capture, and ↑/↓ keep the focus on the moved
  handle.
- **Archive mode is app state, not a route** (`AppState.archiveMode`, reset on a project switch):
  `App.tsx` is shared with the parallel tasks, and the mode must survive opening an archived
  session on a phone and coming back. The search of archive mode is desktop only, as in the
  mockups (`MArchive2` has none).
- **"Archived <when>"** uses the existing `formatDay`: "Archived today at 23:45", "Archived
  yesterday at 09:05", "Archived Oct 25 at 12:00" — in the row and in the band alike (the
  `Archive` row shows "today 23:45" / "Oct 25" without "at"; one format reads better than two).
  Without `archived_at` only "Archived".
- **The band's text on a phone** says "Restore or delete it from the archive." instead of "from
  the list on the left", which is not there on a phone.
- **The composer lost `disabled`**: an archived session has no input at all now, and a session
  VS Code holds already had the handover panel instead, so nothing passed `true` any more; the
  "Session archived" placeholder went with it.
- **Toasts after Restore / Delete in archive mode** ("Restored «…» to sessions", "Deleted «…»")
  stay until clicked, as in the mockup; the Undo toast of the live list has its 5 s.
- **Delete of a live session** from "⋯" or the sheet calls `DELETE` directly: the hub allows it
  for any session, and the mock hub now does too (it used to require archiving first).

## Not done here

- **"Folder rules"** in the row's "⋯" and in the phone sheet: the rules screen is T48, on another
  branch; the item can be added there or at the merge (the "⋯" label says "rename, delete").
- **Rename** uses `prompt()`, like "Rename" of a project; the mockups draw no rename UI.
- The Workers block and the "Requests" link are hidden in archive mode, as in `Archive`.
