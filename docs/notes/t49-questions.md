# T49: decisions and differences from the mockups

Theme, project appearance and the new layout by `docs/design/redesign-2026-11-01`, SPEC 0.51.
No screenshots were compared: this machine has no headless Chromium, so the layout was checked
by the CSS rules and the DOM tests only. The check by hand on a phone and a desktop (acceptance)
is still to do.

## Hub

- **The icon image is not an artifact.** The artifact store would need three exceptions: the
  daily cleanup deletes artifacts no event refers to (a project icon would go after 30 days),
  dedup by `(owner, sha256, name)` shares a row between an icon and a session attachment (so
  deleting the project could not delete it), and a signed link expires after 10 minutes (the
  rail would fetch a new one per project per page). A table `project_icons` (migration `0024`,
  `ON DELETE CASCADE` from `projects`) has none of these. Hence the object is
  `{kind: image, url}`, not `{kind: image, artifact_id}` as in the task text.
- **Upload and selection are one call.** `PUT /api/projects/{id}/icon` (multipart `file`, like an
  attachment) checks, stores and makes the image the icon, and returns the project. `PATCH` with
  `{kind: image}` is `422`; `PATCH` with an emoji or letters deletes the stored image, so there is
  never more than one image per project and none that is not shown. The dialog uploads only on
  "Save", so "Cancel" leaves nothing behind.
- **The checks.** The type comes only from the magic bytes (PNG signature + `IHDR`, JPEG SOI +
  the first SOF segment, RIFF/WEBP with `VP8 `, `VP8L` or `VP8X`); the declared type and the
  name are ignored, and the image is served with the type found. Width and height are read from
  that header; the image is not decoded (no image crate), so a file with a valid header and a
  broken body is stored and shows as a broken image. Too large is `422` like everything else, not
  `413`: the upload stops reading at 256 KB + 1 byte, before the route's body limit (320 KB)
  would cut it, so a 3 MB file gets the same `422` (tested).
- **Cache.** `url` carries `?v=<first 16 hex of the sha256>`; the response is
  `private, max-age=31536000, immutable` with an `ETag` (and `304`), `nosniff` and the artifact
  `sandbox` CSP. It is served under Access like the API, owner only, not by a signed link.
- **Colors on the wire are lower case** (`teal` … `slate`), like `theme`; the labels ("Teal")
  are the client's. The hub fills `color` when none is stored: index FNV-1a-32 of the id's 16
  bytes mod 8, so every client agrees without a hash of its own; a color the client does not
  know reads as `teal`.
- **Emoji are not verified to be emoji**: 1–8 characters, at most 32 bytes, no spaces or control
  characters (a ZWJ family is 7 characters, a flag 2). The dialog offers the 16 of the mockup.
  Letters: 1–2 characters after upper-casing.
- `Me` is now typed in `hqc-proto` (`api.me.json`), as the task asked for the changed objects.

## Client

- `src/theme.ts` is `theme.js` in TypeScript with the same values (`tests/theme.test.ts` imports
  the mockups' file and compares every base × color), plus `statusVars` (the fixed colors as
  `--waiting`, `--online`, `--danger`, … from `STATUS`, so there is one source) and
  `applyTheme`. The variables go on `document.documentElement`, not on `#app`, so `html` and
  `body` take `--bg` too; `<meta name="theme-color">` follows `--side`.
- The old palette is gone from `styles.css` (`:root` keeps only the fonts). The mapping:
  `--panel`, `--code-head` → `--surface`; `--line*`, `--code-line` → `--border` (column
  dividers `rgba(0,0,0,.3)` as in the mockups); `--hover`, `--active` → `--row-hover`, a current
  row `--row-on`; `--text-2`, `--text-3` → `--text2`; `--muted-2`, `--faint` → `--muted` (meta text
  is brighter than before, which is the point of "low in contrast"); `--link`, `--accent-text` →
  `--accent`; `--warn*` → `--waiting`, `--permission-bg`, `--permission-border`; `--ok` →
  `--online`; `--deny` → `--danger`; `--user` → `--bubble`; hard-coded colors moved onto the
  tokens.
- **Live preview inside the dialog only**: the dialog element takes the variables of the pending
  choice (so its buttons and the preview change at once); the app behind the scrim changes on
  "Save". The preview's rows are sample rows, not the project's sessions; the other two tiles
  are the user's real projects.
- **Routes**: `#/p/<id>/workspaces` is replaced by `#/p/<id>/settings` (no redirect); a new
  project opens its settings, as it opened its workspaces before.
- **"Make default"** stays on a workspace row: the mockup has only the "default" tag, but without
  the button the default could not be changed any more.
- **Phone**: the header has the gear to the project settings (from `MList2`) next to search and the
  avatar; the avatar (the app settings: notifications, workers) stays, the mockup does not show
  it but nothing else leads there. A chip of a project without an icon shows a dot of its color
  before the name instead of the initials (initials before the same name read twice).
- **Sizes**: letters in a bar tile are 17 px (the mockup draws icons at 21 px, fine for an emoji,
  too wide for two capitals); global buttons keep their height (40 px; the mockup's are 36 px),
  so the chat's buttons do not change in this task; the pages use the small size (32–34 px).
- The client checks an image's type and size before uploading; the size in pixels only the hub
  checks (it needs decoding). Drop and the file picker work; paste does not.

## Not done here (by the task's split)

Row actions on hover, the "Archived «…» · Undo" toast, Custom and Reorder, archive mode (T50; the
State/Recent switch and the "Archive N" toggle stay as they were); "Rules · N hqc" on a
workspace row and the rules screen (T48); the violet blocks between sessions (T51, the
`--relay` variables are already set). The "Requests" link and the "Workers" block are the
existing components on the new tokens.
