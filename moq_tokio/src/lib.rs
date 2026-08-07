mod net;
mod webtransport;

use moq_core::wire::{RequestId, TrackAlias, MOQ_VERSION_DRAFT_18};
use moq_core::{Config, VersionNegotiation};

/// WebTransport negotiates the MoQ version out-of-band (ALPN / WT-Protocol,
/// draft-16 §3.1), not via CLIENT_SETUP's version list — so both sides are
/// configured with the same pinned version directly rather than negotiating.
/// Passed to `MoqSession::connect(addr, config())`.
pub fn config() -> Config {
    Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_18), ..Config::default() }
}
use moq_core_async::{AsyncMoqSession, AsyncMoqSessionActor, Notify};
use std::collections::HashSet;
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
pub type MoqTrack = moq_core_async::MoqTrack;
pub use moq_core_async::CompleteObject as Object;

// ── server ─────────────────────────────────────────────────────────────

enum ServerCommand {
    Accept { request_id: RequestId, resp: oneshot::Sender<Result<TrackAlias, Error>> },
    Reject { request_id: RequestId },
    SendObject { track_alias: TrackAlias, payload: Vec<u8>, resp: oneshot::Sender<Result<(), Error>> },
}

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
    /// `accept()`/`reject()` before any object flows, same split
    /// `moq_core::SubscriptionRequestAction` uses natively.
    pub async fn next_subscription_request(&mut self) -> Result<SubscriptionRequest, Error> {
        self.incoming_subscriptions.recv().await.ok_or(Error)
    }
}

pub struct SubscriptionRequest {
    request_id: RequestId,
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
        self.cmd_tx.send(ServerCommand::Accept { request_id: self.request_id, resp: tx }).map_err(|_| Error)?;
        self.wake.notify();
        let track_alias = rx.await.map_err(|_| Error)??;
        Ok(Publish { track_alias, cmd_tx: self.cmd_tx, wake: self.wake })
    }

    pub async fn reject(self, _reason: &str) {
        let _ = self.cmd_tx.send(ServerCommand::Reject { request_id: self.request_id });
        self.wake.notify();
    }
}

pub struct Publish {
    track_alias: TrackAlias,
    cmd_tx: mpsc::UnboundedSender<ServerCommand>,
    wake: Notify,
}

impl Publish {
    pub async fn send_object(&self, payload: &[u8]) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(ServerCommand::SendObject { track_alias: self.track_alias, payload: payload.to_vec(), resp: tx })
            .map_err(|_| Error)?;
        self.wake.notify();
        rx.await.map_err(|_| Error)?
    }
}

async fn handle_connection(
    transport: WebTransportHandle,
    wake: Notify,
    sub_tx: mpsc::UnboundedSender<SubscriptionRequest>,
) {
    let Ok(async_session) = AsyncMoqSession::accept(transport, config(), wake.clone()) else { return };
    let (mut actor, _handle) = AsyncMoqSessionActor::new(async_session);
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<ServerCommand>();
    let mut surfaced: HashSet<RequestId> = HashSet::new();

    loop {
        actor.wait_for_wake().await;
        let _poll = actor.begin_handle();

        while let Ok(cmd) = cmd_rx.try_recv() {
            let (session, t) = actor.parts_mut();
            match cmd {
                ServerCommand::Accept { request_id, resp } => {
                    let alias = session.accept_subscription(request_id, None, None, t);
                    let _ = resp.send(Ok(alias));
                }
                ServerCommand::Reject { request_id } => {
                    session.reject_subscription(request_id, 0, t);
                }
                ServerCommand::SendObject { track_alias, payload, resp } => {
                    let r = session.send_obj(&payload, track_alias, t).map_err(|_| Error);
                    let _ = resp.send(r);
                }
            }
        }

        for (request_id, sub) in actor.session().pending_received_subscriptions() {
            if surfaced.insert(*request_id) {
                let _ = sub_tx.send(SubscriptionRequest {
                    request_id: *request_id,
                    namespace_trackname: sub.namespace_trackname.to_string(),
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
