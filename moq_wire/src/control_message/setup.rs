use crate::prelude::*;
use crate::control_message::ControlMessage;
use crate::error::{Error, Result};
use crate::{Frame, KeyValuePairs, Reader, SetupOptions, Version, Writer, SETUP_MESSAGE_ID};

/// SETUP (draft-ietf-moq-transport-19 §10.3) from draft 17, the first message of each endpoint.
/// CLIENT_SETUP and SERVER_SETUP before.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetupMessage {
    pub options: SetupOptions,
}

impl ControlMessage for SetupMessage {
    const MESSAGE_IDS: &'static [u64] = &[SETUP_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "setup" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> Result<()> {
        if version.draft() < 17 {
            return Err(Error::Unsupported(format!("SETUP in draft {}, CLIENT_SETUP and SERVER_SETUP there", version.draft())));
        }
        self.options.to_pairs(version).encode_uncounted(b, version)
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> Result<Self> {
        if version.draft() < 17 {
            return Err(Error::ProtocolViolation(format!("SETUP in draft {}", version.draft())));
        }
        Ok(Self { options: SetupOptions::from_pairs(&KeyValuePairs::decode_to_end(b, version)?, version)? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::{from_slice, to_vec};
    use crate::{MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_17, MOQ_VERSION_DRAFT_18};

    /// draft-ietf-moq-transport-17 §9.4: type 0x2F00, a u16 length, the options to the end.
    #[test]
    fn draft_17_setup() {
        let setup = SetupMessage { options: SetupOptions { implementation: Some(b"q".to_vec()), ..SetupOptions::default() } };
        // type 0x2F00 as a two byte vi64, length, MOQT_IMPLEMENTATION 0x07 length 1 "q"
        let bytes = to_vec(&setup, MOQ_VERSION_DRAFT_17).unwrap();
        assert_eq!(bytes, [0xAF, 0x00, 0, 3, 0x07, 1, b'q']);
        assert_eq!(from_slice::<SetupMessage>(&bytes, MOQ_VERSION_DRAFT_17).unwrap(), setup);
    }

    #[test]
    fn unknown_options_are_ignored() {
        // type, length, option 0x3F length 0
        let decoded = from_slice::<SetupMessage>(&[0xAF, 0x00, 0, 2, 0x3F, 0], MOQ_VERSION_DRAFT_18).unwrap();
        assert_eq!(decoded, SetupMessage::default());
    }

    #[test]
    fn drafts_before_17_have_client_and_server_setup() {
        assert!(matches!(to_vec(&SetupMessage::default(), MOQ_VERSION_DRAFT_16), Err(Error::Unsupported(_))));
    }
}
