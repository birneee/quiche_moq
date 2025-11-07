use crate::bytes::{FromBytes, ToBytes};
use crate::{Version, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_13};
use octets::{Octets, OctetsMut};

pub(crate) struct ControlMessageHeader {
    ty: u64,
    len: usize,
}

impl ControlMessageHeader {
    pub(crate) fn ty(&self) -> u64 {
        self.ty
    }

    pub(crate) fn len(&self) -> usize { self.len }

    pub fn payload_length(&self) -> usize {
        self.len
    }
}

impl FromBytes for ControlMessageHeader {
    fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let ty = b.get_varint()?;
        let len = match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
                b.get_varint()? as usize
            }
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => {
                b.get_u16()? as usize
            }
            _ => unimplemented!()
        };
        Ok(Self{
            ty,
            len
        })
    }
}

impl ToBytes for ControlMessageHeader {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        b.put_varint(self.ty)?;
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
                b.put_varint(self.len as u64)?;
            }
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => {
                b.put_u16(self.len as u16)?;
            }
            _ => unimplemented!()
        };
        Ok(())
    }
}