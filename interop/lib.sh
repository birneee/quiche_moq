# lib.sh - helpers of interop/tests/*.sh, sourced by interop-client.sh, which runs each test in a subshell
# with TEST_NAME, TEST_NUM and TEST_START set.
#
# A test runs `moq` processes and judges them by their qlog, which is written only when a process exits;
# `moq`'s stderr is live, so log lines of moq_cli/src/node/ end waits early ("connected to", "accepted
# subscribe", "subscribe rejected", "withdrew namespace").

now_ms() { date +%s%3N; }

# qlog events are JSON lines with "name" before "data.message.type", the wire messages' names (`qlog_type_name`)
# SERVER_SETUP until draft 16, SETUP from draft 17
PAT_SERVER_SETUP='"name":"moqt:control_message_parsed".*"type":"(server_setup|setup)"'
# PUBLISH_NAMESPACE_OK is a REQUEST_OK from draft 15
PAT_PUBLISH_OK='"name":"moqt:control_message_parsed".*"type":"(publish_ok|request_ok)"'
PAT_PUBLISH_NS_DONE='"name":"moqt:control_message_created".*"type":"publish_namespace_done"'
PAT_REQUEST_ERROR='"name":"moqt:control_message_parsed".*"type":"request_error"'
PAT_SUBSCRIBE_OK='"name":"moqt:control_message_parsed".*"type":"subscribe_ok"'

# moq subscribes only where the namespace was announced, or on an #upstream connection: relays needn't announce
UPSTREAM_URL="$RELAY_URL#upstream"

# qlog_dir [ROLE]: where one `moq` process of the test logs, ROLE naming it among several
qlog_dir() { echo "$WORKDIR/$TEST_NAME/${1:-}"; }

first_qlog() { ls "$1"/client-*.sqlog 2>/dev/null | head -n1; }

# logged PATTERN QLOGDIR: whether the qlog in QLOGDIR has PATTERN
logged() {
  local f; f=$(first_qlog "$2")
  [ -n "$f" ] && grep -qE "$1" "$f"
}

# start_moq QLOGDIR MOQ_ARGS... - prints the pid; connects to CONNECT_URL, default RELAY_URL
start_moq() {
  local qlogdir="$1"; shift
  mkdir -p "$qlogdir"
  # info at least: the lines that end waits early
  local level=info
  [ "$VERBOSE" = 1 ] && level=debug
  QLOGDIR="$qlogdir" RUST_LOG="${CLIENT_RUST_LOG:-$level}" moq --connect "${CONNECT_URL:-$RELAY_URL}" --setup-version 18 "$@" \
    > "$qlogdir/stderr.log" 2>&1 &
  echo $!
}

# stop_moq PID - SIGTERM closes the session and writes the qlog
stop_moq() {
  local pid="$1" i=0
  kill -TERM "$pid" 2>/dev/null
  while kill -0 "$pid" 2>/dev/null && [ $i -lt 20 ]; do sleep 0.1; i=$((i+1)); done
  kill -KILL "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
  return 0
}

# start_publisher: announces NS and serves TRACK, which announcing alone does not
start_publisher() { start_moq "$(qlog_dir publisher)" --pub "$NS" --pub "$NS--$TRACK#passive"; }

start_subscriber() { CONNECT_URL="$UPSTREAM_URL" start_moq "$(qlog_dir subscriber)" --sub "$NS--$TRACK"; }

# wait_stderr QLOGDIR MAX_WAIT_MS [ERE] - until the process logging to QLOGDIR logs ERE, or the time is up
wait_stderr() {
  local file="$1/stderr.log" deadline=$(( $(now_ms) + $2 )) pattern="${3:-}"
  while [ "$(now_ms)" -lt "$deadline" ]; do
    [ -n "$pattern" ] && grep -qE "$pattern" "$file" 2>/dev/null && return 0
    sleep 0.05
  done
  return 1
}

# run_single QLOGDIR MAX_WAIT_MS EARLY_ERE QLOG_ERE MOQ_ARGS... - one `moq` process, passes if its qlog has QLOG_ERE
run_single() {
  local qlogdir="$1" max_wait_ms="$2" early="$3" pattern="$4"; shift 4
  local pid; pid=$(start_moq "$qlogdir" "$@")
  wait_stderr "$qlogdir" "$max_wait_ms" "$early"
  stop_moq "$pid"
  logged "$pattern" "$qlogdir"
}

# result RC [YAML_LINES...] - prints the test's TAP result with the connections of its `moq` processes and
# these lines, exits with RC
result() {
  local rc="$1"; shift
  local dir f role
  local -a lines=("duration_ms: $(( $(now_ms) - TEST_START ))")
  for dir in "$WORKDIR/$TEST_NAME" "$WORKDIR/$TEST_NAME"/*/; do
    f=$(first_qlog "$dir")
    [ -n "$f" ] || continue
    role=$(basename "$dir")
    [ "$dir" = "$WORKDIR/$TEST_NAME" ] && role=
    # qlog files are named client-<scid>.sqlog (moq_cli/src/node/setup.rs, `setup_qlog`)
    lines+=("${role:+${role}_}connection_id: $(basename "$f" .sqlog | sed 's/^client-//')")
    # the client's log as TAP comments, for a failure in the runner
    [ "$rc" != 0 ] && [ "$VERBOSE" = 1 ] && sed 's/^/# /' "$dir/stderr.log" >&2
  done
  [ "$rc" = 0 ] && echo "ok $TEST_NUM - $TEST_NAME" || echo "not ok $TEST_NUM - $TEST_NAME"
  echo "  ---"
  printf '  %s\n' "${lines[@]}" "$@"
  echo "  ..."
  exit "$rc"
}
