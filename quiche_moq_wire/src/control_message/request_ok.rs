use crate::bytes::{FromBytes, ToBytes};
use crate::{RequestId, Version, REQUEST_OK_MESSAGE_ID, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_13};
use octets::{Octets, OctetsMut};
use crate::control_message::ControlMessage;
use crate::namespace::Namespace;

#[derive(Debug)]
/// Called ANNOUNCE_OK before draft-13
/// Called REQUEST_OK since draft-14
pub struct RequestOkMessage {
    /// Some for draft 7 to 10
    request_id: Option<RequestId>,
    /// Some for draft 11 to 13
    track_namespace: Option<Namespace>
}

impl RequestOkMessage {
    pub fn new(request_id: Option<RequestId>, track_namespace: Option<Namespace>) -> Self { RequestOkMessage { request_id, track_namespace } }
}

impl ControlMessage for RequestOkMessage {
    const MESSAGE_IDS: &'static [u64] = &[REQUEST_OK_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "request_ok" }

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
                self.track_namespace.as_ref().unwrap().to_bytes(b, version)?;
            }
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => {
                b.put_varint(self.request_id.unwrap())?;
            }
            _ => unimplemented!()
        }
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
                let track_namespace = Some(Namespace::from_bytes(b, version)?);
                Ok(Self{
                    track_namespace,
                    request_id: None,
                })
            }
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => {
                let request_id = Some(b.get_varint()?);
                Ok(Self {
                    request_id,
                    track_namespace: None,
                })
            }
            _ => unimplemented!()
        }
    }
}
