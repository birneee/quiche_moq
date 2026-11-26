use crate::{Frame, Reader, Writer};
use crate::{RequestId, Version, MAX_REQUEST_ID_MESSAGE_ID};
use crate::control_message::{until_draft, ControlMessage};

/// Raises the peer's Request ID limit (MAX_SUBSCRIBE_ID before draft 11).
/// Removed in draft-ietf-moq-transport-17 (#1471).
#[derive(Debug)]
pub struct MaxRequestIdMessage {
    /// The new maximum Request ID plus 1.
    pub max_request_id: RequestId,
}

impl ControlMessage for MaxRequestIdMessage {
    const MESSAGE_IDS: &'static [u64] = &[MAX_REQUEST_ID_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "max_request_id" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> crate::error::Result<()> {
        until_draft(16, "MAX_REQUEST_ID", version, false)?;
        b.varint(self.max_request_id.0)?;
        Ok(())
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> crate::error::Result<Self> {
        until_draft(16, "MAX_REQUEST_ID", version, true)?;
        Ok(Self { max_request_id: RequestId(b.varint()?) })
    }
}
