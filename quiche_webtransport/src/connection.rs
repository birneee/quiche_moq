use crate::error::Result;
use crate::error_code::{self, decode_stream_error};
use crate::pending_request::PendingRequest;
use crate::pending_response::PendingResponse;
use crate::protocol_negotiation::{encode_string_item, encode_string_list, parse_string_item, parse_string_list};
use crate::session::Session;
use crate::stream::Stream;
use crate::{Error, SessionId, PROTOCOL_HEADER_WEBTRANSPORT, PROTOCOL_HEADER_WEBTRANSPORT_H3, WT_CLOSE_SESSION};
use log::{debug, error, info, trace};
use quiche::h3;
use quiche::h3::NameValue;
use std::collections::{HashMap, VecDeque};

/// Stream-id keyed map on the per-tick hot path.
/// - FxHash for speed; QUIC stream ids are sequential, not freely peer-chosen
type FastMap<K, V> = HashMap<K, V, rustc_hash::FxBuildHasher>;
use url::Url;
use http_capsule::{Capsule, Decoded};
use quiche_h3_utils::{hdrs_to_strings, METHOD_CONNECT};

/// A Webtransport connections manages the state of one HTTP connection.
/// Managing multiple Webtransport sessions.
pub struct Connection {
    /// Established WebTransport sessions.
    /// Key is session id.
    sessions: HashMap<u64, Session>,
    pub(crate) streams: FastMap<u64, Stream>,
    /// Streams marked readable by session, until garbage collected; mirrors the sticky `Stream::readable`,
    /// not the edge-triggered `h3.readable_webtransport_streams()`, which would lose buffered data.
    readable_by_session: FastMap<u64, Vec<u64>>,
    /// Received datagram payloads by session, see `recv_datagram`.
    datagrams: FastMap<u64, VecDeque<Vec<u8>>>,
    /// Server: CONNECT requests that arrived before the client's SETTINGS (draft-ietf-webtrans-http3-16 §7.1).
    deferred_connects: Vec<(u64, Vec<h3::Header>)>,
    /// Server: CONNECT streams of sessions beyond the one we take (§5.1), reset by `poll_h3`.
    rejected_connects: Vec<u64>,
    perspective: Perspective,
}

/// Per session: an application not reading them loses the oldest.
const MAX_QUEUED_DATAGRAMS: usize = 1024;
/// Streams and datagrams kept for sessions not established yet (draft-ietf-webtrans-http3-16 §4.6).
const MAX_BUFFERED_STREAMS: usize = 32;
const MAX_BUFFERED_DATAGRAMS: usize = 64;
/// The CONNECT stream goes ahead of the session's streams: some peers drop the data of streams that arrive
/// before their session instead of buffering it (draft-ietf-webtrans-http3-16 §4.6).
const SESSION_STREAM_URGENCY: u8 = 0;

enum Perspective {
    Client {
        /// Pending requests for opening WebTransport sessions.
        /// Key is the session id.
        pending_requests: HashMap<u64, PendingRequest>,
    },
    Server {
        /// Pending responses to a WebTransport CONNECT request.
        /// Key is the session id.
        pending_responds: HashMap<u64, PendingResponse>,
    }
}



impl Connection {
    pub fn new(is_server: bool) -> Self {
        Self {
            sessions: HashMap::new(),
            streams: FastMap::default(),
            readable_by_session: FastMap::default(),
            datagrams: FastMap::default(),
            deferred_connects: Vec::new(),
            rejected_connects: Vec::new(),
            perspective: match is_server {
                true => Perspective::Server { pending_responds: HashMap::new() },
                false => Perspective::Client { pending_requests: HashMap::new() },
            },
        }
    }

    /// Sends a CONNECT request for a new WebTransport session.
    ///
    /// `available_protocols`, if non-empty, is sent as a `WT-Available-Protocols`
    /// header (draft-ietf-webtrans-http3 §3.3), most-preferred first. Read back
    /// the server's choice, once established, via `Connection::negotiated_protocol`.
    ///
    /// Returns session id.
    /// - the upgrade token of draft 16 if the server speaks it (draft-ietf-webtrans-http3-16 §3.2),
    ///   else the older one with the header of draft 2
    pub fn connect_session<F: quiche::BufFactory>(
        &mut self,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection<F>,
        url: Url,
        available_protocols: &[String],
    ) -> Result<SessionId> {
        let Perspective::Client { pending_requests } = &mut self.perspective else { panic!("Perspective is not client") };
        let draft_16 = crate::draft_16_by_peer(h3);
        let mut hdrs = vec![
                h3::Header::new(b":method", METHOD_CONNECT),
                h3::Header::new(b":protocol", if draft_16 { PROTOCOL_HEADER_WEBTRANSPORT_H3 } else { PROTOCOL_HEADER_WEBTRANSPORT }),
                h3::Header::new(b":scheme", url.scheme().as_bytes()),
                h3::Header::new(b":authority", url.authority().as_bytes()),
                h3::Header::new(b":path", url.path().as_bytes()),
        ];
        if !draft_16 {
            hdrs.push(h3::Header::new(b"sec-webtransport-http3-draft02", b"1"));
        }
        if !available_protocols.is_empty() {
            hdrs.push(h3::Header::new(b"wt-available-protocols", &encode_string_list(available_protocols)));
        }
        trace!("send {:?}", hdrs_to_strings(&hdrs));
        let session_id = h3.send_request(quic, &hdrs, false)?;
        quic.stream_priority(session_id, SESSION_STREAM_URGENCY, false)?;
        pending_requests
            .insert(session_id, PendingRequest::new(available_protocols.to_vec()));
        Ok(session_id)
    }

    /// Handles HTTP/3 events, call before [`Self::poll`].
    /// - headers establish sessions (`recv_hdrs`), a server's only once the client's SETTINGS arrived
    ///   (draft-ietf-webtrans-http3-16 §7.1)
    /// - the end of a session's CONNECT stream ends the session (§6)
    /// - an h3 error or a failed CONNECT closes the connection
    pub fn poll_h3<F: quiche::BufFactory>(&mut self, h3: &mut h3::Connection, quic: &mut quiche::Connection<F>) {
        loop {
            let failure = match h3.poll(quic) {
                Ok((stream_id, h3::Event::Headers { list, .. })) if self.awaits_peer_settings(h3) => {
                    self.deferred_connects.push((stream_id, list));
                    None
                }
                Ok((stream_id, h3::Event::Headers { list, .. })) => self.recv_hdrs(stream_id, &list).err(),
                Ok((stream_id, h3::Event::Data)) => self.recv_capsules(stream_id, h3, quic).err(),
                Ok((stream_id, h3::Event::Finished)) if self.is_session_stream(stream_id) => {
                    debug!("webtransport session {stream_id} ended, closing connection");
                    self.end_session_streams(stream_id, h3, quic);
                    // TODO end only this session once a connection carries several
                    quic.close(true, h3::WireErrorCode::NoError as u64, b"").ok();
                    return;
                }
                Ok((_, h3::Event::Finished | h3::Event::Reset(_) | h3::Event::PriorityUpdate | h3::Event::GoAway)) => None,
                Err(h3::Error::Done) => break,
                Err(e) => Some(Error::InvalidHeaders(format!("h3: {e:?}"))),
            };
            if let Some(e) = failure {
                self.close_connection(e, quic);
                return;
            }
        }
        if !self.awaits_peer_settings(h3) {
            for (stream_id, list) in std::mem::take(&mut self.deferred_connects) {
                if let Err(e) = self.recv_hdrs(stream_id, &list) {
                    self.close_connection(e, quic);
                    return;
                }
            }
        }
        for stream_id in std::mem::take(&mut self.rejected_connects) {
            debug!("rejecting webtransport session {stream_id}, one is established already");
            quic.stream_shutdown(stream_id, quiche::Shutdown::Write, h3::WireErrorCode::RequestRejected as u64).ok();
            quic.stream_shutdown(stream_id, quiche::Shutdown::Read, h3::WireErrorCode::RequestRejected as u64).ok();
        }
    }

    /// Streams of sessions not established yet, kept until they are (draft-ietf-webtrans-http3-16 §4.6).
    fn buffered_streams(&self) -> usize {
        self.streams.values().filter(|s| s.session_id.is_some_and(|id| !self.sessions.contains_key(&id))).count()
    }

    /// Datagrams of sessions not established yet, likewise.
    fn buffered_datagrams(&self) -> usize {
        self.datagrams.iter().filter(|(id, _)| !self.sessions.contains_key(id)).map(|(_, queue)| queue.len()).sum()
    }

    /// A server processes no WebTransport request before the client's SETTINGS (§7.1).
    fn awaits_peer_settings(&self, h3: &h3::Connection) -> bool {
        matches!(self.perspective, Perspective::Server { .. }) && h3.peer_settings_raw().is_none()
    }

    fn close_connection<F: quiche::BufFactory>(&mut self, e: Error, quic: &mut quiche::Connection<F>) {
        error!("closing connection: {e:?}");
        let code = match e {
            Error::InvalidCapsule(_) => h3::WireErrorCode::MessageError,
            _ => h3::WireErrorCode::GeneralProtocolError,
        };
        // quiche may have closed it already with a more specific code
        quic.close(true, code as u64, b"").ok();
    }

    /// Resets and stops reading the streams of the terminated `session_id` with WT_SESSION_GONE,
    /// and forgets its datagrams (§6).
    fn end_session_streams<F: quiche::BufFactory>(&mut self, session_id: u64, h3: &mut h3::Connection, quic: &mut quiche::Connection<F>) {
        let ended: Vec<(u64, Stream)> = self.streams.extract_if(|_, s| s.session_id == Some(session_id)).collect();
        for (stream_id, stream) in ended {
            // a direction a uni stream lacks fails harmlessly
            if !stream.send_ended {
                h3.shutdown_webtransport_stream(quic, stream_id, quiche::Shutdown::Write, crate::WT_SESSION_GONE).ok();
            }
            if !stream.recv_ended {
                h3.shutdown_webtransport_stream(quic, stream_id, quiche::Shutdown::Read, crate::WT_SESSION_GONE).ok();
            }
        }
        self.readable_by_session.remove(&session_id);
        self.datagrams.remove(&session_id);
    }

    /// Reads the capsules on a session's CONNECT stream, drains any other request stream.
    /// - WT_CLOSE_SESSION is answered with our FIN, it ends the session's streams (draft-ietf-webtrans-http3-16 §6)
    /// - data after WT_CLOSE_SESSION or a malformed one is an error
    fn recv_capsules<F: quiche::BufFactory>(&mut self, stream_id: u64, h3: &mut h3::Connection, quic: &mut quiche::Connection<F>) -> Result<()> {
        let mut buf = [0u8; 1024];
        let Some(session) = self.sessions.get_mut(&stream_id) else {
            while h3.recv_body(quic, stream_id, &mut buf).is_ok() {}
            return Ok(());
        };
        let mut closed = false;
        while let Ok(len) = h3.recv_body(quic, stream_id, &mut buf) {
            if session.peer_close.is_some() {
                return Err(Error::InvalidCapsule(format!("data after WT_CLOSE_SESSION on session {stream_id}")));
            }
            session.capsules.feed(&buf[..len]);
            while let Some(decoded) = session.capsules.next(|ty| ty == WT_CLOSE_SESSION).map_err(|e| Error::InvalidCapsule(format!("{e:?}")))? {
                if session.peer_close.is_some() {
                    return Err(Error::InvalidCapsule(format!("capsule after WT_CLOSE_SESSION on session {stream_id}")));
                }
                let Decoded::Kept(close) = decoded else { continue };
                session.peer_close = Some(parse_close(&close)?);
                info!("peer closed webtransport session {stream_id}: {:?}", session.peer_close);
                // our close is already under way if this fails
                h3.send_body(quic, stream_id, &[], true).ok();
                closed = true;
            }
        }
        if closed {
            self.end_session_streams(stream_id, h3, quic);
        }
        Ok(())
    }

    /// Error code and message the peer closed `session_id` with, if it sent WT_CLOSE_SESSION.
    pub fn peer_close(&self, session_id: SessionId) -> Option<(u32, &str)> {
        self.sessions.get(&session_id)?.peer_close.as_ref().map(|(code, message)| (*code, message.as_str()))
    }

    fn is_session_stream(&self, stream_id: u64) -> bool {
        self.sessions.contains_key(&stream_id)
            || matches!(&self.perspective, Perspective::Client { pending_requests } if pending_requests.contains_key(&stream_id))
    }

    /// Processes WebTransport data from HTTP/3.
    pub fn poll<F: quiche::BufFactory>(&mut self, h3: &mut h3::Connection, quic: &mut quiche::Connection<F>) {
        self.recv_datagrams(quic);
        for stream_id in h3.readable_webtransport_streams(quic).collect::<Vec<_>>() {
            let stream = match self.streams.get_mut(&stream_id) {
                Some(s) => s,
                None => {
                    self.streams
                        .insert(stream_id, Stream::new_remote(stream_id));
                    self.streams.get_mut(&stream_id).unwrap()
                }
            };
            match stream.read_session_id(quic, h3) {
                Ok(_) => {}
                Err(Error::Done) => continue,
                // ended before its session ID was read, reading was stopped (`stream_recv_ended`), or failed
                Err(e) => {
                    debug!("dropping webtransport stream {stream_id}: {e:?}");
                    self.streams.remove(&stream_id);
                    continue;
                }
            };
            if stream.readable { continue; }
            let session_id = stream.session_id.unwrap();
            if !self.sessions.contains_key(&session_id) && self.buffered_streams() > MAX_BUFFERED_STREAMS {
                debug!("rejecting webtransport stream {stream_id} of session {session_id}, too many buffered");
                self.streams.remove(&stream_id);
                for direction in [quiche::Shutdown::Read, quiche::Shutdown::Write] {
                    h3.shutdown_webtransport_stream(quic, stream_id, direction, crate::WT_BUFFERED_STREAM_REJECTED).ok();
                }
                continue;
            }
            self.streams.get_mut(&stream_id).unwrap().readable = true;
            self.readable_by_session.entry(session_id).or_default().push(stream_id);
        }
        if let Perspective::Server { pending_responds } = &mut self.perspective {
            pending_responds.retain(|_, resp| {
                let mut hdrs = vec![
                    h3::Header::new(b":status", b"200"),
                    h3::Header::new(b"sec-webtransport-http3-draft", b"draft02"),
                ];
                let protocol_hdr;
                if let Some(protocol) = resp.response_protocol() {
                    protocol_hdr = encode_string_item(protocol);
                    hdrs.push(h3::Header::new(b"wt-protocol", &protocol_hdr));
                }
                let priority = h3::Priority::new(SESSION_STREAM_URGENCY, false);
                if let Err(e) = h3.send_response_with_priority(quic, resp.session_id(), &hdrs, &priority, false) {
                    error!("WebTransport session {} not answered: {e:?}", resp.session_id());
                }
                false
            });
        }
    }

    /// return readable sessions ids
    pub fn readable_sessions(&self) -> Vec<u64> {
        let mut readable = vec![];
        for stream in self.streams.values() {
            if let Some(session_id) = stream.session_id {
                readable.push(session_id);
            }
        }
        readable.sort_unstable();
        readable.dedup();
        readable
    }

    #[cfg(test)]
    pub(crate) fn stream_count(&self) -> usize {
        self.streams.len()
    }

    /// Our side of `stream_id` sends nothing more: FIN sent or reset.
    pub fn stream_send_ended(&mut self, stream_id: u64) {
        if let Some(stream) = self.streams.get_mut(&stream_id) {
            stream.send_ended = true;
        }
        self.collect_stream(stream_id);
    }

    /// Nothing more to read from `stream_id`: it ended, was reset, or reading
    /// was stopped (quiche drops what still arrives).
    pub fn stream_recv_ended(&mut self, stream_id: u64) {
        let Some(stream) = self.streams.get_mut(&stream_id) else { return };
        stream.recv_ended = true;
        if let Some(session_id) = stream.session_id
            && let Some(readable) = self.readable_by_session.get_mut(&session_id)
        {
            readable.retain(|&id| id != stream_id);
        }
        self.collect_stream(stream_id);
    }

    /// Forgets `stream_id` once both directions ended.
    fn collect_stream(&mut self, stream_id: u64) {
        if self.streams.get(&stream_id).is_some_and(|s| s.send_ended && s.recv_ended) {
            trace!("garbage collect stream {}", stream_id);
            self.streams.remove(&stream_id);
        }
    }

    /// Sorts the HTTP Datagrams received by session (RFC 9297 §2.1), buffering those of sessions not
    /// established yet (draft-ietf-webtrans-http3-16 §4.6).
    fn recv_datagrams<F: quiche::BufFactory>(&mut self, quic: &mut quiche::Connection<F>) {
        while let Some(len) = quic.dgram_recv_front_len() {
            let mut buf = vec![0; len];
            if quic.dgram_recv(&mut buf).is_err() { break; }
            let mut b = octets::Octets::with_slice(&buf);
            let Ok(quarter_stream_id) = b.get_varint() else { continue };
            // RFC 9297 §2.1
            if quarter_stream_id >= 1 << 60 {
                error!("closing connection: Quarter Stream ID {quarter_stream_id} beyond 2^60");
                quic.close(true, crate::H3_DATAGRAM_ERROR, b"").ok();
                return;
            }
            let (header_len, session_id) = (b.off(), quarter_stream_id * 4);
            if !self.sessions.contains_key(&session_id) && self.buffered_datagrams() >= MAX_BUFFERED_DATAGRAMS {
                trace!("dropping datagram of session {session_id}, too many buffered");
                continue;
            }
            let queue = self.datagrams.entry(session_id).or_default();
            if queue.len() == MAX_QUEUED_DATAGRAMS {
                queue.pop_front();
            }
            buf.drain(..header_len);
            queue.push_back(buf);
        }
    }

    /// The next HTTP Datagram payload received on `session_id`.
    pub fn recv_datagram(&mut self, session_id: SessionId) -> Option<Vec<u8>> {
        self.datagrams.get_mut(&session_id)?.pop_front()
    }

    /// Sends `payload` as an HTTP Datagram of `session_id` (RFC 9297 §2.1).
    /// - [`Error::Done`] if it doesn't fit into a datagram or the send queue is full
    pub fn send_datagram<F: quiche::BufFactory>(&mut self, session_id: SessionId, quic: &mut quiche::Connection<F>, payload: &[u8]) -> Result<()> {
        let quarter_stream_id = session_id / 4;
        let mut datagram = vec![0; octets::varint_len(quarter_stream_id) + payload.len()];
        let header_len = octets::OctetsMut::with_slice(&mut datagram).put_varint(quarter_stream_id).expect("sized for it").len();
        datagram[header_len..].copy_from_slice(payload);
        match quic.dgram_send(&datagram) {
            Ok(()) => Ok(()),
            Err(quiche::Error::Done | quiche::Error::BufferTooShort) => Err(Error::Done),
            Err(e) => Err(Error::Transport(h3::Error::TransportError(e))),
        }
    }

    /// The largest HTTP Datagram payload `session_id` can send now, `None` if the peer takes no datagrams.
    pub fn max_datagram_payload<F: quiche::BufFactory>(&self, session_id: SessionId, h3: &h3::Connection, quic: &quiche::Connection<F>) -> Option<usize> {
        if !h3.dgram_enabled_by_peer(quic) { return None; }
        quic.dgram_max_writable_len()?.checked_sub(octets::varint_len(session_id / 4))
    }

    /// return readable streams of a session
    pub fn readable_streams(&self, session_id: u64) -> Vec<u64> {
        self.readable_by_session
            .get(&session_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Open a new WebTransport stream.
    ///
    /// Returns [`Error::InsufficientCapacity`] in three distinct cases:
    /// - `h3::Error::StreamBlocked`: the QUIC MAX_STREAMS limit has been reached; the remote
    ///   must extend the limit before a new stream can be opened.
    /// - `h3::Error::Done`: the QUIC send buffer is transiently full (flow control backpressure);
    ///   retrying on the next send opportunity will succeed once the buffer drains.
    /// - `h3::Error::TransportError(quiche::Error::StreamLimit)`: same as `StreamBlocked` but
    ///   surfaced through the transport-error path instead of h3's own bookkeeping; also
    ///   transient, resolved once the remote sends a MAX_STREAMS update.
    pub fn open_stream<F: quiche::BufFactory>(
        &mut self,
        session_id: u64,
        h3_conn: &mut h3::Connection,
        quic_conn: &mut quiche::Connection<F>,
        bidi: bool,
    ) -> Result<u64> {
        // client may open streams optimistically before the CONNECT response (draft-ietf-webtrans-http3-16 §4.2)
        let pending = matches!(&self.perspective, Perspective::Client { pending_requests } if pending_requests.contains_key(&session_id));
        if !self.sessions.contains_key(&session_id) && !pending {
            return Err(Error::MissingSessionID);
        }
        let stream_id = match h3_conn.open_webtransport_stream(quic_conn, bidi) {
            Ok(v) => v,
            Err(h3::Error::Done | h3::Error::StreamBlocked) => return Err(Error::InsufficientCapacity),
            Err(e) => return Err(e.into()),
        };
        let mut stream = Stream::with_session(stream_id, session_id);
        // without capacity yet, it goes with the first data
        match stream.send_session_id(h3_conn, quic_conn) {
            Ok(()) | Err(Error::Done) => {}
            Err(e) => return Err(e),
        }
        self.streams.insert(stream_id, stream);
        trace!("opened stream {}", stream_id);
        Ok(stream_id)
    }

    /// recv stream data from h3.
    /// returns None if the stream is not ready for the application to consume
    /// returns the session_id when the stream is ready
    /// stream data must start after the h3 stream type.
    pub fn recv_stream<F: quiche::BufFactory>(
        &mut self,
        stream_id: u64,
        session_id: u64,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection<F>,
        b: &mut [u8],
    ) -> crate::error::Result<usize> {
        let stream = self
            .streams
            .get_mut(&stream_id)
            .ok_or(Error::NoStreamState)?;
        assert_eq!(stream.session_id, Some(session_id));
        let result = stream.recv(h3, quic, b).map_err(decode_stream_error);
        // Fin, reset, or already collected by quiche (e.g. after STOP_SENDING)
        if matches!(result, Err(Error::Fin | Error::StreamReset(_) | Error::InvalidStreamState(_))) {
            self.stream_recv_ended(stream_id);
        }
        let len = result?;
        trace!("recv on stream {}: {:?}", stream_id, &b[..len]);
        Ok(len)
    }

    /// Return if the WebTransport session has been established.
    /// https://www.ietf.org/archive/id/draft-ietf-webtrans-http3-11.html#name-creating-a-new-session
    pub fn established(&self, session_id: u64) -> bool {
        self.sessions.contains_key(&session_id)
    }

    /// Receive headers from h3.
    /// Must be called to establish sessions.
    pub fn recv_hdrs(&mut self, stream_id: u64, headers: &Vec<h3::Header>) -> Result<()> {
        let mut status: Option<[u8;3]> = None;
        let mut method_connect = false;
        let mut protocol_webtransport = false;
        let mut available_protocols: Vec<String> = vec![];
        let mut selected_protocol: Option<String> = None;
        for header in headers {
            match (header.name(), header.value()) {
                (b":status", s) => {
                    let s = <[u8; 3]>::try_from(s).map_err(|_| Error::InvalidHeaders(format!("malformed status {s:?}")))?;
                    status = Some(s);
                }
                (b":method", b"CONNECT") => method_connect = true,
                // "webtransport-h3" in recent drafts (draft-ietf-webtrans-http3-16 §3.1), "webtransport" before
                (b":protocol", b"webtransport" | b"webtransport-h3") => protocol_webtransport = true,
                (b"wt-available-protocols", v) => available_protocols = parse_string_list(v),
                (b"wt-protocol", v) => selected_protocol = parse_string_item(v),
                _ => debug!("ignore header {:?}", header),
            }
        }

        trace!("{:?}", hdrs_to_strings(headers));

        match &mut self.perspective {
            Perspective::Server { pending_responds } => {
                // only Chrome's draft 2 era clients name their draft, the response names it for them
                if method_connect && protocol_webtransport {
                    // without flow control a connection carries one session (draft-ietf-webtrans-http3-16 §5.1)
                    // TODO flow control, for several sessions per connection
                    if !self.sessions.is_empty() {
                        self.rejected_connects.push(stream_id);
                        return Ok(());
                    }
                    self.sessions.insert(stream_id, Session::accept(stream_id, None));
                    pending_responds
                        .insert(stream_id, PendingResponse::new(stream_id, available_protocols));
                }
            }
            Perspective::Client { pending_requests } => {
                let req = pending_requests
                    .remove(&stream_id)
                    .ok_or_else(|| Error::InvalidHeaders(format!("headers on stream {stream_id} without a CONNECT")))?;
                assert_eq!(req.session_id(), stream_id);
                // established by any 2xx (draft-ietf-webtrans-http3-16 §3.2), naming the draft is Chrome's draft 2 era
                if status.is_none_or(|s| s[0] != b'2') {
                    return Err(Error::InvalidHeaders(format!("CONNECT rejected: {:?}", hdrs_to_strings(headers))));
                }
                let session_id = stream_id;
                debug!("webtransport session {} established", session_id);
                // draft-ietf-webtrans-http3 §3.3: the server's `WT-Protocol`
                // must be one of what we offered, if we offered anything.
                let negotiated_protocol = selected_protocol.filter(|p| {
                    req.available_protocols().is_empty() || req.available_protocols().contains(p)
                });
                self.sessions.insert(session_id, Session::accept(session_id, negotiated_protocol));
            }
        }
        Ok(())
    }

    /// The protocol negotiated for `session_id` via `WT-Available-Protocols`/
    /// `WT-Protocol` (draft-ietf-webtrans-http3 §3.3), if any. `None` if
    /// either side didn't participate in that negotiation (e.g. an older
    /// peer, or a session that doesn't need it).
    pub fn negotiated_protocol(&self, session_id: u64) -> Option<&str> {
        self.sessions.get(&session_id).and_then(Session::negotiated_protocol)
    }

    /// The protocols the client offered via `WT-Available-Protocols` on the
    /// still-pending CONNECT request for `session_id`, most preferred first
    /// — empty if it sent none. Server-side only; use this to pick a value
    /// for `set_negotiated_protocol` before the response goes out (`poll`).
    pub fn pending_available_protocols(&self, session_id: u64) -> &[String] {
        match &self.perspective {
            Perspective::Server { pending_responds } => pending_responds
                .get(&session_id)
                .map(PendingResponse::available_protocols)
                .unwrap_or(&[]),
            Perspective::Client { .. } => &[],
        }
    }

    /// Picks `protocol` as the value to echo back in the pending response's
    /// `WT-Protocol` header (server-side only) — should be one of the values
    /// from `pending_available_protocols`. Takes effect on the next `poll`.
    pub fn set_negotiated_protocol(&mut self, session_id: u64, protocol: String) {
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.set_negotiated_protocol(protocol.clone());
        }
        if let Perspective::Server { pending_responds } = &mut self.perspective
            && let Some(resp) = pending_responds.get_mut(&session_id)
        {
            resp.set_response_protocol(protocol);
        }
    }

    /// A stream to send on. One forgotten ended both ways or with its session (`end_session_streams`),
    /// a send on it fails as stopped with WT_SESSION_GONE.
    fn sending_stream(&mut self, stream_id: u64) -> Result<&mut Stream> {
        self.streams.get_mut(&stream_id).ok_or(Error::StreamStopped(crate::WT_SESSION_GONE))
    }

    pub fn stream_capacity<F: quiche::BufFactory>(
        &mut self,
        stream_id: u64,
        quic: &mut quiche::Connection<F>,
    ) -> Result<usize> {
        self.sending_stream(stream_id)?.capacity(quic).map_err(decode_stream_error)
    }

    pub fn stream_send<F: quiche::BufFactory>(
        &mut self,
        stream_id: u64,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection<F>,
        buf: &[u8],
        fin: bool,
    ) -> Result<usize> {
        trace!("send on {} bytes on stream {}", buf.len(), stream_id);
        let sent = self.sending_stream(stream_id)?.send(h3, quic, buf, fin).map_err(decode_stream_error)?;
        if fin && sent == buf.len() {
            self.stream_send_ended(stream_id);
        }
        Ok(sent)
    }

    /// does not send buf partially.
    pub fn stream_send_if_capacity<F: quiche::BufFactory>(
        &mut self,
        stream_id: u64,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection<F>,
        buf: &[u8],
        fin: bool,
    ) -> Result<()> {
        self.sending_stream(stream_id)?.send_if_capacity(h3, quic, buf, fin).map_err(decode_stream_error)?;
        if fin {
            self.stream_send_ended(stream_id);
        }
        Ok(())
    }

    /// Resets (`Shutdown::Write`) or stops reading (`Shutdown::Read`) a data stream with application error `code`.
    // TODO RESET_STREAM_AT with the stream header as reliable size (draft-ietf-webtrans-http3-16 §4.4),
    //  RESET_STREAM may discard the header that associates the stream with its session; needs the frame and
    //  the empty reset_stream_at transport parameter in quiche, required for draft 16 compliance but harmless
    //  with one session per connection, implement when needed
    pub fn shutdown_stream<F: quiche::BufFactory>(
        &mut self,
        stream_id: u64,
        direction: quiche::Shutdown,
        code: u32,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection<F>,
    ) -> Result<()> {
        h3.shutdown_webtransport_stream(quic, stream_id, direction, error_code::to_http3(code))?;
        Ok(())
    }

    pub fn session_ids(&self) -> Vec<u64> {
        self.sessions.keys().cloned().collect()
    }

    /// Sends WT_CLOSE_SESSION with a FIN (draft-ietf-webtrans-http3-16 §6).
    /// - no-op for a session that is unknown or closed already
    /// - `error_msg` is truncated to 1024 bytes
    /// - resets the CONNECT stream if the capsule doesn't fit, that ends the session too
    pub fn close_session<F: quiche::BufFactory>(&mut self, session_id: SessionId, error_code: u32, error_msg: &str, quic: &mut quiche::Connection<F>, h3: &mut h3::Connection) -> Result<()> {
        if self.sessions.remove(&session_id).is_none() {
            return Ok(());
        }
        self.end_session_streams(session_id, h3, quic);
        let message = truncate_utf8(error_msg, MAX_CLOSE_MESSAGE_LEN);
        let mut value = error_code.to_be_bytes().to_vec();
        value.extend_from_slice(message.as_bytes());
        let mut buf = [0u8; 4 + MAX_CLOSE_MESSAGE_LEN + 16];
        let len = Capsule::new(WT_CLOSE_SESSION, value).encode(&mut buf)?;
        match h3.send_body(quic, session_id, &buf[..len], true) {
            Ok(sent) if sent == len => Ok(()),
            Ok(_) | Err(h3::Error::Done) => {
                quic.stream_shutdown(session_id, quiche::Shutdown::Write, h3::WireErrorCode::NoError as u64).ok();
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// Longest Application Error Message of WT_CLOSE_SESSION (draft-ietf-webtrans-http3-16 §6).
pub(crate) const MAX_CLOSE_MESSAGE_LEN: usize = 1024;

/// At most `max` bytes of `s`, cut at a character boundary.
pub(crate) fn truncate_utf8(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Error code and message of a WT_CLOSE_SESSION capsule.
fn parse_close(close: &Capsule) -> Result<(u32, String)> {
    let (code, message) = close.value().split_first_chunk::<4>()
        .ok_or_else(|| Error::InvalidCapsule(format!("WT_CLOSE_SESSION of {} bytes", close.value().len())))?;
    Ok((u32::from_be_bytes(*code), String::from_utf8_lossy(message).into_owned()))
}
