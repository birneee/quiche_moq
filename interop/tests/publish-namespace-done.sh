# publish-namespace-done.sh - MoQT-18 SS6.2/S10.15/S10.5/S3.3.2:
# PUBLISH_NAMESPACE, REQUEST_OK, withdraw via PUBLISH_NAMESPACE_DONE.
# Test namespace: moq-test/interop. Timeout: 2s.
# See moq-interop-runner's docs/tests/TEST-CASES.md.
#
# Run via: bash interop/tests/publish-namespace-done.sh <test-number>

num="${1:-1}"
qd="$WORKDIR/3-pubdone"
start=$(now_ms)
pid=$(start_moq "$qd" --pub "$NS" --usr1-unpub "$NS")
# No live signal for the initial PUBLISH_NAMESPACE_OK (see lib.sh header);
# 300ms is generous for a same-network round trip, and correctness is
# still verified below via qlog regardless of this grace period's length.
sleep 0.3
kill -USR1 "$pid" 2>/dev/null
wait_stderr "$qd/stderr.log" 1500 'withdrew namespace'
stop_moq "$pid"
f=$(first_qlog "$qd")
YAML_LINES=()
[ -n "$f" ] && YAML_LINES+=("connection_id: $(conn_id_from_qlog "$f")")
if [ -n "$f" ] && grep -qE "$PAT_PUBLISH_OK" "$f" 2>/dev/null && grep -qE "$PAT_PUBLISH_NS_DONE" "$f" 2>/dev/null; then
  print_result "$num" "publish-namespace-done" PASS "$(( $(now_ms) - start ))"
  exit 0
else
  YAML_LINES+=('expected: PUBLISH_NAMESPACE_OK then PUBLISH_NAMESPACE_DONE' 'received: incomplete' 'message: "namespace publish/withdrawal not confirmed in qlog"')
  print_result "$num" "publish-namespace-done" FAIL "$(( $(now_ms) - start ))"
  exit 1
fi
