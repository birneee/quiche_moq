//! LOC properties.

use super::*;

/// `--timestamp` must embed a Timestamp property (type 6, per
/// draft-ietf-moq-loc-02 §2.3.1.1) on every published line object, and a
/// real subscriber must be able to read it back out. We verify this through
/// the existing qlog instrumentation rather than a new output format:
/// `InStream::read_next_object_header` (quiche_moq/src/in_stream.rs) already
/// logs each received object's properties to qlog when the `qlog`
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
        listen: Some(port.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        timestamp: true,
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        qlog_dir: Some(qlog_dir.path().to_path_buf()),
        ..Default::default()
    };

    let before_us = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() as u64;

    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

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
        // properties, but what matters is that the application reading the
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
            if h.get("header_type").and_then(|t| t.as_u64()) == Some(moq_loc::TIMESTAMP_ID.get()) {
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

/// Regression test: `push_sample`'s multi-object groups used to leak one
/// stream per Group (retroactive `end_of_subgroup` marking arrived after
/// `CacheToMoq` had already marked the Group `completed`, so it
/// was never FIN'd), eventually exhausting the connection's stream budget
/// and stalling delivery. Publishes more sync-sample Groups than the
/// 100-stream initial budget, paced like a real stream.
#[test]
fn push_sample_groups_do_not_leak_streams() {
    const NUM_GROUPS: usize = 150; // > the 100-stream initial budget
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let qlog_dir = tempfile::tempdir().unwrap();
    let track: NamespaceTrackname = "integration--stream-leak".parse().unwrap();

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
        qlog_dir: Some(qlog_dir.path().to_path_buf()),
        ..Default::default()
    };

    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

    // codec_config = [FourCC(4)][stsd entry body]; empty body is fine.
    write_only(pub_w, &crate::fmp4_write::ftyp());
    write_only(pub_w, &crate::fmp4_write::moov(&[(90_000, b"avc1")]));
    for i in 0..NUM_GROUPS {
        // Every fragment is a sync sample, so each is its own Group.
        let payload = format!("frame{i}").into_bytes();
        let frag = crate::fmp4_write::fragment(i as u32, 1, i as u64 * 1000, 1000, true, &payload);
        write_only(pub_w, &frag);
        std::thread::sleep(Duration::from_millis(8));
    }
    unsafe { libc::close(pub_w) };

    // Wait for delivery via the output file rather than qlog — qlog events
    // may sit unflushed until shutdown, so only inspect qlog after that.
    let expected_len: usize = (0..NUM_GROUPS).map(|i| format!("frame{i}").len()).sum();
    wait_for_file_len(&out_path, expected_len, Duration::from_secs(15));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    let received_groups: std::collections::HashSet<u64> = qlog_events(qlog_dir.path())
        .iter()
        .filter(|v| v.get("name").and_then(|n| n.as_str()) == Some("moqt:subgroup_object_parsed"))
        .filter_map(|v| v.get("data").and_then(|d| d.get("group_id")).and_then(|g| g.as_u64()))
        .collect();
    let missing: Vec<u64> = (0..NUM_GROUPS as u64).filter(|g| !received_groups.contains(g)).collect();
    assert_eq!(received_groups.len(), NUM_GROUPS, "only {}/{NUM_GROUPS} Groups delivered, missing: {missing:?}", received_groups.len());
}
