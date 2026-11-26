use crate::prelude::*;
use crate::bytes::{FromBytes, ToBytes};
use crate::control_message::ControlMessage;
use crate::error::{Error, Result};
use crate::location::Location;
use crate::message_parameters::{params_as_properties, properties_as_params, MessageParams, Param};
use crate::property::{DEFAULT_PUBLISHER_GROUP_ORDER, MAX_CACHE_DURATION};
use crate::{Frame, GroupOrder, KeyValuePair, KeyValuePairs, Reader, RequestId, TrackAlias, Version, Writer, SUBSCRIBE_OK_MESSAGE_ID};

/// SUBSCRIBE_OK (draft-ietf-moq-transport-19 §10.7).
#[derive(Debug, Eq, PartialEq)]
pub struct SubscribeOkMessage {
    /// 0 from draft 17, where the request's stream names it
    pub(crate) request_id: RequestId,
    /// Drafts 7 to 11 don't repeat the subscriber's from SUBSCRIBE: 0 there until resolved
    track_alias: TrackAlias,
    /// EXPIRES in milliseconds, 0 for never
    expires: u64,
    largest_location: Option<Location>,
    track_properties: KeyValuePairs,
}

impl SubscribeOkMessage {
    /// Accepting `request_id` as `track_alias`, the subscriber's from SUBSCRIBE until draft 11, the publisher's after.
    pub fn new(request_id: RequestId, track_alias: TrackAlias, largest_location: Option<Location>, max_cache_duration: Option<u64>) -> Self {
        let track_properties = max_cache_duration.map(|ms| KeyValuePair::Varint(MAX_CACHE_DURATION, ms)).into_iter().collect();
        Self { request_id, track_alias, expires: 0, largest_location, track_properties }
    }

    pub fn request_id(&self) -> RequestId {
        self.request_id
    }

    pub fn with_track_properties(self, track_properties: KeyValuePairs) -> Self {
        Self { track_properties, ..self }
    }

    pub fn track_alias(&self) -> TrackAlias {
        self.track_alias
    }

    /// Names the subscriber's alias, which drafts 7 to 11 leave out.
    pub fn set_track_alias(&mut self, track_alias: TrackAlias) {
        self.track_alias = track_alias;
    }

    pub fn expires(&self) -> u64 {
        self.expires
    }

    pub fn largest_location(&self) -> Option<Location> {
        self.largest_location
    }

    pub fn track_properties(&self) -> &KeyValuePairs {
        &self.track_properties
    }

    /// The publisher's order, DEFAULT_PUBLISHER_GROUP_ORDER.
    pub fn group_order(&self) -> Option<GroupOrder> {
        GroupOrder::try_from(self.track_properties.get_varint(DEFAULT_PUBLISHER_GROUP_ORDER)?).ok()
    }

    /// MAX_CACHE_DURATION in milliseconds.
    pub fn max_cache_duration(&self) -> Option<u64> {
        self.track_properties.get_varint(MAX_CACHE_DURATION)
    }
}

impl ControlMessage for SubscribeOkMessage {
    const MESSAGE_IDS: &'static [u64] = &[SUBSCRIBE_OK_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "subscribe_ok" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> Result<()> {
        if !version.stream_per_request() {
            b.varint(self.request_id.0)?;
        }
        if version.draft() >= 12 {
            b.varint(self.track_alias.0)?;
        }
        let mut params = MessageParams::new();
        if version.draft() <= 14 {
            // Expires, Group Order, Content Exists and Largest Location as fields
            b.varint(self.expires)?;
            b.u8(self.group_order().unwrap_or(GroupOrder::Ascending) as u8);
            match &self.largest_location {
                Some(location) => {
                    b.u8(1);
                    location.to_bytes(b, version)?;
                }
                None => b.u8(0),
            }
        } else {
            if self.expires != 0 {
                params.put_varint(Param::Expires, self.expires);
            }
            if let Some(location) = self.largest_location {
                params.put_location(Param::LargestObject, location);
            }
        }
        if version.draft() <= 15 {
            properties_as_params(&self.track_properties, &mut params, version);
        }
        params.to_bytes(b, version)?;
        // Track Extensions in draft 16, Track Properties after
        if version.draft() >= 16 {
            self.track_properties.encode_uncounted(b, version)?;
        }
        Ok(())
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> Result<Self> {
        let request_id = match version.stream_per_request() {
            true => RequestId(0),
            false => RequestId(b.varint()?),
        };
        let track_alias = TrackAlias(if version.draft() >= 12 { b.varint()? } else { 0 });
        let mut track_properties = KeyValuePairs::new();
        let (expires, largest_location, mut params) = if version.draft() <= 14 {
            let expires = b.varint()?;
            let order = GroupOrder::try_from(u64::from(b.u8()?))?;
            track_properties.insert(KeyValuePair::Varint(DEFAULT_PUBLISHER_GROUP_ORDER, order.into()));
            let largest_location = match b.u8()? {
                0 => None,
                1 => Some(Location::from_bytes(b, version)?),
                other => return Err(Error::ProtocolViolation(format!("Content Exists {other}"))),
            };
            (expires, largest_location, MessageParams::from_bytes(b, version)?)
        } else {
            let mut params = MessageParams::from_bytes(b, version)?;
            (params.take_varint(Param::Expires).unwrap_or(0), params.take_location(Param::LargestObject), params)
        };
        if version.draft() <= 15 {
            params_as_properties(&mut params, &mut track_properties, version)?;
        }
        params.finish(version)?;
        if version.draft() >= 16 {
            track_properties = KeyValuePairs::decode_to_end(b, version)?;
        }
        Ok(Self { request_id, track_alias, expires, largest_location, track_properties })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::{from_slice, to_vec};
    use crate::{MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_17, MOQ_VERSION_DRAFT_18};
    use rstest::rstest;

    fn subscribe_ok() -> SubscribeOkMessage {
        let track_properties = [KeyValuePair::Varint(MAX_CACHE_DURATION, 5), KeyValuePair::Varint(DEFAULT_PUBLISHER_GROUP_ORDER, 2)].into_iter().collect();
        SubscribeOkMessage { request_id: RequestId(4), track_alias: TrackAlias(1), expires: 0, largest_location: Some(Location { group: 1, object: 2 }), track_properties }
    }

    #[rstest]
    #[case(MOQ_VERSION_DRAFT_14)]
    #[case(MOQ_VERSION_DRAFT_15)]
    #[case(MOQ_VERSION_DRAFT_16)]
    fn round_trips(#[case] version: Version) {
        assert_eq!(from_slice::<SubscribeOkMessage>(&to_vec(&subscribe_ok(), version).unwrap(), version).unwrap(), subscribe_ok());
    }

    #[rstest]
    #[case(MOQ_VERSION_DRAFT_17)]
    #[case(MOQ_VERSION_DRAFT_18)]
    fn round_trips_without_request_id(#[case] version: Version) {
        let decoded: SubscribeOkMessage = from_slice(&to_vec(&subscribe_ok(), version).unwrap(), version).unwrap();
        assert_eq!(decoded, SubscribeOkMessage { request_id: RequestId(0), ..subscribe_ok() });
    }

    /// draft-ietf-moq-transport-16 §9.8: parameters, then the Track Extensions to the end.
    #[test]
    fn draft_16_track_extensions_follow_the_parameters() {
        let bytes = to_vec(&subscribe_ok(), MOQ_VERSION_DRAFT_16).unwrap();
        // type, length, request id, alias, 1 parameter: LARGEST_OBJECT (1, 2) | MAX_CACHE_DURATION 0x04 5, DEFAULT_PUBLISHER_GROUP_ORDER 0x22 - 0x04 2
        assert_eq!(bytes, [0x04, 0, 11, 4, 1, 1, 0x09, 2, 1, 2, 0x04, 5, 0x1E, 2]);
    }

    /// draft-ietf-moq-transport-15 §9.8: no fields but Request ID and Track Alias.
    #[test]
    fn draft_15_has_only_parameters() {
        let bytes = to_vec(&subscribe_ok(), MOQ_VERSION_DRAFT_15).unwrap();
        // type, length, request id, alias, 3 parameters: MAX_CACHE_DURATION 0x04 5, LARGEST_OBJECT 0x09 - 0x04 (1, 2), GROUP_ORDER 0x22 - 0x09 2
        assert_eq!(bytes, [0x04, 0, 11, 4, 1, 3, 0x04, 5, 0x05, 2, 1, 2, 0x19, 2]);
    }

    #[test]
    fn content_exists_over_1_is_protocol_violation() {
        // request id, alias, expires, group order, content exists 2
        let bytes = [0x04, 0, 5, 4, 1, 0, 1, 2];
        assert!(matches!(from_slice::<SubscribeOkMessage>(&bytes, MOQ_VERSION_DRAFT_14), Err(Error::ProtocolViolation(_))));
    }
}
