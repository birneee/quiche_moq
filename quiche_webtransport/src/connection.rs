use crate::error::Result;
use crate::pending_request::PendingRequest;
use crate::pending_response::PendingResponse;
use crate::session::Session;
use crate::stream::Stream;
use crate::{Error, SessionId, PROTOCOL_HEADER_WEBTRANSPORT};
use log::{debug, trace};
use quiche::h3;
use quiche::h3::NameValue;
use std::collections::HashMap;
use url::Url;
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
    /// Returns session id.
    pub fn connect_session(
        &mut self,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
        url: Url,
    ) -> SessionId {
        let Perspective::Client { pending_requests } = &mut self.perspective else { panic!("Perspective is not client") };
        let hdrs = vec![
                h3::Header::new(b":method", METHOD_CONNECT),
                h3::Header::new(b":protocol", PROTOCOL_HEADER_WEBTRANSPORT),
                h3::Header::new(b":scheme", url.scheme().as_bytes()),
                h3::Header::new(b":authority", url.authority().as_bytes()),
                h3::Header::new(b":path", url.path().as_bytes()),
                h3::Header::new(b"sec-webtransport-http3-draft02", b"1"), // this is outdated
        ];
        trace!("send {:?}", hdrs_to_strings(&hdrs));
        let session_id = h3.send_request(quic, &hdrs, false).unwrap();
        pending_requests
            .insert(session_id, PendingRequest::new());
        session_id
    }

    /// Processes WebTransport data from HTTP/3.
    pub fn poll(&mut self, h3: &mut h3::Connection, quic: &mut quiche::Connection) {
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
                h3.send_response(
                    quic,
                    resp.session_id(),
                    &[
                        h3::Header::new(b":status", b"200"),
                        h3::Header::new(b"sec-webtransport-http3-draft", b"draft02"),
                    ],
                    false,
                )
                    .unwrap();
                return false;
            });
        }
    }

    /// return readable sessions ids
    pub fn readable_sessions(&self) -> Vec<u64> {
        let mut readable = vec![];
        for (_, stream) in &self.streams {
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

    pub fn open_stream(
        &mut self,
        session_id: u64,
        h3_conn: &mut h3::Connection,
        quic_conn: &mut quiche::Connection,
        bidi: bool,
    ) -> Result<u64> {
        assert!(self.sessions.contains_key(&session_id));
        let stream_id = match h3_conn.open_webtransport_stream(quic_conn, bidi) {
            Ok(v) => v,
            Err(h3::Error::Done | h3::Error::StreamBlocked) => return Err(Error::InsufficientCapacity),
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
    pub fn recv_stream(
        &mut self,
        stream_id: u64,
        session_id: u64,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
        b: &mut [u8],
    ) -> crate::error::Result<usize> {
        let stream = self
            .streams
            .get_mut(&stream_id)
            .ok_or(Error::NoStreamState)?;
        assert_eq!(stream.session_id, Some(session_id));
        stream.recv(h3, quic, b)
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
        for header in headers {
            match (header.name(), header.value()) {
                (b":status", s) => status = Some(<[u8; 3]>::try_from(s).unwrap()),
                (b":method", b"CONNECT") => method_connect = true,
                (b":protocol", b"webtransport") => protocol_webtransport = true,
                (b"sec-webtransport-http3-draft02", b"1") => wt_draft_supported = true,
                (b"sec-webtransport-http3-draft", b"draft02") => wt_draft_selected = true,
                _ => debug!("ignore header {:?}", header),
            }
        }

        trace!("{:?}", hdrs_to_strings(headers));

        match &mut self.perspective {
            Perspective::Server { pending_responds } => {
                if method_connect && protocol_webtransport && wt_draft_supported {
                    self.sessions.insert(stream_id, Session::accept(stream_id));
                    pending_responds
                        .insert(stream_id, PendingResponse::new(stream_id));
                }
            }
            Perspective::Client { pending_requests } => {
                let req = pending_requests.remove(&stream_id).unwrap();
                assert_eq!(req.session_id(), stream_id);
                assert_eq!(status, Some(*b"200"));
                assert!(wt_draft_selected);
                let session_id = stream_id;
                debug!("webtransport session {} established", session_id);
                self.sessions.insert(session_id, Session::accept(session_id));
            }
        }
    }

    pub fn stream_capacity(
        &mut self,
        stream_id: u64,
        quic: &mut quiche::Connection,
    ) -> Result<usize> {
        let stream = self.streams.get(&stream_id).unwrap();
        stream.capacity(quic)
    }

    pub fn stream_send(
        &mut self,
        stream_id: u64,
        quic: &mut quiche::Connection,
        buf: &[u8],
        fin: bool,
    ) -> Result<usize> {
        trace!("send on stream {}: {:?}", stream_id, buf);
        let stream = self.streams.get_mut(&stream_id).unwrap();
        stream.send(quic, buf, fin)
    }

    /// does not send buf partially.
    pub fn stream_send_if_capacity(
        &mut self,
        stream_id: u64,
        quic: &mut quiche::Connection,
        buf: &[u8],
        fin: bool,
    ) -> Result<()> {
        let stream = self.streams.get_mut(&stream_id).unwrap();
        stream.send_if_capacity(quic, buf, fin)
    }
    
    pub fn session_ids(&self) -> Vec<u64> {
        self.sessions.keys().cloned().collect()
    }
}
