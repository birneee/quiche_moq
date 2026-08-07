use moq_object_cache::PackedObjectCache;
use quiche_mio_runner::quiche_endpoint::ClientId;
use quiche_moq::wire::{Location, RequestId, TrackAlias};

pub(crate) struct RelaySubscriberInfo {
    pub(crate) client_id: ClientId,
    pub(crate) request_id: RequestId,
    pub(crate) subscriber_priority: u8,
    /// This subscriber's own requested `delivery_timeout`, as sent in their
    /// SUBSCRIBE. Aggregated (via `aggregate_delivery_timeout`) across all of
    /// a track's subscribers to decide what the relay itself should request
    /// from its upstream publisher.
    pub(crate) delivery_timeout: Option<u64>,
    pub(crate) track_alias: Option<TrackAlias>,
    pub(crate) publisher_gone: bool,
    pub(crate) location: Option<Location>,
    pub(crate) remaining: usize,
}

impl RelaySubscriberInfo {
    pub(crate) fn is_accepted(&self) -> bool { self.track_alias.is_some() }
}

pub(crate) struct RelayPublisherInfo {
    pub(crate) client_id: ClientId,
    pub(crate) request_id: Option<RequestId>,
    pub(crate) track_alias: Option<TrackAlias>,
    pub(crate) largest_location: Option<Location>,
    /// `delivery_timeout` last sent to the upstream publisher (via the
    /// initial SUBSCRIBE or a subsequent REQUEST_UPDATE), kept so Phase 4 can
    /// detect when the subscriber-side aggregate has changed.
    pub(crate) delivery_timeout: Option<u64>,
    /// `max_cache_duration` as returned by the upstream publisher's
    /// SUBSCRIBE_OK; propagated as-is to every downstream subscriber's own
    /// SUBSCRIBE_OK so their `OutTrack` enforces the same expiry.
    pub(crate) max_cache_duration: Option<u64>,
}

pub(crate) struct RelaySubscription {
    pub(crate) publisher: Option<RelayPublisherInfo>,
    pub(crate) subscribers: Vec<RelaySubscriberInfo>,
    pub(crate) cache: PackedObjectCache,
    pub(crate) subgroup_canceled: bool,
}

impl RelaySubscription {
    pub(crate) fn is_sent(&self) -> bool { self.publisher.as_ref().is_some_and(|p| p.request_id.is_some()) }
    pub(crate) fn is_publisher_accepted(&self) -> bool { self.publisher.as_ref().is_some_and(|p| p.track_alias.is_some()) }
    pub(crate) fn has_accepted_subscribers(&self) -> bool { self.subscribers.iter().any(|s| s.track_alias.is_some()) }
}

/// The most lenient (largest) `delivery_timeout` requested by any of
/// `subscribers`, or `None` if any subscriber requested no timeout at all
/// (an absent timeout means "never expire," which dominates any finite
/// value) — what the relay itself should request from its upstream
/// publisher so no subscriber's objects expire too early.
pub(crate) fn aggregate_delivery_timeout(subscribers: &[RelaySubscriberInfo]) -> Option<u64> {
    subscribers.iter().try_fold(0u64, |max, s| s.delivery_timeout.map(|t| max.max(t)))
        .filter(|_| !subscribers.is_empty())
}
