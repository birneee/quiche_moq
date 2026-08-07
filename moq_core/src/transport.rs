#[derive(Debug)]
pub enum StreamError {
    /// No data/capacity available right now; retry later.
    Done,
    /// Stream ended cleanly (FIN received).
    Fin,
    /// Stream was reset by the peer.
    StreamReset(u64),
    /// The peer sent `STOP_SENDING` on the stream — it doesn't want any
    /// more data on it (e.g. no subscriber wants this track anymore).
    StreamStopped(u64),
    /// No QUIC flow-control capacity to send; retry later.
    InsufficientCapacity,
    /// Stream is in an invalid state.
    InvalidStreamState(u64),
}

/// Transport abstraction for MoQ sessions.
///
/// Implementations provide this to [`crate::session::MoqTransportSession`] so the
/// session state machine remains independent of any concrete transport library.
pub trait Transport {
    /// Read bytes from `stream_id` into `buf`. Returns bytes read.
    fn recv_stream(&mut self, stream_id: u64, buf: &mut [u8]) -> Result<usize, StreamError>;

    /// Write `data` to `stream_id`. `fin` closes the send side. Returns bytes written.
    fn send_stream(&mut self, stream_id: u64, data: &[u8], fin: bool) -> Result<usize, StreamError>;

    /// Like [`send_stream`] but returns [`StreamError::InsufficientCapacity`] instead of
    /// [`StreamError::Done`] when the stream's send buffer is full.
    fn send_stream_if_capacity(
        &mut self,
        stream_id: u64,
        data: &[u8],
        fin: bool,
    ) -> Result<usize, StreamError>;

    /// Open a new outgoing unidirectional stream. Returns its stream id.
    fn open_uni_stream(&mut self) -> Result<u64, StreamError>;

    /// Open a new outgoing bidirectional stream. Returns its stream id.
    fn open_bidi_stream(&mut self) -> Result<u64, StreamError>;

    /// Set the QUIC priority for `stream_id`. No-op on transports without priority support.
    fn set_stream_priority(&mut self, stream_id: u64, urgency: u8, incremental: bool);

    /// Send a RESET_STREAM frame on `stream_id` with `error_code`.
    fn reset_stream(&mut self, stream_id: u64, error_code: u64);

    /// Send a STOP_SENDING frame on `stream_id` with `error_code`.
    fn stop_sending(&mut self, stream_id: u64, error_code: u64);

    /// Close the WebTransport session with the given error code and reason string.
    fn close_session(&mut self, error_code: u64, reason: &str);

    /// Returns a mutable reference to the qlog streamer, if one is active.
    /// Used by moq_core to emit qlog events at key points in the protocol state machine.
    #[cfg(feature = "qlog")]
    fn qlog_streamer(&mut self) -> Option<&mut qlog::streamer::QlogStreamer> {
        None
    }
}
