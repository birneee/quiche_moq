pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Octets(octets::BufferTooShortError),
    FromUtf8Error(std::string::FromUtf8Error),
    ProtocolViolation(String),
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
