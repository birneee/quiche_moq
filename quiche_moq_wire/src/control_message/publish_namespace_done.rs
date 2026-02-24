use crate::bytes::{FromBytes, ToBytes};
use crate::{Namespace, RequestId, Version, PUBLISH_NAMESPACE_DONE_MESSAGE_ID};
use crate::{MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16};
use crate::control_message::ControlMessage;
use octets::{Octets, OctetsMut};

#[derive(Debug)]
/// Called UNANNOUNCE before draft-14
/// Called PUBLISH_NAMESPACE_DONE since draft-14
pub struct PublishNamespaceDoneMessage {
    /// Present in draft 16+
    request_id: Option<RequestId>,
    /// Present in drafts 07–15
    namespace: Option<Namespace>,
}

impl PublishNamespaceDoneMessage {
    pub fn new(request_id: Option<RequestId>, namespace: Option<Namespace>) -> Self {
        Self { request_id, namespace }
    }

    pub fn request_id(&self) -> Option<RequestId> { self.request_id }
    pub fn namespace(&self) -> Option<&Namespace> { self.namespace.as_ref() }
}

impl ControlMessage for PublishNamespaceDoneMessage {
    const MESSAGE_IDS: &'static [u64] = &[PUBLISH_NAMESPACE_DONE_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "publish_namespace_done" }

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_15 => {
                self.namespace.as_ref().unwrap().to_bytes(b, version)?;
            }
            MOQ_VERSION_DRAFT_16.. => {
                b.put_varint(self.request_id.unwrap())?;
            }
            _ => unimplemented!()
        }
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_15 => {
                let namespace = Namespace::from_bytes(b, version)?;
                Ok(Self { request_id: None, namespace: Some(namespace) })
            }
            MOQ_VERSION_DRAFT_16.. => {
                let request_id = b.get_varint()?;
                Ok(Self { request_id: Some(request_id), namespace: None })
            }
            _ => unimplemented!()
        }
    }
}
