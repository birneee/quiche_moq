use crate::error::{Error, Result};
use crate::MAX_VARINT_LEN;
use log::debug;
use octets::{varint_len, varint_parse_len, Octets, OctetsMut};
use quiche::h3;
use short_buf::ShortBuf;

pub struct Stream {
    stream_id: u64,
    pub session_id: Option<u64>,
    pub(crate) buf: ShortBuf<MAX_VARINT_LEN>,
    /// h3 or `self.buf` might still have buffered data to read.
    pub(crate) readable: bool,
    /// quic reports that the stream is finished.
    pub quic_finished: bool,
    /// whether the WebTransport session ID has been sent on the stream.
    /// always true for remote initialized streams.
    pub sent_session_id: bool,
    /// Nothing more to send: FIN sent, reset, or a peer uni stream.
    pub(crate) send_ended: bool,
    /// Nothing more to read: ended, reset, reading stopped, or our uni stream.
    pub(crate) recv_ended: bool,
}

/// QUIC stream id bit 1 (RFC 9000 §2.1).
pub fn is_uni(stream_id: u64) -> bool {
    stream_id & 0x2 != 0
}

impl Stream {
    /// create a stream state for a remote initialized stream.
    pub(crate) fn new_remote(stream_id: u64) -> Self {
        Self {
            stream_id,
            buf: ShortBuf::new(),
            session_id: None,
            readable: false,
            quic_finished: false,
            sent_session_id: true,
            send_ended: is_uni(stream_id),
            recv_ended: false,
        }
    }

    pub(crate) fn with_session(stream_id: u64, session_id: u64) -> Self {
        Self {
            stream_id,
            buf: ShortBuf::new(),
            session_id: Some(session_id),
            readable: false,
            quic_finished: false,
            sent_session_id: false,
            send_ended: false,
            recv_ended: is_uni(stream_id),
        }
    }

    /// returns Done if the stream is not ready for the application to consume
    /// returns the session_id when the stream is ready
    /// returns Fin if the stream ended before its session ID
    pub fn read_session_id<F: quiche::BufFactory>(
        &mut self,
        quic: &mut quiche::Connection<F>,
        h3: &mut h3::Connection,
    ) -> Result<u64> {
        Ok(match self.session_id {
            None => {
                let header_len = |buf: &[u8]| buf.first().map_or(1, |&b| varint_parse_len(b));
                while self.buf.len() < header_len(self.buf.buffer()) {
                    if self.quic_finished {
                        return Err(Error::Fin);
                    }
                    self.fill_buf(quic, h3)?;
                }
                let session_id_len = header_len(self.buf.buffer());
                let session_id = Octets::with_slice(&self.buf.buffer()[0..session_id_len])
                    .get_varint()
                    .unwrap();
                self.buf.consume(session_id_len);
                self.session_id = Some(session_id);
                session_id
            }
            Some(v) => v,
        })
    }

    /// Fill internal buffer from h3 stream, notes a FIN in `quic_finished`.
    /// Returns `Error::Done` when currently not data can be filled.
    pub(crate) fn fill_buf<F: quiche::BufFactory>(
        &mut self,
        quic: &mut quiche::Connection<F>,
        h3: &mut h3::Connection,
    ) -> Result<()> {
        let stream_id = self.stream_id;
        let mut fin = false;
        self.buf.fill(|b| h3.recv_webtransport_stream(quic, stream_id, b).map(|(len, f)| { fin = f; len }))?;
        self.quic_finished |= fin;
        Ok(())
    }

    // returns `Error::Fin` when stream has finished.
    // returns `Error::Done` when no more data can be read currently.
    pub(crate) fn recv<F: quiche::BufFactory>(
        &mut self,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection<F>,
        b: &mut [u8],
    ) -> Result<usize> {
        if self.session_id.is_none() {
            return Err(Error::MissingSessionID);
        }
        match self
            .buf
            .chain_read2(|b| h3.recv_webtransport_stream(quic, self.stream_id, b).map(|v| v.0), b)
        {
            Ok(len) => {
                if quic.stream_finished(self.stream_id) {
                    self.quic_finished = true;
                }
                Ok(len)
            }
            Err(e) => match e {
                h3::Error::TransportError(quiche::Error::InvalidStreamState(i)) => {
                    if self.quic_finished {
                        debug!("wt stream {} finished", self.stream_id);
                        Err(Error::Fin)
                    } else {
                        Err(Error::InvalidStreamState(i))
                    }
                }
                h3::Error::Done => {
                    if self.quic_finished {
                        // FIN was already read (0 bytes + fin in a previous call);
                        // subsequent stream_recv returns Done — treat as Fin.
                        Err(Error::Fin)
                    } else {
                        Err(Error::Done)
                    }
                },
                e => Err(e.into()),
            },
        }
    }

    pub(crate) fn send<F: quiche::BufFactory>(
        &mut self,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection<F>,
        buf: &[u8],
        fin: bool,
    ) -> Result<usize> {
        self.send_session_id(h3, quic)?;
        Ok(h3.send_webtransport_stream(quic, self.stream_id, buf, fin)?)
    }

    /// The Session ID after the stream type, once: right when the stream opens, so peers reading
    /// the stream header from its first frame find it whole, else before the first data.
    pub(crate) fn send_session_id<F: quiche::BufFactory>(&mut self, h3: &mut h3::Connection, quic: &mut quiche::Connection<F>) -> Result<()> {
        if self.sent_session_id {
            return Ok(());
        }
        let mut b = [0u8; MAX_VARINT_LEN];
        let mut b = OctetsMut::with_slice(&mut b);
        let b = b.put_varint(self.session_id.unwrap()).unwrap();
        if quic.stream_capacity(self.stream_id)? < b.len() {
            return Err(Error::Done);
        }
        let sent = h3.send_webtransport_stream(quic, self.stream_id, b, false)?;
        // the capacity was checked
        assert_eq!(b.len(), sent);
        self.sent_session_id = true;
        Ok(())
    }

    /// The buf will not sent partially.
    pub(crate) fn send_if_capacity<F: quiche::BufFactory>(
        &mut self,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection<F>,
        buf: &[u8],
        fin: bool,
    ) -> Result<()> {
        let capacity = self.capacity(quic)?;
        if capacity < buf.len() {
            return Err(Error::InsufficientCapacity);
        }
        self.send(h3, quic, buf, fin)?;
        Ok(())
    }

    pub fn capacity<F: quiche::BufFactory>(&self, quic: &mut quiche::Connection<F>) -> Result<usize> {
        let capacity = quic.stream_capacity(self.stream_id)?;
        if self.sent_session_id {
            return Ok(capacity);
        }
        Ok(capacity.saturating_sub(varint_len(self.session_id.unwrap())))
    }
}
