use crate::bytes::{FromBytes, ToBytes};
use crate::error::Error;
use crate::vi64;
use crate::version;
use crate::{CLIENT_SETUP_MESSAGE_ID, CLIENT_SETUP_MESSAGE_ID_VERSION_UNTIL_10, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_18, SetupParameters, Version};
use octets::{Octets, OctetsMut};
use crate::control_message::ControlMessage;

#[derive(Debug, Eq, PartialEq)]
pub struct ClientSetupMessage {
    pub supported_versions: Vec<Version>,
    pub setup_parameters: SetupParameters,
}

impl ControlMessage for ClientSetupMessage {
    const MESSAGE_IDS: &'static [u64] = &[
        CLIENT_SETUP_MESSAGE_ID_VERSION_UNTIL_10,
        CLIENT_SETUP_MESSAGE_ID,
    ];

    fn message_id_for_version(version: Version) -> u64 {
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => CLIENT_SETUP_MESSAGE_ID_VERSION_UNTIL_10,
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_18 => CLIENT_SETUP_MESSAGE_ID,
            _ => unimplemented!()
        }
    }

    fn qlog_type_name(&self) -> &'static str { "client_setup" }

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        // draft 15+ moved version negotiation to ALPN / WT-Available-Protocols
        // (draft-16 §3.1) and dropped the version list from SETUP entirely.
        if version::negotiates_version_in_band(version) {
            vi64::write_varint(b, self.supported_versions.len() as u64, version)?;
            for supported_version in &self.supported_versions {
                vi64::write_varint(b, *supported_version, version)?;
            }
        }
        self.setup_parameters.to_bytes(b, version)?;
        Ok(())
    }

    /// A receiver can't know in advance which encoding profile the peer used —
    /// that's what SETUP is negotiating. Four profiles exist: vi64 + delta KVPs
    /// (draft 17+), old varints + delta KVPs (draft 15-16), old varints +
    /// absolute KVPs (draft 11-14), old varints + TLV triplets (draft 07-10).
    /// Try each; a wrong guess essentially never fully consumes the body.
    fn from_body_bytes(b: &mut Octets, _version: Version) -> crate::error::Result<Self> {
        let raw = b.buf();
        for encoding in [MOQ_VERSION_DRAFT_18, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_07] {
            if let Ok(v) = Self::decode_body(&mut Octets::with_slice(raw), encoding) {
                return Ok(v);
            }
        }
        Err(Error::ProtocolViolation("CLIENT_SETUP body matches no known encoding".into()))
    }
}

impl ClientSetupMessage {
    /// Decodes the body using `encoding`'s varint scheme, requiring it to consume the
    /// buffer exactly (see [`Self::from_body_bytes`]).
    fn decode_body(b: &mut Octets, encoding: Version) -> crate::error::Result<Self> {
        let supported_versions = if version::negotiates_version_in_band(encoding) {
            let num_supported_versions = vi64::read_varint(b, encoding)?;
            // Each version takes >= 1 byte, so this can never legitimately exceed the
            // remaining buffer — guards against a huge bogus count from misinterpreting
            // this length under the wrong (see `from_body_bytes`) varint scheme.
            if num_supported_versions > b.cap() as u64 {
                return Err(Error::ProtocolViolation("implausible supported_versions count".into()));
            }
            let mut versions = Vec::with_capacity(num_supported_versions as usize);
            for _ in 0..num_supported_versions {
                versions.push(vi64::read_varint(b, encoding)?);
            }
            versions
        } else {
            // No version list on the wire — `encoding` is only the wire-format
            // group (15-16, or 17-18), not the peer's exact version. Report the
            // group's newest member so both ends agree without real ALPN.
            vec![encoding]
        };
        let setup_parameters = SetupParameters::from_bytes(b, encoding)?;
        if b.cap() != 0 {
            return Err(Error::ProtocolViolation("SETUP body not fully consumed".into()));
        }
        Ok(Self { supported_versions, setup_parameters })
    }
}

#[cfg(test)]
mod tests {
    use crate::bytes::{FromBytes, ToBytes};
    use crate::{MOQ_VERSION_LITE_01_BY_KIXELATED, MOQ_VERSION_DRAFT_07};
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
            println!("{:?}", cm);
            assert_eq!(&cm.supported_versions, &[MOQ_VERSION_LITE_01_BY_KIXELATED, MOQ_VERSION_DRAFT_07]);
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