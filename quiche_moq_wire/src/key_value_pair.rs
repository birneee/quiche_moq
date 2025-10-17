use crate::bytes::{FromBytes, ToBytes};
use crate::Version;
use octets::{Octets, OctetsMut};

/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-11.html#name-key-value-pair-structure
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct KeyValuePair {
    pub(crate) ty: u64,
    pub(crate) value: KeyValuePairValue,
}

impl KeyValuePair {
    pub(crate) fn new_varint(ty: u64, value: u64) -> Self {
        debug_assert!(ty % 2 == 0);
        Self {
            ty,
            value: KeyValuePairValue::Varint(value),
        }
    }

    pub(crate) fn new_bytes(ty: u64, value: Vec<u8>) -> Self {
        debug_assert!(ty % 2 == 1);
        Self {
            ty,
            value: KeyValuePairValue::Bytes(value),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum KeyValuePairValue {
    Varint(u64),
    Bytes(Vec<u8>),
}

impl TryInto<u64> for KeyValuePairValue {
    type Error = ();

    fn try_into(self) -> Result<u64, Self::Error> {
        let KeyValuePairValue::Varint(value) = self else { return Err(()) };
        Ok(value)
    }
}

impl TryInto<Vec<u8>> for KeyValuePairValue {
    type Error = ();

    fn try_into(self) -> Result<Vec<u8>, Self::Error> {
        let KeyValuePairValue::Bytes(value) = self else { return Err(()) };
        Ok(value)
    }
}

impl FromBytes for KeyValuePair {
    fn from_bytes(b: &mut Octets, _version: Version) -> crate::error::Result<Self> {
        let ty = b.get_varint()?;
        let value = match ty % 2 == 0 {
            true => KeyValuePairValue::Varint(b.get_varint()?),
            false => {
                let value_len = b.get_varint()? as usize;
                KeyValuePairValue::Bytes(b.get_bytes(value_len)?.to_vec())
            }
        };
        Ok(Self {
            ty,
            value,
        })
    }
}

impl ToBytes for KeyValuePair {
    fn to_bytes(&self, b: &mut OctetsMut, _version: Version) -> crate::error::Result<()> {
        b.put_varint(self.ty)?;
        match &self.value {
            KeyValuePairValue::Varint(v) => { b.put_varint(*v)?; }
            KeyValuePairValue::Bytes(v) => {
                b.put_varint(v.len() as u64)?;
                b.put_bytes(&v)?;
            }
        }
        Ok(())
    }
}