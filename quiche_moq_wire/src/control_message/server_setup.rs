use crate::bytes::{FromBytes, ToBytes};
use crate::{SetupParameters, Version, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_13, SERVER_SETUP_MESSAGE_ID, SERVER_SETUP_MESSAGE_ID_VERSION_UNTIL_10};
use octets::{Octets, OctetsMut};
use crate::control_message::ControlMessage;

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

impl ControlMessage for ServerSetupMessage {
    const MESSAGE_IDS: &'static [u64] = &[
        SERVER_SETUP_MESSAGE_ID_VERSION_UNTIL_10,
        SERVER_SETUP_MESSAGE_ID,
    ];

    fn message_id_for_version(version: Version) -> u64 {
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => SERVER_SETUP_MESSAGE_ID_VERSION_UNTIL_10,
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => SERVER_SETUP_MESSAGE_ID,
            _ => unimplemented!()
        }
    }

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        b.put_varint(self.selected_version)?;
        SetupParameters::to_bytes(&self.setup_parameters, b, version)?;
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
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
    use crate::control_message::ControlMessageEnum;
    use super::*;
    use crate::MOQ_VERSION_DRAFT_07;

    #[test]
    fn decode_draft7() {
        let b = [0x40, 0x41, 0xc, 0xc0, 0x0, 0x0, 0x0, 0xff, 0x0, 0x0, 0x7, 0x1, 0x0, 0x1, 0x3];
        let mut b = Octets::with_slice(&b);
        let cm = ControlMessageEnum::from_bytes(&mut b, MOQ_VERSION_DRAFT_07).unwrap();
        assert_eq!(b.cap(), 0);
        assert!(matches!(cm, ControlMessageEnum::ServerSetup(..)));
        println!("{:?}", cm);

        let b = [0x40, 0x41, 0x9, 0xc0, 0x0, 0x0, 0x0, 0xff, 0xd, 0xad, 0x1, 0x0];
        let mut b = Octets::with_slice(&b);
        let cm = ControlMessageEnum::from_bytes(&mut b, MOQ_VERSION_DRAFT_07).unwrap();
        assert_eq!(b.cap(), 0);
        assert!(matches!(cm, ControlMessageEnum::ServerSetup(..)));
        println!("{:?}", cm);
    }
}