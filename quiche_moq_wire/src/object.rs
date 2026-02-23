use crate::bytes::{FromBytes, ToBytes};
use crate::error::Result;
use crate::{SubgroupType, Version, STREAM_HEADER_SUBGROUP_STREAM_TYPE_ID};
use octets::{Octets, OctetsMut};
use crate::key_value_pair::{KeyValuePair, KvpCtx};
use crate::key_value_pairs::KeyValuePairs;
use crate::subgroup::SubgroupHeader;

#[derive(Debug, Clone)]
pub struct ObjectHeader {
    id: u64,
    /// `0x4` for draft 7 to draft 10
    subgroup_ty: SubgroupType,
    extension_headers: KeyValuePairs,
    payload_len: usize,
    status: Option<u64>,
}

impl ObjectHeader {
    pub fn new(id: u64, payload_len: usize, subgroup_ty: SubgroupType, extension_headers: KeyValuePairs) -> Self {
        Self {
            id,
            subgroup_ty,
            extension_headers,
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
        let mut extension_headers = KeyValuePairs::new();
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

    pub fn extension_headers(&self) -> &KeyValuePairs {
        &self.extension_headers
    }

    pub fn status(&self) -> Option<u64> {
        self.status
    }

    /// Returns extension headers formatted as `[* MOQTExtensionHeader]` per the qlog draft.
    #[cfg(feature = "qlog")]
    pub fn extension_headers_to_qlog(&self) -> Vec<serde_json::Value> {
        use crate::KeyValuePairValue;
        self.extension_headers.0.iter().map(|kvp| {
            match kvp.value() {
                KeyValuePairValue::Varint(v) => serde_json::json!({
                    "header_type": kvp.ty(),
                    "header_value": v,
                }),
                KeyValuePairValue::Bytes(b) => {
                    let hex: String = b.iter().map(|byte| format!("{byte:02x}")).collect();
                    serde_json::json!({
                        "header_type": kvp.ty(),
                        "header_length": b.len() as u64,
                        "payload": { "data": hex },
                    })
                }
            }
        }).collect()
    }
}

impl ToBytes for ObjectHeader {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> Result<()> {
        b.put_varint(self.id)?;
        match self.subgroup_ty {
            STREAM_HEADER_SUBGROUP_STREAM_TYPE_ID => {},
            0xD => {
                let kvps = self.extension_headers.clone();
                b.put_varint(kvps.byte_length(version) as u64)?;
                kvps.to_bytes(b, version)?;
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
