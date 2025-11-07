use crate::bytes::{FromBytes, ToBytes};
use crate::{SetupParameters, Version, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_13, SERVER_SETUP_CONTROL_MESSAGE_ID, SERVER_SETUP_CONTROL_MESSAGE_ID_VERSION_UNTIL_10};
use octets::{Octets, OctetsMut};
use crate::control_message::encode_control_message;
use crate::control_message::header::ControlMessageHeader;

#[derive(Debug)]
pub struct ServerSetupMessage {
    pub selected_version: Version,
    pub setup_parameters: SetupParameters,
}

impl ServerSetupMessage {
    pub fn new(selected_version: Version, setup_parameters: SetupParameters) -> Self {
        Self {
            selected_version,
            setup_parameters,
        }
    }
}

impl ToBytes for ServerSetupMessage {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        let ty = match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => SERVER_SETUP_CONTROL_MESSAGE_ID_VERSION_UNTIL_10,
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => SERVER_SETUP_CONTROL_MESSAGE_ID,
            _ => unimplemented!()
        };
        encode_control_message(ty, version, b, |b| {
            b.put_varint(self.selected_version)?;
            SetupParameters::to_bytes(&self.setup_parameters, b, version)?;
            Ok(())
        })?;
        Ok(())
    }
}

impl FromBytes for ServerSetupMessage {
    fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let header = ControlMessageHeader::from_bytes(b, version)?;
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
                assert_eq!(header.ty(), SERVER_SETUP_CONTROL_MESSAGE_ID_VERSION_UNTIL_10);
            }
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => {
                assert_eq!(header.ty(), SERVER_SETUP_CONTROL_MESSAGE_ID);
            }
            _ => unimplemented!()
        }
        assert!(b.cap() >= header.len());
        let selected_version = b.get_varint().unwrap();
        let setup_parameters = SetupParameters::from_bytes(b, version)?;
        Ok(Self {
            selected_version,
            setup_parameters,
        })
    }
}

#[cfg(test)]
mod test {
    use crate::control_message::ControlMessage;
    use super::*;
    use crate::MOQ_VERSION_DRAFT_07;

    #[test]
    fn decode_draft7() {
        let b = [0x40, 0x41, 0xc, 0xc0, 0x0, 0x0, 0x0, 0xff, 0x0, 0x0, 0x7, 0x1, 0x0, 0x1, 0x3];
        let mut b = Octets::with_slice(&b);
        let cm = ControlMessage::from_bytes(&mut b, MOQ_VERSION_DRAFT_07).unwrap();
        assert_eq!(b.cap(), 0);
        assert!(matches!(cm, ControlMessage::ServerSetup(..)));
        println!("{:?}", cm);

        let b = [0x40, 0x41, 0x9, 0xc0, 0x0, 0x0, 0x0, 0xff, 0xd, 0xad, 0x1, 0x0];
        let mut b = Octets::with_slice(&b);
        let cm = ControlMessage::from_bytes(&mut b, MOQ_VERSION_DRAFT_07).unwrap();
        assert_eq!(b.cap(), 0);
        assert!(matches!(cm, ControlMessage::ServerSetup(..)));
        println!("{:?}", cm);
    }
}