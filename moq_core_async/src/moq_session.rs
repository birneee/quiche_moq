use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use futures::channel::mpsc;
use futures::StreamExt;

use moq_core::wire::control_message::subscribe::FilterType;
use moq_core::wire::{NamespaceTrackname, RequestId, TrackAlias};
use moq_core::{Config, Error};

use crate::actor::{AsyncMoqSessionActor, AsyncMoqSessionHandle};
use crate::buffered_track::CompleteObject;
use crate::session::{AsyncMoqSession, SubscribeError};
use crate::transport::AsyncTransport;

type TrackSenders = Arc<Mutex<HashMap<TrackAlias, mpsc::UnboundedSender<CompleteObject>>>>;

/// The `connect()`/`subscribe()` → `next_object()` shape shared by `moq_web`
/// and `moq_tokio`: identical for both since establishing the MoQ session
/// and reassembling objects is entirely transport-agnostic — only building
/// `T` itself (a `WebTransport` object vs. a UDP socket) is platform-specific,
/// and that stays in each platform's own code before calling `connect()`.
pub struct MoqSession<T: AsyncTransport> {
    handle: AsyncMoqSessionHandle<T>,
    track_senders: TrackSenders,
}

impl<T: AsyncTransport> Clone for MoqSession<T> {
    fn clone(&self) -> Self {
        Self { handle: self.handle.clone(), track_senders: self.track_senders.clone() }
    }
}

pub struct MoqTrack {
    pub request_id: RequestId,
    receiver: mpsc::UnboundedReceiver<CompleteObject>,
}

impl<T: AsyncTransport> MoqSession<T> {
    /// Builds `T` via `AsyncTransport::connect(params)`, completes the MoQ
    /// handshake over it, and returns the session plus the still-undriven
    /// receive-loop future. `moq_core_async` has no tokio or wasm-bindgen
    /// dependency and so can't spawn tasks itself — spawn the returned
    /// future with whichever executor your platform has (`tokio::spawn`,
    /// `wasm_bindgen_futures::spawn_local`, ...).
    pub async fn connect(params: T::ConnectParams, config: Config) -> Result<(Self, impl Future<Output = ()>), ConnectError<T>> {
        let (t, wake) = T::connect(params).await.map_err(ConnectError::Transport)?;
        let mut async_session = AsyncMoqSession::connect(t, config, wake).map_err(ConnectError::Session)?;
        while !async_session.session().initialized() {
            async_session.run_once().await.map_err(ConnectError::Flush)?;
        }
        let (actor, handle) = AsyncMoqSessionActor::new(async_session);
        let (session, track_senders) = Self::from_handle(handle);
        Ok((session, run_receive_loop(actor, track_senders)))
    }

    fn from_handle(handle: AsyncMoqSessionHandle<T>) -> (Self, TrackSenders) {
        let track_senders = Arc::new(Mutex::new(HashMap::new()));
        (Self { handle, track_senders: track_senders.clone() }, track_senders)
    }

    /// Subscribes to `namespace_trackname` (e.g. `"tos--video"`, matching
    /// `moq_cli`'s `namespace--track` convention) and resolves once the peer
    /// accepts or rejects it. Safe to call concurrently from multiple
    /// callers (see `AsyncMoqSessionHandle::subscribe`).
    pub async fn subscribe(&self, namespace_trackname: &str) -> Result<MoqTrack, SubscribeError> {
        let nt: NamespaceTrackname = namespace_trackname
            .parse()
            .map_err(|e| SubscribeError::Session(Error::InvalidNamespaceTrackname(e)))?;
        let sub = self.handle.subscribe(nt, None, None, FilterType::NextGroupStart).await?;
        let (tx, rx) = mpsc::unbounded();
        self.track_senders.lock().unwrap().insert(sub.track_alias, tx);
        Ok(MoqTrack { request_id: sub.request_id, receiver: rx })
    }

    pub fn unsubscribe(&self, request_id: RequestId) {
        self.handle.unsubscribe(request_id);
    }
}

impl MoqTrack {
    /// Waits for the next reassembled object, `None` once the receive loop
    /// has ended (session closed or transport error).
    pub async fn next_object(&mut self) -> Option<CompleteObject> {
        self.receiver.next().await
    }
}

/// Drives `actor` forever, handing each completed object to whichever track
/// registered for it via `MoqSession::subscribe`. Runs until the session
/// ends; returned by `MoqSession::connect` for the caller to spawn.
async fn run_receive_loop<T: AsyncTransport>(mut actor: AsyncMoqSessionActor<T>, track_senders: TrackSenders) {
    let mut tracks = HashMap::new();
    // Mirrors `track_senders` locally so the hot per-object path doesn't lock
    // the shared `Mutex` for every object — only the first time a given
    // track is seen (`subscribe()` always inserts into `track_senders`
    // before any object for that track can arrive).
    let mut senders: HashMap<TrackAlias, mpsc::UnboundedSender<CompleteObject>> = HashMap::new();
    loop {
        let cycle = actor
            .run_receive_cycle(&mut tracks, |track_alias, obj| {
                if !senders.contains_key(&track_alias) {
                    if let Some(sender) = track_senders.lock().unwrap().get(&track_alias).cloned() {
                        senders.insert(track_alias, sender);
                    }
                }
                if let Some(sender) = senders.get(&track_alias) {
                    let _ = sender.unbounded_send(obj);
                }
            })
            .await;
        if cycle.is_err() {
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
