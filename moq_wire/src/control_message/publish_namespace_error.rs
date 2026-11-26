use crate::prelude::*;
use crate::bytes::{FromBytes, ToBytes};
use crate::error::Error::ProtocolViolation;
use crate::namespace::Namespace;
use crate::{ReasonPhrase, RequestErrorCode, RequestErrorMessage, RequestId, Version, NAMESPACE_MESSAGE_ID};
use crate::{Frame, Reader, Writer};
use crate::control_message::{required, ControlMessage};

/// Rejects a PUBLISH_NAMESPACE until draft 14, REQUEST_ERROR does since draft 15.
/// Called ANNOUNCE_ERROR before draft 14.
#[derive(Debug)]
pub struct PublishNamespaceErrorMessage {
    /// 0 until resolved in drafts 7 to 10, which name the namespace
    pub(crate) request_id: RequestId,
    /// the draft 7 to 10 name
    track_namespace: Option<Namespace>,
    error_code: RequestErrorCode,
    error_reason: ReasonPhrase,
}

impl PublishNamespaceErrorMessage {
    pub fn new(request_id: RequestId, track_namespace: Namespace, error_code: RequestErrorCode) -> Self {
        Self {
            request_id,
            track_namespace: Some(track_namespace),
            error_code,
            error_reason: ReasonPhrase::default(),
        }
    }

    pub fn request_id(&self) -> RequestId { self.request_id }

    /// The rejected PUBLISH_NAMESPACE, draft 7 to 10 only.
    pub fn track_namespace(&self) -> Option<&Namespace> { self.track_namespace.as_ref() }

    /// The same rejection as a REQUEST_ERROR.
    pub fn into_request_error(self) -> RequestErrorMessage {
        RequestErrorMessage::new(self.request_id, self.error_code).with_reason(self.error_reason)
    }
}

impl ControlMessage for PublishNamespaceErrorMessage {
    const MESSAGE_IDS: &'static [u64] = &[NAMESPACE_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "publish_namespace_error" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> crate::error::Result<()> {
        match version.draft() {
            ..=10 => required(self.track_namespace.as_ref(), "the namespace drafts 7 to 10 name")?.to_bytes(b, version)?,
            11..=14 => { b.varint(self.request_id.0)?; }
            _ => return Err(ProtocolViolation(format!("no PUBLISH_NAMESPACE_ERROR in draft {}", version.draft()))),
        }
        b.varint(self.error_code.into())?;
        Ok(self.error_reason.encode(b)?)
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> crate::error::Result<Self> {
        let (request_id, track_namespace) = match version.draft() {
            ..=10 => (RequestId(0), Some(Namespace::from_bytes(b, version)?)),
            11..=14 => (RequestId(b.varint()?), None),
            _ => return Err(ProtocolViolation(format!("no PUBLISH_NAMESPACE_ERROR in draft {}", version.draft()))),
        };
        let error_code = RequestErrorCode::from(b.varint()?);
        let error_reason = ReasonPhrase::decode(b)?;
        Ok(Self { request_id, track_namespace, error_code, error_reason })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_14};
    use rstest::rstest;

    #[rstest]
    #[case(MOQ_VERSION_DRAFT_07)]
    #[case(MOQ_VERSION_DRAFT_11)]
    #[case(MOQ_VERSION_DRAFT_14)]
    fn round_trips(#[case] version: Version) {
        let namespace: Namespace = "a-b".parse().unwrap();
        let message = PublishNamespaceErrorMessage::new(RequestId(4), namespace.clone(), RequestErrorCode::Uninterested);
        let mut buf = vec![];
        message.to_bytes(&mut version.writer(&mut buf), version).unwrap();
        let len = buf.len();
        let decoded = PublishNamespaceErrorMessage::from_bytes(&mut version.reader(&buf[..len]), version).unwrap();
        // the draft names the request by namespace or by Request ID
        match version.draft() {
            ..=10 => assert_eq!(decoded.track_namespace(), Some(&namespace)),
            _ => assert_eq!(decoded.request_id(), RequestId(4)),
        }
        assert_eq!(decoded.into_request_error().error_code(), RequestErrorCode::Uninterested);
    }
}
