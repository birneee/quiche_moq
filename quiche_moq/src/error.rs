use quiche::h3;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    ProtocolViolation(String),
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
    /// Insufficient MAX_REQUEST_ID quota from peer
    RequestBlocked,
    Wire(quiche_moq_wire::Error),
}

impl From<quiche_moq_wire::Error> for Error {
    fn from(err: quiche_moq_wire::Error) -> Self {
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
