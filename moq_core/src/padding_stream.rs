use crate::prelude::*;
use core::time::Duration;

use crate::time::Timestamp;

use crate::transport::{StreamError, Transport};
use log::{trace, warn};
use moq_wire::{PADDING_STREAM_TYPE_ID, ResetCode, Version};

/// Smallest rate-limited write: one full packet.
/// - smaller trickles become a storm of tiny packets
const MIN_WRITE: usize = 1200;

/// Padding payload source, not re-zeroed per write.
static ZEROS: [u8; 1 << 16] = [0; 1 << 16];

/// Max banked allowance, in full packets.
/// - bounds the burst after a stall (e.g. cwnd-blocked)
const MAX_BURST_PACKETS: usize = 4;

/// Token bucket for the padding stream, see `Config::padding_stream_max_bytes_per_sec`.
pub(crate) struct RateLimit {
    bytes_per_sec: u64,
    allowance: f64,
    last_refill: Timestamp,
    /// Last pass stopped on an empty bucket, not a full transport.
    /// - only then a timer is needed; a full transport is woken by ACKs
    waiting_for_tokens: bool,
}

impl RateLimit {
    fn new(bytes_per_sec: u64, now: Timestamp) -> Self {
        Self { bytes_per_sec, allowance: 0.0, last_refill: now, waiting_for_tokens: false }
    }

    /// When the next full packet is allowed, if waiting for tokens.
    fn next_write_at(&self) -> Option<Timestamp> {
        if !self.waiting_for_tokens || self.bytes_per_sec == 0 {
            return None;
        }
        let missing = (MIN_WRITE as f64 - self.allowance).max(0.0);
        Some(self.last_refill + Duration::from_secs_f64(missing / self.bytes_per_sec as f64))
    }

    /// Refills, then returns writable bytes: 0 or a multiple of `MIN_WRITE`.
    fn available(&mut self, now: Timestamp) -> usize {
        self.allowance += now.saturating_duration_since(self.last_refill).as_secs_f64() * self.bytes_per_sec as f64;
        self.last_refill = now;
        self.allowance = self.allowance.min((MIN_WRITE * MAX_BURST_PACKETS) as f64);
        if self.allowance < MIN_WRITE as f64 {
            return 0;
        }
        (self.allowance as usize / MIN_WRITE) * MIN_WRITE
    }

    fn consume(&mut self, n: usize) {
        self.allowance -= n as f64;
    }
}

enum State {
    Requested,
    Type { stream_id: u64 },
    Data { stream_id: u64 },
    /// Peer sent STOP_SENDING.
    Stopped,
}

pub(crate) struct OutPaddingStream {
    state: State,
    limit: Option<RateLimit>,
}

impl OutPaddingStream {
    pub fn new(max_bytes_per_sec: Option<u64>, now: Timestamp) -> Self {
        Self { state: State::Requested, limit: max_bytes_per_sec.map(|rate| RateLimit::new(rate, now)) }
    }

    pub fn handle<T: Transport>(&mut self, t: &mut T, version: Version, now: Timestamp) {
        'outer: loop {
            match &mut self.state {
                State::Requested => {
                    let stream_id = match t.open_uni_stream() {
                        Ok(stream_id) => stream_id,
                        // stream limit reached: retry on the next pass
                        Err(StreamError::Done | StreamError::InsufficientCapacity) => break 'outer,
                        Err(e) => {
                            warn!("padding stream not opened: {e:?}");
                            self.state = State::Stopped;
                            break 'outer;
                        }
                    };
                    t.set_stream_priority(stream_id, u8::MAX, true);
                    self.state = State::Type { stream_id };
                    #[cfg(feature = "qlog")]
                    if let Some(qlog) = t.qlog_streamer() {
                        qlog.add_event_now(qlog::events::JsonEvent {
                            time: 0.0,
                            importance: qlog::events::EventImportance::default(),
                            name: "moqt:padding_stream_started".into(),
                            data: serde_json::json!({ "stream_id": stream_id }),
                        }).ok();
                    }
                }
                State::Type { stream_id } => {
                    let mut ty = Vec::new();
                    version.writer(&mut ty).varint(PADDING_STREAM_TYPE_ID).expect("a small constant");
                    match t.send_stream_if_capacity(*stream_id, &ty, false) {
                        Ok(_) => {}
                        Err(StreamError::Done | StreamError::InsufficientCapacity) => break 'outer,
                        Err(e) => {
                            warn!("padding stream type not sent: {e:?}");
                            self.state = State::Stopped;
                            break 'outer;
                        }
                    }
                    self.state = State::Data { stream_id: *stream_id };
                }
                State::Data { stream_id } => {
                    let buf = &ZEROS;
                    loop {
                        let chunk = match &mut self.limit {
                            Some(l) => {
                                let n = l.available(now).min(buf.len());
                                if n == 0 {
                                    l.waiting_for_tokens = true;
                                    break 'outer;
                                }
                                &buf[..n]
                            }
                            None => &buf[..],
                        };
                        match t.send_stream(*stream_id, chunk, false) {
                            Ok(0) | Err(StreamError::Done) => {
                                // transport full: ACKs wake the loop
                                if let Some(l) = &mut self.limit { l.waiting_for_tokens = false }
                                break 'outer;
                            }
                            Ok(n) => {
                                if let Some(l) = &mut self.limit { l.consume(n) }
                                trace!("sent {n} bytes on padding stream");
                            }
                            Err(e) => {
                                if !matches!(e, StreamError::StreamStopped(_)) {
                                    warn!("padding stream ended: {e:?}");
                                }
                                if let Some(l) = &mut self.limit { l.waiting_for_tokens = false }
                                self.state = State::Stopped;
                                break 'outer;
                            }
                        }
                    }
                }
                State::Stopped => break 'outer,
            }
        }
    }

    /// Wakeup needed by a rate-limited stream waiting for tokens.
    /// - `None` when uncapped or transport-blocked (I/O-driven)
    pub fn next_write_at(&self) -> Option<Timestamp> {
        self.limit.as_ref()?.next_write_at()
    }

    /// Cancel the padding stream. No-op if not yet opened.
    pub fn stop<T: Transport>(&self, t: &mut T) {
        if let State::Type { stream_id } | State::Data { stream_id } = &self.state {
            t.reset_stream(*stream_id, ResetCode::Canceled);
            #[cfg(feature = "qlog")]
            if let Some(qlog) = t.qlog_streamer() {
                qlog.add_event_now(qlog::events::JsonEvent {
                    time: 0.0,
                    importance: qlog::events::EventImportance::default(),
                    name: "moqt:padding_stream_stopped".into(),
                    data: serde_json::json!({ "stream_id": stream_id }),
                }).ok();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_releases_only_full_packets() {
        // 1 MB/s: ~1ms accrues just under one packet's worth.
        let start = Timestamp::default();
        let mut l = RateLimit::new(1_000_000, start);
        assert_eq!(l.available(start + Duration::from_micros(500)), 0, "released a sub-packet write");
        let n = l.available(start + Duration::from_micros(2500));
        assert!(n >= MIN_WRITE, "should release at least one packet, got {n}");
        assert_eq!(n % MIN_WRITE, 0, "release should be whole packets, got {n}");
    }

    #[test]
    fn rate_limit_caps_banked_burst() {
        let start = Timestamp::default();
        let mut l = RateLimit::new(1_000_000, start);
        assert_eq!(l.available(start + Duration::from_millis(50)), MIN_WRITE * MAX_BURST_PACKETS);
    }
}
