use octets::{Octets, OctetsMut};

/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-reason-phrase-structure
#[derive(Debug)]
pub struct ReasonPhrase(pub String);

impl ReasonPhrase {
    pub(crate) fn from_bytes(b: &mut Octets) -> crate::error::Result<Self> {
        let len = b.get_varint()?;
        let value = String::from_utf8(b.get_bytes(len as usize)?.to_vec())?;
        Ok(Self(value))
    }

    pub(crate) fn to_bytes(&self, b: &mut OctetsMut) -> crate::error::Result<()> {
        b.put_varint(self.0.len() as u64)?;
        b.put_bytes(self.0.as_bytes())?;
        Ok(())
    }
}
