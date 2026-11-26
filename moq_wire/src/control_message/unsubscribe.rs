use crate::{RequestId, Version, MOQ_VERSION_DRAFT_17, UNSUBSCRIBE_MESSAGE_ID};
use crate::control_message::ControlMessage;
use crate::{Frame, Reader, Writer};

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

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> crate::error::Result<()> {
        reject_from_draft_17(version)?;
        b.varint(self.request_id.0)?;
        Ok(())
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> crate::error::Result<Self> {
        reject_from_draft_17(version)?;
        let request_id = RequestId(b.varint()?);
        Ok(Self { request_id })
    }
}

/// draft 17 §5.1: STOP_SENDING on the request stream replaced UNSUBSCRIBE.
fn reject_from_draft_17(version: Version) -> crate::error::Result<()> {
    if version >= MOQ_VERSION_DRAFT_17 {
        return Err(crate::error::Error::ProtocolViolation("UNSUBSCRIBE from draft 17".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::{FromBytes, ToBytes};
    use crate::MOQ_VERSION_DRAFT_16;

    #[test]
    fn encode_decode() {
        let orig = UnsubscribeMessage { request_id: RequestId(7) };
        let mut b = vec![];
        orig.to_bytes(&mut MOQ_VERSION_DRAFT_16.writer(&mut b), MOQ_VERSION_DRAFT_16).unwrap();
        let len = b.len();
        let decoded = UnsubscribeMessage::from_bytes(&mut MOQ_VERSION_DRAFT_16.reader(&b[..len]), MOQ_VERSION_DRAFT_16).unwrap();
        assert_eq!(decoded.request_id, orig.request_id);
    }

    #[test]
    fn rejected_from_draft_17() {
        let decoded = UnsubscribeMessage::from_body_bytes(&mut crate::MOQ_VERSION_DRAFT_17.frame_reader(&[7]), crate::MOQ_VERSION_DRAFT_17);
        assert!(matches!(decoded, Err(crate::Error::ProtocolViolation(_))), "{decoded:?}");
    }
}
