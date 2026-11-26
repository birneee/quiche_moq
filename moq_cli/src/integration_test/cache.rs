//! The object cache.

use super::*;

/// An object larger than `--cache-max-obj` is dropped, the ones around it still arrive.
#[test]
fn oversized_object_rejected() {
    let port = free_udp_port();
    let (pub_r, pub_w) = make_pipe();
    let out_file = tempfile::NamedTempFile::new().unwrap();
    let out_path = out_file.path().to_path_buf();
    let track: NamespaceTrackname = "integration--oversized".parse().unwrap();

    let pub_args = Args {
        listen: Some(port.into()),
        cache_max_obj: 16,
        cache_avg_obj: 16,
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
    let oversized = [b'x'; 64];
    write_and_close(pub_w, &[b"before\n", &oversized[..], b"\nafter\n"].concat());
    let output = wait_for_file_len(&out_path, b"before\nafter\n".len(), Duration::from_secs(5));

    drop(sub_stop);
    sub_thread.join().unwrap();
    drop(pub_stop);
    pub_thread.join().unwrap();
    unsafe { libc::close(pub_r) };

    assert_eq!(output, b"before\nafter\n");
}
