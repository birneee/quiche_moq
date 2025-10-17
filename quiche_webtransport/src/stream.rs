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
    pub(crate) readable: bool,
    /// quic reports that the stream is finished.
    /// h3 or `self.buf` might still readable buffer data.
    pub quic_finished: bool,
    /// true if stream is closed and no more data is buffered.
    /// stream state can be removed.
    finished: bool,
    /// whether the WebTransport session ID has been sent on the stream.
    /// always true for remote initialized streams.
    pub sent_session_id: bool,
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
            finished: false,
            sent_session_id: true,
        }
    }

    pub(crate) fn with_session(stream_id: u64, session_id: u64) -> Self {
        Self {
            stream_id,
            buf: ShortBuf::new(),
            session_id: Some(session_id),
            readable: false,
            quic_finished: false,
            finished: false,
            sent_session_id: false,
        }
    }

    /// returns Done if the stream is not ready for the application to consume
    /// returns the session_id when the stream is ready
    pub fn read_session_id(
        &mut self,
        quic: &mut quiche::Connection,
        h3: &mut h3::Connection,
    ) -> Result<u64> {
        Ok(match self.session_id {
            None => {
                while self.buf.len() < 1 {
                    self.fill_buf(quic, h3)?;
                }
                let session_id_len = varint_parse_len(self.buf.buffer()[0]);
                while self.buf.len() < session_id_len {
                    self.fill_buf(quic, h3)?;
                }
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

    /// Fill internal buffer from h3 stream
    /// Returns `Error::Done` when currently not data can be filled.
    pub(crate) fn fill_buf(
        &mut self,
        quic: &mut quiche::Connection,
        _h3: &mut h3::Connection,
    ) -> Result<()> {
        match self
            .buf
            .fill(|b| quic.stream_recv(self.stream_id, b).map(|v| v.0))
        {
            Ok(v) => Ok(v),
            Err(quiche::Error::Done) => Err(Error::Done),
            Err(e) => unimplemented!("{:?}", e),
        }
    }

    // returns `Error::Fin` when stream has finished.
    // returns `Error::Done` when no more data can be read currently.
    pub(crate) fn recv(
        &mut self,
        _h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
        b: &mut [u8],
    ) -> Result<usize> {
        if self.session_id.is_none() {
            return Err(Error::MissingSessionID);
        }
        match self
            .buf
            .chain_read2(|b| quic.stream_recv(self.stream_id, b).map(|v| v.0), b)
        {
            Ok(len) => {
                if quic.stream_finished(self.stream_id) {
                    self.quic_finished = true;
                }
                Ok(len)
            }
            Err(e) => match e {
                quiche::Error::InvalidStreamState(_) => {
                    if self.quic_finished {
                        debug!("wt stream {} finished", self.stream_id);
                        self.finished = true;
                        Err(Error::Fin)
                    } else {
                        unimplemented!("{:?}", e)
                    }
                }
                quiche::Error::Done => Err(Error::Done),
                e => unimplemented!("{:?}", e),
            },
        }
    }

    pub(crate) fn send(
        &mut self,
        quic: &mut quiche::Connection,
        buf: &[u8],
        fin: bool,
    ) -> Result<usize> {
        if !self.sent_session_id {
            let mut b = [0u8; MAX_VARINT_LEN];
            let mut b = OctetsMut::with_slice(&mut b);
            let b = b.put_varint(self.session_id.unwrap()).unwrap();
            if quic.stream_capacity(self.stream_id).unwrap() < b.len() {
                return Err(Error::Done);
            }
            let sent = quic.stream_send(self.stream_id, b, false).unwrap();
            assert_eq!(b.len(), sent);
            self.sent_session_id = true;
        };
        let sent = match quic.stream_send(self.stream_id, buf, fin) {
            Ok(v) => v,
            Err(quiche::Error::Done) => return Err(Error::Done),
            Err(e) => unimplemented!("{:?}", e),
        };
        Ok(sent)
    }

    /// The buf will not sent partially.
    pub(crate) fn send_if_capacity(
        &mut self,
        quic: &mut quiche::Connection,
        buf: &[u8],
        fin: bool,
    ) -> Result<()> {
        let capacity = self.capacity(quic)?;
        if capacity < buf.len() {
            return Err(Error::InsufficientCapacity);
        }
        self.send(quic, buf, fin)?;
        Ok(())
    }

    pub fn readable(&self) -> bool {
        self.readable
    }

    pub fn capacity(&self, quic: &mut quiche::Connection) -> Result<usize> {
        let mut capacity = match quic.stream_capacity(self.stream_id) {
            Ok(v) => v,
            Err(e) => unimplemented!("{:?}", e),
        };
        if !self.sent_session_id {
            let session_id_len = varint_len(self.session_id.unwrap());
            capacity -= session_id_len
        }
        Ok(capacity)
    }

    /// true if stream is closed and no more data is buffered.
    /// stream state can be removed.
    pub fn finished(&self) -> bool {
        self.finished
    }
}
