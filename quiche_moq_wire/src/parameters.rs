use crate::bytes::{FromBytes, ToBytes};
use crate::{Parameter, Version};
use octets::{Octets, OctetsMut};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Parameters(pub Vec<Parameter>);

impl Parameters {
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl FromBytes for Parameters {
    /// including the length varint
    fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let mut params = vec![];
        let number_of_parameters = b.get_varint()?;
        for _ in 0..number_of_parameters {
            params.push(Parameter::from_bytes(b, version)?);
        }
        Ok(Self(params))
    }
}

impl ToBytes for Parameters {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        b.put_varint(self.0.len() as u64)?;
        for p in &self.0 {
            p.to_bytes(b, version)?;
        }
        Ok(())
    }
}
