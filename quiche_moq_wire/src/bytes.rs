use octets::{Octets, OctetsMut};
use crate::error::Result;
use crate::Version;

pub trait FromBytes: Sized {
    fn from_bytes(b: &mut Octets, version: Version) -> Result<Self>;
}

pub trait ToBytes {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> Result<()>;
}