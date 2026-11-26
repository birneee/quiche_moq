#!/bin/bash
# interop-client.sh - the moq-interop-runner TAP v14 test client (its docs/TEST-CLIENT-INTERFACE.md and
# docs/tests/TEST-CASES.md): runs tests/<test>.sh, each in a subshell with lib.sh, pass or fail by its exit code.
# Pinned to draft 18, the runner's current target: a draft >= 15 is negotiated by ALPN or WebTransport protocol,
# one per connection attempt.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# the test cases' namespaces moq-test/interop and nonexistent/namespace, tuples of two fields, and track test-track,
# in moq's text form: namespace fields separated by -, a - within a field or track name escaped as .2d
NS="moq.2dtest-interop"
TRACK="test.2dtrack"
BAD_NS="nonexistent-namespace"

ALL_TESTS=()
for f in "$SCRIPT_DIR"/tests/*.sh; do ALL_TESTS+=("$(basename "$f" .sh)"); done

RELAY_URL="${RELAY_URL:-https://localhost:4443}"
TESTCASE="${TESTCASE:-}"
VERBOSE="${VERBOSE:-0}"

while [ $# -gt 0 ]; do
  case "$1" in
    -r|--relay) RELAY_URL="$2"; shift 2 ;;
    -t|--test) TESTCASE="$2"; shift 2 ;;
    -l|--list) printf '%s\n' "${ALL_TESTS[@]}"; exit 0 ;;
    -v|--verbose) VERBOSE=1; shift ;;
    # moq does not verify the relay's certificate
    --tls-disable-verify) shift ;;
    *) shift ;;
  esac
done

TESTS=("${ALL_TESTS[@]}")
if [ -n "$TESTCASE" ]; then
  if [ ! -f "$SCRIPT_DIR/tests/$TESTCASE.sh" ]; then
    echo "TAP version 14"
    echo "1..0"
    echo "# unknown test: $TESTCASE" >&2
    exit 127
  fi
  TESTS=("$TESTCASE")
fi

WORKDIR=$(mktemp -d)
trap 'rm -rf "$WORKDIR"' EXIT

# shellcheck source=lib.sh
source "$SCRIPT_DIR/lib.sh"

echo "TAP version 14"
echo "# birneee-moq interop-client.sh (moq_cli wrapper)"
echo "# Relay: $RELAY_URL"
echo "1..${#TESTS[@]}"

FAILED=0
TEST_NUM=0
for TEST_NAME in "${TESTS[@]}"; do
  TEST_NUM=$((TEST_NUM+1))
  ( TEST_START=$(now_ms); source "$SCRIPT_DIR/tests/$TEST_NAME.sh" ) || FAILED=1
done

exit $FAILED
