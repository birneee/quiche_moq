//! Byte vectors written from the drafts' figures: interoperability, not just round trips.

use crate::prelude::*;
use crate::bytes::{from_slice, to_vec, FromBytes};
use crate::control_message::subscribe::SubscribeMessage;
use crate::control_message::{ControlMessageEnum, FetchKind, FetchMessage, FetchRange, PublishNamespaceMessage};
use crate::object::ObjectHeader;
use crate::subgroup::SubgroupHeader;
use crate::{GroupOrder, KeyValuePairs, Location, RequestId, LocationFilter, VarintKey, Version};
use crate::{MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_08, MOQ_VERSION_DRAFT_09, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_17};

/// A subgroup header and its first object, decoded and encoded.
fn subgroup_object(version: Version, bytes: &[u8]) -> (SubgroupHeader, ObjectHeader) {
    let mut r = version.reader(bytes);
    let header = SubgroupHeader::from_bytes(&mut r, version).unwrap();
    let object = ObjectHeader::decode(&mut r, version, &header, None).unwrap();
    assert_eq!(r.rest(), b"x", "the payload follows");
    let mut encoded = to_vec(&header, version).unwrap();
    object.encode(&mut version.writer(&mut encoded), version, None).unwrap();
    encoded.push(b'x');
    assert_eq!(encoded, bytes);
    (header, object)
}

/// draft-ietf-moq-transport-07 Figures 28, 29: no Subscribe ID, no properties.
#[test]
fn draft_07_subgroup_object() {
    // type, alias, group, subgroup, priority | object id, payload length, payload
    let (header, object) = subgroup_object(MOQ_VERSION_DRAFT_07, &[0x04, 1, 2, 3, 0, 5, 1, b'x']);
    assert_eq!((header.track_alias().0, header.group_id(), header.subgroup_id()), (1, 2, Some(3)));
    assert_eq!(object.id(), 5);
}

/// draft-ietf-moq-transport-08 Figure 32: Extension Count, then the headers.
#[test]
fn draft_08_object_properties_are_counted() {
    // type, alias, group, subgroup, priority | object id, count, type 2 = 7, payload length, payload
    let (_, object) = subgroup_object(MOQ_VERSION_DRAFT_08, &[0x04, 1, 2, 3, 0, 5, 1, 2, 7, 1, b'x']);
    assert_eq!(object.properties().get_varint(VarintKey::new(2)), Some(7));
}

/// draft-ietf-moq-transport-09 Figure 32: Extension Headers Length, then the headers.
#[test]
fn draft_09_object_properties_have_a_length() {
    let (_, object) = subgroup_object(MOQ_VERSION_DRAFT_09, &[0x04, 1, 2, 3, 0, 5, 2, 2, 7, 1, b'x']);
    assert_eq!(object.properties().get_varint(VarintKey::new(2)), Some(7));
}

/// draft-ietf-moq-transport-11 §1.4.2: a value is at most 2^16-1 bytes.
#[test]
fn property_value_over_u16_is_protocol_violation() {
    let version = MOQ_VERSION_DRAFT_11;
    let mut bytes = vec![];
    let mut w = version.writer(&mut bytes);
    // type 1, length 2^16
    w.varint(1).unwrap();
    w.length_prefixed_bytes(&[0; 1 << 16]).unwrap();
    let decoded = KeyValuePairs::decode_to_end(&mut version.frame_reader(&bytes), version);
    assert!(matches!(decoded, Err(crate::Error::ProtocolViolation(_))), "{decoded:?}");
}

/// draft-ietf-moq-transport-17 §9.11: Request ID, then Required Request ID Delta.
#[test]
fn draft_17_subscribe_has_required_request_id_delta() {
    let version = MOQ_VERSION_DRAFT_17;
    let bytes = [
        0x03, 0, 17, // type, length
        2, 0, // request id, required request id delta
        1, 1, b'n', 1, b't', // namespace, track name
        4, 0x10, 1, 0x10, 7, 0x01, 1, 0x02, 0x01, 2, // FORWARD 1, SUBSCRIBER_PRIORITY 7, filter LargestObject, GROUP_ORDER 2
    ];
    let subscribe = SubscribeMessage {
        track_alias: None,
        subscriber_priority: 7,
        group_order: Some(GroupOrder::Descending),
        forward: true,
        filter: LocationFilter::LargestObject,
        ..SubscribeMessage::new(RequestId(2), "n--t".parse().unwrap())
    };
    assert_eq!(to_vec(&subscribe, version).unwrap(), bytes);
    assert_eq!(from_slice::<SubscribeMessage>(&bytes, version).unwrap(), subscribe);
}

/// draft-ietf-moq-transport-17 §9.20, PUBLISH_NAMESPACE.
#[test]
fn draft_17_publish_namespace_has_required_request_id_delta() {
    let version = MOQ_VERSION_DRAFT_17;
    // type, length, request id, required request id delta, namespace, no parameters
    let bytes = [0x06, 0, 6, 4, 0, 1, 1, b'n', 0];
    let message = PublishNamespaceMessage::new(Some(RequestId(4)), "n".parse().unwrap());
    assert_eq!(to_vec(&message, version).unwrap(), bytes);
    let ControlMessageEnum::PublishNamespace(decoded) = from_slice(&bytes, version).unwrap() else { panic!() };
    assert_eq!(decoded.request_id(), Some(RequestId(4)));
}

/// A nonzero delta asks to hold the request back, which isn't implemented.
#[test]
fn draft_17_request_dependency_is_unsupported() {
    let version = MOQ_VERSION_DRAFT_17;
    let bytes = [0x06, 0, 6, 4, 1, 1, 1, b'n', 0];
    let decoded = from_slice::<ControlMessageEnum>(&bytes, version);
    assert!(matches!(decoded, Err(crate::Error::Unsupported(_))), "{decoded:?}");
}

/// draft-ietf-moq-transport-14 §10.4.2: Object IDs 0, 1 and 5 on one stream are deltas 0, 0 and 3.
#[test]
fn object_ids_are_deltas_from_draft_14() {
    let version = crate::MOQ_VERSION_DRAFT_16;
    // type 0x14 (no properties), alias, group, subgroup, priority | delta, payload length, payload, ...
    let bytes = [0x14, 1, 2, 3, 0, 0, 1, b'a', 0, 1, b'b', 3, 1, b'c'];
    let mut r = version.reader(&bytes);
    let header = SubgroupHeader::from_bytes(&mut r, version).unwrap();
    let mut encoded = to_vec(&header, version).unwrap();
    let mut previous = None;
    let mut ids = vec![];
    while !r.is_empty() {
        let object = ObjectHeader::decode(&mut r, version, &header, previous).unwrap();
        object.encode(&mut version.writer(&mut encoded), version, previous).unwrap();
        encoded.extend(r.bytes(1).unwrap());
        previous = Some(object.id());
        ids.push(object.id());
    }
    assert_eq!(ids, [0, 1, 5]);
    assert_eq!(encoded, bytes);
}

/// Before draft 14 an object gives its ID itself.
#[test]
fn object_ids_are_absolute_before_draft_14() {
    let version = MOQ_VERSION_DRAFT_11;
    let header = SubgroupHeader::new(crate::TrackAlias(1), 2, 3, version);
    let object = ObjectHeader::new(5, 1, header.ty(), KeyValuePairs::new());
    let mut encoded = vec![];
    object.encode(&mut version.writer(&mut encoded), version, Some(4)).unwrap();
    // id, no properties (length 0), payload length
    assert_eq!(encoded, [5, 0, 1]);
}

/// draft-ietf-moq-transport-07 Figure 8: the fields after the track, no Fetch Type.
#[test]
fn draft_07_fetch_has_fields_after_the_track() {
    let version = MOQ_VERSION_DRAFT_07;
    let bytes = [
        0x16, 13, // type, length
        2, // subscribe id
        1, 1, b'n', 1, b't', // namespace, track name
        9, 2, // subscriber priority, group order descending
        1, 0, 3, 0, // start group and object, end group and object
        0, // no parameters
    ];
    let range = FetchRange { start: Location { group: 1, object: 0 }, end: Location { group: 3, object: 0 } };
    let fetch = FetchMessage {
        subscriber_priority: 9,
        group_order: Some(GroupOrder::Descending),
        ..FetchMessage::new(RequestId(2), FetchKind::Standalone { namespace_trackname: "n--t".parse().unwrap(), range })
    };
    assert_eq!(to_vec(&fetch, version).unwrap(), bytes);
    assert_eq!(from_slice::<FetchMessage>(&bytes, version).unwrap(), fetch);
}
