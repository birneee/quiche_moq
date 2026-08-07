use std::future::Future;

use moq_core::Transport;

use crate::notify::Notify;

/// Extends `moq_core::Transport` (sync, non-blocking) with the bits an async
/// driver loop needs that the sync trait doesn't provide: which streams
/// currently have buffered data to read, a way to flush queued sends that
/// may itself be async (e.g. a JS Promise from
/// `WritableStreamDefaultWriter.write()`, or a tokio socket write), and how
/// to establish the transport in the first place.
///
/// `moq_core::Transport` itself stays untouched and platform-agnostic;
/// implementing this trait is the only thing a caller needs to plug their
/// transport into [`crate::AsyncMoqSession`] / [`crate::MoqSession`].
pub trait AsyncTransport: Transport + Sized {
    type FlushError;

    /// Stream ids with buffered data ready to read right now.
    fn readable_stream_ids(&self) -> Vec<u64>;

    /// Flush any queued sends.
    fn flush_sends(&mut self) -> impl Future<Output = Result<(), Self::FlushError>>;

    /// Drops bookkeeping for streams the session has finished with (already
    /// reported a `Fin`), called once per receive cycle by
    /// [`crate::AsyncMoqSessionActor::run_receive_cycle`]. Matters for
    /// transports that buffer per-stream state that would otherwise leak
    /// forever and, for `WebTransport`, stall the peer on read credit once
    /// buffered stream ids stop ever being retired. No-op by default —
    /// override if the transport actually accumulates such state.
    fn retire_finished_streams(&mut self) {}

    /// Whatever a caller needs to establish a connection — a `SocketAddr`
    /// for a native UDP transport, a relay URL for `WebTransport`, etc.
    type ConnectParams;
    type ConnectError;

    /// Establishes the transport, plus the `Notify` any background I/O tasks
    /// it spawns must share with the session built on top of it (see
    /// [`crate::AsyncMoqSession::connect`]'s doc comment for why this can't
    /// just be created internally).
    fn connect(params: Self::ConnectParams) -> impl Future<Output = Result<(Self, Notify), Self::ConnectError>>;
}
