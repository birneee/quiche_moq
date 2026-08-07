use std::time::Duration;

/// Track state for an egress subscription.
pub struct OutTrack {
    pub(crate) current_stream_id: Option<u64>,
    /// QUIC stream urgency applied to every subgroup stream opened for this track.
    /// Lower value = higher priority. Default 128 (MoQ spec default).
    pub(crate) urgency: u8,
    /// Whether subgroup streams are scheduled incrementally (round-robin) with other
    /// streams at the same urgency level.
    pub(crate) incremental: bool,
    /// DELIVERY_TIMEOUT from the subscriber's SUBSCRIBE message (milliseconds).
    pub(crate) delivery_timeout: Option<Duration>,
    /// MAX_CACHE_DURATION from the publisher's SUBSCRIBE_OK passed to accept_subscription.
    pub(crate) max_cache_duration: Option<Duration>,
}

impl OutTrack {
    pub fn new(delivery_timeout: Option<Duration>, max_cache_duration: Option<Duration>) -> Self {
        Self {
            current_stream_id: None,
            urgency: 128,
            incremental: false,
            delivery_timeout,
            max_cache_duration,
        }
    }

    pub fn writable(&self) -> bool {
        true
    }
}
