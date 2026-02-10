use octets::{Octets, OctetsMut};
use crate::{ReasonPhrase, RequestId, TrackAlias, Version, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_13, REQUEST_ERROR_MESSAGE_ID};
use crate::control_message::ControlMessage;

#[allow(unused)]
#[derive(Debug)]
/// Called SUBSCRIBE_ERROR before draft-13
/// Called REQUEST_ERROR since draft-14
pub struct RequestErrorMessage {
    /// formerly known as subscribe ID
    request_id: RequestId,
    error_code: u64,
    error_reason: ReasonPhrase,
    /// only present from draft 07 to draft 11
    track_alias: Option<TrackAlias>
}

impl RequestErrorMessage {
    pub fn request_id(&self) -> RequestId {
        self.request_id
    }
    pub fn error_code(&self) -> u64 { self. error_code }
}

impl ControlMessage for RequestErrorMessage {
    const MESSAGE_IDS: &'static [u64] = &[REQUEST_ERROR_MESSAGE_ID];

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        b.put_varint(self.request_id)?;
        b.put_varint(self.error_code)?;
        self.error_reason.to_bytes(b)?;
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11 => {
                b.put_varint(self.track_alias.unwrap())?;
            },
            MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_13 => {},
            _ => unimplemented!()
        };
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let request_id = b.get_varint()?;
        let error_code = b.get_varint()?;
        let error_reason = ReasonPhrase::from_bytes(b)?;
        let track_alias = match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11 => Some(b.get_varint()?),
            MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_13 => None,
            _ => unimplemented!()
        };
        Ok(Self {
            request_id,
            error_code,
            error_reason,
            track_alias,
        })
    }
}
