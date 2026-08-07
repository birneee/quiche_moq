use crate::bytes::{FromBytes, ToBytes};
use crate::error::Error;
use crate::vi64;
use crate::version;
use crate::Version;
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
    /// Create a varint KVP. `ty` must be even; returns an error otherwise.
    pub fn new_varint(ty: u64, value: u64) -> crate::Result<Self> {
        if !ty.is_multiple_of(2) {
            return Err(Error::ProtocolViolation(format!("varint KVP type must be even, got 0x{ty:x}")));
        }
        Ok(Self { ty, value: KeyValuePairValue::Varint(value) })
    }

    /// Create a byte-string KVP. `ty` must be odd; returns an error otherwise.
    pub fn new_bytes(ty: u64, value: Vec<u8>) -> crate::Result<Self> {
        if ty.is_multiple_of(2) {
            return Err(Error::ProtocolViolation(format!("byte-string KVP type must be odd, got 0x{ty:x}")));
        }
        Ok(Self { ty, value: KeyValuePairValue::Bytes(value) })
    }

    pub fn ty(&self) -> u64 {
        self.ty
    }

    pub fn value(&self) -> &KeyValuePairValue {
        &self.value
    }

    /// Returns the encoded byte length of this KVP for the given context.
    pub(crate) fn byte_length(&self, ctx: KvpCtx) -> usize {
        let type_field = if version::kvp_type_delta_encoded(ctx.version) {
            self.ty - ctx.previous_key
        } else {
            self.ty
        };
        vi64::varint_len(type_field, ctx.version) + match &self.value {
            KeyValuePairValue::Varint(v) => vi64::varint_len(*v, ctx.version),
            KeyValuePairValue::Bytes(b) => vi64::varint_len(b.len() as u64, ctx.version) + b.len(),
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
        let ty = if version::kvp_type_delta_encoded(ctx.version) {
            ctx.previous_key + vi64::read_varint(b, ctx.version)?
        } else {
            vi64::read_varint(b, ctx.version)?
        };
        let value = if ty % 2 == 0 {
            KeyValuePairValue::Varint(vi64::read_varint(b, ctx.version)?)
        } else {
            let len = vi64::read_varint(b, ctx.version)? as usize;
            KeyValuePairValue::Bytes(b.get_bytes(len)?.to_vec())
        };
        Ok(Self { ty, value })
    }
}

impl ToBytes<KvpCtx> for KeyValuePair {
    fn to_bytes(&self, b: &mut OctetsMut, ctx: KvpCtx) -> crate::error::Result<()> {
        if version::kvp_type_delta_encoded(ctx.version) {
            vi64::write_varint(b, self.ty - ctx.previous_key, ctx.version)?;
        } else {
            vi64::write_varint(b, self.ty, ctx.version)?;
        }
        match &self.value {
            KeyValuePairValue::Varint(v) => { vi64::write_varint(b, *v, ctx.version)?; }
            KeyValuePairValue::Bytes(v) => {
                vi64::write_varint(b, v.len() as u64, ctx.version)?;
                b.put_bytes(v)?;
            }
        }
        Ok(())
    }
}
