#!/usr/bin/env bash
#
# End-to-end integration test for the M3c re-decryption queue against a real
# Synapse. It exercises the whole prize path with no manual steps:
#
#   1. Bring up Postgres + Synapse (the `integration` compose profile).
#   2. Register a fresh, unique user and run the seeder ("device A"): it makes
#      an encrypted room, sends messages, and mints a Secure Backup recovery key.
#   3. Run axon as a fresh, unverified "device B" with NO recovery key — assert
#      the messages land as UTDs (m.room.encrypted rows with content IS NULL).
#   4. Restart axon WITH the recovery key — assert the rows flip to decrypted
#      m.room.message with content populated (the re-decryption queue working).
#
# Runs locally and in CI. Configuration via env vars (all have defaults):
#
#   POSTGRES_PORT   host port for the compose Postgres        (default 5432)
#   SYNAPSE_PORT    host port for Synapse                     (default 8008)
#   MESSAGE_COUNT   how many messages the seeder sends        (default 3)
#   TIMEOUT         seconds to wait for each DB condition      (default 90)
#   KEEP_UP=1       leave containers running at the end (faster local iteration)
#
# On macOS a Homebrew Postgres often holds 5432, so locally you'll typically run:
#
#   POSTGRES_PORT=5433 scripts/integration-test.sh
#
set -euo pipefail

POSTGRES_PORT=${POSTGRES_PORT:-5432}
SYNAPSE_PORT=${SYNAPSE_PORT:-8008}
MESSAGE_COUNT=${MESSAGE_COUNT:-3}
TIMEOUT=${TIMEOUT:-90}
KEEP_UP=${KEEP_UP:-0}

# A dedicated, throwaway database (dropped + recreated each run) keeps the test
# isolated from the shared dev `axon` DB — otherwise axon tries to sync every
# account row it finds, and stale accounts from other work spam decrypt errors.
ITEST_DB="${ITEST_DB:-axon_itest}"
HS_URL="http://localhost:${SYNAPSE_PORT}"
DATABASE_URL="postgres://axon:axon@127.0.0.1:${POSTGRES_PORT}/${ITEST_DB}"

# Resolve repo root from this script's location so it runs from anywhere.
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Exported so `docker compose` publishes the ports we expect.
export POSTGRES_PORT SYNAPSE_PORT

log()  { printf '\n\033[1;36m=== %s ===\033[0m\n' "$*" >&2; }
info() { printf '    %s\n' "$*" >&2; }
die()  { printf '\n\033[1;31mFAIL: %s\033[0m\n' "$*" >&2; exit 1; }

dc() { docker compose --profile integration "$@"; }

# --- bring up services ------------------------------------------------------

log "Starting Postgres + Synapse"
dc up -d postgres synapse

PG="$(docker compose ps -q postgres)"
[ -n "$PG" ] || die "could not resolve the postgres container"

# admin_q runs against the default `axon` DB (for CREATE/DROP DATABASE);
# psql_q runs against the throwaway test DB (for assertions).
admin_q() { docker exec -i "$PG" psql -U axon -d axon -tAc "$1" | tr -d '[:space:]'; }
# While axon is still running migrations on the fresh DB the tables don't exist
# yet; psql then exits non-zero with "relation does not exist". We capture that
# as an empty string (never letting it trip `set -e`) so the poller just retries.
psql_q() {
    local out
    out="$(docker exec -i "$PG" psql -U axon -d "$ITEST_DB" -tAc "$1" 2>/dev/null)" || out=""
    printf '%s' "$out" | tr -d '[:space:]'
}

log "Waiting for Postgres to accept connections"
for _ in $(seq 1 30); do
    if docker exec "$PG" pg_isready -U axon -d axon >/dev/null 2>&1; then break; fi
    sleep 1
done
docker exec "$PG" pg_isready -U axon -d axon >/dev/null 2>&1 || die "Postgres never became ready"
info "Postgres ready on host port ${POSTGRES_PORT}"

log "Creating throwaway test database ${ITEST_DB}"
# WITH (FORCE) terminates any leftover connections (Postgres 13+); axon runs its
# own migrations on first connect, so an empty DB is all we need.
admin_q "DROP DATABASE IF EXISTS ${ITEST_DB} WITH (FORCE);" >/dev/null
admin_q "CREATE DATABASE ${ITEST_DB};" >/dev/null
info "Created ${ITEST_DB}"

log "Waiting for Synapse + Simplified Sliding Sync (MSC4186)"
for _ in $(seq 1 60); do
    if curl -fsS "${HS_URL}/_matrix/client/versions" 2>/dev/null \
        | grep -q '"org.matrix.simplified_msc3575":[[:space:]]*true'; then
        break
    fi
    sleep 2
done
curl -fsS "${HS_URL}/_matrix/client/versions" 2>/dev/null \
    | grep -q '"org.matrix.simplified_msc3575":[[:space:]]*true' \
    || die "Synapse is not advertising MSC4186 (org.matrix.simplified_msc3575); axon speaks only this"
info "Synapse healthy and advertising MSC4186"

# --- build --------------------------------------------------------------------

log "Building axon-server + seeder"
cargo build -p axon-server -p axon-itest

AXON_BIN="${REPO_ROOT}/target/debug/axon"
SEED_BIN="${REPO_ROOT}/target/debug/seed"
[ -x "$AXON_BIN" ] || die "axon binary not found at $AXON_BIN"
[ -x "$SEED_BIN" ] || die "seed binary not found at $SEED_BIN"

# --- unique identities + run state -------------------------------------------

SUFFIX="$(date +%s)-$$"
LOCALPART="alice-${SUFFIX}"
USER_ID="@${LOCALPART}:localhost"
PASSWORD="pass-${SUFFIX}"
# Isolated run dir: axon loads .env via dotenvy from cwd upward, and the project
# .env points at a *remote* account. Running from here keeps us off it, and the
# SDK store (axon-data/sync) is created fresh under here so device B starts
# unverified. The same dir is reused on restart so the device persists.
RUN_DIR="$(mktemp -d "${TMPDIR:-/tmp}/axon-itest.XXXXXX")"
AXON_PID=""

cleanup() {
    local code=$?
    [ -n "$AXON_PID" ] && kill "$AXON_PID" 2>/dev/null || true
    rm -rf "$RUN_DIR" 2>/dev/null || true
    if [ "$KEEP_UP" != "1" ]; then
        # Drop the throwaway DB, then the containers. (KEEP_UP=1 leaves both for
        # inspection — the seeding user on Synapse is unique per run regardless.)
        admin_q "DROP DATABASE IF EXISTS ${ITEST_DB} WITH (FORCE);" >/dev/null 2>&1 || true
        log "Tearing down containers (KEEP_UP=1 to keep them)"
        dc down >/dev/null 2>&1 || true
    fi
    exit "$code"
}
trap cleanup EXIT

log "Registering seeding user ${USER_ID}"
SYN="$(docker compose ps -q synapse)"
docker exec "$SYN" register_new_matrix_user \
    -c /data/homeserver.yaml -u "$LOCALPART" -p "$PASSWORD" --no-admin "$HS_URL" \
    >/dev/null 2>&1 \
    || die "user registration failed"
info "Registered ${USER_ID}"

# --- seed (device A) ----------------------------------------------------------

log "Seeding encrypted room + key backup (device A)"
SEED_JSON="$(
    SEED_HOMESERVER="$HS_URL" \
    SEED_USER_ID="$USER_ID" \
    SEED_PASSWORD="$PASSWORD" \
    SEED_STORE_DIR="${RUN_DIR}/seed-store" \
    SEED_MESSAGE_COUNT="$MESSAGE_COUNT" \
    RUST_LOG="${RUST_LOG:-warn}" \
    "$SEED_BIN"
)"
[ -n "$SEED_JSON" ] || die "seeder produced no output"

# Parse the seeder's JSON (python3 is present locally and on CI runners).
read_json() { printf '%s' "$SEED_JSON" | python3 -c "import sys,json;print(json.load(sys.stdin)['$1'])"; }
ROOM_ID="$(read_json room_id)"
RECOVERY_KEY="$(read_json recovery_key)"
[ -n "$ROOM_ID" ] || die "no room_id from seeder"
[ -n "$RECOVERY_KEY" ] || die "no recovery_key from seeder"
info "room=${ROOM_ID}  recovery_key=${RECOVERY_KEY:0:9}…  messages=${MESSAGE_COUNT}"

# --- helpers for running axon + polling the DB -------------------------------

# Count UTDs (encrypted, not yet decrypted) for this run's user.
utd_count() {
    psql_q "SELECT count(*) FROM events e JOIN accounts a USING (account_id)
            WHERE a.user_id = '${USER_ID}'
              AND e.event_type = 'm.room.encrypted' AND e.content IS NULL;"
}
# Count back-filled (decrypted) message rows for this run's user.
decrypted_count() {
    psql_q "SELECT count(*) FROM events e JOIN accounts a USING (account_id)
            WHERE a.user_id = '${USER_ID}'
              AND e.event_type = 'm.room.message' AND e.content IS NOT NULL;"
}

# run_axon [recovery_key] — start axon in the background against the local
# servers, isolated from the project .env via cwd=$RUN_DIR. An array keeps the
# env assignments intact — the recovery key contains spaces.
run_axon() {
    local recovery="${1:-}"
    local -a envs=(
        DATABASE_URL="$DATABASE_URL"
        AXON_SERVER__PORT=18080
        AXON_SYNC__STORE_KEY="itest-store-key"
        AXON_SYNC__ACCOUNT__USER_ID="$USER_ID"
        AXON_SYNC__ACCOUNT__HOMESERVER_URL="$HS_URL"
        AXON_SYNC__ACCOUNT__PASSWORD="$PASSWORD"
        RUST_LOG="${RUST_LOG:-info,axon_sync=debug}"
    )
    [ -n "$recovery" ] && envs+=(AXON_SYNC__ACCOUNT__RECOVERY_KEY="$recovery")
    (
        cd "$RUN_DIR"
        exec env "${envs[@]}" "$AXON_BIN"
    ) >"${RUN_DIR}/axon.log" 2>&1 &
    AXON_PID=$!
}

stop_axon() {
    [ -n "$AXON_PID" ] || return 0
    kill "$AXON_PID" 2>/dev/null || true
    wait "$AXON_PID" 2>/dev/null || true
    AXON_PID=""
}

# wait_until <description> <op> <want> <count-fn> — poll until the count
# satisfies the test. <op> is a `test`/`[ ]` integer operator: -eq, -ge, etc.
wait_until() {
    local desc="$1" op="$2" want="$3" fn="$4" got deadline
    deadline=$(( $(date +%s) + TIMEOUT ))
    while :; do
        got="$("$fn")"
        if [ -n "$got" ] && [ "$got" "$op" "$want" ] 2>/dev/null; then
            info "${desc}: ${got} (${op} ${want}, ok)"
            return 0
        fi
        if [ "$(date +%s)" -ge "$deadline" ]; then
            info "----- last 40 lines of axon.log -----"
            tail -40 "${RUN_DIR}/axon.log" >&2 || true
            die "timed out waiting for ${desc}: wanted ${op} ${want}, got ${got:-<none>}"
        fi
        sleep 2
    done
}

# --- phase 1: fresh device B, no recovery key -> UTDs accumulate -------------
#
# axon archives the events Simplified Sliding Sync surfaces. As of M4a the
# per-room timeline window is raised from the SDK default of 1 (sync.timeline_limit,
# default 20), so device B now sees several UTDs rather than just the latest — but
# still bounded by that window, not the full backlog. We assert on the count axon
# *actually* archived rather than MESSAGE_COUNT, then prove that exact set flips.

log "Phase 1: axon as fresh device B (no recovery key) — expect UTDs"
run_axon
wait_until "UTDs accumulated" -ge 1 utd_count
SEEN="$(utd_count)"
info "device B archived ${SEEN} encrypted event(s) as UTD(s)"
[ "$(decrypted_count)" = "0" ] || die "expected 0 decrypted rows before recovery, got $(decrypted_count)"
stop_axon

# --- phase 2: restart with recovery key -> rows flip to decrypted ------------

log "Phase 2: restart axon WITH recovery key — expect re-decryption"
run_axon "$RECOVERY_KEY"
wait_until "rows back-filled to decrypted" -eq "$SEEN" decrypted_count
# And no UTDs should remain.
wait_until "UTDs drained to zero" -eq 0 utd_count
stop_axon

log "PASS — re-decryption queue back-filled ${SEEN}/${SEEN} UTD(s) end to end"
