# subscribe-before-announce - MoQT-18 §5.1, §6.2, §10.7, §10.6, §10.15, §10.5: a subscriber subscribes, a publisher
# announces 500ms later. SUBSCRIBE_OK or REQUEST_ERROR passes, a relay needn't keep pending subscriptions.
# Timeout: 3.5s.
sub=$(start_subscriber)
sleep 0.5
pub=$(start_publisher)
wait_stderr "$(qlog_dir subscriber)" 2800 '(accepted subscribe|subscribe rejected)'
stop_moq "$sub"
stop_moq "$pub"
logged "$PAT_REQUEST_ERROR" "$(qlog_dir subscriber)" && result 0 'received: REQUEST_ERROR'
logged "$PAT_SUBSCRIBE_OK" "$(qlog_dir subscriber)" && result 0 'received: SUBSCRIBE_OK'
result 1 'expected: SUBSCRIBE_OK or REQUEST_ERROR' 'received: timeout' 'message: "no response to subscriber SUBSCRIBE"'
