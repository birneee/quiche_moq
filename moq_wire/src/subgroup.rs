use crate::prelude::*;
use crate::bytes::{FromBytes, ToBytes};
use crate::error::{Error, Result};
use crate::{Mode, Reader, TrackAlias, Version, Writer, DEFAULT_PUBLISHER_PRIORITY};

/// How a subgroup header gives the Subgroup ID.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SubgroupIdMode {
    /// absent, 0
    Zero,
    /// absent, the first object's ID
    FirstObjectId,
    /// a field of the header
    Explicit,
}

/// What the type of a subgroup stream says (draft-ietf-moq-transport-19 §11.4.2).
/// - draft 7 to 10: one type, an explicit Subgroup ID, properties from draft 8
/// - draft 11 to 13: `0x8 | properties | mode << 1`
/// - draft 14: `0x10`, END_OF_GROUP `0x8`; draft 15: DEFAULT_PRIORITY `0x20`; draft 18: FIRST_OBJECT `0x40`
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SubgroupType {
    /// Objects carry properties, Extension Headers before draft 18.
    pub extensions: bool,
    pub subgroup_id: SubgroupIdMode,
    /// The subgroup holds the group's largest object, draft 14 on.
    pub end_of_group: bool,
    /// No Publisher Priority field, draft 15 on.
    pub default_priority: bool,
    /// The subgroup starts with the group's first object, draft 18 on.
    pub first_object: bool,
}

/// The type of draft 7 to 10.
const STREAM_HEADER_SUBGROUP: u64 = 0x4;

impl SubgroupType {
    /// With properties, which draft 7 lacks, and an explicit Subgroup ID.
    pub fn new(version: Version) -> Self {
        Self { extensions: version.draft() >= 8, subgroup_id: SubgroupIdMode::Explicit, end_of_group: false, default_priority: false, first_object: false }
    }

    fn decode(ty: u64, version: Version) -> Result<Self> {
        let unknown = || Error::ProtocolViolation(format!("unknown data stream type 0x{ty:x}"));
        let draft = version.draft();
        if draft <= 10 {
            return match ty {
                STREAM_HEADER_SUBGROUP => Ok(Self::new(version)),
                _ => Err(unknown()),
            };
        }
        let marker = if draft <= 13 { 0x08 } else { 0x10 };
        let flags = match draft {
            11..=13 => 0,
            14 => 0x08,
            15..=17 => 0x08 | 0x20,
            _ => 0x08 | 0x20 | 0x40,
        };
        if ty & !(0x7 | marker | flags) != 0 || ty & marker == 0 {
            return Err(unknown());
        }
        let subgroup_id = match (ty >> 1) & 0x3 {
            0 => SubgroupIdMode::Zero,
            1 => SubgroupIdMode::FirstObjectId,
            2 => SubgroupIdMode::Explicit,
            _ => return Err(unknown()),
        };
        let flag = |bit: u64| flags & bit != 0 && ty & bit != 0;
        Ok(Self { extensions: ty & 0x1 != 0, subgroup_id, end_of_group: flag(0x08), default_priority: flag(0x20), first_object: flag(0x40) })
    }

    /// Properties `version` lacks are an error, except END_OF_GROUP and FIRST_OBJECT, which are hints.
    fn encode(&self, version: Version) -> Result<u64> {
        let draft = version.draft();
        if draft <= 10 {
            return match (self.extensions == (draft >= 8), self.subgroup_id, self.default_priority) {
                (true, SubgroupIdMode::Explicit, false) => Ok(STREAM_HEADER_SUBGROUP),
                _ => Err(Error::Unencodable("a subgroup type drafts 7 to 10 don't have")),
            };
        }
        if self.default_priority && draft < 15 {
            return Err(Error::Unencodable("DEFAULT_PRIORITY before draft 15"));
        }
        let mode = match self.subgroup_id {
            SubgroupIdMode::Zero => 0,
            SubgroupIdMode::FirstObjectId => 1,
            SubgroupIdMode::Explicit => 2,
        };
        let marker = if draft <= 13 { 0x08 } else { 0x10 };
        let flag = |set: bool, bit: u64, since: u8| if set && draft >= since { bit } else { 0 };
        Ok(marker | u64::from(self.extensions) | mode << 1
            | flag(self.end_of_group, 0x08, 14)
            | flag(self.default_priority, 0x20, 15)
            | flag(self.first_object, 0x40, 18))
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct SubgroupHeader {
    ty: SubgroupType,
    track_alias: TrackAlias,
    group_id: u64,
    /// `None` until the first object for [`SubgroupIdMode::FirstObjectId`]
    subgroup_id: Option<u64>,
    publisher_priority: u8,
}

impl SubgroupHeader {
    pub fn new(track_alias: TrackAlias, group_id: u64, subgroup_id: u64, version: Version) -> Self {
        Self { ty: SubgroupType::new(version), track_alias, group_id, subgroup_id: Some(subgroup_id), publisher_priority: 0 }
    }

    pub fn with_first_object(mut self, first_object: bool) -> Self {
        self.ty.first_object = first_object;
        self
    }

    pub fn with_publisher_priority(mut self, publisher_priority: u8) -> Self {
        self.publisher_priority = publisher_priority;
        self
    }

    pub fn with_end_of_group(mut self, end_of_group: bool) -> Self {
        self.ty.end_of_group = end_of_group;
        self
    }

    pub fn ty(&self) -> SubgroupType {
        self.ty
    }

    pub fn first_object(&self) -> bool {
        self.ty.first_object
    }

    pub fn end_of_group(&self) -> bool {
        self.ty.end_of_group
    }

    pub fn track_alias(&self) -> TrackAlias {
        self.track_alias
    }

    pub fn group_id(&self) -> u64 {
        self.group_id
    }

    pub fn subgroup_id(&self) -> Option<u64> {
        self.subgroup_id
    }

    pub fn publisher_priority(&self) -> u8 {
        self.publisher_priority
    }
}

impl FromBytes for SubgroupHeader {
    fn from_bytes<M: Mode>(b: &mut Reader<'_, M>, version: Version) -> Result<Self> {
        let ty = SubgroupType::decode(b.varint()?, version)?;
        let track_alias = TrackAlias(b.varint()?);
        let group_id = b.varint()?;
        let subgroup_id = match ty.subgroup_id {
            SubgroupIdMode::Explicit => Some(b.varint()?),
            SubgroupIdMode::Zero => Some(0),
            SubgroupIdMode::FirstObjectId => None,
        };
        // TODO take the priority the publisher announced for the track
        let publisher_priority = if ty.default_priority { DEFAULT_PUBLISHER_PRIORITY } else { b.u8()? };
        Ok(Self { ty, track_alias, group_id, subgroup_id, publisher_priority })
    }
}

impl ToBytes for SubgroupHeader {
    fn to_bytes(&self, b: &mut Writer, version: Version) -> Result<()> {
        b.varint(self.ty.encode(version)?)?;
        b.varint(self.track_alias.0)?;
        b.varint(self.group_id)?;
        if self.ty.subgroup_id == SubgroupIdMode::Explicit {
            let id = self.subgroup_id.ok_or(Error::Unencodable("an explicit Subgroup ID that is unknown"))?;
            b.varint(id)?;
        }
        if !self.ty.default_priority {
            b.u8(self.publisher_priority);
        }
        Ok(())
    }
}


#[cfg(test)]
mod test {
    use super::*;
    use crate::bytes::{from_slice, to_vec};
    use crate::{MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_13, MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_18};
    use rstest::rstest;

    #[rstest]
    #[case(MOQ_VERSION_DRAFT_10)]
    #[case(MOQ_VERSION_DRAFT_11)]
    #[case(MOQ_VERSION_DRAFT_14)]
    #[case(MOQ_VERSION_DRAFT_18)]
    fn header_round_trips(#[case] version: Version) {
        let subgroup = SubgroupHeader::new(TrackAlias(1), 2, 3, version);
        assert_eq!(from_slice::<SubgroupHeader>(&to_vec(&subgroup, version).unwrap(), version).unwrap(), subgroup);
    }

    /// Every type a draft defines decodes and encodes to itself, all others are rejected.
    #[rstest]
    #[case(MOQ_VERSION_DRAFT_10, &[0x4])]
    #[case(MOQ_VERSION_DRAFT_13, &[0x8, 0x9, 0xA, 0xB, 0xC, 0xD])]
    #[case(MOQ_VERSION_DRAFT_14, &[0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D])]
    fn types_of_a_draft(#[case] version: Version, #[case] known: &[u64]) {
        for ty in 0..0x80 {
            match SubgroupType::decode(ty, version) {
                Ok(decoded) => {
                    assert!(known.contains(&ty), "0x{ty:x}");
                    assert_eq!(decoded.encode(version).unwrap(), ty);
                }
                Err(_) => assert!(!known.contains(&ty), "0x{ty:x}"),
            }
        }
    }

    #[test]
    fn later_drafts_add_flags() {
        assert!(SubgroupType::decode(0x35, MOQ_VERSION_DRAFT_14).is_err());
        assert!(SubgroupType::decode(0x35, MOQ_VERSION_DRAFT_15).unwrap().default_priority);
        assert!(SubgroupType::decode(0x55, MOQ_VERSION_DRAFT_16).is_err());
        assert!(SubgroupType::decode(0x55, MOQ_VERSION_DRAFT_18).unwrap().first_object);
    }

    /// Header type 0x14 has no EXTENSIONS bit: objects carry no extension length.
    #[test]
    fn object_without_extensions_field() {
        let version = MOQ_VERSION_DRAFT_16;
        // type, alias, group, subgroup, priority | object id, payload length, payload
        let bytes = [0x14, 1, 2, 3, 0, 5, 1, b'x'];
        let mut o = version.reader(&bytes);
        let subgroup = SubgroupHeader::from_bytes(&mut o, version).unwrap();
        let object = crate::object::ObjectHeader::decode(&mut o, version, &subgroup, None).unwrap();
        assert_eq!((object.id(), object.payload_len()), (5, 1));
    }

    #[test]
    fn property_beyond_its_length_is_protocol_violation() {
        let version = MOQ_VERSION_DRAFT_16;
        // object id, extensions length 2, odd key 1 with a 5 byte value, payload length, payload
        let bytes = [0x15, 1, 2, 3, 0, 0, 2, 1, 5, 1, b'x'];
        let mut o = version.reader(&bytes);
        let subgroup = SubgroupHeader::from_bytes(&mut o, version).unwrap();
        let object = crate::object::ObjectHeader::decode(&mut o, version, &subgroup, None);
        assert!(matches!(object, Err(Error::ProtocolViolation(_))), "{object:?}");
    }

    /// DEFAULT_PRIORITY (0x20): no priority byte, the next byte is the object's.
    #[test]
    fn default_priority_header_has_no_priority_byte() {
        let version = MOQ_VERSION_DRAFT_16;
        // type 0x35 (0x15 | DEFAULT_PRIORITY), alias 1, group 2, subgroup 3, next byte
        let bytes = [0x35, 1, 2, 3, 0xAA];
        let mut o = version.reader(&bytes);
        let subgroup = SubgroupHeader::from_bytes(&mut o, version).unwrap();
        assert_eq!(o.consumed(), 4);
        assert_eq!(subgroup.subgroup_id(), Some(3));
        assert_eq!(to_vec(&subgroup, version).unwrap(), bytes[..4]);
    }
}
