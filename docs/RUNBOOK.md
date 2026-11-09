# hqc — how to set it up yourself (iteration 1)

For whoever installs hqc on their own machines: the hub in a home DMZ behind Cloudflare Tunnel,
a worker on a Mac, the client in a phone browser. Architecture and protocol are in `docs/SPEC.md`
(§2, §5, §6, §20); this file covers operations only. Every command, variable and config field
below comes from the code (`crates/hqc-hub/src/config.rs`, `crates/hqc-worker/src/config.rs`,
`crates/hqc-worker/src/main.rs`); anything not here does not exist in iteration 1.

Diagram:

```
phone / browser ──HTTPS──> Cloudflare (Access) ──Tunnel──> cloudflared ──> hqc-hub :8080 ──> Postgres
                                                                             ^
home-mac: hqc-worker run ──HTTPS/WSS /worker/* (no Access, worker token)─────┘
          └─ claude -p (subscription logged in on this machine)
```

## 0. Build

You need Rust stable (the version is pinned in `rust-toolchain.toml`, `rustup` installs it
itself) and Node.js 20.19+ to build the client (22+ for its tests). You do not need to build the
hub for the DMZ: there is a ready Docker image with the client inside (§1.8).

```sh
git clone <repo> hqc && cd hqc
cargo build --release -p hqc-hub -p hqc-worker   # target/release/hqc-hub, target/release/hqc-worker
target/release/hqc-hub --version                  # hqc-hub 0.1.0
target/release/hqc-worker --version               # hqc-worker 0.1.0
cd client && npm ci && npm run build              # client/dist
```

Build the hub on the OS and architecture it will run on (for a Linux DMZ host, on that host or
in CI; CI builds the Docker image, §1.8), the worker on the Mac. The binaries are self-contained:
the Postgres client is built in, and so is the worker's SQLite outbox.

## 0.1 Local development without Cloudflare

`tools/dev.sh up` builds the hub and the worker in the dev profile, starts the hub in dev mode on
`127.0.0.1:8080` (database `hqc_dev`, user `dev@local`, client from `client/dist`),
on the first run enrolls the worker with a separate `HQC_HOME` in `tmp/dev/home` and starts
it, and once the worker is online adds, over REST, a workspace with the repository path
(`DEV_WORKSPACE`, inside `allowed_roots` = `DEV_ROOTS`) to the personal project of `dev@local` if it
is not there yet; other workspaces are added in the client. `down`, `restart`, `status`,
`logs`, `reset` do what they say. State persists between runs. From another machine, open the hub
through an ssh tunnel: `ssh -N -L 8080:127.0.0.1:8080 <user>@<host>`, then `http://127.0.0.1:8080`.
The hub listens on loopback only, and in dev mode this is mandatory (§6). The worker runs
`claude`, so `up` must be run from a session with an unlocked Keychain (§2.3).

## 1. Hub in the DMZ

### 1.1 Postgres

The hub works with Postgres 17 (verified). Since migration `0016` (T33, search) it needs the
`pg_trgm` extension: it ships with Postgres (contrib, in the Debian/Ubuntu `postgresql`
package, Homebrew and the `postgres` image) and is trusted, so the migration creates it as the
database owner (`hqc` below); no superuser step. The connection
has no TLS (sqlx is built without TLS), so the database must be on the same host as the hub
(or in a trusted network, without `sslmode=require` in the connection string).

```sh
# Linux (Debian/Ubuntu), as an administrator:
sudo -u postgres createuser --pwprompt hqc
sudo -u postgres createdb -O hqc hqc
# macOS + Homebrew:
brew install postgresql@17 && brew services start postgresql@17
createdb hqc
```

**Migrations** are applied automatically on every hub start (embedded in the binary from
`crates/hqc-hub/migrations`, table `_sqlx_migrations`). There is no separate command;
`sqlx-cli` is not needed. Before upgrading the hub, back up: `pg_dump -Fc hqc > hqc-$(date +%F).dump`.

### 1.2 Hub environment variables

The hub is configured by environment variables only; it takes a single command-line argument,
`--version`. An empty value counts as unset.

| variable | required | what it is |
|---|---|---|
| `HQC_DATABASE_URL` | yes | Postgres connection string, e.g. `postgresql://hqc:<password>@127.0.0.1/hqc` |
| `HQC_HUB_LISTEN` | no, `127.0.0.1:8080` | HTTP address and port. The hub listens only on the interface `cloudflared` connects to (§20); with the tunnel on the same host, loopback. In the Docker image it is `0.0.0.0:8080` inside the container, and the port is not published (§1.8) |
| `HQC_ACCESS_TEAM_DOMAIN` | yes (without `HQC_DEV` or `HQC_OIDC_*`) | Cloudflare Zero Trust team domain, `<team>.cloudflareaccess.com` (`https://` and `/` are allowed; the hub strips them). From it the hub derives the issuer `https://<team>.cloudflareaccess.com` and the JWKS `https://<team>.cloudflareaccess.com/cdn-cgi/access/certs` |
| `HQC_ACCESS_AUD` | yes (without `HQC_DEV` or `HQC_OIDC_*`) | Application Audience (AUD) tag of the hub's Access application; the hub checks `aud` in the `Cf-Access-Jwt-Assertion` JWT |
| `HQC_OIDC_ISSUER` | instead of `HQC_ACCESS_*` | OIDC mode (SPEC §6), for a hub behind a reverse proxy that signs people in with an OIDC provider: the exact `iss` of the provider's ID tokens, e.g. `https://auth.example.com/realms/<realm>` for Keycloak; `https://`, `http://` only to a loopback host. Set together with the three below, or none of them; OIDC mode also requires `HQC_CLIENT_IP_HEADER`; with `HQC_ACCESS_*` or `HQC_DEV=1` the hub does not start. The hub reads the token from `Authorization: Bearer` on `/api/*` and `/api/ws`; the proxy must renew it before it expires (oauth2-proxy `--set-authorization-header` with `--cookie-refresh` shorter than the token lifetime, e.g. `4m` against Keycloak's default 5-minute Access Token Lifespan), otherwise `/api/*` answers `401` a few minutes after sign-in |
| `HQC_OIDC_AUD` | with `HQC_OIDC_ISSUER` | the expected `aud` of the ID token: the client id the proxy signs in with |
| `HQC_OIDC_JWKS_URL` | with `HQC_OIDC_ISSUER` | the provider's JWKS, e.g. `https://auth.example.com/realms/<realm>/protocol/openid-connect/certs` for Keycloak; no discovery. `https://`, `http://` only to a loopback host: over plain http anyone on the way can replace the signing keys |
| `HQC_OIDC_ALLOWED_EMAILS` | with `HQC_OIDC_ISSUER` | comma-separated emails of the people the hub lets in, e.g. `you@example.com`, compared case-insensitively. Any other account of the realm with an ID token for the client gets `401` and no hub user, whichever way the token reaches the hub. Empty, and the hub does not start |
| `HQC_VAPID_PRIVATE` | no | VAPID private key for Web Push: a 32-byte P-256 scalar, base64url without `=` (§1.5). Set together with `HQC_VAPID_PUBLIC` or not at all; otherwise the hub does not start |
| `HQC_VAPID_PUBLIC` | no | VAPID public key: an uncompressed P-256 point (65 bytes), base64url without `=`. Served by `GET /api/push/vapid`. Without the key pair push is off, and the client says "VAPID keys are not configured on the hub" |
| `HQC_VAPID_SUBJECT` | no, `mailto:hqc@localhost` | `sub` in the VAPID signature: `mailto:` or `https:`. Set a real address: push services (Apple especially) may reject signatures with a made-up one |
| `HQC_CLIENT_DIR` | no | directory of the built client (`client/dist`). If set, the hub serves the client at `/` on the same domain; without an `index.html` in it the hub does not start. If unset, the hub serves only `/api/*`, `/worker/*` and `/health` |
| `HQC_DEV` | no | `1`: dev mode **without Access**; all `/api/*` requests come from `HQC_DEV_USER`. For local development only: the hub refuses to start if any `HQC_ACCESS_*` is also set or `HQC_HUB_LISTEN` is not loopback (`127.0.0.0/8`, `[::1]`, `localhost`). Do not use in the DMZ |
| `HQC_DEV_USER` | with `HQC_DEV=1` | email of the dev-mode user; without `HQC_DEV=1` it is ignored with a warning |
| `HQC_ARTIFACT_DIR` | no, `artifacts` (in the Docker image `/var/lib/hqc/artifacts`) | artifact storage directory (SPEC §12): files by sha256, `<dir>/<first 2 chars>/<sha256>`, and `tmp/` for uploads. The hub creates it on start and does not start if it cannot. Back it up together with the database (SPEC §20). In a container it must be a volume: otherwise the hub logs `is in the container's own layer, not a volume` at start, and the files go with the next deploy Unused artifacts are removed by a daily cleanup (`HQC_ARTIFACT_RETENTION_DAYS`) |
| `HQC_ARTIFACT_RETENTION_DAYS` | no, `30` | artifact cleanup (SPEC §12, T21): once a day (and right after start) the hub deletes artifact records that no event references and that have not been uploaded or attached to an input for this many days; the file is deleted from disk once no records for its sha256 remain. Whole days; `0` means never delete |
| `HQC_ARTIFACT_PUBLIC_BASE` | no | origin of a separate artifact domain, `https://artifacts.<hub domain>` without a path (`http://` only for loopback). Signed links point there, and HTML is rendered in an iframe only from there; the hub adds this origin to `img-src`/`frame-src`/`connect-src` of its CSP, and on this host serves only `/artifacts/*` (everything else is `404`). The domain is the same tunnel to the same hub, **outside** Access (§1.6). If unset, links are relative, on the hub's domain (behind Access), and HTML can only be downloaded |
| `HQC_ARTIFACT_KEY` | no | HMAC key for signed links, at least 32 characters (`openssl rand -hex 32`). If unset, a random one per run: links from the previous run stop working, and the client just requests new ones. Needed only if links must survive a restart |
| `HQC_PUBLIC_ORIGINS` | no | comma-separated origins browsers open the hub at: `https://hqc.example.com`. When set, `Origin` on `/api/*` (WebSocket and writes) is checked against this list instead of the request's `Host`. Needed if a proxy in front of the hub rewrites `Host` to the container name (Caddy with `header_up Host`, cloudflared with `httpHostHeader`) and does not set `X-Forwarded-Host`; the symptom is `WebSocket connection to wss://…/api/ws failed` in the browser and `Origin does not match the hub's host` in the hub log |
| `HQC_CLIENT_IP_HEADER` | in OIDC mode | the header a reverse proxy in front of the hub puts the client's address in, from the TCP peer, e.g. `X-Real-Ip` for Traefik (§1.9). When set, the per-address limits on `/worker/*` (SPEC §20) take the address from this header (the TCP peer when it is absent), and the hub reads neither `CF-Connecting-IP` nor `CF-IPCountry` (the country on the enrollment screen). If unset: `CF-Connecting-IP` (Cloudflare Tunnel, §1.6), otherwise the TCP peer; in OIDC mode the hub then does not start |
| `HQC_TURN_WATCHDOG_S` | no, `900` | turn watchdog (SPEC §17.3), whole seconds: if a session is `running` and the worker has sent no event for it (including `agent.delta`) for longer than this, the hub closes the turn with `system turn_lost` "no events for N s", and the session stays in `error` until the next input. Waiting for an answer to approval/clarify and the worker being offline do not count. `0` turns it off |
| `HQC_MIRROR_SESSIONS_PER_DAY` | no, `200` | sessions the mirror may import from one worker's Claude Code transcripts in 24 h (SPEC §9.1, §20), a whole number; over it the worker reads the transcript again an hour later, and `0` imports no new transcript (known ones go on). Raise it to import the history of many folders at once |
| `RUST_LOG` | no, `info` | log level (`tracing`), e.g. `hqc_hub=debug` |

There is no master key for `api_keys` (§3, §20) in iteration 1: user keys are not stored.

The hub needs outbound HTTPS: to `<team>.cloudflareaccess.com` (JWKS) and to browser push
services (`fcm.googleapis.com`, `web.push.apple.com`, `updates.push.services.mozilla.com`
etc.).

Check after start: `curl http://127.0.0.1:8080/health` → `ok`; in the log
`hqc-hub listening addr=… protocol=1`.

### 1.3 systemd (Linux)

Without Docker; with Docker, see §1.8.

`/etc/hqc/hub.env` (mode `0600`, owner `root`; the file holds the database password and the VAPID key):

```sh
HQC_DATABASE_URL=postgresql://hqc:<password>@127.0.0.1/hqc
HQC_HUB_LISTEN=127.0.0.1:8080
HQC_ACCESS_TEAM_DOMAIN=<team>.cloudflareaccess.com
HQC_ACCESS_AUD=<AUD tag>
HQC_VAPID_PRIVATE=<base64url>
HQC_VAPID_PUBLIC=<base64url>
HQC_VAPID_SUBJECT=mailto:<you>@<domain>
HQC_CLIENT_DIR=/opt/hqc/client
RUST_LOG=info
```

`/etc/systemd/system/hqc-hub.service`:

```ini
[Unit]
Description=hqc hub
After=network-online.target postgresql.service
Wants=network-online.target
Requires=postgresql.service

[Service]
User=hqc
Group=hqc
EnvironmentFile=/etc/hqc/hub.env
ExecStart=/usr/local/bin/hqc-hub
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

```sh
sudo useradd --system --no-create-home hqc
sudo install -m 0755 target/release/hqc-hub /usr/local/bin/
sudo mkdir -p /opt/hqc && sudo cp -r client/dist /opt/hqc/client
sudo systemctl daemon-reload && sudo systemctl enable --now hqc-hub
journalctl -u hqc-hub -f            # logs (the hub writes to stdout)
```

The hub reads the client files on every request: a new `client/dist` build in
`HQC_CLIENT_DIR` is picked up without a restart.

### 1.4 launchd (if the hub is on a Mac)

`/Library/LaunchDaemons/local.hqc.hub.plist` (owner `root:wheel`, mode `0600`: it contains
secrets); `UserName` is the unprivileged user the hub runs as:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>local.hqc.hub</string>
  <key>UserName</key><string>hqc</string>
  <key>ProgramArguments</key>
  <array><string>/usr/local/bin/hqc-hub</string></array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HQC_DATABASE_URL</key><string>postgresql://hqc:PASSWORD@127.0.0.1/hqc</string>
    <key>HQC_HUB_LISTEN</key><string>127.0.0.1:8080</string>
    <key>HQC_ACCESS_TEAM_DOMAIN</key><string>TEAM.cloudflareaccess.com</string>
    <key>HQC_ACCESS_AUD</key><string>AUD_TAG</string>
    <key>HQC_VAPID_PRIVATE</key><string>BASE64URL</string>
    <key>HQC_VAPID_PUBLIC</key><string>BASE64URL</string>
    <key>HQC_VAPID_SUBJECT</key><string>mailto:you@example.com</string>
    <key>HQC_CLIENT_DIR</key><string>/usr/local/share/hqc/client</string>
    <key>RUST_LOG</key><string>info</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>StandardOutPath</key><string>/usr/local/var/log/hqc-hub.log</string>
  <key>StandardErrorPath</key><string>/usr/local/var/log/hqc-hub.log</string>
</dict>
</plist>
```

```sh
sudo launchctl bootstrap system /Library/LaunchDaemons/local.hqc.hub.plist
sudo launchctl kickstart -k system/local.hqc.hub     # restart
sudo launchctl bootout system/local.hqc.hub          # stop
```

The log directory must exist and be writable by the `hqc` user.

### 1.5 VAPID keys

You need a P-256 pair in "raw" form, base64url without `=`. With `openssl` (this is also how
`tools/e2e-local.sh` makes them; the hub accepts such keys):

```sh
openssl ecparam -name prime256v1 -genkey -noout -out vapid.pem
b64url() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }
echo "HQC_VAPID_PRIVATE=$(openssl ec -in vapid.pem -outform DER 2>/dev/null | tail -c +8 | head -c 32 | b64url)"
echo "HQC_VAPID_PUBLIC=$(openssl ec -in vapid.pem -pubout -outform DER 2>/dev/null | tail -c 65 | b64url)"
```

Generate the keys once. Changing the keys invalidates all existing subscriptions: on every
device you will have to press "Subscribe to notifications" again. `vapid.pem` can be deleted
once the values are in `hub.env`.

### 1.6 Cloudflare Tunnel and Access

The hub is published under one name, e.g. `hqc.example.com`. People (`/`, `/api/*`,
including the WebSocket `/api/ws`) come in only through Access; workers (`/worker/*`) without
Access, let in by the worker token (§6, §20).

**Tunnel.** `cloudflared tunnel login`, `cloudflared tunnel create hqc`,
`cloudflared tunnel route dns hqc hqc.example.com`. Config `/etc/cloudflared/config.yml`
(two routes to the same hub; the split into "with Access" and "without Access" is done by the
Access applications below; the `/worker/` route is separated explicitly to make it visible and
so it can be pointed at a separate port if you want):

```yaml
tunnel: <tunnel UUID>
credentials-file: /etc/cloudflared/<tunnel UUID>.json
ingress:
  - hostname: hqc.example.com
    path: ^/worker/
    service: http://127.0.0.1:8080
  - hostname: hqc.example.com
    service: http://127.0.0.1:8080
  - service: http_status:404
```

`sudo cloudflared service install` installs it as a service (systemd or launchd). Hub in Docker
(§1.8): `service: http://hqc-hub:8080` for cloudflared in the same Docker network. The tunnel
passes WebSocket with no extra settings; long-lived connections are kept up by the hub and
worker heartbeat (20 s) and by reconnecting.

**Access** (Zero Trust → Access → Applications): two self-hosted applications on the same
domain:

1. `hqc`: domain `hqc.example.com`, empty path (the whole site). An **Allow** policy for your
   emails (or a group). Session Duration: long (e.g. 1 month); otherwise the PWA installed on
   the phone will often open on the login page.
2. `hqc-worker`: domain `hqc.example.com`, path `worker`. A **Bypass** policy, Include:
   Everyone. The more specific path wins, so `/worker/*` bypasses Access.

**Cookie of the `hqc` application** (Applications → `hqc` → Configure → Settings → Cookie
settings): **SameSite = Lax**, **HttpOnly** on, **Binding Cookie** off: with it Cloudflare does not pass WebSocket through Access (verified 2026-10-23). By
default the `CF_Authorization` cookie is sent with `SameSite=None`, and a foreign page in the
browser of someone logged in to hqc can open `wss://hqc.example.com/api/ws` with that cookie. The
hub itself rejects WebSocket and write requests with a foreign `Origin` (SPEC §20); Lax is the
second line of defence. After the change, check that the PWA installed on the phone opens
without logging in again.

**Rate limiting on `/worker/*`** (Security → WAF → Rate limiting rules, on the domain's zone):
a rule "URI Path starts with `/worker/`", characteristic IP, e.g. 60 requests per
10 seconds → Block for 1 minute; a separate, stricter one for `/worker/enroll` (e.g. 10 per
minute). The hub itself also rate-limits `/worker/*` by `CF-Connecting-IP` (SPEC §20); the
Cloudflare rule cuts off a flood before it reaches the tunnel and the database. Leave URL
normalization (Rules → Settings → Normalize incoming URLs) on: the Bypass for `/worker/`
must not be reachable with paths like `/worker/../api/me`.

**Artifact domain** (optional, SPEC §12): a separate name, e.g.
`artifacts.hqc.example.com`: one more `hostname` in the tunnel's `ingress` to the same
`http://127.0.0.1:8080` and `cloudflared tunnel route dns hqc artifacts.hqc.example.com`.
Do **not** put Access on this name (or use Bypass, Everyone): access is granted by the link
signature with a 10-minute TTL. In the hub, `HQC_ARTIFACT_PUBLIC_BASE=https://artifacts.hqc.example.com` (§1.2):
only with it are HTML attachments shown in a sandboxed iframe; on this name the hub serves only
`/artifacts/*`. Everything works without a separate domain: links go to the hub's domain behind
Access, and HTML can only be downloaded.

Where to get the values for the hub:
- `HQC_ACCESS_AUD`: Applications → `hqc` → Configure (Overview) → **Application Audience
  (AUD) Tag**. It is the tag of the `hqc` application specifically (not `hqc-worker`).
- `HQC_ACCESS_TEAM_DOMAIN`: Settings → Custom Pages (or Team domain in the general Zero Trust
  settings): `<team>.cloudflareaccess.com`.

Check from outside: `https://hqc.example.com/` shows the Access login screen, then the client;
`curl -i https://hqc.example.com/worker/enroll -X POST -H 'content-type: application/json'
-d '{}'` returns a hub response (a body parse error, `4xx`), not a redirect to the Access login.
A request to `/api/me` bypassing Access (without the `Cf-Access-Jwt-Assertion` header) is
rejected by the hub itself. The request body limit through the tunnel is ~100 MB (§20).

**Turn HTTP/3 off in the zone** (Network → HTTP/3 with QUIC → Off). Verified 2026-10-23:
Chrome, having opened the site over HTTP/3, sets up the WebSocket inside the same QUIC session
(extended CONNECT, RFC 9220), and the Cloudflare edge does not carry such a stream to the origin
and resets it without a response. Symptoms: in the console `WebSocket connection to wss://…/api/ws failed:`
with no code, in Network the `ws` entry has an empty Protocol and status "Finished", the Caddy and
hub logs show no requests to `/api/ws` at all, while Safari and the phone work and the worker is
connected over `/worker/ws`. To reproduce from the Chrome console: `new WebSocket('wss://<hub>/worker/ws')`
should give "Unexpected response code: 401" from the hub; over HTTP/3 it also fails with no code,
so Access is not the cause. Temporary check on a single computer:
`chrome://flags/#enable-quic` → Disabled. After HTTP/3 is turned off in the zone, Chrome may keep
using QUIC from its Alt-Svc cache for a few more minutes.

### 1.7 Serving the client

The client is the static files of `client/dist` after `npm run build`. It calls `/api` by a
relative path and routes with `#`, so it must live on the same domain as the API, behind the
same Access application. The simplest way is to have the hub serve it:
`HQC_CLIENT_DIR=/opt/hqc/client` (copy the contents of `client/dist` there). `index.html`,
`sw.js` and the manifest are served with `Cache-Control: no-cache`, `assets/*` files (hashed
names) as immutable. In the Docker image (§1.8) the client is already in `/opt/hqc/client` and enabled.

### 1.8 Hub in Docker

Instead of §1.3: the ready image `ghcr.io/drzlo13/hqc-hub` (linux/amd64; Dockerfile at the
repository root). Nothing is built on the server, only `docker compose pull`. The image is
built by the `.github/workflows/image.yml` workflow, only after a green `ci` on the same commit.

**Tags.**

| tag | when it appears | what for |
|---|---|---|
| `sha-<7 chars of the commit>` | every build | deployment: immutable; to roll back, use the previous `sha-…` |
| `master` | every green push to `master` | floating, "latest master"; do not use in a production compose |
| `X.Y.Z`, `X.Y`, `latest` | tag `vX.Y.Z` (the version must match `workspace.package.version` in `Cargo.toml`) | releases; `vX.Y.Z-rc.N` produces only `X.Y.Z-rc.N` and `sha-…` |

In compose, use a specific `sha-…` or `X.Y.Z`. The version in the image is shown by
`docker run --rm ghcr.io/drzlo13/hqc-hub:<tag> --version` (`hqc-hub 0.1.0`), the commit by the
label `org.opencontainers.image.revision` (`docker image inspect`). The package is public: `pull`
needs no login. If the package is private: `docker login ghcr.io -u <github-login>` with a
classic PAT with only `read:packages`.

**What is in the image.** `/usr/local/bin/hqc-hub` (`ENTRYPOINT`, no arguments needed), the
client in `/opt/hqc/client`, user `hqc` (uid/gid 10001), port 8080, and from packages
`ca-certificates` and `libssl3t64`. Defaults (§1.2; override them in env):

| variable | value in the image |
|---|---|
| `HQC_HUB_LISTEN` | `0.0.0.0:8080` |
| `HQC_CLIENT_DIR` | `/opt/hqc/client` |
| `RUST_LOG` | `info` |
| `NO_COLOR` | `1`: logs without ANSI colours |

You must set: `HQC_DATABASE_URL`, `HQC_ACCESS_TEAM_DOMAIN`, `HQC_ACCESS_AUD` (or the four
`HQC_OIDC_*` and `HQC_CLIENT_IP_HEADER` instead of the last two, §1.9); for push,
`HQC_VAPID_PRIVATE`, `HQC_VAPID_PUBLIC`, `HQC_VAPID_SUBJECT` (§1.5). `HQC_DEV` does not work
in the container and is not needed: with `0.0.0.0` the hub refuses to start in dev mode.

**State.** The hub keeps everything in Postgres; it writes nothing to the container's disk (the
image root can be mounted read-only, `/tmp` is not needed), logs go to stdout. Only the database
needs a volume.

**`0.0.0.0:8080` is safe only as long as the port is not published.** Inside the container
this is the compose network's interface, not the host's; Access (§1.6) protects only what comes
through the tunnel. That is why `hqc-hub` has no `ports:`: `cloudflared` or Caddy reach it by
the name `hqc-hub:8080` in the same network. If they run on the host rather than in a container,
publish on loopback only: `ports: ["127.0.0.1:8080:8080"]`, never `8080:8080` (Docker opens
such a port on all interfaces, bypassing the host's ufw/nftables).

`/srv/hqc/compose.yml`:

```yaml
services:
  hqc-db:
    image: postgres:17
    restart: unless-stopped
    env_file: ./db.env
    volumes:
      - hqc-db:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD", "pg_isready", "-U", "hqc", "-d", "hqc"]
      interval: 5s
      timeout: 5s
      retries: 10

  hqc-hub:
    image: ghcr.io/drzlo13/hqc-hub:sha-<commit>   # or X.Y.Z
    restart: unless-stopped
    depends_on:
      hqc-db:
        condition: service_healthy
    env_file: ./hub.env
    read_only: true
    volumes:
      # Artifact storage (the image's HQC_ARTIFACT_DIR); back it up together with the database.
      - hqc-artifacts:/var/lib/hqc/artifacts
    # no ports: the hub is reachable only through cloudflared/Caddy in this same network
    networks: [default, edge]

volumes:
  hqc-db:
  hqc-artifacts:

networks:
  # Shared network with cloudflared/Caddy from their compose project: docker network create edge.
  # If they are defined in this same compose.yml, remove edge and keep default.
  edge:
    external: true
```

`/srv/hqc/db.env` and `/srv/hqc/hub.env`: mode `0600`, keep them out of git; the password is
the same in both (`openssl rand -hex 24`; hex so it needs no escaping in the URL):

```sh
# db.env
POSTGRES_USER=hqc
POSTGRES_DB=hqc
POSTGRES_PASSWORD=<password>
```

```sh
# hub.env
HQC_DATABASE_URL=postgresql://hqc:<password>@hqc-db/hqc
HQC_ACCESS_TEAM_DOMAIN=<team>.cloudflareaccess.com
HQC_ACCESS_AUD=<AUD tag>
HQC_VAPID_PRIVATE=<base64url>
HQC_VAPID_PUBLIC=<base64url>
HQC_VAPID_SUBJECT=mailto:<you>@<domain>
```

**Check that `hqc-hub` and `hqc-db` have no `ports:` in `compose.yml`** (or only
`127.0.0.1:…`, see above). The hub trusts `CF-Connecting-IP` and `Cf-Access-Jwt-Assertion`
only because it can be reached only through the tunnel: a port open to the outside exposes
`/worker/*` and the static files without Cloudflare, and makes per-address limits spoofable.

The database is in the same compose network with no published port, so a connection without
TLS (§1.1) is acceptable here. `POSTGRES_*` take effect only on the first start on an empty
volume: to change the password later, run `ALTER ROLE hqc PASSWORD …` in the database and edit
both files.

In the tunnel (§1.6), use `http://hqc-hub:8080` instead of `http://127.0.0.1:8080` (cloudflared
in a container in the `edge` network); for Caddy, `reverse_proxy hqc-hub:8080`.

```sh
cd /srv/hqc
docker compose pull && docker compose up -d
docker compose logs -f hqc-hub       # wait for: hqc-hub listening addr=0.0.0.0:8080 protocol=1
docker compose exec hqc-db psql -U hqc -d hqc -c 'select version, description from _sqlx_migrations'
```

The image has no `HEALTHCHECK` (it has no `curl`); `GET /health` → `ok` is checked by the proxy
or by `curl` from a host that can see the hub. If the database is unreachable, the hub exits with
an error, and `restart: unless-stopped` restarts it.

**Upgrade.** Migrations are applied when the new container starts (§1.1), and there is no
migration rollback, so dump first:

```sh
cd /srv/hqc
docker compose exec -T hqc-db pg_dump -U hqc -Fc hqc > backup/hqc-$(date +%F-%H%M).dump
# in compose.yml: image: ghcr.io/drzlo13/hqc-hub:<new tag>
docker compose pull hqc-hub
docker compose up -d hqc-hub
docker compose logs --since 5m hqc-hub
```

Rollback: the previous tag in `compose.yml` and `up -d`. If the new version has already applied
a migration, the old hub may fail to start on that database; then restore the dump:
`docker compose stop hqc-hub`,
`docker compose exec -T hqc-db pg_restore -U hqc -d hqc --clean --if-exists < backup/<file>.dump`,
then `up -d`. The Postgres major version is tied to the volume: `postgres:17` → `18` only
through `pg_dump`/`pg_restore` into a new volume, not by changing the tag.

To build the image yourself: `docker build -t hqc-hub .` at the repository root (on a machine
with the same architecture as the server, or with `--platform linux/amd64`).

### 1.9 Behind Traefik with oauth2-proxy and Keycloak

Instead of Cloudflare Tunnel and Access (§1.6): Traefik sends every request for the site through
a forwardAuth middleware to oauth2-proxy, which signs people in with Keycloak and hands the hub
the ID token as `Authorization: Bearer`; the hub runs in OIDC mode (§1.2) and verifies the token
itself. Workers keep their own tokens: `/worker/*` has a router of its own without the
middleware. Only the emails of `HQC_OIDC_ALLOWED_EMAILS` get into the hub; any other account of
the realm gets `401`.

A hub user is bound to the `sub` of their first OIDC sign-in, and a token with the same email
and another `sub` gets `401`. When the account is recreated in the realm, or `HQC_OIDC_ISSUER`
changes, clear the binding in the hub's database — `UPDATE users SET oidc_sub = NULL WHERE
email = '<email>'`, for every user when the issuer changes; the next sign-in binds it again.

The realm the hub trusts has User registration off (Realm settings → Login): with it on, anyone
on the internet gets an account of the realm, and the hub's allow-list is then the only thing
between them and the hub.

The Keycloak client, in that realm:

- Client ID `hqc` (it is `HQC_OIDC_AUD`), Client authentication on, Standard flow only.
- Valid redirect URIs `https://hqc.example.com/oauth2/callback`, Web origins
  `https://hqc.example.com`.
- Advanced → Proof Key for Code Exchange Code Challenge Method: `S256`.
- `offline_access` among the client's optional scopes (it is there by default): with it the
  refresh token outlives the SSO session, and the phone stays signed in for the cookie's
  lifetime.
- The hub lets in only users whose email is verified (`email_verified` in the ID token, SPEC §6):
  tick Email verified on each user, or turn on Verify email in the realm. With
  `email_verified: false` oauth2-proxy itself refuses the sign-in at `/oauth2/callback` (`500`,
  `email in id_token … isn't verified` in its log); a token without the claim gets `403` from
  the hub.

The compose file of §1.8 with these changes: drop the `HQC_ACCESS_*` lines from `hub.env` (next
to `HQC_OIDC_*` the hub refuses to start), give the hub the environment and labels below, add
oauth2-proxy, and replace the `edge` network with `hqc-traefik`, a network only Traefik and
these containers are on, in the services and at the top level:

```yaml
  hqc-hub:
    # image, restart, depends_on, env_file and volumes as in §1.8; no ports
    environment:
      HQC_OIDC_ISSUER: https://auth.example.com/realms/<realm>
      HQC_OIDC_AUD: hqc
      HQC_OIDC_JWKS_URL: https://auth.example.com/realms/<realm>/protocol/openid-connect/certs
      HQC_OIDC_ALLOWED_EMAILS: you@example.com
      HQC_CLIENT_IP_HEADER: X-Real-Ip
      HQC_ARTIFACT_PUBLIC_BASE: https://artifacts.hqc.example.com
    networks: [default, hqc-traefik]
    labels:
      traefik.enable: "true"
      traefik.docker.network: hqc-traefik
      # People: the whole site behind the sign-in.
      traefik.http.routers.hqc.rule: Host(`hqc.example.com`)
      traefik.http.routers.hqc.entrypoints: websecure
      traefik.http.routers.hqc.tls.certresolver: letsencrypt
      traefik.http.routers.hqc.middlewares: hqc-hsts,hqc-auth
      traefik.http.routers.hqc.service: hqc-hub
      # Workers: their own token, no sign-in (the longer rule wins over the one above).
      traefik.http.routers.hqc-worker.rule: Host(`hqc.example.com`) && PathPrefix(`/worker/`)
      traefik.http.routers.hqc-worker.entrypoints: websecure
      traefik.http.routers.hqc-worker.tls.certresolver: letsencrypt
      traefik.http.routers.hqc-worker.middlewares: hqc-hsts
      traefik.http.routers.hqc-worker.service: hqc-hub
      # Artifact domain: public, signed links only (SPEC §12), no sign-in.
      traefik.http.routers.hqc-artifacts.rule: Host(`artifacts.hqc.example.com`)
      traefik.http.routers.hqc-artifacts.entrypoints: websecure
      traefik.http.routers.hqc-artifacts.tls.certresolver: letsencrypt
      traefik.http.routers.hqc-artifacts.middlewares: hqc-hsts
      traefik.http.routers.hqc-artifacts.service: hqc-hub
      # Plain http: a redirect to https for both names.
      traefik.http.routers.hqc-http.rule: Host(`hqc.example.com`) || Host(`artifacts.hqc.example.com`)
      traefik.http.routers.hqc-http.entrypoints: web
      traefik.http.routers.hqc-http.middlewares: hqc-https
      traefik.http.routers.hqc-http.service: hqc-hub
      traefik.http.services.hqc-hub.loadbalancer.server.port: "8080"
      traefik.http.middlewares.hqc-https.redirectscheme.scheme: https
      traefik.http.middlewares.hqc-https.redirectscheme.permanent: "true"
      traefik.http.middlewares.hqc-hsts.headers.stsSeconds: "31536000"
      traefik.http.middlewares.hqc-auth.forwardauth.address: http://hqc-oauth2-proxy:4180/
      traefik.http.middlewares.hqc-auth.forwardauth.trustForwardHeader: "true"
      traefik.http.middlewares.hqc-auth.forwardauth.authResponseHeaders: Authorization
      traefik.http.middlewares.hqc-auth.forwardauth.addAuthCookiesToResponse: _oauth2_proxy,_oauth2_proxy_0,_oauth2_proxy_1,_oauth2_proxy_2

  hqc-oauth2-proxy:
    image: quay.io/oauth2-proxy/oauth2-proxy:v7.15.5
    restart: unless-stopped
    env_file: oauth2-proxy.env   # OAUTH2_PROXY_CLIENT_SECRET, OAUTH2_PROXY_COOKIE_SECRET
    environment:
      OAUTH2_PROXY_PROVIDER: keycloak-oidc
      OAUTH2_PROXY_OIDC_ISSUER_URL: https://auth.example.com/realms/<realm>
      OAUTH2_PROXY_CLIENT_ID: hqc
      OAUTH2_PROXY_REDIRECT_URL: https://hqc.example.com/oauth2/callback
      OAUTH2_PROXY_HTTP_ADDRESS: 0.0.0.0:4180
      OAUTH2_PROXY_REVERSE_PROXY: "true"
      OAUTH2_PROXY_WHITELIST_DOMAINS: hqc.example.com
      OAUTH2_PROXY_UPSTREAMS: static://202
      OAUTH2_PROXY_EMAIL_DOMAINS: "*"
      OAUTH2_PROXY_SET_AUTHORIZATION_HEADER: "true"
      OAUTH2_PROXY_SKIP_PROVIDER_BUTTON: "true"
      OAUTH2_PROXY_SCOPE: openid email profile offline_access
      OAUTH2_PROXY_CODE_CHALLENGE_METHOD: S256
      OAUTH2_PROXY_COOKIE_EXPIRE: 720h
      OAUTH2_PROXY_COOKIE_REFRESH: 4m
      OAUTH2_PROXY_COOKIE_SAMESITE: lax
      OAUTH2_PROXY_COOKIE_SECURE: "true"
    networks: [hqc-traefik]
    labels:
      traefik.enable: "true"
      traefik.docker.network: hqc-traefik
      traefik.http.routers.hqc-oauth2.rule: Host(`hqc.example.com`) && PathPrefix(`/oauth2/`)
      traefik.http.routers.hqc-oauth2.entrypoints: websecure
      traefik.http.routers.hqc-oauth2.tls.certresolver: letsencrypt
      traefik.http.routers.hqc-oauth2.middlewares: hqc-hsts
      traefik.http.services.hqc-oauth2.loadbalancer.server.port: "4180"
```

```yaml
networks:
  # Only Traefik and this stack, instead of edge: docker network create hqc-traefik, and add
  # it to the networks of Traefik's service in its own compose project.
  hqc-traefik:
    external: true
```

- `hqc-traefik`, not a network Traefik shares with other stacks (a common `proxy`): any
  container on the hub's network reaches `hqc-hub:8080` past Traefik and sets `X-Real-Ip`
  itself. The hub's port is not published (no `ports:`, as in §1.8), and the entrypoints'
  `forwardedHeaders.trustedIPs` stays empty (and `forwardedHeaders.insecure` off): otherwise
  Traefik keeps the `X-Real-Ip` that a trusted peer sent.
- `HQC_CLIENT_IP_HEADER: X-Real-Ip`, required in OIDC mode: Traefik sets `X-Real-Ip` to the
  address of the TCP peer and drops one the client sent (unless the entrypoint's
  `forwardedHeaders` trusts that peer), so the hub's per-address limits on `/worker/*` count
  real clients. Without the variable the hub does not start: it would take the address from a
  `CF-Connecting-IP` the client makes up, and without either header every client is Traefik's
  address, so 20 bad worker tokens a minute from anyone would turn away every worker's reconnect
  with `429`. Behind another proxy, name a header that the proxy sets to the TCP peer's address
  in place of whatever the client sent, not one it appends to (`X-Forwarded-For`): the hub reads
  only the first value of the header (the client's own when the proxy adds its value after it),
  and a list of addresses in one value is not an address, so every client counts as the proxy.
- `hqc-hsts` and `hqc-http`: SPEC §20 leaves HSTS to Cloudflare; here Traefik sets
  `Strict-Transport-Security` for a year on every https answer and permanently redirects plain
  http on the `web` entrypoint to https. If Traefik's static configuration already redirects
  `web` to `websecure` (`entryPoints.web.http.redirections`), the `hqc-http` router is not
  needed.
- `addAuthCookiesToResponse` (Traefik 3.0 or later): oauth2-proxy renews the session every
  `OAUTH2_PROXY_COOKIE_REFRESH` and sends the new cookie in its forwardAuth answer, which Traefik
  passes to the browser only for the cookies named here: `_oauth2_proxy` and the parts
  `_oauth2_proxy_0` to `_2` of a session over 4 KB (up to about 12 KB; a bigger one needs more
  names); without it every request after the first renewal renews again.
- Artifact domain: a DNS name for `artifacts.hqc.example.com` pointing at Traefik, and the router
  above without the sign-in; the hub serves only `/artifacts/*` there (§1.2
  `HQC_ARTIFACT_PUBLIC_BASE`). Without the router and the variable, signed links go to the hub's
  domain behind the sign-in and HTML attachments can only be downloaded.
- `OAUTH2_PROXY_COOKIE_REFRESH` shorter than the ID token lifetime, see `HQC_OIDC_ISSUER` in §1.2.
- Without `OAUTH2_PROXY_WHITELIST_DOMAINS` oauth2-proxy refuses to return to the requested page
  after sign-in (`Rejecting invalid redirect … domain not in whitelist` in its log).
- `upstreams static://202`: oauth2-proxy only answers the forwardAuth check and proxies nothing.
- `OAUTH2_PROXY_COOKIE_SECRET`: 32 random bytes, `openssl rand -base64 32 | tr -- '+/' '-_'`;
  `OAUTH2_PROXY_CLIENT_SECRET`: Credentials of the Keycloak client.

Check from outside:

```sh
curl -s -o /dev/null -w '%{http_code} %{redirect_url}\n' https://hqc.example.com/
#   302 https://auth.example.com/realms/<realm>/protocol/openid-connect/auth?...client_id=hqc...
curl -s -o /dev/null -w '%{http_code}\n' https://hqc.example.com/worker/enroll
#   the hub's own 405, not a 302 to the sign-in
```

In the browser, after the sign-in the client opens and Settings shows the signed-in address.

## 2. Worker on macOS

### 2.1 Installation

```sh
cargo build --release -p hqc-worker
sudo install -m 0755 target/release/hqc-worker /usr/local/bin/
hqc-worker --version
which claude          # path to the Claude Code CLI, needed for claude_bin
claude --version      # the CLI is installed and logged in with this user's subscription
```

The worker runs as the macOS user whose Claude Code subscription and projects are used.
Subcommands: `enroll <hub_url>`, `run`; `mcp` and `env` print "not implemented" in
iteration 1.

Worker environment variables:

| variable | default | what it is |
|---|---|---|
| `HQC_HOME` | `~/.hqc` | worker directory: `worker.toml`, outbox, the token in file mode |
| `HQC_WORKER_CONFIG` | `$HQC_HOME/worker.toml` | config path, if it is not in `HQC_HOME` |
| `HQC_TOKEN_STORE` | unset | `file`: store the token in the file `$HQC_HOME/token` (0600) instead of Keychain; on a Mac only needed for tests and runs like `tools/e2e-local.sh` |
| `CLAUDE_CONFIG_DIR` | `~/.claude` | where Claude Code keeps transcripts; the worker looks there to choose between `--resume` and `--session-id` |
| `RUST_LOG` | `info` | log level; the token never appears in logs, even at `trace` |

### 2.2 `worker.toml`

`~/.hqc/worker.toml`. This is the only source of truth for the worker's restrictions: the hub
can neither set nor widen them (§2.1, §14). An unknown field is a startup error (a typo must not
silently weaken the restrictions).

```toml
# Hub URL, exactly the same as in `hqc-worker enroll` (no trailing /): the token is looked up
# in Keychain by it. Only https://; http:// is for localhost / 127.0.0.0/8 / [::1].
hub_url = "https://hqc.example.com"

# Allow http:// to a non-loopback hub (the worker token will travel in plain text). Only
# for debugging in an isolated network; for `enroll` the same is the --insecure-http flag.
# insecure_http = false

# How many agent processes may run at once; the hub receives the next start as
# `agent_start_failed` (retryable) and retries later. Default 8.
# max_sessions = 8

# Seconds a `git clone` of a workspace made with "Clone a repository", or a `git worktree add`
# of one made with "New worktree", may take; then git is killed and the half-made folder
# removed. Default 600. Both run in the environment of the new folder, never prompt, and the
# folders the worker created are listed in $HQC_HOME/created_workspaces.json (the only ones it
# ever deletes; a worktree only through `git worktree remove` in its origin).
# clone_timeout = 600

# Roots sessions never leave. Absolute paths; symlinks are resolved.
# A non-empty list. Workspaces (session folders) are added in the web client and must lie
# inside them: the worker checks every path itself (fs.stat, session.start), and the folder
# tree in the client shows only what is inside these roots.
allowed_roots = ["/Users/me/work"]

# Idle seconds after which an agent process exits (the next input brings it back
# with --resume). 0 or absent: an idle agent is left alone (§17.3).
idle_timeout = 0

# Whether to let in sessions of other hub users. Iteration 1 has one user: false.
allow_foreign_sessions = false

# Path to the Claude Code CLI. Defaults to `claude` from the worker process's PATH. Under
# launchd PATH is short (~/.local/bin is not in it), so better set it explicitly.
claude_bin = "/Users/me/.local/bin/claude"

# Let agents continue their own subagents with Claude Code's SendMessage (by default it is
# disallowed together with ListAgents). Messages from other local sessions are then refused,
# but an agent can hand work to any local Claude session that accepts them, past hqc (SPEC §8.1).
# allow_subagent_messages = false

# The [workspace] block is gone (since T14): workspaces are hub objects added in the client
# (project → Workspaces → "Add a workspace"). The worker ignores the old iteration 1 block and
# logs a warning about it on start; remove it from the file.

# Claude Code permission policy (SPEC §8.1, §14). The block may be omitted: then mode = "ask",
# allow_auto = true, allow is empty, deny is the default set (sudo, ssh, scp, rm -rf /*,
# curl | sh, dd, mkfs, git push --force / -f).
[permissions]
# The mode of a session that does not choose one in hqc ("ask" | "auto").
mode = "ask"
# Whether a session may be switched to "auto" in hqc. false: every session runs in "ask",
# whatever the web client says.
allow_auto = true
allow = [
  # Edit/Write without parentheses: editing any file the user can write to, not only the
  # workspace; remove them if you want to confirm every edit.
  "Read", "Glob", "Grep", "Edit", "Write",
  "Bash(cargo build:*)", "Bash(cargo check:*)", "Bash(cargo test:*)",
  "Bash(cargo clippy:*)", "Bash(cargo fmt:*)", "Bash(cargo run:*)",
  "Bash(git status:*)", "Bash(git diff:*)", "Bash(git log:*)", "Bash(git show:*)",
  "Bash(git add:*)", "Bash(git commit:*)",
  "Bash(ls:*)", "Bash(mkdir:*)", "Bash(touch:*)",
  "mcp__hqc",
]
deny = [
  "Bash(sudo:*)", "Bash(ssh:*)", "Bash(scp:*)", "Bash(rm -rf /*)",
  "Bash(curl * | sh)", "Bash(curl * | bash)", "Bash(dd:*)", "Bash(mkfs*)",
  "Bash(git push:*)", "Bash(cargo publish:*)", "Bash(git reset --hard:*)",
  "WebFetch",
]
```

Three levels of `[permissions]`, from strongest to weakest:

1. **`deny`**: not run and not asked about. Neither "Always allow", nor `mode = "auto"`, nor
   the hub's `auto_approve` overrides it; instead of a card the chat shows the line
   "Blocked by worker policy". A tool without parentheses (`WebFetch`) disappears from the
   session entirely. Your own `deny` **replaces** the default set, so copy into it the default
   rules you need; `deny = []` disables the bans altogether.
2. **`allow`** and "Always allow" rules (the CLI writes them to
   `<git repository root>/.claude/settings.local.json`): run without asking.
3. **Everything else**: in "Ask" a permission card appears; in "Auto" the Claude Code
   classifier decides without asking (verified: it lets through both `rm -rf` outside the
   folder and `git push --force` if the user asked for it), so in this mode `deny` is the only
   boundary.

In an environment with `project_settings = true` (§14 of the SPEC) the permission rules of the
repository's `.claude/settings.json` join levels 1 and 2 (the rules screen lists them), and none
of the three levels covers the repository's own hooks: Claude Code runs them itself, without a
card and outside `deny`.

**The mode of each session** (T38). Every session runs in "Ask" or "Auto". The chat header
shows which: an orange "Auto" badge, or "Ask" in plain text ("Ask (Auto asked)" when the
session asked for Auto but the worker runs it in Ask). It is chosen in the new-session form
("Permissions": the worker's default, Ask or Auto) and switched in the session's "More" menu
("Switch to Auto" / "Switch to Ask"); going to Auto asks for one confirmation, going to Ask
none. A running agent switches at once, even in the middle of a turn (the next tool call is
decided in the new mode); a stopped one starts in it. Only you, as the worker's owner, can
switch a session on your worker to Auto. Sessions an agent starts with `hqc_spawn` always
begin in Ask, and a message from another session never changes a session's mode.

- `mode` is the default for sessions created without a choice (and for sessions from before
  T38). "auto" here makes every such session run without asking, so the worker logs a warning
  at start.
- `allow_auto = false` is the machine's veto: the worker runs every session in Ask whatever
  the hub sends, logs `the hub asked for permission mode auto; [permissions] allow_auto =
  false, running ask`, and reports Ask, which the header then shows. `mode = "auto"` together
  with `allow_auto = false` is a startup error.
- In Auto the `deny` list is still passed to Claude Code and still blocks; nothing else does.
  `auto_approve` of the hub is a different, older switch: it answers the cards that do reach
  the hub; in Auto there are none.

Rule syntax: `Bash(cargo test:*)` is the `cargo test` command with any arguments (a prefix up
to a word boundary: `Bash(mkfs*)` does not catch `mkfs.ext4`), `Bash(curl * | sh)`: `*` is any
characters, `Bash(git status)` is exactly this command; compound commands (`a && b`, `a | b`)
the CLI checks part by part. `Read`, `Edit`, `WebFetch` are the whole tool,
`mcp__hqc` is all tools of the MCP server. An invalid rule is a worker startup error: the CLI
would silently drop the whole settings file, `deny` included. `Bash(rm -rf /*)` catches any
absolute path, including inside the workspace; relative ones (`rm -rf target`) pass.

**Environments and bindings** (SPEC §14, T43). `[permissions]` applies to every session on the
machine; to let one folder do what the others may not (here: `ssh` to one host), give it an
environment and bind the folder to it:

```toml
[environments.infra]
env = { GITHUB_TOKEN = "..." }        # variables for the agent, on top of the allowlist below
ssh_keys = ["infra_ed25519"]          # keys of the store $HQC_HOME/keys/ (below)
allow_local_mcp = false               # true: the machine's MCP servers too (hqc's stays)
project_settings = false              # true: the repository's CLAUDE.md, .claude/settings.json too
[environments.infra.permissions]
unblock = ["Bash(ssh:*)"]             # deny rules of [permissions] lifted here only
allow = ["Bash(ssh debian@192.168.1.20:*)"]
deny = ["Bash(git push:*)"]           # added to the deny of [permissions]

[[env_bindings]]
path = "/Users/me/work/lab/infra"
environment = "infra"
```

- A session's environment is the binding with the longest path that contains its workspace
  (`/a/b` does not contain `/a/bc`; symlinks are resolved), else `default` (no variables, no
  local MCP, no project settings, `[permissions]` as it is). A binding may name `default` to give a subfolder back.
  The worker decides this itself on every start; the hub only shows it (the rules screen's
  header, "env infra" on the workspace row in the project settings).
- Effective rules in a bound folder: the deny of `[permissions]` minus `unblock`, plus the
  environment's `deny`; the allow of `[permissions]` plus the environment's `allow`; then the
  folder's HQC rules; then the session's mode. `unblock` must name a rule of the `deny` of
  `[permissions]` exactly (with the default deny: as listed above), otherwise the worker does not
  start. The rules screen lists the environment's rules with the source
  `worker.toml [environments.infra]`.
- `env` values never leave the machine: not in the hub, the logs or `hqc-worker env`. `HQC_*`
  names are refused. A changed `worker.toml` takes effect after a restart of the worker.
- `hqc-worker env` lists the environments (keys by name) and bindings; `hqc-worker env which
  <folder>` shows the environment of a folder, the binding that decided it, its keys and the
  effective rules with their sources. An environment no binding names is marked there ("no
  binding names it") and logged as a warning at start: check the table names, a
  `[environments.x.permissions]` under `[environments.y]` makes a separate environment `x`.

**ssh keys of an environment** (SPEC §14, T44). A session gets no ssh-agent unless its folder's
environment has `ssh_keys`: the user's own agent (`SSH_AUTH_SOCK` of the worker) is never passed,
so `git` over ssh and `ssh` from an agent work only in such a folder. Your terminal and VS Code
are not affected. To give `infra` a key of its own:

```sh
ssh-keygen -t ed25519 -N "" -C hqc-infra -f /tmp/infra_ed25519   # or an existing key without passphrase
hqc-worker env add-key infra_ed25519 /tmp/infra_ed25519
rm /tmp/infra_ed25519                                              # the store has the copy
# add the public half (/tmp/infra_ed25519.pub) to the host's authorized_keys, then
# ssh_keys = ["infra_ed25519"] in [environments.infra], and restart the worker
```

- The store is `$HQC_HOME/keys/` (`~/.hqc/keys/`): 0700, one key per file, 0600, the file name is
  the key name. `add-key` creates it with these modes and refuses to replace a key of the same
  name (remove the file first). A key with a passphrase does not work: nothing can prompt for
  it, and the session fails to start with `ssh-add`'s message.
- The worker refuses to start (and `hqc-worker env` refuses to print) if the store, a file in
  it or `$HQC_HOME/agents/` is readable by group or others, is a symlink or belongs to another
  user; the message says which `chmod` fixes it. A name in `ssh_keys` that is not in the store
  also stops it.
- Every process of a session in the environment gets its own `ssh-agent` on a socket in
  `$HQC_HOME/agents/` with the keys loaded; it stops when the process does (stop, kill, crash,
  idle stop, worker shutdown). After a crash of the worker the next start empties the agents it
  left and removes their sockets (`stale ssh-agent sockets removed` in the log).
- Move the project's key out of `~/.ssh`: without a separate OS user the agent can still read
  `~/.ssh` and use what is there; the store only helps for keys that are no longer there.
- `hqc-worker env` and the logs show key names, never keys; `hello` carries no key names.

`show_in_vscode` (default `true`): the worker starts Claude Code with
`CLAUDE_CODE_ENTRYPOINT=claude-vscode`; otherwise the CLI writes the transcript with
`entrypoint: sdk-cli`, and the VS Code extension's conversation history shows only its own
sessions (verified 2026-10-24 on 2.1.309). The worker always puts `voiceEnabled: false` into
`--settings`, the title runs below included: otherwise macOS asks for microphone access on
behalf of `hqc-worker`.

Session titles (SPEC §3 Session, §8.1, T24) are made up by the worker on the same subscription:
after the session's first reply the hub sends `title.request`, and the worker runs `claude -p --model
claude-haiku-5-5 --max-turns 1 --no-session-persistence …` without tools in an empty folder
`~/.hqc/titles/<0|1>` and then deletes it. This is one short haiku turn per session; it leaves no
transcript and no trace in the VS Code history. If `claude` is not logged in, the worker log says
`no session title: …`, and the title is taken from the first line of the prompt.

### 2.3 Enrollment (`enroll`)

```sh
hqc-worker enroll https://hqc.example.com
# Enrollment code: K7QM-3XPW
# Confirm it in the hqc web app: https://hqc.example.com/#/enroll?code=K7QM-3XPW
# Waiting...
```

Open the link (from a phone or laptop, logged in through Access) and confirm the code, or
in the client go to "Settings" → "Confirm a worker code" and type the code by hand (case and hyphen
do not matter). Before the "Confirm" button the client shows the machine name, OS, request time
and country (from Cloudflare): if this is not your machine and not an `enroll` you have just
started, do not confirm; the link may have been sent to you. The code lives for 10 minutes. After
confirmation the worker prints `Enrolled as worker <id>.` and stores the token:

- on macOS in Keychain, generic password, service `hqc-worker`, account = `hub_url`.
  To view: `security find-generic-password -s hqc-worker -a https://hqc.example.com`;
- with `HQC_TOKEN_STORE=file` (and on other OSes) in `$HQC_HOME/token`, mode 0600.

The worker's name in the web app is the host name without the domain (`home-mac`, not
`home-mac.tail….ts.net`); set a different one with `--name`:
`hqc-worker enroll --name home-mac https://hqc.example.com`.

Over ssh the keychain is usually locked. `enroll` tests the store with a trial write before
requesting a code, and if Keychain refuses it exits right away with a hint: run
`security unlock-keychain` in the same ssh session and retry, or set `HQC_TOKEN_STORE=file`
(then `run` must use it too). The hub issues the token only once, so the hub is never reached
and nothing is burned.

`enroll` does not write `worker.toml`: if it is missing, the worker will tell you the path.

**Re-enrolling the same machine** (SPEC §5.1, since 0.21). If a machine is enrolled again
(after rebuilding the binary, changing the token store (Keychain → `HQC_TOKEN_STORE=file`),
or token revocation), it must stay the same worker; otherwise its sessions, workspaces and the
worker's service session stay forever on the old id, "offline". Steps:

1. Stop the old `run` (`launchctl bootout gui/$(id -u)/local.hqc.worker`): a worker that is
   currently connected cannot be replaced (`409 worker_online`).
2. `hqc-worker enroll <hub_url>` (with `HQC_TOKEN_STORE=file` if the token should be kept in a
   file), open the link.
3. On the confirmation screen, below the machine details, is a list of your workers that are
   offline or revoked, with their session counts. Choose "The same machine, replace <name>" and press
   "Confirm replacement". "New worker" (the default) creates a new record with a new id.
4. `enroll` prints `Enrolled as worker <id> (replaced <name>).`: `<id>` is the same as before.
   The old token stopped working at the moment of confirmation; the worker's service session
   shows the line `Worker re-enrolled: “old” → “new”`. Start `run` again.

The name and OS come from the new enrollment (use `--name` to keep the old one). Agent
processes of the old run are considered finished (as on a worker restart): input continues
through `--resume`. If "New worker" was confirmed by mistake, revoke it, then re-enroll the
machine once more replacing the old one; a spare worker without sessions is removed with the
"Delete" button (§2.6).

### 2.4 launchd agent for `run`

An agent (LaunchAgent), not a daemon: the worker needs the user's Keychain (its token and the
Claude Code login), which is available only in the user's session.

`~/Library/LaunchAgents/local.hqc.worker.plist` (paths are absolute; launchd does not expand
`~`):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>local.hqc.worker</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/hqc-worker</string>
    <string>run</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>/Users/me/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
    <key>RUST_LOG</key><string>info</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardOutPath</key><string>/Users/me/Library/Logs/hqc-worker.log</string>
  <key>StandardErrorPath</key><string>/Users/me/Library/Logs/hqc-worker.log</string>
</dict>
</plist>
```

`PATH` is also needed by `claude` itself (it runs `git`, the shell and so on), even if
`claude_bin` is set.

```sh
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/local.hqc.worker.plist
launchctl print gui/$(id -u)/local.hqc.worker | grep -E 'state|pid'
launchctl kickstart -k gui/$(id -u)/local.hqc.worker   # restart (e.g. after an update)
launchctl bootout gui/$(id -u)/local.hqc.worker        # stop
```

Stopping sends SIGTERM: the worker ends the agent processes, reports `stopped` to the hub and
spends up to 5 s flushing the outbox; whatever is unsent stays on disk and goes out on the next start.

A Keychain item is bound to the signature of the binary that created it. An unsigned build (`cargo build`, the linker's ad-hoc signature) gets a new signature on every rebuild, so after `hqc-worker` is replaced, the worker under launchd gets stuck on a Keychain dialog on the Mac's screen (`SecurityAgent` shows up next to it in `ps`), and the log stays empty. Verified 2026-10-23 on 0.1.0. For a machine where the worker is rebuilt, use the file store: `HQC_TOKEN_STORE=file` at `enroll` and the same variable in the plist's `EnvironmentVariables`; the token lives in `$HQC_HOME/token` with mode 0600. Keychain makes sense only for a signed release binary.

### 2.5 Where things are, logs

| what | where |
|---|---|
| config | `~/.hqc/worker.toml` |
| token | Keychain (`hqc-worker` / `hub_url`); in file mode `~/.hqc/token` |
| outbox (events not yet acknowledged by the hub) | `~/.hqc/outbox/outbox.sqlite3` (SQLite, WAL); `sqlite3 ~/.hqc/outbox/outbox.sqlite3 'select count(*) from outbox'` shows how many are waiting to be sent |
| worker logs | stderr; under launchd `~/Library/Logs/hqc-worker.log` (`tail -f`) |
| title run folders | `~/.hqc/titles/0`, `~/.hqc/titles/1`; exist only during a run |
| Claude Code transcripts | `~/.claude/projects/<workspace path with everything except letters and digits replaced by ->/<runtime_session_id>.jsonl` |

What to look for in the log: `connected to hub worker_id=…`: connected; `hub rejected the
worker token`: the token is revoked or this is the wrong hub; `worker update needed`: the protocol
version does not match (the worker retries once an hour and does nothing else); `agent start failed`:
`claude` does not start (path, login).

### 2.6 Token revocation

In the client: "Settings" → "Workers" → "Revoke". The hub marks the token revoked, drops the
connection and archives the worker's service session (with the line `Token of worker “…” revoked` in it);
the worker disappears from the "Workers" group in the left column, and in "Settings" it stays,
marked "revoked", with a "Delete" button. The worker itself will keep reconnecting and logging `hub
rejected the worker token; run hqc-worker enroll again`. On the machine itself, then stop the
agent (`launchctl bootout …`) and delete the old token:
`security delete-generic-password -s hqc-worker -a https://hqc.example.com`
(in file mode, `rm ~/.hqc/token`). To bring the machine back with its sessions: `hqc-worker
enroll <hub_url>` and, when confirming, "The same machine, replace <name>" (§2.3): the
revocation is lifted, and the id, sessions and workspaces remain.

**Deleting a revoked worker.** "Settings" → "Delete" on the revoked worker: the hub deletes the
worker record, its service session with its history and its workspace records (folders on the
machine are not touched). While the worker has non-archived sessions, the hub refuses (`409
worker_has_sessions`), and the client shows their number: either move them by re-enrolling the
machine as a replacement for this worker (§2.3), or archive them. Archived sessions survive the
deletion, without a worker or workspace, as history only (they cannot be restored). Revoke a
non-revoked worker first.

If a laptop or a token is stolen, revoke from the client right away: the hub stores only a hash,
and a new token is issued only through code confirmation under your Access login.

## 3. Client

Build: `cd client && npm ci && npm run build` → `client/dist`; serving is in §1.7. For
development, see `client/README.md` (`npm run dev`, `npm run dev:mock`).

From the phone:

1. Open `https://hqc.example.com/`, log in through Access.
2. Install the PWA. Android (Chrome): menu → "Install app". iPhone (Safari):
   "Share" → "Add to Home Screen". On iOS the installed app keeps cookies separately from
   Safari, so you will have to log in through Access once more inside it.
3. Turn on notifications: in the app, "Settings" → "Subscribe to notifications", and
   allow. Push arrives for `approval.request`, `clarify.request`, the agent's final reply
   and an undelivered input, if that session is not currently open on any device
   (§16). Tapping it opens the session.

Push limitations:
- **iOS / iPadOS**: Web Push works only from a PWA installed to the Home Screen
  (iOS 16.4+), not from a Safari tab; subscribe from inside the installed app.
- Push requires HTTPS (through the tunnel you have it); it does not work over a LAN IP.
- If the Access session has expired, the notification still arrives (push goes from the hub
  through the browser's service), but tapping it opens the login page.
- The client's service worker caches nothing: without a connection to the hub the client does not open.

## 4. Check

A manual version of what `tools/e2e-local.sh` does (it can be run on any developer machine:
it starts the hub in dev mode on loopback, a separate database `hqc_e2e` and a temporary
worker, runs the scenario with one `claude-haiku-5-5` turn and cleans everything up; it costs
one short subscription request).

1. `https://hqc.example.com/health` (after logging in to Access): `ok`. The client opens,
   and "Settings" shows your email.
2. "Settings" → "Notifications": not "VAPID keys are not configured on the hub". Subscribe.
3. On the Mac: `hqc-worker enroll https://hqc.example.com`, confirm the code via the link,
   `launchctl bootstrap …` (§2.4). In "Settings" → "Workers" the machine is `online`.
   In the left column, on a project (the personal one or a new one, "+ project"), go to
   "Workspaces" → "Add a workspace": choose the worker and a folder inside `allowed_roots` (from
   the tree or by path). The project's first workspace becomes its primary one.
4. Create a session in the project ("New session": title optional, the primary workspace
   preselected, a folder inside it optional), send:
   `Run the shell command: touch hqc-e2e && echo hqc-e2e. Then reply with exactly: DONE`.
   The "awaiting delivery" mark under the message goes away, the session is "Running".
5. A `Bash` card appears and a permission request with buttons; close the app on the
   phone: a push about the request should arrive. Open it, press "Allow".
6. The tool card gets the result `hqc-e2e`, the final reply `DONE` arrives, the
   session is "Idle".
7. On the Mac: the file `hqc-e2e` appeared in the workspace folder; the transcript
   `~/.claude/projects/<encoded workspace path>/<uuid>.jsonl` exists.
8. "Stop" during the next turn interrupts it; an open permission request is then
   closed as cancelled.
9. Taking over a session from VS Code (SPEC §9.3; needs a Claude Code with a registry of live
   sessions `~/.claude/sessions`; on start the worker log shows `capabilities` with `sessions_registry: true`).
   In VS Code, in a folder inside `allowed_roots`, start a Claude Code session and get a reply. In
   the client: project → "Workspaces" → "Import from Claude Code", choose this folder (or, if the
   folder is already a workspace of the project, its "VS Code sessions" switch is on: since T21 it
   is on for every workspace by default). The session appears
   in the project with the banner "Session is open in VS Code … close it there"; there is no
   "Take over" button, and the input field is closed.
10. **Close the session in VS Code** (the Claude Code tab with this session). Within a couple of
    seconds the banner goes away and the session is "Idle"; no need to wait 30 s. Send a message
    from the client: the reply arrives, nothing changes in VS Code.
11. Reopen the session in VS Code (from the list of past sessions): the session in the client is
    "In VS Code" again; if the hqc agent was still working, it stops after the turn and the feed
    shows the line "Returned to VS Code" asking to close the session there. If the feed shows a
    "Fork" line with a warning, VS Code did not continue from the end of the transcript: the
    listed hqc turns did not make it into its context (reopen the session in VS Code so it rereads
    the file).

If something is wrong: the hub log (`journalctl -u hqc-hub`, in Docker `docker compose logs hqc-hub`),
the worker log (`~/Library/Logs/hqc-worker.log`), the session state in the client ("Error": press
"Retry" after fixing the cause).

## 5. Known limitations of iteration 1

Per §19.1 and the notes `docs/notes/t2..t6-questions.md`:

- ~~One project, one workspace~~: lifted in T14: projects are created, renamed and
  deleted in the client (the personal one: description only), and a project has several
  workspaces on any workers. Workspaces are `dir` only (an existing folder); `clone`, `worktree`,
  `ephemeral` and the session template are iteration 4. A session without a workspace (§8.4)
  cannot be created from the client. Deleting a workspace removes only the record; while it has
  non-archived sessions, the hub does not delete it.
- **Upgrading from iteration 1** (hub migration `0005`): the workspace from the old `[workspace]
  path` (the hub remembers it from the last `hello`) moves into the personal project, and sessions
  are bound to it; sessions whose folder is unknown are archived with the system
  record "Archived on upgrade…". The `[workspace]` block in `worker.toml` can be removed after the
  worker is upgraded (it is ignored).
- **No hqc MCP** (`hqc-worker mcp` is not implemented): agents do not message each other,
  no `ask`/`send`, no attachments from the agent, no search, no schedules.
- **No mirror and no import of VS Code sessions**, no takeover; a session started in hqc is visible
  in `~/.claude/projects`, but changes made in VS Code do not come back to hqc.
- **No artifacts**: attachments to messages are not passed to the agent (the worker logs a warn).
- **Environments** (since T43, §2.2): the agent process gets only `PATH`, `HOME`, `USER`,
  `LOGNAME`, `SHELL`, `LANG`, `LC_*`, `TERM`, `TMPDIR`, `CLAUDE_*`, `ANTHROPIC_*` from the
  worker's environment, plus the `env` of the folder's environment, plus (since T44)
  `SSH_AUTH_SOCK` of its own ssh-agent if the environment has `ssh_keys`; `HQC_*`, the user's
  `SSH_AUTH_SOCK` and everything else are not passed.
- **Transcripts are kept by Claude Code**: at its start the CLI deletes transcripts older than
  `cleanupPeriodDays` (30 days by default), and a session whose transcript is gone continues
  without its conversation (the feed says `transcript_missing`, SPEC §8.1). On a worker set it
  high in `~/.claude/settings.json` of the account the worker runs as:
  `"cleanupPeriodDays": 100000` (about 270 years).
- **One user**: no sharing and no grants; keep `allow_foreign_sessions` `false`.
- **Claude Code adapter**: `system_prompt` and `disallowed_tools` are passed
  (`--append-system-prompt`, `--disallowedTools`), `restricted` as `--restricted`;
  the repository's `.claude/settings.json` is not applied (`--setting-sources user,local`), but
  `.claude/settings.local.json` is (that is where `always` writes); in an environment with
  `project_settings = true` the repository's `CLAUDE.md` and `.claude/settings.json` apply too
  (`--setting-sources user,project,local`, SPEC §14). `MCP_TOOL_TIMEOUT` is not
  passed; there is no turn limit (`--max-turns`). Read-only commands
  (e.g. `echo`) the CLI allows by itself, without a request (§8.1). The format of the answer to a
  multiple-choice question (`AskUserQuestion`) has not been verified on a live build.
- **Turn state**: a turn that ended without final text may, with a late `ack` of the input,
  stay "Running" until the next turn (§5.2 **[verify]**).
- **`session.kill`** clears the worker's queue; pending inputs are resent only after the
  worker's next reconnect (T3, item 10).
- **Worker token**: Keychain only on macOS; on other OSes a 0600 file. No tray and no
  `hqc-worker update`: an incompatible version is reported only in the log.
- **Hub**: Postgres without TLS (database on the same host); `/worker/*` limits are kept in the
  hub process's memory, by the address from `CF-Connecting-IP` (enroll 10/min and 10 live codes per
  address, 1000 in total; poll 60/min; 20 invalid `/worker/ws` tokens per minute; 16 token checks
  at once), and the counters reset on restart; no master key and no `api_keys`.
- **Client**: verified by tests and the build; a manual run on a phone through Access and
  real push on iOS/Android were not done within T6 (no device in the autonomous
  run); this is the first thing to do after installation (§4). Session history
  loads from the beginning in full (no lazy loading of the tail), and there is no offline mode.
