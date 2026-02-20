use crate::bytes::{FromBytes, ToBytes};
use crate::key_value_pair::{KeyValuePair, KvpCtx};
use crate::key_value_pairs::KeyValuePairs;
use crate::parameter::ParameterValue;
use crate::{Parameter, Version, MOQ_VERSION_DRAFT_10};
use octets::{Octets, OctetsMut};

#[derive(Debug, Clone)]
pub struct Parameters(pub Vec<Parameter>);

impl PartialEq for Parameters {
    /// Parameters are conceptually a map keyed by type ID, so ordering is irrelevant.
    fn eq(&self, other: &Self) -> bool {
        if self.0.len() != other.0.len() { return false; }
        let mut a = self.0.clone();
        let mut b = other.0.clone();
        a.sort_by_key(|p| p.ty);
        b.sort_by_key(|p| p.ty);
        a == b
    }
}
impl Eq for Parameters {}

impl Parameters {
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Get the varint value for the given even type ID.
    pub fn get_varint(&self, ty: u64) -> Option<u64> {
        debug_assert!(ty.is_multiple_of(2), "varint parameters have even type IDs");
        self.0.iter().find(|p| p.ty == ty).and_then(|p| {
            if let ParameterValue::Varint(v) = p.value { Some(v) } else { None }
        })
    }

    /// Get the byte-string value for the given odd type ID.
    pub fn get_bytes(&self, ty: u64) -> Option<&[u8]> {
        debug_assert!(ty % 2 == 1, "byte-string parameters have odd type IDs");
        self.0.iter().find(|p| p.ty == ty).and_then(|p| {
            if let ParameterValue::Bytes(ref v) = p.value { Some(v.as_slice()) } else { None }
        })
    }
}

impl FromBytes for Parameters {
    /// Reads a count-prefixed sequence of parameters.
    /// For draft-07–10 uses the old (ty + len + bytes) encoding via `Parameter::from_bytes`.
    /// For draft-11+ delegates to `KeyValuePairs` which handles delta-decoding for draft-15+.
    fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let count = b.get_varint()?;
        if version <= MOQ_VERSION_DRAFT_10 {
            let mut params = vec![];
            let mut prev_key = 0u64;
            for _ in 0..count {
                let p = Parameter::from_bytes(b, KvpCtx::new(version).with_previous_key(prev_key))?;
                prev_key = p.ty;
                params.push(p);
            }
            Ok(Self(params))
        } else {
            let kvps = KeyValuePairs::from_bytes(b, (version, count))?;
            Ok(Self(kvps.0.into_iter().map(Parameter::from).collect()))
        }
    }
}

impl ToBytes for Parameters {
    /// Writes a count-prefixed sequence of parameters.
    /// For draft-07–10 uses the old (ty + len + bytes) encoding via `Parameter::to_bytes`.
    /// For draft-11+ delegates to `KeyValuePairs` which sorts by type ID for draft-15+.
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        b.put_varint(self.0.len() as u64)?;
        if version <= MOQ_VERSION_DRAFT_10 {
            for p in &self.0 {
                p.to_bytes(b, KvpCtx::new(version))?;
            }
        } else {
            let kvps: Vec<KeyValuePair> = self.0.iter().cloned().map(KeyValuePair::from).collect();
            KeyValuePairs(kvps).to_bytes(b, version)?;
        }
        Ok(())
    }
}
