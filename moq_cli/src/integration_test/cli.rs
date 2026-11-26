//! CLI flags, compared with moq_utils pub/sub.

use super::*;

/// With --keepalive shorter than the idle timeout and no application data flowing,
/// the connector keeps pinging the listener so neither side notices an idle timeout.
/// A client-role node auto-exits once its connection count hits zero (see
/// `graceful_shutdown_closes_peer_promptly`), so a still-running connector thread
/// after waiting past the idle timeout is proof the connection survived.
#[test]
fn keepalive_pings_keep_idle_connection_open() {
    let port = free_udp_port();

    // Short idle timeout so a dead connection would be noticed quickly.
    let listen_args = Args { listen: Some(port.into()), timeout: 600, ..Default::default() };
    let connect_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        timeout: 600,
        keepalive: 100, // well under the idle timeout
        keepalive_no_pto_limit: true, // exact interval, no PTO-floor uncertainty
        ..Default::default()
    };

    let (listen_thread, listen_stop) = node::run_stoppable(listen_args);
    let (connect_thread, connect_stop) = node::run_stoppable(connect_args);
    std::thread::sleep(Duration::from_millis(200)); // let the session establish

    // Wait well past the idle timeout with no application data flowing.
    std::thread::sleep(Duration::from_millis(1000));

    assert!(!connect_thread.is_finished(), "connector exited — keepalive did not prevent the idle timeout from firing");

    drop(connect_stop);
    connect_thread.join().unwrap();
    drop(listen_stop);
    listen_thread.join().unwrap();
}

#[test]
fn keylog_file_written_when_sslkeylogfile_set() {
    let port = free_udp_port();
    let keylog_file = tempfile::NamedTempFile::new().unwrap();

    let listen_args = Args { listen: Some(port.into()), ..Default::default() };
    let connect_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        // Only the outbound (--connect) side is wired up; --listen-accepted
        // connections don't apply the keylog hook (see ssl_key_log_file's doc
        // comment in args/mod.rs).
        ssl_key_log_file: Some(keylog_file.path().to_path_buf()),
        ..Default::default()
    };

    let (listen_thread, listen_stop) = node::run_stoppable(listen_args);
    let (connect_thread, connect_stop) = node::run_stoppable(connect_args);
    std::thread::sleep(Duration::from_millis(300)); // let the handshake complete

    drop(connect_stop);
    connect_thread.join().unwrap();
    drop(listen_stop);
    listen_thread.join().unwrap();

    let contents = std::fs::read_to_string(keylog_file.path()).unwrap();
    assert!(
        contents.contains("CLIENT_HANDSHAKE_TRAFFIC_SECRET"),
        "keylog file missing CLIENT_HANDSHAKE_TRAFFIC_SECRET, got: {contents:?}"
    );
}

/// Runs a publisher/subscriber pair, with `listen` and `connect` setting
/// their version options, and returns whatever the subscriber wrote out —
/// empty if the SETUP handshake never completed. Panics if a node crashed.
fn run_version_case(listen: impl FnOnce(&mut Args), connect: impl FnOnce(&mut Args)) -> Vec<u8> {
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "version--line".parse().unwrap();

    let mut listen_args = Args {
        listen: Some(port.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    listen(&mut listen_args);
    let mut connect_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };
    connect(&mut connect_args);

    let (listen_thread, listen_stop) = node::run_stoppable(listen_args);
    let (connect_thread, connect_stop) = node::run_stoppable(connect_args);
    std::thread::sleep(Duration::from_millis(200));

    write_and_close(pub_w, TEST_LINES);
    std::thread::sleep(Duration::from_millis(600));

    drop(connect_stop);
    connect_thread.join().unwrap();
    drop(listen_stop);
    listen_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    std::fs::read(&out_path).unwrap_or_default()
}

fn setup_version(v: &str) -> impl FnOnce(&mut Args) + use<'_> {
    move |a| a.setup_version = Some(v.parse().unwrap())
}

fn supported_versions<'a>(vs: &'a [&str]) -> impl FnOnce(&mut Args) + use<'a> {
    move |a| a.supported_versions = Some(vs.iter().map(|v| v.parse().unwrap()).collect())
}

/// `--setup-version` must restrict the *offered/required* version exclusively,
/// not just add it to the default range — otherwise a broken implementation
/// (e.g. only setting `setup_version` but leaving `supported_versions` at its
/// default full range) would silently pass. Verified two ways:
/// 1. Connector pinned to draft07, listener pinned to draft16: the SETUP
///    handshake must fail and no objects are delivered.
/// 2. Both ends pinned to the same explicit draft07 — must still complete
///    a normal pub/sub exchange.
#[test]
fn setup_version_negotiates_requested_draft() {
    // Mismatched, non-overlapping versions: SETUP must fail, nothing delivered.
    // (Not `None` for the listener: that means "unpinned", i.e. the *default*
    // supported-versions range, which has always included draft07 — so it
    // would actually overlap the connector's sole draft07 offer and negotiate
    // successfully, not what this case is testing.)
    let mismatched = run_version_case(setup_version("draft16"), setup_version("draft07"));
    assert!(mismatched.is_empty(), "objects delivered despite a SETUP version mismatch: {mismatched:?}");

    // Both ends pinned to the same explicit (non-default) draft: must still work.
    let matched = run_version_case(setup_version("draft13"), setup_version("draft13"));
    assert_eq!(matched, TEST_LINES);
}

/// `--supported-versions` provides real negotiation (unlike `--setup-version`,
/// which hard-pins to exactly one): two sides whose *preferred* versions
/// differ can still complete a handshake as long as their supported lists
/// overlap, settling on the highest-priority version present in both.
///
/// Restricted to draft < 15: draft >= 15 negotiates via ALPN, not a version
/// list (see `VersionNegotiation`), so a multi-entry offer including one
/// can't be conveyed at all — `VersionNegotiation::validate` rejects it.
#[test]
fn supported_versions_allows_overlap_negotiation() {
    // Disjoint supported-version sets: SETUP must fail, nothing delivered.
    let disjoint = run_version_case(supported_versions(&["draft07", "draft08"]), supported_versions(&["draft13", "draft14"]));
    assert!(disjoint.is_empty(), "objects delivered despite disjoint supported-versions sets: {disjoint:?}");

    // Overlapping but non-identical sets: must negotiate the shared version
    // and complete a normal pub/sub exchange.
    let overlapping = run_version_case(supported_versions(&["draft12", "draft13"]), supported_versions(&["draft13", "draft14"]));
    assert_eq!(overlapping, TEST_LINES);
}

/// Saved handshake round trips, 200 ms one-way delay:
/// - server SETTINGS in 0.5-RTT, so CONNECT goes out with the server's first flight, not 1 RTT later (RFC 9114 §7.2.4.2)
/// - pinned draft: CLIENT_SETUP with the CONNECT (draft-ietf-webtrans-http3-16 §4.2, §4.6)
#[test]
fn webtransport_handshake_round_trips() {
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let qlog_dir = tempfile::tempdir().unwrap();
    let track: NamespaceTrackname = "integration--optimistic-setup".parse().unwrap();
    let version: MoqVersionArg = "16".parse().unwrap();

    let pub_args = Args {
        listen: Some(port.into()),
        setup_version: Some(version),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    // long, so that scheduling delays do not blur the round trips
    let rtt_proxy = UdpDelayProxy::start(port, Duration::from_millis(200));
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{}", rtt_proxy.port()).parse().unwrap()],
        setup_version: Some(version),
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        qlog_dir: Some(qlog_dir.path().to_path_buf()),
        ..Default::default()
    };

    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);
    write_and_close(pub_w, TEST_LINES);
    let output = wait_for_file_len(&out_path, TEST_LINES.len(), Duration::from_secs(5));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    assert_eq!(output, TEST_LINES);
    let events = qlog_events(qlog_dir.path());
    let time_of = |pred: &dyn Fn(&serde_json::Value) -> bool| {
        events.iter().find(|e| pred(e)).and_then(|e| e["time"].as_f64()).unwrap()
    };
    // from the server's first flight, so that delays before it do not count
    let first_received = time_of(&|e| e["name"] == "quic:packet_received");
    let connect = time_of(&|e| e["name"] == "http3:frame_created" && e["data"]["frame"]["frame_type"] == "headers");
    let client_setup = time_of(&|e| e["name"] == "moqt:control_message_created" && e["data"]["message"]["type"] == "client_setup");
    // 1 RTT is 400 ms: SETTINGS in 0.5-RTT arrive with the first flight, else 1 RTT after it
    assert!(connect - first_received < 200.0, "CONNECT {:.1} ms after the server's first flight, expected with it", connect - first_received);
    assert!(client_setup - connect < 200.0, "CLIENT_SETUP {:.1} ms after CONNECT, expected same flight", client_setup - connect);
}

/// NAT rebinding with `--no-migration`: the publisher drops the new path, the
/// subscriber's idle timeout closes the connection, `--reconnect` re-dials and
/// resumes right after the last received object.
/// - `two` is published while the path is dead, served from cache after reconnect
/// - keepalive makes the subscriber send on the rebound path, exercising the drop
#[test]
fn reconnect_resumes_after_dropped_nat_rebinding() {
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "integration--reconnect".parse().unwrap();
    let qlog_dir = tempfile::tempdir().unwrap();

    let pub_args = Args {
        listen: Some(port.into()),
        no_migration: true,
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    let nat = UdpDelayProxy::start(port, Duration::ZERO);
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{}#upstream", nat.port()).parse().unwrap()],
        reconnect: true,
        timeout: 300,
        keepalive: 50,
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        qlog_dir: Some(qlog_dir.path().to_path_buf()),
        ..Default::default()
    };

    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

    write_only(pub_w, b"one\n");
    assert_eq!(wait_for_file_len(&out_path, 4, Duration::from_secs(5)), b"one\n");

    nat.rebind();
    write_only(pub_w, b"two\n");
    assert_eq!(wait_for_file_len(&out_path, 8, Duration::from_secs(5)), b"one\ntwo\n");

    write_and_close(pub_w, b"three\n");
    let output = wait_for_file_len(&out_path, 14, Duration::from_secs(5));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    assert_eq!(output, b"one\ntwo\nthree\n");
    assert_eq!(std::fs::read_dir(qlog_dir.path()).unwrap().count(), 2, "expected one reconnect, not a migration");
}
