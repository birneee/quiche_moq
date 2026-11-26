use crate::{Frame, Reader, Writer};
use crate::{Version, UNSUBSCRIBE_NAMESPACE_MESSAGE_ID};
use crate::control_message::ControlMessage;

#[derive(Debug)]
pub struct UnsubscribeNamespaceMessage;

impl ControlMessage for UnsubscribeNamespaceMessage {
    const MESSAGE_IDS: &'static [u64] = &[UNSUBSCRIBE_NAMESPACE_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "unsubscribe_namespace" }

    // TODO implement both with SUBSCRIBE_NAMESPACE; we never send it, so a peer has nothing to unsubscribe
    fn to_body_bytes(&self, _b: &mut Writer, _version: Version) -> crate::error::Result<()> {
        Err(crate::error::Error::Unsupported("UNSUBSCRIBE_NAMESPACE".into()))
    }

    fn from_body_bytes(_b: &mut Reader<'_, Frame>, _version: Version) -> crate::error::Result<Self> {
        Err(crate::error::Error::Unsupported("UNSUBSCRIBE_NAMESPACE".into()))
    }
}
