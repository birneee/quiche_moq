use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use moq_core::session::MoqTransportSession;
use moq_core::{Event, Subscription};
use moq_core::SubscribeOptions;
use moq_core::wire::{NamespaceTrackname, RequestId, TrackAlias};

use futures::channel::oneshot;

use crate::notify::Notify;
use crate::session::{AsyncMoqSession, SubscribeError, Subscribed};
use crate::transport::AsyncTransport;

type SubscribeResponder<Q> = oneshot::Sender<Result<Subscribed<Q>, SubscribeError>>;

struct SubscribeCommand<Q: moq_core::DropQueue> {
    namespace_trackname: NamespaceTrackname,
    options: SubscribeOptions,
    responder: SubscribeResponder<Q>,
}

/// A SUBSCRIBE awaiting its response.
struct PendingSubscribe<Q: moq_core::DropQueue> {
    subscription: Subscription<Q>,
    responder: SubscribeResponder<Q>,
}

/// A clonable handle for calling `subscribe()` concurrently from multiple
/// callers — e.g. several JS `await session.subscribe(...)` calls in flight
/// at once. Each call just queues a command and awaits its own response;
/// [`AsyncMoqSessionActor`] (the single task that actually owns the session)
/// picks them up and drives them, so no caller needs `&mut` access to the
/// session itself.
pub struct AsyncMoqSessionHandle<T: AsyncTransport> {
    commands: Arc<Mutex<Vec<SubscribeCommand<T::DropQueue>>>>,
    wake: Notify,
}

impl<T: AsyncTransport> Clone for AsyncMoqSessionHandle<T> {
    fn clone(&self) -> Self {
        Self { commands: self.commands.clone(), wake: self.wake.clone() }
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
        options: SubscribeOptions,
    ) -> Result<Subscribed<T::DropQueue>, SubscribeError> {
        let (tx, rx) = oneshot::channel();
        self.commands.lock().unwrap().push(SubscribeCommand {
            namespace_trackname,
            options,
            responder: tx,
        });
        self.wake.notify();
        match rx.await {
            Ok(result) => result,
            // The actor was dropped before processing this command at all.
            Err(oneshot::Canceled) => Err(SubscribeError::SessionEnded),
        }
    }

    /// Runs another cycle, e.g. to read a track left unread.
    pub fn wake(&self) {
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
    commands: Arc<Mutex<Vec<SubscribeCommand<T::DropQueue>>>>,
    pending_subscribes: HashMap<RequestId, PendingSubscribe<T::DropQueue>>,
    events: VecDeque<Event<T::DropQueue>>,
}

impl<T: AsyncTransport> AsyncMoqSessionActor<T> {
    pub fn new(session: AsyncMoqSession<T>) -> (Self, AsyncMoqSessionHandle<T>) {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let handle = AsyncMoqSessionHandle { commands: commands.clone(), wake: session.notifier() };
        (Self { session, commands, pending_subscribes: HashMap::new(), events: VecDeque::new() }, handle)
    }

    pub fn session(&self) -> &MoqTransportSession<T::DropQueue> {
        self.session.session()
    }

    /// The next event not resolving a `subscribe()`, see `AsyncMoqSession::next_event`.
    pub fn next_event(&mut self) -> Option<Event<T::DropQueue>> {
        self.events.pop_front()
    }

    fn transport_mut(&mut self) -> &mut T {
        self.session.transport_mut()
    }

    pub fn parts_mut(&mut self) -> (&mut MoqTransportSession<T::DropQueue>, &mut T) {
        self.session.parts_mut()
    }

    pub async fn wait_for_wake(&mut self) {
        self.session.wait_for_wake().await;
    }

    /// Sends queued SUBSCRIBEs, calls `handle()`, then resolves the
    /// subscribe responses that arrived. `end_handle()` flushes what follows.
    pub fn begin_handle(&mut self) {
        // dropping the subscription unsubscribes in `handle()` below
        self.pending_subscribes.retain(|_, pending| !pending.responder.is_canceled());
        let commands = std::mem::take(&mut *self.commands.lock().unwrap());
        for SubscribeCommand { namespace_trackname, options, responder } in commands {
            match self.session.subscribe_request(&namespace_trackname, options) {
                Ok(subscription) => {
                    self.pending_subscribes.insert(subscription.request_id(), PendingSubscribe { subscription, responder });
                }
                Err(e) => {
                    let _ = responder.send(Err(SubscribeError::Session(e)));
                }
            }
        }

        self.session.begin_handle();
        while let Some(event) = self.session.next_event() {
            let (request_id, response) = match event {
                Event::SubscribeOk { request_id, track_alias, .. } if self.pending_subscribes.contains_key(&request_id) => (request_id, Ok(track_alias)),
                Event::RequestFailed { request_id, failure: error } if self.pending_subscribes.contains_key(&request_id) => (request_id, Err(error)),
                event => { self.events.push_back(event); continue }
            };
            let PendingSubscribe { subscription, responder } = self.pending_subscribes.remove(&request_id).unwrap();
            let response = match response {
                Ok(track_alias) => Ok(Subscribed { subscription, track_alias }),
                Err(error) => {
                    let (session, t) = self.session.parts_mut();
                    session.unsubscribe(subscription, t);
                    Err(SubscribeError::Rejected(error))
                }
            };
            let _ = responder.send(response);
        }
    }

    /// Closes out the window `begin_handle()` opened and flushes queued sends.
    pub async fn end_handle(&mut self) -> Result<(), T::FlushError> {
        self.session.end_handle().await
    }

    /// One `begin_handle()`/`end_handle()` cycle of a subscriber after a wake, calling `on_readable` for every track with data to read, e.g. to
    /// advance its [`crate::BufferedTrack`]. A track left unread stays readable. Other events are dropped, which
    /// rejects the peer's requests.
    pub async fn run_receive_cycle(
        &mut self,
        mut on_readable: impl FnMut(TrackAlias, &mut MoqTransportSession<T::DropQueue>, &mut T),
    ) -> Result<(), T::FlushError> {
        self.wait_for_wake().await;
        self.begin_handle();

        let (session, t) = self.session.parts_mut();
        // an owned `SmallVec`, no borrow of `session` across the loop
        for track_alias in session.readable() {
            on_readable(track_alias, session, t);
        }
        self.events.clear();

        self.end_handle().await?;
        self.transport_mut().retire_finished_streams();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use futures::executor::block_on;
    use moq_core::test_utils::{mock_transports, subscribe_response, MockTransport};
    use moq_core::{Config, LocalDropQueue, MoqTransportSession, SubscribeOptions, Timestamp};

    use super::AsyncMoqSessionActor;
    use crate::notify::Notify;
    use crate::session::AsyncMoqSession;
    use crate::transport::AsyncTransport;

    impl AsyncTransport for MockTransport {
        type FlushError = Infallible;
        type DropQueue = LocalDropQueue;
        type ConnectParams = ();
        type ConnectError = Infallible;

        fn readable_stream_ids(&self) -> Vec<u64> {
            self.readable_streams()
        }

        async fn flush_sends(&mut self) -> Result<(), Infallible> {
            self.deliver();
            Ok(())
        }

        async fn connect(_: ()) -> Result<(Self, Notify), Infallible> {
            unreachable!("built by mock_transports")
        }
    }

    #[test]
    fn receive_cycle_rejects_the_peers_requests() {
        let (client_t, mut server_t) = mock_transports();
        let wake = Notify::new();
        let (mut actor, _handle) = AsyncMoqSessionActor::new(AsyncMoqSession::connect(client_t, Config::default(), wake.clone()).unwrap());
        let mut server = MoqTransportSession::accept(Config::default()).unwrap();
        let cycle = |actor: &mut AsyncMoqSessionActor<MockTransport>, server: &mut MoqTransportSession, server_t: &mut MockTransport| {
            wake.notify();
            block_on(actor.run_receive_cycle(|_, _, _| {})).unwrap();
            server.handle(server_t, &server_t.readable_streams(), Timestamp::default());
            server_t.deliver();
        };
        cycle(&mut actor, &mut server, &mut server_t);
        cycle(&mut actor, &mut server, &mut server_t);
        assert!(actor.session().initialized());
        let subscription = server.subscribe(&"n--t".parse().unwrap(), SubscribeOptions::default(), &mut server_t).unwrap();
        server_t.deliver();
        // the first drops the request, the second resolves the drop
        cycle(&mut actor, &mut server, &mut server_t);
        cycle(&mut actor, &mut server, &mut server_t);
        assert!(matches!(subscribe_response(&mut server, subscription.request_id()), Some(Err(_))));
    }
}
