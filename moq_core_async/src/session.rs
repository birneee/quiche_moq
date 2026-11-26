use std::collections::VecDeque;

use moq_core::session::MoqTransportSession;
use moq_core::{Event, IncomingRequest, Timestamp};
use moq_core::SubscribeOptions;
use moq_core::wire::{NamespaceTrackname, TrackAlias};
use moq_core::{Config, DropQueue, Error, Subscription};

use crate::notify::Notify;
use crate::transport::AsyncTransport;

/// `moq_core::MoqTransportSession` driven by `async fn`s, e.g.
/// `session.run_once().await`. One call at a time, for several in flight
/// use [`crate::AsyncMoqSessionActor`]/[`crate::AsyncMoqSessionHandle`].
pub struct AsyncMoqSession<T: AsyncTransport> {
    session: MoqTransportSession<T::DropQueue>,
    t: T,
    wake: Notify,
    /// Events other than REQUEST_UPDATE requests, see `next_event`.
    events: VecDeque<Event<T::DropQueue>>,
}

impl<T: AsyncTransport> AsyncMoqSession<T> {
    /// `wake` must be the same `Notify` the transport's background I/O tasks
    /// (readers, writers) were constructed with — this doesn't create its
    /// own, since `t` is typically already wired to one before it gets here
    /// (e.g. `BrowserWebTransport::new` takes a `Notify` its reader/writer
    /// tasks capture, before the session that wraps it exists).
    pub fn connect(mut t: T, config: Config, wake: Notify) -> Result<Self, Error> {
        let session = MoqTransportSession::connect(&mut t, config)?;
        Ok(Self::new(session, t, wake))
    }

    pub fn accept(t: T, config: Config, wake: Notify) -> Result<Self, Error> {
        let session = MoqTransportSession::accept(config)?;
        Ok(Self::new(session, t, wake))
    }

    /// Dropped requests and handles wake the session to resolve them.
    fn new(session: MoqTransportSession<T::DropQueue>, t: T, wake: Notify) -> Self {
        session.set_drop_waker(wake.waker());
        Self { session, t, wake, events: VecDeque::new() }
    }

    /// A clonable handle background I/O tasks call `.notify()` on when new
    /// data arrives or a stream becomes writable.
    pub fn notifier(&self) -> Notify {
        self.wake.clone()
    }

    pub fn session(&self) -> &MoqTransportSession<T::DropQueue> {
        &self.session
    }


    /// The next event, REQUEST_UPDATEs are answered by `begin_handle()` already.
    pub fn next_event(&mut self) -> Option<Event<T::DropQueue>> {
        self.events.pop_front()
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.t
    }

    /// Waits for `notify()`.
    pub async fn wait_for_wake(&mut self) {
        self.wake.notified().await;
    }

    /// Calls `handle()`; application work follows, then `end_handle()`
    /// flushes it. Prefer `run_once()` if there's nothing to do in between.
    pub fn begin_handle(&mut self) {
        let readable = self.t.readable_stream_ids();
        self.session.handle(&mut self.t, &readable, Timestamp::now());
        while let Some(event) = self.session.next_event() {
            match event {
                // without an object cache, answered without LARGEST_OBJECT
                Event::Request(IncomingRequest::RequestUpdate(update)) => {
                    let _ = self.session.accept_request(update, None, &mut self.t);
                }
                event => self.events.push_back(event),
            }
        }
    }

    /// Sends padding and flushes queued sends.
    pub async fn end_handle(&mut self) -> Result<(), T::FlushError> {
        self.session.send_padding(&mut self.t);
        self.t.flush_sends().await
    }

    /// Waits for `notify()`, then drives one `begin_handle()`/`end_handle()`
    /// cycle with nothing in between.
    /// For anything that needs to run in that window, call
    /// `wait_for_wake()`/`begin_handle()`/`end_handle()` directly instead.
    pub async fn run_once(&mut self) -> Result<(), T::FlushError> {
        self.wait_for_wake().await;
        self.begin_handle();
        self.end_handle().await
    }

    /// Sends SUBSCRIBE without waiting for a response.
    pub fn subscribe_request(
        &mut self,
        namespace_trackname: &NamespaceTrackname,
        options: SubscribeOptions,
    ) -> Result<moq_core::Subscription<T::DropQueue>, Error> {
        self.session.subscribe(namespace_trackname, options, &mut self.t)
    }

    /// Simultaneous mutable access to the session and transport, for
    /// operations `AsyncMoqSession` doesn't wrap directly (e.g. reading
    /// track data via `MoqTransportSession::read_obj_hdr`/`read_obj_pld`,
    /// which need both at once).
    pub fn parts_mut(&mut self) -> (&mut MoqTransportSession<T::DropQueue>, &mut T) {
        (&mut self.session, &mut self.t)
    }
}

#[derive(Debug)]
pub enum SubscribeError {
    Session(Error),
    Rejected(moq_core::RequestFailure),
    /// The session ended before a response arrived.
    SessionEnded,
}

/// An accepted SUBSCRIBE, dropping `subscription` unsubscribes.
#[derive(Debug)]
pub struct Subscribed<Q: DropQueue> {
    pub subscription: Subscription<Q>,
    /// Identifies the track's objects, see `MoqTransportSession::readable`.
    pub track_alias: TrackAlias,
}
