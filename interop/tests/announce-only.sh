# announce-only - MoQT-18 §6.2, §10.15, §10.5: PUBLISH_NAMESPACE, then REQUEST_OK. Timeout: 2s.
# no log line tells the REQUEST_OK, the test waits its whole timeout
run_single "$(qlog_dir)" 2000 '' "$PAT_PUBLISH_OK" --pub "$NS" && result 0
result 1 'expected: PUBLISH_NAMESPACE_OK' 'received: timeout' 'message: "no PUBLISH_NAMESPACE_OK confirmed"'
