use crate::bytes::{FromBytes, ToBytes};
use crate::{SetupParameters, Version, CLIENT_SETUP_CONTROL_MESSAGE_ID, CLIENT_SETUP_CONTROL_MESSAGE_ID_VERSION_UNTIL_10, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_13};
use octets::{Octets, OctetsMut};
use crate::control_message::header::ControlMessageHeader;

#[derive(Debug, Eq, PartialEq)]
pub struct ClientSetupMessage {
    pub supported_versions: Vec<Version>,
    pub setup_parameters: SetupParameters,
}

impl ToBytes for ClientSetupMessage {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
                b.put_varint(CLIENT_SETUP_CONTROL_MESSAGE_ID_VERSION_UNTIL_10)?;
            }
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => {
                b.put_varint(CLIENT_SETUP_CONTROL_MESSAGE_ID)?;
            }
            _ => unimplemented!()
        }
        let len_off = b.off();
        b.skip(2)?;
        b.put_varint(self.supported_versions.len() as u64)?;
        for supported_version in &self.supported_versions {
            b.put_varint(*supported_version)?;
        }
        self.setup_parameters.to_bytes(b, version)?;
        crate::control_message::set_control_message_length(b, len_off, version)?;
        Ok(())
    }
}

impl FromBytes for ClientSetupMessage {
    fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let header = ControlMessageHeader::from_bytes(b, version)?;
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => assert_eq!(header.ty, CLIENT_SETUP_CONTROL_MESSAGE_ID_VERSION_UNTIL_10),
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => assert_eq!(header.ty, CLIENT_SETUP_CONTROL_MESSAGE_ID),
            _ => unimplemented!()
        };
        let start_off = b.off();
        let num_supported_versions = b.get_varint()?;
        let mut supported_versions = Vec::with_capacity(num_supported_versions as usize);
        for _ in 0..num_supported_versions {
            supported_versions.push(b.get_varint()?);
        }
        let setup_parameters = SetupParameters::from_bytes(b, version)?;
        let payload_len = b.off() - start_off;
        assert_eq!(payload_len, header.len);
        Ok(Self{
            supported_versions,
            setup_parameters,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::bytes::{FromBytes, ToBytes};
    use crate::MOQ_VERSION_DRAFT_07;
    use crate::{Parameter, SetupParameters};
    use octets::{Octets, OctetsMut};
    use crate::control_message::client_setup::ClientSetupMessage;
    use crate::role::Role;

    #[test]
    fn decode_draft07() {
        let msgs = [
            [0x40, 0x40, 0x15, 0x2, 0xc0, 0x0, 0x0, 0x0, 0xff, 0xd, 0xad, 0x1, 0xc0, 0x0, 0x0, 0x0, 0xff, 0x0, 0x0, 0x7, 0x1, 0x0, 0x1, 0x3].as_slice(),
        ];
        for b in msgs {
            let mut b = Octets::with_slice(&b);
            let cm = ClientSetupMessage::from_bytes(&mut b, MOQ_VERSION_DRAFT_07).unwrap();
            println!("{:?}", cm)
        }
    }

    #[test]
    fn encode_decode_draft07() {
        let mut b = [0u8; 100];
        let mut o = OctetsMut::with_slice(&mut b);
        let orig = ClientSetupMessage {
            supported_versions: vec![MOQ_VERSION_DRAFT_07, 0xff],
            setup_parameters: SetupParameters {
                path: None,
                max_request_id: Some(100),
                role: Some(Role::Publisher),
                extra_parameters: vec![
                    Parameter::new_bytes(8, vec![9])
                ],
            },
        };
        orig.to_bytes(&mut o, MOQ_VERSION_DRAFT_07).unwrap();
        let len = o.off();
        let b = &b[..len];
        let mut o = Octets::with_slice(b);
        let decoded = ClientSetupMessage::from_bytes(&mut o, MOQ_VERSION_DRAFT_07).unwrap();
        assert_eq!(orig, decoded);
    }
}