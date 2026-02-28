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
    /// associated data.
    StreamReset(u64),
    /// The operation cannot be completed because the stream is in an
    /// invalid state.
    ///
    /// The stream ID is provided as associated data.
    InvalidStreamState(u64),
}

impl From<octets::BufferTooShortError> for Error {
    fn from(_: octets::BufferTooShortError) -> Self {
        Error::InsufficientCapacity
    }
}
