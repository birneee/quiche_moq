use crate::bytes::{FromBytes, ToBytes};
use crate::control_message::{decode_request_id, encode_request_id, ControlMessage};
use crate::message_parameters::{MessageParams, Param};
use crate::{Frame, NamespaceTuple, Reader, RequestId, Version, Writer};
use crate::{NAMESPACE_DONE_MESSAGE_ID, NAMESPACE_MESSAGE_ID, SUBSCRIBE_NAMESPACE_REQUEST_MESSAGE_ID};

// TODO SUBSCRIBE_NAMESPACE of drafts 13 to 17 (type 0x11), until then refused as an unknown message,
//  implement when needed
/// The drafts with SUBSCRIBE_NAMESPACE on its own request stream, answered by NAMESPACE and NAMESPACE_DONE on it.
fn on_request_stream(version: Version) -> bool {
    version.draft() >= 18
}

/// SUBSCRIBE_NAMESPACE (draft-ietf-moq-transport-18 §10.18): the namespaces under `prefix`, now and later.
#[derive(Debug)]
pub struct SubscribeNamespaceMessage {
    pub request_id: RequestId,
    pub prefix: NamespaceTuple,
}

impl ControlMessage for SubscribeNamespaceMessage {
    const MESSAGE_IDS: &'static [u64] = &[SUBSCRIBE_NAMESPACE_REQUEST_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "subscribe_namespace" }

    fn accepts(version: Version) -> bool { on_request_stream(version) }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> crate::Result<()> {
        encode_request_id(b, self.request_id, version)?;
        self.prefix.to_bytes(b, version)?;
        MessageParams::new().to_bytes(b, version)
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> crate::Result<Self> {
        let request_id = decode_request_id(b, version)?;
        let prefix = NamespaceTuple::from_bytes(b, version)?;
        let mut params = MessageParams::from_bytes(b, version)?;
        // TODO authorization, until then tokens are ignored
        params.take_all_bytes(Param::AuthorizationToken);
        params.finish(version)?;
        Ok(Self { request_id, prefix })
    }
}

/// NAMESPACE (§10.16): a namespace under the SUBSCRIBE_NAMESPACE's prefix, by the fields after it.
#[derive(Debug)]
pub struct NamespaceMessage {
    pub suffix: NamespaceTuple,
}

impl ControlMessage for NamespaceMessage {
    const MESSAGE_IDS: &'static [u64] = &[NAMESPACE_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "namespace" }

    fn accepts(version: Version) -> bool { on_request_stream(version) }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> crate::Result<()> {
        self.suffix.to_bytes(b, version)
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> crate::Result<Self> {
        Ok(Self { suffix: NamespaceTuple::from_bytes(b, version)? })
    }
}

/// NAMESPACE_DONE (§10.17): a namespace a NAMESPACE named is no longer published.
#[derive(Debug)]
pub struct NamespaceDoneMessage {
    pub suffix: NamespaceTuple,
}

impl ControlMessage for NamespaceDoneMessage {
    const MESSAGE_IDS: &'static [u64] = &[NAMESPACE_DONE_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "namespace_done" }

    fn accepts(version: Version) -> bool { on_request_stream(version) }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> crate::Result<()> {
        self.suffix.to_bytes(b, version)
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> crate::Result<Self> {
        Ok(Self { suffix: NamespaceTuple::from_bytes(b, version)? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::{from_slice, to_vec};
    use crate::control_message::ControlMessageEnum;
    use crate::MOQ_VERSION_DRAFT_18;

    #[test]
    fn subscribe_namespace_round_trips() {
        let version = MOQ_VERSION_DRAFT_18;
        let message = SubscribeNamespaceMessage { request_id: RequestId(4), prefix: NamespaceTuple::new(vec![b"moq-test".to_vec()]).unwrap() };
        let decoded: SubscribeNamespaceMessage = from_slice(&to_vec(&message, version).unwrap(), version).unwrap();
        assert_eq!((decoded.request_id, decoded.prefix), (message.request_id, message.prefix));
    }

    /// Types 0x8 and 0xE are NAMESPACE and NAMESPACE_DONE from draft 16, other messages before.
    #[test]
    fn namespace_messages_take_their_type_by_draft() {
        let version = MOQ_VERSION_DRAFT_18;
        let suffix = NamespaceTuple::new(vec![b"interop".to_vec()]).unwrap();
        let namespace = to_vec(&NamespaceMessage { suffix: suffix.clone() }, version).unwrap();
        assert!(matches!(from_slice(&namespace, version).unwrap(), ControlMessageEnum::Namespace(m) if m.suffix == suffix));
        let done = to_vec(&NamespaceDoneMessage { suffix: suffix.clone() }, version).unwrap();
        assert!(matches!(from_slice(&done, version).unwrap(), ControlMessageEnum::NamespaceDone(m) if m.suffix == suffix));
    }
}
