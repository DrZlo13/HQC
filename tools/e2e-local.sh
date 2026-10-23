#!/usr/bin/env bash
# End-to-end run of the iteration 1 criterion (SPEC §19.1) on one machine:
# dev-mode hub on loopback (with Web Push keys and, if client/dist is built, the web client)
# + a fresh worker + one live Claude Code turn with an approval.
#
#   tools/e2e-local.sh
#
# Spawns `claude -p` once (model claude-haiku-5-5, override with E2E_MODEL): costs the
# subscription. Uses its own database (hqc_e2e, dropped at the end), its own HQC_HOME and
# workspace in tmp/e2e-<date>/ (recreated on every run) and never touches ~/.hqc.
# Environment: E2E_PROFILE=release builds release binaries; E2E_DB (default hqc_e2e);
# E2E_TURN_TIMEOUT (seconds, default 240); E2E_CLAUDE_BIN (claude_bin in worker.toml).
# Rehearsal: if E2E_CLAUDE_BIN is not Claude Code (its `--version` does not say so, e.g.
# /usr/bin/false), everything but the live turn is checked and the turn must fail to start:
# `system agent_start_failed`, the input `failed`, state `error`; prints "PASS (rehearsal)".
# Exit status 0 = PASS.

set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HELPER="$ROOT/tools/e2e-local.py"
RUN="$ROOT/tmp/e2e-$(date +%Y%m%d)"
DB="${E2E_DB:-hqc_e2e}"
PROFILE="${E2E_PROFILE:-dev}"
HUB_PID="" WORKER_PID="" ENROLL_PID=""
RESULT=FAIL
REHEARSAL=""

log() { printf '[e2e] %s\n' "$*" >&2; }
fail() { log "FAIL: $*"; exit 1; }

stop_pid() { # pid name: SIGTERM, wait up to 15 s, then SIGKILL
    local pid=$1 name=$2 i
    [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null || return 0
    log "stopping $name (pid $pid)"
    kill -TERM "$pid" 2>/dev/null
    for i in $(seq 1 30); do
        kill -0 "$pid" 2>/dev/null || { wait "$pid" 2>/dev/null; return 0; }
        sleep 0.5
    done
    log "$name did not exit on SIGTERM, killing"
    kill -KILL "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
}

cleanup() {
    local status=$?
    trap - EXIT INT TERM
    stop_pid "$ENROLL_PID" "hqc-worker enroll"
    stop_pid "$WORKER_PID" "hqc-worker run"
    stop_pid "$HUB_PID" "hqc-hub"
    dropdb --if-exists --force "$DB" 2>/dev/null && log "database $DB dropped"
    if [ -f "$RUN/scenario.json" ]; then
        echo
        python3 "$HELPER" report "$RUN/scenario.json"
    fi
    echo
    echo "logs: $RUN"
    [ "$status" -eq 0 ] && RESULT=PASS
    [ -n "$REHEARSAL" ] && RESULT="$RESULT (rehearsal)"
    echo "$RESULT"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP
# Output piped into `head` must not kill the script before cleanup: writes just fail.
trap '' PIPE

# 1. Build, database, hub.
log "building hqc-hub and hqc-worker ($PROFILE)"
if [ "$PROFILE" = release ]; then
    cargo build --release --manifest-path "$ROOT/Cargo.toml" -p hqc-hub -p hqc-worker || fail "build"
    BIN="$ROOT/target/release"
else
    cargo build --manifest-path "$ROOT/Cargo.toml" -p hqc-hub -p hqc-worker || fail "build"
    BIN="$ROOT/target/debug"
fi

rm -rf "$RUN"
mkdir -p "$RUN/home" "$RUN/workspace"
chmod 700 "$RUN/home"
WORKSPACE="$(cd "$RUN/workspace" && pwd -P)"

PORT="$(python3 -c '
import random, socket
for p in random.sample(range(18000, 29000), 200):
    s = socket.socket()
    try:
        s.bind(("127.0.0.1", p)); print(p); break
    except OSError: pass
    finally: s.close()
')"
[ -n "$PORT" ] || fail "no free port above 18000"
export HUB="http://127.0.0.1:$PORT"

dropdb --if-exists --force "$DB" || fail "dropdb $DB"
createdb "$DB" || fail "createdb $DB"
log "database $DB created"

# VAPID keys (SPEC §16): raw P-256 private scalar and uncompressed public point, base64url.
openssl ecparam -name prime256v1 -genkey -noout -out "$RUN/vapid.pem" 2>/dev/null || fail "openssl"
b64url() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }
VAPID_PRIVATE="$(openssl ec -in "$RUN/vapid.pem" -outform DER 2>/dev/null | tail -c +8 | head -c 32 | b64url)"
VAPID_PUBLIC="$(openssl ec -in "$RUN/vapid.pem" -pubout -outform DER 2>/dev/null | tail -c 65 | b64url)"

# The built client is served too, if there is one (npm run build in client/).
CLIENT_DIR=""
[ -f "$ROOT/client/dist/index.html" ] && CLIENT_DIR="$ROOT/client/dist"

log "starting hqc-hub on $HUB"
env -u HQC_ACCESS_TEAM_DOMAIN -u HQC_ACCESS_AUD HQC_CLIENT_DIR="$CLIENT_DIR" \
    HQC_DEV=1 HQC_DEV_USER=e2e@local HQC_HUB_LISTEN="127.0.0.1:$PORT" \
    HQC_DATABASE_URL="postgresql://localhost/$DB" \
    HQC_VAPID_PRIVATE="$VAPID_PRIVATE" HQC_VAPID_PUBLIC="$VAPID_PUBLIC" \
    HQC_VAPID_SUBJECT=mailto:e2e@local RUST_LOG="${RUST_LOG:-info}" \
    "$BIN/hqc-hub" >"$RUN/hub.log" 2>&1 &
HUB_PID=$!
python3 "$HELPER" wait-health || fail "hub did not come up, see $RUN/hub.log"
python3 "$HELPER" vapid "$VAPID_PUBLIC" || fail "vapid"
if [ -n "$CLIENT_DIR" ]; then
    python3 "$HELPER" client || fail "client"
else
    log "client/dist not built: skipping the web client check"
fi

# 2. Worker home and config.
CLAUDE_BIN_LINE=""
if [ -n "${E2E_CLAUDE_BIN:-}" ]; then
    CLAUDE_BIN_LINE="claude_bin = \"$E2E_CLAUDE_BIN\""
    if ! "$E2E_CLAUDE_BIN" --version 2>/dev/null | grep -q "(Claude Code)"; then
        REHEARSAL=1
        log "$E2E_CLAUDE_BIN is not Claude Code: rehearsal, the turn must fail to start"
    fi
fi
cat >"$RUN/home/worker.toml" <<EOF
hub_url = "$HUB"
allowed_roots = ["$WORKSPACE"]
idle_timeout = 0
allow_foreign_sessions = false
$CLAUDE_BIN_LINE
EOF
# Run directly (env execs), not through a shell function: $! must be the worker itself.
WORKER=(env -u HQC_WORKER_CONFIG HQC_HOME="$RUN/home" HQC_TOKEN_STORE=file
    RUST_LOG="${RUST_LOG:-info}" "$BIN/hqc-worker")

# 3. Enroll, confirm as the dev user, run.
log "hqc-worker enroll $HUB"
"${WORKER[@]}" enroll "$HUB" >"$RUN/enroll.log" 2>&1 &
ENROLL_PID=$!
python3 "$HELPER" confirm "$RUN/enroll.log" >"$RUN/worker_id" || fail "confirm"
wait "$ENROLL_PID" || { ENROLL_PID=""; cat "$RUN/enroll.log" >&2; fail "enroll"; }
ENROLL_PID=""
[ -s "$RUN/home/token" ] || fail "no token in $RUN/home/token"
log "token stored in $RUN/home/token ($(stat -f %Lp "$RUN/home/token"))"

"${WORKER[@]}" run >"$RUN/worker.log" 2>&1 &
WORKER_PID=$!
python3 "$HELPER" wait-online "$RUN/worker_id" || fail "worker not online, see $RUN/worker.log"
# Workspaces are hub objects (SPEC §3): the worker checks the folder with fs.stat.
python3 "$HELPER" workspace "$RUN/worker_id" "$WORKSPACE" || fail "workspace, see $RUN/hub.log"

# 4. Session in that workspace, input, approval, final answer.
E2E_REHEARSAL="$REHEARSAL" python3 "$HELPER" scenario "$WORKSPACE" "$RUN/scenario.json" \
    || fail "scenario checks"

# 5. The worker stops on SIGTERM (kills the agent, flushes the outbox); cleanup does the rest.
stop_pid "$WORKER_PID" "hqc-worker run"
WORKER_PID=""
grep -q "worker stopped" "$RUN/worker.log" || fail "worker did not shut down cleanly"
exit 0
