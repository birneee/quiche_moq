use crate::bytes::{FromBytes, ToBytes};
use crate::control_message::publish_ok::PublishOkMessage;
use crate::control_message::{required, ControlMessage};
use crate::error::Result;
use crate::location::Location;
use crate::message_parameters::{MessageParams, Param};
use crate::namespace::Namespace;
use crate::{Frame, KeyValuePairs, Reader, RequestId, Version, Writer, REQUEST_OK_MESSAGE_ID};

/// REQUEST_OK (draft-ietf-moq-transport-19 §10.5), ANNOUNCE_OK before draft 13.
/// Its parameters mean what the request it answers defines.
#[derive(Debug)]
pub struct RequestOkMessage {
    /// 0 until resolved where the draft names the namespace or the stream names it
    pub(crate) request_id: RequestId,
    /// the namespace of an ANNOUNCE_OK, drafts 7 to 10 name it
    track_namespace: Option<Namespace>,
    /// draft 15 on
    params: MessageParams,
    /// draft 18 on
    track_properties: KeyValuePairs,
}

impl RequestOkMessage {
    pub fn new(request_id: RequestId) -> Self {
        Self { request_id, track_namespace: None, params: MessageParams::new(), track_properties: KeyValuePairs::new() }
    }

    /// Accepting a PUBLISH_NAMESPACE, which drafts 7 to 10 name by `track_namespace`.
    pub fn for_publish_namespace(request_id: RequestId, track_namespace: Namespace) -> Self {
        Self { track_namespace: Some(track_namespace), ..Self::new(request_id) }
    }

    /// Answering a subscription's REQUEST_UPDATE with its LARGEST_OBJECT (draft-ietf-moq-transport-19 §10.2.16).
    pub fn for_request_update(request_id: RequestId, largest_location: Option<Location>) -> Self {
        let mut message = Self::new(request_id);
        if let Some(location) = largest_location {
            message.params.put_location(Param::LargestObject, location);
        }
        message
    }

    /// TRACK_STATUS_OK from draft 15: what a SUBSCRIBE_OK would name, Track Properties from draft 18.
    pub fn for_track_status(request_id: RequestId, largest_location: Option<Location>, track_properties: KeyValuePairs) -> Self {
        Self { track_properties, ..Self::for_request_update(request_id, largest_location) }
    }

    /// LARGEST_OBJECT, of a REQUEST_UPDATE_OK or TRACK_STATUS_OK.
    pub fn largest_location(&self) -> Option<Location> {
        self.params.clone().take_location(Param::LargestObject)
    }

    /// The PUBLISH_OK this is from draft 18.
    pub fn into_publish_ok(self, version: Version) -> Result<PublishOkMessage> {
        PublishOkMessage::from_params(self.request_id, self.params, version)
    }

    pub fn track_properties(&self) -> &KeyValuePairs {
        &self.track_properties
    }

    /// The ANNOUNCE it accepts, draft 7 to 10 only.
    pub fn track_namespace(&self) -> Option<&Namespace> {
        self.track_namespace.as_ref()
    }

    pub fn request_id(&self) -> RequestId {
        self.request_id
    }
}

impl ControlMessage for RequestOkMessage {
    const MESSAGE_IDS: &'static [u64] = &[REQUEST_OK_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "request_ok" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> Result<()> {
        match version.draft() {
            ..=10 => required(self.track_namespace.as_ref(), "the namespace drafts 7 to 10 name")?.to_bytes(b, version)?,
            11..=16 => b.varint(self.request_id.0)?,
            17.. => {}
        }
        if version.draft() >= 15 {
            self.params.to_bytes(b, version)?;
        }
        if version.draft() >= 18 {
            self.track_properties.encode_uncounted(b, version)?;
        }
        Ok(())
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> Result<Self> {
        let mut message = Self::new(RequestId(0));
        match version.draft() {
            ..=10 => message.track_namespace = Some(Namespace::from_bytes(b, version)?),
            11..=16 => message.request_id = RequestId(b.varint()?),
            17.. => {}
        }
        if version.draft() >= 15 {
            message.params = MessageParams::from_bytes(b, version)?;
        }
        if version.draft() >= 18 {
            message.track_properties = KeyValuePairs::decode_to_end(b, version)?;
        }
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::{from_slice, to_vec};
    use crate::control_message::subscribe::LocationFilter;
    use crate::{MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_18};

    #[test]
    fn request_update_ok_carries_largest_object() {
        let version = MOQ_VERSION_DRAFT_16;
        let location = Location { group: 3, object: 4 };
        let bytes = to_vec(&RequestOkMessage::for_request_update(RequestId(2), Some(location)), version).unwrap();
        // type, length, request id, 1 parameter: LARGEST_OBJECT length 2 (3, 4)
        assert_eq!(bytes, [0x07, 0, 6, 2, 1, 0x09, 2, 3, 4]);
        assert_eq!(from_slice::<RequestOkMessage>(&bytes, version).unwrap().largest_location(), Some(location));
    }

    #[test]
    fn draft_14_has_only_the_request_id() {
        let bytes = to_vec(&RequestOkMessage::new(RequestId(2)), MOQ_VERSION_DRAFT_14).unwrap();
        assert_eq!(bytes, [0x07, 0, 1, 2]);
    }

    /// From draft 18 a PUBLISH is answered with a REQUEST_OK, its parameters those of a PUBLISH_OK.
    #[test]
    fn draft_18_publish_ok_is_a_request_ok() {
        let version = MOQ_VERSION_DRAFT_18;
        let publish_ok = PublishOkMessage { filter: LocationFilter::LargestObject, ..PublishOkMessage::new(RequestId(0)) };
        let request_ok: RequestOkMessage = from_slice(&to_vec(&publish_ok, version).unwrap(), version).unwrap();
        assert_eq!(request_ok.into_publish_ok(version).unwrap(), publish_ok);
    }
}
