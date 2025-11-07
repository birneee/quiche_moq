use crate::bytes::FromBytes;
use crate::{ReasonPhrase, RequestId, TrackAlias, Version, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_13, SUBSCRIBE_ERROR_CONTROL_MESSAGE_ID};
use octets::Octets;
use crate::control_message::header::ControlMessageHeader;

#[derive(Debug)]
pub struct SubscribeErrorMessage {
    /// formerly known as subscribe ID
    request_id: RequestId,
    error_code: u64,
    error_reason: ReasonPhrase,
    /// only present from draft 07 to draft 11
    track_alias: Option<TrackAlias>
}

impl SubscribeErrorMessage {
    pub fn request_id(&self) -> RequestId {
        self.request_id
    }
    pub fn error_code(&self) -> u64 { self. error_code }
}

impl FromBytes for SubscribeErrorMessage {
    fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let header = ControlMessageHeader::from_bytes(b, version)?;
        assert_eq!(header.ty(), SUBSCRIBE_ERROR_CONTROL_MESSAGE_ID);
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
