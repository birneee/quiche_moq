use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// A [`crate::Stream`] ended mid-value, retry once it holds `needed` bytes from its start.
    Incomplete { needed: usize },
    /// The input breaks the encoding, more input can't fix it.
    Malformed(&'static str),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Incomplete { needed } => write!(f, "incomplete, needs {needed} bytes"),
            Self::Malformed(reason) => write!(f, "malformed: {reason}"),
        }
    }
}

impl core::error::Error for DecodeError {}

/// A value the wire can't carry, e.g. a QUIC varint of 2^62 or more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeError(pub &'static str);

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "unencodable: {}", self.0)
    }
}

impl core::error::Error for EncodeError {}
