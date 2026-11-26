//! What the caller ends exactly once, see the README's "Obligations are
//! linear types". Each ends by dropping it too.

use core::fmt;

use moq_wire::{RequestId, TrackAlias};

use crate::request::{DropQueue, LocalDropQueue, Obligation};

/// Our subscription: a sent SUBSCRIBE or an accepted PUBLISH. Ended by
/// `MoqTransportSession::unsubscribe`.
#[must_use = "dropping it unsubscribes"]
pub struct Subscription<Q: DropQueue = LocalDropQueue> {
    pub(crate) obligation: Obligation<Q>,
}

impl<Q: DropQueue> Subscription<Q> {
    pub fn request_id(&self) -> RequestId {
        self.obligation.request_id()
    }
}

/// Our egress track: an accepted SUBSCRIBE or a sent PUBLISH. Ended by
/// `MoqTransportSession::publish_done`.
#[must_use = "dropping it sends PUBLISH_DONE with an internal error"]
pub struct Publication<Q: DropQueue = LocalDropQueue> {
    pub(crate) obligation: Obligation<Q>,
    pub(crate) track_alias: TrackAlias,
}

impl<Q: DropQueue> Publication<Q> {
    pub fn request_id(&self) -> RequestId {
        self.obligation.request_id()
    }

    pub fn track_alias(&self) -> TrackAlias {
        self.track_alias
    }
}

/// A namespace we announced. Ended by `MoqTransportSession::publish_namespace_done`.
#[must_use = "dropping it sends PUBLISH_NAMESPACE_DONE"]
pub struct Announcement<Q: DropQueue = LocalDropQueue> {
    pub(crate) obligation: Obligation<Q>,
}

impl<Q: DropQueue> Announcement<Q> {
    pub fn request_id(&self) -> RequestId {
        self.obligation.request_id()
    }
}

/// A peer's SUBSCRIBE_NAMESPACE we accepted: `send_namespace` and `send_namespace_done`
/// tell it about namespaces under its prefix. Ended by `end_namespace_subscription`.
#[must_use = "dropping it ends the subscription"]
pub struct NamespaceSubscription<Q: DropQueue = LocalDropQueue> {
    pub(crate) obligation: Obligation<Q>,
}

impl<Q: DropQueue> NamespaceSubscription<Q> {
    pub fn request_id(&self) -> RequestId {
        self.obligation.request_id()
    }
}

/// A peer's SUBSCRIBE_TRACKS we accepted: the tracks under its prefix are PUBLISHed to it.
/// Ended by `end_tracks_subscription`.
#[must_use = "dropping it ends the subscription"]
pub struct TracksSubscription<Q: DropQueue = LocalDropQueue> {
    pub(crate) obligation: Obligation<Q>,
}

impl<Q: DropQueue> TracksSubscription<Q> {
    pub fn request_id(&self) -> RequestId {
        self.obligation.request_id()
    }
}

/// Our FETCH, its objects read with `MoqTransportSession::recv_fetch_entry`. Ended by `cancel_fetch`, also once read to its end.
#[must_use = "dropping it cancels the fetch"]
pub struct Fetching<Q: DropQueue = LocalDropQueue> {
    pub(crate) obligation: Obligation<Q>,
}

impl<Q: DropQueue> Fetching<Q> {
    pub fn request_id(&self) -> RequestId {
        self.obligation.request_id()
    }
}

/// Our answer to a peer's FETCH, its objects written with `MoqTransportSession::send_fetch_entry`. Ended by `finish_fetch`.
#[must_use = "dropping it resets the fetch stream"]
pub struct FetchResponse<Q: DropQueue = LocalDropQueue> {
    pub(crate) obligation: Obligation<Q>,
}

impl<Q: DropQueue> FetchResponse<Q> {
    pub fn request_id(&self) -> RequestId {
        self.obligation.request_id()
    }
}

impl<Q: DropQueue> fmt::Debug for Fetching<Q> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fetching").field("request_id", &self.request_id()).finish()
    }
}

impl<Q: DropQueue> fmt::Debug for FetchResponse<Q> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FetchResponse").field("request_id", &self.request_id()).finish()
    }
}

impl<Q: DropQueue> fmt::Debug for Subscription<Q> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Subscription").field("request_id", &self.request_id()).finish()
    }
}

impl<Q: DropQueue> fmt::Debug for Publication<Q> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Publication").field("request_id", &self.request_id()).field("track_alias", &self.track_alias).finish()
    }
}

impl<Q: DropQueue> fmt::Debug for Announcement<Q> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Announcement").field("request_id", &self.request_id()).finish()
    }
}
