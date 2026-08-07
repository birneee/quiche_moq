use octets::{Octets, OctetsMut};
use crate::error::Result;
use crate::Version;

pub trait FromBytes<CTX = Version>: Sized {
    fn from_bytes(b: &mut Octets, ctx: CTX) -> Result<Self>;
}

pub trait ToBytes<CTX = Version> {
    fn to_bytes(&self, b: &mut OctetsMut, ctx: CTX) -> Result<()>;
}
