#!/usr/bin/env bash
# Two-instance drill: serve a verified snapshot from one instance and
# pull it into a second instance over loopback, then compare payloads.
# Throwaway fake data only; everything lives in a temporary work area
# that is removed on every exit path.

set -u
set -o pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
NODE_KEY="node-a-key"
WORK=""
SERVE_PID=""
PORT_A=""
PORT_B=""

fail() {
  echo "FAIL: $*"
  exit 1
}

step() {
  echo "== $*"
}

# A port is free when a throwaway listener can bind it on loopback.
port_free() {
  node -e 'const net=require("net");const p=Number(process.argv[1]);const s=net.createServer();s.once("error",()=>process.exit(1));s.once("listening",()=>{s.close(()=>process.exit(0));});s.listen(p,"127.0.0.1");' "$1"
}

cleanup() {
  if [ -n "$SERVE_PID" ] && kill -0 "$SERVE_PID" 2>/dev/null; then
    kill "$SERVE_PID" 2>/dev/null
    wait "$SERVE_PID" 2>/dev/null
  fi
  if [ -n "$WORK" ] && [ -d "$WORK" ]; then
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

choose_ports() {
  if [ -n "${AVMESH_PORT_A:-}" ] || [ -n "${AVMESH_PORT_B:-}" ]; then
    if [ -z "${AVMESH_PORT_A:-}" ] || [ -z "${AVMESH_PORT_B:-}" ]; then
      echo "error: set both AVMESH_PORT_A and AVMESH_PORT_B, or neither"
      exit 1
    fi
    PORT_A="$AVMESH_PORT_A"
    PORT_B="$AVMESH_PORT_B"
    for p in "$PORT_A" "$PORT_B"; do
      case "$p" in
        ''|*[!0-9]*) echo "error: port $p is not a number"; exit 1 ;;
      esac
      if [ "$p" -lt 18000 ] || [ "$p" -gt 18999 ]; then
        echo "error: port $p is outside the allowed range 18000-18999"
        exit 1
      fi
    done
    if [ "$PORT_A" = "$PORT_B" ]; then
      echo "error: the two chosen ports must differ"
      exit 1
    fi
    for p in "$PORT_A" "$PORT_B"; do
      if ! port_free "$p"; then
        echo "error: port $p is already in use; refusing to run"
        exit 1
      fi
    done
  else
    local p
    for p in $(seq 18100 18199); do
      if port_free "$p"; then
        if [ -z "$PORT_A" ]; then
          PORT_A="$p"
        else
          PORT_B="$p"
          break
        fi
      fi
    done
    if [ -z "$PORT_A" ] || [ -z "$PORT_B" ]; then
      echo "error: no two free ports found in 18100-18199"
      exit 1
    fi
  fi
}

step "preflight"
command -v node >/dev/null 2>&1 || fail "node is required but was not found"
choose_ports
echo "ports chosen: $PORT_A $PORT_B (second port reserved and checked)"

step "build"
if [ -n "${AVMESH_BIN:-}" ]; then
  BIN="$AVMESH_BIN"
else
  (cd "$REPO_ROOT" && cargo build --offline) || fail "cargo build failed"
  BIN="$REPO_ROOT/target/debug/avmesh"
fi
[ -x "$BIN" ] || fail "agent binary is not executable: $BIN"
echo "binary: $BIN"

WORK="$(mktemp -d)" || fail "could not create the work area"
A_HOME="$WORK/a"
A_DATA="$A_HOME/store"
B_DATA="$WORK/b"
SRC="$WORK/src"
mkdir -p "$A_DATA" "$B_DATA" "$SRC/secrets" || fail "could not create instance directories"
echo "work area: $WORK"

step "fixtures"
# Fake sources only: a small database export, a secrets directory with
# a marker file whose bytes are compared after the pull, and an env file.
printf 'fake database export\n' > "$SRC/db-export.sql" || fail "fixture write failed"
printf 'fake marker payload for comparison\n' > "$SRC/secrets/marker.txt" || fail "fixture write failed"
printf 'fake signing material\n' > "$SRC/secrets/jwt-key.txt" || fail "fixture write failed"
printf 'PRIVATE_EMAIL_DOMAINS=example.ts.net\n' > "$SRC/instance.env" || fail "fixture write failed"

step "instance A: token, snapshot, promote"
"$BIN" token init --data-dir "$A_DATA" || fail "token init on A failed"
"$BIN" snapshot create --data-dir "$A_HOME" --db-export "$SRC/db-export.sql" --secrets-dir "$SRC/secrets" --env-file "$SRC/instance.env" --node-id "$NODE_KEY" --vault v1 --revision 3 --at 1700000000 || fail "snapshot create on A failed"
SNAP="$A_HOME/snapshots/snap-1700000000"
[ -d "$SNAP" ] || fail "created snapshot is missing: $SNAP"
# Stage the created snapshot into the store staging area, then promote
# it with the CLI so A ends with a verified current snapshot.
mkdir -p "$A_DATA" || fail "could not create the store directory"
cp -R "$SNAP" "$A_DATA/staging" || fail "staging the snapshot on A failed"
"$BIN" promote --data-dir "$A_HOME" --confirm-primary-down || fail "promote on A failed"
[ -d "$A_DATA/current" ] || fail "A has no current snapshot after promote"
"$BIN" snapshot verify --snapshot "$A_DATA/current" || fail "verify of A's current snapshot failed"
echo "A current snapshot verified"

step "instance B: token copy"
# Copying A's token file into B's data dir simulates the out-of-band
# token copy an operator performs when enrolling a second node.
cp "$A_DATA/mesh.token" "$B_DATA/mesh.token" || fail "token copy to B failed"

step "serve on A"
"$BIN" serve --data-dir "$A_DATA" --listen "127.0.0.1:$PORT_A" --allow "$NODE_KEY" > "$WORK/serve.log" 2>&1 &
SERVE_PID=$!
READY=0
for i in $(seq 1 50); do
  if ! kill -0 "$SERVE_PID" 2>/dev/null; then
    cat "$WORK/serve.log"
    fail "serve exited before accepting connections"
  fi
  if grep -q "127.0.0.1:$PORT_A" "$WORK/serve.log" 2>/dev/null || ! port_free "$PORT_A"; then
    READY=1
    break
  fi
  sleep 0.2
done
[ "$READY" = "1" ] || fail "serve did not start accepting on port $PORT_A"
echo "serve running: pid $SERVE_PID on 127.0.0.1:$PORT_A"

step "pull into B"
"$BIN" pull --data-dir "$B_DATA" --from "http://127.0.0.1:$PORT_A" --allow "$NODE_KEY" || fail "pull into B failed"
[ -d "$B_DATA/staging" ] || fail "B has no staged snapshot after pull"
"$BIN" snapshot verify --snapshot "$B_DATA/staging" || fail "verify of B's staged snapshot failed"
cmp "$A_DATA/current/payload/secrets/marker.txt" "$B_DATA/staging/payload/secrets/marker.txt" || fail "marker payload differs between A and B"
echo "marker payload matches between A and B"

step "negative control: wrong token is refused"
printf '0000000000000000000000000000000000000000000000000000000000000000\n' > "$WORK/wrong.token"
if "$BIN" pull --data-dir "$WORK/b-wrong" --from "http://127.0.0.1:$PORT_A" --allow "$NODE_KEY" --token-file "$WORK/wrong.token"; then
  fail "a pull with a wrong token succeeded; it must be refused"
fi
echo "wrong-token pull refused as expected"

step "teardown checks"
kill "$SERVE_PID" 2>/dev/null
wait "$SERVE_PID" 2>/dev/null
CHECKS_FAILED=0
if kill -0 "$SERVE_PID" 2>/dev/null; then
  echo "check: serve process stopped: FAILED (pid $SERVE_PID still running)"
  CHECKS_FAILED=1
else
  echo "check: serve process stopped: ok"
fi
SERVE_PID=""
for p in "$PORT_A" "$PORT_B"; do
  if port_free "$p"; then
    echo "check: port $p free: ok"
  else
    echo "check: port $p free: FAILED"
    CHECKS_FAILED=1
  fi
done
rm -rf "$WORK"
if [ -e "$WORK" ]; then
  echo "check: work area removed: FAILED ($WORK still exists)"
  CHECKS_FAILED=1
else
  echo "check: work area removed: ok"
fi
WORK=""
[ "$CHECKS_FAILED" = "0" ] || fail "one or more teardown checks failed"
echo "drill result: ok"
exit 0
