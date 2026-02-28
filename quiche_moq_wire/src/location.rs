use crate::bytes::{FromBytes, ToBytes};
use crate::Version;
use octets::{Octets, OctetsMut};
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy)]
pub struct Location {
    pub group: u64,
    pub object: u64,
}

/// draft-ietf-moq-transport-16 §1.4.1:
///   Location A < Location B if:
///   A.Group < B.Group || (A.Group == B.Group && A.Object < B.Object)
impl Ord for Location {
    fn cmp(&self, other: &Self) -> Ordering {
        self.group.cmp(&other.group).then(self.object.cmp(&other.object))
    }
}

impl PartialOrd for Location {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Location {
    fn eq(&self, other: &Self) -> bool {
        self.group == other.group && self.object == other.object
    }
}

impl Eq for Location {}

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
