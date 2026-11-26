pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Done,
    MissingSessionID,
    /// Stream state does not yet exist or is already cleaned up.
    NoStreamState,
    InsufficientCapacity,
    Fin,
    /// The specified stream was reset by the peer.
    ///
    /// The error code sent as part of the `RESET_STREAM` frame is provided as
    /// associated data, on data streams the application error (see `error_code`).
    StreamReset(u64),
    /// The peer sent `STOP_SENDING` on the specified stream — it doesn't
    /// want any more data on it (e.g. no subscriber wants this track).
    ///
    /// The error code sent as part of the `STOP_SENDING` frame is provided
    /// as associated data, on data streams the application error (see `error_code`).
    StreamStopped(u64),
    /// The operation cannot be completed because the stream is in an
    /// invalid state.
    ///
    /// The stream ID is provided as associated data.
    InvalidStreamState(u64),
    /// The peer's CONNECT request or response can't establish a session.
    InvalidHeaders(String),
    /// The peer's capsules on a CONNECT stream break draft-ietf-webtrans-http3.
    InvalidCapsule(String),
    /// Any other HTTP/3 or QUIC error.
    Transport(quiche::h3::Error),
}

impl From<quiche::h3::Error> for Error {
    fn from(err: quiche::h3::Error) -> Self {
        match err {
            quiche::h3::Error::Done => Error::Done,
            quiche::h3::Error::TransportError(e) => e.into(),
            e => Error::Transport(e),
        }
    }
}

impl From<quiche::Error> for Error {
    fn from(err: quiche::Error) -> Self {
        match err {
            quiche::Error::Done => Error::Done,
            quiche::Error::StreamStopped(code) => Error::StreamStopped(code),
            quiche::Error::StreamReset(code) => Error::StreamReset(code),
            quiche::Error::InvalidStreamState(id) => Error::InvalidStreamState(id),
            // the peer's stream limit, retried once it grants more
            quiche::Error::StreamLimit => Error::InsufficientCapacity,
            e => Error::Transport(quiche::h3::Error::TransportError(e)),
        }
    }
}

impl From<octets::BufferTooShortError> for Error {
    fn from(_: octets::BufferTooShortError) -> Self {
        Error::InsufficientCapacity
    }
}
