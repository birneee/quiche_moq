use crate::bytes::{FromBytes, ToBytes};
use crate::{TrackAlias, Version, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_08, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_13, STREAM_HEADER_SUBGROUP_STREAM_TYPE_ID, SUBGROUP_UNI_STREAM_TYPE_IDS};
use octets::{Octets, OctetsMut};

#[derive(Debug, Eq, PartialEq)]
pub struct SubgroupHeader {
    ty: u64,
    track_alias: TrackAlias,
    group_id: u64,
    subgroup_id: Option<u64>,
    publisher_priority: u8,
}

impl SubgroupHeader {
    pub fn new(track_alias: TrackAlias, group_id: u64, subgroup_id: u64, version: Version) -> Self {
        let ty = match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => STREAM_HEADER_SUBGROUP_STREAM_TYPE_ID,
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => 0xD, //todo support other types
            _ => unimplemented!()
        };
        Self {
            ty,
            track_alias,
            group_id,
            subgroup_id: Some(subgroup_id),
            publisher_priority: 0,
        }
    }

    pub fn ty(&self) -> u64 {
        self.ty
    }

    pub fn extensions_present(ty: u64) -> bool {
        [0x9, 0xB, 0xD].contains(&ty)
    }

    pub fn subgroup_id_present(ty: u64) -> bool {
        [0xC, 0xD].contains(&ty)
    }

    pub fn subgroup_id_implicit_zero(ty: u64) -> bool {
        [0x8, 0x9].contains(&ty)
    }
    
    pub fn track_alias(&self) -> TrackAlias {
        self.track_alias
    }
}

impl FromBytes for SubgroupHeader {
    fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let ty = b.get_varint()?;
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
                assert_eq!(ty, STREAM_HEADER_SUBGROUP_STREAM_TYPE_ID)
            }
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => {
                assert!(SUBGROUP_UNI_STREAM_TYPE_IDS.contains(&ty));
            }
            _ => unimplemented!()
        }
        let _subscribe_id = match version {
            MOQ_VERSION_DRAFT_07 => Some(b.get_varint()?), // todo not sure, this is not in the spec, but cloudflare uses it, https://github.com/englishm/moq-rs/blob/ebc843de8504e37d36c3134a1181513ebdf7a34a/moq-transport/src/data/subgroup.rs
            MOQ_VERSION_DRAFT_08..=MOQ_VERSION_DRAFT_13 => None,
            _ => unimplemented!()
        };
        let track_alias = b.get_varint()?;
        let group_id = b.get_varint()?;
        let subgroup_id = match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => Some(b.get_varint()?),
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => {
                if Self::subgroup_id_present(ty) {
                    Some(b.get_varint()?)
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
        })
    }
}

impl  ToBytes for SubgroupHeader {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
                assert_eq!(self.ty, STREAM_HEADER_SUBGROUP_STREAM_TYPE_ID)
            }
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => {
                assert!(SUBGROUP_UNI_STREAM_TYPE_IDS.contains(&self.ty));
            }
            _ => unimplemented!()
        }
        b.put_varint(self.ty)?;
        match version {
            MOQ_VERSION_DRAFT_07 => { b.put_varint(0)?; }, // todo not sure, this is not in the spec, but cloudflare uses it, https://github.com/englishm/moq-rs/blob/ebc843de8504e37d36c3134a1181513ebdf7a34a/moq-transport/src/data/subgroup.rs
            MOQ_VERSION_DRAFT_08..=MOQ_VERSION_DRAFT_13 => {},
            _ => unimplemented!()
        }
        b.put_varint(self.track_alias)?;
        b.put_varint(self.group_id)?;
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => { b.put_varint(self.subgroup_id.unwrap())?; },
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => {
                if Self::subgroup_id_present(self.ty) {
                    b.put_varint(self.subgroup_id.unwrap())?;
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