use crate::bytes::{FromBytes, ToBytes};
use crate::vi64;
use crate::version;
use crate::Version;
use octets::{Octets, OctetsMut};

pub(crate) struct ControlMessageHeader {
    ty: u64,
    len: usize,
}

impl ControlMessageHeader {
    pub(crate) fn ty(&self) -> u64 {
        self.ty
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }
}

impl FromBytes for ControlMessageHeader {
    fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let ty = vi64::read_varint(b, version)?;
        let len = if version::message_length_is_fixed_u16(version) {
            b.get_u16()? as usize
        } else {
            vi64::read_varint(b, version)? as usize
        };
        Ok(Self{
            ty,
            len
        })
    }
}

impl ToBytes for ControlMessageHeader {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        vi64::write_varint(b, self.ty, version)?;
        if version::message_length_is_fixed_u16(version) {
            b.put_u16(self.len as u16)?;
        } else {
            vi64::write_varint(b, self.len as u64, version)?;
        }
        Ok(())
    }
}