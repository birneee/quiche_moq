use crate::bytes::FromBytes;
use crate::{Version, UNSUBSCRIBE_NAMESPACE_MESSAGE_ID};
use octets::Octets;
use crate::control_message::header::ControlMessageHeader;

#[derive(Debug)]
pub struct UnsubscribeNamespaceMessage {

}

impl FromBytes for UnsubscribeNamespaceMessage {
    fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let header = ControlMessageHeader::from_bytes(b, version)?;
        assert_eq!(header.ty(), UNSUBSCRIBE_NAMESPACE_MESSAGE_ID);
        todo!()
    }
}
