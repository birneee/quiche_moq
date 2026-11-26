mod net;
mod webtransport;

use moq_core::OutgoingObject;
use moq_core::wire::{Location, PublishDoneStatus, TrackAlias, MOQ_VERSION_DRAFT_19};
use moq_core::{Config, Event, IncomingRequest, Request, SharedDropQueue, SubscribeOk, Transport, VersionNegotiation};

/// WebTransport negotiates the MoQ version out-of-band (ALPN / WT-Protocol,
/// draft-16 §3.1), not via CLIENT_SETUP's version list — so both sides are
/// configured with the same pinned version directly rather than negotiating.
/// Passed to `MoqSession::connect(addr, config())`.
pub fn config() -> Config {
    Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_19), ..Config::default() }
}
use moq_core_async::{AsyncMoqSession, AsyncMoqSessionActor, Notify};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use tokio::sync::{mpsc, oneshot};
use webtransport::WebTransportHandle;

#[derive(Debug)]
pub struct Error;

// ── client ─────────────────────────────────────────────────────────────

/// `moq_core_async`'s generic session/track directly — the same
/// `connect()`/`subscribe()`/`next_object()` shape `moq_web` uses in the
/// browser, parameterized here with the native WebTransport-over-H3
/// transport. `WebTransportHandle::connect` (in `webtransport.rs`) dials the
/// real UDP socket (`net::connect`) — the only genuinely tokio-specific step
/// — so `MoqSession::connect(addr, config())` works directly:
/// `tokio::spawn` the returned receive-loop future yourself, same as any
/// other `moq_core_async::MoqSession::connect` caller.
pub type MoqSession = moq_core_async::MoqSession<WebTransportHandle>;
pub type MoqTrack = moq_core_async::MoqTrack<SharedDropQueue>;
pub use moq_core_async::{CompleteObject as Object, SubscribeError};

// ── server ─────────────────────────────────────────────────────────────

type MoqTransportSession = moq_core::MoqTransportSession<SharedDropQueue>;
type SubscribeRequest = Request<moq_core::request::Subscribe, SharedDropQueue>;
type Publication = moq_core::Publication<SharedDropQueue>;

enum ServerCommand {
    Accept { request: SubscribeRequest, resp: oneshot::Sender<Result<Publication, Error>> },
    SendObject { track_alias: TrackAlias, payload: Vec<u8>, resp: oneshot::Sender<Result<(), Error>> },
    Finish { publication: Publication },
}

/// An object `Publish::send_object` handed over, sent as flow control allows.
struct PendingObject {
    track_alias: TrackAlias,
    location: Location,
    payload: Vec<u8>,
    /// payload bytes sent, `None` until the header is sent
    sent: Option<usize>,
    resp: oneshot::Sender<Result<(), Error>>,
}

impl PendingObject {
    /// Sends as much as possible, `Ok(true)` once complete.
    fn flush(&mut self, session: &mut MoqTransportSession, t: &mut impl Transport) -> moq_core::Result<bool> {
        use moq_core::Error::{Done, InsufficientCapacity};
        if self.sent.is_none() {
            match session.send_obj_hdr(self.track_alias, OutgoingObject::new(self.location, SUBGROUP_ID, self.payload.len()), t) {
                Ok(()) => self.sent = Some(0),
                Err(Done | InsufficientCapacity) => return Ok(false),
                Err(e) => return Err(e),
            }
        }
        let sent = self.sent.as_mut().unwrap();
        while *sent < self.payload.len() {
            match session.send_obj_pld(self.track_alias, self.location, &self.payload[*sent..], t) {
                Ok(n) => *sent += n,
                Err(Done | InsufficientCapacity) => return Ok(false),
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }
}

/// Sends pending objects in order, answering each once it's fully sent.
fn flush_pending(pending: &mut VecDeque<PendingObject>, session: &mut MoqTransportSession, t: &mut impl Transport) {
    while let Some(obj) = pending.front_mut() {
        let result = match obj.flush(session, t) {
            Ok(false) => return,
            Ok(true) => Ok(()),
            Err(_) => Err(Error),
        };
        let _ = pending.pop_front().unwrap().resp.send(result);
    }
}

// `Publish::send_object` sends one whole object per call with no subgroup
// addressing of its own — everything for a track goes on this single
// persistent Group/Subgroup (0, 0).
const GROUP_ID: u64 = 0;
const SUBGROUP_ID: u64 = 0;

pub struct MoqServer {
    local_addr: SocketAddr,
    incoming_subscriptions: mpsc::UnboundedReceiver<SubscriptionRequest>,
}

impl MoqServer {
    pub async fn listen(addr: &str) -> Result<Self, Error> {
        let mut listener = net::Listener::bind(addr).await?;
        let local_addr = listener.local_addr();
        let (sub_tx, sub_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let Ok((transport, wake)) = listener.accept().await else { break };
                tokio::spawn(handle_connection(transport, wake, sub_tx.clone()));
            }
        });
        Ok(Self { local_addr, incoming_subscriptions: sub_rx })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Resolves once a peer sends a SUBSCRIBE — inspect it and call
    /// `accept()`/`reject()` before any object flows. Dropping it rejects it.
    pub async fn next_subscription_request(&mut self) -> Result<SubscriptionRequest, Error> {
        self.incoming_subscriptions.recv().await.ok_or(Error)
    }
}

pub struct SubscriptionRequest {
    request: SubscribeRequest,
    namespace_trackname: String,
    cmd_tx: mpsc::UnboundedSender<ServerCommand>,
    wake: Notify,
}

impl SubscriptionRequest {
    pub fn namespace_trackname(&self) -> &str {
        &self.namespace_trackname
    }

    pub async fn accept(self) -> Result<Publish, Error> {
        let (tx, rx) = oneshot::channel();
        send(&self.cmd_tx, &self.wake, ServerCommand::Accept { request: self.request, resp: tx })?;
        let publication = rx.await.map_err(|_| Error)??;
        Ok(Publish { publication, cmd_tx: self.cmd_tx, wake: self.wake })
    }

    /// Same as dropping it.
    pub fn reject(self) {}
}

/// An accepted subscription. Dropping it ends the track with an internal error, see `finish`.
pub struct Publish {
    publication: Publication,
    cmd_tx: mpsc::UnboundedSender<ServerCommand>,
    wake: Notify,
}

impl Publish {
    pub async fn send_object(&self, payload: &[u8]) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        let track_alias = self.publication.track_alias();
        send(&self.cmd_tx, &self.wake, ServerCommand::SendObject { track_alias, payload: payload.to_vec(), resp: tx })?;
        rx.await.map_err(|_| Error)?
    }

    /// Ends the track after the objects sent so far.
    pub fn finish(self) -> Result<(), Error> {
        let Self { publication, cmd_tx, wake } = self;
        send(&cmd_tx, &wake, ServerCommand::Finish { publication })
    }
}

fn send(cmd_tx: &mpsc::UnboundedSender<ServerCommand>, wake: &Notify, command: ServerCommand) -> Result<(), Error> {
    cmd_tx.send(command).map_err(|_| Error)?;
    wake.notify();
    Ok(())
}

async fn handle_connection(
    transport: WebTransportHandle,
    wake: Notify,
    sub_tx: mpsc::UnboundedSender<SubscriptionRequest>,
) {
    let Ok(async_session) = AsyncMoqSession::accept(transport, config(), wake.clone()) else { return };
    let (mut actor, _handle) = AsyncMoqSessionActor::new(async_session);
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<ServerCommand>();

    let mut next_object_id: HashMap<TrackAlias, u64> = HashMap::new();
    let mut pending: VecDeque<PendingObject> = VecDeque::new();

    loop {
        actor.wait_for_wake().await;
        actor.begin_handle();

        while let Ok(cmd) = cmd_rx.try_recv() {
            let (session, t) = actor.parts_mut();
            match cmd {
                ServerCommand::Accept { request, resp } => {
                    let _ = resp.send(session.accept_request(request, SubscribeOk::default(), t).map_err(|_| Error));
                }
                ServerCommand::Finish { publication } => {
                    next_object_id.remove(&publication.track_alias());
                    session.publish_done(publication, PublishDoneStatus::TrackEnded, t);
                }
                ServerCommand::SendObject { track_alias, payload, resp } => {
                    let object_id = next_object_id.entry(track_alias).or_insert(0);
                    let location = Location { group: GROUP_ID, object: *object_id };
                    *object_id += 1;
                    pending.push_back(PendingObject { track_alias, location, payload, sent: None, resp });
                }
            }
        }
        let (session, t) = actor.parts_mut();
        flush_pending(&mut pending, session, t);

        // other requests are dropped, which rejects them
        while let Some(event) = actor.next_event() {
            if let Event::Request(IncomingRequest::Subscribe(request)) = event {
                let _ = sub_tx.send(SubscriptionRequest {
                    namespace_trackname: request.message().namespace_trackname.to_string(),
                    request,
                    cmd_tx: cmd_tx.clone(),
                    wake: wake.clone(),
                });
            }
        }

        if actor.end_handle().await.is_err() {
            return;
        }
    }
}
