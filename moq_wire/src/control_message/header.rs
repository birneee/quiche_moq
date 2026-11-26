use crate::bytes::FromBytes;
use crate::Version;
use crate::{Mode, Reader};

pub(crate) struct ControlMessageHeader {
    ty: u64,
    len: u64,
}

impl ControlMessageHeader {
    pub(crate) fn ty(&self) -> u64 {
        self.ty
    }

    pub(crate) fn len(&self) -> u64 {
        self.len
    }
}

impl FromBytes for ControlMessageHeader {
    fn from_bytes<M: Mode>(b: &mut Reader<'_, M>, version: Version) -> crate::error::Result<Self> {
        let ty = b.varint()?;
        let len = if version.message_length_is_fixed_u16() {
            u64::from(b.u16()?)
        } else {
            b.varint()?
        };
        Ok(Self{
            ty,
            len
        })
    }
}
