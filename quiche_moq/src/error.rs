use quiche::h3;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Octets(octets::BufferTooShortError),
    PartialControlMessage,
    FromUtf8Error(std::string::FromUtf8Error),
    Unimplemented,
    IO(std::io::Error),
    H3(h3::Error),
    WT(quiche_webtransport::Error),
    Done,
    Fin,
    InsufficientCapacity,
    ObjectToLong,
    UnfinishedPayload,
    ExceededPayload,
    /// Insufficient MAX_REQUEST_ID quota from peer
    RequestBlocked,
    /// Object is older than the subscriber's delivery_timeout or max_cache_duration; not sent.
    ObjectExpired,
    Wire(moq_wire::Error),
    InvalidNamespaceTrackname(String),
}

impl From<moq_core::Error> for Error {
    fn from(e: moq_core::Error) -> Self {
        match e {
            moq_core::Error::Octets(e) => Error::Octets(e),
            moq_core::Error::PartialControlMessage => Error::PartialControlMessage,
            moq_core::Error::FromUtf8Error(e) => Error::FromUtf8Error(e),
            moq_core::Error::Unimplemented => Error::Unimplemented,
            moq_core::Error::IO(e) => Error::IO(e),
            moq_core::Error::Done => Error::Done,
            moq_core::Error::Fin => Error::Fin,
            moq_core::Error::InsufficientCapacity => Error::InsufficientCapacity,
            moq_core::Error::ObjectToLong => Error::ObjectToLong,
            moq_core::Error::UnfinishedPayload => Error::UnfinishedPayload,
            moq_core::Error::ExceededPayload => Error::ExceededPayload,
            moq_core::Error::RequestBlocked => Error::RequestBlocked,
            moq_core::Error::ObjectExpired => Error::ObjectExpired,
            moq_core::Error::Wire(e) => Error::Wire(e),
            moq_core::Error::Transport(_) => unreachable!("raw transport errors are handled inside QuicheTransport"),
            moq_core::Error::InvalidNamespaceTrackname(s) => Error::InvalidNamespaceTrackname(s),
        }
    }
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

impl From<h3::Error> for Error {
    fn from(err: h3::Error) -> Self {
        Error::H3(err)
    }
}

impl From<quiche_webtransport::Error> for Error {
    fn from(err: quiche_webtransport::Error) -> Self {
        Error::WT(err)
    }
}
