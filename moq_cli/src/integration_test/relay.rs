//! Relay behaviour.

use super::*;
use quiche_moq::wire::PublishDoneStatus;

/// Both publisher and subscriber dial *into* the same relay (unlike
/// `unknown_namespace_forwarded_upstream`, which relies on `#upstream`): the
/// relay must learn the namespace purely from the publisher's explicit
/// `--pub <NS>` PUBLISH_NAMESPACE announcement (trusted via its
/// `--sub "*"` wildcard) and route the subscriber's SUBSCRIBE to it
/// via `namespaces`/`relay_subscriptions`, with no upstream fallback
/// configured at all.
#[test]
fn relay_forwards_publisher_to_subscriber() {
    let relay_port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "relay--direct".parse().unwrap();

    // A `--sub "*"` wildcard makes the relay both trust peers'
    // PUBLISH_NAMESPACE announcements and relay-forward subscriptions for
    // namespaces it learns about that way.
    let relay_args = Args {
        listen: Some(relay_port.into()),
        subscribe: vec!["*".parse().unwrap()],
        ..Default::default()
    };
    let pub_args = Args {
        connect: vec![format!("https://127.0.0.1:{relay_port}").parse().unwrap()],
        // The passive track entry only serves the track; namespace
        // announcement is a separate, explicit concern (--pub <NS>).
        publish: vec![pub_passive_arg(&track), "relay".parse().unwrap()],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{relay_port}#upstream").parse().unwrap()],
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
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
    sub_stop.wait_subscribed(&track);

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

/// Over WebTransport and over native QUIC for moqt:// URLs (draft-ietf-moq-transport-18 §3.1.4),
/// objects published in streams or datagrams are relayed alike (§11.3).
#[rstest::rstest]
fn relay_forwards_over_each_transport(#[values(false, true)] native: bool, #[values(false, true)] datagram: bool) {
    let relay_port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "relayed--lines".parse().unwrap();
    // ALPN names the draft from 15, datagrams of this form from 14
    let version: MoqVersionArg = "18".parse().unwrap();
    let scheme = if native { "moqt" } else { "https" };

    let relay_args = Args {
        listen: Some(ListenArg { native, ..relay_port.into() }),
        setup_version: Some(version),
        subscribe: vec!["*".parse().unwrap()],
        ..Default::default()
    };
    let pub_args = Args {
        connect: vec![format!("{scheme}://127.0.0.1:{relay_port}").parse().unwrap()],
        setup_version: Some(version),
        publish: vec![PubArg { datagram, ..pub_passive_arg(&track) }, "relayed".parse().unwrap()],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("{scheme}://127.0.0.1:{relay_port}#upstream").parse().unwrap()],
        setup_version: Some(version),
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(150));
    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    std::thread::sleep(Duration::from_millis(200));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

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

/// The subscriber's SUBSCRIBE arrives before the publisher's announcement:
/// with RENDEZVOUS_TIMEOUT the relay holds it until the publisher appears
/// (draft-ietf-moq-transport-18 §10.2.6), instead of rejecting it.
#[test]
fn relay_holds_subscribe_until_publisher_appears() {
    let relay_port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "rendezvous--late".parse().unwrap();
    // RENDEZVOUS_TIMEOUT from draft 17
    let version: MoqVersionArg = "18".parse().unwrap();

    let relay_args = Args {
        listen: Some(relay_port.into()),
        setup_version: Some(version),
        subscribe: vec!["*".parse().unwrap()],
        ..Default::default()
    };
    let pub_args = Args {
        connect: vec![format!("https://127.0.0.1:{relay_port}").parse().unwrap()],
        setup_version: Some(version),
        publish: vec![pub_passive_arg(&track), "rendezvous".parse().unwrap()],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    let mut sub = sub_arg(&track);
    sub.options.rendezvous_timeout = Some(5000);
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{relay_port}#upstream").parse().unwrap()],
        setup_version: Some(version),
        subscribe: vec![sub],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(150));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    std::thread::sleep(Duration::from_millis(300));
    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    sub_stop.wait_subscribed(&track);

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

/// A subscriber in Forward State 0 alone: the relay subscribes upstream in Forward 0 too,
/// with `--prewarm` in Forward 1 to fill its cache (draft-ietf-moq-transport-19 §9.2).
/// A second subscriber in Forward 1 turns upstream to Forward 1 (draft-ietf-moq-transport-19 §9.5).
#[rstest::rstest]
#[case::paused(false, 0, &[1])]
#[case::prewarm(true, 1, &[])]
fn relay_forwards_upstream_only_if_wanted_or_prewarm(#[case] prewarm: bool, #[case] upstream_forward: u64, #[case] updates: &[u64]) {
    let relay_port = free_udp_port();
    let pub_qlog = tempfile::tempdir().unwrap();
    let track: NamespaceTrackname = "prewarm--track".parse().unwrap();

    let relay_args = Args {
        listen: Some(relay_port.into()),
        subscribe: vec!["*".parse().unwrap()],
        prewarm,
        ..Default::default()
    };
    let pub_args = Args {
        connect: vec![format!("https://127.0.0.1:{relay_port}").parse().unwrap()],
        publish: vec![pub_passive_arg(&track), "prewarm".parse().unwrap()],
        qlog_dir: Some(pub_qlog.path().to_path_buf()),
        ..Default::default()
    };
    let sub_args = |forward: bool| {
        let mut sub = sub_arg(&track);
        sub.options.forward = forward;
        Args {
            connect: vec![format!("https://127.0.0.1:{relay_port}#upstream").parse().unwrap()],
            subscribe: vec![sub],
            ..Default::default()
        }
    };

    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(150));
    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    std::thread::sleep(Duration::from_millis(200));
    let (paused_thread, paused_stop) = node::run_stoppable(sub_args(false));
    paused_stop.wait_subscribed(&track);
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args(true));
    sub_stop.wait_subscribed(&track);
    std::thread::sleep(Duration::from_millis(200));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(paused_stop);
    paused_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    drop(relay_stop);
    relay_thread.join().unwrap();

    let parsed: Vec<serde_json::Value> = control_messages(pub_qlog.path()).into_iter()
        .filter(|(name, _)| name == "moqt:control_message_parsed")
        .map(|(_, m)| m)
        .collect();
    let forwards: Vec<u64> = parsed.iter().filter(|m| m["type"] == "subscribe").map(|m| m["forward"].as_u64().unwrap()).collect();
    assert_eq!(forwards, [upstream_forward]);
    let forward_updates: Vec<u64> = parsed.iter()
        .filter(|m| m["type"] == "request_update")
        .flat_map(|m| m["parameters"].as_array().cloned().unwrap_or_default())
        .filter(|p| p["name"] == "forward")
        .map(|p| p["value"].as_u64().unwrap())
        .collect();
    assert_eq!(forward_updates, updates);
}

/// The publisher has objects before a subscriber starting at Location {0, 0} arrives: the relay
/// subscribes upstream from there, or, subscribed already from the next Group for an earlier
/// subscriber, fetches what is before (draft-ietf-moq-transport-19 §2.3.1).
#[rstest::rstest]
fn relay_delivers_what_starts_before_its_upstream_subscription(#[values(false, true)] subscribed_before: bool) {
    let relay_port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "fill--early".parse().unwrap();
    // FETCH from draft 15
    let version: MoqVersionArg = "18".parse().unwrap();

    let relay_args = Args {
        listen: Some(relay_port.into()),
        setup_version: Some(version),
        subscribe: vec!["*".parse().unwrap()],
        ..Default::default()
    };
    let pub_args = Args {
        setup_version: Some(version),
        connect: vec![format!("https://127.0.0.1:{relay_port}").parse().unwrap()],
        publish: vec![pub_passive_arg(&track), "fill".parse().unwrap()],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    let mut sub = sub_arg(&track);
    sub.options.filter = LocationFilter::AbsoluteStart { start: Location { group: 0, object: 0 } };
    let sub_args = Args {
        setup_version: Some(version),
        connect: vec![format!("https://127.0.0.1:{relay_port}#upstream").parse().unwrap()],
        subscribe: vec![sub],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let earlier_args = Args {
        setup_version: Some(version),
        connect: vec![format!("https://127.0.0.1:{relay_port}#upstream").parse().unwrap()],
        subscribe: vec![sub_arg(&track)],
        ..Default::default()
    };

    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(150));
    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    write_and_close(pub_w, TEST_LINES);
    std::thread::sleep(Duration::from_millis(300));
    let earlier = subscribed_before.then(|| {
        let (thread, stop) = node::run_stoppable(earlier_args);
        stop.wait_subscribed(&track);
        (thread, stop)
    });
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

    std::thread::sleep(Duration::from_millis(600));

    drop(sub_stop);
    sub_thread.join().unwrap();
    if let Some((thread, stop)) = earlier {
        drop(stop);
        thread.join().unwrap();
    }
    drop(pub_stop);
    pub_thread.join().unwrap();
    drop(relay_stop);
    relay_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    let output = std::fs::read(&out_path).unwrap();
    assert_eq!(output, TEST_LINES);
}

/// origin --(PUBLISH_NAMESPACE)--> relay --(#upstream, relay --relay-namespace)--> observer.
/// `relay` learns the namespace solely via its own `#upstream` connection to
/// `origin` (always auto-accepted by `process_upstream_conn`); whether it then
/// re-announces that learned namespace to `observer` is exactly what
/// `--relay-namespace` gates. Propagating it is otherwise
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
        listen: Some(port_a.into()),
        publish: vec![PubArg { target: PubTarget::Namespace(ns.clone()), max_cache_duration: None, passive: false, datagram: false }],
        ..Default::default()
    };

    // Relay: learns the namespace only via #upstream, then re-announces
    // everything it knows (--relay-namespace) to its own accepted connections.
    let relay_args = Args {
        listen: Some(port_b.into()),
        connect: vec![format!("https://127.0.0.1:{port_a}#upstream").parse().unwrap()],
        relay_namespace: vec!["*".parse().unwrap()],
        ..Default::default()
    };

    // Observer: only connection is to relay (also via #upstream, the only
    // role that processes incoming PUBLISH_NAMESPACE today) — qlog-enabled so
    // the test can see what crossed the wire.
    let observer_args = Args {
        connect: vec![format!("https://127.0.0.1:{port_b}#upstream").parse().unwrap()],
        qlog_dir: Some(qlog_dir.path().to_path_buf()),
        ..Default::default()
    };

    let (origin_thread, origin_stop) = node::run_stoppable(origin_args);
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

/// origin → relay (--relay-namespace) → observer. Once the origin is gone
/// the relay withdraws the namespace it re-announced to the observer.
#[test]
fn relay_sends_publish_namespace_done_on_publisher_disconnect() {
    let port_a = free_udp_port();
    let port_b = free_udp_port();
    let qlog_dir = tempfile::tempdir().unwrap();
    let ns: Namespace = "withdraw-ns-test".parse().unwrap();

    let origin_args = Args {
        listen: Some(port_a.into()),
        publish: vec![PubArg { target: PubTarget::Namespace(ns.clone()), max_cache_duration: None, passive: false, datagram: false }],
        ..Default::default()
    };
    let relay_args = Args {
        listen: Some(port_b.into()),
        connect: vec![format!("https://127.0.0.1:{port_a}#upstream").parse().unwrap()],
        relay_namespace: vec!["*".parse().unwrap()],
        ..Default::default()
    };
    let observer_args = Args {
        connect: vec![format!("https://127.0.0.1:{port_b}#upstream").parse().unwrap()],
        qlog_dir: Some(qlog_dir.path().to_path_buf()),
        ..Default::default()
    };

    let (origin_thread, origin_stop) = node::run_stoppable(origin_args);
    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(250));
    let (observer_thread, observer_stop) = node::run_stoppable(observer_args);
    std::thread::sleep(Duration::from_millis(400));
    drop(origin_stop);
    origin_thread.join().unwrap();
    std::thread::sleep(Duration::from_millis(400));

    drop(observer_stop);
    observer_thread.join().unwrap();
    drop(relay_stop);
    relay_thread.join().unwrap();

    let parsed: Vec<String> = control_messages(qlog_dir.path()).into_iter()
        .filter(|(name, _)| name == "moqt:control_message_parsed")
        .filter_map(|(_, m)| m["type"].as_str().map(str::to_owned))
        .collect();
    assert!(parsed.iter().any(|t| t == "publish_namespace"), "{parsed:?}");
    assert!(parsed.iter().any(|t| t == "publish_namespace_done"), "{parsed:?}");
}

/// publisher → relay ← subscriber, then `leaving` disconnects. Returns the
/// relay's and the subscriber's control messages.
fn relay_session_where_one_side_leaves(track: &NamespaceTrackname, publisher_leaves: bool) -> (ControlMessages, ControlMessages) {
    let relay_port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let relay_qlog = tempfile::tempdir().unwrap();
    let sub_qlog = tempfile::tempdir().unwrap();

    let relay_args = Args {
        listen: Some(relay_port.into()),
        subscribe: vec!["*".parse().unwrap()],
        qlog_dir: Some(relay_qlog.path().to_path_buf()),
        ..Default::default()
    };
    let pub_args = Args {
        connect: vec![format!("https://127.0.0.1:{relay_port}").parse().unwrap()],
        publish: vec![pub_passive_arg(track), announce_ns(track)],
        r#in: vec![in_arg(pub_r, track, b'\n')],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{relay_port}#upstream").parse().unwrap()],
        subscribe: vec![sub_arg(track)],
        out: vec![out_arg(track, out_path.clone())],
        qlog_dir: Some(sub_qlog.path().to_path_buf()),
        ..Default::default()
    };

    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(150));
    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    std::thread::sleep(Duration::from_millis(200));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    std::thread::sleep(Duration::from_millis(200));
    write_only(pub_w, TEST_LINES);
    let output = wait_for_file_len(&out_path, TEST_LINES.len(), Duration::from_secs(5));
    assert_eq!(output, TEST_LINES);

    let (leaving, leaving_stop, staying, staying_stop) = if publisher_leaves {
        (pub_thread, pub_stop, sub_thread, sub_stop)
    } else {
        (sub_thread, sub_stop, pub_thread, pub_stop)
    };
    drop(leaving_stop);
    leaving.join().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    drop(staying_stop);
    staying.join().unwrap();
    drop(relay_stop);
    relay_thread.join().unwrap();
    unsafe { libc::close(pub_r); libc::close(pub_w) };
    (control_messages(relay_qlog.path()), control_messages(sub_qlog.path()))
}

/// The relayed publisher disconnects: the subscriber gets PUBLISH_DONE
/// TRACK_ENDED, not a silent hang.
#[test]
fn relay_sends_publish_done_to_subscriber_on_publisher_disconnect() {
    let track: NamespaceTrackname = "relaydone--track".parse().unwrap();
    let (_, sub) = relay_session_where_one_side_leaves(&track, true);
    let done = sub.iter()
        .find(|(name, m)| name == "moqt:control_message_parsed" && m["type"] == "publish_done")
        .map(|(_, m)| m.clone());
    let done = done.expect("subscriber received no PUBLISH_DONE");
    assert_eq!(done["status_code"], u64::from(PublishDoneStatus::TrackEnded));
    assert!(done["stream_count"].as_u64().unwrap() > 0, "streams were opened: {done}");
}

/// The last subscriber of a relayed track disconnects: the relay
/// unsubscribes upstream instead of receiving the track forever.
#[test]
fn relay_unsubscribes_upstream_when_last_subscriber_leaves() {
    let track: NamespaceTrackname = "relayunsub--track".parse().unwrap();
    let (relay, _) = relay_session_where_one_side_leaves(&track, false);
    let unsubscribes = relay.iter()
        .filter(|(name, m)| name == "moqt:control_message_created" && m["type"] == "unsubscribe")
        .count();
    assert_eq!(unsubscribes, 1);
}

/// origin ← relayA (#upstream) ← relayB (#upstream) ← subscriber. Two hops
/// of relay-forwarding: relayA learns the namespace/track from origin and
/// re-announces it (--relay-namespace) so relayB can learn it transitively;
/// relayB forwards the final subscriber's SUBSCRIBE upstream through relayA
/// to origin, and objects must flow all the way back down both hops.
/// `relay_forwards_publisher_to_subscriber` already covers the single-hop
/// case — this exercises `RecvObjHdr::SubgroupFinished` propagating through
/// an intermediate relay rather than terminating at the first hop.
#[test]
fn relay_chains_through_upstream() {
    let origin_port = free_udp_port();
    let relay_a_port = free_udp_port();
    let relay_b_port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "relaychain--video".parse().unwrap();

    let origin_args = Args {
        listen: Some(origin_port.into()),
        publish: vec![pub_passive_arg(&track), "relaychain".parse().unwrap()],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    let relay_a_args = Args {
        listen: Some(relay_a_port.into()),
        connect: vec![format!("https://127.0.0.1:{origin_port}#upstream").parse().unwrap()],
        relay_namespace: vec!["*".parse().unwrap()],
        subscribe: vec!["*".parse().unwrap()],
        ..Default::default()
    };
    let relay_b_args = Args {
        listen: Some(relay_b_port.into()),
        connect: vec![format!("https://127.0.0.1:{relay_a_port}#upstream").parse().unwrap()],
        subscribe: vec!["*".parse().unwrap()],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{relay_b_port}#upstream").parse().unwrap()],
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (origin_thread, origin_stop) = node::run_stoppable(origin_args);
    let (relay_a_thread, relay_a_stop) = node::run_stoppable(relay_a_args);
    std::thread::sleep(Duration::from_millis(250));
    let (relay_b_thread, relay_b_stop) = node::run_stoppable(relay_b_args);
    std::thread::sleep(Duration::from_millis(400));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);

    write_and_close(pub_w, TEST_LINES);

    let output = wait_for_file_len(&out_path, TEST_LINES.len(), Duration::from_secs(5));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(relay_b_stop);
    relay_b_thread.join().unwrap();
    drop(relay_a_stop);
    relay_a_thread.join().unwrap();
    drop(origin_stop);
    origin_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    assert_eq!(output, TEST_LINES, "objects must flow through both relay hops to the final subscriber");
}

/// A relay must drop an object once it's older than the track's
/// `max_cache_duration`, instead of forwarding it late (draft-16 §11.1.1:
/// "the relay MUST NOT start forwarding" an expired object). This also
/// pins down that the relay's downstream `OutTrack` actually has the real
/// `max_cache_duration` to check against, which depends on
/// `relay_subscribe_ok_deferred_until_upstream_resolves_carries_max_cache_duration`'s
/// fix: before that fix, a relay's own upstream subscription could still be
/// unresolved when it answered a downstream SUBSCRIBE, silently locking
/// that downstream `OutTrack` to `max_cache_duration: None` (never expire)
/// forever.
///
/// Uses `UdpDelayProxy` to add real latency to the relay<->subscriber
/// handshake (QUIC + WebTransport + MoQT SETUP/SUBSCRIBE, several real
/// round trips) rather than throttling flow control: relay caches the
/// first object from origin (an *undelayed* connection) almost
/// immediately, but the delayed subscriber connection takes long enough to
/// actually complete that the object is already stale by the time relay
/// gets a subscriber to forward it to *at all* — so `send_obj_hdr`'s
/// `check_not_expired` rejects it on the very first attempt, no race
/// against already-sent bytes (a RESET_STREAM can't outrun data already
/// queued ahead of it on a constant-delay path, which is what made a
/// flow-control-throttling version of this test unreliable).
#[test]
fn relay_honors_max_cache_duration_object_expiry() {
    let origin_port = free_udp_port();
    let relay_port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "expire--mcd".parse().unwrap();
    // Comfortably shorter than the ~250-300ms the proxy-delayed handshake
    // takes (5 round trips x 2x25ms), so the first object is unambiguously
    // stale by the time relay gets a subscriber to forward it to at all.
    const MAX_CACHE_DURATION_MS: u64 = 80;
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
    // An *active* --sub (not the usual passive --sub "*" wildcard) so relay
    // caches the first object from origin immediately at startup,
    // independent of whether/when the (deliberately slow) downstream
    // subscriber ever connects.
    let relay_args = Args {
        listen: Some(relay_port.into()),
        setup_version: Some(version),
        connect: vec![format!("https://127.0.0.1:{origin_port}#upstream").parse().unwrap()],
        subscribe: vec![sub_arg(&track)],
        ..Default::default()
    };

    let (origin_thread, origin_stop) = node::run_stoppable(origin_args);
    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(200));

    // First object: relay (already connected+subscribed to origin) caches
    // this almost immediately -- well before the subscriber below even
    // starts its (slow) handshake.
    write_only(pub_w, &[b'a'; 6]);
    write_only(pub_w, b"\n");
    std::thread::sleep(Duration::from_millis(50));

    let proxy = UdpDelayProxy::start(relay_port, Duration::from_millis(25));
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{}#upstream", proxy.port()).parse().unwrap()],
        setup_version: Some(version),
        // from the start, else the default filter skips the stale object anyway
        subscribe: vec![SubArg { options: quiche_moq::SubscribeOptions { filter: LocationFilter::AbsoluteStart { start: Location { group: 0, object: 0 } }, ..SubArg::default_options() }, ..sub_arg(&track) }],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);

    // Long enough for the delayed handshake to fully complete (comfortably
    // more than the ~250-300ms it needs) -- by the time relay gets this
    // subscriber's alias, well over MAX_CACHE_DURATION_MS has passed since
    // the first object was cached.
    std::thread::sleep(Duration::from_millis(600));

    // Second, small object -- fresh relative to *now*, so even with the
    // same 25ms one-way delay it arrives comfortably inside
    // MAX_CACHE_DURATION_MS of its own (much more recent) received_at.
    write_and_close(pub_w, b"second\n");

    let output = wait_for_file_len(&out_path, b"second\n".len(), Duration::from_secs(5));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(proxy);
    drop(relay_stop);
    relay_thread.join().unwrap();
    drop(origin_stop);
    origin_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    assert_eq!(
        output, b"second\n",
        "the stale first object (cached long before the deliberately slow subscriber handshake even \
         completed) must be dropped, not forwarded late, while the fresh second object still gets through"
    );
}

/// A subscriber's priority applies downstream only (draft-19 §7.3: relays
/// SHOULD NOT reuse it upstream), its effect on the egress streams is
/// covered by `moq_core`'s `subscriber_priority_applies_per_egress_track`.
#[test]
fn relay_keeps_subscriber_priority_downstream() {
    let relay_port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let relay_qlog = tempfile::tempdir().unwrap();
    let track: NamespaceTrackname = "relayprio--track".parse().unwrap();

    let relay_args = Args {
        listen: Some(relay_port.into()),
        subscribe: vec!["*".parse().unwrap()],
        qlog_dir: Some(relay_qlog.path().to_path_buf()),
        ..Default::default()
    };
    let pub_args = Args {
        connect: vec![format!("https://127.0.0.1:{relay_port}").parse().unwrap()],
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    let sub_args = Args {
        connect: vec![format!("https://127.0.0.1:{relay_port}#upstream").parse().unwrap()],
        subscribe: vec![SubArg { options: quiche_moq::SubscribeOptions { priority: 5, ..SubArg::default_options() }, ..sub_arg(&track) }],
        out: vec![out_arg(&track, out_path.clone())],
        ..Default::default()
    };

    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(150));
    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    std::thread::sleep(Duration::from_millis(200));
    let (sub_thread, sub_stop) = node::run_stoppable(sub_args);
    sub_stop.wait_subscribed(&track);
    write_and_close(pub_w, TEST_LINES);
    let output = wait_for_file_len(&out_path, TEST_LINES.len(), Duration::from_secs(5));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    drop(relay_stop);
    relay_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    assert_eq!(output, TEST_LINES);
    let priority = |direction: &str| control_messages(relay_qlog.path()).into_iter()
        .find(|(name, m)| name == direction && m["type"] == "subscribe")
        .map(|(_, m)| m["subscriber_priority"].as_u64().unwrap());
    assert_eq!(priority("moqt:control_message_parsed"), Some(5), "from the subscriber");
    assert_eq!(priority("moqt:control_message_created"), Some(moq_wire::DEFAULT_SUBSCRIBER_PRIORITY as u64), "to the publisher");
}
