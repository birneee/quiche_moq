use crate::bytes::{FromBytes, ToBytes};
use crate::Version;
use octets::{Octets, OctetsMut};

#[derive(Debug, Eq, PartialEq)]
pub struct Location {
    pub group: u64,
    pub object: u64,
}

impl FromBytes for Location {
    fn from_bytes(b: &mut Octets, _version: Version) -> crate::error::Result<Self> {
        Ok(Self {
            group: b.get_varint()?,
            object: b.get_varint()?,
        })
    }
}

impl ToBytes for Location {
    fn to_bytes(&self, b: &mut OctetsMut, _version: Version) -> crate::error::Result<()> {
        b.put_varint(self.group)?;
        b.put_varint(self.object)?;
        Ok(())
    }
}
