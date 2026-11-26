use crate::bytes::{FromBytes, ToBytes};
use crate::Version;
use crate::{Mode, Reader, Writer};

/// Ordered by group, then object (draft-ietf-moq-transport-16 §1.4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Location {
    pub group: u64,
    pub object: u64,
}

impl Location {
    /// The Location right after it in its Group.
    pub fn next(self) -> Self {
        Self { group: self.group, object: self.object + 1 }
    }

    /// The Location right before it, the end of the previous Group if it starts one: its last Object
    /// ID every draft can encode. `None` before the first.
    pub fn prev(self) -> Option<Self> {
        match self {
            Self { group: 0, object: 0 } => None,
            Self { group, object: 0 } => Some(Self { group: group - 1, object: moq_wire_primitives::VarIntForm::QUIC_MAX }),
            Self { group, object } => Some(Self { group, object: object - 1 }),
        }
    }
}

impl FromBytes for Location {
    fn from_bytes<M: Mode>(b: &mut Reader<'_, M>, _version: Version) -> crate::error::Result<Self> {
        Ok(Self {
            group: b.varint()?,
            object: b.varint()?,
        })
    }
}

impl ToBytes for Location {
    fn to_bytes(&self, b: &mut Writer, _version: Version) -> crate::error::Result<()> {
        b.varint(self.group)?;
        b.varint(self.object)?;
        Ok(())
    }
}

/// A subgroup's identity: (group_id, subgroup_id). Every subgroup is
/// delivered on exactly one QUIC stream (see `SubgroupHeader`), so this is
/// the natural, transport-free way to name "the stream a given object
/// arrived on" — unlike `Location` (group + object only, per
/// draft-ietf-moq-transport-16 §1.4.1), which can't disambiguate between two
/// different subgroups in the same group that happen to number their
/// objects the same way. `subgroup_id` is always resolved to a concrete
/// value (`SubgroupHeader::subgroup_id()` is `Option<u64>` — `None` means
/// "the first Object ID" for some wire encodings), since this type is only ever
/// used as an opaque identity, never re-serialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubgroupLocation {
    pub group_id: u64,
    pub subgroup_id: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use crate::{MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_17};

    /// A Group's start is preceded by the end of the previous Group.
    #[rstest::rstest]
    #[case(Location { group: 0, object: 0 }, None)]
    #[case(Location { group: 2, object: 3 }, Some(Location { group: 2, object: 2 }))]
    #[case(Location { group: 2, object: 0 }, Some(Location { group: 1, object: moq_wire_primitives::VarIntForm::QUIC_MAX }))]
    fn prev_is_the_location_before(#[case] loc: Location, #[case] expected: Option<Location>) {
        assert_eq!(loc.prev(), expected);
    }

    /// The QUIC varint ends below 2^62, draft 17's reaches `u64::MAX`.
    #[test]
    fn group_beyond_quic_varint_is_an_error() {
        let location = Location { group: 1 << 62, object: 0 };
        let mut buf = vec![];
        let encoded = location.to_bytes(&mut MOQ_VERSION_DRAFT_16.writer(&mut buf), MOQ_VERSION_DRAFT_16);
        assert!(matches!(encoded, Err(crate::Error::Unencodable(_))), "{encoded:?}");
        let mut buf = vec![];
        location.to_bytes(&mut MOQ_VERSION_DRAFT_17.writer(&mut buf), MOQ_VERSION_DRAFT_17).unwrap();
        assert_eq!(Location::from_bytes(&mut MOQ_VERSION_DRAFT_17.reader(&buf), MOQ_VERSION_DRAFT_17).unwrap(), location);
    }
}
