//! `#race` subscriptions.

use super::*;

/// `#race` promotion is trigger-only (`SigAction::RaceProbe`,
/// `crate::race::trigger_race`), no background polling.
/// - trigger flips every non-leader candidate to Forward=1, `AbsoluteStart`
///   one past the leader's last delivery (`{0,0}` if none), leader stays Forward=1
/// - first candidate to prove real content wins
/// - here `pub_a` (leader) never publishes, so `{0,0}` pulls pub_b's entire backlog
#[test]
fn relay_race_switches_to_the_candidate_that_actually_has_content() {
    race_mode_delivers_from_caught_up_standby(RaceMode::Data);
}

/// Leader (`pub_a`) empty, standby (`pub_b`) has content.
/// - `Data`: the standby wins the race
/// - `Keep`: forwarding must move to the standby
/// - `Switch`: the standby must keep the track
fn race_mode_delivers_from_caught_up_standby(mode: RaceMode) {
    let port_a = free_udp_port();
    let port_b = free_udp_port();
    let relay_port = free_udp_port();
    let (pub_r_b, pub_w_b) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "race--video".parse().unwrap();
    let all_lines: &[u8] = b"hello\nworld\nmoq\n";
    // #race needs draft-15+ REQUEST_UPDATE features
    let version: MoqVersionArg = "16".parse().unwrap();

    let pub_args_a = Args {
        listen: Some(port_a.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        setup_version: Some(version),
        ..Default::default()
    };
    let pub_args_b = Args {
        listen: Some(port_b.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_b, &track, b'\n')],
        setup_version: Some(version),
        ..Default::default()
    };
    let relay_args = Args {
        listen: Some(relay_port.into()),
        connect: vec![
            format!("https://127.0.0.1:{port_a}").parse().unwrap(),
            format!("https://127.0.0.1:{port_b}").parse().unwrap(),
        ],
        subscribe: vec![SubArg { race_mode: mode, ..sub_arg_race(&track) }],
        out: vec![out_arg(&track, out_path.clone())],
        setup_version: Some(version),
        sigusr1: vec![SigAction::RaceProbe],
        ..Default::default()
    };

    let (pub_a_thread, pub_a_stop) = node::run_stoppable(pub_args_a);
    let (pub_b_thread, pub_b_stop) = node::run_stoppable(pub_args_b);
    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(250)); // let pub_b's persistent candidate subscription establish

    write_and_close(pub_w_b, all_lines);
    std::thread::sleep(Duration::from_millis(150)); // let pub_b actually cache the content
    signal_node_thread(&relay_thread, libc::SIGUSR1);

    let output = wait_for_file_len(&out_path, all_lines.len(), Duration::from_secs(10));

    drop(relay_stop);
    relay_thread.join().unwrap();
    drop(pub_a_stop);
    pub_a_thread.join().unwrap();
    drop(pub_b_stop);
    pub_b_thread.join().unwrap();
    unsafe { libc::close(pub_r_b); }

    assert_eq!(output, all_lines, "{mode:?} race must end up forwarding from the caught-up candidate");
}

#[test]
fn relay_race_keep_switches_to_caught_up_standby() {
    race_mode_delivers_from_caught_up_standby(RaceMode::Keep);
}

#[test]
fn relay_race_switch_keeps_caught_up_standby() {
    race_mode_delivers_from_caught_up_standby(RaceMode::Switch);
}

/// Promotion following a leader that *did* deliver content re-anchors the
/// winner with `AbsoluteStart` at the first object not yet fully received,
/// not `LargestObject` — so a backlog already cached on a non-forwarding
/// candidate gets delivered, not skipped.
/// - `pub_a` (leader) delivers one line first, so the cache is not empty
///   (the empty case is covered by
///   `relay_race_switches_to_the_candidate_that_actually_has_content`)
/// - `pub_b` then caches two more lines, undelivered, before promotion —
///   both must show up after, not just content published post-promotion
#[test]
fn relay_race_promotion_backfills_gap_from_winner_cache() {
    let port_a = free_udp_port();
    let port_b = free_udp_port();
    let relay_port = free_udp_port();
    let (pub_r_a, pub_w_a) = make_pipe();
    let (pub_r_b, pub_w_b) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "racebackfill--video".parse().unwrap();
    let version: MoqVersionArg = "16".parse().unwrap();

    let pub_args_a = Args {
        listen: Some(port_a.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_a, &track, b'\n')],
        setup_version: Some(version),
        ..Default::default()
    };
    let pub_args_b = Args {
        listen: Some(port_b.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_b, &track, b'\n')],
        setup_version: Some(version),
        ..Default::default()
    };
    let relay_args = Args {
        listen: Some(relay_port.into()),
        connect: vec![
            format!("https://127.0.0.1:{port_a}").parse().unwrap(),
            format!("https://127.0.0.1:{port_b}").parse().unwrap(),
        ],
        subscribe: vec![sub_arg_race(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        setup_version: Some(version),
        sigusr1: vec![SigAction::RaceProbe],
        ..Default::default()
    };

    let (pub_a_thread, pub_a_stop) = node::run_stoppable(pub_args_a);
    let (pub_b_thread, pub_b_stop) = node::run_stoppable(pub_args_b);
    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    std::thread::sleep(Duration::from_millis(250));

    // pub_a/pub_b each number lines independently (`LocalTrack::push_line`);
    // "l0" goes to both to keep Group numbers aligned like real race
    // mirrors of the same upstream. pub_a (leader) delivers first, to
    // put content in the cache to backfill after.
    write_only(pub_w_a, b"l0\n");
    write_only(pub_w_b, b"l0\n");
    wait_for_file_len(&out_path, b"l0\n".len(), Duration::from_secs(10));

    // pub_b gets two more lines *before* the race is triggered — both sit
    // cached-but-undelivered (Forward=0) on its persistent candidate
    // subscription.
    write_and_close(pub_w_b, b"l1\nl2\n");
    std::thread::sleep(Duration::from_millis(150)); // let pub_b actually cache them
    signal_node_thread(&relay_thread, libc::SIGUSR1);

    let output = wait_for_file_len(&out_path, b"l0\nl1\nl2\n".len(), Duration::from_secs(10));

    drop(relay_stop);
    relay_thread.join().unwrap();
    drop(pub_a_stop);
    pub_a_thread.join().unwrap();
    drop(pub_b_stop);
    pub_b_thread.join().unwrap();
    unsafe { libc::close(pub_r_a); libc::close(pub_r_b); }

    assert_eq!(output, b"l0\nl1\nl2\n", "promotion must backfill from the winner's own cache, not skip to the live edge");
}

/// Regression test: a candidate disconnecting must not stall a later race
/// for whoever's left.
/// - `pub_b`: disconnects before publishing, exercises `cleanup_closed_conns`
///   clearing it from `race.candidates`/`race.established`
/// - `pub_c`: gets content, must still win a `SigAction::RaceProbe`-triggered
///   race afterward — if stale bookkeeping blocked `trigger_race`/
///   `poll_active_race`, `pub_c` never promotes and the test times out
///
/// The leader (`pub_a`) disconnecting hands the track to the established
/// standby (`pub_b`) without a race being triggered.
#[test]
fn relay_race_standby_replaces_lost_leader() {
    let port_a = free_udp_port();
    let port_b = free_udp_port();
    let relay_port = free_udp_port();
    let (pub_r_b, pub_w_b) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "raceleaderloss--video".parse().unwrap();
    let all_lines: &[u8] = b"hello\nworld\n";
    let version: MoqVersionArg = "16".parse().unwrap();

    let pub_args_a = Args {
        listen: Some(port_a.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        setup_version: Some(version),
        ..Default::default()
    };
    let pub_args_b = Args {
        listen: Some(port_b.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_b, &track, b'\n')],
        setup_version: Some(version),
        ..Default::default()
    };
    let relay_args = Args {
        listen: Some(relay_port.into()),
        connect: vec![
            format!("https://127.0.0.1:{port_a}").parse().unwrap(),
            format!("https://127.0.0.1:{port_b}").parse().unwrap(),
        ],
        subscribe: vec![sub_arg_race(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        setup_version: Some(version),
        ..Default::default()
    };

    let (pub_a_thread, pub_a_stop) = node::run_stoppable(pub_args_a);
    let (pub_b_thread, pub_b_stop) = node::run_stoppable(pub_args_b);
    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    // long enough for the leader and the standby subscription
    std::thread::sleep(Duration::from_millis(500));

    drop(pub_a_stop);
    pub_a_thread.join().unwrap();
    std::thread::sleep(Duration::from_millis(200)); // let the relay observe the disconnect
    write_and_close(pub_w_b, all_lines);

    let output = wait_for_file_len(&out_path, all_lines.len(), Duration::from_secs(5));

    drop(relay_stop);
    relay_thread.join().unwrap();
    drop(pub_b_stop);
    pub_b_thread.join().unwrap();
    unsafe { libc::close(pub_r_b); }

    assert_eq!(output, all_lines);
}

#[test]
fn relay_race_survives_candidate_disconnect() {
    let port_a = free_udp_port();
    let port_b = free_udp_port();
    let port_c = free_udp_port();
    let relay_port = free_udp_port();
    let (pub_r_c, pub_w_c) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "racedisconnect--video".parse().unwrap();
    let all_lines: &[u8] = b"hello\nworld\nmoq\n";
    let version: MoqVersionArg = "16".parse().unwrap();

    let pub_args_a = Args {
        listen: Some(port_a.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        setup_version: Some(version),
        ..Default::default()
    };
    let pub_args_b = Args {
        listen: Some(port_b.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        setup_version: Some(version),
        ..Default::default()
    };
    let pub_args_c = Args {
        listen: Some(port_c.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_c, &track, b'\n')],
        setup_version: Some(version),
        ..Default::default()
    };
    let relay_args = Args {
        listen: Some(relay_port.into()),
        connect: vec![
            format!("https://127.0.0.1:{port_a}").parse().unwrap(),
            format!("https://127.0.0.1:{port_b}").parse().unwrap(),
            format!("https://127.0.0.1:{port_c}").parse().unwrap(),
        ],
        subscribe: vec![sub_arg_race(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        setup_version: Some(version),
        sigusr1: vec![SigAction::RaceProbe],
        ..Default::default()
    };

    let (pub_a_thread, pub_a_stop) = node::run_stoppable(pub_args_a);
    let (pub_b_thread, pub_b_stop) = node::run_stoppable(pub_args_b);
    let (pub_c_thread, pub_c_stop) = node::run_stoppable(pub_args_c);
    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    // Long enough for the relay to establish candidate subscriptions to all three.
    std::thread::sleep(Duration::from_millis(500));

    drop(pub_b_stop);
    pub_b_thread.join().unwrap();
    std::thread::sleep(Duration::from_millis(200)); // let the relay observe the disconnect

    write_and_close(pub_w_c, all_lines);
    std::thread::sleep(Duration::from_millis(150)); // let pub_c actually cache the content
    signal_node_thread(&relay_thread, libc::SIGUSR1);

    let output = wait_for_file_len(&out_path, all_lines.len(), Duration::from_secs(10));

    drop(relay_stop);
    relay_thread.join().unwrap();
    drop(pub_a_stop);
    pub_a_thread.join().unwrap();
    drop(pub_c_stop);
    pub_c_thread.join().unwrap();
    unsafe { libc::close(pub_r_c); }

    assert_eq!(output, all_lines, "relay must still race/promote the remaining candidates after one disconnects");
}

/// `trigger_race` generalizes to any number of established candidates.
/// - `pub_b`: stays connected but never publishes — its `REQUEST_UPDATE_OK`
///   comes back with no usable Location, exercising `poll_active_race`'s
///   "prove not ready, demote immediately" path, not the "connection gone" one
/// - `pub_c`: publishes, its content must arrive
#[test]
fn relay_race_promotes_the_one_candidate_with_content_among_several() {
    let port_a = free_udp_port();
    let port_b = free_udp_port();
    let port_c = free_udp_port();
    let relay_port = free_udp_port();
    let (pub_r_c, pub_w_c) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "racemulti--video".parse().unwrap();
    let all_lines: &[u8] = b"cwin\n";
    let version: MoqVersionArg = "16".parse().unwrap();

    let pub_args_a = Args {
        listen: Some(port_a.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        setup_version: Some(version),
        ..Default::default()
    };
    let pub_args_b = Args {
        listen: Some(port_b.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        setup_version: Some(version),
        ..Default::default()
    };
    let pub_args_c = Args {
        listen: Some(port_c.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r_c, &track, b'\n')],
        setup_version: Some(version),
        ..Default::default()
    };
    let relay_args = Args {
        listen: Some(relay_port.into()),
        connect: vec![
            format!("https://127.0.0.1:{port_a}").parse().unwrap(),
            format!("https://127.0.0.1:{port_b}").parse().unwrap(),
            format!("https://127.0.0.1:{port_c}").parse().unwrap(),
        ],
        subscribe: vec![sub_arg_race(&track)],
        out: vec![out_arg(&track, out_path.clone())],
        setup_version: Some(version),
        sigusr1: vec![SigAction::RaceProbe],
        ..Default::default()
    };

    let (pub_a_thread, pub_a_stop) = node::run_stoppable(pub_args_a);
    let (pub_b_thread, pub_b_stop) = node::run_stoppable(pub_args_b);
    let (pub_c_thread, pub_c_stop) = node::run_stoppable(pub_args_c);
    let (relay_thread, relay_stop) = node::run_stoppable(relay_args);
    // Long enough for the relay to establish all candidate subscriptions.
    std::thread::sleep(Duration::from_millis(500));

    write_and_close(pub_w_c, all_lines);
    std::thread::sleep(Duration::from_millis(150)); // let pub_c actually cache it
    signal_node_thread(&relay_thread, libc::SIGUSR1);

    let output = wait_for_file_len(&out_path, all_lines.len(), Duration::from_secs(10));

    drop(relay_stop);
    relay_thread.join().unwrap();
    drop(pub_a_stop);
    pub_a_thread.join().unwrap();
    drop(pub_b_stop);
    pub_b_thread.join().unwrap();
    drop(pub_c_stop);
    pub_c_thread.join().unwrap();
    unsafe { libc::close(pub_r_c); }

    assert_eq!(output, all_lines, "relay must promote the one candidate that actually has content, among several racing");
}
