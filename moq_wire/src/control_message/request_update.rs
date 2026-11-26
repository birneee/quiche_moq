use crate::prelude::*;
use crate::bytes::{from_slice, to_vec, FromBytes, ToBytes};
use crate::control_message::subscribe::{forward_state, LocationFilter};
use crate::range_filter::RangeFilters;
use crate::control_message::{decode_request_id, encode_request_id, required, ControlMessage};
use crate::error::Error;
use crate::message_parameters::{MessageParams, Param};
use crate::{Frame, Location, Reader, RequestId, Version, Writer, REQUEST_UPDATE_MESSAGE_ID};

/// REQUEST_UPDATE (draft-ietf-moq-transport-19 §10.9), SUBSCRIBE_UPDATE before draft 15.
/// From draft 15 every `None` stays as it is, before the update names the whole subscription: priority,
/// from draft 11 the Forward State, and an absolute filter; it has no reply then.
#[derive(Debug)]
pub struct RequestUpdateMessage {
    /// From draft 14, an update has none of its own before
    pub request_id: Option<RequestId>,
    /// The request it updates, 0 from draft 17 until resolved, where the request's stream names it
    pub existing_request_id: RequestId,
    pub subscriber_priority: Option<u8>,
    /// The Forward State, always 1 before draft 11
    pub forward: Option<bool>,
    pub filter: Option<LocationFilter>,
    /// OBJECT_DELIVERY_TIMEOUT in milliseconds
    pub object_delivery_timeout: Option<u64>,
    /// SUBGROUP_DELIVERY_TIMEOUT in milliseconds, draft 18 on
    pub subgroup_delivery_timeout: Option<u64>,
    pub new_group_request: Option<u64>,
    pub range_filters: RangeFilters,
}

impl RequestUpdateMessage {
    /// Changing nothing yet.
    pub fn new(request_id: Option<RequestId>, existing_request_id: RequestId) -> Self {
        Self {
            request_id,
            existing_request_id,
            subscriber_priority: None,
            forward: None,
            filter: None,
            object_delivery_timeout: None,
            subgroup_delivery_timeout: None,
            new_group_request: None,
            range_filters: RangeFilters::default(),
        }
    }

    /// Drafts 7 to 14: the fields of SUBSCRIBE_UPDATE, the parameters follow.
    fn encode_subscribe_update(&self, b: &mut Writer, version: Version) -> crate::Result<()> {
        let draft = version.draft();
        if draft == 14 {
            b.varint(required(self.request_id, "the Request ID of a draft 14 SUBSCRIBE_UPDATE")?.0)?;
        }
        b.varint(self.existing_request_id.0)?;
        let (start, end_group) = match self.filter {
            Some(LocationFilter::AbsoluteStart { start }) => (start, 0),
            Some(LocationFilter::AbsoluteRange { start, end_group }) => (start, end_group + 1),
            _ => return Err(Error::Unencodable("SUBSCRIBE_UPDATE needs an absolute filter before draft 15")),
        };
        b.varint(start.group)?;
        b.varint(start.object)?;
        // End Group plus 1, 0 for none
        b.varint(end_group)?;
        if draft == 7 {
            // End Object plus 1, 0 for the whole group
            b.varint(0)?;
        }
        b.u8(required(self.subscriber_priority, "the priority of a SUBSCRIBE_UPDATE")?);
        match self.forward {
            Some(forward) if draft >= 11 => b.u8(forward.into()),
            None if draft >= 11 => return Err(Error::Unencodable("the Forward State of a SUBSCRIBE_UPDATE")),
            Some(false) => return Err(Error::Unsupported(format!("Forward State 0 in draft {draft}"))),
            _ => {}
        }
        Ok(())
    }

    fn decode_subscribe_update(b: &mut Reader<'_, Frame>, version: Version) -> crate::Result<Self> {
        let draft = version.draft();
        let request_id = (draft == 14).then(|| b.varint().map(RequestId)).transpose()?;
        let existing_request_id = RequestId(b.varint()?);
        let start = Location { group: b.varint()?, object: b.varint()? };
        let filter = match b.varint()? {
            0 => LocationFilter::AbsoluteStart { start },
            end => LocationFilter::AbsoluteRange { start, end_group: end - 1 },
        };
        if draft == 7 {
            // TODO an end within the group, until then the whole group is delivered
            b.varint()?;
        }
        let subscriber_priority = b.u8()?;
        let forward = if draft >= 11 { forward_state(b.u8()?.into())? } else { true };
        Ok(Self { subscriber_priority: Some(subscriber_priority), forward: Some(forward), filter: Some(filter), ..Self::new(request_id, existing_request_id) })
    }
}

impl ControlMessage for RequestUpdateMessage {
    const MESSAGE_IDS: &'static [u64] = &[REQUEST_UPDATE_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "request_update" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> crate::Result<()> {
        let mut params = MessageParams::new();
        if let Some(ms) = self.object_delivery_timeout && Param::ObjectDeliveryTimeout.ty(version).is_some() {
            params.put_varint(Param::ObjectDeliveryTimeout, ms);
        }
        if !version.request_update_has_parameters() {
            self.encode_subscribe_update(b, version)?;
            return params.to_bytes(b, version);
        }
        encode_request_id(b, required(self.request_id, "the Request ID of a REQUEST_UPDATE")?, version)?;
        if !version.stream_per_request() {
            b.varint(self.existing_request_id.0)?;
        }
        if let Some(priority) = self.subscriber_priority {
            params.put_u8(Param::SubscriberPriority, priority);
        }
        if let Some(forward) = self.forward {
            params.put_u8(Param::Forward, forward.into());
        }
        if let Some(filter) = &self.filter {
            params.put_bytes(Param::LocationFilter, to_vec(filter, version)?);
        }
        // hints a draft lacks are left out
        for (param, value) in [(Param::SubgroupDeliveryTimeout, self.subgroup_delivery_timeout), (Param::NewGroupRequest, self.new_group_request)] {
            if let Some(v) = value && param.ty(version).is_some() {
                params.put_varint(param, v);
            }
        }
        self.range_filters.put(&mut params);
        params.to_bytes(b, version)
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> crate::Result<Self> {
        if !version.request_update_has_parameters() {
            let mut message = Self::decode_subscribe_update(b, version)?;
            let mut params = MessageParams::from_bytes(b, version)?;
            message.object_delivery_timeout = params.take_varint(Param::ObjectDeliveryTimeout);
            params.take_all_bytes(Param::AuthorizationToken);
            params.finish(version)?;
            return Ok(message);
        }
        let request_id = decode_request_id(b, version)?;
        let existing_request_id = match version.stream_per_request() {
            true => RequestId(0),
            false => RequestId(b.varint()?),
        };
        let mut params = MessageParams::from_bytes(b, version)?;
        let message = Self {
            subscriber_priority: params.take_u8(Param::SubscriberPriority),
            forward: params.take_forward()?,
            filter: params.take_bytes(Param::LocationFilter).map(|bytes| from_slice(&bytes, version)).transpose()?,
            object_delivery_timeout: params.take_varint(Param::ObjectDeliveryTimeout),
            subgroup_delivery_timeout: params.take_varint(Param::SubgroupDeliveryTimeout),
            new_group_request: params.take_varint(Param::NewGroupRequest),
            range_filters: RangeFilters::take(&mut params),
            ..Self::new(Some(request_id), existing_request_id)
        };
        // TODO a new prefix for a SUBSCRIBE_NAMESPACE or SUBSCRIBE_TRACKS (§10.2.19), implement when needed
        params.ignore(&[Param::TrackNamespacePrefix]);
        // TODO authorization, until then tokens are ignored
        params.take_all_bytes(Param::AuthorizationToken);
        params.finish(version)?;
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Location, MOQ_VERSION_DRAFT_16};

    #[test]
    fn round_trips() {
        let version = MOQ_VERSION_DRAFT_16;
        let update = RequestUpdateMessage {
            subscriber_priority: Some(3),
            forward: Some(false),
            filter: Some(LocationFilter::AbsoluteStart { start: Location { group: 4, object: 0 } }),
            object_delivery_timeout: Some(500),
            ..RequestUpdateMessage::new(Some(RequestId(2)), RequestId(0))
        };
        let decoded: RequestUpdateMessage = from_slice(&to_vec(&update, version).unwrap(), version).unwrap();
        assert_eq!(decoded.subscriber_priority, Some(3));
        assert_eq!(decoded.forward, Some(false));
        assert_eq!(decoded.filter, update.filter);
        assert_eq!(decoded.object_delivery_timeout, Some(500));
    }

    /// draft-ietf-moq-transport-17 §9.10, 18 §10.9: the request's stream names the updated request, draft 17 adds a Required Request ID Delta.
    #[test]
    fn drafts_17_on_have_no_existing_request_id() {
        let update = RequestUpdateMessage::new(Some(RequestId(2)), RequestId(0));
        // type, length, request id, (delta,) no parameters
        assert_eq!(to_vec(&update, crate::MOQ_VERSION_DRAFT_17).unwrap(), [0x02, 0, 3, 2, 0, 0]);
        assert_eq!(to_vec(&update, crate::MOQ_VERSION_DRAFT_18).unwrap(), [0x02, 0, 2, 2, 0]);
        let decoded: RequestUpdateMessage = from_slice(&[0x02, 0, 2, 2, 0], crate::MOQ_VERSION_DRAFT_19).unwrap();
        assert_eq!((decoded.request_id, decoded.existing_request_id), (Some(RequestId(2)), RequestId(0)));
    }

    /// draft-ietf-moq-transport-19 §10.2.19: TRACK_NAMESPACE_PREFIX 0x34, a Track Namespace.
    #[test]
    fn draft_19_takes_track_namespace_prefix() {
        // type, length, request id, one parameter: 0x34, namespace `a`
        let decoded: RequestUpdateMessage = from_slice(&[0x02, 0, 6, 2, 1, 0x34, 1, 1, b'a'], crate::MOQ_VERSION_DRAFT_19).unwrap();
        assert_eq!(decoded.request_id, Some(RequestId(2)));
    }

    fn subscribe_update(request_id: Option<RequestId>) -> RequestUpdateMessage {
        RequestUpdateMessage {
            subscriber_priority: Some(3),
            forward: Some(true),
            filter: Some(LocationFilter::AbsoluteRange { start: Location { group: 4, object: 1 }, end_group: 9 }),
            ..RequestUpdateMessage::new(request_id, RequestId(6))
        }
    }

    /// draft-ietf-moq-transport-07 §6.5, 08 §7.5, 11 §8.10 and 14 §9.10: the subscription's fields, End Group plus 1.
    #[rstest::rstest]
    #[case(crate::MOQ_VERSION_DRAFT_07, None, &[0x02, 7, 6, 4, 1, 10, 0, 3, 0])]
    #[case(crate::MOQ_VERSION_DRAFT_08, None, &[0x02, 6, 6, 4, 1, 10, 3, 0])]
    #[case(crate::MOQ_VERSION_DRAFT_11, None, &[0x02, 0, 7, 6, 4, 1, 10, 3, 1, 0])]
    #[case(crate::MOQ_VERSION_DRAFT_14, Some(RequestId(8)), &[0x02, 0, 8, 8, 6, 4, 1, 10, 3, 1, 0])]
    fn subscribe_update_until_draft_14(#[case] version: Version, #[case] request_id: Option<RequestId>, #[case] bytes: &[u8]) {
        let update = subscribe_update(request_id);
        assert_eq!(to_vec(&update, version).unwrap(), bytes);
        let decoded: RequestUpdateMessage = from_slice(bytes, version).unwrap();
        assert_eq!((decoded.request_id, decoded.existing_request_id, decoded.filter, decoded.subscriber_priority, decoded.forward), (request_id, RequestId(6), update.filter, Some(3), Some(true)));
    }

    /// Before draft 15 an update names the whole subscription, and before draft 11 it can't stop forwarding.
    #[test]
    fn subscribe_update_needs_the_whole_subscription() {
        let relative = RequestUpdateMessage { filter: Some(LocationFilter::LargestObject), ..subscribe_update(None) };
        assert!(matches!(to_vec(&relative, crate::MOQ_VERSION_DRAFT_11), Err(Error::Unencodable(_))));
        let paused = RequestUpdateMessage { forward: Some(false), ..subscribe_update(None) };
        assert!(matches!(to_vec(&paused, crate::MOQ_VERSION_DRAFT_10), Err(Error::Unsupported(_))));
    }
}
