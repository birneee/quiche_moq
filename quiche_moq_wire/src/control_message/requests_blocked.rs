use octets::{Octets, OctetsMut};
use crate::{RequestId, Version, REQUESTS_BLOCKED_MESSAGE_ID};
use crate::control_message::ControlMessage;

#[derive(Debug)]
/// Called REQUEST_BLOCKED before draft-13
/// Called REQUESTS_BLOCKED since draft-14
pub struct RequestsBlockedMessage {
    pub maximum_request_id: RequestId,
}

impl ControlMessage for RequestsBlockedMessage {
    const MESSAGE_IDS: &'static [u64] = &[REQUESTS_BLOCKED_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "requests_blocked" }

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