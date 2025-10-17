use crate::bytes::ToBytes;
use crate::{RequestId, Version, ANNOUNCE_OK_CONTROL_MESSAGE_ID, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_13};
use octets::OctetsMut;
use crate::control_message::encode_control_message;
use crate::namespace::Namespace;

#[derive(Debug)]
pub struct AnnounceOkMessage {
    /// Some for draft 7 to 10
    request_id: Option<RequestId>,
    /// Some for draft 11 to 13
    track_namespace: Option<Namespace>
}

impl AnnounceOkMessage {
    pub fn new(request_id: Option<RequestId>, track_namespace: Option<Namespace>) -> Self { AnnounceOkMessage { request_id, track_namespace } }
}

impl ToBytes for AnnounceOkMessage {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        encode_control_message(ANNOUNCE_OK_CONTROL_MESSAGE_ID, version, b, |b| {
            match version {
                MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
                    self.track_namespace.as_ref().unwrap().to_bytes(b, version)?;
                }
                MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => {
                    b.put_varint(self.request_id.unwrap())?;
                }
                _ => unimplemented!()
            }
            b.put_varint(version)?;
            Ok(())
        })
    }
}


