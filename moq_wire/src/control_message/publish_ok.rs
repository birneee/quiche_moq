use crate::prelude::*;
use crate::bytes::{from_slice, to_vec, FromBytes, ToBytes};
use crate::control_message::subscribe::{forward_state, LocationFilter};
use crate::control_message::ControlMessage;
use crate::error::{Error, Result};
use crate::message_parameters::{MessageParams, Param};
use crate::range_filter::RangeFilters;
use crate::{Frame, GroupOrder, KeyValuePairs, Location, Reader, RequestId, Version, Writer};
use crate::{DEFAULT_SUBSCRIBER_PRIORITY, PUBLISH_OK_MESSAGE_ID, REQUEST_OK_MESSAGE_ID};

/// The filter of a PUBLISH_OK without one: unfiltered, the same as AbsoluteStart at 0 (draft-ietf-moq-transport-19 §5.1.2).
const UNFILTERED: LocationFilter = LocationFilter::AbsoluteStart { start: Location { group: 0, object: 0 } };

/// PUBLISH_OK (draft-ietf-moq-transport-19 §10.5), a REQUEST_OK from draft 18.
/// Values equal to what an omitted parameter means are left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishOkMessage {
    /// 0 from draft 17, where the request's stream names it
    pub request_id: RequestId,
    /// The Forward State
    pub forward: bool,
    pub subscriber_priority: u8,
    /// up to draft 18, SUBSCRIBE_TRACKS carries it in draft 19
    pub group_order: Option<GroupOrder>,
    pub filter: LocationFilter,
    /// OBJECT_DELIVERY_TIMEOUT in milliseconds
    pub object_delivery_timeout: Option<u64>,
    /// SUBGROUP_DELIVERY_TIMEOUT in milliseconds, draft 18 on
    pub subgroup_delivery_timeout: Option<u64>,
    /// EXPIRES in milliseconds, 0 for never, draft 15 on
    pub expires: u64,
    /// NEW_GROUP_REQUEST, draft 15 on
    pub new_group_request: Option<u64>,
    /// draft 19 on
    pub range_filters: RangeFilters,
}

impl PublishOkMessage {
    /// Accepting as an omitted parameter would.
    pub fn new(request_id: RequestId) -> Self {
        Self {
            request_id,
            forward: true,
            subscriber_priority: DEFAULT_SUBSCRIBER_PRIORITY,
            group_order: None,
            filter: UNFILTERED,
            object_delivery_timeout: None,
            subgroup_delivery_timeout: None,
            expires: 0,
            new_group_request: None,
            range_filters: RangeFilters::default(),
        }
    }

    /// The parameters from draft 15, hints a draft lacks are left out.
    fn put_params(&self, params: &mut MessageParams, version: Version) -> Result<()> {
        if let Some(ms) = self.object_delivery_timeout {
            params.put_varint(Param::ObjectDeliveryTimeout, ms);
        }
        if version.draft() < 15 {
            return Ok(());
        }
        if !self.forward {
            params.put_u8(Param::Forward, 0);
        }
        if self.subscriber_priority != DEFAULT_SUBSCRIBER_PRIORITY {
            params.put_u8(Param::SubscriberPriority, self.subscriber_priority);
        }
        if let Some(order) = self.group_order && version.draft() <= 18 {
            params.put_u8(Param::GroupOrder, order as u8);
        }
        if self.filter != UNFILTERED {
            params.put_bytes(Param::LocationFilter, to_vec(&self.filter, version)?);
        }
        if self.expires != 0 {
            params.put_varint(Param::Expires, self.expires);
        }
        for (param, value) in [(Param::SubgroupDeliveryTimeout, self.subgroup_delivery_timeout), (Param::NewGroupRequest, self.new_group_request)] {
            if let Some(v) = value && param.ty(version).is_some() {
                params.put_varint(param, v);
            }
        }
        if version.draft() >= 19 {
            self.range_filters.put(params);
        }
        Ok(())
    }

    /// From the parameters of a PUBLISH_OK, or of a REQUEST_OK answering a PUBLISH from draft 18.
    pub(crate) fn from_params(request_id: RequestId, mut params: MessageParams, version: Version) -> Result<Self> {
        let mut message = Self::new(request_id);
        message.object_delivery_timeout = params.take_varint(Param::ObjectDeliveryTimeout);
        if version.draft() >= 15 {
            if let Some(forward) = params.take_forward()? {
                message.forward = forward;
            }
            message.subscriber_priority = params.take_u8(Param::SubscriberPriority).unwrap_or(DEFAULT_SUBSCRIBER_PRIORITY);
            message.group_order = params.take_u8(Param::GroupOrder).map(|order| GroupOrder::try_from(u64::from(order))).transpose()?;
            if let Some(bytes) = params.take_bytes(Param::LocationFilter) {
                message.filter = from_slice(&bytes, version)?;
            }
            message.expires = params.take_varint(Param::Expires).unwrap_or(0);
            message.subgroup_delivery_timeout = params.take_varint(Param::SubgroupDeliveryTimeout);
            message.new_group_request = params.take_varint(Param::NewGroupRequest);
            message.range_filters = RangeFilters::take(&mut params);
        }
        params.finish(version)?;
        Ok(message)
    }
}

impl ControlMessage for PublishOkMessage {
    const MESSAGE_IDS: &'static [u64] = &[PUBLISH_OK_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "publish_ok" }

    fn message_id_for_version(version: Version) -> u64 {
        if version.draft() >= 18 { REQUEST_OK_MESSAGE_ID } else { PUBLISH_OK_MESSAGE_ID }
    }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> Result<()> {
        if version.draft() < 12 {
            return Err(Error::Unsupported(format!("PUBLISH_OK in draft {}", version.draft())));
        }
        if !version.stream_per_request() {
            b.varint(self.request_id.0)?;
        }
        if version.draft() <= 14 {
            b.u8(self.forward.into());
            b.u8(self.subscriber_priority);
            b.u8(GroupOrder::requested_id(self.group_order));
            self.filter.to_bytes(b, version)?;
        }
        let mut params = MessageParams::new();
        self.put_params(&mut params, version)?;
        params.to_bytes(b, version)?;
        // the Track Properties of a REQUEST_OK, a PUBLISH_OK has none
        if version.draft() >= 18 {
            KeyValuePairs::new().encode_uncounted(b, version)?;
        }
        Ok(())
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> Result<Self> {
        if version.draft() < 12 {
            return Err(Error::ProtocolViolation(format!("PUBLISH_OK in draft {}", version.draft())));
        }
        let request_id = match version.stream_per_request() {
            true => RequestId(0),
            false => RequestId(b.varint()?),
        };
        if version.draft() <= 14 {
            let forward = forward_state(b.u8()?.into())?;
            let subscriber_priority = b.u8()?;
            let group_order = GroupOrder::from_requested(b.u8()?)?;
            let filter = LocationFilter::from_bytes(b, version)?;
            let params = MessageParams::from_bytes(b, version)?;
            return Ok(Self { forward, subscriber_priority, group_order, filter, ..Self::from_params(request_id, params, version)? });
        }
        let message = Self::from_params(request_id, MessageParams::from_bytes(b, version)?, version)?;
        if version.draft() >= 18 && !KeyValuePairs::decode_to_end(b, version)?.is_empty() {
            return Err(Error::ProtocolViolation("Track Properties in a PUBLISH_OK".into()));
        }
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_17, MOQ_VERSION_DRAFT_18};
    use rstest::rstest;

    fn publish_ok() -> PublishOkMessage {
        PublishOkMessage {
            forward: false,
            subscriber_priority: 3,
            filter: LocationFilter::LargestObject,
            object_delivery_timeout: Some(100),
            ..PublishOkMessage::new(RequestId(5))
        }
    }

    #[rstest]
    #[case(MOQ_VERSION_DRAFT_14)]
    #[case(MOQ_VERSION_DRAFT_16)]
    fn round_trips(#[case] version: Version) {
        assert_eq!(from_slice::<PublishOkMessage>(&to_vec(&publish_ok(), version).unwrap(), version).unwrap(), publish_ok());
    }

    #[test]
    fn draft_17_has_no_request_id() {
        let version = MOQ_VERSION_DRAFT_17;
        let decoded: PublishOkMessage = from_slice(&to_vec(&publish_ok(), version).unwrap(), version).unwrap();
        assert_eq!(decoded, PublishOkMessage { request_id: RequestId(0), ..publish_ok() });
    }

    /// draft-ietf-moq-transport-18 §10.5: a REQUEST_OK, PUBLISH_OK's type 0x1E is reserved.
    #[test]
    fn draft_18_is_a_request_ok() {
        // type REQUEST_OK, length, no parameters, no Track Properties
        assert_eq!(to_vec(&PublishOkMessage::new(RequestId(5)), MOQ_VERSION_DRAFT_18).unwrap(), [0x07, 0, 1, 0]);
    }

    /// Accepting with defaults sends no parameters, as before.
    #[test]
    fn defaults_are_left_out() {
        // type, length, request id, no parameters
        assert_eq!(to_vec(&PublishOkMessage::new(RequestId(5)), MOQ_VERSION_DRAFT_16).unwrap(), [0x1E, 0, 2, 5, 0]);
    }
}
