use crate::bytes::{FromBytes, ToBytes};
use crate::error::Result;
use crate::{Parameters, RequestId, Version, PUBLISH_NAMESPACE_MESSAGE_ID, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_13};
use octets::{Octets, OctetsMut};
use crate::control_message::ControlMessage;
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

impl ControlMessage for PublishNamespaceMessage {
    const MESSAGE_IDS: &'static [u64] = &[PUBLISH_NAMESPACE_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "publish_namespace" }

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> Result<()> {
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
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> Result<Self> {
        let request_id = match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => None,
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => {
                Some(b.get_varint()?)
            }
            _ => unimplemented!()
        };
        let track_namespace = Namespace::from_bytes(b, version)?;
        let parameters = Parameters::from_bytes(b, version)?;
        Ok(Self {
            request_id,
            track_namespace,
            parameters,
        })
    }
}
