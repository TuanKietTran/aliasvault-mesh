#!/usr/bin/env bash
# End-to-end drills on two throwaway AliasVault all-in-one instances.
# Usage: harness/drill.sh   (DRILL_KEEP=1 skips the final teardown)
#   Drill 1 promote:   seed fake data on A, snapshot A (pg_dump +
#                      secrets/ + compose env), fresh B, stop B,
#                      apply the snapshot to B, start B, log in on B
#                      and read A's fake credential back.
#   Drill 2 node-loss: stop A; B (promoted) keeps serving, readback OK.
#   Drill 3 fork:      start A again, let both accept a write, then
#                      compare VaultManifests — they must diverge.
# Snapshot secrets follow ADR 0001 decision C (file-level rule):
# jwt_key and data_protection_cert_pass are replicated;
# postgres_password is node-local and is NEVER applied over B's own.
# One transcript per drill in docs/drills/. Names and OK/FAIL only;
# secret values are never printed. All mx-test instances are torn
# down at the end. The protected instance is never touched.
set -u
export PATH="$HOME/.local/bin:/opt/homebrew/bin:$PATH"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WT="$(cd "$SCRIPT_DIR/.." && pwd)"
LAB_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
INST_ROOT="$LAB_ROOT/notes/instances"
IMAGE="ghcr.io/aliasvault/aliasvault@sha256:50ca5355cade78373b459c125709e92be3b7566e5b7349aee7d7ec586cc8d2df"
if [ -z "${NODE_PATH:-}" ]; then
  echo "error: NODE_PATH is not set; set NODE_PATH to the node_modules directory of a local Playwright install (usage: NODE_PATH=<playwright modules> node harness/fake-account.mjs)" >&2
  exit 9
fi
export NODE_PATH
cd "$WT" || exit 9
A_NAME=drill-a; A_PORT=18050; A_CID=mx-test-drill-a-aliasvault-1; A_INST="$INST_ROOT/$A_NAME"
B_NAME=drill-b; B_PORT=18080; B_CID=mx-test-drill-b-aliasvault-1; B_INST="$INST_ROOT/$B_NAME"
DRILLS_DIR="$WT/docs/drills"; mkdir -p "$DRILLS_DIR"
WORK="$(mktemp -d /tmp/drill.XXXXXX)"
SNAP="$WORK/snap"; mkdir -p "$SNAP/secrets"
# Fake identity for this run only; values are never printed.
pwgen() { printf 'Aa1!%s' "$(head -c 30 /dev/urandom | base64 | tr -d '\n' | tr '+/' 'AB' | head -c 24)"; }
export FAKE_USER="fake$(date +%s)$RANDOM"
export FAKE_ACCOUNT_PW="$(pwgen)" FAKE_CRED_PW="$(pwgen)"
FAILS=0; T=/dev/null
say() { echo "$@" | tee -a "$T"; }
ok() { say "OK $1"; }
bad() { say "FAIL $1"; FAILS=$((FAILS+1)); }
result() { if [ "$FAILS" = "$1" ]; then say "RESULT $2 OK"; else say "RESULT $2 FAIL"; fi; }
teardown_all() {
  bash harness/instance.sh down "$A_NAME" "$A_PORT" >/dev/null 2>&1 || true
  bash harness/instance.sh down "$B_NAME" "$B_PORT" >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
if [ "${DRILL_KEEP:-}" != "1" ]; then trap teardown_all EXIT; fi
api_ok() { [ "$(curl -s "http://127.0.0.1:$1/api" 2>/dev/null)" = "OK" ]; }
db_ok() {
  local cid="$1" n
  n="$(docker logs "$cid" 2>&1 | grep -c 'password authentication failed')"
  [ "$n" = "0" ] && docker logs "$cid" 2>&1 | grep -qi 'schema is up-to-date'
}
fake() {
  local mode="$1" port="$2" svc="${3:-}" out
  if [ -n "$svc" ]; then
    out="$(FAKE_SERVICE="$svc" node harness/fake-account.mjs "http://127.0.0.1:$port" "$mode" 2>"$WORK/fake.err")"
  else
    out="$(node harness/fake-account.mjs "http://127.0.0.1:$port" "$mode" 2>"$WORK/fake.err")"
  fi
  echo "$out"
}
psql_c() {
  docker exec -e PGPASSWORD="$(docker exec "$1" cat /secrets/postgres_password)" "$1" psql -h localhost -U aliasvault -d aliasvault -t -A -c "$2" 2>/dev/null
}
manifest_summary() { psql_c "$1" 'SELECT count(*) || '\''/'\'' || coalesce(max("RevisionNumber"),0) || '\''/'\'' || coalesce(sum("CredentialsCount"),0) FROM "VaultManifests"'; }
manifest_hashes() { psql_c "$1" 'SELECT coalesce(string_agg("ManifestCiphertextHash", '\'', '\'' ORDER BY "ManifestCiphertextHash"), '\''-'\'') FROM "VaultManifests"'; }
restore_b() {
  docker run --rm --entrypoint sh -v "$B_INST/database:/database" -v "$B_INST/secrets:/bsecrets:ro" -v "$SNAP:/snap:ro" "$IMAGE" -c '
    mkdir -p /var/run/postgresql && chown postgres:postgres /var/run/postgresql
    export PGPASSWORD="$(cat /bsecrets/postgres_password)"
    su postgres -c "/usr/lib/postgresql/16/bin/postgres -D /database/postgres -p 5433" > /tmp/pg.log 2>&1 &
    for i in $(seq 1 60); do pg_isready -h localhost -p 5433 -q && break; sleep 1; done
    pg_isready -h localhost -p 5433 || { echo HELPER_RESTORE_ERRORS=server-not-ready; exit 1; }
    psql -h localhost -p 5433 -U aliasvault -d postgres -q -v ON_ERROR_STOP=1 -c "DROP DATABASE aliasvault" -c "CREATE DATABASE aliasvault OWNER aliasvault" || { echo HELPER_RESTORE_ERRORS=drop-create-failed; exit 1; }
    psql -h localhost -p 5433 -U aliasvault -d aliasvault -q -o /dev/null -f /snap/dump.sql 2> /tmp/restore.err
    echo "HELPER_RESTORE_ERRORS=$(grep -c ERROR /tmp/restore.err)"
    su postgres -c "/usr/lib/postgresql/16/bin/pg_ctl -D /database/postgres stop -m fast" > /dev/null 2>&1
  ' > "$WORK/helper.log" 2>&1
  grep -o 'HELPER_RESTORE_ERRORS=[^ ]*' "$WORK/helper.log" | tail -1
}
# ---------------- Drill 1: promote ----------------
T="$DRILLS_DIR/promote.txt"; : > "$T"; D0=$FAILS
say "# Drill 1 — promote (replicate A -> B) $(date '+%Y-%m-%dT%H:%M:%S%z')"
bash harness/instance.sh down "$A_NAME" "$A_PORT" >/dev/null 2>&1 || true
bash harness/instance.sh down "$B_NAME" "$B_PORT" >/dev/null 2>&1 || true
if bash harness/instance.sh up "$A_NAME" "$A_PORT" >/dev/null 2>&1; then ok "fresh-a up"; else bad "fresh-a up"; fi
OUT="$(fake create "$A_PORT")"
if [ "$OUT" = "ok items=1 readback=1" ]; then ok "seed-a fake-account $OUT"; else bad "seed-a fake-account ($OUT)"; fi
if docker exec -e PGPASSWORD="$(docker exec "$A_CID" cat /secrets/postgres_password)" "$A_CID" pg_dump -h localhost -U aliasvault -d aliasvault > "$SNAP/dump.sql" 2>"$WORK/dump.err" && [ -s "$SNAP/dump.sql" ]; then ok "snapshot-db pg_dump bytes=$(wc -c < "$SNAP/dump.sql" | tr -d ' ') errors=$(grep -c ERROR "$WORK/dump.err")"; else bad "snapshot-db pg_dump"; fi
cp -a "$A_INST/secrets/." "$SNAP/secrets/"
say "snapshot-secrets names: $(ls -1 "$SNAP/secrets" | tr '\n' ' ')"
cp "$A_INST/docker-compose.yml" "$SNAP/docker-compose.yml"
ENAMES="$(sed -n '/environment:/,/ports:/p' "$SNAP/docker-compose.yml" | grep -oE '[A-Z][A-Z_]+:' | tr -d ':' | sort -u | tr '\n' ' ')"
if [ -f "$SNAP/docker-compose.yml" ]; then ok "snapshot-env compose env names: $ENAMES"; else bad "snapshot-env"; fi
if bash harness/instance.sh up "$B_NAME" "$B_PORT" >/dev/null 2>&1; then ok "fresh-b up"; else bad "fresh-b up"; fi
B_PG_BEFORE="$(sha256sum "$B_INST/secrets/postgres_password" | cut -d' ' -f1)"
docker stop "$B_CID" >/dev/null 2>&1 && ok "stop-b" || bad "stop-b"
R="$(restore_b)"
if [ "$R" = "HELPER_RESTORE_ERRORS=0" ]; then ok "apply-db helper restore errors=0"; else bad "apply-db helper restore ($R)"; fi
cp "$SNAP/secrets/jwt_key" "$B_INST/secrets/jwt_key"
cp "$SNAP/secrets/data_protection_cert_pass" "$B_INST/secrets/data_protection_cert_pass"
if cmp -s "$SNAP/secrets/jwt_key" "$B_INST/secrets/jwt_key" && cmp -s "$SNAP/secrets/data_protection_cert_pass" "$B_INST/secrets/data_protection_cert_pass"; then ok "secrets-apply replicated: jwt_key data_protection_cert_pass"; else bad "secrets-apply replicated files differ"; fi
B_PG_AFTER="$(sha256sum "$B_INST/secrets/postgres_password" | cut -d' ' -f1)"
if [ "$B_PG_BEFORE" = "$B_PG_AFTER" ]; then ok "secrets-apply postgres_password node-local (not applied, B unchanged)"; else bad "secrets-apply postgres_password changed on B"; fi
if bash harness/instance.sh up "$B_NAME" "$B_PORT" >/dev/null 2>&1; then ok "start-b"; else bad "start-b"; fi
if api_ok "$B_PORT"; then ok "api-b GET /api 200 OK"; else bad "api-b GET /api"; fi
if db_ok "$B_CID"; then ok "db-b connect (authfail=0, schema up-to-date)"; else bad "db-b connect"; fi
OUT="$(fake verify "$B_PORT")"
if [ "$OUT" = "ok items=1 readback=1" ]; then ok "readback-b fake-account verify $OUT (A credential read on promoted B)"; else bad "readback-b fake-account verify ($OUT)"; fi
result "$D0" promote
# ---------------- Drill 2: node-loss ----------------
T="$DRILLS_DIR/node-loss.txt"; : > "$T"; D0=$FAILS
say "# Drill 2 — node-loss (stop A, B promoted) $(date '+%Y-%m-%dT%H:%M:%S%z')"
docker stop "$A_CID" >/dev/null 2>&1 || true
if [ -z "$(docker ps -q --filter "name=$A_CID")" ]; then ok "stop-a (A container not running)"; else bad "stop-a (A still running)"; fi
if api_ok "$B_PORT"; then ok "api-b GET /api 200 OK with A down"; else bad "api-b GET /api with A down"; fi
OUT="$(fake verify "$B_PORT")"
if [ "$OUT" = "ok items=1 readback=1" ]; then ok "readback-b with A down $OUT"; else bad "readback-b with A down ($OUT)"; fi
result "$D0" node-loss
# ---------------- Drill 3: fork ----------------
T="$DRILLS_DIR/fork.txt"; : > "$T"; D0=$FAILS
say "# Drill 3 — fork (both accept a write, compare manifests) $(date '+%Y-%m-%dT%H:%M:%S%z')"
if bash harness/instance.sh up "$A_NAME" "$A_PORT" >/dev/null 2>&1; then ok "restart-a (old primary back, pre-fork state)"; else bad "restart-a"; fi
HA="$(manifest_hashes "$A_CID")"; HB="$(manifest_hashes "$B_CID")"
if [ -n "$HA" ] && [ "$HA" = "$HB" ]; then ok "fork-baseline manifests equal (count/rev/creds A=$(manifest_summary "$A_CID") B=$(manifest_summary "$B_CID"))"; else bad "fork-baseline manifests differ before writes"; fi
OUT="$(fake add "$A_PORT" "Fork Service A")"
if [ "$OUT" = "ok items=1 readback=1" ]; then ok "write-a add credential (Fork Service A) $OUT"; else bad "write-a add credential ($OUT)"; fi
OUT="$(fake add "$B_PORT" "Fork Service B")"
if [ "$OUT" = "ok items=1 readback=1" ]; then ok "write-b add credential (Fork Service B) $OUT"; else bad "write-b add credential ($OUT)"; fi
SA="$(manifest_summary "$A_CID")"; SB="$(manifest_summary "$B_CID")"
HA="$(manifest_hashes "$A_CID")"; HB="$(manifest_hashes "$B_CID")"
say "manifests after writes (count/revision/credentials): A=$SA B=$SB"
if [ -n "$HA" ] && [ -n "$HB" ] && [ "$HA" != "$HB" ]; then ok "fork-detected manifests diverged after independent writes"; else bad "fork-detected manifests still identical"; fi
result "$D0" fork
# ---------------- Teardown ----------------
teardown_all
trap - EXIT
echo "drill.sh done fails=$FAILS transcripts in docs/drills/"
[ "$FAILS" = "0" ]
