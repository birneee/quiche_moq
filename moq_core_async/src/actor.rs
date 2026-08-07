use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use moq_core::session::PollResult;
use moq_core::wire::control_message::subscribe::FilterType;
use moq_core::wire::{NamespaceTrackname, RequestId, TrackAlias};

use futures::channel::oneshot;

use crate::buffered_track::{BufferedTrack, CompleteObject};
use crate::notify::Notify;
use crate::session::{AsyncMoqSession, SubscribeError, Subscription};
use crate::transport::AsyncTransport;

type SubscribeResponder = oneshot::Sender<Result<Subscription, SubscribeError>>;

enum Command {
    Subscribe {
        namespace_trackname: NamespaceTrackname,
        subscriber_priority: Option<u8>,
        delivery_timeout: Option<u64>,
        filter_type: FilterType,
        responder: SubscribeResponder,
    },
    Unsubscribe {
        request_id: RequestId,
    },
}

/// A clonable, `Send`-free-to-share handle for calling `subscribe()`/
/// `unsubscribe()` concurrently from multiple callers — e.g. several JS
/// `await session.subscribe(...)` calls in flight at once. Each call just
/// queues a command and awaits its own response; [`AsyncMoqSessionActor`]
/// (the single task that actually owns the session) picks them up and drives
/// them, so no caller needs `&mut` access to the session itself.
pub struct AsyncMoqSessionHandle<T: AsyncTransport> {
    commands: Arc<Mutex<Vec<Command>>>,
    wake: Notify,
    // `Command` itself doesn't carry any `T`-typed data, but the type
    // parameter is kept so a handle stays tied to the transport type of the
    // session it actually talks to (e.g. `moq_tokio`'s and `moq_web`'s
    // handles can't be mixed up even though they're structurally identical).
    _transport: std::marker::PhantomData<fn() -> T>,
}

impl<T: AsyncTransport> Clone for AsyncMoqSessionHandle<T> {
    fn clone(&self) -> Self {
        Self { commands: self.commands.clone(), wake: self.wake.clone(), _transport: std::marker::PhantomData }
    }
}

impl<T: AsyncTransport> AsyncMoqSessionHandle<T> {
    /// Sends SUBSCRIBE and resolves once the response arrives. Safe to call
    /// concurrently from as many callers as you like — unlike
    /// `AsyncMoqSession::subscribe()`, this doesn't need `&mut` access to the
    /// session, so multiple calls can be genuinely in flight (e.g.
    /// `Promise.all([...])` from JS) without one blocking another.
    pub async fn subscribe(
        &self,
        namespace_trackname: NamespaceTrackname,
        subscriber_priority: Option<u8>,
        delivery_timeout: Option<u64>,
        filter_type: FilterType,
    ) -> Result<Subscription, SubscribeError> {
        let (tx, rx) = oneshot::channel();
        self.commands.lock().unwrap().push(Command::Subscribe {
            namespace_trackname,
            subscriber_priority,
            delivery_timeout,
            filter_type,
            responder: tx,
        });
        self.wake.notify();
        match rx.await {
            Ok(result) => result,
            // The actor was dropped before processing this command at all.
            Err(oneshot::Canceled) => Err(SubscribeError::Unsubscribed),
        }
    }

    pub fn unsubscribe(&self, request_id: RequestId) {
        self.commands.lock().unwrap().push(Command::Unsubscribe { request_id });
        self.wake.notify();
    }
}

/// Owns the actual `AsyncMoqSession` and services commands queued by any
/// number of [`AsyncMoqSessionHandle`] clones. The caller drives this the
/// same way it would drive a bare `AsyncMoqSession` directly
/// (`wait_for_wake()`/`begin_handle()`/`end_handle()`, with track-reading
/// or other app logic in between) — `begin_handle()` additionally drains
/// and services the command queue.
pub struct AsyncMoqSessionActor<T: AsyncTransport> {
    session: AsyncMoqSession<T>,
    commands: Arc<Mutex<Vec<Command>>>,
    pending_subscribes: HashMap<RequestId, SubscribeResponder>,
}

impl<T: AsyncTransport> AsyncMoqSessionActor<T> {
    pub fn new(session: AsyncMoqSession<T>) -> (Self, AsyncMoqSessionHandle<T>) {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let handle =
            AsyncMoqSessionHandle { commands: commands.clone(), wake: session.notifier(), _transport: std::marker::PhantomData };
        (Self { session, commands, pending_subscribes: HashMap::new() }, handle)
    }

    pub fn session(&self) -> &moq_core::session::MoqTransportSession {
        self.session.session()
    }

    fn transport_mut(&mut self) -> &mut T {
        self.session.transport_mut()
    }

    pub fn parts_mut(&mut self) -> (&mut moq_core::session::MoqTransportSession, &mut T) {
        self.session.parts_mut()
    }

    pub async fn wait_for_wake(&mut self) {
        self.session.wait_for_wake().await;
    }

    /// Drains and services queued commands (firing new SUBSCRIBEs, sending
    /// UNSUBSCRIBEs), calls `handle()`, then resolves any subscribe
    /// responses that arrived. Must be paired with `end_handle()` — see
    /// `AsyncMoqSession::begin_handle()`'s doc comment for the same
    /// dirty-flag contract, which applies here identically since this
    /// wraps that method.
    pub fn begin_handle(&mut self) -> PollResult {
        let commands = std::mem::take(&mut *self.commands.lock().unwrap());
        for command in commands {
            match command {
                Command::Subscribe { namespace_trackname, subscriber_priority, delivery_timeout, filter_type, responder } => {
                    match self.session.subscribe_request(&namespace_trackname, subscriber_priority, delivery_timeout, filter_type) {
                        Ok(request_id) => {
                            self.pending_subscribes.insert(request_id, responder);
                        }
                        Err(e) => {
                            let _ = responder.send(Err(SubscribeError::Session(e)));
                        }
                    }
                }
                Command::Unsubscribe { request_id } => {
                    self.session.unsubscribe(request_id);
                    if let Some(responder) = self.pending_subscribes.remove(&request_id) {
                        let _ = responder.send(Err(SubscribeError::Unsubscribed));
                    }
                }
            }
        }

        let result = self.session.begin_handle();

        let pending_ids: Vec<RequestId> = self.pending_subscribes.keys().copied().collect();
        for request_id in pending_ids {
            if let Some(response) = self.session.session_mut().poll_subscribe_response(request_id) {
                if let Some(responder) = self.pending_subscribes.remove(&request_id) {
                    let _ = responder.send(match response {
                        Ok((track_alias, _ok)) => Ok(Subscription { request_id, track_alias }),
                        Err(e) => Err(SubscribeError::Rejected(e)),
                    });
                }
            }
        }

        result
    }

    /// Closes out the window `begin_handle()` opened and flushes queued sends.
    pub async fn end_handle(&mut self) -> Result<(), T::FlushError> {
        self.session.end_handle().await
    }

    /// Waits for `notify()`, then drives one `begin_handle()`/`end_handle()`
    /// cycle with nothing else in between.
    pub async fn run_once(&mut self) -> Result<PollResult, T::FlushError> {
        self.wait_for_wake().await;
        let result = self.begin_handle();
        self.end_handle().await?;
        Ok(result)
    }

    /// Like `run_once()`, but also drains every readable track through a
    /// [`BufferedTrack`] (creating one with `with_defaults` the first time a
    /// track is seen) and calls `on_object` for each object completed this
    /// cycle — the "wait, handle, read all tracks, end" shape `moq_web` and
    /// `moq_tokio` both otherwise hand-roll identically. Callers that need
    /// per-track state beyond reassembly (decode dispatch, per-track
    /// channels) do that inside `on_object`; callers that need something
    /// entirely different after `begin_handle()` (e.g. accepting incoming
    /// subscriptions) should keep using `wait_for_wake()`/`begin_handle()`/
    /// `end_handle()` directly instead of this.
    pub async fn run_receive_cycle<F>(
        &mut self,
        tracks: &mut HashMap<TrackAlias, BufferedTrack>,
        mut on_object: F,
    ) -> Result<PollResult, T::FlushError>
    where
        F: FnMut(TrackAlias, CompleteObject),
    {
        self.wait_for_wake().await;
        let result = self.begin_handle();

        let (session, t) = self.session.parts_mut();
        // `readable()` returns an owned `SmallVec` (not borrowed from
        // `session`), so iterating it directly doesn't hold any borrow of
        // `session` across the loop body's `&mut session` use below.
        for track_alias in session.readable() {
            let track = tracks.entry(track_alias).or_insert_with(|| BufferedTrack::with_defaults(track_alias));
            while let Some(obj) = track.poll_object(session, t) {
                on_object(track_alias, obj);
            }
        }

        self.end_handle().await?;
        self.transport_mut().retire_finished_streams();
        Ok(result)
    }
}
