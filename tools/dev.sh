#!/bin/bash
# Local development loop on one machine (no Cloudflare, no Docker): a dev-mode hub on
# 127.0.0.1 and a worker enrolled against it, both in the background with logs under
# tmp/dev/. State persists between runs (database hqc_dev, worker token, outbox).
#
#   tools/dev.sh up        build (dev profile), start hub + worker, enroll on first run
#   tools/dev.sh down      stop both
#   tools/dev.sh restart   down + up (after a code change)
#   tools/dev.sh status    pids, /health, workers
#   tools/dev.sh logs      tail both logs
#   tools/dev.sh reset     down, drop hqc_dev and tmp/dev (fresh start)
#
# Open http://127.0.0.1:8080 on this machine, or from another one through an ssh tunnel:
#   ssh -N -L 8080:127.0.0.1:8080 home-mac@home-mac.ebal.uk
# The hub listens on loopback only (HQC_DEV=1 requires it, SPEC §6).
#
# The worker spawns `claude`, whose login lives in the Keychain, which is locked in plain ssh
# sessions. So the worker is started as a launchd job in the user's GUI domain (gui/<uid>),
# where the login keychain is unlocked; `up` can be run from any ssh session as long as the
# user is logged in on the GUI. The hub itself runs as a plain background process.
#
# Environment: DEV_PORT (8080), DEV_DB (hqc_dev), DEV_USER (dev@local),
# DEV_WORKSPACE (this repository), DEV_ROOTS (parent of the workspace), RUST_LOG (info).

set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HELPER="$ROOT/tools/e2e-local.py"
RUN="$ROOT/tmp/dev"
PORT="${DEV_PORT:-8080}"
DB="${DEV_DB:-hqc_dev}"
USER_EMAIL="${DEV_USER:-dev@local}"
WORKSPACE="$(cd "${DEV_WORKSPACE:-$ROOT}" && pwd -P)"
ROOTS="${DEV_ROOTS:-$(dirname "$WORKSPACE")}"
export HUB="http://127.0.0.1:$PORT"
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:/opt/homebrew/opt/postgresql@17/bin:/opt/homebrew/bin:$PATH"

log() { printf '[dev] %s\n' "$*" >&2; }
fail() { log "FAIL: $*"; exit 1; }
alive() { [ -f "$1" ] && kill -0 "$(cat "$1")" 2>/dev/null; }

stop_pidfile() { # pidfile name
    alive "$1" || { rm -f "$1"; return 0; }
    local pid; pid="$(cat "$1")"
    log "stopping $2 (pid $pid)"
    kill -TERM "$pid" 2>/dev/null
    for _ in $(seq 1 30); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
    kill -0 "$pid" 2>/dev/null && { log "$2 did not exit, killing"; kill -KILL "$pid" 2>/dev/null; }
    rm -f "$1"
}

b64url() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }

LABEL="local.hqc.dev.worker"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"

start_worker_gui() { # binary: run `hqc-worker run` as a one-shot job in the GUI launchd domain
    launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null
    mkdir -p "$(dirname "$PLIST")"
    cat >"$PLIST" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array><string>$1</string><string>run</string></array>
  <key>EnvironmentVariables</key><dict>
    <key>PATH</key><string>$PATH</string>
    <key>HQC_HOME</key><string>$RUN/home</string>
    <key>HQC_TOKEN_STORE</key><string>file</string>
    <key>RUST_LOG</key><string>${RUST_LOG:-info}</string>
  </dict>
  <key>WorkingDirectory</key><string>$ROOT</string>
  <key>RunAtLoad</key><true/><key>KeepAlive</key><false/>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardOutPath</key><string>$RUN/worker.log</string>
  <key>StandardErrorPath</key><string>$RUN/worker.log</string>
</dict></plist>
PL
    launchctl bootstrap "gui/$(id -u)" "$PLIST" || fail "launchctl bootstrap gui/$(id -u): is the user logged in on the GUI?"
    local pid=""
    for _ in $(seq 1 20); do
        pid="$(launchctl print "gui/$(id -u)/$LABEL" 2>/dev/null | awk '$1 == "pid" && $2 == "=" {print $3}')"
        [ -n "$pid" ] && break; sleep 0.5
    done
    [ -n "$pid" ] || fail "worker did not start, see $RUN/worker.log"
    echo "$pid" >"$RUN/worker.pid"
    log "worker started in gui/$(id -u) (pid $pid)"
}

stop_worker_gui() {
    if launchctl print "gui/$(id -u)/$LABEL" >/dev/null 2>&1; then
        log "stopping hqc-worker run (launchd $LABEL)"
        launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null
        for _ in $(seq 1 30); do alive "$RUN/worker.pid" || break; sleep 0.5; done
    fi
    rm -f "$RUN/worker.pid"
}

cmd_up() {
    alive "$RUN/hub.pid" && fail "hub already running (pid $(cat "$RUN/hub.pid")); use restart"
    mkdir -p "$RUN/home" && chmod 700 "$RUN/home"
    log "building hqc-hub and hqc-worker (dev)"
    cargo build --manifest-path "$ROOT/Cargo.toml" -p hqc-hub -p hqc-worker || fail "build"
    BIN="$ROOT/target/debug"
    if [ ! -f "$ROOT/client/dist/index.html" ]; then
        log "building the web client"
        (cd "$ROOT/client" && npm ci --silent && npm run build --silent) || fail "client build"
    fi
    psql -lqt | cut -d'|' -f1 | grep -qw "$DB" || { createdb "$DB" || fail "createdb $DB"; log "database $DB created"; }
    if [ ! -f "$RUN/vapid.pem" ]; then
        openssl ecparam -name prime256v1 -genkey -noout -out "$RUN/vapid.pem" 2>/dev/null || fail "openssl"
    fi
    VAPID_PRIVATE="$(openssl ec -in "$RUN/vapid.pem" -outform DER 2>/dev/null | tail -c +8 | head -c 32 | b64url)"
    VAPID_PUBLIC="$(openssl ec -in "$RUN/vapid.pem" -pubout -outform DER 2>/dev/null | tail -c 65 | b64url)"

    log "starting hqc-hub on $HUB (dev user $USER_EMAIL, db $DB)"
    env -u HQC_ACCESS_TEAM_DOMAIN -u HQC_ACCESS_AUD HQC_CLIENT_DIR="$ROOT/client/dist" \
        HQC_DEV=1 HQC_DEV_USER="$USER_EMAIL" HQC_HUB_LISTEN="127.0.0.1:$PORT" \
        HQC_DATABASE_URL="postgresql://localhost/$DB" \
        HQC_VAPID_PRIVATE="$VAPID_PRIVATE" HQC_VAPID_PUBLIC="$VAPID_PUBLIC" \
        HQC_VAPID_SUBJECT="mailto:$USER_EMAIL" RUST_LOG="${RUST_LOG:-info}" \
        "$BIN/hqc-hub" >>"$RUN/hub.log" 2>&1 &
    echo $! >"$RUN/hub.pid"
    python3 "$HELPER" wait-health || fail "hub did not come up, see $RUN/hub.log"

    cat >"$RUN/home/worker.toml" <<TOML
hub_url = "$HUB"
allowed_roots = ["$ROOTS"]
idle_timeout = 0
allow_foreign_sessions = false
claude_bin = "$(command -v claude || echo "$HOME/.local/bin/claude")"

[workspace]
path = "$WORKSPACE"

# Политика одобрений (SPEC §14, RUNBOOK §2.2): allow — без вопросов, deny — никогда и без
# вопроса (сильнее always), остальное спрашивает. Свой deny заменяет набор по умолчанию.
[permissions]
mode = "ask"
allow = [
  "Read", "Glob", "Grep", "Edit", "Write",
  "Bash(cargo build:*)", "Bash(cargo check:*)", "Bash(cargo test:*)", "Bash(cargo clippy:*)",
  "Bash(cargo fmt:*)", "Bash(cargo run:*)", "Bash(cargo doc:*)",
  "Bash(npm test:*)", "Bash(npm run:*)", "Bash(npm ci:*)", "Bash(node:*)",
  "Bash(git status:*)", "Bash(git diff:*)", "Bash(git log:*)", "Bash(git show:*)",
  "Bash(git add:*)", "Bash(git commit:*)", "Bash(git stash:*)", "Bash(git checkout:*)", "Bash(git switch:*)",
  "Bash(ls:*)", "Bash(cat:*)", "Bash(head:*)", "Bash(tail:*)", "Bash(wc:*)", "Bash(grep:*)", "Bash(rg:*)",
  "Bash(find:*)", "Bash(mkdir:*)", "Bash(touch:*)", "Bash(sed -n:*)", "Bash(diff:*)", "Bash(which:*)",
  "mcp__hqc",
]
deny = [
  "Bash(sudo:*)", "Bash(ssh:*)", "Bash(scp:*)", "Bash(rm -rf /*)",
  "Bash(curl * | sh)", "Bash(curl * | bash)", "Bash(dd:*)", "Bash(mkfs*)",
  "Bash(git push:*)", "Bash(git reset --hard:*)", "Bash(cargo publish:*)", "Bash(npm publish:*)",
  "WebFetch",
]
TOML
    WORKER=(env HQC_HOME="$RUN/home" HQC_TOKEN_STORE=file RUST_LOG="${RUST_LOG:-info}" "$BIN/hqc-worker")
    if [ ! -s "$RUN/home/token" ]; then
        log "enrolling the worker (first run)"
        "${WORKER[@]}" enroll "$HUB" >"$RUN/enroll.log" 2>&1 &
        local epid=$!
        python3 "$HELPER" confirm "$RUN/enroll.log" >"$RUN/worker_id" || fail "confirm"
        wait "$epid" || { cat "$RUN/enroll.log" >&2; fail "enroll"; }
    fi
    start_worker_gui "$BIN/hqc-worker"
    python3 "$HELPER" wait-online "$RUN/worker_id" || fail "worker not online, see $RUN/worker.log"
    log "up: $HUB  workspace $WORKSPACE  logs $RUN/{hub,worker}.log"
}

cmd_down() {
    stop_worker_gui
    stop_pidfile "$RUN/hub.pid" "hqc-hub"
}

cmd_status() {
    for p in hub worker; do
        if alive "$RUN/$p.pid"; then echo "$p: running (pid $(cat "$RUN/$p.pid"))"; else echo "$p: stopped"; fi
    done
    curl -s -m 2 "$HUB/health" >/dev/null 2>&1 && echo "health: ok" || echo "health: no answer at $HUB"
    curl -s -m 2 "$HUB/api/workers" 2>/dev/null | python3 -c "import sys, json
try:
    for w in json.load(sys.stdin)[\"workers\"]: print(\"worker\", w[\"name\"] + \":\", w[\"status\"])
except Exception: pass"
}

cmd_logs() { tail -n 20 -F "$RUN/hub.log" "$RUN/worker.log"; }

cmd_reset() {
    cmd_down
    dropdb --if-exists --force "$DB" 2>/dev/null && log "database $DB dropped"
    rm -rf "$RUN" && log "$RUN removed"
}

case "${1:-}" in
    up) cmd_up ;;
    down) cmd_down ;;
    restart) cmd_down; cmd_up ;;
    status) cmd_status ;;
    logs) cmd_logs ;;
    reset) cmd_reset ;;
    *) sed -n '2,20p' "$0"; exit 2 ;;
esac
