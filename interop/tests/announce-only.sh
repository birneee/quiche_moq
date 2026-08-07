# announce-only.sh - MoQT-18 SS6.2/S10.15/S10.5: PUBLISH_NAMESPACE then
# REQUEST_OK. Test namespace: moq-test/interop. Timeout: 2s.
# See moq-interop-runner's docs/tests/TEST-CASES.md.
#
# Run via: bash interop/tests/announce-only.sh <test-number>

num="${1:-1}"
qd="$WORKDIR/2-announce"
start=$(now_ms)
f=$(run_single "$qd" 2000 '' "$PAT_PUBLISH_OK" --pub "$NS"); rc=$?
YAML_LINES=()
[ -n "$f" ] && YAML_LINES+=("connection_id: $(conn_id_from_qlog "$f")")
if [ "$rc" -eq 0 ]; then
  print_result "$num" "announce-only" PASS "$(( $(now_ms) - start ))"
  exit 0
else
  YAML_LINES+=('expected: PUBLISH_NAMESPACE_OK' 'received: timeout' 'message: "no PUBLISH_NAMESPACE_OK confirmed"')
  print_result "$num" "announce-only" FAIL "$(( $(now_ms) - start ))"
  exit 1
fi
