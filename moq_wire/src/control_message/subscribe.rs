use crate::prelude::*;
use crate::bytes::{from_slice, to_vec, FromBytes, ToBytes};
use crate::error::{Error, Result};
use crate::control_message::{decode_request_id, encode_request_id, ControlMessage};
use crate::location::Location;
use crate::range_filter::RangeFilters;
use crate::message_parameters::{MessageParams, Param};
use crate::{Frame, Mode, Reader, Writer};
use crate::{GroupOrder, Namespace, NamespaceTrackname, RequestId, TrackAlias, Version};
use crate::{
    ABSOLUTE_RANGE_FILTER_ID, ABSOLUTE_START_FILTER_ID, DEFAULT_SUBSCRIBER_PRIORITY, LARGEST_OBJECT_FILTER_ID, MOQ_VERSION_DRAFT_11,
    MOQ_VERSION_DRAFT_15, NEXT_GROUP_START_FILTER_ID, SUBSCRIBE_MESSAGE_ID,
};

/// SUBSCRIBE (draft-ietf-moq-transport-19 §10.7).
#[derive(Debug, Eq, PartialEq)]
pub struct SubscribeMessage {
    pub request_id: RequestId,
    /// `Some` from draft 7 to draft 11, where the subscriber picks it
    pub track_alias: Option<TrackAlias>,
    pub namespace_trackname: NamespaceTrackname,
    pub subscriber_priority: u8,
    /// `None` for the publisher's order
    pub group_order: Option<GroupOrder>,
    /// The Forward State, always forwarding before draft 11
    pub forward: bool,
    pub filter: LocationFilter,
    /// OBJECT_DELIVERY_TIMEOUT in milliseconds
    pub object_delivery_timeout: Option<u64>,
    /// SUBGROUP_DELIVERY_TIMEOUT in milliseconds, draft 18 on
    pub subgroup_delivery_timeout: Option<u64>,
    /// RENDEZVOUS_TIMEOUT in milliseconds, draft 17 on
    pub rendezvous_timeout: Option<u64>,
    /// NEW_GROUP_REQUEST, draft 15 on
    pub new_group_request: Option<u64>,
    pub range_filters: RangeFilters,
}

impl SubscribeMessage {
    /// Subscribing to `namespace_trackname` from the next group, with the spec's defaults.
    pub fn new(request_id: RequestId, namespace_trackname: NamespaceTrackname) -> Self {
        Self {
            request_id,
            track_alias: None,
            namespace_trackname,
            subscriber_priority: DEFAULT_SUBSCRIBER_PRIORITY,
            group_order: None,
            forward: true,
            filter: LocationFilter::NextGroupStart,
            object_delivery_timeout: None,
            subgroup_delivery_timeout: None,
            rendezvous_timeout: None,
            new_group_request: None,
            range_filters: RangeFilters::default(),
        }
    }

    pub fn track_namespace(&self) -> &Namespace {
        self.namespace_trackname.namespace()
    }

    pub fn track_name(&self) -> &[u8] {
        self.namespace_trackname.trackname()
    }
}


impl ControlMessage for SubscribeMessage {
    const MESSAGE_IDS: &'static [u64] = &[SUBSCRIBE_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "subscribe" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> Result<()> {
        encode_request_id(b, self.request_id, version)?;
        if version.draft() <= 11 {
            let alias = self.track_alias.ok_or(Error::Unencodable("SUBSCRIBE without the track alias of drafts 7 to 11"))?;
            b.varint(alias.0)?;
        }
        self.namespace_trackname.to_bytes(b, version)?;
        let mut params = MessageParams::new();
        if let Some(ms) = self.object_delivery_timeout {
            params.put_varint(Param::ObjectDeliveryTimeout, ms);
        }
        // hints a draft lacks are left out
        for (param, value) in [
            (Param::SubgroupDeliveryTimeout, self.subgroup_delivery_timeout),
            (Param::RendezvousTimeout, self.rendezvous_timeout),
            (Param::NewGroupRequest, self.new_group_request),
        ] {
            if let Some(v) = value && param.ty(version).is_some() {
                params.put_varint(param, v);
            }
        }
        self.range_filters.put(&mut params);
        // fields until draft 14, parameters from draft 15 (draft-16 §9.2.2.3-.5, .8)
        if version < MOQ_VERSION_DRAFT_15 {
            b.u8(self.subscriber_priority);
            b.u8(GroupOrder::requested_id(self.group_order));
            if version >= MOQ_VERSION_DRAFT_11 {
                b.u8(self.forward.into());
            }
            self.filter.to_bytes(b, version)?;
        } else {
            params.put_u8(Param::SubscriberPriority, self.subscriber_priority);
            params.put_u8(Param::Forward, self.forward.into());
            params.put_bytes(Param::LocationFilter, to_vec(&self.filter, version)?);
            if let Some(order) = self.group_order {
                params.put_u8(Param::GroupOrder, order as u8);
            }
        }
        params.to_bytes(b, version)
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> Result<Self> {
        let request_id = decode_request_id(b, version)?;
        let track_alias = match version.draft() {
            ..=11 => Some(TrackAlias(b.varint()?)),
            12.. => None,
        };
        let namespace_trackname = NamespaceTrackname::from_bytes(b, version)?;
        let mut message = Self { track_alias, ..Self::new(request_id, namespace_trackname) };
        if version < MOQ_VERSION_DRAFT_15 {
            message.subscriber_priority = b.u8()?;
            message.group_order = GroupOrder::from_requested(b.u8()?)?;
            if version >= MOQ_VERSION_DRAFT_11 {
                message.forward = forward_state(b.u8()?.into())?;
            }
            message.filter = LocationFilter::from_bytes(b, version)?;
        }
        let mut params = MessageParams::from_bytes(b, version)?;
        if version >= MOQ_VERSION_DRAFT_15 {
            message.subscriber_priority = params.take_u8(Param::SubscriberPriority).unwrap_or(DEFAULT_SUBSCRIBER_PRIORITY);
            message.group_order = params.take_u8(Param::GroupOrder).map(|order| GroupOrder::try_from(u64::from(order))).transpose()?;
            message.forward = params.take_forward()?.unwrap_or(true);
            // without one the subscription is unfiltered (§9.2.2.5), the same as AbsoluteStart at 0 (§5.1.2)
            message.filter = params.take_bytes(Param::LocationFilter)
                .map(|bytes| from_slice(&bytes, version))
                .transpose()?
                .unwrap_or(LocationFilter::AbsoluteStart { start: Location { group: 0, object: 0 } });
        }
        message.object_delivery_timeout = params.take_varint(Param::ObjectDeliveryTimeout);
        message.subgroup_delivery_timeout = params.take_varint(Param::SubgroupDeliveryTimeout);
        message.rendezvous_timeout = params.take_varint(Param::RendezvousTimeout);
        message.new_group_request = params.take_varint(Param::NewGroupRequest);
        message.range_filters = RangeFilters::take(&mut params);
        // TODO authorization, until then tokens are ignored
        params.take_all_bytes(Param::AuthorizationToken);
        params.finish(version)?;
        Ok(message)
    }
}

/// Where a subscription starts and ends (draft-ietf-moq-transport-16 §5.1.2).
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LocationFilter {
    NextGroupStart,
    LargestObject,
    AbsoluteStart { start: Location },
    AbsoluteRange { start: Location, end_group: u64 },
}

impl LocationFilter {
    /// The Filter Type, e.g. for qlog.
    pub fn wire_id(&self) -> u64 {
        match self {
            Self::NextGroupStart => NEXT_GROUP_START_FILTER_ID,
            Self::LargestObject => LARGEST_OBJECT_FILTER_ID,
            Self::AbsoluteStart { .. } => ABSOLUTE_START_FILTER_ID,
            Self::AbsoluteRange { .. } => ABSOLUTE_RANGE_FILTER_ID,
        }
    }

    pub fn start_location(&self) -> Option<Location> {
        match *self {
            Self::AbsoluteStart { start } | Self::AbsoluteRange { start, .. } => Some(start),
            Self::NextGroupStart | Self::LargestObject => None,
        }
    }

    pub fn end_group(&self) -> Option<u64> {
        match *self {
            Self::AbsoluteRange { end_group, .. } => Some(end_group),
            _ => None,
        }
    }

    /// The SUBSCRIPTION_FILTER parameter's bytes.
    pub fn encode(&self, version: Version) -> Result<Vec<u8>> {
        to_vec(self, version)
    }

    /// From the SUBSCRIPTION_FILTER parameter's bytes.
    pub fn decode(bytes: &[u8], version: Version) -> Result<Self> {
        from_slice(bytes, version)
    }
}

impl LocationFilter {
    /// The absolute filter a relative one is once SUBSCRIBE_OK or PUBLISH named the Largest Object,
    /// from `{0, 0}` without one (draft-ietf-moq-transport-14 §5.1.2). Before draft 11 type 1 is
    /// Latest Group, from the Largest Object's group, and Latest Object starts after it as later.
    pub fn resolved(self, largest: Option<Location>, version: Version) -> Self {
        let start = |from_largest: fn(Location) -> Location| largest.map_or(Location { group: 0, object: 0 }, from_largest);
        match self {
            Self::LargestObject => Self::AbsoluteStart { start: start(Location::next) },
            Self::NextGroupStart if version.draft() <= 10 => Self::AbsoluteStart { start: start(|l| Location { group: l.group, object: 0 }) },
            Self::NextGroupStart => Self::AbsoluteStart { start: start(|l| Location { group: l.group + 1, object: 0 }) },
            absolute => absolute,
        }
    }
}

impl FromBytes for LocationFilter {
    fn from_bytes<M: Mode>(r: &mut Reader<'_, M>, version: Version) -> Result<Self> {
        Ok(match r.varint()? {
            NEXT_GROUP_START_FILTER_ID => Self::NextGroupStart,
            LARGEST_OBJECT_FILTER_ID => Self::LargestObject,
            ABSOLUTE_START_FILTER_ID => Self::AbsoluteStart { start: Location::from_bytes(r, version)? },
            ABSOLUTE_RANGE_FILTER_ID => Self::AbsoluteRange { start: Location::from_bytes(r, version)?, end_group: r.varint()? },
            ty => return Err(Error::ProtocolViolation(format!("unknown filter type {ty}"))),
        })
    }
}

impl ToBytes for LocationFilter {
    fn to_bytes(&self, w: &mut Writer, version: Version) -> Result<()> {
        w.varint(self.wire_id())?;
        if let Some(start) = self.start_location() {
            start.to_bytes(w, version)?;
        }
        if let Some(end_group) = self.end_group() {
            w.varint(end_group)?;
        }
        Ok(())
    }
}

/// The Forward State: 0 or 1 (draft-ietf-moq-transport-16 §9.2.2.8).
pub(crate) fn forward_state(v: u64) -> Result<bool> {
    match v {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(Error::ProtocolViolation(format!("FORWARD {v}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_16};
    use rstest::rstest;

    #[rstest]
    #[case(LocationFilter::NextGroupStart)]
    #[case(LocationFilter::LargestObject)]
    #[case(LocationFilter::AbsoluteStart { start: Location { group: 3, object: 7 } })]
    #[case(LocationFilter::AbsoluteRange { start: Location { group: 3, object: 7 }, end_group: 12 })]
    fn filter_round_trips(#[case] filter: LocationFilter) {
        let bytes = filter.encode(MOQ_VERSION_DRAFT_16).unwrap();
        assert_eq!(LocationFilter::decode(&bytes, MOQ_VERSION_DRAFT_16).unwrap(), filter);
    }

    #[test]
    fn unknown_filter_type_is_protocol_violation() {
        let decoded = LocationFilter::decode(&[0x3f], MOQ_VERSION_DRAFT_16);
        assert!(matches!(decoded, Err(Error::ProtocolViolation(_))), "{decoded:?}");
    }

    fn subscribe(version: Version) -> SubscribeMessage {
        SubscribeMessage {
            track_alias: (version.draft() <= 11).then_some(TrackAlias(2)),
            subscriber_priority: 7,
            group_order: Some(GroupOrder::Descending),
            forward: false,
            filter: LocationFilter::AbsoluteRange { start: Location { group: 1, object: 2 }, end_group: 3 },
            object_delivery_timeout: Some(100),
            ..SubscribeMessage::new(RequestId(2), "n--t".parse().unwrap())
        }
    }

    #[rstest]
    #[case(MOQ_VERSION_DRAFT_14)]
    #[case(MOQ_VERSION_DRAFT_16)]
    #[case(crate::MOQ_VERSION_DRAFT_18)]
    fn subscribe_round_trips(#[case] version: Version) {
        let bytes = to_vec(&subscribe(version), version).unwrap();
        assert_eq!(from_slice::<SubscribeMessage>(&bytes, version).unwrap(), subscribe(version));
    }

    /// A draft 16 SUBSCRIBE for request 2 of `n--t` with `params`.
    fn draft_16_subscribe(params: &[u8]) -> Result<SubscribeMessage> {
        let body = [&[2, 1, 1, b'n', 1, b't'][..], params].concat();
        let bytes = [&[0x03, 0, body.len() as u8][..], &body].concat();
        from_slice(&bytes, MOQ_VERSION_DRAFT_16)
    }

    /// draft-ietf-moq-transport-16 §9.2.2.4: only 1 and 2, omitted is the publisher's order.
    #[test]
    fn group_order_parameter() {
        assert_eq!(draft_16_subscribe(&[0]).unwrap().group_order, None);
        assert_eq!(draft_16_subscribe(&[1, 0x22, 2]).unwrap().group_order, Some(GroupOrder::Descending));
        assert!(matches!(draft_16_subscribe(&[1, 0x22, 0]), Err(Error::ProtocolViolation(_))));
    }

    #[test]
    fn forward_over_1_is_protocol_violation() {
        assert!(matches!(draft_16_subscribe(&[1, 0x10, 2]), Err(Error::ProtocolViolation(_))));
    }

    /// Range filters change which objects arrive, older drafts can't express them.
    #[test]
    fn range_filters_need_draft_19() {
        let mut params = MessageParams::new();
        params.put_bytes(Param::SubgroupFilter, vec![0]);
        let subscribe = SubscribeMessage { range_filters: RangeFilters::take(&mut params), ..subscribe(MOQ_VERSION_DRAFT_16) };
        assert!(matches!(to_vec(&subscribe, MOQ_VERSION_DRAFT_16), Err(Error::Unsupported(_))));
    }
}
