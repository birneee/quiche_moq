use octets::Octets;
use crate::bytes::FromBytes;
use crate::error::Result;
use crate::Version;

#[derive(Debug)]
pub struct Tuple(pub(crate) Vec<Vec<u8>>);

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
