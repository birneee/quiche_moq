use crate::prelude::*;
use crate::bytes::{FromBytes, ToBytes};
use crate::control_message::{required, ControlMessage};
use crate::{Frame, Namespace, Reader, RequestId, Version, Writer, PUBLISH_NAMESPACE_DONE_MESSAGE_ID};

/// PUBLISH_NAMESPACE_DONE until draft 16, UNANNOUNCE before draft 14; ending the request's stream from draft 17.
#[derive(Debug)]
pub struct PublishNamespaceDoneMessage {
    /// 0 until resolved where the draft names the namespace
    pub(crate) request_id: RequestId,
    /// the draft 7 to 15 name, `None` where the draft names the Request ID
    namespace: Option<Namespace>,
}

impl PublishNamespaceDoneMessage {
    pub fn new(request_id: RequestId, namespace: Namespace) -> Self {
        Self { request_id, namespace: Some(namespace) }
    }

    pub fn request_id(&self) -> RequestId { self.request_id }

    pub fn namespace(&self) -> Option<&Namespace> { self.namespace.as_ref() }
}

impl ControlMessage for PublishNamespaceDoneMessage {
    const MESSAGE_IDS: &'static [u64] = &[PUBLISH_NAMESPACE_DONE_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "publish_namespace_done" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> crate::Result<()> {
        match version.draft() {
            ..=15 => required(self.namespace.as_ref(), "the namespace drafts 7 to 15 name")?.to_bytes(b, version),
            16 => Ok(b.varint(self.request_id.0)?),
            _ => Err(crate::Error::Unsupported(format!("PUBLISH_NAMESPACE_DONE in draft {}, the request's stream ends instead", version.draft()))),
        }
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> crate::Result<Self> {
        match version.draft() {
            ..=15 => Ok(Self { request_id: RequestId(0), namespace: Some(Namespace::from_bytes(b, version)?) }),
            16 => Ok(Self { request_id: RequestId(b.varint()?), namespace: None }),
            _ => Err(crate::Error::ProtocolViolation(format!("PUBLISH_NAMESPACE_DONE in draft {}", version.draft()))),
        }
    }
}
