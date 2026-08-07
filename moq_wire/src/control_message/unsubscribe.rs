use crate::vi64;
use crate::{RequestId, Version, UNSUBSCRIBE_MESSAGE_ID};
use crate::control_message::ControlMessage;
use octets::{Octets, OctetsMut};

/// Sent by a subscriber to tell the publisher to stop sending Objects for a
/// track (draft-16 §9.12). Same shape (Type 0xA, just an ID) across every
/// draft this crate supports — only the field's name changed, from
/// "Subscribe ID" (draft 07-10) to "Request ID" (draft 11+).
#[derive(Debug)]
pub struct UnsubscribeMessage {
    pub request_id: RequestId,
}

impl ControlMessage for UnsubscribeMessage {
    const MESSAGE_IDS: &'static [u64] = &[UNSUBSCRIBE_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "unsubscribe" }

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        vi64::write_varint(b, self.request_id, version)?;
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let request_id = vi64::read_varint(b, version)?;
        Ok(Self { request_id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::{FromBytes, ToBytes};
    use crate::MOQ_VERSION_DRAFT_16;

    #[test]
    fn encode_decode() {
        let orig = UnsubscribeMessage { request_id: 7 };
        let mut b = [0u8; 32];
        let mut o = OctetsMut::with_slice(&mut b);
        orig.to_bytes(&mut o, MOQ_VERSION_DRAFT_16).unwrap();
        let len = o.off();
        let decoded = UnsubscribeMessage::from_bytes(&mut Octets::with_slice(&b[..len]), MOQ_VERSION_DRAFT_16).unwrap();
        assert_eq!(decoded.request_id, orig.request_id);
    }
}
