use crate::bytes::FromBytes;
use crate::error::Result;
use crate::{Parameters, RequestId, ToBytes, Version, ANNOUNCE_CONTROL_MESSAGE_ID, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_13};
use octets::{Octets, OctetsMut};
use crate::control_message::encode_control_message;
use crate::control_message::header::ControlMessageHeader;
use crate::namespace::Namespace;

#[derive(Debug)]
/// Called ANNOUNCE before draft-13
/// Called PUBLISH_NAMESPACE since draft-14
pub struct PublishNamespaceMessage {
    /// Some for DRAFT 11 to 13
    request_id: Option<RequestId>,
    track_namespace: Namespace,
    parameters: Parameters,
}

impl PublishNamespaceMessage {
    pub fn new(request_id: Option<RequestId>, track_namespace: Namespace, parameters: Parameters) -> Self {
        Self {
            request_id,
            track_namespace,
            parameters,
        }
    }

    /// Some for DRAFT 11 to 13
    pub fn request_id(&self) -> Option<RequestId> { self.request_id }

    pub fn track_namespace(&self) -> &Namespace {
        &self.track_namespace
    }

    pub fn take_track_namespace(self) -> Namespace {
        self.track_namespace
    }
}

impl FromBytes for PublishNamespaceMessage {
    fn from_bytes(b: &mut Octets, version: Version) -> Result<Self> {
        let header = ControlMessageHeader::from_bytes(b, version)?;
        assert_eq!(header.ty(), ANNOUNCE_CONTROL_MESSAGE_ID);
        let payload_start = b.off();
        let request_id = match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => None,
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => {
                Some(b.get_varint()?)
            }
            _ => unimplemented!()
        };
        let track_namespace = Namespace::from_bytes(b, version)?;
        let parameters = Parameters::from_bytes(b, version)?;
        let payload_end = b.off();
        assert_eq!(payload_end - payload_start, header.payload_length());
        Ok(Self {
            request_id,
            track_namespace,
            parameters,
        })
    }
}

impl ToBytes for PublishNamespaceMessage {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> Result<()> {
        encode_control_message(ANNOUNCE_CONTROL_MESSAGE_ID, version, b, |b| {
            match version {
                MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {},
                MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => {
                    b.put_varint(self.request_id.unwrap())?;
                }
                _ => unimplemented!()
            }
            self.track_namespace.to_bytes(b, version)?;
            self.parameters.to_bytes(b, version)?;
            Ok(())
        })
    }
}
