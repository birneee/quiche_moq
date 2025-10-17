use crate::bytes::FromBytes;
use crate::error::Result;
use crate::{Parameters, RequestId, Version, ANNOUNCE_CONTROL_MESSAGE_ID, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_13};
use octets::Octets;
use crate::control_message::header::ControlMessageHeader;
use crate::namespace::Namespace;

#[derive(Debug)]
pub struct AnnounceMessage {
    /// Some for DRAFT 11 to 13
    request_id: Option<RequestId>,
    track_namespace: Namespace,
    parameters: Parameters,
}

impl AnnounceMessage {
    /// Some for DRAFT 11 to 13
    pub fn request_id(&self) -> Option<RequestId> { self.request_id }
}

impl FromBytes for AnnounceMessage {
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
        assert_eq!(payload_end - payload_start, header.payload_length() as usize);
        Ok(Self {
            request_id,
            track_namespace,
            parameters,
        })
    }
}

#[cfg(test)]
mod test {
    use crate::bytes::FromBytes;
    use crate::MOQ_VERSION_DRAFT_07;
    use octets::Octets;
    use crate::control_message::ControlMessage;

    #[test]
    fn decode_announce_draft7() {
        let msg: &[u8] = &[0x6, 0x14, 0xc, 0x4, 0x1d, 0x1, 0x11, 0x75, 0x6e, 0x65, 0x78, 0x70, 0x65, 0x63, 0x74, 0x65, 0x64, 0x2d, 0x6d, 0x69, 0x6e, 0x6e, 0x6f, 0x77, 0x0];

        let mut o = Octets::with_slice(&msg);
        let cm = ControlMessage::from_bytes(&mut o, MOQ_VERSION_DRAFT_07).unwrap();
        println!("{:?}", cm);
        todo!()
    }
}