//! Connections coming and going.

use super::*;

/// A second, independent subscriber connecting after the first disconnects
/// must still be served correctly — proves the first's teardown only tore
/// down its own `CacheToMoq` entry, not the `LocalTrack` itself
/// (its cache, or any other subscriber's state).
#[test]
fn subscriber_disconnect_does_not_affect_publisher() {
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file_a = tempfile::NamedTempFile::new().unwrap();
    let out_path_a = out_file_a.path().to_path_buf();
    let out_file_b = tempfile::NamedTempFile::new().unwrap();
    let out_path_b = out_file_b.path().to_path_buf();
    let track: NamespaceTrackname = "integration--sub-disconnect".parse().unwrap();

    let pub_args = Args {
        listen: Some(port.into()),
        publish: vec![pub_passive_arg(&track), announce_ns(&track)],
        r#in: vec![in_arg(pub_r, &track, b'\n')],
        ..Default::default()
    };
    let sub_args_a = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path_a.clone())],
        ..Default::default()
    };

    let (pub_thread, pub_stop) = node::run_stoppable(pub_args);
    let (sub_a_thread, sub_a_stop) = node::run_stoppable(sub_args_a);
    std::thread::sleep(Duration::from_millis(200));

    write_only(pub_w, b"first\n");
    let _ = wait_for_file_len(&out_path_a, b"first\n".len(), Duration::from_secs(3));

    // Subscriber A disconnects mid-stream — the publisher's input is
    // intentionally left open, since more objects can still arrive.
    drop(sub_a_stop);
    sub_a_thread.join().unwrap();

    let sub_args_b = Args {
        connect: vec![format!("https://127.0.0.1:{port}").parse().unwrap()],
        subscribe: vec![sub_arg(&track)],
        out: vec![out_arg(&track, out_path_b.clone())],
        ..Default::default()
    };
    let (sub_b_thread, sub_b_stop) = node::run_stoppable(sub_args_b);
    std::thread::sleep(Duration::from_millis(200));

    // NextGroupStart (the default filter) means B only sees what's
    // published after it subscribes, which is exactly what's being tested.
    write_and_close(pub_w, b"second\n");
    let output_b = wait_for_file_len(&out_path_b, b"second\n".len(), Duration::from_secs(5));

    drop(sub_b_stop);
    sub_b_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    assert_eq!(output_b, b"second\n", "second subscriber must be served correctly after the first disconnected mid-stream");
}
