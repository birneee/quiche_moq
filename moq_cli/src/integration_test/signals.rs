//! SIGUSR1/SIGUSR2 actions.

use super::*;

/// `SigAction::Ping` must send an ack-eliciting PING immediately on SIGUSR1,
/// without waiting for other traffic. Verified via qlog: quiche logs every
/// sent packet's frames as `quic:packet_sent`, so a deliberate PING
/// (`Connection::send_ack_eliciting_on_path`) shows up as `frame_type: "ping"`.
#[test]
fn sigusr1_ping_sends_ping_observable_via_qlog() {
    let _signals = signal_lock();
    let port = free_udp_port();
    let qlog_dir = tempfile::tempdir().unwrap();

    let listen_args = Args { listen: Some(port.into()), ..Default::default() };
    let connect_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        sigusr1: vec![SigAction::Ping],
        qlog_dir: Some(qlog_dir.path().to_path_buf()),
        ..Default::default()
    };

    let (listen_thread, listen_stop) = node::run_stoppable(listen_args);
    let (connect_thread, connect_stop) = node::run_stoppable(connect_args);
    std::thread::sleep(Duration::from_millis(200)); // allow the session to establish

    raise(libc::SIGUSR1);
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
    assert!(found, "no PING frame observed in qlog after SIGUSR1 with SigAction::Ping");
}

/// `SigAction::TogglePaddingStream` must start the padding stream on the
/// first SIGUSR1 and stop it on the second. Verified via the (non-standard,
/// see padding_stream.rs) `moqt:padding_stream_started` / `..._stopped` qlog events.
#[test]
fn sigusr1_toggle_padding_stream_starts_then_stops_observable_via_qlog() {
    let _signals = signal_lock();
    let port = free_udp_port();
    let qlog_dir = tempfile::tempdir().unwrap();

    let listen_args = Args { listen: Some(port.into()), ..Default::default() };
    let connect_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        sigusr1: vec![SigAction::TogglePaddingStream],
        qlog_dir: Some(qlog_dir.path().to_path_buf()),
        ..Default::default()
    };

    let (listen_thread, listen_stop) = node::run_stoppable(listen_args);
    let (connect_thread, connect_stop) = node::run_stoppable(connect_args);
    std::thread::sleep(Duration::from_millis(200)); // allow the session to establish

    raise(libc::SIGUSR1); // start
    std::thread::sleep(Duration::from_millis(200));
    raise(libc::SIGUSR1); // stop
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

/// `SigAction::Unpub` must withdraw (PUBLISH_NAMESPACE_DONE) a namespace
/// this node announced via --pub, on SIGUSR1. Verified via qlog:
/// quiche_moq logs every control message it sends as
/// `moqt:control_message_created`, same mechanism used elsewhere in this file.
#[test]
fn sigusr1_unpub_withdraws_announced_namespace_observable_via_qlog() {
    let _signals = signal_lock();
    let port = free_udp_port();
    let qlog_dir = tempfile::tempdir().unwrap();

    // The wildcard --sub is what makes the listener trust (and reply
    // PUBLISH_OK to) an incoming PUBLISH_NAMESPACE announcement in the first
    // place — without it the announcement is left pending forever and
    // publish_namespace_done silently no-ops (sent_namespaces is only
    // populated once that reply is processed on the announcing side).
    let listen_args = Args { listen: Some(port.into()), subscribe: vec!["*".parse().unwrap()], ..Default::default() };
    let connect_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        publish: vec!["tos".parse().unwrap()],
        unpub: vec!["tos".parse().unwrap()],
        sigusr1: vec![SigAction::Unpub],
        qlog_dir: Some(qlog_dir.path().to_path_buf()),
        ..Default::default()
    };

    let (listen_thread, listen_stop) = node::run_stoppable(listen_args);
    let (connect_thread, connect_stop) = node::run_stoppable(connect_args);
    std::thread::sleep(Duration::from_millis(300)); // allow PUBLISH_NAMESPACE + REQUEST_OK round trip

    raise(libc::SIGUSR1);
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
    assert!(withdrawn, "namespace was never withdrawn (PUBLISH_NAMESPACE_DONE) after SIGUSR1 with SigAction::Unpub");
}

/// Regression test: publishing must not permanently stall after a
/// subscriber's connection migrates to a new local address
/// (`SigAction::Migrate`) — e.g. via stream-credit exhaustion after the path
/// change, or a forwarder that never revisits out-of-order objects.
#[test]
fn sigusr1_migrate_does_not_stall_local_track_publishing() {
    let _signals = signal_lock();
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let qlog_dir = tempfile::tempdir().unwrap();
    let track: NamespaceTrackname = "integration--migrate-stall".parse().unwrap();

    let pub_args = Args {
        listen: Some(port.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        init_cwnd: Some(10),
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        sigusr1: vec![SigAction::Migrate],
        init_cwnd: Some(10),
        qlog_dir: Some(qlog_dir.path().to_path_buf()),
        ..Default::default()
    };

    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

    // Continuously publish one line every 2ms from a background thread,
    // straddling the migration trigger below — matching the testbed's
    // `while true; do ...; sleep N; done > fifo` line-rate publish pattern,
    // as opposed to the single write-then-close every other test here uses.
    let keep_writing = Arc::new(AtomicBool::new(true));
    let writer = {
        let keep_writing = keep_writing.clone();
        std::thread::spawn(move || {
            let mut n: u64 = 0;
            while keep_writing.load(Ordering::Relaxed) {
                let line = format!("line-{n}\n");
                unsafe { libc::write(pub_w, line.as_ptr() as *const _, line.len()) };
                n += 1;
                std::thread::sleep(Duration::from_millis(2));
            }
        })
    };

    // Let a healthy pre-migration baseline establish (~150 lines at 2ms/line).
    std::thread::sleep(Duration::from_millis(300));

    raise(libc::SIGUSR1); // SigAction::Migrate

    // Give the migrated path time to validate and, critically, time for
    // publishing to resume afterward if it's going to (this is generous:
    // in the original testbed run the stall was permanent for the rest of
    // a 10s run, not just briefly delayed).
    std::thread::sleep(Duration::from_millis(1500));

    keep_writing.store(false, Ordering::Relaxed);
    writer.join().unwrap();
    unsafe { libc::close(pub_w) };

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    // Sanity: migration was actually attempted (a PATH_CHALLENGE or
    // PATH_RESPONSE frame is quiche's on-the-wire signature of validating a
    // new path — same mechanism already relied on elsewhere for path
    // validation; the analysis tooling this bug was found
    // from also keys off these frame types).
    let events = qlog_events(qlog_dir.path());
    let migration_attempted = events.iter().any(|v| {
        v.get("data")
            .and_then(|d| d.get("frames"))
            .and_then(|f| f.as_array())
            .is_some_and(|frames| {
                frames.iter().any(|fr| {
                    matches!(fr.get("frame_type").and_then(|t| t.as_str()), Some("path_challenge" | "path_response"))
                })
            })
    });
    assert!(migration_attempted, "no path_challenge/path_response frame observed in qlog after SIGUSR1 with SigAction::Migrate");

    // The actual regression check: the subscriber must have kept receiving
    // objects well after migration, not just the pre-migration baseline.
    // At 2ms/line, line-400 was written ~800ms after the migration trigger
    // — comfortably past both path validation and the ~150 lines produced
    // before migration — so seeing it proves publishing survived the
    // migration rather than stalling shortly afterward.
    let received = std::fs::read_to_string(&out_path).unwrap();
    let max_received_n = received
        .lines()
        .filter_map(|l| l.strip_prefix("line-").and_then(|n| n.parse::<u64>().ok()))
        .max();
    assert!(
        max_received_n.is_some_and(|n| n >= 400),
        "publishing stalled after migration: highest line received was {max_received_n:?}, expected >= 400"
    );
}
