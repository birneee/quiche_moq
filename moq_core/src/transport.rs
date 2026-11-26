use crate::prelude::*;
use moq_wire::{ResetCode, SessionCloseCode};

/// QUIC stream urgency bands (draft-19 §7.2): control, then request streams,
/// then objects. Lower urgency is served first.
/// - objects are offset so subscriber priority 0 can't tie with control
pub const CONTROL_STREAM_URGENCY: u8 = 0;
/// draft 17+ per-request bidi streams.
pub const REQUEST_STREAM_URGENCY: u8 = 1;

/// Maps subscriber priority (draft-19 §10.2.7) into the object band.
/// - saturating: 253–255 collapse onto 255
pub const fn object_urgency(subscriber_priority: u8) -> u8 {
    subscriber_priority.saturating_add(REQUEST_STREAM_URGENCY + 1)
}

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
    /// Any other failure, e.g. the connection closed. Permanent.
    Failed,
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

    /// Like [`Self::send_stream`] but returns [`StreamError::InsufficientCapacity`] instead of
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
    // TODO RESET_STREAM_AT keeping the subgroup header, or the Objects sent, reliable: the subscriber can then
    //  count the stream for PUBLISH_DONE instead of waiting for a timeout (draft-ietf-moq-transport-18 §11.4.3);
    //  optional for MOQT (§1.3), needs it in quiche, implement when needed
    fn reset_stream(&mut self, stream_id: u64, code: ResetCode);

    /// Send a STOP_SENDING frame on `stream_id` with `error_code`.
    /// - `true`: the transport discards what still arrives on the stream
    /// - `false`: it keeps reporting the stream readable, the session drains it until it ends
    fn stop_sending(&mut self, stream_id: u64, code: ResetCode) -> bool;

    /// True once the peer sent STOP_SENDING for `stream_id`, without writing to it.
    /// - `false` on transports that only report it to `send_stream`
    fn stream_stopped(&mut self, stream_id: u64) -> bool;

    /// True once all data written to `stream_id` (including FIN) has been
    /// acknowledged by the peer — i.e. it's actually left the local send
    /// buffer and been confirmed delivered, not just handed to `send_stream`.
    /// Also true for a stream that no longer exists (already collected), so
    /// the session can drop its bookkeeping. False for a stream never sent
    /// with `fin: true` or whose FIN is still unacked.
    fn stream_finished(&self, stream_id: u64) -> bool;

    /// Send `datagram` unreliably.
    /// - [`StreamError::Done`]: it doesn't fit or there is no room now, it is dropped
    /// - [`StreamError::Failed`] on transports without datagrams
    fn send_datagram(&mut self, _datagram: &[u8]) -> Result<(), StreamError> {
        Err(StreamError::Failed)
    }

    /// The next datagram received, `None` if there is none.
    fn recv_datagram(&mut self) -> Option<Vec<u8>> {
        None
    }

    /// Close the WebTransport session with the given error code and reason string.
    fn close_session(&mut self, code: SessionCloseCode, reason: &str);

    /// Returns a mutable reference to the qlog streamer, if one is active.
    /// Used by moq_core to emit qlog events at key points in the protocol state machine.
    #[cfg(feature = "qlog")]
    fn qlog_streamer(&mut self) -> Option<&mut qlog::streamer::QlogStreamer> {
        None
    }
}
