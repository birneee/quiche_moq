use octets::{Octets, OctetsMut};
use crate::{Version, UNSUBSCRIBE_NAMESPACE_MESSAGE_ID};
use crate::control_message::ControlMessage;

#[derive(Debug)]
pub struct UnsubscribeNamespaceMessage {

}

impl ControlMessage for UnsubscribeNamespaceMessage {
    const MESSAGE_IDS: &'static [u64] = &[UNSUBSCRIBE_NAMESPACE_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "unsubscribe_namespace" }

    fn to_body_bytes(&self, _b: &mut OctetsMut, _version: Version) -> crate::error::Result<()> {
        todo!()
    }

    fn from_body_bytes(_b: &mut Octets, _version: Version) -> crate::error::Result<Self> {
        todo!()
    }
}
