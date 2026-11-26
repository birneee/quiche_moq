//! Incoming requests the caller must answer exactly once, see the README's
//! "Obligations are linear types".

use alloc::rc::{Rc, Weak};
use core::cell::RefCell;
use core::task::Waker;
#[cfg(feature = "std")]
use std::sync::{Arc, Mutex, PoisonError};

use crate::prelude::*;

use moq_wire::control_message::subscribe::SubscribeMessage;
use moq_wire::control_message::{FetchMessage, PublishMessage, PublishNamespaceMessage, PublishOkMessage, RequestUpdateMessage, SubscribeNamespaceMessage, SubscribeTracksMessage, TrackStatusMessage};
use moq_wire::{Location, RequestId};

use crate::handles::{FetchResponse, NamespaceSubscription, Publication, Subscription, TracksSubscription};

use crate::session::MoqTransportSession;
use crate::transport::Transport;

/// An incoming request, answered with `MoqTransportSession::accept_request`
/// or `reject_request`. Dropping it unanswered rejects it.
#[must_use = "answer with accept_request() or reject_request(), dropping rejects it"]
pub struct Request<K: RequestKind, Q: DropQueue = LocalDropQueue> {
    pub(crate) message: K::Message,
    pub(crate) obligation: Obligation<Q>,
}

impl<K: RequestKind, Q: DropQueue> Request<K, Q> {
    pub fn message(&self) -> &K::Message {
        &self.message
    }

    pub fn request_id(&self) -> RequestId {
        self.obligation.request_id()
    }
}

/// What `Event::Request` carries.
#[must_use = "answer with accept_request() or reject_request(), dropping rejects it"]
pub enum IncomingRequest<Q: DropQueue = LocalDropQueue> {
    Subscribe(Request<Subscribe, Q>),
    Publish(Request<Publish, Q>),
    PublishNamespace(Request<PublishNamespace, Q>),
    RequestUpdate(Request<RequestUpdate, Q>),
    TrackStatus(Request<TrackStatus, Q>),
    Fetch(Request<Fetch, Q>),
    SubscribeNamespace(Request<SubscribeNamespace, Q>),
    SubscribeTracks(Request<SubscribeTracks, Q>),
}

impl<Q: DropQueue> IncomingRequest<Q> {
    pub fn request_id(&self) -> RequestId {
        match self {
            Self::SubscribeNamespace(r) => r.request_id(),
            Self::SubscribeTracks(r) => r.request_id(),
            Self::Subscribe(r) => r.request_id(),
            Self::Publish(r) => r.request_id(),
            Self::PublishNamespace(r) => r.request_id(),
            Self::RequestUpdate(r) => r.request_id(),
            Self::TrackStatus(r) => r.request_id(),
            Self::Fetch(r) => r.request_id(),
        }
    }
}

/// Kind of a [`Request`]: what was asked, and what accepting it takes and gives.
pub trait RequestKind: sealed::Sealed + Sized {
    type Message;
    type Accept;
    type Accepted<Q: DropQueue>;

    #[doc(hidden)]
    fn accept<Q: DropQueue>(
        session: &mut MoqTransportSession<Q>,
        request_id: RequestId,
        message: Self::Message,
        accept: Self::Accept,
        t: &mut impl Transport,
        _: sealed::Token,
    ) -> Self::Accepted<Q>;
}

/// SUBSCRIBE: accepting answers SUBSCRIBE_OK and gives the egress track.
pub enum Subscribe {}
/// PUBLISH: accepting answers PUBLISH_OK with these parameters and gives
/// the subscription to its track.
pub enum Publish {}
/// PUBLISH_NAMESPACE (ANNOUNCE before draft 14).
pub enum PublishNamespace {}
/// REQUEST_UPDATE for one of our egress tracks: accepting answers REQUEST_OK
/// with the track's LARGEST_OBJECT, which needs the caller's cache.
pub enum RequestUpdate {}
/// TRACK_STATUS: accepting answers TRACK_STATUS_OK with what a SUBSCRIBE_OK would name.
pub enum TrackStatus {}
/// FETCH, from draft 15: accepting answers FETCH_OK and gives the stream to write its objects on.
pub enum Fetch {}
/// SUBSCRIBE_NAMESPACE, from draft 18: accepting answers REQUEST_OK and gives
/// the subscription to send NAMESPACE and NAMESPACE_DONE on.
pub enum SubscribeNamespace {}
/// SUBSCRIBE_TRACKS, from draft 18: accepting answers REQUEST_OK, PUBLISHes for its tracks follow.
pub enum SubscribeTracks {}

/// FETCH_OK contents only the caller knows: where the objects it has end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchOk {
    pub end_of_track: bool,
    /// the last Location plus one, its Object 0 the whole end group
    pub end_location: Location,
}

/// SUBSCRIBE_OK contents only the caller knows.
#[derive(Debug, Clone, Copy, Default)]
pub struct SubscribeOk {
    pub largest_location: Option<Location>,
    /// MAX_CACHE_DURATION in ms.
    pub max_cache_duration: Option<u64>,
}

/// PUBLISH_OK contents only the caller knows, by default as if no parameter were sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishOk {
    /// The Forward State
    pub forward: bool,
    pub subscriber_priority: u8,
    pub filter: moq_wire::LocationFilter,
    /// OBJECT_DELIVERY_TIMEOUT in ms.
    pub object_delivery_timeout: Option<u64>,
    /// SUBGROUP_DELIVERY_TIMEOUT in ms, draft 18 on
    pub subgroup_delivery_timeout: Option<u64>,
}

impl Default for PublishOk {
    fn default() -> Self {
        let omitted = PublishOkMessage::new(RequestId(0));
        Self { forward: omitted.forward, subscriber_priority: omitted.subscriber_priority, filter: omitted.filter, object_delivery_timeout: omitted.object_delivery_timeout, subgroup_delivery_timeout: omitted.subgroup_delivery_timeout }
    }
}

impl RequestKind for Subscribe {
    type Message = SubscribeMessage;
    type Accept = SubscribeOk;
    type Accepted<Q: DropQueue> = Publication<Q>;

    fn accept<Q: DropQueue>(s: &mut MoqTransportSession<Q>, _: RequestId, m: SubscribeMessage, ok: SubscribeOk, t: &mut impl Transport, _: sealed::Token) -> Publication<Q> {
        s.accept_subscribe(m, ok, t)
    }
}

impl RequestKind for Publish {
    type Message = PublishMessage;
    type Accept = PublishOk;
    type Accepted<Q: DropQueue> = Subscription<Q>;

    fn accept<Q: DropQueue>(s: &mut MoqTransportSession<Q>, _: RequestId, m: PublishMessage, ok: PublishOk, t: &mut impl Transport, _: sealed::Token) -> Subscription<Q> {
        s.accept_publish(m, ok, t)
    }
}

impl RequestKind for PublishNamespace {
    type Message = PublishNamespaceMessage;
    type Accept = ();
    type Accepted<Q: DropQueue> = ();

    fn accept<Q: DropQueue>(s: &mut MoqTransportSession<Q>, request_id: RequestId, m: PublishNamespaceMessage, _: (), t: &mut impl Transport, _: sealed::Token) {
        s.accept_publish_namespace(request_id, m, t)
    }
}

impl RequestKind for SubscribeNamespace {
    type Message = SubscribeNamespaceMessage;
    type Accept = ();
    type Accepted<Q: DropQueue> = NamespaceSubscription<Q>;

    fn accept<Q: DropQueue>(s: &mut MoqTransportSession<Q>, request_id: RequestId, _: SubscribeNamespaceMessage, _: (), t: &mut impl Transport, _: sealed::Token) -> NamespaceSubscription<Q> {
        s.accept_subscribe_namespace(request_id, t)
    }
}

impl RequestKind for SubscribeTracks {
    type Message = SubscribeTracksMessage;
    type Accept = ();
    type Accepted<Q: DropQueue> = TracksSubscription<Q>;

    fn accept<Q: DropQueue>(s: &mut MoqTransportSession<Q>, request_id: RequestId, _: SubscribeTracksMessage, _: (), t: &mut impl Transport, _: sealed::Token) -> TracksSubscription<Q> {
        s.accept_subscribe_tracks(request_id, t)
    }
}

impl RequestKind for RequestUpdate {
    type Message = RequestUpdateMessage;
    /// The track's LARGEST_OBJECT.
    type Accept = Option<Location>;
    type Accepted<Q: DropQueue> = ();

    fn accept<Q: DropQueue>(s: &mut MoqTransportSession<Q>, _: RequestId, m: RequestUpdateMessage, largest: Option<Location>, t: &mut impl Transport, _: sealed::Token) {
        s.accept_request_update(m, largest, t)
    }
}

impl RequestKind for TrackStatus {
    type Message = TrackStatusMessage;
    type Accept = SubscribeOk;
    type Accepted<Q: DropQueue> = ();

    fn accept<Q: DropQueue>(s: &mut MoqTransportSession<Q>, request_id: RequestId, m: TrackStatusMessage, ok: SubscribeOk, t: &mut impl Transport, _: sealed::Token) {
        s.accept_track_status(request_id, m, ok, t)
    }
}

impl RequestKind for Fetch {
    type Message = FetchMessage;
    type Accept = FetchOk;
    type Accepted<Q: DropQueue> = FetchResponse<Q>;

    fn accept<Q: DropQueue>(s: &mut MoqTransportSession<Q>, request_id: RequestId, m: FetchMessage, ok: FetchOk, t: &mut impl Transport, _: sealed::Token) -> FetchResponse<Q> {
        s.accept_fetch(request_id, m, ok, t)
    }
}

pub(crate) mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Fetch {}
    impl Sealed for super::TrackStatus {}
    impl Sealed for super::Subscribe {}
    impl Sealed for super::Publish {}
    impl Sealed for super::PublishNamespace {}
    impl Sealed for super::RequestUpdate {}
    impl Sealed for super::SubscribeNamespace {}
    impl Sealed for super::SubscribeTracks {}

    /// Keeps `RequestKind::accept` uncallable outside this crate.
    pub struct Token(pub(crate) ());
}

/// What a dropped value leaves its session to do on the next `handle()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dropped {
    pub(crate) kind: DroppedKind,
    pub(crate) request_id: RequestId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DroppedKind {
    /// reject it
    Request,
    /// unsubscribe
    Subscription,
    /// PUBLISH_DONE
    Publication,
    /// PUBLISH_NAMESPACE_DONE
    Announcement,
    /// FETCH_CANCEL or cancelling its stream
    Fetching,
    /// reset the fetch stream
    FetchResponse,
    /// end the SUBSCRIBE_NAMESPACE's or SUBSCRIBE_TRACKS's stream
    PrefixSubscription,
}

/// Reports a dropped, unresolved value to the session that created it.
/// - disarmed when the value is resolved
pub(crate) struct Obligation<Q: DropQueue> {
    pub(crate) dropped: Dropped,
    pub(crate) queue: Option<Q::Handle>,
}

impl<Q: DropQueue> Obligation<Q> {
    pub(crate) fn request_id(&self) -> RequestId {
        self.dropped.request_id
    }
}

impl<Q: DropQueue> Drop for Obligation<Q> {
    fn drop(&mut self) {
        if let Some(queue) = self.queue.take() {
            Q::push(&queue, self.dropped);
        }
    }
}

/// How a dropped value reaches its session, which resolves it on the next
/// `handle()`. Chosen by the implementation, see the README's "Atomics only
/// when necessary".
pub trait DropQueue: Default {
    /// Held by each request. Pushing through it once the session is gone does nothing.
    type Handle;
    fn handle(&self) -> Self::Handle;
    fn owns(&self, handle: &Self::Handle) -> bool;
    /// Wakes the waker set by `set_waker`.
    fn push(handle: &Self::Handle, dropped: Dropped);
    fn pop(&self) -> Option<Dropped>;
    fn set_waker(&self, waker: Waker);
}

#[derive(Default)]
pub struct Pending {
    dropped: Vec<Dropped>,
    waker: Option<Waker>,
}

impl Pending {
    fn push(&mut self, dropped: Dropped) {
        self.dropped.push(dropped);
        if let Some(waker) = &self.waker {
            waker.wake_by_ref();
        }
    }
}

/// Thread-per-core: no atomics, not `Send`.
#[derive(Default)]
pub struct LocalDropQueue(Rc<RefCell<Pending>>);

impl DropQueue for LocalDropQueue {
    type Handle = Weak<RefCell<Pending>>;

    fn handle(&self) -> Self::Handle {
        Rc::downgrade(&self.0)
    }

    fn owns(&self, handle: &Self::Handle) -> bool {
        core::ptr::eq(handle.as_ptr(), Rc::as_ptr(&self.0))
    }

    fn push(handle: &Self::Handle, dropped: Dropped) {
        if let Some(pending) = handle.upgrade() {
            pending.borrow_mut().push(dropped);
        }
    }

    fn pop(&self) -> Option<Dropped> {
        self.0.borrow_mut().dropped.pop()
    }

    fn set_waker(&self, waker: Waker) {
        self.0.borrow_mut().waker = Some(waker);
    }
}

/// Multi-threaded runtimes: `Send`, requests can move between threads.
#[cfg(feature = "std")]
#[derive(Default)]
pub struct SharedDropQueue(Arc<Mutex<Pending>>);

#[cfg(feature = "std")]
fn lock(pending: &Mutex<Pending>) -> std::sync::MutexGuard<'_, Pending> {
    pending.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(feature = "std")]
impl DropQueue for SharedDropQueue {
    type Handle = std::sync::Weak<Mutex<Pending>>;

    fn handle(&self) -> Self::Handle {
        Arc::downgrade(&self.0)
    }

    fn owns(&self, handle: &Self::Handle) -> bool {
        core::ptr::eq(handle.as_ptr(), Arc::as_ptr(&self.0))
    }

    fn push(handle: &Self::Handle, dropped: Dropped) {
        if let Some(pending) = handle.upgrade() {
            lock(&pending).push(dropped);
        }
    }

    fn pop(&self) -> Option<Dropped> {
        lock(&self.0).dropped.pop()
    }

    fn set_waker(&self, waker: Waker) {
        lock(&self.0).waker = Some(waker);
    }
}
