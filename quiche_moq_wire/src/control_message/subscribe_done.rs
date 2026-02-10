use octets::{Octets, OctetsMut};
use crate::{ReasonPhrase, Version, SUBSCRIBE_DONE_CONTROL_MESSAGE_ID};
use crate::control_message::ControlMessage;

#[allow(unused)]
#[derive(Debug)]
pub struct SubscribeDoneMessage {
    request_id: u64,
    status_code: u64,
    stream_count: u64,
    error_reason: ReasonPhrase,
}

impl ControlMessage for SubscribeDoneMessage {
    const MESSAGE_IDS: &'static [u64] = &[SUBSCRIBE_DONE_CONTROL_MESSAGE_ID];

    fn to_body_bytes(&self, b: &mut OctetsMut, _version: Version) -> crate::error::Result<()> {
        b.put_varint(self.request_id)?;
        b.put_varint(self.status_code)?;
        b.put_varint(self.stream_count)?;
        self.error_reason.to_bytes(b)?;
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, _version: Version) -> crate::error::Result<Self> {
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
