use crate::error::Result;
use crate::pending_request::PendingRequest;
use crate::pending_response::PendingResponse;
use crate::protocol_negotiation::{encode_string_item, encode_string_list, parse_string_item, parse_string_list};
use crate::session::Session;
use crate::stream::Stream;
use crate::{Error, SessionId, PROTOCOL_HEADER_WEBTRANSPORT, WT_CLOSE_SESSION};
use log::{debug, trace};
use quiche::h3;
use quiche::h3::NameValue;
use std::collections::HashMap;
use octets::OctetsMut;
use url::Url;
use http_capsule::Capsule;
use quiche_h3_utils::{hdrs_to_strings, METHOD_CONNECT};

/// A Webtransport connections manages the state of one HTTP connection.
/// Managing multiple Webtransport sessions.
pub struct Connection {
    /// Established WebTransport sessions.
    /// Key is session id.
    sessions: HashMap<u64, Session>,
    pub(crate) streams: HashMap<u64, Stream>,
    closed: bool,
    perspective: Perspective,
}

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
            streams: HashMap::new(),
            perspective: match is_server {
                true => Perspective::Server { pending_responds: HashMap::new() },
                false => Perspective::Client { pending_requests: HashMap::new() },
            },
            closed: false,
        }
    }

    /// Sends a CONNECT request for a new WebTransport session.
    ///
    /// `available_protocols`, if non-empty, is sent as a `WT-Available-Protocols`
    /// header (draft-ietf-webtrans-http3 §3.3), most-preferred first. Read back
    /// the server's choice, once established, via `Connection::negotiated_protocol`.
    ///
    /// Returns session id.
    pub fn connect_session<F: quiche::BufFactory>(
        &mut self,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection<F>,
        url: Url,
        available_protocols: &[String],
    ) -> SessionId {
        let Perspective::Client { pending_requests } = &mut self.perspective else { panic!("Perspective is not client") };
        let mut hdrs = vec![
                h3::Header::new(b":method", METHOD_CONNECT),
                h3::Header::new(b":protocol", PROTOCOL_HEADER_WEBTRANSPORT),
                h3::Header::new(b":scheme", url.scheme().as_bytes()),
                h3::Header::new(b":authority", url.authority().as_bytes()),
                h3::Header::new(b":path", url.path().as_bytes()),
                h3::Header::new(b"sec-webtransport-http3-draft02", b"1"), // this is outdated
        ];
        if !available_protocols.is_empty() {
            hdrs.push(h3::Header::new(b"wt-available-protocols", &encode_string_list(available_protocols)));
        }
        trace!("send {:?}", hdrs_to_strings(&hdrs));
        let session_id = h3.send_request(quic, &hdrs, false).unwrap();
        pending_requests
            .insert(session_id, PendingRequest::new(available_protocols.to_vec()));
        session_id
    }

    /// Processes WebTransport data from HTTP/3.
    pub fn poll<F: quiche::BufFactory>(&mut self, h3: &mut h3::Connection, quic: &mut quiche::Connection<F>) {
        // garbage collect finished streams
        self.streams.retain(|stream_id, stream| {
            if stream.finished() {
                trace!("garbage collect stream {}", stream_id);
                return false
            }
            true
        });

        for stream_id in h3.readable_webtransport_streams(quic) {
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
                Err(e) => unimplemented!("{:?}", e),
            };
            if quic.stream_finished(stream_id) {
                self.closed = true;
            }
            stream.readable = true;
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
                h3.send_response(quic, resp.session_id(), &hdrs, false).unwrap();
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

    /// return readable streams of a session
    pub fn readable_streams(&self, session_id: u64) -> Vec<u64> {
        let mut readable = vec![];
        for (stream_id, stream) in &self.streams {
            if stream.session_id == Some(session_id) && stream.readable() {
                readable.push(*stream_id);
            }
        }
        readable
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
        assert!(self.sessions.contains_key(&session_id));
        let stream_id = match h3_conn.open_webtransport_stream(quic_conn, bidi) {
            Ok(v) => v,
            Err(h3::Error::Done | h3::Error::StreamBlocked) => return Err(Error::InsufficientCapacity),
            Err(h3::Error::TransportError(quiche::Error::StreamLimit)) => return Err(Error::InsufficientCapacity),
            Err(e) => unimplemented!("{:?}", e),
        };
        self.streams
            .insert(stream_id, Stream::with_session(stream_id, session_id));
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
        let len = stream.recv(h3, quic, b)?;
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
    pub fn recv_hdrs(&mut self, stream_id: u64, headers: &Vec<h3::Header>) {
        let mut status: Option<[u8;3]> = None;
        let mut method_connect = false;
        let mut wt_draft_supported = false;
        let mut wt_draft_selected = false;
        let mut protocol_webtransport = false;
        let mut available_protocols: Vec<String> = vec![];
        let mut selected_protocol: Option<String> = None;
        for header in headers {
            match (header.name(), header.value()) {
                (b":status", s) => status = Some(<[u8; 3]>::try_from(s).unwrap()),
                (b":method", b"CONNECT") => method_connect = true,
                (b":protocol", b"webtransport") => protocol_webtransport = true,
                (b"sec-webtransport-http3-draft02", b"1") => wt_draft_supported = true,
                (b"sec-webtransport-http3-draft", b"draft02") => wt_draft_selected = true,
                (b"wt-available-protocols", v) => available_protocols = parse_string_list(v),
                (b"wt-protocol", v) => selected_protocol = parse_string_item(v),
                _ => debug!("ignore header {:?}", header),
            }
        }

        trace!("{:?}", hdrs_to_strings(headers));

        match &mut self.perspective {
            Perspective::Server { pending_responds } => {
                if method_connect && protocol_webtransport && wt_draft_supported {
                    self.sessions.insert(stream_id, Session::accept(stream_id, None));
                    pending_responds
                        .insert(stream_id, PendingResponse::new(stream_id, available_protocols));
                }
            }
            Perspective::Client { pending_requests } => {
                let req = pending_requests.remove(&stream_id).unwrap();
                assert_eq!(req.session_id(), stream_id);
                assert_eq!(status, Some(*b"200"));
                assert!(wt_draft_selected);
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

    pub fn stream_capacity<F: quiche::BufFactory>(
        &mut self,
        stream_id: u64,
        quic: &mut quiche::Connection<F>,
    ) -> Result<usize> {
        let stream = self.streams.get(&stream_id).unwrap();
        stream.capacity(quic)
    }

    pub fn stream_send<F: quiche::BufFactory>(
        &mut self,
        stream_id: u64,
        quic: &mut quiche::Connection<F>,
        buf: &[u8],
        fin: bool,
    ) -> Result<usize> {
        trace!("send on {} bytes on stream {}", buf.len(), stream_id);
        let stream = self.streams.get_mut(&stream_id).unwrap();
        stream.send(quic, buf, fin)
    }

    /// does not send buf partially.
    pub fn stream_send_if_capacity<F: quiche::BufFactory>(
        &mut self,
        stream_id: u64,
        quic: &mut quiche::Connection<F>,
        buf: &[u8],
        fin: bool,
    ) -> Result<()> {
        let stream = self.streams.get_mut(&stream_id).unwrap();
        stream.send_if_capacity(quic, buf, fin)
    }

    pub fn session_ids(&self) -> Vec<u64> {
        self.sessions.keys().cloned().collect()
    }

    pub fn close_session<F: quiche::BufFactory>(&mut self, session_id: SessionId, error_code: u32, error_msg: &str, quic: &mut quiche::Connection<F>, h3: &mut h3::Connection) -> Result<()> {
        let _session = self.sessions.remove(&session_id).unwrap();
        let mut buf = [0u8; 1024];
        let value = {
            let mut o = OctetsMut::with_slice(&mut buf);
            o.put_u32(error_code)?;
            o.put_bytes(error_msg.as_bytes())?;
            let len = o.off();
            buf[..len].to_vec()
        };
        let capsule = Capsule::new(WT_CLOSE_SESSION, value);
        let len = capsule.encode(&mut buf)?;
        let sent = match h3.send_body(quic, session_id, &buf[..len], false) {
            Ok(v) => v,
            Err(h3::Error::Done) => return Err(Error::Done),
            Err(e) => unimplemented!("{:?}", e),
        };
        assert_eq!(sent, len);
        Ok(())
    }
}
