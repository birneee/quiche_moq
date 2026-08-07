use moq_core::session::{MoqTransportSession, PollResult};
use moq_core::wire::control_message::subscribe::FilterType;
use moq_core::wire::control_message::RequestErrorMessage;
use moq_core::wire::{NamespaceTrackname, RequestId, TrackAlias};
use moq_core::{Config, Error};

use crate::notify::Notify;
use crate::transport::AsyncTransport;

/// Wraps `moq_core::MoqTransportSession` to turn its synchronous
/// `handle()`/`post_handle()` + poll-style getters into `async fn`s a caller
/// can `.await` directly, e.g. `session.run_once().await`.
///
/// Holds `&mut self` for the duration of any `async fn` here (there's no
/// interior mutability) — so only one such call can be in flight at a time,
/// same as `MoqTransportSession` itself. Callers that need several logical
/// operations in flight concurrently (e.g. multiple `subscribe()`s at once)
/// should drive this from a single task and use
/// [`crate::AsyncMoqSessionActor`]/[`crate::AsyncMoqSessionHandle`] instead
/// of calling this directly from several places.
pub struct AsyncMoqSession<T: AsyncTransport> {
    session: MoqTransportSession,
    t: T,
    wake: Notify,
}

impl<T: AsyncTransport> AsyncMoqSession<T> {
    /// `wake` must be the same `Notify` the transport's background I/O tasks
    /// (readers, writers) were constructed with — this doesn't create its
    /// own, since `t` is typically already wired to one before it gets here
    /// (e.g. `BrowserWebTransport::new` takes a `Notify` its reader/writer
    /// tasks capture, before the session that wraps it exists).
    pub fn connect(mut t: T, config: Config, wake: Notify) -> Result<Self, Error> {
        let session = MoqTransportSession::connect(&mut t, config)?;
        Ok(Self { session, t, wake })
    }

    pub fn accept(t: T, config: Config, wake: Notify) -> Result<Self, Error> {
        let session = MoqTransportSession::accept(config)?;
        Ok(Self { session, t, wake })
    }

    /// A clonable handle background I/O tasks call `.notify()` on when new
    /// data arrives or a stream becomes writable.
    pub fn notifier(&self) -> Notify {
        self.wake.clone()
    }

    pub fn session(&self) -> &MoqTransportSession {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut MoqTransportSession {
        &mut self.session
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.t
    }

    /// Waits for `notify()`.
    pub async fn wait_for_wake(&mut self) {
        self.wake.notified().await;
    }

    /// Calls `handle()`. Must be followed by `end_handle()` before this (or
    /// `subscribe_request()`/`unsubscribe()`) is called again — like
    /// `MoqTransportSession` itself, mutating calls made in between (e.g.
    /// `parts_mut()`'s `read_obj_hdr`/`read_obj_pld` to read track data, or
    /// accepting an incoming subscription) share the same window and only
    /// need the one `end_handle()` to close it out. Prefer `run_once()` if
    /// nothing needs to run in that window.
    pub fn begin_handle(&mut self) -> PollResult {
        let readable = self.t.readable_stream_ids();
        self.session.handle(&mut self.t, &readable)
    }

    /// Closes out the window `begin_handle()` opened and flushes queued sends.
    pub async fn end_handle(&mut self) -> Result<(), T::FlushError> {
        self.session.post_handle(&mut self.t);
        self.t.flush_sends().await
    }

    /// Waits for `notify()`, then drives one `begin_handle()`/`end_handle()`
    /// cycle with nothing in between, returning the resulting `PollResult`.
    /// For anything that needs to run in that window, call
    /// `wait_for_wake()`/`begin_handle()`/`end_handle()` directly instead.
    pub async fn run_once(&mut self) -> Result<PollResult, T::FlushError> {
        self.wait_for_wake().await;
        let result = self.begin_handle();
        self.end_handle().await?;
        Ok(result)
    }

    /// Sends SUBSCRIBE without waiting for a response.
    ///
    /// Like `handle()`, this marks the session dirty; it closes that out
    /// itself (calling `end_handle()`-equivalent cleanup before returning),
    /// so it's safe to call standalone, outside any `begin_handle()`/
    /// `end_handle()` window. It's also safe to call *inside* one (e.g.
    /// `moq_cli`'s tick loop calls `subscribe()` many times before its one
    /// `post_handle()`) — the extra `post_handle()` this does is redundant
    /// but harmless there, since `post_handle()` has no "already called"
    /// check.
    pub fn subscribe_request(
        &mut self,
        namespace_trackname: &NamespaceTrackname,
        subscriber_priority: Option<u8>,
        delivery_timeout: Option<u64>,
        filter_type: FilterType,
    ) -> Result<RequestId, Error> {
        let result = self.session.subscribe(namespace_trackname, subscriber_priority, delivery_timeout, filter_type, &mut self.t);
        self.session.post_handle(&mut self.t);
        result
    }

    /// Also marks the session dirty; see `subscribe_request()`'s doc comment
    /// for why this closes that out itself before returning.
    pub fn unsubscribe(&mut self, request_id: RequestId) {
        self.session.unsubscribe(request_id, &mut self.t);
        self.session.post_handle(&mut self.t);
    }

    /// Simultaneous mutable access to the session and transport, for
    /// operations `AsyncMoqSession` doesn't wrap directly (e.g. reading
    /// track data via `MoqTransportSession::read_obj_hdr`/`read_obj_pld`,
    /// which need both at once).
    pub fn parts_mut(&mut self) -> (&mut MoqTransportSession, &mut T) {
        (&mut self.session, &mut self.t)
    }
}

#[derive(Debug)]
pub enum SubscribeError {
    Session(Error),
    Rejected(RequestErrorMessage),
    /// `unsubscribe()` was called for this request before a response arrived.
    Unsubscribed,
}

/// A successful subscription: `track_alias` identifies the track for reading
/// data (`MoqTransportSession::read_obj_hdr`/`read_obj_pld`); `request_id`
/// is what `unsubscribe()` needs.
#[derive(Debug, Clone, Copy)]
pub struct Subscription {
    pub request_id: RequestId,
    pub track_alias: TrackAlias,
}
