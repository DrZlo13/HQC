# T8 — the hqc-hub Docker image and the ghcr.io build: decisions and open questions

The task was done without a human: the Dockerfile and compose were checked locally on home-mac
(OrbStack, linux/arm64, native), the `image.yml` workflow only with `actionlint`; it has not
run on GitHub yet (the push and the package settings are up to a human). `docs/SPEC.md` was not
changed: the hub in the container listens on `0.0.0.0` inside its own network and is visible
from outside only through the tunnel, which is the same §20 rule ("only the tunnel
interface"); serving the client from the hub is described in RUNBOOK §1.7, not in the spec.

## Dockerfile

1. **Debian trixie in all stages** (`rust:1-slim-trixie`, `node:22-trixie-slim`,
   `debian:trixie-slim`). The hub links against the system OpenSSL: `web-push` → `ece` →
   `openssl-sys` (rustls only in reqwest and sqlx). Build and runtime on the same Debian
   release, so that glibc and `libssl3t64` are guaranteed to match. That is why the runtime has
   `libssl3t64` besides `ca-certificates`.
2. **`rust:1`, not a pinned version**: it matches `channel = "stable"` in
   `rust-toolchain.toml` and `dtolnay/rust-toolchain@stable` in CI (1.99.1 as of 2026-10-22).
   `rust-toolchain.toml` itself is kept out of the build context (`.dockerignore`): the image's
   toolchain is named `1.99.1-<triple>`, not `stable`, and rustup would download a second one.
   The downside: on CI the image may be built by a newer compiler than last time. If
   reproducibility is needed, pin `rust:1.99-slim-trixie` and bump it by hand. Open question.
3. **The dependency cache uses stub crates, not `--mount=type=cache`.** `cache-to: type=gha`
   saves layers but not the contents of cache mounts; on a clean GitHub runner such a cache
   would always be empty. The layer "dependencies built from `Cargo.toml`/`Cargo.lock` with
   empty `lib.rs`/`main.rs`" is cached in gha and rebuilt only when the manifests change.
   After the real sources are copied, all `*.rs` are `touch`ed; otherwise cargo may consider
   the stub newer (COPY keeps the mtime from the context). No cargo-chef: an extra third-party
   image/binary for four crates is not needed. When a crate is added to the workspace, its
   `Cargo.toml` must be added to the Dockerfile as a separate `COPY` line; otherwise the build
   fails at once (cargo will not find the workspace member), so it will not break silently.
4. **The client is built on `$BUILDPLATFORM`**: the result is static files, so there is no
   point running node under emulation when cross-building. Node 22, not 20: `vitest` 5
   requires 22+, `vite` 8 requires 20.19+; the client CI job (item 9) uses 22 as well.
5. **User `hqc` 10001:10001**, with no home directory and no shell. The hub writes nothing to
   disk: checked with `read_only: true` in compose; the RUNBOOK example sets this flag.
6. **`NO_COLOR=1` in `ENV`.** `tracing-subscriber` 0.3.23 writes ANSI colours by default even
   without a TTY; `docker logs`/journald got `\e[2m…`. The library supports the variable, so
   `main.rs` did not need changing. Not on the task list; added after the local run.
7. **No HEALTHCHECK** (as the task allows): `curl` was not pulled into the image, and no
   `hqc-hub --health` was made. Without a database the hub exits at once (no connection
   retries), and `restart: unless-stopped` restarts it; compose waits for the database's
   `pg_isready` through `depends_on`.
8. The OCI labels `source`, `revision`, `version` (+ `title`, `description`, `licenses`) are
   set in the Dockerfile via `ARG VERSION`/`REVISION`; in CI the `metadata-action` labels
   override them with the correct values.

## Workflow

9. **A `client` job was added to `ci.yml`** (`npm ci`, `npm test`, `npm run build`). CI used to
   check only Rust, and the image now includes the client: without this job, "the image after
   a green CI" would not guarantee that the client builds at all (the build in the Dockerfile
   would fail only in image.yml, after the push to master).
10. **Triggers**: `workflow_run` on `ci` (`completed`, `branches: [master]`), `push` of `v*`
    tags, `workflow_dispatch`. For `workflow_run` the job runs only when
    `conclusion == success`, `event == push` and `head_repository == github.repository`:
    the `branches` filter compares only the branch name, so a PR from a fork's `master` branch
    would otherwise trigger the build too.
11. **`v*` tags and manual runs also wait for a green CI**: the first step polls
    `gh run list -w ci.yml -c <sha>` every 30 s for up to 30 minutes; a successful `ci` on this
    commit lets it continue; all finished without success, or the timeout, fails the job. This
    way a tag pushed together with a commit waits for its `ci` run (`ci` runs on tags too:
    `on: push` without a filter). Needs `permissions: actions: read`.
12. **The image tags are computed explicitly**, not `type=sha`/`type=ref`: under
    `workflow_run` the `metadata-action` context is the head of the default branch, not the
    commit that passed CI. The checkout is also by `workflow_run.head_sha`. `master` is set on
    `workflow_run` and on a manual run from the master branch; a manual run from another branch
    gives only `sha-…`.
13. **A `vX.Y.Z` tag must match `workspace.package.version`** in `Cargo.toml`, otherwise the
    job fails before the build: otherwise an image `0.2.0` would print `hqc-hub 0.1.0`.
    `latest` and `X.Y` are only for release `vX.Y.Z`; `vX.Y.Z-rc.N` gets only `X.Y.Z-rc.N`
    and `sha-…`.
14. After the push, a smoke test: `docker run --rm <image>:sha-… --version` must print
    `hqc-hub <version from Cargo.toml>`.
15. Platform: only `linux/amd64`, as in the task. The Dockerfile does not pin the
    architecture; arm64 is added with a line in `platforms:` (but under QEMU the Rust build
    will take tens of minutes).

## Documentation

16. RUNBOOK §1.8 is a new section; §0, §1.2 (`HQC_HUB_LISTEN`), §1.3, §1.6, §1.7, §4 got only
    links to it and one correction: in §0 "Node.js 20+" → "20.19+ for the build, 22+ for the
    tests" (per `engines` in `package-lock.json`).
17. The compose example has two env files: `db.env` (`POSTGRES_*`) and `hub.env` (`HQC_*`), so
    that the database does not get the hub's secrets and vice versa. A hex password is
    recommended: it does not need URL-escaping in `HQC_DATABASE_URL`.
18. The `edge` network in the example is external, in case cloudflared/Caddy live in a
    different compose project (another agent installs them; I do not know how exactly). If in
    the same one, use `default` instead. A question for whoever deploys the DMZ.
19. The upgrade and rollback commands (`pg_dump -Fc` via `docker compose exec -T`,
    `pg_restore --clean --if-exists` with the hub stopped) were run on a local setup: a row
    deleted after the dump came back after the restore, and the hub started.
