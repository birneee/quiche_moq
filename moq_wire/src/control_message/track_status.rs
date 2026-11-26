use crate::prelude::*;
use crate::bytes::{FromBytes, ToBytes};
use crate::control_message::subscribe::SubscribeMessage;
use crate::control_message::{required, ControlMessage, RequestErrorMessage, SubscribeOkMessage};
use crate::error::{Error, Result};
use crate::message_parameters::{MessageParams, Param};
use crate::{Frame, KeyValuePairs, Location, NamespaceTrackname, ReasonPhrase, Reader, RequestErrorCode, RequestId, TrackAlias, Version, Writer};
use crate::{TRACK_STATUS_ERROR_MESSAGE_ID, TRACK_STATUS_MESSAGE_ID, TRACK_STATUS_OK_MESSAGE_ID};

/// TRACK_STATUS (draft-ietf-moq-transport-19 §10.14), TRACK_STATUS_REQUEST before draft 13.
/// SUBSCRIBE's layout from draft 13, the subscription hints left at their defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackStatusMessage {
    /// From draft 11
    pub request_id: Option<RequestId>,
    pub namespace_trackname: NamespaceTrackname,
}

impl ControlMessage for TrackStatusMessage {
    const MESSAGE_IDS: &'static [u64] = &[TRACK_STATUS_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "track_status" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> Result<()> {
        if version.draft() >= 13 {
            let request_id = required(self.request_id, "the Request ID of TRACK_STATUS")?;
            return SubscribeMessage::new(request_id, self.namespace_trackname.clone()).to_body_bytes(b, version);
        }
        if version.draft() >= 11 {
            b.varint(required(self.request_id, "the Request ID of TRACK_STATUS_REQUEST")?.0)?;
        }
        self.namespace_trackname.to_bytes(b, version)?;
        if version.draft() >= 11 {
            MessageParams::new().to_bytes(b, version)?;
        }
        Ok(())
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> Result<Self> {
        if version.draft() >= 13 {
            let subscribe = SubscribeMessage::from_body_bytes(b, version)?;
            return Ok(Self { request_id: Some(subscribe.request_id), namespace_trackname: subscribe.namespace_trackname });
        }
        let request_id = (version.draft() >= 11).then(|| b.varint().map(RequestId)).transpose()?;
        let namespace_trackname = NamespaceTrackname::from_bytes(b, version)?;
        if version.draft() >= 11 {
            let mut params = MessageParams::from_bytes(b, version)?;
            // TODO authorization, until then tokens are ignored
            params.take_all_bytes(Param::AuthorizationToken);
            params.finish(version)?;
        }
        Ok(Self { request_id, namespace_trackname })
    }
}

/// The Status Code of TRACK_STATUS before draft 13, both its success and its failure.
mod status_code {
    pub(super) const IN_PROGRESS: u64 = 0x00;
    pub(super) const DOES_NOT_EXIST: u64 = 0x01;
    pub(super) const NOT_BEGUN: u64 = 0x02;
    /// a relay that can't obtain the status
    pub(super) const UNKNOWN: u64 = 0x04;
}

/// Before draft 13 the TRACK_STATUS reply: drafts 7 to 10 name the track, 11 and 12 the request.
fn encode_status_reply(b: &mut Writer, version: Version, request_id: RequestId, track: Option<&NamespaceTrackname>, status: u64, largest: Option<Location>) -> Result<()> {
    let largest = largest.unwrap_or(Location { group: 0, object: 0 });
    if version.draft() <= 10 {
        required(track, "the track TRACK_STATUS names before draft 11")?.to_bytes(b, version)?;
        b.varint(status)?;
        return largest.to_bytes(b, version);
    }
    b.varint(request_id.0)?;
    b.varint(status)?;
    largest.to_bytes(b, version)?;
    MessageParams::new().to_bytes(b, version)
}

/// TRACK_STATUS_OK (draft-ietf-moq-transport-19 §10.14): a REQUEST_OK from draft 15, SUBSCRIBE_OK's layout in
/// drafts 13 and 14, a TRACK_STATUS with a success Status Code before.
/// - only answered in drafts before 13, whose reply can't be told from a failure by its type
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackStatusOkMessage {
    pub(crate) request_id: RequestId,
    /// drafts 7 to 10 name the track
    pub namespace_trackname: Option<NamespaceTrackname>,
    pub largest_location: Option<Location>,
    /// drafts 13 and 14
    pub track_properties: KeyValuePairs,
}

impl TrackStatusOkMessage {
    pub fn new(request_id: RequestId, namespace_trackname: NamespaceTrackname, largest_location: Option<Location>, track_properties: KeyValuePairs) -> Self {
        Self { request_id, namespace_trackname: Some(namespace_trackname), largest_location, track_properties }
    }

    pub fn request_id(&self) -> RequestId {
        self.request_id
    }
}

impl ControlMessage for TrackStatusOkMessage {
    const MESSAGE_IDS: &'static [u64] = &[TRACK_STATUS_OK_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "track_status_ok" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> Result<()> {
        match version.draft() {
            ..=12 => {
                let status = if self.largest_location.is_some() { status_code::IN_PROGRESS } else { status_code::NOT_BEGUN };
                encode_status_reply(b, version, self.request_id, self.namespace_trackname.as_ref(), status, self.largest_location)
            }
            13..=14 => SubscribeOkMessage::new(self.request_id, TrackAlias(0), self.largest_location, None)
                .with_track_properties(self.track_properties.clone())
                .to_body_bytes(b, version),
            _ => Err(Error::Unsupported(format!("TRACK_STATUS_OK in draft {}, a REQUEST_OK there", version.draft()))),
        }
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> Result<Self> {
        match version.draft() {
            // we only ask from draft 13
            ..=12 => Err(Error::ProtocolViolation("TRACK_STATUS reply to no TRACK_STATUS_REQUEST".into())),
            13..=14 => {
                let ok = SubscribeOkMessage::from_body_bytes(b, version)?;
                Ok(Self { request_id: ok.request_id(), namespace_trackname: None, largest_location: ok.largest_location(), track_properties: ok.track_properties().clone() })
            }
            _ => Err(Error::ProtocolViolation(format!("TRACK_STATUS_OK type in draft {}", version.draft()))),
        }
    }
}

/// TRACK_STATUS_ERROR of drafts 13 and 14, SUBSCRIBE_ERROR's layout. A TRACK_STATUS with a failure Status Code
/// before, REQUEST_ERROR from draft 15.
#[derive(Debug)]
pub struct TrackStatusErrorMessage {
    pub(crate) request_id: RequestId,
    /// drafts 7 to 10 name the track
    pub namespace_trackname: Option<NamespaceTrackname>,
    error_code: RequestErrorCode,
    error_reason: ReasonPhrase,
}

impl TrackStatusErrorMessage {
    pub fn new(request_id: RequestId, namespace_trackname: NamespaceTrackname, error_code: RequestErrorCode) -> Self {
        Self { request_id, namespace_trackname: Some(namespace_trackname), error_code, error_reason: ReasonPhrase::default() }
    }

    pub fn request_id(&self) -> RequestId {
        self.request_id
    }

    pub fn into_request_error(self) -> RequestErrorMessage {
        RequestErrorMessage::new(self.request_id, self.error_code).with_reason(self.error_reason)
    }
}

impl ControlMessage for TrackStatusErrorMessage {
    const MESSAGE_IDS: &'static [u64] = &[TRACK_STATUS_ERROR_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "track_status_error" }

    fn message_id_for_version(version: Version) -> u64 {
        if version.draft() <= 12 { TRACK_STATUS_OK_MESSAGE_ID } else { TRACK_STATUS_ERROR_MESSAGE_ID }
    }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> Result<()> {
        match version.draft() {
            ..=12 => {
                let status = if self.error_code == RequestErrorCode::DoesNotExist { status_code::DOES_NOT_EXIST } else { status_code::UNKNOWN };
                encode_status_reply(b, version, self.request_id, self.namespace_trackname.as_ref(), status, None)
            }
            13..=14 => RequestErrorMessage::new(self.request_id, self.error_code).with_reason(self.error_reason.clone()).to_body_bytes(b, version),
            _ => Err(Error::Unsupported(format!("TRACK_STATUS_ERROR in draft {}, a REQUEST_ERROR there", version.draft()))),
        }
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> Result<Self> {
        if !(13..=14).contains(&version.draft()) {
            return Err(Error::ProtocolViolation(format!("TRACK_STATUS_ERROR in draft {}", version.draft())));
        }
        let error = RequestErrorMessage::from_body_bytes(b, version)?;
        Ok(Self { request_id: error.request_id(), namespace_trackname: None, error_code: error.error_code(), error_reason: error.error_reason().clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::{from_slice, to_vec};
    use crate::{MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_19};
    use rstest::rstest;

    fn track() -> NamespaceTrackname {
        "n--t".parse().unwrap()
    }

    #[rstest]
    #[case(MOQ_VERSION_DRAFT_07, None)]
    #[case(MOQ_VERSION_DRAFT_11, Some(RequestId(2)))]
    #[case(MOQ_VERSION_DRAFT_14, Some(RequestId(2)))]
    #[case(MOQ_VERSION_DRAFT_16, Some(RequestId(2)))]
    #[case(MOQ_VERSION_DRAFT_19, Some(RequestId(2)))]
    fn request_round_trips(#[case] version: Version, #[case] request_id: Option<RequestId>) {
        let request = TrackStatusMessage { request_id, namespace_trackname: track() };
        assert_eq!(from_slice::<TrackStatusMessage>(&to_vec(&request, version).unwrap(), version).unwrap(), request);
    }

    /// draft-ietf-moq-transport-07 §6.21: the track, Status Code, Last Group and Last Object.
    #[test]
    fn draft_7_reply_names_the_track() {
        let ok = TrackStatusOkMessage::new(RequestId(0), track(), Some(Location { group: 3, object: 4 }), KeyValuePairs::new());
        // type, length, namespace (1 field "n"), track "t", in progress, group, object
        assert_eq!(to_vec(&ok, MOQ_VERSION_DRAFT_07).unwrap(), [0x0E, 8, 1, 1, b'n', 1, b't', 0, 3, 4]);
        let error = TrackStatusErrorMessage::new(RequestId(0), track(), RequestErrorCode::DoesNotExist);
        assert_eq!(to_vec(&error, MOQ_VERSION_DRAFT_07).unwrap(), [0x0E, 8, 1, 1, b'n', 1, b't', 1, 0, 0]);
    }

    #[test]
    fn draft_14_reply_round_trips() {
        let version = MOQ_VERSION_DRAFT_14;
        let ok = TrackStatusOkMessage { namespace_trackname: None, ..TrackStatusOkMessage::new(RequestId(2), track(), Some(Location { group: 3, object: 4 }), KeyValuePairs::new()) };
        let decoded = from_slice::<TrackStatusOkMessage>(&to_vec(&ok, version).unwrap(), version).unwrap();
        assert_eq!((decoded.request_id, decoded.largest_location), (RequestId(2), ok.largest_location));
        let error = TrackStatusErrorMessage::new(RequestId(2), track(), RequestErrorCode::DoesNotExist);
        let decoded = from_slice::<TrackStatusErrorMessage>(&to_vec(&error, version).unwrap(), version).unwrap();
        assert_eq!((decoded.request_id, decoded.error_code), (RequestId(2), RequestErrorCode::DoesNotExist));
    }
}
