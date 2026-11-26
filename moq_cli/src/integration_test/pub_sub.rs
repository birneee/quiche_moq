//! Publish → subscribe, also through relays and with several sources.

use super::*;

/// fMP4 in, MoQ, fMP4 out: the subscriber's file parses as fMP4 with the
/// published codec, timescale and samples.
#[test]
fn fmp4_roundtrip_valid_output() {
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "integration--fmp4-roundtrip".parse().unwrap();
    let samples: Vec<(Vec<u8>, bool)> = (0..6).map(|i| (format!("frame{i}").into_bytes(), i % 3 == 0)).collect();

    let pub_args = Args {
        listen: Some(port.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        subscribe: vec![sub_arg(&track)],
        out: vec![OutArg { track: track.clone(), file: Some(out_path.clone()), output_kind: crate::file_output::OutputKind::fmp4() }],
        ..Default::default()
    };

    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

    write_only(pub_w, &crate::fmp4_write::ftyp());
    write_only(pub_w, &crate::fmp4_write::moov(&[(90_000, b"avc1")]));
    for (i, (payload, sync)) in samples.iter().enumerate() {
        write_only(pub_w, &crate::fmp4_write::fragment(i as u32, 1, i as u64 * 3000, 3000, *sync, payload));
    }
    unsafe { libc::close(pub_w) };

    let parse = |data: &[u8]| {
        let mut demux = crate::fmp4_demux::FMp4Demuxer::new();
        demux.push(data);
        let received: Vec<_> = std::iter::from_fn(|| demux.next_sample()).collect();
        (demux, received)
    };
    let start = std::time::Instant::now();
    let (demux, received) = loop {
        let (demux, received) = parse(&std::fs::read(&out_path).unwrap_or_default());
        if received.len() >= samples.len() || start.elapsed() > Duration::from_secs(5) {
            break (demux, received);
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    let info = demux.track.expect("output has a moov");
    // LOC carries microseconds
    assert_eq!(info.timescale, 1_000_000);
    assert!(info.codec_config.is_some_and(|c| c.starts_with(b"avc1")));
    // 3000 ticks at 90 kHz, give or take the playback pacing
    let steps: Vec<u64> = received.windows(2).map(|w| w[1].decode_time - w[0].decode_time).collect();
    assert!(steps.iter().all(|&step| step.abs_diff(33_333) <= 33_333 / 10), "{steps:?}");
    let received: Vec<(Vec<u8>, bool)> = received.into_iter().map(|s| (s.data, s.is_sync)).collect();
    assert_eq!(received, samples);
}

/// Exercises per-subscriber forwarding state being genuinely independent
/// (`CacheToMoq` in `moq_cli::cache_to_moq`, one per alias) —
/// both subscribers must see the identical object sequence off the same
/// `LocalTrack` cache.
#[test]
fn two_subscribers_receive_identical_objects() {
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file_1 = tempfile::NamedTempFile::new().unwrap();
    let out_path_1 = out_file_1.path().to_path_buf();
    let out_file_2 = tempfile::NamedTempFile::new().unwrap();
    let out_path_2 = out_file_2.path().to_path_buf();
    let track: NamespaceTrackname = "integration--two-subs".parse().unwrap();

    let pub_args = Args {
        listen: Some(port.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    let sub_args_1 = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path_1.clone())],
        ..Default::default()
    };
    let sub_args_2 = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path_2.clone())],
        ..Default::default()
    };

    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    let (sub1_thread, sub1_stop) = node::run_stoppable(sub_args_1);
    let (sub2_thread, sub2_stop) = node::run_stoppable(sub_args_2);
    std::thread::sleep(Duration::from_millis(200));

    write_and_close(pub_w, TEST_LINES);

    let output_1 = wait_for_file_len(&out_path_1, TEST_LINES.len(), Duration::from_secs(5));
    let output_2 = wait_for_file_len(&out_path_2, TEST_LINES.len(), Duration::from_secs(5));

    drop(sub1_stop);
    sub1_thread.join().unwrap();
    drop(sub2_stop);
    sub2_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    assert_eq!(output_1, TEST_LINES);
    assert_eq!(output_2, TEST_LINES);
}

/// Subscribing to the same track via two connections must merge into one
/// coherent stream, not duplicate every object — the bug fixed by unifying
/// `--sub` ingestion onto `moq_object_cache::{begin_or_join, fold}` (see node/ingest.rs).
#[test]
fn two_publishers_same_track_merge_without_duplication() {
    let port_a = free_udp_port();
    let port_b = free_udp_port();
    let (pub_r_a, pub_w_a) = make_pipe();
    let (pub_r_b, pub_w_b) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "multipath--merge".parse().unwrap();

    let pub_args_a = Args {
        listen: Some(port_a.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_a, &track, b'\n')],
        ..Default::default()
    };
    let pub_args_b = Args {
        listen: Some(port_b.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_b, &track, b'\n')],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![
            format!("https://127.0.0.1:{port_a}").parse().unwrap(),
            format!("https://127.0.0.1:{port_b}").parse().unwrap(),
        ],
        subscribe: vec![sub_arg_multi(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (pub_a_thread, pub_a_stop) = node::run_stoppable(pub_args_a);
    let (pub_b_thread, pub_b_stop) = node::run_stoppable(pub_args_b);
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    std::thread::sleep(Duration::from_millis(200));

    // Identical content from both publishers, arriving over two independent
    // connections — same Group/Object IDs, since each publisher derives
    // them the same deterministic way from the same input.
    write_and_close(pub_w_a, TEST_LINES);
    write_and_close(pub_w_b, TEST_LINES);

    let output = wait_for_file_len(&out_path, TEST_LINES.len(), Duration::from_secs(5));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_a_stop);
    pub_a_thread.join().unwrap();
    drop(pub_b_stop);
    pub_b_thread.join().unwrap();
    unsafe { libc::close(pub_r_a); libc::close(pub_r_b); }

    assert_eq!(output, TEST_LINES, "expected merged content with no duplication from two publishers of the same track");
}

/// Without `#multi`, `--sub` picks exactly one of two announcers: one SUBSCRIBE.
/// Which one wins isn't pinned down.
#[test]
fn sub_default_picks_one_candidate_not_both() {
    let port_a = free_udp_port();
    let port_b = free_udp_port();
    let (pub_r_a, pub_w_a) = make_pipe();
    let (pub_r_b, pub_w_b) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let qlog_dir = tempfile::tempdir().unwrap();
    let track: NamespaceTrackname = "singlecandidate--track".parse().unwrap();

    let pub_args_a = Args {
        listen: Some(port_a.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_a, &track, b'\n')],
        ..Default::default()
    };
    let pub_args_b = Args {
        listen: Some(port_b.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_b, &track, b'\n')],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![
            format!("https://127.0.0.1:{port_a}").parse().unwrap(),
            format!("https://127.0.0.1:{port_b}").parse().unwrap(),
        ],
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        qlog_dir: Some(qlog_dir.path().to_path_buf()),
        ..Default::default()
    };

    let (pub_a_thread, pub_a_stop) = node::run_stoppable(pub_args_a);
    let (pub_b_thread, pub_b_stop) = node::run_stoppable(pub_args_b);
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

    write_and_close(pub_w_a, b"from-a\n");
    write_and_close(pub_w_b, b"from-b\n");

    let output = wait_for_file_len(&out_path, b"from-a\n".len(), Duration::from_secs(5));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_a_stop);
    pub_a_thread.join().unwrap();
    drop(pub_b_stop);
    pub_b_thread.join().unwrap();
    unsafe { libc::close(pub_r_a); libc::close(pub_r_b); }

    assert!(output == b"from-a\n" || output == b"from-b\n", "expected one candidate's content: {output:?}");
    let subscribes = qlog_events(qlog_dir.path()).iter()
        .filter(|e| e["name"] == "moqt:control_message_created" && e["data"]["message"]["type"] == "subscribe")
        .count();
    assert_eq!(subscribes, 1);
}

/// Losing the sole non-`multi` source must not fail over to an
/// already-known alternate — only a *new* candidate can (see
/// `sync_sub_track_templates`). Second candidate is known from the start but
/// never used, proving the selection sticks.
#[test]
fn sub_default_no_automatic_failover_after_source_disconnect() {
    let port_a = free_udp_port();
    let port_b = free_udp_port();
    let (pub_r_a, pub_w_a) = make_pipe();
    let (pub_r_b, pub_w_b) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "nofailover--track".parse().unwrap();

    let pub_args_a = Args {
        listen: Some(port_a.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_a, &track, b'\n')],
        ..Default::default()
    };
    let pub_args_b = Args {
        listen: Some(port_b.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_b, &track, b'\n')],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![
            format!("https://127.0.0.1:{port_a}").parse().unwrap(),
            format!("https://127.0.0.1:{port_b}").parse().unwrap(),
        ],
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (pub_a_thread, pub_a_stop) = node::run_stoppable(pub_args_a);
    let (pub_b_thread, pub_b_stop) = node::run_stoppable(pub_args_b);
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

    write_only(pub_w_a, b"from-a\n");
    write_only(pub_w_b, b"from-b\n");
    let output = wait_for_file_len(&out_path, b"from-a\n".len(), Duration::from_secs(5));
    assert!(output == b"from-a\n" || output == b"from-b\n", "expected exactly one candidate chosen: {output:?}");
    let (chosen_stop, chosen_thread, chosen_fds, other_stop, other_thread, other_fds) = if output == b"from-a\n" {
        (pub_a_stop, pub_a_thread, (pub_r_a, pub_w_a), pub_b_stop, pub_b_thread, (pub_r_b, pub_w_b))
    } else {
        (pub_b_stop, pub_b_thread, (pub_r_b, pub_w_b), pub_a_stop, pub_a_thread, (pub_r_a, pub_w_a))
    };

    // The chosen source disconnects, then the other one publishes more: a
    // failover to it would deliver that.
    drop(chosen_stop);
    chosen_thread.join().unwrap();
    unsafe { libc::close(chosen_fds.0); libc::close(chosen_fds.1) };
    std::thread::sleep(Duration::from_millis(300));
    write_and_close(other_fds.1, b"more\n");
    std::thread::sleep(Duration::from_millis(600));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(other_stop);
    other_thread.join().unwrap();
    unsafe { libc::close(other_fds.0) };

    let final_output = std::fs::read(&out_path).unwrap();
    assert_eq!(final_output, output, "must not automatically fail over to an already-known alternate candidate");
}

/// A slow/never-contributing source must not hold back delivery — the
/// subscriber gets the complete object from whichever source is faster.
#[test]
fn subscriber_gets_complete_object_despite_one_slow_publisher() {
    let port_fast = free_udp_port();
    let port_slow = free_udp_port();
    let (pub_r_fast, pub_w_fast) = make_pipe();
    let (pub_r_slow, pub_w_slow) = make_pipe(); // never written
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "multipath--race".parse().unwrap();

    let pub_args_fast = Args {
        listen: Some(port_fast.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_fast, &track, b'\n')],
        ..Default::default()
    };
    let pub_args_slow = Args {
        listen: Some(port_slow.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_slow, &track, b'\n')],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![
            format!("https://127.0.0.1:{port_fast}").parse().unwrap(),
            format!("https://127.0.0.1:{port_slow}").parse().unwrap(),
        ],
        subscribe: vec![sub_arg_multi(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (pub_fast_thread, pub_fast_stop) = node::run_stoppable(pub_args_fast);
    let (pub_slow_thread, pub_slow_stop) = node::run_stoppable(pub_args_slow);
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    std::thread::sleep(Duration::from_millis(200));

    write_and_close(pub_w_fast, TEST_LINES);

    // Complete content from the fast publisher alone, well within a bound
    // that doesn't depend on the slow publisher ever doing anything.
    let output = wait_for_file_len(&out_path, TEST_LINES.len(), Duration::from_secs(3));
    assert_eq!(output, TEST_LINES, "expected complete content from the fast publisher alone");

    // The slow publisher is stopped having never written anything — proves
    // the subscriber didn't wait on it.
    drop(pub_slow_stop);
    pub_slow_thread.join().unwrap();
    unsafe { libc::close(pub_r_slow); libc::close(pub_w_slow) };

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_fast_stop);
    pub_fast_thread.join().unwrap();
    unsafe { libc::close(pub_r_fast) };
}

/// A relay is nothing special: it's a track that both subscribes (multiple
/// upstream sources, via explicit `--sub` over two `--connect` targets) and
/// publishes (--listen) at once. Proves the same merge mechanism works
/// end-to-end through a relay hop, not just for a plain client.
#[test]
fn relay_multipath_merges_two_upstream_publishers() {
    let port_a = free_udp_port();
    let port_b = free_udp_port();
    let relay_port = free_udp_port();
    let (pub_r_a, pub_w_a) = make_pipe();
    let (pub_r_b, pub_w_b) = make_pipe();
    let relay_out_file = tempfile::NamedTempFile::new().unwrap();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "relaymultipath--video".parse().unwrap();

    let pub_args_a = Args {
        listen: Some(port_a.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_a, &track, b'\n')],
        ..Default::default()
    };
    let pub_args_b = Args {
        listen: Some(port_b.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_b, &track, b'\n')],
        ..Default::default()
    };
    let relay_args = Args {
        listen: Some(relay_port.into()),
        connect: vec![
            format!("https://127.0.0.1:{port_a}").parse().unwrap(),
            format!("https://127.0.0.1:{port_b}").parse().unwrap(),
        ],
        subscribe: vec![sub_arg_multi(&track)],
        out: vec![out_arg(&track, relay_out_file.path().to_path_buf())],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{relay_port}#upstream").parse().unwrap()],
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (pub_a_thread, pub_a_stop) = node::run_stoppable(pub_args_a);
    let (pub_b_thread, pub_b_stop) = node::run_stoppable(pub_args_b);
    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(250));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

    write_and_close(pub_w_a, TEST_LINES);
    write_and_close(pub_w_b, TEST_LINES);

    let output = wait_for_file_len(&out_path, TEST_LINES.len(), Duration::from_secs(5));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(relay_stop);
    relay_thread.join().unwrap();
    drop(pub_a_stop);
    pub_a_thread.join().unwrap();
    drop(pub_b_stop);
    pub_b_thread.join().unwrap();
    unsafe { libc::close(pub_r_a); libc::close(pub_r_b); }

    assert_eq!(output, TEST_LINES, "relay must merge both upstream sources without duplication and serve the result downstream");
}

/// Regression test: a relay must defer its downstream SUBSCRIBE_OK until its
/// own upstream subscription resolves.
#[test]
fn relay_subscribe_ok_deferred_until_upstream_resolves_carries_max_cache_duration() {
    let origin_port = free_udp_port();
    let relay_port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "defer--mcd".parse().unwrap();
    let qlog_dir = tempfile::tempdir().unwrap();
    const MAX_CACHE_DURATION_MS: u64 = 12345;
    // PUBLISH (used to set max_cache_duration) needs draft 16+; unpinned
    // nodes cap out at draft 14 (see unknown_namespace_forwarded_upstream).
    let version: MoqVersionArg = "16".parse().unwrap();

    let origin_args = Args {
        listen: Some(origin_port.into()),
        setup_version: Some(version),
        publish: vec![PubArg {
            target: PubTarget::Track(TrackPattern::Exact(track.clone())),
            max_cache_duration: Some(MAX_CACHE_DURATION_MS),
            passive: false,
            datagram: false,
        }],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    // `--sub "*"` (not a namespace announcement) makes the relay
    // relay-forward any SUBSCRIBE it doesn't already know about via its
    // `#upstream` fallback -- same shape as `unknown_namespace_forwarded_upstream`.
    let relay_args = Args {
        listen: Some(relay_port.into()),
        setup_version: Some(version),
        connect: vec![format!("https://127.0.0.1:{origin_port}#upstream").parse().unwrap()],
        subscribe: vec!["*".parse().unwrap()],
        qlog_dir: Some(qlog_dir.path().to_path_buf()),
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{relay_port}#upstream").parse().unwrap()],
        setup_version: Some(version),
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };

    // origin, then relay, then subscriber -- same ordering as the already-
    // reliable unknown_namespace_forwarded_upstream. Relay still won't know
    // the track (and thus won't know max_cache_duration) until its own
    // #upstream subscribe to origin round-trips, so the subscriber's
    // SUBSCRIBE -- sent shortly after connecting, well inside that window --
    // still reliably exercises the deferred-accept path.
    let (origin_thread, origin_stop) = node::run_stoppable(origin_args);
    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(200));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

    write_and_close(pub_w, TEST_LINES);

    let output = wait_for_file_len(&out_path, TEST_LINES.len(), Duration::from_secs(5));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(origin_stop);
    origin_thread.join().unwrap();
    drop(relay_stop);
    relay_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    assert_eq!(output, TEST_LINES, "object delivery must still complete despite the deferred accept");

    fn subscribe_ok_max_cache_duration(v: &serde_json::Value) -> Option<u64> {
        if v.get("name").and_then(|n| n.as_str()) != Some("moqt:control_message_created") { return None; }
        let msg = v.get("data")?.get("message")?;
        if msg.get("type").and_then(|t| t.as_str()) != Some("subscribe_ok") { return None; }
        msg.get("parameters")?.as_array()?.iter()
            .find(|p| p.get("name").and_then(|n| n.as_str()) == Some("max_cache_duration"))
            .and_then(|p| p.get("value"))
            .and_then(|v| v.as_u64())
    }

    let events = qlog_events(qlog_dir.path());
    let sent = events.iter().find_map(subscribe_ok_max_cache_duration);
    assert_eq!(
        sent, Some(MAX_CACHE_DURATION_MS),
        "relay's SUBSCRIBE_OK to the subscriber must carry the origin's real max_cache_duration -- \
         None would mean it answered before its own upstream subscription resolved"
    );
}

/// A `#multi` downstream SUBSCRIBE must be accepted once *any* upstream
/// candidate resolves, not held hostage by one that fails. `candidate_a`
/// announces the namespace but never actually has the track, so the
/// relay's own SUBSCRIBE to it gets rejected (RequestErrorCode::DoesNotExist);
/// `candidate_b` is a real publisher. `local_track_ready`
/// (node/requests.rs) treats the track as ready as soon as one `TrackSubscription`
/// resolves, regardless of how many others are still pending or have
/// already failed -- this is the "any, not all" half of that logic.
#[test]
fn relay_multipath_subscribe_accepted_via_second_candidate_after_first_rejects() {
    let port_a = free_udp_port();
    let port_b = free_udp_port();
    let relay_port = free_udp_port();
    let (pub_r_b, pub_w_b) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "relaymultipath--reject".parse().unwrap();

    // Announces the namespace (so it's a `#multi` candidate) but never
    // publishes the track itself -- any SUBSCRIBE for it is rejected.
    let candidate_a_args = Args {
        listen: Some(port_a.into()),
        publish: vec![announce_ns(&track)],
        ..Default::default()
    };
    let candidate_b_args = Args {
        listen: Some(port_b.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_b, &track, b'\n')],
        ..Default::default()
    };
    let relay_args = Args {
        listen: Some(relay_port.into()),
        connect: vec![
            format!("https://127.0.0.1:{port_a}").parse().unwrap(),
            format!("https://127.0.0.1:{port_b}").parse().unwrap(),
        ],
        subscribe: vec![sub_arg_multi(&track)],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{relay_port}#upstream").parse().unwrap()],
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (candidate_a_thread, candidate_a_stop) = node::run_stoppable(candidate_a_args);
    let (candidate_b_thread, candidate_b_stop) = node::run_stoppable(candidate_b_args);
    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    // Subscriber connects quickly, before the relay's own subscriptions to
    // A/B have necessarily resolved -- exercises the deferred-accept path,
    // not just "both already resolved by the time anyone asked".
    std::thread::sleep(Duration::from_millis(50));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

    write_and_close(pub_w_b, TEST_LINES);

    let output = wait_for_file_len(&out_path, TEST_LINES.len(), Duration::from_secs(5));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(relay_stop);
    relay_thread.join().unwrap();
    drop(candidate_a_stop);
    candidate_a_thread.join().unwrap();
    drop(candidate_b_stop);
    candidate_b_thread.join().unwrap();
    unsafe { libc::close(pub_r_b) };

    assert_eq!(
        output, TEST_LINES,
        "subscriber must still be served via candidate_b despite candidate_a rejecting the relay's upstream subscribe"
    );
}

/// Regression test for a relay panic when forwarding a canceled object.
///
/// Forces origin to RESET_STREAM an object still mid-transfer: `--cache-depth
/// 1` means the second published object evicts the first from origin's own
/// cache, and a tiny `--listen-max-stream-data` window guarantees origin
/// can't have finished sending the (deliberately oversized) first object to
/// the relay before that eviction happens. The relay's subscription sees the
/// reset mid-payload and cancels its own cache entry for that object
/// (`ObjectMeta::cancel`). Before the fix, `cache_to_moq::forward` treated
/// the incomplete object as a normal one ready to send and called
/// `MoqHandle::send_obj_hdr`, which asserts `size > 0` — panicking the whole
/// relay process (this is what crashed `air` in the real multipath testbed:
/// two upstream `#multi` sources racing over the same track cache). The fix
/// (`CachedObjectRef::state` reporting `ObjectState::Canceled`) makes
/// `forward` skip the canceled object instead of trying to send it.
#[test]
fn relay_does_not_panic_forwarding_canceled_object() {
    let origin_port = free_udp_port();
    let relay_port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "cancelrelay--video".parse().unwrap();

    let origin_args = Args {
        listen: Some(origin_port.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        cache_depth: 1,
        ..Default::default()
    };
    let relay_args = Args {
        listen: Some(relay_port.into()),
        connect: vec![format!("https://127.0.0.1:{origin_port}#upstream").parse().unwrap()],
        subscribe: vec!["*".parse().unwrap()],
        // Throttles how much of the (deliberately oversized) first object
        // origin can actually put on the wire before needing more credit —
        // object data flows origin -> relay, so it's the relay's advertised
        // window (as the --connect side) that matters, not origin's.
        connect_max_stream_data: Some(128),
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
    std::thread::sleep(Duration::from_millis(200));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

    // First object: larger than the stream-data window, so origin can't
    // finish sending it to the relay in one go.
    write_only(pub_w, &vec![b'a'; 4000]);
    write_only(pub_w, b"\n");
    std::thread::sleep(Duration::from_micros(300));
    // Second object: triggers eviction of the first.
    write_and_close(pub_w, b"second\n");

    std::thread::sleep(Duration::from_millis(500));

    drop(sub_stop);
    sub_thread.join().unwrap();
    // If the relay panicked forwarding the canceled object, its thread
    // already died and this join returns Err, failing the test.
    drop(relay_stop);
    relay_thread.join().unwrap();
    drop(origin_stop);
    origin_thread.join().unwrap();
    unsafe { libc::close(pub_r); }
}

/// A subscriber joining mid-stream (NextGroupStart) starts at a group start,
/// whose object carries the codec config: its fMP4 output is playable.
#[test]
fn late_subscriber_next_group_start_gets_codec_config() {
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "integration--late-config".parse().unwrap();
    let frame = |i: usize| (format!("frame{i}").into_bytes(), i.is_multiple_of(3));

    let pub_args = Args {
        listen: Some(port.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        subscribe: vec![SubArg { options: quiche_moq::SubscribeOptions { filter: LocationFilter::NextGroupStart, ..SubArg::default_options() }, ..sub_arg(&track) }],
        out: vec![OutArg { track: track.clone(), file: Some(out_path.clone()), output_kind: crate::file_output::OutputKind::fmp4() }],
        ..Default::default()
    };
    let write_frames = |range: std::ops::Range<usize>| {
        for i in range {
            let (payload, sync) = frame(i);
            write_only(pub_w, &crate::fmp4_write::fragment(i as u32, 1, i as u64 * 3000, 3000, sync, &payload));
        }
    };

    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    write_only(pub_w, &crate::fmp4_write::ftyp());
    write_only(pub_w, &crate::fmp4_write::moov(&[(90_000, b"avc1")]));
    // group 0 and the start of group 1, before the subscriber exists
    write_frames(0..4);
    std::thread::sleep(Duration::from_millis(100));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);
    write_frames(4..9);
    unsafe { libc::close(pub_w) };

    let expected: Vec<(Vec<u8>, bool)> = (6..9).map(frame).collect();
    let start = std::time::Instant::now();
    let (demux, received) = loop {
        let mut demux = crate::fmp4_demux::FMp4Demuxer::new();
        demux.push(&std::fs::read(&out_path).unwrap_or_default());
        let received: Vec<(Vec<u8>, bool)> = std::iter::from_fn(|| demux.next_sample()).map(|s| (s.data, s.is_sync)).collect();
        if received.len() >= expected.len() || start.elapsed() > Duration::from_secs(5) {
            break (demux, received);
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    assert!(demux.track.is_some_and(|t| t.codec_config.is_some_and(|c| c.starts_with(b"avc1"))), "no moov: the first object had no codec config");
    assert_eq!(received, expected, "starts at the next group (frame 6)");
}

/// A subscriber joining with `filter=largest-object` misses everything cached
/// before it (draft-16 §5.1.2). Line input has one object per group, so the
/// start within a group is covered by `filter_start_is_relative_to_largest`.
#[test]
fn late_subscriber_largest_object_starts_after_latest() {
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "integration--largest-object".parse().unwrap();

    let pub_args = Args {
        listen: Some(port.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        subscribe: vec![SubArg { options: quiche_moq::SubscribeOptions { filter: quiche_moq::wire::control_message::subscribe::LocationFilter::LargestObject, ..SubArg::default_options() }, ..sub_arg(&track) }],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    write_only(pub_w, b"one\ntwo\nthree\n");
    std::thread::sleep(Duration::from_millis(100));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);
    write_and_close(pub_w, b"four\n");
    wait_for_file_len(&out_path, b"four\n".len(), Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(100)); // nothing more may arrive

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    assert_eq!(std::fs::read(&out_path).unwrap(), b"four\n");
}

/// Publisher's input EOF marks its last (only) group's Subgroup as finished
/// (`LocalTrack::close()`) — the subscriber must drain everything already
/// published and the whole pipeline must shut down cleanly on request, not
/// hang waiting on a stream that (without that fix) would never explicitly
/// FIN.
#[test]
fn publisher_finishes_subscriber_drains_and_exits() {
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "integration--pub-finishes".parse().unwrap();

    let pub_args = Args {
        listen: Some(port.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

    write_and_close(pub_w, TEST_LINES);

    let output = wait_for_file_len(&out_path, TEST_LINES.len(), Duration::from_secs(5));
    assert_eq!(output, TEST_LINES, "subscriber must drain everything published before the publisher's input EOF");

    drop(sub_stop);
    drop(pub_stop);
    let start = std::time::Instant::now();
    while (!sub_thread.is_finished() || !pub_thread.is_finished()) && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(sub_thread.is_finished() && pub_thread.is_finished(), "publisher/subscriber did not exit within 3s of being stopped");

    sub_thread.join().unwrap();
    pub_thread.join().unwrap();
    unsafe { libc::close(pub_r) };
}
