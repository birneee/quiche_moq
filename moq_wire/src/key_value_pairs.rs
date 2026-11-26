use crate::prelude::*;
use crate::bytes::{FromBytes, ToBytes};
use crate::error::{Error, Result};
use crate::key_value_pair::{BytesKey, KeyValuePair, VarintKey};
use crate::{Frame, Mode, Reader, Version, Writer};

/// Pairs sorted by type, equal types in insertion order: the order delta-encoded types need from draft 15.
/// - message parameters: [`FromBytes`] / [`ToBytes`], a count and then the pairs
/// - object and track properties: [`Self::decode_to_end`] / [`Self::encode_uncounted`]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyValuePairs(Vec<KeyValuePair>);

impl KeyValuePairs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &KeyValuePair> {
        self.0.iter()
    }

    /// After the pairs of the same type.
    pub fn insert(&mut self, pair: KeyValuePair) {
        let at = self.0.partition_point(|p| p.ty() <= pair.ty());
        self.0.insert(at, pair);
    }

    pub fn with(mut self, pair: KeyValuePair) -> Self {
        self.insert(pair);
        self
    }

    pub fn retain(&mut self, keep: impl FnMut(&KeyValuePair) -> bool) {
        self.0.retain(keep);
    }

    /// The first value of `key`.
    pub fn get_varint(&self, key: VarintKey) -> Option<u64> {
        self.0.iter().find_map(|p| match p {
            KeyValuePair::Varint(k, v) if *k == key => Some(*v),
            _ => None,
        })
    }

    /// The first value of `key`.
    pub fn get_bytes(&self, key: BytesKey) -> Option<&[u8]> {
        self.0.iter().find_map(|p| match p {
            KeyValuePair::Bytes(k, v) if *k == key => Some(v.as_slice()),
            _ => None,
        })
    }

    /// Removes the first value of `key`, e.g. to turn a parameter into a message field.
    pub fn take_varint(&mut self, key: VarintKey) -> Option<u64> {
        let at = self.0.iter().position(|p| matches!(p, KeyValuePair::Varint(k, _) if *k == key))?;
        match self.0.remove(at) {
            KeyValuePair::Varint(_, v) => Some(v),
            KeyValuePair::Bytes(..) => None,
        }
    }

    /// Removes the first value of `key`, e.g. to turn a parameter into a message field.
    pub fn take_bytes(&mut self, key: BytesKey) -> Option<Vec<u8>> {
        let at = self.0.iter().position(|p| matches!(p, KeyValuePair::Bytes(k, _) if *k == key))?;
        match self.0.remove(at) {
            KeyValuePair::Bytes(_, v) => Some(v),
            KeyValuePair::Varint(..) => None,
        }
    }

    /// Every pair up to the end of `r`, e.g. draft 18 track properties.
    pub fn decode_to_end(r: &mut Reader<'_, Frame>, version: Version) -> Result<Self> {
        let mut types = TypeCoding::new(version);
        let mut pairs = vec![];
        while !r.is_empty() {
            pairs.push(types.decode_pair(r)?);
        }
        Ok(pairs.into_iter().collect())
    }

    /// A count, then the pairs: draft 8 object properties.
    pub(crate) fn decode_counted<M: Mode>(r: &mut Reader<'_, M>, version: Version) -> Result<Self> {
        let count = r.varint()?;
        let mut types = TypeCoding::new(version);
        let mut pairs = vec![];
        for _ in 0..count {
            pairs.push(types.decode_pair(r)?);
        }
        Ok(pairs.into_iter().collect())
    }

    pub(crate) fn encode_counted(&self, w: &mut Writer, version: Version) -> Result<()> {
        w.varint(self.0.len() as u64)?;
        self.encode_uncounted(w, version)
    }

    /// The pairs without a count, e.g. object properties after their length.
    pub fn encode_uncounted(&self, w: &mut Writer, version: Version) -> Result<()> {
        let mut types = TypeCoding::new(version);
        self.0.iter().try_for_each(|pair| types.encode_pair(w, pair))
    }
}

impl FromIterator<KeyValuePair> for KeyValuePairs {
    fn from_iter<I: IntoIterator<Item = KeyValuePair>>(pairs: I) -> Self {
        let mut pairs: Vec<_> = pairs.into_iter().collect();
        pairs.sort_by_key(KeyValuePair::ty);
        Self(pairs)
    }
}

impl FromBytes for KeyValuePairs {
    fn from_bytes<M: Mode>(r: &mut Reader<'_, M>, version: Version) -> Result<Self> {
        if !version.legacy_parameter_encoding() {
            return Self::decode_counted(r, version);
        }
        let count = r.varint()?;
        let mut pairs = vec![];
        for _ in 0..count {
            pairs.extend(KeyValuePair::decode_legacy(r)?);
        }
        Ok(pairs.into_iter().collect())
    }
}

impl ToBytes for KeyValuePairs {
    fn to_bytes(&self, w: &mut Writer, version: Version) -> Result<()> {
        if !version.legacy_parameter_encoding() {
            return self.encode_counted(w, version);
        }
        w.varint(self.0.len() as u64)?;
        self.0.iter().try_for_each(|pair| pair.encode_legacy(w))
    }
}

/// Types of a sequence of pairs, from draft 15 each the difference to the one before.
struct TypeCoding {
    version: Version,
    previous: u64,
}

impl TypeCoding {
    fn new(version: Version) -> Self {
        Self { version, previous: 0 }
    }

    fn decode_pair<M: Mode>(&mut self, r: &mut Reader<'_, M>) -> Result<KeyValuePair> {
        let field = r.varint()?;
        let ty = match self.version.kvp_type_delta_encoded() {
            true => self.previous.checked_add(field).ok_or_else(|| Error::ProtocolViolation("key-value pair type overflows".into()))?,
            false => field,
        };
        self.previous = ty;
        KeyValuePair::decode_value(ty, r, self.version)
    }

    /// The pairs are sorted, so a delta never underflows.
    fn encode_pair(&mut self, w: &mut Writer, pair: &KeyValuePair) -> Result<()> {
        let ty = pair.ty();
        w.varint(if self.version.kvp_type_delta_encoded() { ty - self.previous } else { ty })?;
        self.previous = ty;
        pair.encode_value(w, self.version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_18};
    use rstest::rstest;

    const A: VarintKey = VarintKey::new(2);
    const B: BytesKey = BytesKey::new(5);

    fn pairs() -> KeyValuePairs {
        [KeyValuePair::Bytes(B, vec![1, 2]), KeyValuePair::Varint(A, 300), KeyValuePair::Varint(A, 1)].into_iter().collect()
    }

    #[test]
    fn sorted_by_type_in_insertion_order() {
        let types: Vec<_> = pairs().iter().map(|p| p.ty()).collect();
        assert_eq!(types, [2, 2, 5]);
        assert_eq!(pairs().get_varint(A), Some(300));
        assert_eq!(pairs().get_bytes(B), Some(&[1, 2][..]));
    }

    #[rstest]
    #[case(MOQ_VERSION_DRAFT_07)]
    #[case(MOQ_VERSION_DRAFT_14)]
    #[case(MOQ_VERSION_DRAFT_16)]
    #[case(MOQ_VERSION_DRAFT_18)]
    fn counted_round_trip(#[case] version: Version) {
        let mut buf = vec![];
        pairs().to_bytes(&mut version.writer(&mut buf), version).unwrap();
        assert_eq!(KeyValuePairs::from_bytes(&mut version.frame_reader(&buf), version).unwrap(), pairs());
    }

    /// Every pair up to the frame's end, not only the first half.
    #[test]
    fn decode_to_end_reads_every_pair() {
        let version = MOQ_VERSION_DRAFT_18;
        let mut buf = vec![];
        pairs().encode_uncounted(&mut version.writer(&mut buf), version).unwrap();
        assert_eq!(KeyValuePairs::decode_to_end(&mut version.frame_reader(&buf), version).unwrap(), pairs());
    }

    /// Draft 7 to 10 hold every value in bytes, a varint of 64 and more too.
    #[test]
    fn legacy_varint_is_length_prefixed() {
        let mut buf = vec![];
        let max_request_id: KeyValuePairs = [KeyValuePair::Varint(A, 300)].into_iter().collect();
        max_request_id.to_bytes(&mut MOQ_VERSION_DRAFT_07.writer(&mut buf), MOQ_VERSION_DRAFT_07).unwrap();
        // count, type, length, varint 300
        assert_eq!(buf, [1, 2, 2, 0x41, 0x2c]);
    }

    /// An even type's bytes that aren't one varint, e.g. a draft 7 AUTHORIZATION_INFO, are skipped.
    #[test]
    fn legacy_even_type_with_other_bytes_is_skipped() {
        // count, type 2, 3 bytes
        let buf = [1, 2, 3, b'a', b'b', b'c'];
        let decoded = KeyValuePairs::from_bytes(&mut MOQ_VERSION_DRAFT_07.frame_reader(&buf), MOQ_VERSION_DRAFT_07).unwrap();
        assert!(decoded.is_empty());
    }
}
