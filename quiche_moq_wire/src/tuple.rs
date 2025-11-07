use octets::{Octets, OctetsMut};
use crate::bytes::FromBytes;
use crate::error::Result;
use crate::{ToBytes, Version};

#[derive(Debug)]
pub struct Tuple(pub Vec<Vec<u8>>);

impl FromBytes for Tuple {
    fn from_bytes(b: &mut Octets, _version: Version) -> Result<Self> {
        let num_fields = b.get_varint()?;
        let mut fields = Vec::with_capacity(num_fields as usize);
        for _ in 0..num_fields {
            let len = b.get_varint()? as usize;
            let data = b.get_bytes(len)?.to_vec();
            fields.push(data);
        }
        Ok(Tuple(fields))
    }
}

impl ToBytes for Tuple {
    fn to_bytes(&self, b: &mut OctetsMut, _version: Version) -> Result<()> {
        b.put_varint(self.0.len() as u64)?;
        for field in &self.0 {
            b.put_varint(field.len() as u64)?;
            b.put_bytes(field)?;
        }
        Ok(())
    }
}
