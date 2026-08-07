use crate::transport::StreamError;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Octets(octets::BufferTooShortError),
    PartialControlMessage,
    FromUtf8Error(std::string::FromUtf8Error),
    Unimplemented,
    IO(std::io::Error),
    Done,
    Fin,
    InsufficientCapacity,
    ObjectToLong,
    UnfinishedPayload,
    ExceededPayload,
    /// Insufficient MAX_REQUEST_ID quota from peer.
    RequestBlocked,
    /// Object is older than the subscriber's delivery_timeout or max_cache_duration; not sent.
    ObjectExpired,
    Wire(moq_wire::Error),
    Transport(StreamError),
    InvalidNamespaceTrackname(String),
}

impl From<moq_wire::Error> for Error {
    fn from(err: moq_wire::Error) -> Self {
        Error::Wire(err)
    }
}

impl From<octets::BufferTooShortError> for Error {
    fn from(err: octets::BufferTooShortError) -> Self {
        Error::Octets(err)
    }
}

impl From<std::string::FromUtf8Error> for Error {
    fn from(err: std::string::FromUtf8Error) -> Self {
        Error::FromUtf8Error(err)
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::IO(err)
    }
}

impl From<StreamError> for Error {
    fn from(err: StreamError) -> Self {
        match err {
            StreamError::Done => Error::Done,
            StreamError::Fin => Error::Fin,
            StreamError::InsufficientCapacity => Error::InsufficientCapacity,
            other => Error::Transport(other),
        }
    }
}
