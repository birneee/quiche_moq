//! Nodes connect, negotiate versions and shut down.

use super::*;

/// End-to-end: publisher announces a namespace and streams delimited objects
/// from a pipe; subscriber connects and writes them to a temp file. Run twice
/// — default `\n` framing, and a non-default `,` separator — since the `--in`/`--out`
/// delimiter must control framing identically on both the publisher's input and
/// the subscriber's output writing.
#[rstest]
#[case(b'\n', TEST_LINES)]
#[case(b',', b"hello,world,moq,")]
fn integration_pub_sub_roundtrip(
    #[case] separator: u8,
    #[case] test_data: &[u8],
    // Every draft this codebase implements wire/session support for, including
    // draft-17/18's uni-pair control stream + per-request bidi streams — this is
    // the one integration test that exercises that machinery over real QUIC,
    // not just via moq_wire's byte-level unit tests.
    #[values("07", "08", "09", "10", "11", "12", "13", "14", "15", "16", "17", "18", "19")]
    version: &str,
) {
    let version: MoqVersionArg = version.parse().unwrap();
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "integration--pubsub".parse().unwrap();

    let pub_args = Args {
        listen: Some(port.into()),
        setup_version: Some(version),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r, &track, separator)],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        setup_version: Some(version),
        subscribe: vec![sub_arg(&track)],
        out: vec![OutArg { track: track.clone(), file: Some(out_path.clone()), output_kind: crate::file_output::OutputKind::lines(separator) }],
        ..Default::default()
    };

    // Start publisher first, the subscriber connects to it.
    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);

    // Start subscriber; give it time to connect and send SUBSCRIBE before data arrives.
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

    // Write data into the pipe now that the subscriber is already subscribed.
    // NextGroupStart delivers objects in the group that starts after subscription,
    // so writing after subscribe ensures the subscriber receives everything.
    write_and_close(pub_w, test_data);

    // Wait for delivery instead of a fixed sleep — under full-suite
    // parallelism a blind 600ms sleep can be too short.
    let output = wait_for_file_len(&out_path, test_data.len(), Duration::from_secs(5));

    // Shut both nodes down cleanly.
    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    assert_eq!(output, test_data);
}

/// A server offering a broad `supported_versions` (the default — i.e. NOT
/// pinned via `--setup-version`) must be able to accept a client pinned to any
/// draft < 15 this codebase supports, even though the server's own
/// `Config::setup_version` guess (draft-16) won't match what most clients
/// actually send. This is the scenario `moq_web`/nixnet exercises (an unpinned
/// native server, a browser client pinned via `?draft=`), and where two real
/// bugs were found — both hung forever, since the server never even recognized
/// the client's opening message:
/// - draft 17/18: the client's uni-pair control stream vs. the server's
///   Bidi-shaped guess — `handle()`'s server-side control-channel setup used to
///   decide the topology from its own guess instead of observing what arrived.
/// - draft 07–10: these use a variable-length SETUP header `Length` field where
///   11+ uses a fixed `u16` — `next_message_on_stream`'s version guess (used to
///   decode the very first message, before the real version is known) didn't
///   account for this, so it misparsed the header. Fixed by peeking the
///   version-independent Type field first (see `bootstrap_version_guess`).
///
/// draft >= 15 negotiates via ALPN, not CLIENT_SETUP's version list (see
/// `VersionNegotiation`), so an unpinned server can't accept an arbitrary
/// new-draft client. For those cases the server is pinned to match instead,
/// simulating what ALPN would already have resolved before SETUP.
#[rstest]
#[case("07")]
#[case("08")]
#[case("09")]
#[case("10")]
#[case("11")]
#[case("12")]
#[case("13")]
#[case("14")]
#[case("15")]
#[case("16")]
#[case("17")]
#[case("18")]
#[case("19")]
fn server_unpinned_accepts_client_pinned_to_any_draft(#[case] client_version: &str) {
    let client_version: MoqVersionArg = client_version.parse().unwrap();
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "integration--pubsub-unpinned-server".parse().unwrap();

    let pub_args = Args {
        listen: Some(port.into()),
        // Server: no --setup-version below draft 15, matching the
        // nixnet/moq_web test setup — but draft >= 15 has no unpinned mode
        // to test (see doc comment above), so pin the server there too.
        setup_version: (client_version.0 >= moq_wire::MOQ_VERSION_DRAFT_15).then_some(client_version),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        setup_version: Some(client_version),
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

    write_and_close(pub_w, TEST_LINES);

    std::thread::sleep(Duration::from_millis(1200));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    let output = std::fs::read(&out_path).unwrap();
    assert_eq!(output, TEST_LINES);
}

/// A subscriber that receives a PUBLISH_NAMESPACE/ANNOUNCE from its peer (e.g.
/// the publisher side always announcing via `--pub`, exactly what the moq_web
/// nixnet setup does) must not crash on receipt, across every draft this
/// codebase supports. Draft 07–10 is the interesting case — ANNOUNCE has no
/// Request ID at all there (it predates the concept) — `dispatch_received_message`
/// used to unconditionally unwrap it; found via manual moq_web testing pinned to
/// draft07. Doesn't exercise accept/reject (moq_web's subscribe-only client does
/// neither); see `relay_forwards_publisher_to_subscriber` for accept-side
/// coverage (draft16+ only — draft 07–13's ANNOUNCE_ERROR shape differs and
/// isn't implemented, see `reject_namespace_publish`).
#[rstest]
#[case("07")]
#[case("08")]
#[case("09")]
#[case("10")]
#[case("11")]
#[case("12")]
#[case("13")]
#[case("14")]
#[case("15")]
#[case("16")]
#[case("17")]
#[case("18")]
#[case("19")]
fn subscriber_receives_namespace_announcement_without_crashing(#[case] version: &str) {
    let version: MoqVersionArg = version.parse().unwrap();
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "integrationannounce--line".parse().unwrap();

    let pub_args = Args {
        listen: Some(port.into()),
        setup_version: Some(version),
        publish: vec!["integrationannounce".parse().unwrap(), pub_passive_arg(&track)],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        setup_version: Some(version),
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
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
}

/// When a node shuts down (SIGINT, or here its `StopHandle` dropped — both
/// funnel through the same graceful-shutdown code path in `post_handle_recvs`),
/// it must send a CLOSE_WEBTRANSPORT_SESSION capsule + CONNECTION_CLOSE before
/// exiting, not just stop responding. We verify this indirectly: per
/// `quiche_mio_runner`'s run loop, a client-role node auto-exits once its
/// connection count drops to zero. So if the listener's shutdown is graceful,
/// the connector (a client) should notice the close and exit within well under
/// its (intentionally large) idle timeout — not need to wait the full timeout.
#[test]
fn graceful_shutdown_closes_peer_promptly() {
    let port = free_udp_port();

    // Intentionally large idle timeout: a graceful close must not rely on it.
    let common = Args { timeout: 30_000, ..Default::default() };
    let listen_args = Args { listen: Some(port.into()), ..common.clone() };
    let connect_args = Args { connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()], ..common };

    let (listen_thread, listen_stop) = node::run_stoppable(listen_args);
    let (connect_thread, connect_stop) = node::run_stoppable(connect_args);
    std::thread::sleep(Duration::from_millis(200)); // allow the session to establish

    // Trigger the listener's graceful shutdown; it should close its session
    // with the connector before its own thread exits.
    drop(listen_stop);

    let start = std::time::Instant::now();
    while !connect_thread.is_finished() && start.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let elapsed = start.elapsed();
    assert!(connect_thread.is_finished(), "connector did not exit after listener's graceful shutdown");
    assert!(elapsed < Duration::from_secs(2), "connector took {elapsed:?} to notice the close — too close to the idle timeout to be a real close frame");

    connect_thread.join().unwrap();
    drop(connect_stop);
    listen_thread.join().unwrap();
}
