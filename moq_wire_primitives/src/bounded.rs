use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Deref;

use crate::{DecodeError, EncodeError, Mode, Reader, Writer};

/// A value over the bound of its type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooLong {
    pub len: usize,
    pub max: usize,
}

/// At most `MAX` bytes, encoded after a varint length.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BoundedBytes<const MAX: usize>(Vec<u8>);

impl<const MAX: usize> BoundedBytes<MAX> {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, TooLong> {
        let bytes = bytes.into();
        match bytes.len() <= MAX {
            true => Ok(Self(bytes)),
            false => Err(TooLong { len: bytes.len(), max: MAX }),
        }
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.0
    }
}

impl<const MAX: usize> Deref for BoundedBytes<MAX> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.0
    }
}

/// At most `MAX` bytes of UTF-8, encoded after a varint length.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BoundedString<const MAX: usize>(String);

impl<const MAX: usize> BoundedString<MAX> {
    pub fn new(s: impl Into<String>) -> Result<Self, TooLong> {
        let s = s.into();
        match s.len() <= MAX {
            true => Ok(Self(s)),
            false => Err(TooLong { len: s.len(), max: MAX }),
        }
    }

    /// The first `MAX` bytes of `s`, cut at a character boundary.
    pub fn truncated(s: &str) -> Self {
        let end = s.floor_char_boundary(MAX);
        Self(String::from(&s[..end]))
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl<const MAX: usize> Deref for BoundedString<MAX> {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl<const MAX: usize> BoundedBytes<MAX> {
    pub fn encode(&self, w: &mut Writer) -> Result<(), EncodeError> {
        w.length_prefixed_bytes(&self.0)
    }

    /// A varint length of at most `MAX`, then the bytes.
    pub fn decode<M: Mode>(r: &mut Reader<'_, M>) -> Result<Self, DecodeError> {
        bounded(r, MAX).map(|b| Self(b.to_vec()))
    }
}

impl<const MAX: usize> BoundedString<MAX> {
    pub fn encode(&self, w: &mut Writer) -> Result<(), EncodeError> {
        w.length_prefixed_bytes(self.0.as_bytes())
    }

    /// A varint length of at most `MAX`, then the UTF-8 bytes.
    pub fn decode<M: Mode>(r: &mut Reader<'_, M>) -> Result<Self, DecodeError> {
        let bytes = bounded(r, MAX)?;
        let s = core::str::from_utf8(bytes).map_err(|_| DecodeError::Malformed("invalid UTF-8"))?;
        Ok(Self(String::from(s)))
    }
}

/// The bytes after a varint length of at most `max`, consuming nothing if it fails.
fn bounded<'a, M: Mode>(r: &mut Reader<'a, M>, max: usize) -> Result<&'a [u8], DecodeError> {
    let mut ahead = r.clone();
    let len = ahead.varint()?;
    if len > max as u64 {
        return Err(DecodeError::Malformed("length over the field's bound"));
    }
    let bytes = ahead.bytes(len)?;
    *r = ahead;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VarIntForm;
    use alloc::vec;

    type Phrase = BoundedString<4>;

    #[test]
    fn bound_is_checked_on_build_and_decode() {
        assert_eq!(Phrase::new("abcde"), Err(TooLong { len: 5, max: 4 }));
        assert_eq!(&*Phrase::truncated("abé"), "abé");
        assert_eq!(&*Phrase::truncated("abcé"), "abc");

        let mut r = Reader::new(&[5, b'a', b'b', b'c', b'd', b'e'], VarIntForm::Quic);
        assert!(matches!(Phrase::decode(&mut r), Err(DecodeError::Malformed(_))));
        let mut r = Reader::new(&[2, 0xff, 0xfe], VarIntForm::Quic);
        assert!(matches!(Phrase::decode(&mut r), Err(DecodeError::Malformed(_))));
    }

    #[test]
    fn round_trips() {
        let mut buf = vec![];
        let phrase = Phrase::new("ab").unwrap();
        phrase.encode(&mut Writer::new(&mut buf, VarIntForm::Quic)).unwrap();
        assert_eq!(Phrase::decode(&mut Reader::new(&buf, VarIntForm::Quic)), Ok(phrase));
    }
}
