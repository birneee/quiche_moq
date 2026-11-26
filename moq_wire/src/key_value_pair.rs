use crate::prelude::*;
use crate::error::{Error, Result};
use crate::{Mode, Reader, VarIntForm, Version, Writer};
use moq_wire_primitives::BoundedBytes;

/// The longest value of a pair from draft 11 (draft-ietf-moq-transport-19 §1.4.2).
const MAX_VALUE_LEN: usize = u16::MAX as usize;

/// Type of a pair holding a varint, even (draft-ietf-moq-transport-16 §1.4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VarintKey(u64);

/// Type of a pair holding bytes, odd (draft-ietf-moq-transport-16 §1.4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BytesKey(u64);

impl VarintKey {
    /// Panics if `ty` is odd, in a `const` at compile time.
    pub const fn new(ty: u64) -> Self {
        assert!(ty.is_multiple_of(2), "the type of a varint pair is even");
        Self(ty)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl BytesKey {
    /// Panics if `ty` is even, in a `const` at compile time.
    pub const fn new(ty: u64) -> Self {
        assert!(!ty.is_multiple_of(2), "the type of a bytes pair is odd");
        Self(ty)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A parameter or property: even types hold a varint, odd types bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyValuePair {
    Varint(VarintKey, u64),
    Bytes(BytesKey, Vec<u8>),
}

impl KeyValuePair {
    /// `None` if `ty` is odd.
    pub(crate) fn new_varint(ty: u64, v: u64) -> Option<Self> {
        ty.is_multiple_of(2).then_some(Self::Varint(VarintKey(ty), v))
    }

    /// `None` if `ty` is even.
    pub(crate) fn new_bytes(ty: u64, v: Vec<u8>) -> Option<Self> {
        (!ty.is_multiple_of(2)).then_some(Self::Bytes(BytesKey(ty), v))
    }

    pub fn ty(&self) -> u64 {
        match self {
            Self::Varint(key, _) => key.0,
            Self::Bytes(key, _) => key.0,
        }
    }

    /// The value of a pair of type `ty`, read as its parity says.
    pub(crate) fn decode_value<M: Mode>(ty: u64, r: &mut Reader<'_, M>, version: Version) -> Result<Self> {
        if ty.is_multiple_of(2) {
            return Ok(Self::Varint(VarintKey(ty), r.varint()?));
        }
        let bytes = match version.limits_pair_values() {
            true => BoundedBytes::<{ MAX_VALUE_LEN }>::decode(r)?.into_inner(),
            false => r.length_prefixed_bytes()?.to_vec(),
        };
        Ok(Self::Bytes(BytesKey(ty), bytes))
    }

    pub(crate) fn encode_value(&self, w: &mut Writer, version: Version) -> Result<()> {
        match self {
            Self::Varint(_, v) => w.varint(*v)?,
            Self::Bytes(_, bytes) if version.limits_pair_values() && bytes.len() > MAX_VALUE_LEN => {
                return Err(Error::Unencodable("a key-value pair value over 2^16-1 bytes"));
            }
            Self::Bytes(_, bytes) => w.length_prefixed_bytes(bytes)?,
        }
        Ok(())
    }
}

/// Draft 7 to 10 parameters: every value is length-prefixed bytes, holding a varint for an even type.
impl KeyValuePair {
    /// `None` for an even type whose bytes aren't one varint, e.g. AUTHORIZATION_INFO (0x02) before draft 11.
    pub(crate) fn decode_legacy<M: Mode>(r: &mut Reader<'_, M>) -> Result<Option<Self>> {
        let ty = r.varint()?;
        let value = r.length_prefixed_bytes()?;
        if ty % 2 == 1 {
            return Ok(Some(Self::Bytes(BytesKey(ty), value.to_vec())));
        }
        let mut value = Reader::frame_of(value, VarIntForm::Quic);
        let varint = value.varint().ok().filter(|_| value.is_empty());
        Ok(varint.map(|v| Self::Varint(VarintKey(ty), v)))
    }

    pub(crate) fn encode_legacy(&self, w: &mut Writer) -> Result<()> {
        w.varint(self.ty())?;
        match self {
            Self::Varint(_, v) => w.with_varint_length(|w| w.varint(*v))?,
            Self::Bytes(_, bytes) => w.length_prefixed_bytes(bytes)?,
        }
        Ok(())
    }
}
