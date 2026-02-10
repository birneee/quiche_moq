use octets::{Octets, OctetsMut};
use crate::{RequestId, Version, REQUEST_BLOCKED_CONTROL_MESSAGE_ID};
use crate::control_message::ControlMessage;

#[derive(Debug)]
pub struct RequestBlockedMessage {
    pub maximum_request_id: RequestId,
}

impl ControlMessage for RequestBlockedMessage {
    const MESSAGE_IDS: &'static [u64] = &[REQUEST_BLOCKED_CONTROL_MESSAGE_ID];

    fn to_body_bytes(&self, b: &mut OctetsMut, _version: Version) -> crate::error::Result<()> {
        b.put_varint(self.maximum_request_id)?;
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, _version: Version) -> crate::error::Result<Self> {
        Ok(Self{
            maximum_request_id: b.get_varint()?,
        })
    }
}