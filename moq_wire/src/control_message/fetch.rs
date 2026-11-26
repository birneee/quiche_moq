use crate::prelude::*;
use crate::bytes::{FromBytes, ToBytes};
use crate::control_message::{decode_request_id, encode_request_id, until_draft, ControlMessage, RequestErrorMessage};
use crate::error::{Error, Result};
use crate::message_parameters::{MessageParams, Param};
use crate::range_filter::RangeFilters;
use crate::{Frame, GroupOrder, KeyValuePairs, Location, NamespaceTrackname, ReasonPhrase, Reader, RequestErrorCode, RequestId, Version, Writer};
use crate::{DEFAULT_SUBSCRIBER_PRIORITY, FETCH_CANCEL_MESSAGE_ID, FETCH_ERROR_MESSAGE_ID, FETCH_MESSAGE_ID, FETCH_OK_MESSAGE_ID};

const STANDALONE: u64 = 0x1;
const RELATIVE_JOINING: u64 = 0x2;
const ABSOLUTE_JOINING: u64 = 0x3;

/// Subscriber Priority and Group Order, fields until draft 14.
type Fields = (u8, Option<GroupOrder>);

/// The Objects a Standalone FETCH asks for (draft-ietf-moq-transport-19 §10.12.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchRange {
    pub start: Location,
    /// the last Location plus one, its Object 0 the whole `end` group
    pub end: Location,
}

impl FetchRange {
    pub fn contains(&self, location: Location) -> bool {
        location >= self.start && match self.end.object {
            0 => location.group <= self.end.group,
            _ => location < self.end,
        }
    }

    /// FETCH_OK's End Location: `end`, or past `largest` if the range reaches beyond (§10.13).
    pub fn end_location(&self, largest: Location) -> Location {
        let beyond = match self.end.object {
            0 => self.end.group >= largest.group,
            object => Location { group: self.end.group, object: object - 1 } > largest,
        };
        if beyond { largest.next() } else { self.end }
    }
}

/// What a FETCH asks for (draft-ietf-moq-transport-19 §10.12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchKind {
    Standalone { namespace_trackname: NamespaceTrackname, range: FetchRange },
    /// From `groups_before` groups before the joined subscription's Joining Location up to it, draft 8 on
    RelativeJoining { joining_request_id: RequestId, groups_before: u64 },
    /// From `start_group` up to the joined subscription's Joining Location, draft 12 on
    AbsoluteJoining { joining_request_id: RequestId, start_group: u64 },
}

/// FETCH (draft-ietf-moq-transport-19 §10.12), subscriber priority and group order as fields before draft 15.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchMessage {
    pub request_id: RequestId,
    pub kind: FetchKind,
    pub subscriber_priority: u8,
    /// `None` for the publisher's
    pub group_order: Option<GroupOrder>,
    /// draft 19 on
    pub range_filters: RangeFilters,
}

impl FetchMessage {
    pub fn new(request_id: RequestId, kind: FetchKind) -> Self {
        Self { request_id, kind, subscriber_priority: DEFAULT_SUBSCRIBER_PRIORITY, group_order: None, range_filters: RangeFilters::default() }
    }

    fn encode_fields(&self, b: &mut Writer) {
        b.u8(self.subscriber_priority);
        b.u8(GroupOrder::requested_id(self.group_order));
    }

    fn decode_fields(b: &mut Reader<'_, Frame>) -> Result<Fields> {
        Ok((b.u8()?, GroupOrder::from_requested(b.u8()?)?))
    }

    fn encode_kind(&self, b: &mut Writer, version: Version) -> Result<()> {
        let draft = version.draft();
        let ty = match self.kind {
            FetchKind::Standalone { .. } => STANDALONE,
            FetchKind::RelativeJoining { .. } if draft >= 8 => RELATIVE_JOINING,
            FetchKind::AbsoluteJoining { .. } if draft >= 12 => ABSOLUTE_JOINING,
            _ => return Err(Error::Unsupported(format!("this Joining FETCH in draft {draft}"))),
        };
        if draft >= 8 {
            b.varint(ty)?;
        }
        match &self.kind {
            FetchKind::Standalone { namespace_trackname, range } => {
                namespace_trackname.to_bytes(b, version)?;
                if draft == 7 {
                    self.encode_fields(b);
                }
                range.start.to_bytes(b, version)?;
                range.end.to_bytes(b, version)
            }
            FetchKind::RelativeJoining { joining_request_id, groups_before: start } | FetchKind::AbsoluteJoining { joining_request_id, start_group: start } => {
                b.varint(joining_request_id.0)?;
                Ok(b.varint(*start)?)
            }
        }
    }

    /// The kind, and in draft 7 the fields after its track.
    fn decode_kind(b: &mut Reader<'_, Frame>, version: Version) -> Result<(FetchKind, Option<Fields>)> {
        let draft = version.draft();
        let ty = if draft >= 8 { b.varint()? } else { STANDALONE };
        Ok(match ty {
            STANDALONE => {
                let namespace_trackname = NamespaceTrackname::from_bytes(b, version)?;
                let fields = (draft == 7).then(|| Self::decode_fields(b)).transpose()?;
                let range = FetchRange { start: Location::from_bytes(b, version)?, end: Location::from_bytes(b, version)? };
                (FetchKind::Standalone { namespace_trackname, range }, fields)
            }
            RELATIVE_JOINING => (FetchKind::RelativeJoining { joining_request_id: RequestId(b.varint()?), groups_before: b.varint()? }, None),
            ABSOLUTE_JOINING if draft >= 12 => (FetchKind::AbsoluteJoining { joining_request_id: RequestId(b.varint()?), start_group: b.varint()? }, None),
            ty => return Err(Error::ProtocolViolation(format!("Fetch Type {ty}"))),
        })
    }
}

impl ControlMessage for FetchMessage {
    const MESSAGE_IDS: &'static [u64] = &[FETCH_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "fetch" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> Result<()> {
        encode_request_id(b, self.request_id, version)?;
        let mut params = MessageParams::new();
        match version.draft() {
            // after the track (draft-ietf-moq-transport-07 §6.7)
            7 => {}
            8..=14 => self.encode_fields(b),
            _ => {
                if self.subscriber_priority != DEFAULT_SUBSCRIBER_PRIORITY {
                    params.put_u8(Param::SubscriberPriority, self.subscriber_priority);
                }
                if let Some(order) = self.group_order {
                    params.put_u8(Param::GroupOrder, order as u8);
                }
            }
        }
        self.encode_kind(b, version)?;
        if version.draft() >= 19 {
            self.range_filters.put(&mut params);
        }
        params.to_bytes(b, version)
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> Result<Self> {
        let request_id = decode_request_id(b, version)?;
        let fields = (8..=14).contains(&version.draft()).then(|| Self::decode_fields(b)).transpose()?;
        let (kind, draft_7_fields) = Self::decode_kind(b, version)?;
        let mut params = MessageParams::from_bytes(b, version)?;
        let (subscriber_priority, group_order) = match fields.or(draft_7_fields) {
            Some(fields) => fields,
            None => (
                params.take_u8(Param::SubscriberPriority).unwrap_or(DEFAULT_SUBSCRIBER_PRIORITY),
                params.take_u8(Param::GroupOrder).map(|order| GroupOrder::try_from(u64::from(order))).transpose()?,
            ),
        };
        let range_filters = RangeFilters::take(&mut params);
        // TODO the upstream fill budget (§10.2.5), implement when needed
        params.ignore(&[Param::FillTimeout]);
        // TODO authorization, until then tokens are ignored
        params.take_all_bytes(Param::AuthorizationToken);
        params.finish(version)?;
        Ok(Self { request_id, kind, subscriber_priority, group_order, range_filters })
    }
}

/// FETCH_OK (draft-ietf-moq-transport-19 §10.13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOkMessage {
    /// 0 from draft 17, where the request's stream names it
    pub(crate) request_id: RequestId,
    pub end_of_track: bool,
    /// the last Location plus one, its Object 0 the whole end group
    pub end_location: Location,
    /// the order objects are sent in, until draft 14
    pub group_order: GroupOrder,
    /// Track Extensions in draft 16, Track Properties from 17
    pub track_properties: KeyValuePairs,
}

impl FetchOkMessage {
    pub fn new(request_id: RequestId, end_of_track: bool, end_location: Location, group_order: GroupOrder) -> Self {
        Self { request_id, end_of_track, end_location, group_order, track_properties: KeyValuePairs::new() }
    }

    pub fn request_id(&self) -> RequestId {
        self.request_id
    }
}

impl ControlMessage for FetchOkMessage {
    const MESSAGE_IDS: &'static [u64] = &[FETCH_OK_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "fetch_ok" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> Result<()> {
        if !version.stream_per_request() {
            b.varint(self.request_id.0)?;
        }
        if version.draft() <= 14 {
            b.u8(self.group_order as u8);
        }
        b.u8(self.end_of_track.into());
        self.end_location.to_bytes(b, version)?;
        MessageParams::new().to_bytes(b, version)?;
        if version.draft() >= 16 {
            self.track_properties.encode_uncounted(b, version)?;
        }
        Ok(())
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> Result<Self> {
        let request_id = if version.stream_per_request() { RequestId(0) } else { RequestId(b.varint()?) };
        let group_order = if version.draft() <= 14 { GroupOrder::try_from(u64::from(b.u8()?))? } else { GroupOrder::Ascending };
        let end_of_track = match b.u8()? {
            0 => false,
            1 => true,
            other => return Err(Error::ProtocolViolation(format!("End Of Track {other}"))),
        };
        let end_location = Location::from_bytes(b, version)?;
        MessageParams::from_bytes(b, version)?.finish(version)?;
        let track_properties = if version.draft() >= 16 { KeyValuePairs::decode_to_end(b, version)? } else { KeyValuePairs::new() };
        Ok(Self { request_id, end_of_track, end_location, group_order, track_properties })
    }
}

/// FETCH_ERROR until draft 14, REQUEST_ERROR after.
#[derive(Debug)]
pub struct FetchErrorMessage {
    pub(crate) request_id: RequestId,
    error_code: RequestErrorCode,
    error_reason: ReasonPhrase,
}

impl FetchErrorMessage {
    pub fn new(request_id: RequestId, error_code: RequestErrorCode) -> Self {
        Self { request_id, error_code, error_reason: ReasonPhrase::default() }
    }

    pub fn request_id(&self) -> RequestId {
        self.request_id
    }

    pub fn into_request_error(self) -> RequestErrorMessage {
        RequestErrorMessage::new(self.request_id, self.error_code).with_reason(self.error_reason)
    }
}

impl ControlMessage for FetchErrorMessage {
    const MESSAGE_IDS: &'static [u64] = &[FETCH_ERROR_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "fetch_error" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> Result<()> {
        until_draft(14, "FETCH_ERROR", version, false)?;
        b.varint(self.request_id.0)?;
        b.varint(self.error_code.into())?;
        self.error_reason.encode(b)?;
        Ok(())
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> Result<Self> {
        until_draft(14, "FETCH_ERROR", version, true)?;
        Ok(Self { request_id: RequestId(b.varint()?), error_code: RequestErrorCode::from(b.varint()?), error_reason: ReasonPhrase::decode(b)? })
    }
}

/// FETCH_CANCEL until draft 16, cancelling the request's stream from draft 17.
#[derive(Debug)]
pub struct FetchCancelMessage {
    pub request_id: RequestId,
}

impl ControlMessage for FetchCancelMessage {
    const MESSAGE_IDS: &'static [u64] = &[FETCH_CANCEL_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "fetch_cancel" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> Result<()> {
        until_draft(16, "FETCH_CANCEL", version, false)?;
        Ok(b.varint(self.request_id.0)?)
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> Result<Self> {
        until_draft(16, "FETCH_CANCEL", version, true)?;
        Ok(Self { request_id: RequestId(b.varint()?) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::{from_slice, to_vec};
    use crate::{MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_17, MOQ_VERSION_DRAFT_19};
    use rstest::rstest;

    fn at(group: u64, object: u64) -> Location {
        Location { group, object }
    }

    #[rstest]
    #[case::whole_end_group(at(3, 0), at(3, 100), true)]
    #[case::past_the_end_group(at(3, 0), at(4, 0), false)]
    #[case::before_end(at(3, 5), at(3, 4), true)]
    #[case::at_end(at(3, 5), at(3, 5), false)]
    #[case::before_start(at(3, 5), at(0, 9), false)]
    fn range_contains(#[case] end: Location, #[case] location: Location, #[case] contained: bool) {
        assert_eq!(FetchRange { start: at(1, 0), end }.contains(location), contained);
    }

    #[rstest]
    #[case::whole_group_reaching_beyond(at(3, 0), at(2, 7), at(2, 8))]
    #[case::whole_group_within(at(3, 0), at(5, 0), at(3, 0))]
    #[case::object_reaching_beyond(at(3, 5), at(3, 2), at(3, 3))]
    #[case::object_within(at(3, 5), at(3, 4), at(3, 5))]
    fn range_end_location(#[case] end: Location, #[case] largest: Location, #[case] expected: Location) {
        assert_eq!(FetchRange { start: at(1, 0), end }.end_location(largest), expected);
    }

    fn standalone() -> FetchKind {
        FetchKind::Standalone { namespace_trackname: "n--t".parse().unwrap(), range: FetchRange { start: Location { group: 1, object: 0 }, end: Location { group: 3, object: 0 } } }
    }

    #[rstest]
    #[case(MOQ_VERSION_DRAFT_07, standalone())]
    #[case(MOQ_VERSION_DRAFT_11, FetchKind::RelativeJoining { joining_request_id: RequestId(0), groups_before: 2 })]
    #[case(MOQ_VERSION_DRAFT_14, FetchKind::AbsoluteJoining { joining_request_id: RequestId(0), start_group: 4 })]
    #[case(MOQ_VERSION_DRAFT_16, standalone())]
    #[case(MOQ_VERSION_DRAFT_19, FetchKind::RelativeJoining { joining_request_id: RequestId(0), groups_before: 2 })]
    fn fetch_round_trips(#[case] version: Version, #[case] kind: FetchKind) {
        let fetch = FetchMessage { subscriber_priority: 9, group_order: Some(GroupOrder::Descending), ..FetchMessage::new(RequestId(2), kind) };
        assert_eq!(from_slice::<FetchMessage>(&to_vec(&fetch, version).unwrap(), version).unwrap(), fetch);
    }

    #[rstest]
    #[case(MOQ_VERSION_DRAFT_11)]
    #[case(MOQ_VERSION_DRAFT_16)]
    #[case(MOQ_VERSION_DRAFT_17)]
    fn fetch_ok_round_trips(#[case] version: Version) {
        let ok = FetchOkMessage::new(RequestId(2), true, Location { group: 3, object: 1 }, GroupOrder::Ascending);
        let decoded = from_slice::<FetchOkMessage>(&to_vec(&ok, version).unwrap(), version).unwrap();
        let request_id = if version.stream_per_request() { RequestId(0) } else { RequestId(2) };
        assert_eq!(decoded, FetchOkMessage { request_id, ..ok });
    }

    /// draft-ietf-moq-transport-19 §10.12.1: Request ID, Fetch Type, the track, Start and End Location, no parameters.
    #[test]
    fn draft_19_standalone_fetch() {
        let bytes = to_vec(&FetchMessage::new(RequestId(2), standalone()), MOQ_VERSION_DRAFT_19).unwrap();
        assert_eq!(bytes, [0x16, 0, 12, 2, 1, 1, 1, b'n', 1, b't', 1, 0, 3, 0, 0]);
    }

    /// draft-ietf-moq-transport-19 §10.2.5: FILL_TIMEOUT 0x0A, a varint.
    #[test]
    fn draft_19_fetch_takes_fill_timeout() {
        let bytes = [0x16, 0, 14, 2, 1, 1, 1, b'n', 1, b't', 1, 0, 3, 0, 1, 0x0A, 50];
        assert_eq!(from_slice::<FetchMessage>(&bytes, MOQ_VERSION_DRAFT_19).unwrap(), FetchMessage::new(RequestId(2), standalone()));
    }
}
