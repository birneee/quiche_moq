use crate::bytes::{FromBytes, ToBytes};
use crate::vi64;
use crate::{TrackAlias, Version, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_08, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_13, MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_18, STREAM_HEADER_SUBGROUP_STREAM_TYPE_ID, SUBGROUP_UNI_STREAM_TYPE_IDS};
use octets::{Octets, OctetsMut};

/// draft 14 moves the type marker from bit 3 (0x8..0xD) to bit 4 (0x10..0x1D),
/// freeing bit 3 for a real END_OF_GROUP flag — confirmed against each
/// draft's own SUBGROUP_HEADER figure (draft-13: `0x8..0xD`, draft-14:
/// `0x10..0x1D`). draft-15 adds DEFAULT_PRIORITY (0x20); draft-18 adds
/// FIRST_OBJECT (0x40).
fn is_known_type(ty: u64, version: Version) -> bool {
    match version {
        MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => SUBGROUP_UNI_STREAM_TYPE_IDS.contains(&ty),
        MOQ_VERSION_DRAFT_14..=MOQ_VERSION_DRAFT_18 => {
            ty & 0x10 != 0
                && ty & !0x7Fu64 == 0
                && ty & 0x6 != 0x6 // SUBGROUP_ID_MODE 0b11 is reserved/invalid
                && (version == MOQ_VERSION_DRAFT_18 || ty & 0x40 == 0)
        }
        _ => unimplemented!()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct SubgroupHeader {
    /// Canonical type value, without the FIRST_OBJECT bit (0x40) folded in —
    /// see [`Self::first_object`].
    ty: u64,
    track_alias: TrackAlias,
    group_id: u64,
    subgroup_id: Option<u64>,
    publisher_priority: u8,
    /// FIRST_OBJECT bit (0x40), added in draft-ietf-moq-transport-18 (#1618).
    /// Ignored (always false) before draft-18.
    first_object: bool,
}

impl SubgroupHeader {
    pub fn new(track_alias: TrackAlias, group_id: u64, subgroup_id: u64, version: Version) -> Self {
        let ty = match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => STREAM_HEADER_SUBGROUP_STREAM_TYPE_ID,
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => 0xD, //todo support other types
            MOQ_VERSION_DRAFT_14..=MOQ_VERSION_DRAFT_18 => 0x1D, //todo support other types
            _ => unimplemented!()
        };
        Self {
            ty,
            track_alias,
            group_id,
            subgroup_id: Some(subgroup_id),
            publisher_priority: 0,
            first_object: false,
        }
    }

    pub fn with_first_object(mut self, first_object: bool) -> Self {
        self.first_object = first_object;
        self
    }

    pub fn first_object(&self) -> bool {
        self.first_object
    }

    pub fn ty(&self) -> u64 {
        self.ty
    }

    /// EXTENSIONS bit (0x01 of the low nibble) — present regardless of
    /// END_OF_GROUP (bit 3), so both the draft 11-13 values (always bit 3 = 1)
    /// and draft 14+'s bit-3-is-0 counterparts (0x1, 0x3, 0x5) need covering.
    pub fn extensions_present(ty: u64) -> bool {
        [0x1, 0x3, 0x5, 0x9, 0xB, 0xD].contains(&(ty & 0xF))
    }

    /// SUBGROUP_ID_MODE (bits 1-2) == 0b10, "Subgroup ID field is present".
    pub fn subgroup_id_present(ty: u64) -> bool {
        [0x4, 0x5, 0xC, 0xD].contains(&(ty & 0xF))
    }

    /// SUBGROUP_ID_MODE (bits 1-2) == 0b00, "absent, Subgroup ID is 0".
    pub fn subgroup_id_implicit_zero(ty: u64) -> bool {
        [0x0, 0x1, 0x8, 0x9].contains(&(ty & 0xF))
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
    fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let wire_ty = vi64::read_varint(b, version)?;
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
                assert_eq!(wire_ty, STREAM_HEADER_SUBGROUP_STREAM_TYPE_ID)
            }
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_18 => {
                assert!(is_known_type(wire_ty, version), "unsupported uni stream type: 0x{wire_ty:x}");
            }
            _ => unimplemented!()
        }
        let first_object = version == MOQ_VERSION_DRAFT_18 && wire_ty & 0x40 != 0;
        let ty = wire_ty & !0x40u64;
        let _subscribe_id = match version {
            MOQ_VERSION_DRAFT_07 => Some(vi64::read_varint(b, version)?), // todo not sure, this is not in the spec, but cloudflare uses it, https://github.com/englishm/moq-rs/blob/ebc843de8504e37d36c3134a1181513ebdf7a34a/moq-transport/src/data/subgroup.rs
            MOQ_VERSION_DRAFT_08..=MOQ_VERSION_DRAFT_18 => None,
            _ => unimplemented!()
        };
        let track_alias = vi64::read_varint(b, version)?;
        let group_id = vi64::read_varint(b, version)?;
        let subgroup_id = match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => Some(vi64::read_varint(b, version)?),
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_18 => {
                if Self::subgroup_id_present(ty) {
                    Some(vi64::read_varint(b, version)?)
                } else if Self::subgroup_id_implicit_zero(ty) {
                    Some(0)
                } else {
                    None
                }
            },
            _ => unimplemented!()
        };
        let publisher_priority = b.get_u8()?;
        Ok(Self {
            ty,
            track_alias,
            group_id,
            subgroup_id,
            publisher_priority,
            first_object,
        })
    }
}

impl  ToBytes for SubgroupHeader {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
                assert_eq!(self.ty, STREAM_HEADER_SUBGROUP_STREAM_TYPE_ID)
            }
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_18 => {
                assert!(is_known_type(self.ty, version));
            }
            _ => unimplemented!()
        }
        let wire_ty = if version == MOQ_VERSION_DRAFT_18 && self.first_object {
            self.ty | 0x40
        } else {
            self.ty
        };
        vi64::write_varint(b, wire_ty, version)?;
        match version {
            MOQ_VERSION_DRAFT_07 => { vi64::write_varint(b, 0, version)?; }, // todo not sure, this is not in the spec, but cloudflare uses it, https://github.com/englishm/moq-rs/blob/ebc843de8504e37d36c3134a1181513ebdf7a34a/moq-transport/src/data/subgroup.rs
            MOQ_VERSION_DRAFT_08..=MOQ_VERSION_DRAFT_18 => {},
            _ => unimplemented!()
        }
        vi64::write_varint(b, self.track_alias, version)?;
        vi64::write_varint(b, self.group_id, version)?;
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => { vi64::write_varint(b, self.subgroup_id.unwrap(), version)?; },
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_18 => {
                if Self::subgroup_id_present(self.ty) {
                    vi64::write_varint(b, self.subgroup_id.unwrap(), version)?;
                }
            },
            _ => unimplemented!()
        }
        b.put_u8(self.publisher_priority)?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use crate::bytes::{FromBytes, ToBytes};
    use crate::MOQ_VERSION_DRAFT_11;
    use octets::{Octets, OctetsMut};
    use crate::subgroup::SubgroupHeader;

    #[test]
    fn test_encode_decode() {
        let subgroup = SubgroupHeader::new(1, 2, 3, MOQ_VERSION_DRAFT_11);
        let mut b = [0; 100];
        let mut o = OctetsMut::with_slice(&mut b);
        subgroup.to_bytes(&mut o, MOQ_VERSION_DRAFT_11).unwrap();
        let len = o.off();
        let subgroup2 = SubgroupHeader::from_bytes(&mut Octets::with_slice(&b[..len]), MOQ_VERSION_DRAFT_11).unwrap();
        assert_eq!(subgroup, subgroup2);
    }

}