use octets::Octets;

/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-reason-phrase-structure
#[derive(Debug)]
pub struct ReasonPhrase(pub String);

impl ReasonPhrase {
    pub(crate) fn from_bytes(b: &mut Octets) -> crate::error::Result<Self> {
        let len = b.get_varint()?;
        let value = String::from_utf8(b.get_bytes(len as usize)?.to_vec())?;
        Ok(Self(value))
    }
}
