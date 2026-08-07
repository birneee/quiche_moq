use crate::vi64;
use crate::Version;
use std::fmt::{Display, Formatter};
use octets::{Octets, OctetsMut};

/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-reason-phrase-structure
#[derive(Debug)]
pub struct ReasonPhrase(pub String);

impl ReasonPhrase {
    pub(crate) fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let len = vi64::read_varint(b, version)?;
        let value = String::from_utf8(b.get_bytes(len as usize)?.to_vec())?;
        Ok(Self(value))
    }

    pub(crate) fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        vi64::write_varint(b, self.0.len() as u64, version)?;
        b.put_bytes(self.0.as_bytes())?;
        Ok(())
    }
}

impl Display for ReasonPhrase {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
