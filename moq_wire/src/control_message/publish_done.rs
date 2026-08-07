use octets::{Octets, OctetsMut};
use crate::vi64;
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

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        vi64::write_varint(b, self.request_id, version)?;
        vi64::write_varint(b, self.status_code, version)?;
        vi64::write_varint(b, self.stream_count, version)?;
        self.error_reason.to_bytes(b, version)?;
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let request_id = vi64::read_varint(b, version)?;
        let status_code = vi64::read_varint(b, version)?;
        let stream_count = vi64::read_varint(b, version)?;
        let error_reason = ReasonPhrase::from_bytes(b, version)?;
        Ok(Self{
            request_id,
            status_code,
            stream_count,
            error_reason,
        })
    }
}
