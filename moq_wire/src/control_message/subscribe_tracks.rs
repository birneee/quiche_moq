use crate::prelude::*;
use crate::bytes::{FromBytes, ToBytes};
use crate::control_message::{decode_request_id, encode_request_id, ControlMessage};
use crate::message_parameters::{MessageParams, Param};
use crate::range_filter::RangeFilters;
use crate::{Frame, GroupOrder, NamespaceTuple, Reader, RequestId, Version, Writer};
use crate::{PUBLISH_BLOCKED_MESSAGE_ID, SUBSCRIBE_TRACKS_MESSAGE_ID};

/// SUBSCRIBE_TRACKS (draft-ietf-moq-transport-18 §10.19): a PUBLISH for every track under `prefix`, now and later.
#[derive(Debug)]
pub struct SubscribeTracksMessage {
    pub request_id: RequestId,
    pub prefix: NamespaceTuple,
    /// The Forward State of the PUBLISHes.
    pub forward: bool,
    /// The group order of the PUBLISHes, `None` for the publisher's, draft 19 on (§10.19.1)
    pub group_order: Option<GroupOrder>,
}

impl ControlMessage for SubscribeTracksMessage {
    const MESSAGE_IDS: &'static [u64] = &[SUBSCRIBE_TRACKS_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "subscribe_tracks" }

    fn accepts(version: Version) -> bool { version.draft() >= 18 }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> crate::Result<()> {
        encode_request_id(b, self.request_id, version)?;
        self.prefix.to_bytes(b, version)?;
        let mut params = MessageParams::new();
        if !self.forward {
            params.put_u8(Param::Forward, 0);
        }
        if let Some(order) = self.group_order {
            params.put_u8(Param::GroupOrder, order as u8);
        }
        params.to_bytes(b, version)
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> crate::Result<Self> {
        let request_id = decode_request_id(b, version)?;
        let prefix = NamespaceTuple::from_bytes(b, version)?;
        let mut params = MessageParams::from_bytes(b, version)?;
        let forward = params.take_forward()?.unwrap_or(true);
        let group_order = params.take_u8(Param::GroupOrder).map(|order| GroupOrder::try_from(u64::from(order))).transpose()?;
        // TODO the other Subscription parameters as the PUBLISHes' defaults (§10.19.1), implement when needed
        RangeFilters::take(&mut params);
        params.ignore(&[Param::SubscriberPriority, Param::LocationFilter, Param::ObjectDeliveryTimeout, Param::SubgroupDeliveryTimeout,
            Param::RendezvousTimeout, Param::NewGroupRequest]);
        // TODO authorization, until then tokens are ignored
        params.take_all_bytes(Param::AuthorizationToken);
        params.finish(version)?;
        Ok(Self { request_id, prefix, forward, group_order })
    }
}

/// PUBLISH_BLOCKED (§10.20): the track with `suffix` after the SUBSCRIBE_TRACKS's prefix and
/// `track_name` gets no PUBLISH, no stream was available for it (§6.1).
#[derive(Debug)]
pub struct PublishBlockedMessage {
    pub suffix: NamespaceTuple,
    pub track_name: Vec<u8>,
}

impl ControlMessage for PublishBlockedMessage {
    const MESSAGE_IDS: &'static [u64] = &[PUBLISH_BLOCKED_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "publish_blocked" }

    fn accepts(version: Version) -> bool { version.draft() >= 18 }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> crate::Result<()> {
        self.suffix.to_bytes(b, version)?;
        b.length_prefixed_bytes(&self.track_name)?;
        Ok(())
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> crate::Result<Self> {
        let suffix = NamespaceTuple::from_bytes(b, version)?;
        let track_name = b.length_prefixed_bytes()?.to_vec();
        Ok(Self { suffix, track_name })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_message::ControlMessageEnum;
    use crate::{MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_18, MOQ_VERSION_DRAFT_19};

    #[rstest::rstest]
    fn round_trips(#[values(false, true)] forward: bool) {
        let sent = ControlMessageEnum::SubscribeTracks(SubscribeTracksMessage { request_id: RequestId(4), prefix: NamespaceTuple::new(vec![b"a".to_vec()]).unwrap(), forward, group_order: None });
        let version = MOQ_VERSION_DRAFT_18;
        let received: ControlMessageEnum = crate::bytes::from_slice(&crate::bytes::to_vec(&sent, version).unwrap(), version).unwrap();
        assert!(matches!(received, ControlMessageEnum::SubscribeTracks(m) if m.request_id == RequestId(4) && m.forward == forward));
    }

    /// draft-ietf-moq-transport-19 §10.19.1: the Subscription parameters, GROUP_ORDER kept for the PUBLISHes.
    #[test]
    fn draft_19_takes_subscription_parameters() {
        let version = MOQ_VERSION_DRAFT_19;
        // request id, prefix `a`, FORWARD 0, SUBSCRIBER_PRIORITY 7, GROUP_ORDER descending
        let bytes = [0x51, 0, 11, 4, 1, 1, b'a', 3, 0x10, 0, 0x10, 7, 0x02, 2];
        let ControlMessageEnum::SubscribeTracks(received) = crate::bytes::from_slice(&bytes, version).unwrap() else { panic!() };
        assert_eq!((received.forward, received.group_order), (false, Some(GroupOrder::Descending)));
        let encoded = crate::bytes::to_vec(&ControlMessageEnum::SubscribeTracks(received), version).unwrap();
        assert_eq!(encoded, [0x51, 0, 9, 4, 1, 1, b'a', 2, 0x10, 0, 0x12, 2]);
    }

    #[test]
    fn publish_blocked_round_trips() {
        let sent = ControlMessageEnum::PublishBlocked(PublishBlockedMessage { suffix: NamespaceTuple::new(vec![b"a".to_vec()]).unwrap(), track_name: b"t".to_vec() });
        let version = MOQ_VERSION_DRAFT_18;
        let received: ControlMessageEnum = crate::bytes::from_slice(&crate::bytes::to_vec(&sent, version).unwrap(), version).unwrap();
        assert!(matches!(received, ControlMessageEnum::PublishBlocked(m) if m.suffix.fields() == [b"a".to_vec()] && m.track_name == b"t"));
    }

    #[test]
    fn from_draft_18() {
        assert!(!SubscribeTracksMessage::accepts(MOQ_VERSION_DRAFT_16));
    }
}
