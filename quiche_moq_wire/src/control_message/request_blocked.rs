use crate::bytes::FromBytes;
use crate::{RequestId, Version, REQUEST_BLOCKED_CONTROL_MESSAGE_ID};
use octets::Octets;
use crate::control_message::header::ControlMessageHeader;

#[derive(Debug)]
pub struct RequestBlockedMessage {
    pub maximum_request_id: RequestId,
}

impl FromBytes for RequestBlockedMessage {
    fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let header = ControlMessageHeader::from_bytes(b, version)?;
        assert_eq!(header.ty(), REQUEST_BLOCKED_CONTROL_MESSAGE_ID);
        assert!(b.cap() >= header.len());
        Ok(Self{
            maximum_request_id: b.get_varint()?,
        })
    }
}