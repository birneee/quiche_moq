//! Test-only helper: a userspace UDP relay that adds a fixed one-way delay
//! to packets forwarded toward a target port, so integration tests can
//! simulate real network latency between two real `moq_cli` nodes without
//! root/network-namespace tricks (unlike a testbed
//! using `netem` on virtual interfaces).

use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Bind a node's `--connect` at `proxy.port()` instead of the real target's
/// port; the proxy learns the peer's address from its first received
/// packet, so exactly one peer can use a given proxy instance. Every packet,
/// in both directions, is held and released after `delay` — a symmetric
/// added one-way latency, same as real network RTT.
///
/// This makes the whole multi-round-trip handshake (QUIC + WebTransport +
/// MoQT SETUP/SUBSCRIBE) take real, controllable wall-clock time, without
/// touching flow-control windows at all. That's a more reliable way to
/// test `max_cache_duration` expiry than throttling bandwidth: starving a
/// stream/connection window just delays *delivery* of bytes already handed
/// to the transport, so a subsequent RESET_STREAM (itself delayed the same
/// way) can never outrun data already in flight on a constant-delay path.
/// Delaying the *handshake* instead means the object is already stale by
/// the time the relay ever gets a subscriber to forward it to at all —
/// `send_obj_hdr`'s `check_not_expired` rejects it on the very first
/// attempt, no race against already-sent bytes involved.
pub(crate) struct UdpDelayProxy {
    port: u16,
    stop: Arc<AtomicBool>,
    rebind: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn bind() -> UdpSocket {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_read_timeout(Some(Duration::from_millis(1))).unwrap();
    socket
}

impl UdpDelayProxy {
    pub(crate) fn start(target_port: u16, delay: Duration) -> Self {
        let peer_side = bind();
        let port = peer_side.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let rebind = Arc::new(AtomicBool::new(false));
        let rebind_thread = rebind.clone();
        let target: SocketAddr = format!("127.0.0.1:{target_port}").parse().unwrap();

        let thread = std::thread::spawn(move || {
            let mut target_side = bind();
            let mut peer: Option<SocketAddr> = None;
            // (release_at, toward_target, payload) -- small volumes in these
            // tests, so a linear scan/remove is fine.
            let mut pending: Vec<(Instant, bool, Vec<u8>)> = Vec::new();
            let mut buf = [0u8; 65535];
            while !stop_thread.load(Ordering::Relaxed) {
                if rebind_thread.swap(false, Ordering::Relaxed) {
                    target_side = bind();
                }
                for (socket, toward_target) in [(&peer_side, true), (&target_side, false)] {
                    match socket.recv_from(&mut buf) {
                        Ok((n, src)) => {
                            if toward_target { peer = Some(src); }
                            pending.push((Instant::now() + delay, toward_target, buf[..n].to_vec()));
                        }
                        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                        Err(_) => {}
                    }
                }
                let now = Instant::now();
                let mut i = 0;
                while i < pending.len() {
                    if pending[i].0 <= now {
                        let (_, toward_target, data) = pending.remove(i);
                        if toward_target {
                            let _ = target_side.send_to(&data, target);
                        } else if let Some(peer) = peer {
                            let _ = peer_side.send_to(&data, peer);
                        }
                    } else {
                        i += 1;
                    }
                }
            }
        });

        Self { port, stop, rebind, thread: Some(thread) }
    }

    pub(crate) fn port(&self) -> u16 { self.port }

    /// Moves the target-facing side to a new source port, like a NAT
    /// rebinding; the old port is closed. Returns once applied.
    pub(crate) fn rebind(&self) {
        self.rebind.store(true, Ordering::Relaxed);
        while self.rebind.load(Ordering::Relaxed) { std::thread::sleep(Duration::from_millis(1)); }
    }
}

impl Drop for UdpDelayProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() { let _ = t.join(); }
    }
}
