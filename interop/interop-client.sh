#!/bin/bash
# interop-client.sh - moq-interop-runner TAP v14 test client wrapper around
# `moq` (moq_cli). Parses the runner's CLI/env interface, then for each
# requested test case runs interop/tests/<test-id>.sh as its own process
# (exit code 0/1 = pass/fail), passing shared helpers/state through
# exported functions and environment variables. Each test script starts
# one or more `moq` subprocesses and judges pass/fail from their qlog
# output - see interop/tests/lib.sh for why (qlog only flushes on close,
# so `moq`'s live stderr logging is used for early-exit, qlog is the
# authoritative source).
#
# Pinned to draft-18 (--setup-version 18) throughout: moq_core's
# VersionNegotiation can't multi-candidate-negotiate among drafts >= 15 in a
# single connection attempt (see moq_core/src/config.rs), so a single `moq`
# invocation targets exactly one draft. draft-18 is moq-interop-runner's
# current_target.
#
# See docs/TEST-CLIENT-INTERFACE.md and docs/tests/TEST-CASES.md in
# moq-interop-runner for the interface contract and test procedures this
# implements.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

export NS="moq-test/interop"
export TRACK="test-track"
export BAD_NS="nonexistent/namespace"

ALL_TESTS=(setup-only announce-only publish-namespace-done subscribe-error announce-subscribe subscribe-before-announce)

RELAY_URL="${RELAY_URL:-https://localhost:4443}"
TESTCASE="${TESTCASE:-}"
VERBOSE="${VERBOSE:-0}"
LIST=0

while [ $# -gt 0 ]; do
  case "$1" in
    -r|--relay) RELAY_URL="$2"; shift 2 ;;
    -t|--test) TESTCASE="$2"; shift 2 ;;
    -l|--list) LIST=1; shift ;;
    -v|--verbose) VERBOSE=1; shift ;;
    # TLS verification is unconditionally disabled in moq's outbound
    # connect path today (cfg.verify_peer(false)), so there's nothing to
    # toggle - accepted only so callers passing this flag don't error.
    --tls-disable-verify) shift ;;
    *) shift ;;
  esac
done
export RELAY_URL VERBOSE

if [ "$LIST" = "1" ]; then
  printf '%s\n' "${ALL_TESTS[@]}"
  exit 0
fi

if [ -n "$TESTCASE" ]; then
  known=0
  for t in "${ALL_TESTS[@]}"; do [ "$t" = "$TESTCASE" ] && known=1; done
  if [ "$known" != "1" ]; then
    echo "TAP version 14"
    echo "1..0"
    echo "# unknown test: $TESTCASE" >&2
    exit 127
  fi
  TESTS=("$TESTCASE")
else
  TESTS=("${ALL_TESTS[@]}")
fi

export WORKDIR
WORKDIR=$(mktemp -d)
trap 'rm -rf "$WORKDIR"' EXIT

# shellcheck source=tests/lib.sh
source "$SCRIPT_DIR/tests/lib.sh"
export -f now_ms conn_id_from_qlog first_qlog start_moq stop_moq wait_stderr run_single print_result
export PAT_SERVER_SETUP PAT_PUBLISH_OK PAT_PUBLISH_NS_DONE PAT_REQUEST_ERROR PAT_SUBSCRIBE_OK PAT_SUB_OK_OR_ERR

echo "TAP version 14"
echo "# birneee-moq interop-client.sh (moq_cli wrapper)"
echo "# Relay: $RELAY_URL"
echo "1..${#TESTS[@]}"

FAILED=0
i=0
for t in "${TESTS[@]}"; do
  i=$((i+1))
  bash "$SCRIPT_DIR/tests/$t.sh" "$i" || FAILED=1
done

exit $FAILED
