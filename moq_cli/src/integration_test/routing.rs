//! Which connection a subscription is routed to.

use super::*;

/// Three-node chain: origin --(A)-- relay --(B)-- subscriber. The relay has
/// no local knowledge of the namespace at all (the origin actively
/// `--pub`lishes the track without ever announcing its namespace via
/// PUBLISH_NAMESPACE), so the relay can only serve the downstream
/// subscriber's SUBSCRIBE by falling back to its `#upstream` connection.
/// A relayed track whose upstream is gone is removed, so a later subscriber
/// is relayed via the new source (`origin_b`) instead of a dead track.
#[test]
fn relayed_track_is_rebuilt_after_losing_its_upstream() {
    let port_a = free_udp_port();
    let relay_port = free_udp_port();
    let (pub_r_a, _pub_w_a) = make_pipe();
    let (pub_r_b, pub_w_b) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "rebuild--relayed".parse().unwrap();
    let version: MoqVersionArg = "16".parse().unwrap();

    let origin_a_args = Args {
        listen: Some(port_a.into()),
        setup_version: Some(version),
        publish: vec![PubArg { target: PubTarget::Track(TrackPattern::Exact(track.clone())), max_cache_duration: None, passive: false, datagram: false }],
        r#in: vec![in_arg(pub_r_a, &track, b'\n')],
        ..Default::default()
    };
    let relay_args = Args {
        listen: Some(relay_port.into()),
        setup_version: Some(version),
        connect: vec![format!("https://127.0.0.1:{port_a}#upstream").parse().unwrap()],
        subscribe: vec!["*".parse().unwrap()],
        ..Default::default()
    };
    let sub_args = |out: Vec<OutArg>| Args {
        connect: vec![format!("https://127.0.0.1:{relay_port}#upstream").parse().unwrap()],
        setup_version: Some(version),
        subscribe: vec![sub_arg(&track)],
        out,
        ..Default::default()
    };
    // connects to the relay and announces the namespace
    let origin_b_args = Args {
        connect: vec![format!("https://127.0.0.1:{relay_port}").parse().unwrap()],
        setup_version: Some(version),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_b, &track, b'\n')],
        ..Default::default()
    };

    let (origin_a_thread, origin_a_stop) = node::run_stoppable(origin_a_args);
    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(200));
    let (sub_1_thread, sub_1_stop) = node::run_stoppable(sub_args(Vec::new()));
    std::thread::sleep(Duration::from_millis(300));

    drop(origin_a_stop);
    origin_a_thread.join().unwrap();
    std::thread::sleep(Duration::from_millis(300)); // relay drops the relayed track
    let (origin_b_thread, origin_b_stop) = node::run_stoppable(origin_b_args);
    std::thread::sleep(Duration::from_millis(300));
    let (sub_2_thread, sub_2_stop) = node::run_stoppable(sub_args(vec![out_arg(&track, out_path.clone())]));
    std::thread::sleep(Duration::from_millis(300));
    write_and_close(pub_w_b, TEST_LINES);

    let output = wait_for_file_len(&out_path, TEST_LINES.len(), Duration::from_secs(5));

    drop(sub_2_stop);
    sub_2_thread.join().unwrap();
    drop(sub_1_stop);
    sub_1_thread.join().unwrap();
    drop(origin_b_stop);
    origin_b_thread.join().unwrap();
    drop(relay_stop);
    relay_thread.join().unwrap();
    unsafe { libc::close(pub_r_a); libc::close(_pub_w_a); libc::close(pub_r_b); }

    assert_eq!(output, TEST_LINES);
}

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
    // its #upstream.
    let origin_args = Args {
        listen: Some(port_a.into()),
        setup_version: Some(version),
        publish: vec![PubArg { target: PubTarget::Track(TrackPattern::Exact(track.clone())), max_cache_duration: None, passive: false, datagram: false }],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };

    // Relay: knows nothing about `fallback--upstream` locally, but has a
    // `#upstream` fallback and a `--sub "*"` wildcard so
    // it relay-forwards any subscription it doesn't serve itself.
    let relay_args = Args {
        listen: Some(port_b.into()),
        setup_version: Some(version),
        connect: vec![format!("https://127.0.0.1:{port_a}#upstream").parse().unwrap()],
        subscribe: vec!["*".parse().unwrap()],
        ..Default::default()
    };

    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{port_b}#upstream").parse().unwrap()],
        setup_version: Some(version),
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (origin_thread, origin_stop) = node::run_stoppable(origin_args);
    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(200));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

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

/// origin ← relay (#upstream, own --pub track) ← subscriber: a SUBSCRIBE for
/// the relay's own track is served locally, the upstream never sees it.
#[test]
fn local_namespace_not_forwarded_upstream() {
    let origin_port = free_udp_port();
    let relay_port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let origin_qlog = tempfile::tempdir().unwrap();
    let track: NamespaceTrackname = "localonly--track".parse().unwrap();

    let origin_args = Args {
        listen: Some(origin_port.into()),
        qlog_dir: Some(origin_qlog.path().to_path_buf()),
        ..Default::default()
    };
    let relay_args = Args {
        listen: Some(relay_port.into()),
        connect: vec![format!("https://127.0.0.1:{origin_port}#upstream").parse().unwrap()],
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{relay_port}#upstream").parse().unwrap()],
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (origin_thread, origin_stop) = node::run_stoppable(origin_args);
    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(250));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);
    write_and_close(pub_w, TEST_LINES);
    let output = wait_for_file_len(&out_path, TEST_LINES.len(), Duration::from_secs(5));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(relay_stop);
    relay_thread.join().unwrap();
    drop(origin_stop);
    origin_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    assert_eq!(output, TEST_LINES);
    let subscribes = control_messages(origin_qlog.path()).into_iter()
        .filter(|(name, m)| name == "moqt:control_message_parsed" && m["type"] == "subscribe")
        .count();
    assert_eq!(subscribes, 0, "the upstream was asked for a local track");
}
