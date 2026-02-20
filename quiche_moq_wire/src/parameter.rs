use crate::bytes::{FromBytes, ToBytes};
use crate::{MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_16};
use octets::{Octets, OctetsMut};
use crate::key_value_pair::{KeyValuePair, KeyValuePairValue, KvpCtx};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Parameter{
    pub(crate) ty: u64,
    pub(crate) value: ParameterValue,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ParameterValue {
    Bytes(Vec<u8>),
    Varint(u64),
}

impl Parameter {
    pub fn ty(&self) -> u64 {
        self.ty
    }

    pub fn value(&self) -> &ParameterValue {
        &self.value
    }

    pub(crate) fn new_bytes(ty: u64, value: Vec<u8>) -> Self {
        Self { ty, value: ParameterValue::Bytes(value) }
    }

    pub(crate) fn new_varint(ty: u64, value: u64) -> Self {
        Self { ty, value: ParameterValue::Varint(value) }
    }
}

impl From<KeyValuePair> for Parameter {
    fn from(kvp: KeyValuePair) -> Self {
        Self {
            ty: kvp.ty,
            value: match kvp.value {
                KeyValuePairValue::Varint(v) => ParameterValue::Varint(v),
                KeyValuePairValue::Bytes(b) => ParameterValue::Bytes(b),
            },
        }
    }
}

impl From<Parameter> for KeyValuePair {
    fn from(p: Parameter) -> Self {
        Self {
            ty: p.ty,
            value: match p.value {
                ParameterValue::Varint(v) => KeyValuePairValue::Varint(v),
                ParameterValue::Bytes(b) => KeyValuePairValue::Bytes(b),
            },
        }
    }
}

impl FromBytes<KvpCtx> for Parameter {
    fn from_bytes(b: &mut Octets, ctx: KvpCtx) -> crate::error::Result<Self> {
        Ok(match ctx.version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
                let ty = b.get_varint()?;
                let len = b.get_varint()?;
                let value = b.get_bytes(len as usize)?;
                Self {
                    ty,
                    value: ParameterValue::Bytes(value.to_vec()),
                }
            }
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_16 => {
                let kvp = KeyValuePair::from_bytes(b, ctx)?;
                Self {
                    ty: kvp.ty,
                    value: match kvp.value {
                        KeyValuePairValue::Bytes(v) => ParameterValue::Bytes(v),
                        KeyValuePairValue::Varint(v) => ParameterValue::Varint(v),
                    },
                }
            }
            _ => unimplemented!()
        })
    }
}

impl ToBytes<KvpCtx> for Parameter {
    fn to_bytes(&self, b: &mut OctetsMut, ctx: KvpCtx) -> crate::error::Result<()> {
        match ctx.version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
                b.put_varint(self.ty)?;
                match &self.value {
                    ParameterValue::Varint(v) => {
                        b.put_varint(1)?;
                        b.put_u8(*v as u8)?;
                    }
                    ParameterValue::Bytes(v) => {
                        b.put_varint(v.len() as u64)?;
                        b.put_bytes(v)?;
                    }
                }
            }
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_16 => {
                let kvp = KeyValuePair {
                    ty: self.ty,
                    value: match &self.value {
                        ParameterValue::Bytes(v) => KeyValuePairValue::Bytes(v.clone()),
                        ParameterValue::Varint(v) => KeyValuePairValue::Varint(*v),
                    },
                };
                kvp.to_bytes(b, ctx)?;
            }
            _ => unimplemented!()
        }
        Ok(())
    }
}
