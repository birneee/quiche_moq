use moq_core::transport::{StreamError, Transport};
use quiche::h3;
use quiche_webtransport as wt;

fn wt_err(e: wt::Error) -> StreamError {
    match e {
        wt::Error::Done => StreamError::Done,
        wt::Error::Fin => StreamError::Fin,
        wt::Error::StreamReset(c) => StreamError::StreamReset(c),
        wt::Error::StreamStopped(c) => StreamError::StreamStopped(c),
        wt::Error::InsufficientCapacity => StreamError::InsufficientCapacity,
        wt::Error::InvalidStreamState(c) => StreamError::InvalidStreamState(c),
        other => unimplemented!("{:?}", other),
    }
}

/// Bridges `moq_core::Transport` over the native quiche WebTransport stack.
pub struct QuicheTransport<'a> {
    session_id: u64,
    wt: Option<&'a mut wt::Connection>,
    h3: Option<&'a mut h3::Connection>,
    quic: &'a mut quiche::Connection,
}

impl<'a> QuicheTransport<'a> {
    pub fn new(
        session_id: u64,
        wt: &'a mut wt::Connection,
        h3: &'a mut h3::Connection,
        quic: &'a mut quiche::Connection,
    ) -> Self {
        Self { session_id, wt: Some(wt), h3: Some(h3), quic }
    }

    /// For operations that only send on already-open streams and never call
    /// `open_stream` or `recv_stream`.
    pub fn without_h3(
        session_id: u64,
        wt: &'a mut wt::Connection,
        quic: &'a mut quiche::Connection,
    ) -> Self {
        Self { session_id, wt: Some(wt), h3: None, quic }
    }

    /// For operations that only touch the QUIC layer (stream resets, priority,
    /// stop-sending). Panics if a WebTransport send/recv is attempted.
    pub fn quic_only(quic: &'a mut quiche::Connection) -> Self {
        Self { session_id: 0, wt: None, h3: None, quic }
    }
}

impl Transport for QuicheTransport<'_> {
    fn recv_stream(&mut self, stream_id: u64, buf: &mut [u8]) -> Result<usize, StreamError> {
        let session_id = self.session_id;
        let wt = self.wt.as_mut().expect("wt required for recv_stream");
        let h3 = self.h3.as_mut().expect("h3 required for recv_stream");
        wt.recv_stream(stream_id, session_id, h3, self.quic, buf).map_err(wt_err)
    }

    fn send_stream(&mut self, stream_id: u64, data: &[u8], fin: bool) -> Result<usize, StreamError> {
        let wt = self.wt.as_mut().expect("wt required for send_stream");
        wt.stream_send(stream_id, self.quic, data, fin).map_err(wt_err)
    }

    fn send_stream_if_capacity(
        &mut self,
        stream_id: u64,
        data: &[u8],
        fin: bool,
    ) -> Result<usize, StreamError> {
        let wt = self.wt.as_mut().expect("wt required for send_stream_if_capacity");
        wt.stream_send_if_capacity(stream_id, self.quic, data, fin)
            .map(|()| data.len())
            .map_err(wt_err)
    }

    fn open_uni_stream(&mut self) -> Result<u64, StreamError> {
        let session_id = self.session_id;
        let wt = self.wt.as_mut().expect("wt required for open_uni_stream");
        let h3 = self.h3.as_mut().expect("h3 required for open_uni_stream");
        wt.open_stream(session_id, h3, self.quic, false).map_err(wt_err)
    }

    fn open_bidi_stream(&mut self) -> Result<u64, StreamError> {
        let session_id = self.session_id;
        let wt = self.wt.as_mut().expect("wt required for open_bidi_stream");
        let h3 = self.h3.as_mut().expect("h3 required for open_bidi_stream");
        wt.open_stream(session_id, h3, self.quic, true).map_err(wt_err)
    }

    fn set_stream_priority(&mut self, stream_id: u64, urgency: u8, incremental: bool) {
        self.quic.stream_priority(stream_id, urgency, incremental).ok();
    }

    fn reset_stream(&mut self, stream_id: u64, error_code: u64) {
        self.quic
            .stream_shutdown(stream_id, quiche::Shutdown::Write, error_code)
            .ok();
    }

    fn stop_sending(&mut self, stream_id: u64, error_code: u64) {
        self.quic
            .stream_shutdown(stream_id, quiche::Shutdown::Read, error_code)
            .ok();
    }

    fn close_session(&mut self, error_code: u64, reason: &str) {
        let session_id = self.session_id;
        let wt = self.wt.as_mut().expect("wt required for close_session");
        let h3 = self.h3.as_mut().expect("h3 required for close_session");
        wt.close_session(session_id, error_code as u32, reason, self.quic, h3).ok();
    }

    #[cfg(feature = "qlog")]
    fn qlog_streamer(&mut self) -> Option<&mut qlog::streamer::QlogStreamer> {
        self.quic.qlog_streamer()
    }
}
