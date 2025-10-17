use crate::bytes::{FromBytes, ToBytes};
use crate::{Version, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_13};
use octets::{Octets, OctetsMut};
use crate::key_value_pair::{KeyValuePair, KeyValuePairValue};

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

impl FromBytes for Parameter {
    fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        Ok(match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
                let ty = b.get_varint()?;
                let len = b.get_varint()?;
                let value = b.get_bytes(len as usize)?;
                Self {
                    ty,
                    value: ParameterValue::Bytes(value.to_vec()),
                }
            }
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => {
                let kvp = KeyValuePair::from_bytes(b, version)?;
                Self{
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

impl ToBytes for Parameter {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
                b.put_varint(self.ty)?;
                match &self.value {
                    ParameterValue::Varint(v) => {
                        b.put_varint(1)?;
                        b.put_u8(*v as u8)?;
                    }
                    ParameterValue::Bytes(v) => {
                        b.put_varint(v.len() as u64)?;
                        b.put_bytes(&v)?;
                    }
                }
            }
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => {
                let kvp = KeyValuePair {
                    ty: self.ty,
                    value: match &self.value {
                        ParameterValue::Bytes(v) => KeyValuePairValue::Bytes(v.clone()),
                        ParameterValue::Varint(v) => KeyValuePairValue::Varint(*v),
                    },
                };
                kvp.to_bytes(b, version)?;
            }
            _ => unimplemented!()
        }
        Ok(())
    }
}
