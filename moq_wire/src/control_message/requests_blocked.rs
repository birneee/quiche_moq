use crate::{Frame, Reader, Writer};
use crate::{RequestId, Version, REQUESTS_BLOCKED_MESSAGE_ID};
use crate::control_message::{until_draft, ControlMessage};

#[derive(Debug)]
/// Called REQUEST_BLOCKED before draft-13
/// Called REQUESTS_BLOCKED since draft-14
/// Removed in draft-ietf-moq-transport-17 (#1471) along with MAX_REQUEST_ID.
pub struct RequestsBlockedMessage {
    pub maximum_request_id: RequestId,
}

impl ControlMessage for RequestsBlockedMessage {
    const MESSAGE_IDS: &'static [u64] = &[REQUESTS_BLOCKED_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "requests_blocked" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> crate::error::Result<()> {
        until_draft(16, "REQUESTS_BLOCKED", version, false)?;
        b.varint(self.maximum_request_id.0)?;
        Ok(())
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> crate::error::Result<Self> {
        until_draft(16, "REQUESTS_BLOCKED", version, true)?;
        Ok(Self{
            maximum_request_id: RequestId(b.varint()?),
        })
    }
}