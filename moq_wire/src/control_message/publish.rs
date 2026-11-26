use crate::prelude::*;
use crate::bytes::{FromBytes, ToBytes};
use crate::control_message::subscribe::forward_state;
use crate::control_message::{decode_request_id, encode_request_id, ControlMessage};
use crate::error::{Error, Result};
use crate::message_parameters::{params_as_properties, properties_as_params, MessageParams, Param};
use crate::property::DEFAULT_PUBLISHER_GROUP_ORDER;
use crate::{Frame, GroupOrder, KeyValuePair, KeyValuePairs, Location, NamespaceTrackname, Reader, RequestId, TrackAlias, Version, Writer, PUBLISH_MESSAGE_ID};

/// PUBLISH (draft-ietf-moq-transport-19 §10.9) from draft 12.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishMessage {
    pub request_id: RequestId,
    pub namespace_trackname: NamespaceTrackname,
    pub track_alias: TrackAlias,
    /// The Forward State
    pub forward: bool,
    /// EXPIRES in milliseconds, 0 for never, draft 15 on
    pub expires: u64,
    pub largest_location: Option<Location>,
    pub track_properties: KeyValuePairs,
}

impl PublishMessage {
    pub fn new(request_id: RequestId, namespace_trackname: NamespaceTrackname, track_alias: TrackAlias, track_properties: KeyValuePairs) -> Self {
        Self { request_id, namespace_trackname, track_alias, forward: true, expires: 0, largest_location: None, track_properties }
    }

    fn group_order(&self) -> Option<GroupOrder> {
        GroupOrder::try_from(self.track_properties.get_varint(DEFAULT_PUBLISHER_GROUP_ORDER)?).ok()
    }
}

impl ControlMessage for PublishMessage {
    const MESSAGE_IDS: &'static [u64] = &[PUBLISH_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "publish" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> Result<()> {
        if version.draft() < 12 {
            return Err(Error::Unsupported(format!("PUBLISH in draft {}", version.draft())));
        }
        encode_request_id(b, self.request_id, version)?;
        self.namespace_trackname.to_bytes(b, version)?;
        b.varint(self.track_alias.0)?;
        let mut params = MessageParams::new();
        if version.draft() <= 14 {
            // Group Order, Content Exists, Largest and Forward as fields
            b.u8(self.group_order().unwrap_or(GroupOrder::Ascending) as u8);
            match &self.largest_location {
                Some(location) => {
                    b.u8(1);
                    location.to_bytes(b, version)?;
                }
                None => b.u8(0),
            }
            b.u8(self.forward.into());
        } else {
            if !self.forward {
                params.put_u8(Param::Forward, 0);
            }
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
        if version.draft() < 12 {
            return Err(Error::ProtocolViolation(format!("PUBLISH in draft {}", version.draft())));
        }
        let request_id = decode_request_id(b, version)?;
        let namespace_trackname = NamespaceTrackname::from_bytes(b, version)?;
        let track_alias = TrackAlias(b.varint()?);
        let mut message = Self::new(request_id, namespace_trackname, track_alias, KeyValuePairs::new());
        if version.draft() <= 14 {
            let order = GroupOrder::try_from(u64::from(b.u8()?))?;
            message.track_properties.insert(KeyValuePair::Varint(DEFAULT_PUBLISHER_GROUP_ORDER, order.into()));
            message.largest_location = match b.u8()? {
                0 => None,
                1 => Some(Location::from_bytes(b, version)?),
                other => return Err(Error::ProtocolViolation(format!("ContentExists {other}"))),
            };
            message.forward = forward_state(b.u8()?.into())?;
        }
        let mut params = MessageParams::from_bytes(b, version)?;
        if version.draft() >= 15 {
            message.forward = params.take_forward()?.unwrap_or(true);
            message.expires = params.take_varint(Param::Expires).unwrap_or(0);
            message.largest_location = params.take_location(Param::LargestObject);
        }
        if version.draft() <= 15 {
            params_as_properties(&mut params, &mut message.track_properties, version)?;
        }
        // TODO authorization, until then tokens are ignored
        params.take_all_bytes(Param::AuthorizationToken);
        params.finish(version)?;
        if version.draft() >= 16 {
            message.track_properties = KeyValuePairs::decode_to_end(b, version)?;
        }
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::{from_slice, to_vec};
    use crate::property::MAX_CACHE_DURATION;
    use crate::{MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_18};
    use rstest::rstest;

    fn publish() -> PublishMessage {
        let properties = [KeyValuePair::Varint(MAX_CACHE_DURATION, 9), KeyValuePair::Varint(DEFAULT_PUBLISHER_GROUP_ORDER, 2)].into_iter().collect();
        PublishMessage { forward: false, largest_location: Some(Location { group: 3, object: 1 }), ..PublishMessage::new(RequestId(1), "n--t".parse().unwrap(), TrackAlias(7), properties) }
    }

    #[rstest]
    #[case(MOQ_VERSION_DRAFT_12)]
    #[case(MOQ_VERSION_DRAFT_15)]
    #[case(MOQ_VERSION_DRAFT_16)]
    #[case(MOQ_VERSION_DRAFT_18)]
    fn round_trips(#[case] version: Version) {
        assert_eq!(from_slice::<PublishMessage>(&to_vec(&publish(), version).unwrap(), version).unwrap(), publish());
    }

    /// draft-ietf-moq-transport-16 §9.13: the Track Extensions run to the end, without a count.
    #[test]
    fn draft_16_track_extensions_are_uncounted() {
        let message = PublishMessage::new(RequestId(1), "n--t".parse().unwrap(), TrackAlias(7), [KeyValuePair::Varint(MAX_CACHE_DURATION, 9)].into_iter().collect());
        // type, length, request id, namespace, track name, alias, no parameters | MAX_CACHE_DURATION 0x04 9
        assert_eq!(to_vec(&message, MOQ_VERSION_DRAFT_16).unwrap(), [0x1D, 0, 10, 1, 1, 1, b'n', 1, b't', 7, 0, 0x04, 9]);
    }
}
