use crate::bytes::{FromBytes, ToBytes};
use crate::error::Result;
use crate::message_parameters::{MessageParams, Param};
use crate::{RequestId, Version, PUBLISH_NAMESPACE_MESSAGE_ID};
use crate::{Frame, Reader, Writer};
use crate::control_message::{decode_request_id, encode_request_id, required, ControlMessage};
use crate::namespace::Namespace;

/// PUBLISH_NAMESPACE (draft-ietf-moq-transport-19 §10.13), ANNOUNCE before draft 14.
#[derive(Debug)]
pub struct PublishNamespaceMessage {
    /// `None` until draft 10
    request_id: Option<RequestId>,
    track_namespace: Namespace,
}

impl PublishNamespaceMessage {
    pub fn new(request_id: Option<RequestId>, track_namespace: Namespace) -> Self {
        Self { request_id, track_namespace }
    }

    /// `None` until draft 10
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

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> Result<()> {
        match version.draft() {
            ..=10 => {},
            11.. => {
                encode_request_id(b, required(self.request_id, "a Request ID")?, version)?;
            }
        }
        self.track_namespace.to_bytes(b, version)?;
        MessageParams::new().to_bytes(b, version)
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> Result<Self> {
        let request_id = match version.draft() {
            ..=10 => None,
            11.. => {
                Some(decode_request_id(b, version)?)
            }
        };
        let track_namespace = Namespace::from_bytes(b, version)?;
        let mut params = MessageParams::from_bytes(b, version)?;
        // TODO authorization, until then tokens are ignored
        params.take_all_bytes(Param::AuthorizationToken);
        params.finish(version)?;
        Ok(Self { request_id, track_namespace })
    }
}
