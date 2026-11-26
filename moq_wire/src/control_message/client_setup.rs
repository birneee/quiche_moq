use crate::prelude::*;
use crate::bytes::{FromBytes, ToBytes};
use crate::error::Error;
use crate::{CLIENT_SETUP_MESSAGE_ID, CLIENT_SETUP_MESSAGE_ID_VERSION_UNTIL_10, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_16, KeyValuePairs, SetupOptions, Version};
use crate::{Frame, Reader, Writer};
use crate::control_message::{until_draft, ControlMessage};

#[derive(Debug, Eq, PartialEq)]
pub struct ClientSetupMessage {
    /// Raw, a peer may offer versions this crate doesn't implement.
    pub supported_versions: Vec<u64>,
    pub options: SetupOptions,
}

impl ControlMessage for ClientSetupMessage {
    const MESSAGE_IDS: &'static [u64] = &[
        CLIENT_SETUP_MESSAGE_ID_VERSION_UNTIL_10,
        CLIENT_SETUP_MESSAGE_ID,
    ];

    fn message_id_for_version(version: Version) -> u64 {
        match version.draft() {
            ..=10 => CLIENT_SETUP_MESSAGE_ID_VERSION_UNTIL_10,
            11.. => CLIENT_SETUP_MESSAGE_ID,
        }
    }

    fn qlog_type_name(&self) -> &'static str { "client_setup" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> crate::error::Result<()> {
        until_draft(16, "CLIENT_SETUP", version, false)?;
        // draft 15+ moved version negotiation to ALPN / WT-Available-Protocols
        // (draft-16 §3.1) and dropped the version list from SETUP entirely.
        if version.negotiates_in_band() {
            b.varint(self.supported_versions.len() as u64)?;
            for supported_version in &self.supported_versions {
                b.varint(*supported_version)?;
            }
        }
        self.options.to_pairs(version).to_bytes(b, version)?;
        Ok(())
    }

    /// A receiver can't know in advance which encoding profile the peer used —
    /// that's what SETUP is negotiating. Three profiles exist: delta KVPs (draft 15-16),
    /// absolute KVPs (draft 11-14), TLV triplets (draft 07-10); SETUP replaces it from draft 17.
    /// Try each; a wrong guess rarely fully consumes the body, but a short
    /// one can fit several, so `version` goes first: exact when the version
    /// is known out of band (draft 15+), the local guess otherwise.
    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> crate::error::Result<Self> {
        until_draft(16, "CLIENT_SETUP", version, true)?;
        let raw = b.rest();
        for encoding in [version, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_07] {
            if let Ok(v) = Self::decode_body(encoding.frame_reader(raw), encoding) {
                return Ok(v);
            }
        }
        Err(Error::ProtocolViolation("CLIENT_SETUP body matches no known encoding".into()))
    }
}

impl ClientSetupMessage {
    /// Decodes the body using `encoding`'s varint scheme, requiring it to consume the
    /// buffer exactly (see [`Self::from_body_bytes`]).
    fn decode_body(mut b: Reader<'_, Frame>, encoding: Version) -> crate::error::Result<Self> {
        let supported_versions = if encoding.negotiates_in_band() {
            let num_supported_versions = b.varint()?;
            // Each version takes >= 1 byte, so this can never legitimately exceed the
            // remaining buffer — guards against a huge bogus count from misinterpreting
            // this length under the wrong (see `from_body_bytes`) varint scheme.
            if num_supported_versions > b.remaining() as u64 {
                return Err(Error::ProtocolViolation("implausible supported_versions count".into()));
            }
            let mut versions = Vec::with_capacity(num_supported_versions as usize);
            for _ in 0..num_supported_versions {
                versions.push(b.varint()?);
            }
            versions
        } else {
            // No version list on the wire — `encoding` is only the wire-format
            // group (15-16, or 17-18), not the peer's exact version. Report the
            // group's newest member so both ends agree without real ALPN.
            vec![encoding.to_wire()]
        };
        let options = SetupOptions::from_pairs(&KeyValuePairs::from_bytes(&mut b, encoding)?, encoding)?;
        b.finish()?;
        Ok(Self { supported_versions, options })
    }
}

#[cfg(test)]
mod tests {
    use crate::RequestId;
    use crate::bytes::{FromBytes, ToBytes};
    use crate::MOQ_VERSION_DRAFT_07;
    use crate::SetupOptions;
    
    use crate::control_message::client_setup::ClientSetupMessage;
    use crate::role::Role;

    #[test]
    fn decode_draft07() {
        let msgs = [
            [0x40, 0x40, 0x15, 0x2, 0xc0, 0x0, 0x0, 0x0, 0xff, 0xd, 0xad, 0x1, 0xc0, 0x0, 0x0, 0x0, 0xff, 0x0, 0x0, 0x7, 0x1, 0x0, 0x1, 0x3].as_slice(),
        ];
        for b in msgs {
            let mut b = MOQ_VERSION_DRAFT_07.reader(b);
            let cm = ClientSetupMessage::from_bytes(&mut b, MOQ_VERSION_DRAFT_07).unwrap();
            println!("{:?}", cm);
            // moq-lite-01, which a real client offered next to a draft
            assert_eq!(&cm.supported_versions, &[0xff0dad01, MOQ_VERSION_DRAFT_07.to_wire()]);
        }
    }

    #[test]
    fn encode_decode_draft07() {
        let mut b = vec![];
        let orig = ClientSetupMessage {
            supported_versions: vec![MOQ_VERSION_DRAFT_07.to_wire(), 0xff],
            options: SetupOptions { max_request_id: Some(RequestId(100)), role: Some(Role::Publisher), ..SetupOptions::default() },
        };
        orig.to_bytes(&mut MOQ_VERSION_DRAFT_07.writer(&mut b), MOQ_VERSION_DRAFT_07).unwrap();
        let len = b.len();
        let b = &b[..len];
        let mut o = MOQ_VERSION_DRAFT_07.reader(b);
        let decoded = ClientSetupMessage::from_bytes(&mut o, MOQ_VERSION_DRAFT_07).unwrap();
        assert_eq!(orig, decoded);
    }
}