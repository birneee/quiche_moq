# announce-subscribe.sh - MoQT-18 SS5.1/S6.2/S10.7-8/S10.15/S10.5: two
# concurrent connections, publisher announces + serves, subscriber
# subscribes and gets SUBSCRIBE_OK. Test namespace: moq-test/interop, test
# track: test-track. Timeout: 3s.
# See moq-interop-runner's docs/tests/TEST-CASES.md.
#
# Run via: bash interop/tests/announce-subscribe.sh <test-number>

num="${1:-1}"
qd_pub="$WORKDIR/5-pub"
qd_sub="$WORKDIR/5-sub"
start=$(now_ms)

# Publisher must both announce the namespace (--pub NS) and be willing to
# serve the relay-forwarded SUBSCRIBE it'll receive on this connection
# (--pub-passive NS--TRACK) - announcing alone doesn't serve anything,
# confirmed empirically against a local relay.
pub_pid=$(start_moq "$qd_pub" --pub "$NS" --pub-passive "$NS--$TRACK")
sleep 0.4  # grace for the publisher's announce to land before subscribing
sub_pid=$(start_moq "$qd_sub" --sub "$NS--$TRACK")
wait_stderr "$qd_sub/stderr.log" 2400 '(accepted subscribe|subscribe rejected)'
stop_moq "$sub_pid"
stop_moq "$pub_pid"

pf=$(first_qlog "$qd_pub")
sf=$(first_qlog "$qd_sub")
YAML_LINES=()
[ -n "$pf" ] && YAML_LINES+=("publisher_connection_id: $(conn_id_from_qlog "$pf")")
[ -n "$sf" ] && YAML_LINES+=("subscriber_connection_id: $(conn_id_from_qlog "$sf")")
if [ -n "$pf" ] && [ -n "$sf" ] && grep -qE "$PAT_PUBLISH_OK" "$pf" 2>/dev/null && grep -qE "$PAT_SUBSCRIBE_OK" "$sf" 2>/dev/null; then
  print_result "$num" "announce-subscribe" PASS "$(( $(now_ms) - start ))"
  exit 0
else
  YAML_LINES+=('expected: SUBSCRIBE_OK' 'received: timeout-or-error' 'message: "publisher announce or subscriber subscribe not confirmed ok in qlog"')
  print_result "$num" "announce-subscribe" FAIL "$(( $(now_ms) - start ))"
  exit 1
fi
