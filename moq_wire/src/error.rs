use crate::prelude::*;
use moq_wire_primitives::{DecodeError, EncodeError};

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// The input ended mid-message, retry once it holds `needed` bytes from where decoding started.
    Incomplete { needed: usize },
    ProtocolViolation(String),
    /// A valid message this crate can't handle.
    Unsupported(String),
    /// A control message type this crate doesn't know in the draft, the type.
    UnknownMessage(u64),
    /// A value the wire can't carry, e.g. a QUIC varint of 2^62 or more.
    Unencodable(&'static str),
}

impl From<DecodeError> for Error {
    fn from(err: DecodeError) -> Self {
        match err {
            DecodeError::Incomplete { needed } => Error::Incomplete { needed },
            DecodeError::Malformed(reason) => Error::ProtocolViolation(reason.into()),
        }
    }
}

impl From<EncodeError> for Error {
    fn from(err: EncodeError) -> Self {
        Error::Unencodable(err.0)
    }
}
