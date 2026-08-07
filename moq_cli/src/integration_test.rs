//! End-to-end tests that exercise multiple `moq_cli` nodes wired together over
//! real QUIC/WebTransport connections (`node::run_stoppable`). Unit tests for
//! individual components live alongside that component's own source file
//! (e.g. `args.rs`, `fmp4.rs`) instead.

use super::*;
use crate::args::MoqVersionArg;
use crate::node;
use moq_wire::{Namespace, NamespaceTrackname};
use rstest::rstest;
use std::path::Path;
use std::time::Duration;

const TEST_LINES: &[u8] = b"hello\nworld\nmoq\n";

// ── Shared test helpers ───────────────────────────────────────────────────────

/// Finds a free UDP port by binding to one and releasing it immediately.
/// Every node here uses `SO_REUSEADDR`, so the port stays usable across the
/// tiny window between this call returning and the caller's own bind.
fn free_udp_port() -> u16 {
    let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    port
}

/// An OS pipe, used as publisher input so MIO fires READABLE events (a plain
/// in-memory buffer wouldn't wake the event loop).
fn make_pipe() -> (i32, i32) {
    let mut fds = [0i32; 2];
    unsafe { libc::pipe(fds.as_mut_ptr()) };
    (fds[0], fds[1])
}

fn write_and_close(fd: i32, data: &[u8]) {
    unsafe {
        libc::write(fd, data.as_ptr() as *const _, data.len());
        libc::close(fd);
    }
}

/// A `--sub` entry with only `track`/`file` set, the common case.
fn sub_arg(track: &NamespaceTrackname, file: std::path::PathBuf) -> SubArg {
    SubArg { track: TrackPattern::Exact(track.clone()), file: Some(file), output_kind: crate::file_output::OutputKind::auto(b'\n'), priority: None, delivery_timeout: None, filter: None }
}

/// Parses every JSON-SEQ (RFC 7464) record out of every qlog file in `dir`.
/// Each record is an ASCII Record Separator (0x1E) followed by one JSON value.
fn qlog_events(dir: &Path) -> Vec<serde_json::Value> {
    let mut events = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let content = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        for line in content.lines() {
            let line = line.trim_start_matches('\u{1e}');
            if let Ok(value) = serde_json::from_str(line) {
                events.push(value);
            }
        }
    }
    events
}

// ── Integration tests ─────────────────────────────────────────────────────────

/// End-to-end: publisher announces a namespace and streams delimited objects
/// from a pipe; subscriber connects and writes them to a temp file. Run twice
/// — default `\n` framing, and a non-default `,` separator — since `--separator`
/// must control framing identically on both the publisher's input parsing and
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
    #[values("07", "08", "09", "10", "11", "12", "13", "14", "15", "16", "17", "18")]
    version: &str,
) {
    let version: MoqVersionArg = version.parse().unwrap();
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "integration--pubsub".parse().unwrap();

    let pub_args = Args {
        listen: Some(port),
        setup_version: Some(version),
        pub_passive: vec![TrackArg { file: Some(format!("/proc/self/fd/{pub_r}").into()), track: TrackPattern::Exact(track.clone()), delimiter: separator }],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        setup_version: Some(version),
        subscribe: vec![SubArg { track: TrackPattern::Exact(track.clone()), file: Some(out_path.clone()), output_kind: crate::file_output::OutputKind::lines(separator), priority: None, delivery_timeout: None, filter: None }],
        ..Default::default()
    };

    // Start publisher first; give it time to bind and announce the namespace.
    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    std::thread::sleep(Duration::from_millis(150));

    // Start subscriber; give it time to connect and send SUBSCRIBE before data arrives.
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    std::thread::sleep(Duration::from_millis(200));

    // Write data into the pipe now that the subscriber is already subscribed.
    // NextGroupStart delivers objects in the group that starts after subscription,
    // so writing after subscribe ensures the subscriber receives everything.
    write_and_close(pub_w, test_data);

    // Allow enough time for object delivery.
    std::thread::sleep(Duration::from_millis(600));

    // Shut both nodes down cleanly.
    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    let output = std::fs::read(&out_path).unwrap();
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
fn server_unpinned_accepts_client_pinned_to_any_draft(#[case] client_version: &str) {
    let client_version: MoqVersionArg = client_version.parse().unwrap();
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "integration--pubsub-unpinned-server".parse().unwrap();

    let pub_args = Args {
        listen: Some(port),
        // Server: no --setup-version below draft 15, matching the
        // nixnet/moq_web test setup — but draft >= 15 has no unpinned mode
        // to test (see doc comment above), so pin the server there too.
        setup_version: (client_version.0 >= moq_wire::MOQ_VERSION_DRAFT_15).then_some(client_version),
        pub_passive: vec![TrackArg { file: Some(format!("/proc/self/fd/{pub_r}").into()), track: TrackPattern::Exact(track.clone()), delimiter: b'\n' }],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        setup_version: Some(client_version),
        subscribe: vec![sub_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    std::thread::sleep(Duration::from_millis(150));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    std::thread::sleep(Duration::from_millis(200));

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
/// coverage (draft16+ only — draft 07–13's ANNOUNCE_OK/ERROR shape differs and
/// isn't fully implemented, see `accept_namespace_publish`/`reject_namespace_publish`).
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
fn subscriber_receives_namespace_announcement_without_crashing(#[case] version: &str) {
    let version: MoqVersionArg = version.parse().unwrap();
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "integrationannounce--line".parse().unwrap();

    let pub_args = Args {
        listen: Some(port),
        setup_version: Some(version),
        publish: vec!["integrationannounce".parse().unwrap()],
        pub_passive: vec![TrackArg { file: Some(format!("/proc/self/fd/{pub_r}").into()), track: TrackPattern::Exact(track.clone()), delimiter: b'\n' }],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        setup_version: Some(version),
        subscribe: vec![sub_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    std::thread::sleep(Duration::from_millis(150));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    std::thread::sleep(Duration::from_millis(200));

    write_and_close(pub_w, TEST_LINES);

    std::thread::sleep(Duration::from_millis(600));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    let output = std::fs::read(&out_path).unwrap();
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
    let listen_args = Args { listen: Some(port), ..common.clone() };
    let connect_args = Args { connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()], ..common };

    let (listen_thread, listen_stop) = node::run_stoppable(listen_args);
    std::thread::sleep(Duration::from_millis(150));
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

// ── Publish → Subscribe roundtrip ─────────────────────────────────────────────

#[test]
#[ignore]
fn line_delimited_roundtrip() {
    todo!("publish line-delimited bytes; subscribe and assert bytes match with delimiter preserved");
}

#[test]
#[ignore]
fn fmp4_roundtrip_valid_output() {
    todo!("publish single-track fMP4; subscribe to file; assert output is parseable fMP4 with matching samples");
}

#[test]
#[ignore]
fn two_subscribers_receive_identical_objects() {
    todo!("two simultaneous subscribers on the same track receive the same object sequence");
}

#[test]
#[ignore]
fn late_subscriber_next_group_start_gets_codec_config() {
    todo!("subscriber joining mid-stream with NextGroupStart receives codec config as first object");
}

#[test]
#[ignore]
fn late_subscriber_largest_object_starts_from_latest() {
    todo!("subscriber joining with LargestObject filter receives objects from latest only");
}

#[test]
#[ignore]
fn publisher_finishes_subscriber_drains_and_exits() {
    todo!("publisher closes; subscriber drains remaining cache objects then exits cleanly");
}

// ── Cache ─────────────────────────────────────────────────────────────────────

#[test]
#[ignore]
fn cache_depth_evicts_oldest() {
    todo!("fill cache beyond depth; assert oldest object is evicted and not delivered to new subscriber");
}

#[test]
#[ignore]
fn oversized_object_rejected() {
    todo!("publish object larger than cache_max_obj; assert it is dropped without panic");
}

#[test]
#[ignore]
fn evicted_object_not_delivered() {
    todo!("subscriber starts after cache eviction; assert evicted objects are not received");
}

// ── LoC extension headers ─────────────────────────────────────────────────────

/// `--timestamp` must embed a Timestamp KVP (extension header type 6, per
/// draft-ietf-moq-loc-02 §2.3.1.1) on every published line object, and a
/// real subscriber must be able to read it back out. We verify this through
/// the existing qlog instrumentation rather than a new output format:
/// `InStream::read_next_object_header` (quiche_moq/src/in_stream.rs) already
/// logs each received object's extension headers to qlog when the `qlog`
/// feature is enabled (it is, for moq-cli) and `Args.qlog_dir` is set.
#[test]
fn every_object_has_timestamp() {
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let qlog_dir = tempfile::tempdir().unwrap();
    let track: NamespaceTrackname = "integration--timestamp".parse().unwrap();

    let pub_args = Args {
        listen: Some(port),
        pub_passive: vec![TrackArg { file: Some(format!("/proc/self/fd/{pub_r}").into()), track: TrackPattern::Exact(track.clone()), delimiter: b'\n' }],
        timestamp: true,
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        subscribe: vec![sub_arg(&track, out_path.clone())],
        qlog_dir: Some(qlog_dir.path().to_path_buf()),
        ..Default::default()
    };

    let before_us = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() as u64;

    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    std::thread::sleep(Duration::from_millis(150));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    std::thread::sleep(Duration::from_millis(200));

    write_and_close(pub_w, TEST_LINES);

    std::thread::sleep(Duration::from_millis(600));

    let after_us = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() as u64;

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    // Sanity: the line objects were actually delivered before inspecting qlog.
    let output = std::fs::read(&out_path).unwrap();
    assert_eq!(output, TEST_LINES);

    // Scan every qlog file written during the run for a Timestamp KVP on a
    // received object, via the `moqt:subgroup_object_parsed` qlog event
    // emitted by `quiche_moq::InStream` (quiche_moq/src/in_stream.rs).
    let mut found_timestamps = 0;
    for value in qlog_events(qlog_dir.path()) {
        // Only count the *receiver's* view (logged by `InStream::read_next_object_header`)
        // — the publisher's own `subgroup_object_created` event also carries the same
        // extension headers, but what matters is that the application reading the
        // track back out actually sees them.
        if value.get("name").and_then(|n| n.as_str()) != Some("moqt:subgroup_object_parsed") {
            continue;
        }
        let Some(headers) = value
            .get("data")
            .and_then(|d| d.get("extension_headers"))
            .and_then(|h| h.as_array())
        else {
            continue;
        };
        for h in headers {
            if h.get("header_type").and_then(|t| t.as_u64()) == Some(moq_loc::TIMESTAMP_ID) {
                let ts = h
                    .get("header_value")
                    .and_then(|v| v.as_u64())
                    .expect("Timestamp KVP missing header_value");
                assert!(
                    ts >= before_us && ts <= after_us,
                    "timestamp {ts} not within expected range [{before_us}, {after_us}]"
                );
                found_timestamps += 1;
            }
        }
    }
    assert_eq!(found_timestamps, 3, "expected a Timestamp KVP on each of the 3 published line objects");
}

#[test]
#[ignore]
fn first_object_has_timescale() {
    todo!("assert group 0 object 0 carries a Timescale KVP");
}

#[test]
#[ignore]
fn group_start_object_has_codec_config() {
    todo!("assert object_id == 0 of every group carries a CODEC_CONFIG_ID KVP");
}

#[test]
#[ignore]
fn non_group_start_has_no_codec_config() {
    todo!("assert objects with object_id > 0 do not carry CODEC_CONFIG_ID");
}

#[test]
#[ignore]
fn audio_group_boundary_every_second() {
    todo!("assert audio group boundaries fire at ~1 second intervals (±1 frame tolerance)");
}

#[test]
#[ignore]
fn video_group_boundary_at_keyframe() {
    todo!("assert video group boundaries coincide with fMP4 sync samples");
}

// ── Routing ───────────────────────────────────────────────────────────────────

#[test]
#[ignore]
fn auto_accept_serves_local_cache() {
    todo!("with --auto-accept-subscriptions, incoming subscription for local track is served without prompt");
}

/// Three-node chain: origin --(A)-- relay --(B)-- subscriber. The relay has
/// no local knowledge of the namespace at all (the origin actively
/// `--pub`lishes the track without ever announcing its namespace via
/// PUBLISH_NAMESPACE), so the relay can only serve the downstream
/// subscriber's SUBSCRIBE by falling back to its configured `--upstream`.
#[test]
fn unknown_namespace_forwarded_upstream() {
    let port_a = free_udp_port();
    let port_b = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "fallback--upstream".parse().unwrap();

    // PUBLISH needs draft 16+, but unpinned nodes now cap out at draft 14
    // (draft >= 15 negotiates via ALPN — see `VersionNegotiation`), so pin.
    let version: MoqVersionArg = "16".parse().unwrap();

    // Origin: actively `--pub`lishes the track (no PUBLISH_NAMESPACE announcement)
    // so the relay genuinely has no way to learn this namespace except by asking
    // its --upstream.
    let origin_args = Args {
        listen: Some(port_a),
        setup_version: Some(version),
        publish: vec![PubArg { file: Some(format!("/proc/self/fd/{pub_r}").into()), target: PubTarget::Track(track.clone()), delimiter: b'\n' }],
        ..Default::default()
    };

    // Relay: knows nothing about `fallback--upstream` locally, but has a
    // configured --upstream fallback and a `--sub-passive "*"` wildcard so
    // it relay-forwards any subscription it doesn't serve itself.
    let relay_args = Args {
        listen: Some(port_b),
        setup_version: Some(version),
        connect: vec![format!("https://127.0.0.1:{port_a}#upstream").parse().unwrap()],
        sub_passive: vec!["*".parse().unwrap()],
        ..Default::default()
    };

    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{port_b}").parse().unwrap()],
        setup_version: Some(version),
        subscribe: vec![sub_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (origin_thread, origin_stop) = node::run_stoppable(origin_args);
    std::thread::sleep(Duration::from_millis(150));
    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(200));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    std::thread::sleep(Duration::from_millis(200));

    write_and_close(pub_w, TEST_LINES);

    std::thread::sleep(Duration::from_millis(600));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(relay_stop);
    relay_thread.join().unwrap();
    drop(origin_stop);
    origin_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    let output = std::fs::read(&out_path).unwrap();
    assert_eq!(output, TEST_LINES);
}

#[test]
#[ignore]
fn local_namespace_not_forwarded_upstream() {
    todo!("subscription for locally-cached namespace is served from cache; upstream not contacted");
}

#[test]
#[ignore]
fn auto_accept_publications() {
    todo!("with --auto-accept-publications, incoming namespace announcement is accepted without prompt");
}

// ── Connection ────────────────────────────────────────────────────────────────

#[test]
#[ignore]
fn listen_and_connect_simultaneously() {
    todo!("node listens on a port and connects upstream; subscriber served from upstream-fetched data");
}

#[test]
#[ignore]
fn upstream_disconnect_propagates_error() {
    todo!("upstream disconnects mid-stream; error propagated to subscriber without hang");
}

#[test]
#[ignore]
fn subscriber_disconnect_does_not_affect_publisher() {
    todo!("subscriber disconnects mid-stream; publisher continues producing objects unaffected");
}

// ── Relay behavior parity (vs moq_relay) ──────────────────────────────────────

/// Both publisher and subscriber dial *into* the same relay (unlike
/// `unknown_namespace_forwarded_upstream`, which relies on `--upstream`): the
/// relay must learn the namespace purely from the publisher's explicit
/// `--pub <NS>` PUBLISH_NAMESPACE announcement (trusted via its
/// `--sub-passive "*"` wildcard) and route the subscriber's SUBSCRIBE to it
/// via `namespaces`/`relay_subscriptions`, with no upstream fallback
/// configured at all.
#[test]
fn relay_forwards_publisher_to_subscriber() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).is_test(true).try_init();
    let relay_port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "relay--direct".parse().unwrap();

    // A `--sub-passive "*"` wildcard makes the relay both trust peers'
    // PUBLISH_NAMESPACE announcements and relay-forward subscriptions for
    // namespaces it learns about that way.
    let relay_args = Args {
        listen: Some(relay_port),
        sub_passive: vec!["*".parse().unwrap()],
        ..Default::default()
    };
    let pub_args = Args {
        connect: vec![format!("https://127.0.0.1:{relay_port}").parse().unwrap()],
        pub_passive: vec![TrackArg { file: Some(format!("/proc/self/fd/{pub_r}").into()), track: TrackPattern::Exact(track.clone()), delimiter: b'\n' }],
        // --pub-passive only serves the track; namespace announcement is a
        // separate, explicit concern (--pub <NS>).
        publish: vec!["relay".parse().unwrap()],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{relay_port}").parse().unwrap()],
        subscribe: vec![sub_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(150));
    // Publisher must connect and have its PUBLISH_NAMESPACE accepted before
    // the subscriber's SUBSCRIBE arrives: the relay rejects outright (no
    // retry) a SUBSCRIBE for a namespace it doesn't know about yet.
    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    std::thread::sleep(Duration::from_millis(200));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    std::thread::sleep(Duration::from_millis(200));

    write_and_close(pub_w, TEST_LINES);

    std::thread::sleep(Duration::from_millis(600));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    drop(relay_stop);
    relay_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    let output = std::fs::read(&out_path).unwrap();
    assert_eq!(output, TEST_LINES);
}

/// origin --(PUBLISH_NAMESPACE)--> relay --(--upstream, relay --relay-namespaces)--> observer.
/// `relay` learns the namespace solely via its own `--upstream` connection to
/// `origin` (always auto-accepted by `process_upstream_conn`); whether it then
/// re-announces that learned namespace to `observer` is exactly what
/// `--relay-namespaces` gates (node.rs Phase 2). Propagating it is otherwise
/// invisible to a test treating each node as an opaque thread, so this scans
/// `observer`'s qlog for the `moqt:control_message_parsed` event of a
/// received PUBLISH_NAMESPACE — proof the message actually crossed the wire
/// from relay to observer.
#[test]
fn relay_propagates_namespace_announcement_downstream() {
    let port_a = free_udp_port();
    let port_b = free_udp_port();
    let qlog_dir = tempfile::tempdir().unwrap();
    let ns: Namespace = "propagate-ns-test".parse().unwrap();

    // Origin: announces the namespace only (no track) to whoever connects in.
    let origin_args = Args {
        listen: Some(port_a),
        publish: vec![PubArg { file: None, target: PubTarget::Namespace(ns.clone()), delimiter: b'\n' }],
        ..Default::default()
    };

    // Relay: learns the namespace only via --upstream, then re-announces
    // everything it knows (--relay-namespaces) to its own accepted connections.
    let relay_args = Args {
        listen: Some(port_b),
        connect: vec![format!("https://127.0.0.1:{port_a}#upstream").parse().unwrap()],
        relay_namespace: vec!["*".parse().unwrap()],
        ..Default::default()
    };

    // Observer: only connection is to relay (also via --upstream, the only
    // role that processes incoming PUBLISH_NAMESPACE today) — qlog-enabled so
    // the test can see what crossed the wire.
    let observer_args = Args {
        connect: vec![format!("https://127.0.0.1:{port_b}#upstream").parse().unwrap()],
        qlog_dir: Some(qlog_dir.path().to_path_buf()),
        ..Default::default()
    };

    let (origin_thread, origin_stop) = node::run_stoppable(origin_args);
    std::thread::sleep(Duration::from_millis(150));
    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(250));
    let (observer_thread, observer_stop) = node::run_stoppable(observer_args);
    std::thread::sleep(Duration::from_millis(400));

    drop(observer_stop);
    observer_thread.join().unwrap();
    drop(relay_stop);
    relay_thread.join().unwrap();
    drop(origin_stop);
    origin_thread.join().unwrap();

    let found = qlog_events(qlog_dir.path()).iter().any(|value| {
        value.get("name").and_then(|n| n.as_str()) == Some("moqt:control_message_parsed")
            && value.get("data").and_then(|d| d.get("message")).and_then(|m| m.get("type")).and_then(|t| t.as_str()) == Some("publish_namespace")
    });
    assert!(found, "observer never received a parsed PUBLISH_NAMESPACE control message from relay");
}

#[test]
#[ignore]
fn relay_sends_publish_namespace_done_on_publisher_disconnect() {
    todo!("publisher announces then disconnects; relay sends PUBLISH_NAMESPACE_DONE to other clients that were told about the namespace");
}

// ── SIGUSR1 ────────────────────────────────────────────────────────────────────

/// Sends `sig` to the specific thread backing `handle`, not the whole process.
/// `node::run_stoppable` runs every node's `SignalSource` in its own thread
/// (each blocks the signal via `sigprocmask`, which — per glibc — affects only
/// the calling thread); a process-wide `raise()`/`kill(getpid(), ..)` would be
/// delivered to an arbitrary thread of this test binary, which could just kill
/// the test process since most other threads don't block the signal at all.
fn signal_node_thread(handle: &std::thread::JoinHandle<()>, sig: libc::c_int) {
    use std::os::unix::thread::JoinHandleExt;
    unsafe { libc::pthread_kill(handle.as_pthread_t(), sig) };
}

/// --usr1-ping must send an ack-eliciting PING immediately on SIGUSR1, without
/// waiting for any other traffic. Verified via qlog rather than connection-timing
/// tricks: quiche itself logs every packet it sends (including its frames) as a
/// `quic:packet_sent` event when qlog is enabled, so a deliberate PING shows up
/// as a frame with `frame_type: "ping"` (`Connection::send_ack_eliciting_on_path`,
/// which --usr1-ping uses, makes quiche include a bare PING frame on the next
/// packet for that path if nothing else ack-eliciting is already pending).
#[test]
fn usr1_ping_sends_ping_observable_via_qlog() {
    let port = free_udp_port();
    let qlog_dir = tempfile::tempdir().unwrap();

    let listen_args = Args { listen: Some(port), ..Default::default() };
    let connect_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        usr1_ping: true,
        qlog_dir: Some(qlog_dir.path().to_path_buf()),
        ..Default::default()
    };

    let (listen_thread, listen_stop) = node::run_stoppable(listen_args);
    std::thread::sleep(Duration::from_millis(150));
    let (connect_thread, connect_stop) = node::run_stoppable(connect_args);
    std::thread::sleep(Duration::from_millis(200)); // allow the session to establish

    signal_node_thread(&connect_thread, libc::SIGUSR1);
    std::thread::sleep(Duration::from_millis(200)); // allow the ping to be sent and qlog'd

    drop(connect_stop);
    connect_thread.join().unwrap();
    drop(listen_stop);
    listen_thread.join().unwrap();

    let found = qlog_events(qlog_dir.path()).iter().any(|value| {
        value.get("name").and_then(|n| n.as_str()) == Some("quic:packet_sent")
            && value
                .get("data")
                .and_then(|d| d.get("frames"))
                .and_then(|f| f.as_array())
                .is_some_and(|frames| {
                    frames.iter().any(|f| f.get("frame_type").and_then(|t| t.as_str()) == Some("ping"))
                })
    });
    assert!(found, "no PING frame observed in qlog after SIGUSR1 with --usr1-ping");
}

/// --usr1-toggle-padding-stream must start the padding stream on the first
/// SIGUSR1 and stop it on the second. Verified via the (non-standard, see
/// padding_stream.rs) `moqt:padding_stream_started` / `..._stopped` qlog events.
#[test]
fn usr1_toggle_padding_stream_starts_then_stops_observable_via_qlog() {
    let port = free_udp_port();
    let qlog_dir = tempfile::tempdir().unwrap();

    let listen_args = Args { listen: Some(port), ..Default::default() };
    let connect_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        usr1_toggle_padding_stream: true,
        qlog_dir: Some(qlog_dir.path().to_path_buf()),
        ..Default::default()
    };

    let (listen_thread, listen_stop) = node::run_stoppable(listen_args);
    std::thread::sleep(Duration::from_millis(150));
    let (connect_thread, connect_stop) = node::run_stoppable(connect_args);
    std::thread::sleep(Duration::from_millis(200)); // allow the session to establish

    signal_node_thread(&connect_thread, libc::SIGUSR1); // start
    std::thread::sleep(Duration::from_millis(200));
    signal_node_thread(&connect_thread, libc::SIGUSR1); // stop
    std::thread::sleep(Duration::from_millis(200));

    drop(connect_stop);
    connect_thread.join().unwrap();
    drop(listen_stop);
    listen_thread.join().unwrap();

    let events = qlog_events(qlog_dir.path());
    let started = events.iter().any(|v| v.get("name").and_then(|n| n.as_str()) == Some("moqt:padding_stream_started"));
    let stopped = events.iter().any(|v| v.get("name").and_then(|n| n.as_str()) == Some("moqt:padding_stream_stopped"));
    assert!(started, "padding stream never started on first SIGUSR1");
    assert!(stopped, "padding stream never stopped on second SIGUSR1");
}

/// --usr1-unpub must withdraw (PUBLISH_NAMESPACE_DONE) a namespace this node
/// announced via --pub, on SIGUSR1. Verified via qlog: quiche_moq logs every
/// control message it sends as a `moqt:control_message_created` event, so a
/// PUBLISH_NAMESPACE_DONE frame shows up with `message.type: "publish_namespace_done"`
/// (same mechanism the SETUP/SUBSCRIBE tests elsewhere in this file already
/// rely on via `qlog_events`).
#[test]
fn usr1_unpub_withdraws_announced_namespace_observable_via_qlog() {
    let port = free_udp_port();
    let qlog_dir = tempfile::tempdir().unwrap();

    // sub_passive is what makes the listener trust (and reply PUBLISH_OK to)
    // an incoming PUBLISH_NAMESPACE announcement in the first place — without
    // it the announcement is left pending forever and publish_namespace_done
    // silently no-ops (sent_namespaces is only populated once that reply is
    // processed on the announcing side).
    let listen_args = Args { listen: Some(port), sub_passive: vec!["*".parse().unwrap()], ..Default::default() };
    let connect_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        publish: vec!["tos".parse().unwrap()],
        usr1_unpub: vec!["tos".parse().unwrap()],
        qlog_dir: Some(qlog_dir.path().to_path_buf()),
        ..Default::default()
    };

    let (listen_thread, listen_stop) = node::run_stoppable(listen_args);
    std::thread::sleep(Duration::from_millis(150));
    let (connect_thread, connect_stop) = node::run_stoppable(connect_args);
    std::thread::sleep(Duration::from_millis(300)); // allow PUBLISH_NAMESPACE + REQUEST_OK round trip

    signal_node_thread(&connect_thread, libc::SIGUSR1);
    std::thread::sleep(Duration::from_millis(200)); // allow PUBLISH_NAMESPACE_DONE to be sent and qlog'd

    drop(connect_stop);
    connect_thread.join().unwrap();
    drop(listen_stop);
    listen_thread.join().unwrap();

    fn message_type(v: &serde_json::Value) -> Option<&str> {
        v.get("name")
            .and_then(|n| n.as_str())
            .filter(|n| *n == "moqt:control_message_created")
            .and_then(|_| v.get("data"))
            .and_then(|d| d.get("message"))
            .and_then(|m| m.get("type"))
            .and_then(|t| t.as_str())
    }

    let events = qlog_events(qlog_dir.path());
    let announced = events.iter().any(|v| message_type(v) == Some("publish_namespace"));
    let withdrawn = events.iter().any(|v| message_type(v) == Some("publish_namespace_done"));
    assert!(announced, "namespace was never announced (PUBLISH_NAMESPACE) before SIGUSR1");
    assert!(withdrawn, "namespace was never withdrawn (PUBLISH_NAMESPACE_DONE) after SIGUSR1 with --usr1-unpub");
}

#[test]
#[ignore]
fn relay_sends_publish_done_to_subscriber_on_publisher_disconnect() {
    todo!("relayed publisher disconnects mid-track; subscriber receives PUBLISH_DONE (exercises the currently-dead publisher_gone path), not a silent hang");
}

#[test]
#[ignore]
fn relay_chains_through_upstream() {
    todo!("origin ← relayA --upstream→ relayB ← subscriber; subscriber on relayB receives objects originating behind relayA via upstream subscription forwarding");
}

#[test]
#[ignore]
fn relay_aggregates_delivery_timeout_and_sends_request_update() {
    todo!("two subscribers with differing delivery_timeout; relay subscribes upstream with the max and sends REQUEST_UPDATE when the max changes");
}

#[test]
#[ignore]
fn relay_honors_max_cache_duration_object_expiry() {
    todo!("publisher SUBSCRIBE_OK carries max_cache_duration; relay drops/expires objects older than it instead of forwarding stale data");
}

#[test]
#[ignore]
fn relay_forwards_subscriber_priority_upstream() {
    todo!("subscriber sends SUBSCRIBE with a non-default priority; relay applies it via set_out_track_priority on the forwarded track");
}

// ── CLI flag parity (vs moq_utils pub/sub) ────────────────────────────────────

/// With --keepalive shorter than the idle timeout and no application data flowing,
/// the connector keeps pinging the listener so neither side notices an idle timeout.
/// A client-role node auto-exits once its connection count hits zero (see
/// `graceful_shutdown_closes_peer_promptly`), so a still-running connector thread
/// after waiting past the idle timeout is proof the connection survived.
#[test]
fn keepalive_pings_keep_idle_connection_open() {
    let port = free_udp_port();

    // Short idle timeout so a dead connection would be noticed quickly.
    let listen_args = Args { listen: Some(port), timeout: 600, ..Default::default() };
    let connect_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        timeout: 600,
        keepalive: 100, // well under the idle timeout
        keepalive_no_pto_limit: true, // exact interval, no PTO-floor uncertainty
        ..Default::default()
    };

    let (listen_thread, listen_stop) = node::run_stoppable(listen_args);
    std::thread::sleep(Duration::from_millis(150));
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
#[ignore]
fn keepalive_no_pto_limit_uses_exact_interval() {
    todo!("with --keepalive-no-pto-limit, pings are emitted at exactly the configured interval regardless of the current PTO estimate");
}

#[test]
fn keylog_file_written_when_sslkeylogfile_set() {
    let port = free_udp_port();
    let keylog_file = tempfile::NamedTempFile::new().unwrap();

    let listen_args = Args { listen: Some(port), ..Default::default() };
    let connect_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        // Only the outbound (--connect) side is wired up; --listen-accepted
        // connections don't apply the keylog hook (see ssl_key_log_file's doc
        // comment in args.rs).
        ssl_key_log_file: Some(keylog_file.path().to_path_buf()),
        ..Default::default()
    };

    let (listen_thread, listen_stop) = node::run_stoppable(listen_args);
    std::thread::sleep(Duration::from_millis(150));
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

/// Runs a publisher/subscriber pair pinned to the given `--setup-version`s (if
/// any) and returns whatever the subscriber wrote out — empty if the SETUP
/// handshake never completed.
fn run_setup_version_case(listen_version: Option<&str>, connect_version: Option<&str>) -> Vec<u8> {
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "setup-version--line".parse().unwrap();

    let listen_args = Args {
        listen: Some(port),
        pub_passive: vec![TrackArg { file: Some(format!("/proc/self/fd/{pub_r}").into()), track: TrackPattern::Exact(track.clone()), delimiter: b'\n' }],
        setup_version: listen_version.map(|v| v.parse().unwrap()),
        ..Default::default()
    };
    let connect_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        subscribe: vec![sub_arg(&track, out_path.clone())],
        setup_version: connect_version.map(|v| v.parse().unwrap()),
        ..Default::default()
    };

    let (listen_thread, listen_stop) = node::run_stoppable(listen_args);
    std::thread::sleep(Duration::from_millis(150));
    let (connect_thread, connect_stop) = node::run_stoppable(connect_args);
    std::thread::sleep(Duration::from_millis(200));

    write_and_close(pub_w, TEST_LINES);
    std::thread::sleep(Duration::from_millis(600));

    drop(connect_stop);
    let _ = connect_thread.join();
    drop(listen_stop);
    let _ = listen_thread.join();
    unsafe { libc::close(pub_r) };

    std::fs::read(&out_path).unwrap_or_default()
}

/// `--setup-version` must restrict the *offered/required* version exclusively,
/// not just add it to the default range — otherwise a broken implementation
/// (e.g. only setting `setup_version` but leaving `supported_versions` at its
/// default full range) would silently pass. Verified two ways:
/// 1. Connector pinned to draft07 only, against a listener that still
///    requires the default draft16 — draft16 isn't in the connector's sole
///    offer, so the SETUP handshake must fail and no objects are delivered.
/// 2. Both ends pinned to the same explicit draft07 — must still complete
///    a normal pub/sub exchange.
#[test]
fn setup_version_negotiates_requested_draft() {
    // Mismatched, non-overlapping versions: SETUP must fail, nothing delivered.
    // (Not `None` for the listener: that means "unpinned", i.e. the *default*
    // supported-versions range, which has always included draft07 — so it
    // would actually overlap the connector's sole draft07 offer and negotiate
    // successfully, not what this case is testing.)
    let mismatched = run_setup_version_case(Some("draft16"), Some("draft07"));
    assert!(mismatched.is_empty(), "objects delivered despite a SETUP version mismatch: {mismatched:?}");

    // Both ends pinned to the same explicit (non-default) draft: must still work.
    let matched = run_setup_version_case(Some("draft13"), Some("draft13"));
    assert_eq!(matched, TEST_LINES);
}

/// Runs a publisher/subscriber pair with the given `--supported-versions`
/// lists (if any) and returns whatever the subscriber wrote out — empty if
/// the SETUP handshake never completed.
fn run_supported_versions_case(listen_versions: Option<&[&str]>, connect_versions: Option<&[&str]>) -> Vec<u8> {
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "supported-versions--line".parse().unwrap();

    let listen_args = Args {
        listen: Some(port),
        pub_passive: vec![TrackArg { file: Some(format!("/proc/self/fd/{pub_r}").into()), track: TrackPattern::Exact(track.clone()), delimiter: b'\n' }],
        supported_versions: listen_versions.map(|vs| vs.iter().map(|v| v.parse().unwrap()).collect()),
        ..Default::default()
    };
    let connect_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        subscribe: vec![sub_arg(&track, out_path.clone())],
        supported_versions: connect_versions.map(|vs| vs.iter().map(|v| v.parse().unwrap()).collect()),
        ..Default::default()
    };

    let (listen_thread, listen_stop) = node::run_stoppable(listen_args);
    std::thread::sleep(Duration::from_millis(150));
    let (connect_thread, connect_stop) = node::run_stoppable(connect_args);
    std::thread::sleep(Duration::from_millis(200));

    write_and_close(pub_w, TEST_LINES);
    std::thread::sleep(Duration::from_millis(600));

    drop(connect_stop);
    let _ = connect_thread.join();
    drop(listen_stop);
    let _ = listen_thread.join();
    unsafe { libc::close(pub_r) };

    std::fs::read(&out_path).unwrap_or_default()
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
    let disjoint = run_supported_versions_case(Some(&["draft07", "draft08"]), Some(&["draft13", "draft14"]));
    assert!(disjoint.is_empty(), "objects delivered despite disjoint supported-versions sets: {disjoint:?}");

    // Overlapping but non-identical sets: must negotiate the shared version
    // and complete a normal pub/sub exchange.
    let overlapping = run_supported_versions_case(Some(&["draft12", "draft13"]), Some(&["draft13", "draft14"]));
    assert_eq!(overlapping, TEST_LINES);
}

// ── Shutdown / cleanup ────────────────────────────────────────────────────────
