use octets::{Octets, OctetsMut};
use crate::bytes::FromBytes;
use crate::error::Result;
use crate::vi64;
use crate::{ToBytes, Version};

#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub struct Tuple(pub Vec<Vec<u8>>);

impl FromBytes for Tuple {
    fn from_bytes(b: &mut Octets, version: Version) -> Result<Self> {
        let num_fields = vi64::read_varint(b, version)?;
        let mut fields = Vec::with_capacity(num_fields as usize);
        for _ in 0..num_fields {
            let len = vi64::read_varint(b, version)? as usize;
            let data = b.get_bytes(len)?.to_vec();
            fields.push(data);
        }
        Ok(Tuple(fields))
    }
}

impl ToBytes for Tuple {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> Result<()> {
        vi64::write_varint(b, self.0.len() as u64, version)?;
        for field in &self.0 {
            vi64::write_varint(b, field.len() as u64, version)?;
            b.put_bytes(field)?;
        }
        Ok(())
    }
}
