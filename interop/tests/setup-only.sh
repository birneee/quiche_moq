# setup-only - MoQT-18 §3.3, §10.3: connect, exchange SETUP, close. Timeout: 2s.
run_single "$(qlog_dir)" 2000 'connected to ' "$PAT_SERVER_SETUP" && result 0
result 1 'expected: SERVER_SETUP' 'received: timeout' 'message: "no SERVER_SETUP confirmed"'
