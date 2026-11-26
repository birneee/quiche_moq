#!/bin/bash
# run-local.sh - the interop test client against our own relay on localhost, without containers.
#   cargo build --release -p moq-cli && bash interop/run-local.sh [-t TEST] [-v]
# The arguments go to interop-client.sh.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PATH="$SCRIPT_DIR/../target/release:$PATH"
PORT="${MOQT_PORT:-4443}"

log=$(mktemp)
relay=
cleanup() {
  [ -n "$relay" ] && kill "$relay" 2>/dev/null && wait "$relay" 2>/dev/null
  rm -f "$log"
}
trap cleanup EXIT

MOQT_PORT="$PORT" bash "$SCRIPT_DIR/entrypoint-relay.sh" > "$log" 2>&1 &
relay=$!
until grep -q 'listening on' "$log"; do
  kill -0 "$relay" 2>/dev/null || { cat "$log" >&2; exit 1; }
  sleep 0.1
done

RELAY_URL="https://localhost:$PORT" bash "$SCRIPT_DIR/interop-client.sh" "$@" || {
  echo "# relay log:" >&2
  tail -n 20 "$log" >&2
  exit 1
}
