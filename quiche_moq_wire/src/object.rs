use crate::bytes::{FromBytes, ToBytes};
use crate::error::Result;
use crate::{SubgroupType, Version, STREAM_HEADER_SUBGROUP_STREAM_TYPE_ID};
use octets::{Octets, OctetsMut};
use crate::key_value_pair::{KeyValuePair, KvpCtx};
use crate::subgroup::SubgroupHeader;

#[derive(Debug, Clone)]
pub struct ObjectHeader {
    id: u64,
    /// `0x4` for draft 7 to draft 10
    subgroup_ty: SubgroupType,
    extension_headers: Vec<KeyValuePair>,
    payload_len: usize,
    status: Option<u64>,
}

impl ObjectHeader {
    pub fn new(id: u64, payload_len: usize, subgroup_ty: SubgroupType) -> Self {
        Self {
            id,
            subgroup_ty,
            extension_headers: vec![],
            payload_len,
            status: None,
        }
    }

    pub fn from_bytes(
        b: &mut Octets,
        version: Version,
        subgroup: &SubgroupHeader,
    ) -> Result<Self> {
        let subgroup_ty = subgroup.ty();
        let id = b.get_varint()?;
        let mut extension_headers = vec![];
        match subgroup_ty {
            STREAM_HEADER_SUBGROUP_STREAM_TYPE_ID => {},
            0xD => {
                let ext_hdr_len = b.get_varint()? as usize;
                let ext_hdr_end = b.off() + ext_hdr_len;
                let mut prev_key = 0u64;
                while b.off() < ext_hdr_end {
                    let kvp = KeyValuePair::from_bytes(b, KvpCtx::new(version).with_previous_key(prev_key))?;
                    prev_key = kvp.ty;
                    extension_headers.push(kvp);
                }
                assert_eq!(b.off(), ext_hdr_end);
            }
            _ => unimplemented!()
        }
        let payload_len = b.get_varint()? as usize;
        let status = if payload_len == 0 {
            Some(b.get_varint()?)
        } else {
            None
        };

        Ok(Self {
            id,
            subgroup_ty,
            extension_headers,
            payload_len,
            status,
        })
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn payload_len(&self) -> usize {
        self.payload_len
    }

    pub fn extension_headers_len(&self) -> usize {
        self.extension_headers.len()
    }

    pub fn status(&self) -> Option<u64> {
        self.status
    }
}

impl ToBytes for ObjectHeader {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> Result<()> {
        b.put_varint(self.id)?;
        match self.subgroup_ty {
            STREAM_HEADER_SUBGROUP_STREAM_TYPE_ID => {},
            0xD => {
                //todo maybe use SubgroupHeader::extensions_present
                b.put_varint(self.extension_headers.len() as u64)?;
                let mut prev_key = 0u64;
                for header in &self.extension_headers {
                    header.to_bytes(b, KvpCtx::new(version).with_previous_key(prev_key))?;
                    prev_key = header.ty;
                }
            }
            _ => unimplemented!(),
        }
        b.put_varint(self.payload_len as u64)?;
        if self.payload_len == 0 {
            b.put_varint(self.status.unwrap())?;
        }
        Ok(())
    }
}
