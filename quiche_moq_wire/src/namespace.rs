use crate::bytes::{FromBytes, ToBytes};
use crate::Version;
use octets::{Octets, OctetsMut};
use std::fmt::{Debug, Display, Formatter};
use crate::tuple::Tuple;

#[derive(Eq, PartialEq, Clone, Hash)]
pub struct Namespace(pub Tuple);

impl Namespace {
    pub fn len(&self) -> usize {
        self.0.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.0.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Vec<u8>> {
        self.0.0.iter()
    }
}

impl<'a> IntoIterator for &'a Namespace {
    type Item = &'a Vec<u8>;
    type IntoIter = std::slice::Iter<'a, Vec<u8>>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

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

impl Display for Namespace {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        for (i, part) in self.0.0.iter().enumerate() {
            if i > 0 {
                f.write_str("-")?;
            }
            crate::namespace_trackname::write_escape(f, part)?;
        }
        Ok(())
    }
}