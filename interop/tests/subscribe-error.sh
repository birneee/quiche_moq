# subscribe-error - MoQT-18 §5.1, §10.7, §10.6: SUBSCRIBE to a namespace nobody published, REQUEST_ERROR passes.
# Timeout: 2s.
CONNECT_URL="$UPSTREAM_URL" run_single "$(qlog_dir)" 2000 '(subscribe rejected|accepted subscribe)' "$PAT_REQUEST_ERROR" --sub "$BAD_NS--$TRACK" && result 0
result 1 'expected: REQUEST_ERROR' 'received: timeout-or-success' 'message: "no REQUEST_ERROR confirmed"'
