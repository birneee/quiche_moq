use octets::{Octets, OctetsMut};
use crate::{ReasonPhrase, Version, PUBLISH_DONE_MESSAGE_ID};
use crate::control_message::ControlMessage;

#[allow(unused)]
#[derive(Debug)]
/// Called SUBSCRIBE_DONE before draft-13
/// Called PUBLISH_DONE since draft-14
pub struct PublishDoneMessage {
    request_id: u64,
    status_code: u64,
    stream_count: u64,
    error_reason: ReasonPhrase,
}

impl PublishDoneMessage {
    pub fn request_id(&self) -> u64 { self.request_id }
    pub fn stream_count(&self) -> u64 { self.stream_count }

    pub fn new(request_id: u64, status_code: u64) -> Self {
        Self {
            request_id,
            status_code,
            stream_count: 0,
            error_reason: ReasonPhrase(String::new()),
        }
    }
}

impl ControlMessage for PublishDoneMessage {
    const MESSAGE_IDS: &'static [u64] = &[PUBLISH_DONE_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "publish_done" }

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
