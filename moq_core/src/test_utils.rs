//! In-memory two-endpoint harness for moq_core-level tests: two
//! `MoqTransportSession`s (client/server) driven directly over a mock
//! `Transport`, without a real QUIC connection. Mirrors
//! `quiche::test_utils::Pipe`, one layer up.

use crate::prelude::*;

use crate::config::Config;
use crate::handles::{Publication, Subscription};
use crate::request::{IncomingRequest, Publish, PublishNamespace, Request, Subscribe, SubscribeOk};
use crate::session::{Event, MoqTransportSession};
use crate::time::Timestamp;
use crate::transport::{StreamError, Transport};
use moq_wire::control_message::SubscribeOkMessage;
use crate::RequestFailure;
use moq_wire::{RequestId, ResetCode, SessionCloseCode, TrackAlias};
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use core::cell::RefCell;
use hashbrown::{HashMap, HashSet};

/// One direction of a stream's data. A bidi stream is two independent
/// `Half`s sharing one ID; a uni stream only ever uses one of them.
#[derive(Default)]
struct Half {
    /// Written by `send_stream`/`send_stream_if_capacity`, staged until `Network::flush`.
    staged: VecDeque<u8>,
    /// Delivered to the peer's `recv_stream`.
    visible: VecDeque<u8>,
    fin_staged: bool,
    fin_visible: bool,
    reset_staged: Option<u64>,
    reset_visible: Option<u64>,
}

#[derive(Default)]
struct StreamState {
    from_client: Half,
    from_server: Half,
}

#[derive(Default)]
struct Network {
    streams: HashMap<u64, StreamState>,
    /// Datagrams sent by the client and the server, staged until `flush`.
    datagrams_staged: [VecDeque<Vec<u8>>; 2],
    datagrams_visible: [VecDeque<Vec<u8>>; 2],
}

impl Network {
    /// Moves every stream's staged writes into its visible half — the mock
    /// equivalent of one packet round trip.
    fn flush(&mut self) {
        for (staged, visible) in self.datagrams_staged.iter_mut().zip(&mut self.datagrams_visible) {
            visible.extend(staged.drain(..));
        }
        for s in self.streams.values_mut() {
            for half in [&mut s.from_client, &mut s.from_server] {
                half.visible.extend(half.staged.drain(..));
                half.fin_visible |= half.fin_staged;
                half.reset_visible = half.reset_visible.or(half.reset_staged);
            }
        }
    }
}

/// `Transport` impl for one side of a `MockPipe`. Stream IDs follow QUIC's
/// own low-bit convention (initiator, bidi/uni) purely so both sides land
/// on the same ID for the same logical stream without extra coordination.
pub struct MockTransport {
    is_server: bool,
    next_uni: u64,
    next_bidi: u64,
    net: Rc<RefCell<Network>>,
    /// Error code of `close_session`, if called.
    pub closed_with: Option<SessionCloseCode>,
    /// Last urgency `set_stream_priority` set per stream.
    pub urgencies: HashMap<u64, u8>,
    /// Streams the peer stopped (STOP_SENDING): sending on them fails.
    pub stopped: HashSet<u64>,
    /// The peer's stream limit is reached: opening streams fails.
    pub streams_blocked: bool,
}

impl MockTransport {
    fn new(is_server: bool, net: Rc<RefCell<Network>>) -> Self {
        Self {
            is_server,
            next_uni: if is_server { 3 } else { 2 },
            next_bidi: if is_server { 1 } else { 0 },
            net,
            closed_with: None,
            urgencies: HashMap::new(),
            stopped: HashSet::new(),
            streams_blocked: false,
        }
    }

    /// This side's own outgoing half of `stream_id`.
    fn own_half(s: &mut StreamState, is_server: bool) -> &mut Half {
        if is_server { &mut s.from_server } else { &mut s.from_client }
    }

    /// The peer's half of `stream_id` — what this side reads.
    fn peer_half(s: &mut StreamState, is_server: bool) -> &mut Half {
        if is_server { &mut s.from_client } else { &mut s.from_server }
    }

    /// Delivers both sides' pending writes.
    pub fn deliver(&self) {
        self.net.borrow_mut().flush();
    }

    /// Every stream but the uni streams this side opened, it never reads those.
    pub fn readable_streams(&self) -> Vec<u64> {
        let is_own_uni = |id: u64| id & 0x2 != 0 && (id & 0x1 == 1) == self.is_server;
        self.net.borrow().streams.keys().copied().filter(|&id| !is_own_uni(id)).collect()
    }
}

/// A client's and a server's transport over one network, without sessions.
pub fn mock_transports() -> (MockTransport, MockTransport) {
    let net = Rc::new(RefCell::new(Network::default()));
    (MockTransport::new(false, net.clone()), MockTransport::new(true, net))
}

impl Transport for MockTransport {
    fn recv_stream(&mut self, stream_id: u64, buf: &mut [u8]) -> core::result::Result<usize, StreamError> {
        let mut net = self.net.borrow_mut();
        let half = Self::peer_half(net.streams.entry(stream_id).or_default(), self.is_server);
        if !half.visible.is_empty() {
            let n = half.visible.len().min(buf.len());
            for b in buf.iter_mut().take(n) { *b = half.visible.pop_front().unwrap(); }
            return Ok(n);
        }
        if let Some(code) = half.reset_visible { return Err(StreamError::StreamReset(code)); }
        if half.fin_visible { return Err(StreamError::Fin); }
        Err(StreamError::Done)
    }

    fn send_stream(&mut self, stream_id: u64, data: &[u8], fin: bool) -> core::result::Result<usize, StreamError> {
        if self.stopped.contains(&stream_id) {
            return Err(StreamError::StreamStopped(0));
        }
        let mut net = self.net.borrow_mut();
        let half = Self::own_half(net.streams.entry(stream_id).or_default(), self.is_server);
        half.staged.extend(data.iter().copied());
        half.fin_staged |= fin;
        Ok(data.len())
    }

    fn send_stream_if_capacity(&mut self, stream_id: u64, data: &[u8], fin: bool) -> core::result::Result<usize, StreamError> {
        self.send_stream(stream_id, data, fin)
    }

    fn open_uni_stream(&mut self) -> core::result::Result<u64, StreamError> {
        if self.streams_blocked {
            return Err(StreamError::Done);
        }
        let id = self.next_uni;
        self.next_uni += 4;
        Ok(id)
    }

    fn open_bidi_stream(&mut self) -> core::result::Result<u64, StreamError> {
        if self.streams_blocked {
            return Err(StreamError::Done);
        }
        let id = self.next_bidi;
        self.next_bidi += 4;
        Ok(id)
    }

    fn set_stream_priority(&mut self, stream_id: u64, urgency: u8, _incremental: bool) {
        self.urgencies.insert(stream_id, urgency);
    }

    fn reset_stream(&mut self, stream_id: u64, code: ResetCode) {
        let mut net = self.net.borrow_mut();
        let half = Self::own_half(net.streams.entry(stream_id).or_default(), self.is_server);
        half.staged.clear();
        half.reset_staged = Some(code.into());
    }

    /// Ignored, like moq_web and moq_tokio do: the writer isn't blocked.
    fn stop_sending(&mut self, _stream_id: u64, _code: ResetCode) -> bool {
        false
    }

    fn stream_stopped(&mut self, stream_id: u64) -> bool {
        self.stopped.contains(&stream_id)
    }

    fn stream_finished(&self, stream_id: u64) -> bool {
        let mut net = self.net.borrow_mut();
        Self::own_half(net.streams.entry(stream_id).or_default(), self.is_server).fin_visible
    }

    fn send_datagram(&mut self, datagram: &[u8]) -> core::result::Result<(), StreamError> {
        self.net.borrow_mut().datagrams_staged[self.is_server as usize].push_back(datagram.to_vec());
        Ok(())
    }

    fn recv_datagram(&mut self) -> Option<Vec<u8>> {
        self.net.borrow_mut().datagrams_visible[!self.is_server as usize].pop_front()
    }

    fn close_session(&mut self, code: SessionCloseCode, _reason: &str) {
        self.closed_with = Some(code);
    }
}

/// Two connected `MoqTransportSession`s over an in-memory mock transport —
/// for testing moq_core protocol behavior without a real QUIC connection.
pub struct MockPipe {
    pub client_t: MockTransport,
    pub server_t: MockTransport,
    pub client: MoqTransportSession,
    pub server: MoqTransportSession,
    /// Kept alive for the test, dropping them would end them.
    pub subscriptions: Vec<Subscription>,
    pub publications: Vec<Publication>,
    /// Passed to both sessions' `handle()`; tests move it forward instead of sleeping.
    pub now: Timestamp,
}

impl MockPipe {
    /// Delivers both sides' pending writes without letting the sessions read them.
    pub fn deliver(&mut self) {
        self.client_t.deliver();
    }

    /// Flushes both sides' pending writes onto the wire, delivers them, then
    /// lets each session process what just arrived — one full round trip.
    pub fn advance(&mut self) {
        self.client.send_padding(&mut self.client_t);
        self.server.send_padding(&mut self.server_t);
        self.deliver();
        let (client_readable, server_readable) = (self.client_t.readable_streams(), self.server_t.readable_streams());
        self.client.handle(&mut self.client_t, &client_readable, self.now);
        self.server.handle(&mut self.server_t, &server_readable, self.now);
    }
}

/// Builds a `MockPipe` with a completed SETUP handshake.
pub fn init_mock_pipe(config: Config) -> MockPipe {
    let mut p = connect_mock_pipe(config.clone(), config);
    assert!(p.server.initialized(), "server must be initialized after the SETUP round trip");
    p.advance();
    assert!(p.client.initialized(), "client must be initialized after SERVER_SETUP is delivered");
    p
}

/// Sends CLIENT_SETUP and lets the server process it.
pub fn connect_mock_pipe(client_config: Config, server_config: Config) -> MockPipe {
    let mut p = unconnected_mock_pipe(client_config, server_config);
    p.advance();
    p
}

/// The client's SETUP is sent, not delivered yet.
pub fn unconnected_mock_pipe(client_config: Config, server_config: Config) -> MockPipe {
    let (mut client_t, server_t) = mock_transports();
    let client = MoqTransportSession::connect(&mut client_t, client_config).unwrap();
    let server = MoqTransportSession::accept(server_config).unwrap();
    MockPipe { client_t, server_t, client, server, subscriptions: vec![], publications: vec![], now: Timestamp::default() }
}

/// Subscribes from the client and accepts from the server — shared setup
/// for tests that need an established out_track to send Objects on.
pub fn subscribed_track(p: &mut MockPipe) -> TrackAlias {
    let subscription = p.client
        .subscribe(&"n1--t1".parse().unwrap(), crate::SubscribeOptions::default(), &mut p.client_t)
        .unwrap();
    p.advance();
    let request = next_subscribe(&mut p.server);
    let publication = p.server.accept_request(request, SubscribeOk::default(), &mut p.server_t).unwrap();
    let track_alias = publication.track_alias();
    p.advance();
    subscribe_response(&mut p.client, subscription.request_id()).unwrap().unwrap();
    p.subscriptions.push(subscription);
    p.publications.push(publication);
    track_alias
}

/// `s`'s next request, which must be a SUBSCRIBE.
pub fn next_subscribe(s: &mut MoqTransportSession) -> Request<Subscribe> {
    match next_request(s) {
        Some(IncomingRequest::Subscribe(r)) => r,
        _ => panic!("expected a SUBSCRIBE"),
    }
}

/// `s`'s next request, which must be a PUBLISH.
pub fn next_publish(s: &mut MoqTransportSession) -> Request<Publish> {
    match next_request(s) {
        Some(IncomingRequest::Publish(r)) => r,
        _ => panic!("expected a PUBLISH"),
    }
}

/// `s`'s next request, which must be a PUBLISH_NAMESPACE.
pub fn next_publish_namespace(s: &mut MoqTransportSession) -> Request<PublishNamespace> {
    match next_request(s) {
        Some(IncomingRequest::PublishNamespace(r)) => r,
        _ => panic!("expected a PUBLISH_NAMESPACE"),
    }
}

/// Pops `s`'s events up to the first one `f` maps to `Some`.
pub fn find_event<T>(s: &mut MoqTransportSession, mut f: impl FnMut(Event) -> Option<T>) -> Option<T> {
    while let Some(event) = s.next_event() {
        if let Some(found) = f(event) {
            return Some(found);
        }
    }
    None
}

pub fn next_request(s: &mut MoqTransportSession) -> Option<IncomingRequest> {
    find_event(s, |e| match e {
        Event::Request(r) => Some(r),
        _ => None,
    })
}

/// The answer to our SUBSCRIBE `request_id`.
pub fn subscribe_response(s: &mut MoqTransportSession, request_id: RequestId) -> Option<Result<(TrackAlias, SubscribeOkMessage), RequestFailure>> {
    find_event(s, |e| match e {
        Event::SubscribeOk { request_id: id, track_alias, message } if id == request_id => Some(Ok((track_alias, message))),
        Event::RequestFailed { request_id: id, failure } if id == request_id => Some(Err(failure)),
        _ => None,
    })
}
