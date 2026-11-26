use moq_core::transport::{StreamError, Transport};
use moq_core::wire::{ResetCode, SessionCloseCode};
use quiche::h3;
use quiche_webtransport as wt;

fn wt_err(e: wt::Error) -> StreamError {
    match e {
        wt::Error::Done => StreamError::Done,
        wt::Error::Fin => StreamError::Fin,
        // Stream already removed after a prior Fin — same "nothing more to read" outcome.
        wt::Error::NoStreamState => StreamError::Fin,
        wt::Error::StreamReset(c) => StreamError::StreamReset(c),
        wt::Error::StreamStopped(c) => StreamError::StreamStopped(c),
        wt::Error::InsufficientCapacity => StreamError::InsufficientCapacity,
        wt::Error::InvalidStreamState(c) => StreamError::InvalidStreamState(c),
        wt::Error::MissingSessionID | wt::Error::InvalidHeaders(_) | wt::Error::InvalidCapsule(_) | wt::Error::Transport(_) => {
            log::debug!("transport failed: {e:?}");
            StreamError::Failed
        }
    }
}

fn quic_err(e: quiche::Error) -> StreamError {
    match e {
        quiche::Error::Done | quiche::Error::StreamLimit => StreamError::Done,
        quiche::Error::StreamReset(c) => StreamError::StreamReset(c),
        quiche::Error::StreamStopped(c) => StreamError::StreamStopped(c),
        // collected after its FIN was read
        quiche::Error::InvalidStreamState(_) => StreamError::Fin,
        e => {
            log::debug!("transport failed: {e:?}");
            StreamError::Failed
        }
    }
}

/// Bridges `moq_core::Transport` over a quiche connection, on a WebTransport session or the
/// connection itself (native QUIC, draft-ietf-moq-transport-18 §3.1.4).
pub struct QuicheTransport<'a> {
    quic: &'a mut quiche::Connection,
    carrier: Carrier<'a>,
}

enum Carrier<'a> {
    WebTransport { session_id: u64, wt: &'a mut wt::Connection, h3: &'a mut h3::Connection },
    Raw(&'a mut RawStreams),
}

/// The streams a raw QUIC session opens next, by QUIC's stream ID rules (RFC 9000 §2.1).
#[derive(Debug)]
pub struct RawStreams {
    next_bidi: u64,
    next_uni: u64,
}

impl RawStreams {
    pub fn new(is_server: bool) -> Self {
        Self { next_bidi: is_server as u64, next_uni: 2 | is_server as u64 }
    }

    fn open(&mut self, bidi: bool, left: u64) -> Result<u64, StreamError> {
        // as WebTransport reports the peer's stream limit
        if left == 0 {
            return Err(StreamError::InsufficientCapacity);
        }
        let next = if bidi { &mut self.next_bidi } else { &mut self.next_uni };
        let id = *next;
        *next += 4;
        Ok(id)
    }
}

impl<'a> QuicheTransport<'a> {
    pub fn new(
        session_id: u64,
        wt: &'a mut wt::Connection,
        h3: &'a mut h3::Connection,
        quic: &'a mut quiche::Connection,
    ) -> Self {
        Self { quic, carrier: Carrier::WebTransport { session_id, wt, h3 } }
    }

    /// A session that is the whole QUIC connection.
    pub fn raw(quic: &'a mut quiche::Connection, streams: &'a mut RawStreams) -> Self {
        Self { quic, carrier: Carrier::Raw(streams) }
    }

    pub fn quic(&mut self) -> &mut quiche::Connection {
        self.quic
    }

    /// Drives h3 and WebTransport, before the session reads what arrived.
    pub fn poll_webtransport(&mut self) {
        if let Carrier::WebTransport { wt, h3, .. } = &mut self.carrier {
            wt.poll_h3(h3, self.quic);
            wt.poll(h3, self.quic);
        }
    }

    /// Streams of the session with data to read.
    pub fn readable_streams(&self) -> Vec<u64> {
        match &self.carrier {
            Carrier::WebTransport { session_id, wt, .. } => wt.readable_streams(*session_id).to_vec(),
            Carrier::Raw(_) => self.quic.readable().collect(),
        }
    }
}

impl Transport for QuicheTransport<'_> {
    fn recv_stream(&mut self, stream_id: u64, buf: &mut [u8]) -> Result<usize, StreamError> {
        match &mut self.carrier {
            Carrier::WebTransport { session_id, wt, h3 } => wt.recv_stream(stream_id, *session_id, h3, self.quic, buf).map_err(wt_err),
            Carrier::Raw(_) => match self.quic.stream_recv(stream_id, buf) {
                Ok((0, true)) => Err(StreamError::Fin),
                Ok((n, _)) => Ok(n),
                Err(e) => Err(quic_err(e)),
            },
        }
    }

    fn send_stream(&mut self, stream_id: u64, data: &[u8], fin: bool) -> Result<usize, StreamError> {
        match &mut self.carrier {
            Carrier::WebTransport { wt, h3, .. } => wt.stream_send(stream_id, h3, self.quic, data, fin).map_err(wt_err),
            Carrier::Raw(_) => match self.quic.stream_send(stream_id, data, fin) {
                Err(quiche::Error::InvalidStreamState(id)) => Err(StreamError::InvalidStreamState(id)),
                sent => sent.map_err(quic_err),
            },
        }
    }

    fn send_stream_if_capacity(
        &mut self,
        stream_id: u64,
        data: &[u8],
        fin: bool,
    ) -> Result<usize, StreamError> {
        match &mut self.carrier {
            Carrier::WebTransport { wt, h3, .. } => wt.stream_send_if_capacity(stream_id, h3, self.quic, data, fin)
                .map(|()| data.len())
                .map_err(wt_err),
            Carrier::Raw(_) => {
                // a stream not opened yet has its initial credit
                if self.quic.stream_capacity(stream_id).is_ok_and(|capacity| capacity < data.len()) {
                    return Err(StreamError::InsufficientCapacity);
                }
                self.send_stream(stream_id, data, fin)
            }
        }
    }

    fn open_uni_stream(&mut self) -> Result<u64, StreamError> {
        match &mut self.carrier {
            Carrier::WebTransport { session_id, wt, h3 } => wt.open_stream(*session_id, h3, self.quic, false).map_err(wt_err),
            Carrier::Raw(streams) => streams.open(false, self.quic.peer_streams_left_uni()),
        }
    }

    fn open_bidi_stream(&mut self) -> Result<u64, StreamError> {
        match &mut self.carrier {
            Carrier::WebTransport { session_id, wt, h3 } => wt.open_stream(*session_id, h3, self.quic, true).map_err(wt_err),
            Carrier::Raw(streams) => streams.open(true, self.quic.peer_streams_left_bidi()),
        }
    }

    fn set_stream_priority(&mut self, stream_id: u64, urgency: u8, incremental: bool) {
        self.quic.stream_priority(stream_id, urgency, incremental).ok();
    }

    /// Through h3 on WebTransport, which then frees the stream's H3 state.
    fn reset_stream(&mut self, stream_id: u64, code: ResetCode) {
        match &mut self.carrier {
            Carrier::WebTransport { wt, h3, .. } => {
                wt.shutdown_stream(stream_id, quiche::Shutdown::Write, code.to_u32(), h3, self.quic).ok();
                wt.stream_send_ended(stream_id);
            }
            Carrier::Raw(_) => { self.quic.stream_shutdown(stream_id, quiche::Shutdown::Write, code.into()).ok(); }
        }
    }

    /// quiche drops the stream's buffered and future data.
    fn stop_sending(&mut self, stream_id: u64, code: ResetCode) -> bool {
        match &mut self.carrier {
            Carrier::WebTransport { wt, h3, .. } => {
                wt.shutdown_stream(stream_id, quiche::Shutdown::Read, code.to_u32(), h3, self.quic).ok();
                wt.stream_recv_ended(stream_id);
            }
            Carrier::Raw(_) => { self.quic.stream_shutdown(stream_id, quiche::Shutdown::Read, code.into()).ok(); }
        }
        true
    }

    fn stream_stopped(&mut self, stream_id: u64) -> bool {
        matches!(self.quic.stream_capacity(stream_id), Err(quiche::Error::StreamStopped(_)))
    }

    fn stream_finished(&self, stream_id: u64) -> bool {
        self.quic.stream_finished(stream_id)
    }

    fn send_datagram(&mut self, datagram: &[u8]) -> Result<(), StreamError> {
        match &mut self.carrier {
            Carrier::WebTransport { session_id, wt, h3 } => {
                // the peer takes no datagrams
                if !h3.dgram_enabled_by_peer(self.quic) {
                    return Err(StreamError::Failed);
                }
                wt.send_datagram(*session_id, self.quic, datagram).map_err(wt_err)
            }
            Carrier::Raw(_) => match self.quic.dgram_send(datagram) {
                Err(quiche::Error::BufferTooShort) => Err(StreamError::Done),
                // the peer takes no datagrams
                Err(quiche::Error::InvalidState) => Err(StreamError::Failed),
                sent => sent.map_err(quic_err),
            },
        }
    }

    fn recv_datagram(&mut self) -> Option<Vec<u8>> {
        match &mut self.carrier {
            Carrier::WebTransport { session_id, wt, .. } => wt.recv_datagram(*session_id),
            Carrier::Raw(_) => {
                let mut datagram = vec![0; self.quic.dgram_recv_front_len()?];
                let len = self.quic.dgram_recv(&mut datagram).ok()?;
                datagram.truncate(len);
                Some(datagram)
            }
        }
    }

    /// On raw QUIC with CONNECTION_CLOSE, the session is the connection (draft-ietf-moq-transport-18 §3.5).
    fn close_session(&mut self, code: SessionCloseCode, reason: &str) {
        match &mut self.carrier {
            Carrier::WebTransport { session_id, wt, h3 } => { wt.close_session(*session_id, code.into(), reason, self.quic, h3).ok(); }
            Carrier::Raw(_) => { self.quic.close(true, u32::from(code).into(), reason.as_bytes()).ok(); }
        }
    }

    #[cfg(feature = "qlog")]
    fn qlog_streamer(&mut self) -> Option<&mut qlog::streamer::QlogStreamer> {
        self.quic.qlog_streamer()
    }
}
