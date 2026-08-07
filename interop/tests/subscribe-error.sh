# subscribe-error.sh - MoQT-18 SS5.1/S10.7/S10.6: SUBSCRIBE to a
# non-existent namespace/track, expect REQUEST_ERROR (that's the pass
# condition, not a failure). Timeout: 2s.
# See moq-interop-runner's docs/tests/TEST-CASES.md.
#
# Run via: bash interop/tests/subscribe-error.sh <test-number>

num="${1:-1}"
qd="$WORKDIR/4-suberr"
start=$(now_ms)
f=$(run_single "$qd" 2000 '(subscribe rejected|accepted subscribe)' "$PAT_REQUEST_ERROR" --sub "$BAD_NS--$TRACK"); rc=$?
YAML_LINES=()
[ -n "$f" ] && YAML_LINES+=("connection_id: $(conn_id_from_qlog "$f")")
if [ "$rc" -eq 0 ]; then
  print_result "$num" "subscribe-error" PASS "$(( $(now_ms) - start ))"
  exit 0
else
  YAML_LINES+=('expected: REQUEST_ERROR' 'received: timeout-or-success' 'message: "no REQUEST_ERROR confirmed"')
  print_result "$num" "subscribe-error" FAIL "$(( $(now_ms) - start ))"
  exit 1
fi
