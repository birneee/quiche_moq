//! End-to-end tests that exercise multiple `moq_cli` nodes wired together over
//! real QUIC/WebTransport connections (`node::run_stoppable`). Unit tests for
//! individual components live alongside that component's own source file
//! (e.g. `args/mod.rs`, `fmp4.rs`) instead.

use super::*;
use crate::args::{ListenArg, MoqVersionArg, RaceMode};
use crate::node;
use crate::udp_delay_proxy::UdpDelayProxy;
use moq_wire::control_message::subscribe::LocationFilter;
use moq_wire::{Location, Namespace, NamespaceTrackname};
use rstest::rstest;
use std::path::Path;
use std::time::Duration;

const TEST_LINES: &[u8] = b"hello\nworld\nmoq\n";

// ── Shared test helpers ───────────────────────────────────────────────────────

/// A UDP port no other test gets.
/// - below the ephemeral range, so no bind to port 0 takes it either
/// - a range of 1000 per process, concurrent test runs don't share ports
fn free_udp_port() -> u16 {
    static NEXT_PORT: std::sync::LazyLock<std::sync::atomic::AtomicU16> = std::sync::LazyLock::new(|| {
        std::sync::atomic::AtomicU16::new(20_000 + (std::process::id() % 12) as u16 * 1000)
    });
    loop {
        let port = NEXT_PORT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if std::net::UdpSocket::bind(("0.0.0.0", port)).is_ok() {
            return port;
        }
    }
}

// TODO relays, multi-source subscribers and handshake-only tests still sleep a
//  fixed time, wait on `NodeStatus` there too

/// An OS pipe, used as publisher input so MIO fires READABLE events (a plain
/// in-memory buffer wouldn't wake the event loop).
fn make_pipe() -> (i32, i32) {
    let mut fds = [0i32; 2];
    unsafe { libc::pipe(fds.as_mut_ptr()) };
    (fds[0], fds[1])
}

fn write_only(fd: i32, data: &[u8]) {
    unsafe {
        libc::write(fd, data.as_ptr() as *const _, data.len());
    }
}

fn write_and_close(fd: i32, data: &[u8]) {
    write_only(fd, data);
    unsafe { libc::close(fd) };
}

/// Polls `path` until its contents reach `expected_len` bytes or `timeout`
/// elapses — more robust than a fixed sleep when the whole suite runs at
/// full parallelism and real delivery can take longer than usual.
fn wait_for_file_len(path: &Path, expected_len: usize, timeout: Duration) -> Vec<u8> {
    let start = std::time::Instant::now();
    loop {
        let data = std::fs::read(path).unwrap_or_default();
        if data.len() >= expected_len || start.elapsed() >= timeout {
            return data;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A `--sub` entry for `track`, the common case (SUBSCRIBE with default options).
fn sub_arg(track: &NamespaceTrackname) -> SubArg {
    SubArg { track: TrackPattern::Exact(track.clone()), options: SubArg::default_options(), passive: false, multi: false, race: false, race_mode: RaceMode::default() }
}

/// A `--sub#multi` entry: subscribes via every currently-known candidate
/// connection instead of just one.
fn sub_arg_multi(track: &NamespaceTrackname) -> SubArg {
    SubArg { multi: true, ..sub_arg(track) }
}

/// A `--out` entry draining `track`'s cache to `file`, the common case.
fn out_arg(track: &NamespaceTrackname, file: std::path::PathBuf) -> OutArg {
    OutArg { track: track.clone(), file: Some(file), output_kind: crate::file_output::OutputKind::auto(b'\n') }
}

/// A passive `--pub` entry (standby track buffer, fed via a companion `--in`).
fn pub_passive_arg(track: &NamespaceTrackname) -> PubArg {
    PubArg { max_cache_duration: None, passive: true, datagram: false, target: PubTarget::Track(TrackPattern::Exact(track.clone())) }
}

/// A `--in` entry feeding `track`'s cache from `fd`.
fn in_arg(fd: i32, track: &NamespaceTrackname, delimiter: u8) -> InArg {
    InArg { track: track.clone(), file: Some(format!("/dev/fd/{fd}").into()), delimiter }
}

/// A bare `--pub <NS>` announcement for `track`'s namespace — needed since
/// `--sub` only picks candidates from announcers or `#upstream`.
fn announce_ns(track: &NamespaceTrackname) -> PubArg {
    PubArg { max_cache_duration: None, passive: false, datagram: false, target: PubTarget::Namespace(track.namespace().clone()) }
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

/// A signal reaches every node of this test process: a test raising one holds this while its nodes run,
/// so no other test's node takes its `--sigusr1` actions.
fn signal_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn raise(sig: libc::c_int) {
    unsafe { libc::raise(sig) };
}

/// `name` and `type` of every control message in `dir`'s qlogs.
fn control_messages(dir: &Path) -> ControlMessages {
    qlog_events(dir).into_iter()
        .filter(|e| e["name"] == "moqt:control_message_created" || e["name"] == "moqt:control_message_parsed")
        .map(|e| (e["name"].as_str().unwrap().to_owned(), e["data"]["message"].clone()))
        .collect()
}

/// A qlog's control messages: their type and the message.
type ControlMessages = Vec<(String, serde_json::Value)>;

mod basics;
mod pub_sub;
mod race;
mod cache;
mod loc;
mod routing;
mod connection;
mod relay;
mod signals;
mod cli;
