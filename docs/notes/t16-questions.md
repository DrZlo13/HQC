# T16: artifacts — decisions and open questions

Code: hub `crates/hqc-hub/src/artifacts.rs` (+ `security.rs`, migration `0007_artifacts.sql`),
worker `crates/hqc-worker/src/inbox.rs` (+ `supervisor.rs`, `adapter/claude_code.rs`,
`engine.rs`), client `client/src/artifacts.ts`, `client/src/ui/attachments.tsx`,
`Chat.tsx`, `Markdown.tsx`. Tests: `crates/hqc-hub/tests/artifacts.rs`,
`crates/hqc-worker/tests/worker.rs` (`inbox_*`), `client/tests/attachments.test.tsx`.

## Decided without asking

1. **Deduplication by `(owner_id, sha256, name)`**, not by the hash alone. The content on disk
   is one per hash for everyone, but a record with a different name is a different record:
   otherwise the same file uploaded as `copy.png` would reach the agent and the feed under the
   old name. A repeat with the same name is `200` and the existing object, a new one is `201`.
2. **Files from the client always go to `user.message.artifact_ids`**, including without text
   (the text is empty). §4 says "a separate `attachment` event — for files without text", but
   `attachment` is not delivered to the agent (it is not input), and a user who attaches a
   file without words clearly wants the agent to see it. The `attachment` event remains for
   `hqc_attach` (iteration 3). I propose fixing the wording of §4 — question 1.
3. **`GET /api/artifacts/{id}`** was added: `user.message` has only the id, and the client
   needs `kind`, the name and the size to render it.
4. **Signing key**: `HQC_ARTIFACT_KEY` or a random one per start. Links live 10 minutes, and
   the client re-requests them on `403`/`410`, so a key change on restart is harmless.
5. **Without `HQC_ARTIFACT_PUBLIC_BASE` links are relative** (`/artifacts/...`) and go to the
   hub's domain behind Access: the browser sends the Access cookie, images load from `'self'`.
6. **`&dl=1`** is outside the signature: the parameter can only make the response an
   `attachment`, i.e. safer, so it does not need signing. The "Download" button uses it,
   because browsers ignore the `download` attribute on a link to another origin.
7. **The artifacts host serves only `/artifacts/*`** (and `/health`): it is outside Access,
   and without this the client and `/api` would open on it (the latter still requires a JWT,
   but extra surface is pointless).
8. **`frame-ancestors` is not set for HTML on the artifacts domain**: the hub does not know
   its own origin (there is no variable with the hub's public URL). The link is signed and
   lives 10 minutes, the page is sandboxed without an origin, so embedding by a foreign site
   gains nothing. If `HQC_PUBLIC_URL` appears, set `frame-ancestors <hub>`.
9. **Worker permissions**: only an artifact that came in an input of a session of this worker
   is served (table `artifact_sessions`), without a TTL. Another worker of the same user gets
   `404`. A wrong token counts toward the same failed-attempt limit as `/worker/ws`.
10. **Paths in the input text are relative** (`.hqc/inbox/<event_id>/<name>`) — from the
    agent's working directory; Claude Code opens them just like absolute ones, while the text
    is shorter and does not reveal the machine's layout in the history.
11. **The download runs in the session's task** before the adapter's `input`: while 100 MB
    downloads, a `stop` of this session waits. The turn has not started yet, so there is
    nothing to stop; other sessions are not blocked.
12. **Images as an image block**: only `png/jpeg/gif/webp` up to 5 MB (the Messages API
    per-image limit); the rest and large ones only by path. The block format is taken from
    the Messages API and was not checked on a live CLI (`HQC_LIVE_CLAUDE` was not required in
    the task) — **[verify]** in SPEC §8.1. How the CLI behaves if it does not accept the
    `content` array is unknown: worth running `claude_live` with an image (two turns are not
    needed, one is enough).
13. **The preview before sending is a `data:` URL**, not `blob:`: the hub's CSP
    (`img-src 'self' data:`) does not allow `blob:`, and §20 could not be changed in this
    task. Only images up to 10 MB get a preview.
14. **File links in markdown**: a path in inline code (`src/main.rs:42`, `/abs/x.ts`,
    `main.rs` with a known extension) and a relative link (`[x](src/main.rs)`) become a button
    that copies the path as written (with `:42`; for a link, its target). An absolute link
    `[x](/api/me)` is still text (T11 #1: an address relative to the hub is misleading). A
    bare word without `/` counts as a path only with an extension from the list, so that
    `self.method` does not become a button.
15. **```` ```diff ```` and ```` ```patch ```` code blocks in messages** are highlighted too —
    it is the same renderer as for a diff attachment.

## Not done

- Deletion and garbage collection of artifacts (neither records nor files): §17.4 says
  nothing about it.
  Resolved in T21: a hub cleanup once a day, SPEC 0.23 §12.
- S3 storage; Range requests (video/large files are re-downloaded from the start).
- `hqc_attach`, `PUT /worker/artifacts`, `tool.result.artifact_id` from adapters — iteration 3.
- The client's mock hub (`client/mock/server.mjs`) does not support `/api/artifacts`: in
  `npm run dev:mock` sending with a file ends in an upload error.
- Limits on the number and total size of files in one message on the client (hub: up to 32).

## Open questions

1. **§4 and files without text** — change it to "the user's attachments are always in
   `user.message.artifact_ids` (the text may be empty); `attachment` is for files shown in the
   feed without delivery to the agent (`hqc_attach`)"? §4 could not be edited in this task.
   Resolved in T18: written that way in §4 as a **[decision]** (SPEC 0.19).
2. **Migration number and spec versions**: T15 and T17 run in parallel; if they also added
   `0006_*.sql` or raised SPEC to 0.17, renumbering is needed on merge (sqlx requires unique
   versions) and the §23 entries must be merged.
   Resolved in T18: `0006` — T17, `0007_artifacts.sql` — T16, `0008_mirror.sql` — T15; §23 —
   0.16 → 0.17 (T17), 0.17 → 0.18 (T16), 0.18 → 0.19 (T15).
3. **Deleting artifacts**: when a session/project is deleted, delete the artifacts that no
   other session refers to, and files without records? Proposal: periodic cleanup by the hub
   based on `artifact_sessions` and age.
   Resolved in T21: once a day, artifacts that no event refers to and that are older than
   `HQC_ARTIFACT_RETENTION_DAYS` (30) are deleted; a file — when there are no records for its
   sha256 (§12).
4. **`.hqc/inbox` grows** in the working directory with every input that has files. Clean by
   age (e.g. older than 30 days) at session start?
   Resolved in T21: on archiving and deleting a session the hub sends `session.cleanup`, and
   the worker deletes the inbox folders of its inputs; at worker start — all inbox folders
   older than 14 days in known workspaces (SPEC 0.23 §17.4).
5. **A per-user quota** (total artifact volume) — is it needed before iteration 6 (multiple
   users)?
6. **CSP §20** lists the hub's headers literally; with `HQC_ARTIFACT_PUBLIC_BASE` the
   artifacts origin is added to `img-src`/`frame-src`/`connect-src` (described in §12). Move
   this to §20?
