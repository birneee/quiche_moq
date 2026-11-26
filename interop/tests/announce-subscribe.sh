# announce-subscribe - MoQT-18 §5.1, §6.2, §10.7-8, §10.15, §10.5: a publisher announces and serves, then a
# subscriber gets SUBSCRIBE_OK. Timeout: 3s.
pub=$(start_publisher)
# for the announcement to land first
sleep 0.4
sub=$(start_subscriber)
wait_stderr "$(qlog_dir subscriber)" 2400 '(accepted subscribe|subscribe rejected)'
stop_moq "$sub"
stop_moq "$pub"
logged "$PAT_PUBLISH_OK" "$(qlog_dir publisher)" && logged "$PAT_SUBSCRIBE_OK" "$(qlog_dir subscriber)" && result 0
result 1 'expected: SUBSCRIBE_OK' 'received: timeout-or-error' 'message: "publisher announce or subscriber subscribe not confirmed ok in qlog"'
