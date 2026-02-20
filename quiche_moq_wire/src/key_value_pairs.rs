use octets::{Octets, OctetsMut};

use crate::bytes::{FromBytes, ToBytes};
use crate::key_value_pair::{KeyValuePair, KvpCtx};
use crate::{MOQ_VERSION_DRAFT_15, Version};

/// A sequence of `KeyValuePair`s without a count prefix.
/// For draft-15+, pairs are sorted by type ID before delta-encoding so deltas never underflow.
pub(crate) struct KeyValuePairs(pub(crate) Vec<KeyValuePair>);

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

impl ToBytes for KeyValuePairs {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        let sorted;
        let pairs: &[KeyValuePair] = if version >= MOQ_VERSION_DRAFT_15 {
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
