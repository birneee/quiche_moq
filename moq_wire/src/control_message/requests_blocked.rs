use octets::{Octets, OctetsMut};
use crate::error::Error;
use crate::vi64;
use crate::{RequestId, Version, MOQ_VERSION_DRAFT_17, MOQ_VERSION_DRAFT_18, REQUESTS_BLOCKED_MESSAGE_ID};
use crate::control_message::ControlMessage;

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

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        // Matched by exact version, not `>=`: MOQ_VERSION_LITE_01_BY_KIXELATED is
        // numerically larger than the draft constants despite not being a newer draft.
        if matches!(version, MOQ_VERSION_DRAFT_17 | MOQ_VERSION_DRAFT_18) {
            return Err(Error::ProtocolViolation(format!("REQUESTS_BLOCKED was removed in draft-ietf-moq-transport-17 (version 0x{version:x})")));
        }
        vi64::write_varint(b, self.maximum_request_id, version)?;
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        if matches!(version, MOQ_VERSION_DRAFT_17 | MOQ_VERSION_DRAFT_18) {
            return Err(Error::ProtocolViolation(format!("REQUESTS_BLOCKED was removed in draft-ietf-moq-transport-17 (version 0x{version:x})")));
        }
        Ok(Self{
            maximum_request_id: vi64::read_varint(b, version)?,
        })
    }
}