use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use quiche::Connection;

/// Sends keepalive pings when no packet has been received on a path for `interval`.
/// Inspired by quic-go.
/// Tracks last-recv time per path by observing `recv_bytes` changes in `PathStats`.
/// This is only required because quiche tracks `largest_rx_pkt_time` per packet number space
/// internally but does not expose it.
pub struct KeepaliveHandler {
    pub config_interval: Duration,
    /// When `true`, the effective ping interval is
    /// `max(config_interval, PTO * 3/2)` so pings are never sent faster than
    /// the connection can react.  Set to `false` to send pings at exactly
    /// `config_interval` regardless of PTO (useful for high-frequency path
    /// probing, e.g. to detect NAT rebinds quickly).
    pub pto_limit: bool,
    paths: HashMap<Key, Value>,
}

type Key = (SocketAddr, SocketAddr);

struct Value {
    recv_bytes: u64,
    last_recv: Instant,
    /// Cached per handle() call: bridges the immutable path_stats() pass and
    /// the mutable send pass, which cannot be combined due to borrow constraints.
    effective_interval: Duration,
    /// When the last ping was sent this idle period; reset to `None` when new
    /// data arrives.  Next ping is due at `last_ping_sent + effective_interval`.
    last_ping_sent: Option<Instant>,
    /// Mark-and-sweep flag: cleared at the start of handle(), set for each path
    /// seen in path_stats(), stale paths pruned afterwards.
    retain: bool,
}

impl KeepaliveHandler {
    pub fn new(interval: Duration) -> Self {
        Self { config_interval: interval, pto_limit: true, paths: HashMap::new() }
    }

    /// Create a handler that ignores the PTO floor and always pings at exactly
    /// `interval` (useful for high-frequency path probing).
    pub fn new_no_pto_limit(interval: Duration) -> Self {
        Self { config_interval: interval, pto_limit: false, paths: HashMap::new() }
    }

    /// The ping interval on a path with this RTT, see `pto_limit`.
    fn effective_interval(&self, rtt: Duration, rttvar: Duration) -> Duration {
        if !self.pto_limit {
            return self.config_interval;
        }
        // RFC 9002 §6.2.1: PTO = smoothed_rtt + max(4*rttvar, kGranularity)
        let pto = rtt + rttvar * 4;
        self.config_interval.max(pto * 3 / 2)
    }

    /// Update internal state, send PING frame if necessary, and return the duration until the next ping is due.
    /// Must be called after every batch of received packets and again when the returned duration elapses.
    /// Returns `None` only when no paths are known.
    pub fn handle(&mut self, conn: &mut Connection, now: Instant) -> Option<Duration> {
        for v in self.paths.values_mut() {
            v.retain = false;
        }
        for path in conn.path_stats() {
            let key = (path.local_addr, path.peer_addr);

            let effective_interval = self.effective_interval(path.rtt, path.rttvar);
            let entry = self.paths.entry(key).or_insert(Value {
                recv_bytes: 0,
                last_recv: now,
                effective_interval,
                last_ping_sent: None,
                retain: true,
            });
            entry.retain = true;
            entry.effective_interval = effective_interval;
            if path.recv_bytes > entry.recv_bytes {
                entry.recv_bytes = path.recv_bytes;
                entry.last_recv = now;
                entry.last_ping_sent = None;
            }
        }
        self.paths.retain(|_, v| v.retain);
        for (key, v) in self.paths.iter_mut() {
            let idle_since = v.last_ping_sent.unwrap_or(v.last_recv);
            if now >= idle_since + v.effective_interval {
                let _ = conn.send_ack_eliciting_on_path(key.0, key.1);
                v.last_ping_sent = Some(now);
            }
        }
        self.paths.values()
            .map(|v| {
                let base = v.last_ping_sent.unwrap_or(v.last_recv);
                (base + v.effective_interval).saturating_duration_since(now)
            })
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RTT: Duration = Duration::from_millis(100);
    const RTTVAR: Duration = Duration::from_millis(25);

    /// Never faster than the connection can react: 1.5 PTO.
    #[test]
    fn pto_limit_floors_the_interval() {
        let handler = KeepaliveHandler::new(Duration::from_millis(10));
        assert_eq!(handler.effective_interval(RTT, RTTVAR), Duration::from_millis(300));
        let slow = KeepaliveHandler::new(Duration::from_secs(1));
        assert_eq!(slow.effective_interval(RTT, RTTVAR), Duration::from_secs(1));
    }

    #[test]
    fn no_pto_limit_uses_exact_interval() {
        let handler = KeepaliveHandler::new_no_pto_limit(Duration::from_millis(10));
        assert_eq!(handler.effective_interval(RTT, RTTVAR), Duration::from_millis(10));
    }
}
