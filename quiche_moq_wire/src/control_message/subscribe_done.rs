use crate::{ReasonPhrase, Version};
use octets::Octets;

#[derive(Debug)]
pub struct SubscribeDoneMessage {
    request_id: u64,
    status_code: u64,
    stream_count: u64,
    error_reason: ReasonPhrase,
}

impl SubscribeDoneMessage {
    pub fn from_bytes(b: &mut Octets, _version: Version) -> crate::error::Result<Self> {
        //todo!("parse header")
        let request_id = b.get_varint()?;
        let status_code = b.get_varint()?;
        let stream_count = b.get_varint()?;
        let error_reason = ReasonPhrase::from_bytes(b)?;
        Ok(Self{
            request_id,
            status_code,
            stream_count,
            error_reason,
        })
    }
}
