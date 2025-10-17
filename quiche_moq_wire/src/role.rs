use crate::bytes::{FromBytes, ToBytes};
use crate::error::{Error, Result};
use crate::{Version, MOQ_VERSION_DRAFT_07};
use octets::{Octets, OctetsMut};

/// not used after draft7
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Role {
    Publisher,
    Subscriber,
    PubSub,
}

impl Role {
    pub fn from_id(v: u64) -> Result<Self> {
        Ok(match v {
            0x01 => Self::Publisher,
            0x02 => Self::Subscriber,
            0x03 => Self::PubSub,
            _ => return Err(Error::ProtocolViolation("unexpected role".to_string()))
        })
    }

    pub fn to_id(&self) -> u64 {
        match self {
            Role::Publisher => 0x01,
            Role::Subscriber => 0x02,
            Role::PubSub => 0x03,
        }
    }
}

impl FromBytes for Role {
    fn from_bytes(b: &mut Octets, version: Version) -> Result<Self> {
        assert_eq!(version, MOQ_VERSION_DRAFT_07);
        Ok(Self::from_id(b.get_varint()?)?)
    }
}

impl ToBytes for Role {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> Result<()> {
        assert_eq!(version, MOQ_VERSION_DRAFT_07);
        b.put_varint(self.to_id())?;
        Ok(())
    }
}