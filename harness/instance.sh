#!/usr/bin/env bash
# Throwaway AliasVault all-in-one instance lifecycle for mesh experiments.
# Usage: instance.sh up|down NAME PORT
#   NAME  instance name ([a-z0-9-]); compose project is mx-test-<name>
#   PORT  base port 18010-18996 (127.0.0.1 only): PORT=web/80,
#         PORT+1=https/443, PORT+2=smtp/25, PORT+3=submission/587
# Data lives under $LAB_ROOT/notes/instances/<name> and is removed on down.
# LAB_ROOT defaults to the workspace dir containing this worktree.
set -euo pipefail
IMAGE="ghcr.io/aliasvault/aliasvault@sha256:50ca5355cade78373b459c125709e92be3b7566e5b7349aee7d7ec586cc8d2df"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
LAB_ROOT="${LAB_ROOT:-$(cd "$SCRIPT_DIR/../../.." && pwd)}"
INST_ROOT="$LAB_ROOT/notes/instances"
cmd="${1:-}"; name="${2:-}"; port="${3:-}"
case "$name" in *[!a-z0-9-]*|''|-*) echo "bad NAME: $name" >&2; exit 2;; esac
case "$cmd" in up|down) ;; *) echo "usage: instance.sh up|down NAME PORT" >&2; exit 2;; esac
data="$INST_ROOT/$name"
proj="mx-test-$name"
compose_file="$data/docker-compose.yml"
if [ "$cmd" = down ]; then
  if [ -f "$compose_file" ]; then
    docker compose -p "$proj" -f "$compose_file" down -v
  else
    docker compose -p "$proj" down -v 2>/dev/null || true
  fi
  case "$data" in "$INST_ROOT"/*) rm -rf "$data";; *) echo "refusing rm: $data" >&2; exit 2;; esac
  echo "down $name"
  exit 0
fi
case "$port" in *[!0-9]*|'') echo "bad PORT: $port" >&2; exit 2;; esac
if [ "$port" -lt 18010 ] || [ "$port" -gt 18996 ]; then echo "PORT out of range 18010-18996: $port" >&2; exit 2; fi
for p in "$port" $((port+1)) $((port+2)) $((port+3)); do
  if lsof -nP -iTCP:"$p" -sTCP:LISTEN >/dev/null 2>&1; then echo "port $p already in use" >&2; exit 1; fi
done
mkdir -p "$data/database" "$data/logs" "$data/secrets" "$data/certificates"
chmod 700 "$data" "$data/database" "$data/logs" "$data/secrets" "$data/certificates"
cat > "$compose_file" <<EOF
services:
  aliasvault:
    image: $IMAGE
    restart: "no"
    environment:
      TZ: "UTC"
      FORCE_HTTPS_REDIRECT: "false"
      PUBLIC_REGISTRATION_ENABLED: "true"
    ports:
      - "127.0.0.1:$port:80"
      - "127.0.0.1:$((port+1)):443"
      - "127.0.0.1:$((port+2)):25"
      - "127.0.0.1:$((port+3)):587"
    volumes:
      - ./database:/database
      - ./logs:/logs
      - ./secrets:/secrets
      - ./certificates:/certificates
EOF
docker compose -p "$proj" -f "$compose_file" up -d
for i in $(seq 1 90); do
  code="$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port/" 2>/dev/null || true)"
  if [ "$code" = 200 ]; then echo "up $name http://127.0.0.1:$port"; exit 0; fi
  sleep 2
done
echo "instance $name did not become ready on port $port" >&2
docker compose -p "$proj" -f "$compose_file" logs --tail 40 >&2 || true
exit 1
