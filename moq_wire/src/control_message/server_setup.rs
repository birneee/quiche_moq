
use crate::prelude::*;
use crate::bytes::{FromBytes, ToBytes};
use crate::error::Error;
use crate::{MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_16, SERVER_SETUP_MESSAGE_ID, SERVER_SETUP_MESSAGE_ID_VERSION_UNTIL_10, KeyValuePairs, SetupOptions, Version};
use crate::{Frame, Reader};
use crate::Writer;
use crate::control_message::{until_draft, ControlMessage};

#[derive(Debug)]
pub struct ServerSetupMessage {
    pub selected_version: Version,
    pub options: SetupOptions,
}

impl ServerSetupMessage {
    pub fn new(selected_version: Version, options: SetupOptions) -> Self {
        Self { selected_version, options }
    }

    /// Decodes the body using `encoding`'s varint scheme, requiring it to consume the
    /// buffer exactly (see [`Self::from_body_bytes`]).
    fn decode_body(mut b: Reader<'_, Frame>, encoding: Version) -> crate::error::Result<Self> {
        // draft 15+ dropped the selected-version field from SERVER_SETUP's wire
        // format too (see `ClientSetupMessage::to_body_bytes`/`decode_body` for
        // the full rationale and the same group-ambiguity caveat).
        let selected_version = if encoding.negotiates_in_band() {
            let raw = b.varint()?;
            Version::from_wire(raw).ok_or_else(|| Error::ProtocolViolation(format!("selected version 0x{raw:x} not implemented")))?
        } else {
            encoding
        };
        let options = SetupOptions::from_pairs(&KeyValuePairs::from_bytes(&mut b, encoding)?, encoding)?;
        b.finish()?;
        Ok(Self { selected_version, options })
    }
}

impl ControlMessage for ServerSetupMessage {
    const MESSAGE_IDS: &'static [u64] = &[
        SERVER_SETUP_MESSAGE_ID_VERSION_UNTIL_10,
        SERVER_SETUP_MESSAGE_ID,
    ];

    fn message_id_for_version(version: Version) -> u64 {
        match version.draft() {
            ..=10 => SERVER_SETUP_MESSAGE_ID_VERSION_UNTIL_10,
            11.. => SERVER_SETUP_MESSAGE_ID,
        }
    }

    fn qlog_type_name(&self) -> &'static str { "server_setup" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> crate::error::Result<()> {
        until_draft(16, "SERVER_SETUP", version, false)?;
        if version.negotiates_in_band() {
            b.varint(self.selected_version.to_wire())?;
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
        until_draft(16, "SERVER_SETUP", version, true)?;
        let raw = b.rest();
        for encoding in [version, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_07] {
            if let Ok(v) = Self::decode_body(encoding.frame_reader(raw), encoding) {
                return Ok(v);
            }
        }
        Err(Error::ProtocolViolation("SERVER_SETUP body matches no known encoding".into()))
    }
}

#[cfg(test)]
mod test {
    use crate::RequestId;
    use crate::control_message::ControlMessageEnum;
    use super::*;
    use crate::{MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_13,
        MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_15};
    use rstest::rstest;

    /// A receiver decoding SERVER_SETUP never actually knows the real negotiated
    /// version in advance (that's what SETUP is negotiating) — it always falls back
    /// to some local default guess (`Config::setup_version`, draft-16 by default).
    /// Encode with the *real* version but always decode with that same draft-16
    /// guess, covering every real version this crate offers by default
    /// (`SUPPORTED_MOQ_VERSIONS`, draft 11+ — all sharing the fixed-u16 length field
    /// this guess needs to parse the header at all) to prove `from_body_bytes`'s
    /// try-each-profile disambiguation (see its doc comment) doesn't depend on
    /// guessing right. This is the exact scenario that broke default (unpinned)
    /// negotiation once draft-17/18 joined `SUPPORTED_MOQ_VERSIONS`.
    ///
    /// draft 15+ dropped the selected-version field from the wire, so only the
    /// wire-format group is recoverable, not the exact version — `expected_version`
    /// diverges from `real_version` for 15 and 17 here.
    #[rstest]
    #[case(MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_11)]
    #[case(MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_12)]
    #[case(MOQ_VERSION_DRAFT_13, MOQ_VERSION_DRAFT_13)]
    #[case(MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_14)]
    #[case(MOQ_VERSION_DRAFT_15, crate::MOQ_VERSION_DRAFT_16)]
    #[case(crate::MOQ_VERSION_DRAFT_16, crate::MOQ_VERSION_DRAFT_16)]
    fn roundtrips_regardless_of_decoder_version_guess(#[case] real_version: Version, #[case] expected_version: Version) {
        let orig = ServerSetupMessage::new(real_version, SetupOptions { max_request_id: Some(RequestId(100)), ..SetupOptions::default() });
        let mut b = vec![];
        orig.to_bytes(&mut real_version.writer(&mut b), real_version).unwrap();
        let len = b.len();
        let mut o = crate::MOQ_VERSION_DRAFT_16.reader(&b[..len]);
        // moq_core::Config::default().setup_version, duplicated here since moq_wire
        // doesn't depend on moq_core.
        let guess = crate::MOQ_VERSION_DRAFT_16;
        let ControlMessageEnum::ServerSetup(decoded) = ControlMessageEnum::from_bytes(&mut o, guess).unwrap() else { panic!() };
        assert_eq!(decoded.selected_version, expected_version);
        assert_eq!(decoded.options, orig.options);
    }

    /// A short body fits several encodings: the expected version wins.
    #[test]
    fn short_body_decodes_with_expected_version() {
        // parameter count, MAX_REQUEST_ID, 4
        let bytes = [0x21, 0, 3, 1, 2, 4];
        let ControlMessageEnum::ServerSetup(decoded) =
            ControlMessageEnum::from_bytes(&mut crate::MOQ_VERSION_DRAFT_16.reader(&bytes), crate::MOQ_VERSION_DRAFT_16).unwrap()
        else { panic!() };
        assert_eq!(decoded.selected_version, crate::MOQ_VERSION_DRAFT_16);
        assert_eq!(decoded.options.max_request_id, Some(RequestId(4)));
    }

    #[test]
    fn decode_draft7() {
        let b = [0x40, 0x41, 0xc, 0xc0, 0x0, 0x0, 0x0, 0xff, 0x0, 0x0, 0x7, 0x1, 0x0, 0x1, 0x3];
        let mut b = MOQ_VERSION_DRAFT_07.reader(&b);
        let cm = ControlMessageEnum::from_bytes(&mut b, MOQ_VERSION_DRAFT_07).unwrap();
        assert!(b.is_empty());
        assert!(matches!(cm, ControlMessageEnum::ServerSetup(..)));
        println!("{:?}", cm);

    }

    /// moq-lite-01 is a different protocol, selecting it is refused.
    #[test]
    fn selecting_unimplemented_version_is_refused() {
        let b = [0x40, 0x41, 0x9, 0xc0, 0x0, 0x0, 0x0, 0xff, 0xd, 0xad, 0x1, 0x0];
        assert!(ControlMessageEnum::from_bytes(&mut MOQ_VERSION_DRAFT_07.reader(&b), MOQ_VERSION_DRAFT_07).is_err());
    }
}
