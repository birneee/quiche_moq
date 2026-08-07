use octets::{Octets, OctetsMut};

use crate::bytes::{FromBytes, ToBytes};
use crate::key_value_pair::{KeyValuePair, KvpCtx};
use crate::version;
use crate::Version;

/// A sequence of `KeyValuePair`s without a count prefix.
/// For draft-15+, pairs are sorted by type ID before delta-encoding so deltas never underflow.
#[derive(Clone, Debug, Default)]
pub struct KeyValuePairs(pub(crate) Vec<KeyValuePair>);

impl KeyValuePairs {
    pub fn new() -> Self {
        KeyValuePairs(vec![])
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn push(&mut self, value: KeyValuePair) {
        self.0.push(value)
    }

    pub fn iter(&self) -> impl Iterator<Item = &KeyValuePair> {
        self.0.iter()
    }

    /// Returns the encoded byte length of all KVPs for the given version.
    pub fn byte_length(&self, version: Version) -> usize {
        let sorted;
        let pairs: &[KeyValuePair] = if version::kvp_type_delta_encoded(version) {
            sorted = { let mut v = self.0.clone(); v.sort_by_key(|p| p.ty); v };
            &sorted
        } else {
            &self.0
        };
        let mut total = 0usize;
        let mut prev_key = 0u64;
        for p in pairs {
            total += p.byte_length(KvpCtx { version, previous_key: prev_key });
            prev_key = p.ty;
        }
        total
    }
}

impl FromBytes<(Version, u64)> for KeyValuePairs {
    fn from_bytes(b: &mut Octets, (version, count): (Version, u64)) -> crate::error::Result<Self> {
        let mut pairs = vec![];
        let mut prev_key = 0u64;
        for _ in 0..count {
            let p = KeyValuePair::from_bytes(b, KvpCtx { version, previous_key: prev_key })?;
            prev_key = p.ty;
            pairs.push(p);
        }
        Ok(Self(pairs))
    }
}

impl KeyValuePairs {
    /// Reads KVPs with no leading count, consuming until `b` is exhausted. Used for
    /// draft-18+ "Track Properties" fields, whose length is implicitly the remaining
    /// bytes of the enclosing message rather than a count prefix.
    pub(crate) fn from_bytes_until_exhausted(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let mut pairs = vec![];
        let mut prev_key = 0u64;
        while b.off() < b.cap() {
            let p = KeyValuePair::from_bytes(b, KvpCtx { version, previous_key: prev_key })?;
            prev_key = p.ty;
            pairs.push(p);
        }
        Ok(Self(pairs))
    }
}

impl ToBytes for KeyValuePairs {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        let sorted;
        let pairs: &[KeyValuePair] = if version::kvp_type_delta_encoded(version) {
            sorted = { let mut v = self.0.clone(); v.sort_by_key(|p| p.ty); v };
            &sorted
        } else {
            &self.0
        };
        let mut prev_key = 0u64;
        for p in pairs {
            p.to_bytes(b, KvpCtx { version, previous_key: prev_key })?;
            prev_key = p.ty;
        }
        Ok(())
    }
}

impl From<Vec<KeyValuePair>> for KeyValuePairs {
    fn from(value: Vec<KeyValuePair>) -> Self {
        Self(value)
    }
}
