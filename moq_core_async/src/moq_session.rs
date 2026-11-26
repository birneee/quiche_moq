use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use moq_core::{SubscribeOptions, LocationFilter};
use moq_core::wire::{NamespaceTrackname, TrackAlias};
use moq_core::{Config, Error, SessionStats, Subscription};

use crate::actor::{AsyncMoqSessionActor, AsyncMoqSessionHandle};
use crate::buffered_track::{BufferedTrack, CompleteObject, TrackStats};
use crate::session::{AsyncMoqSession, SubscribeError};
use crate::notify::Notify;
use crate::transport::AsyncTransport;

/// A subscribed track's playout buffer, shared by the receive loop filling it and the `TrackObjects` taking from it.
struct TrackBuffer {
    track: Mutex<BufferedTrack>,
    /// a receive cycle advanced `track`, or the receive loop ended
    wake: Notify,
    ended: AtomicBool,
}

type Tracks = Arc<Mutex<HashMap<TrackAlias, Arc<TrackBuffer>>>>;

/// The `connect()`/`subscribe()` → `next_object()` shape shared by `moq_web`
/// and `moq_tokio`: identical for both since establishing the MoQ session
/// and reassembling objects is entirely transport-agnostic — only building
/// `T` itself (a `WebTransport` object vs. a UDP socket) is platform-specific,
/// and that stays in each platform's own code before calling `connect()`.
pub struct MoqSession<T: AsyncTransport> {
    handle: AsyncMoqSessionHandle<T>,
    tracks: Tracks,
    /// as of the last receive cycle
    stats: Arc<Mutex<SessionStats>>,
}

impl<T: AsyncTransport> Clone for MoqSession<T> {
    fn clone(&self) -> Self {
        Self { handle: self.handle.clone(), tracks: self.tracks.clone(), stats: self.stats.clone() }
    }
}

/// A subscribed track, dropping `subscription` unsubscribes.
pub struct MoqTrack<Q: moq_core::DropQueue> {
    pub subscription: Subscription<Q>,
    pub objects: TrackObjects,
}

/// The objects of a `MoqTrack`, buffered until taken. A clone reads the same buffer, e.g. for `stats`.
#[derive(Clone)]
pub struct TrackObjects(Arc<TrackBuffer>);

impl<T: AsyncTransport> MoqSession<T> {
    /// Builds `T` via `AsyncTransport::connect(params)`, completes the MoQ
    /// handshake over it, and returns the session plus the still-undriven
    /// receive-loop future. `moq_core_async` has no tokio or wasm-bindgen
    /// dependency and so can't spawn tasks itself — spawn the returned
    /// future with whichever executor your platform has (`tokio::spawn`,
    /// `wasm_bindgen_futures::spawn_local`, ...).
    pub async fn connect(params: T::ConnectParams, config: Config) -> Result<(Self, impl Future<Output = ()>), ConnectError<T>> {
        let (t, wake) = T::connect(params).await.map_err(ConnectError::Transport)?;
        Self::connect_transport(t, wake, config).await
    }

    /// `connect` over a transport the caller connected, e.g. to keep a handle on it.
    pub async fn connect_transport(t: T, wake: Notify, config: Config) -> Result<(Self, impl Future<Output = ()>), ConnectError<T>> {
        let mut async_session = AsyncMoqSession::connect(t, config, wake).map_err(ConnectError::Session)?;
        while !async_session.session().initialized() {
            async_session.run_once().await.map_err(ConnectError::Flush)?;
        }
        let (actor, handle) = AsyncMoqSessionActor::new(async_session);
        let session = Self { handle, tracks: Tracks::default(), stats: Arc::default() };
        let receive_loop = run_receive_loop(actor, session.tracks.clone(), session.stats.clone());
        Ok((session, receive_loop))
    }

    /// As of the last receive cycle.
    pub fn stats(&self) -> SessionStats {
        *self.stats.lock().unwrap()
    }

    /// Subscribes to `namespace_trackname` (e.g. `"tos--video"`, matching
    /// `moq_cli`'s `namespace--track` convention) and resolves once the peer
    /// accepts or rejects it. Safe to call concurrently from multiple
    /// callers (see `AsyncMoqSessionHandle::subscribe`).
    pub async fn subscribe(&self, namespace_trackname: &str) -> Result<MoqTrack<T::DropQueue>, SubscribeError> {
        let nt: NamespaceTrackname = namespace_trackname
            .parse()
            .map_err(|e| SubscribeError::Session(Error::InvalidNamespaceTrackname(e)))?;
        let subscribed = self.handle.subscribe(nt, SubscribeOptions { filter: LocationFilter::NextGroupStart, ..Default::default() }).await?;
        // until registered, the receive loop leaves the track's objects unread: then a cycle reads what arrived
        let buffer = Arc::new(TrackBuffer {
            track: Mutex::new(BufferedTrack::with_defaults(subscribed.track_alias)),
            wake: Notify::new(),
            ended: AtomicBool::new(false),
        });
        self.tracks.lock().unwrap().insert(subscribed.track_alias, buffer.clone());
        self.handle.wake();
        Ok(MoqTrack { subscription: subscribed.subscription, objects: TrackObjects(buffer) })
    }
}

impl<Q: moq_core::DropQueue> MoqTrack<Q> {
    /// See `TrackObjects::next_object`.
    pub async fn next_object(&mut self) -> Option<CompleteObject> {
        self.objects.next_object().await
    }
}

impl TrackObjects {
    /// Waits for the next object in playout order, see `BufferedTrack::take_ready`; `None` once the receive loop
    /// has ended (session closed or transport error) and nothing is left.
    pub async fn next_object(&mut self) -> Option<CompleteObject> {
        loop {
            if let Some(object) = self.0.track.lock().unwrap().take_ready() {
                return Some(object);
            }
            if self.0.ended.load(Ordering::Acquire) {
                // the last cycle may have advanced the track after the take above
                return self.0.track.lock().unwrap().take_ready();
            }
            self.0.wake.notified().await;
        }
    }

    pub fn stats(&self) -> TrackStats {
        self.0.track.lock().unwrap().stats()
    }
}

/// Drives `actor` until the session ends, advancing the buffer of each subscribed track and waking its reader.
/// Returned by `MoqSession::connect` for the caller to spawn.
async fn run_receive_loop<T: AsyncTransport>(mut actor: AsyncMoqSessionActor<T>, tracks: Tracks, stats: Arc<Mutex<SessionStats>>) {
    loop {
        let cycle = actor
            .run_receive_cycle(|track_alias, session, t| {
                let Some(buffer) = tracks.lock().unwrap().get(&track_alias).cloned() else { return };
                buffer.track.lock().unwrap().advance(session, t);
                buffer.wake.notify();
            })
            .await;
        *stats.lock().unwrap() = actor.session().stats();
        // dropped by every reader
        tracks.lock().unwrap().retain(|_, buffer| Arc::strong_count(buffer) > 1);
        if cycle.is_err() {
            for buffer in tracks.lock().unwrap().values() {
                buffer.ended.store(true, Ordering::Release);
                buffer.wake.notify();
            }
            return;
        }
    }
}

pub enum ConnectError<T: AsyncTransport> {
    Transport(T::ConnectError),
    Session(Error),
    Flush(T::FlushError),
}

// Can't `#[derive(Debug)]`: that would additionally (and incorrectly) bound
// `T: Debug` itself rather than just its associated error types.
impl<T: AsyncTransport> std::fmt::Debug for ConnectError<T>
where
    T::ConnectError: std::fmt::Debug,
    T::FlushError: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(e) => f.debug_tuple("Transport").field(e).finish(),
            Self::Session(e) => f.debug_tuple("Session").field(e).finish(),
            Self::Flush(e) => f.debug_tuple("Flush").field(e).finish(),
        }
    }
}
