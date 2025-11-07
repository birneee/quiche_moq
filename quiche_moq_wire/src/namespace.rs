use crate::bytes::{FromBytes, ToBytes};
use crate::Version;
use octets::{Octets, OctetsMut};
use std::fmt::{Debug, Formatter};
use crate::tuple::Tuple;

pub struct Namespace(pub Tuple);

impl FromBytes for Namespace {
    fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        Ok(Self(Tuple::from_bytes(b, version)?))
    }
}

impl ToBytes for Namespace {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        self.0.to_bytes(b, version)
    }
}

impl Debug for Namespace {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.0.0.iter().map(|v| String::from_utf8_lossy(v.as_slice())))
            .finish()
    }
}
