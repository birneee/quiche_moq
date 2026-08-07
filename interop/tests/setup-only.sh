# setup-only.sh - MoQT-18 SS3.3/S10.3: connect, exchange SETUP, close.
# Timeout: 2s. See moq-interop-runner's docs/tests/TEST-CASES.md.
#
# Run via: bash interop/tests/setup-only.sh <test-number>
# (lib.sh's functions/vars and WORKDIR/RELAY_URL/VERBOSE/NS/TRACK/BAD_NS
# must already be in the environment - see interop-client.sh.)

num="${1:-1}"
qd="$WORKDIR/1-setup"
start=$(now_ms)
f=$(run_single "$qd" 2000 'connected to ' "$PAT_SERVER_SETUP"); rc=$?
YAML_LINES=()
[ -n "$f" ] && YAML_LINES+=("connection_id: $(conn_id_from_qlog "$f")")
if [ "$rc" -eq 0 ]; then
  print_result "$num" "setup-only" PASS "$(( $(now_ms) - start ))"
  exit 0
else
  YAML_LINES+=('expected: SERVER_SETUP' 'received: timeout' 'message: "no SERVER_SETUP confirmed"')
  print_result "$num" "setup-only" FAIL "$(( $(now_ms) - start ))"
  exit 1
fi
