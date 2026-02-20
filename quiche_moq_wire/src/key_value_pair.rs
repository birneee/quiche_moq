use crate::bytes::{FromBytes, ToBytes};
use crate::{Version, MOQ_VERSION_DRAFT_15};
use octets::{Octets, OctetsMut};

/// Context for reading/writing Key-Value-Pairs.
/// In draft-15+ the type field is delta-encoded relative to the previous key in the sequence.
/// Set `previous_key` to 0 for the first KVP in a sequence.
#[derive(Clone, Copy)]
pub(crate) struct KvpCtx {
    pub(crate) version: Version,
    pub(crate) previous_key: u64,
}

impl KvpCtx {
    pub(crate) fn new(version: Version) -> Self {
        Self { version, previous_key: 0 }
    }
    pub(crate) fn with_previous_key(self, previous_key: u64) -> Self {
        Self { previous_key, ..self }
    }
}

/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-11.html#name-key-value-pair-structure
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct KeyValuePair {
    pub(crate) ty: u64,
    pub(crate) value: KeyValuePairValue,
}

impl KeyValuePair {
    #[allow(unused)]
    pub(crate) fn new_varint(ty: u64, value: u64) -> Self {
        debug_assert!(ty.is_multiple_of(2));
        Self {
            ty,
            value: KeyValuePairValue::Varint(value),
        }
    }

    #[allow(unused)]
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

impl FromBytes<KvpCtx> for KeyValuePair {
    fn from_bytes(b: &mut Octets, ctx: KvpCtx) -> crate::error::Result<Self> {
        let ty = if ctx.version >= MOQ_VERSION_DRAFT_15 {
            ctx.previous_key + b.get_varint()?
        } else {
            b.get_varint()?
        };
        let value = if ty % 2 == 0 {
            KeyValuePairValue::Varint(b.get_varint()?)
        } else {
            let len = b.get_varint()? as usize;
            KeyValuePairValue::Bytes(b.get_bytes(len)?.to_vec())
        };
        Ok(Self { ty, value })
    }
}

impl ToBytes<KvpCtx> for KeyValuePair {
    fn to_bytes(&self, b: &mut OctetsMut, ctx: KvpCtx) -> crate::error::Result<()> {
        if ctx.version >= MOQ_VERSION_DRAFT_15 {
            b.put_varint(self.ty - ctx.previous_key)?;
        } else {
            b.put_varint(self.ty)?;
        }
        match &self.value {
            KeyValuePairValue::Varint(v) => { b.put_varint(*v)?; }
            KeyValuePairValue::Bytes(v) => {
                b.put_varint(v.len() as u64)?;
                b.put_bytes(v)?;
            }
        }
        Ok(())
    }
}
