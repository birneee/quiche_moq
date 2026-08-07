# lib.sh - shared helpers for interop/tests/*.sh, sourced by interop-client.sh.
#
# IMPORTANT: qlog output is only flushed to disk when a connection closes
# (verified empirically - polling a live process's qlog file never sees
# anything until it's terminated). So qlog is read only *after* a process
# is stopped, and is the authoritative pass/fail source; `moq`'s stderr
# logging (which *is* live/unbuffered) is used only to shorten the wait via
# early-exit where a suitable log line already exists (see node.rs: "conn N
# connected to ... vNN", "accepted subscribe ...", "subscribe rejected
# ...: ...", "withdrew namespace ... (SIGUSR1)"). There's no live signal
# for the client's own outbound PUBLISH_NAMESPACE getting REQUEST_OK'd, so
# those tests just wait out their full timeout before checking qlog.
#
# Expects RELAY_URL, VERBOSE, WORKDIR to already be set by the caller, and
# uses/mutates the caller's YAML_LINES and FAILED variables.

now_ms() { date +%s%3N; }

# Each qlog event is a self-contained JSON-Lines line with "name" always
# preceding "data.message.type", so a single grep -E per line is enough; no
# JSON parser needed. Response type names are relabeled by control_channel.rs
# per request kind (not the generic wire "request_ok") - verified empirically
# against a local relay/client pair, both at draft-14 (default) and
# draft-18 (--setup-version 18): namespace-publish acks are "publish_ok",
# subscribe acks are "subscribe_ok", any rejection is "request_error".
PAT_SERVER_SETUP='"name":"moqt:control_message_parsed".*"type":"server_setup"'
PAT_PUBLISH_OK='"name":"moqt:control_message_parsed".*"type":"publish_ok"'
PAT_PUBLISH_NS_DONE='"name":"moqt:control_message_created".*"type":"publish_namespace_done"'
PAT_REQUEST_ERROR='"name":"moqt:control_message_parsed".*"type":"request_error"'
PAT_SUBSCRIBE_OK='"name":"moqt:control_message_parsed".*"type":"subscribe_ok"'
PAT_SUB_OK_OR_ERR='"name":"moqt:control_message_parsed".*"type":"(subscribe_ok|request_error)"'

conn_id_from_qlog() {
  # qlog files are named <role>-<scid-hex>.sqlog (moq_cli/src/node.rs, setup_qlog()).
  basename "$1" 2>/dev/null | sed -E 's/^client-([0-9a-f]+)\.sqlog$/\1/'
}

first_qlog() {
  ls "$1"/client-*.sqlog 2>/dev/null | head -n1
}

start_moq() {
  # start_moq <qlogdir> <extra moq args...> -> prints pid on stdout
  local qlogdir="$1"; shift
  mkdir -p "$qlogdir"
  # info is required, not just for verbosity: the early-exit stderr
  # patterns (e.g. "connected to ...", "accepted subscribe ...") are
  # logged at info level in node.rs, and are the only live (non-qlog)
  # signal available for those tests - see file header.
  local level="info"
  [ "$VERBOSE" = "1" ] && level="debug"
  QLOGDIR="$qlogdir" RUST_LOG="$level" moq --connect "$RELAY_URL" --setup-version 18 "$@" \
    > "$qlogdir/stderr.log" 2>&1 &
  echo $!
}

stop_moq() {
  # SIGTERM triggers moq's own graceful close path (session close + QUIC
  # close) before exit, which is also what flushes the qlog writer - see
  # moq_cli/src/node.rs shutdown handling.
  local pid="$1"
  kill -TERM "$pid" 2>/dev/null
  local i=0
  while kill -0 "$pid" 2>/dev/null && [ $i -lt 20 ]; do sleep 0.1; i=$((i+1)); done
  kill -KILL "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
  return 0
}

# wait_stderr <file> <max_wait_ms> <ere_pattern|empty> - polls (or just
# sleeps, if pattern is empty) up to max_wait_ms for an early-exit signal.
# Always returns once the wait is over; the caller still needs to stop the
# process and check qlog for the authoritative result. Takes milliseconds
# (not seconds) because bash's $(( )) has no floating point support.
wait_stderr() {
  local file="$1" max_wait_ms="$2" pattern="$3"
  local deadline=$(( $(now_ms) + max_wait_ms ))
  while [ "$(now_ms)" -lt "$deadline" ]; do
    if [ -n "$pattern" ] && grep -qE "$pattern" "$file" 2>/dev/null; then
      return 0
    fi
    sleep 0.05
  done
  return 1
}

# run_single <qlogdir> <max_wait_ms> <early_stderr_pattern|empty> <qlog_pass_pattern> <extra moq args...>
# Starts one `moq --connect` process, waits (with optional early exit on a
# live stderr signal), stops it (flushing qlog), then checks the flushed
# qlog for <qlog_pass_pattern>. Prints the qlog file path (may be empty) to
# stdout regardless of outcome; returns 0 on pass, 1 on fail.
run_single() {
  local qlogdir="$1" max_wait_ms="$2" early_pattern="$3" qlog_pattern="$4"; shift 4
  local pid; pid=$(start_moq "$qlogdir" "$@")
  wait_stderr "$qlogdir/stderr.log" "$max_wait_ms" "$early_pattern"
  stop_moq "$pid"
  local f; f=$(first_qlog "$qlogdir")
  echo "$f"
  [ -n "$f" ] && grep -qE "$qlog_pattern" "$f" 2>/dev/null && return 0
  return 1
}

print_result() {
  # print_result <num> <name> <PASS|FAIL|SKIP> <duration_ms>
  # Reads diagnostic lines from the YAML_LINES array (caller-populated),
  # then clears it. Pass/fail is communicated to the driver via this
  # script's own exit code, not a shared variable - each test in
  # interop/tests/ runs as its own `bash` process.
  local num="$1" name="$2" result="$3" duration_ms="$4"
  case "$result" in
    PASS) echo "ok $num - $name" ;;
    FAIL) echo "not ok $num - $name" ;;
    SKIP) echo "ok $num - $name # SKIP not implemented"; YAML_LINES=(); return ;;
  esac
  if [ -n "$duration_ms" ] || [ "${#YAML_LINES[@]}" -gt 0 ]; then
    echo "  ---"
    [ -n "$duration_ms" ] && echo "  duration_ms: $duration_ms"
    local line
    for line in "${YAML_LINES[@]}"; do echo "  $line"; done
    echo "  ..."
  fi
  YAML_LINES=()
}
