# publish-namespace-done - MoQT-18 §6.2, §10.15, §10.5, §3.3.2: PUBLISH_NAMESPACE, REQUEST_OK, withdraw by
# cancelling its request stream (PUBLISH_NAMESPACE_DONE before draft 17). Timeout: 2s.
qd=$(qlog_dir)
pid=$(start_moq "$qd" --pub "$NS" --sigusr1 unpub --unpub "$NS")
# no log line tells the REQUEST_OK; the qlog verifies it below
sleep 0.3
kill -USR1 "$pid" 2>/dev/null
wait_stderr "$qd" 1500 'withdrew namespace'
stop_moq "$pid"
# from draft 17 withdrawing cancels the request stream, no message qlog shows: moq's own log tells it withdrew
logged "$PAT_PUBLISH_OK" "$qd" && { logged "$PAT_PUBLISH_NS_DONE" "$qd" || grep -q 'withdrew namespace' "$qd/stderr.log"; } && result 0
result 1 'expected: PUBLISH_NAMESPACE_OK then PUBLISH_NAMESPACE_DONE' 'received: incomplete' 'message: "namespace publish/withdrawal not confirmed in qlog"'
