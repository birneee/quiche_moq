# subscribe-before-announce.sh - MoQT-18 SS5.1/S6.2/S10.7/S10.6/S10.15/S10.5:
# subscriber connects and subscribes first; publisher connects/announces
# 500ms later. Either an eventual SUBSCRIBE_OK or a REQUEST_ERROR is a pass
# (the relay is allowed to not buffer pending subscriptions). Test
# namespace: moq-test/interop, test track: test-track. Timeout: 3.5s.
# See moq-interop-runner's docs/tests/TEST-CASES.md.
#
# Run via: bash interop/tests/subscribe-before-announce.sh <test-number>

num="${1:-1}"
qd_sub="$WORKDIR/6-sub"
qd_pub="$WORKDIR/6-pub"
start=$(now_ms)

sub_pid=$(start_moq "$qd_sub" --sub "$NS--$TRACK")
sleep 0.5
pub_pid=$(start_moq "$qd_pub" --pub "$NS" --pub-passive "$NS--$TRACK")
wait_stderr "$qd_sub/stderr.log" 2800 '(accepted subscribe|subscribe rejected)'
stop_moq "$sub_pid"
stop_moq "$pub_pid"

sf=$(first_qlog "$qd_sub")
pf=$(first_qlog "$qd_pub")
YAML_LINES=()
[ -n "$sf" ] && YAML_LINES+=("subscriber_connection_id: $(conn_id_from_qlog "$sf")")
[ -n "$pf" ] && YAML_LINES+=("publisher_connection_id: $(conn_id_from_qlog "$pf")")
if [ -n "$sf" ] && grep -qE "$PAT_SUB_OK_OR_ERR" "$sf" 2>/dev/null; then
  outcome="SUBSCRIBE_OK"
  grep -qE "$PAT_REQUEST_ERROR" "$sf" 2>/dev/null && outcome="REQUEST_ERROR"
  YAML_LINES+=("received: $outcome")
  print_result "$num" "subscribe-before-announce" PASS "$(( $(now_ms) - start ))"
  exit 0
else
  YAML_LINES+=('expected: SUBSCRIBE_OK or REQUEST_ERROR' 'received: timeout' 'message: "no response to subscriber SUBSCRIBE"')
  print_result "$num" "subscribe-before-announce" FAIL "$(( $(now_ms) - start ))"
  exit 1
fi
